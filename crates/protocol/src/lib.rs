use alixer_core::*;
use serde::{Deserialize, Serialize};
pub const VERSION: u32 = 5;
pub const MAX_FRAME: usize = 1024 * 1024;
// Port metadata for a large LV2 installation can exceed the normal IPC limit.
pub const MAX_CATALOG_FRAME: usize = 32 * 1024 * 1024;
pub fn reply_frame_limit(command: &Command) -> usize {
    if matches!(command, Command::Plugins) {
        MAX_CATALOG_FRAME
    } else {
        MAX_FRAME
    }
}
// Discovery can involve hundreds of bundles on slow storage.
pub const PLUGIN_SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub revision: Option<u64>,
    pub command: Command,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Command {
    Snapshot,
    Edit(Edit),
    Resolve { conflict: Id, restore: bool },
    Plugins,
    ApplyOutputNames,
    Subscribe,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub scene: Scene,
    pub observation: Observation,
    pub conflicts: Vec<Conflict>,
    pub status: String,
    pub peaks: Vec<(Id, f32)>,
    pub channel_peaks: Vec<(Id, Vec<f32>)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub id: u64,
    pub revision: u64,
    pub result: Result<Payload, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Payload {
    Snapshot(Snapshot),
    Accepted,
    Plugins(serde_json::Value),
}

pub fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "alixer-user-{}",
                std::env::var("USER").unwrap_or_default()
            ))
        })
        .join("alixer")
}
pub fn state_dir() -> std::path::PathBuf {
    std::env::var_os("ALIXER_STATE_DIR")
        .or_else(|| std::env::var_os("LIXER_STATE_DIR"))
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                        .join(".local/state")
                })
                .join("alixer")
        })
}

/// Copy legacy state atomically, retaining its assets for existing LV2 state
/// files that may refer to absolute paths in the old directory.
pub fn migrate_legacy_state() -> std::io::Result<()> {
    let legacy_runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "lixer-user-{}",
                std::env::var("USER").unwrap_or_default()
            ))
        })
        .join("lixer/control.sock");
    if std::os::unix::net::UnixStream::connect(legacy_runtime).is_ok() {
        return Err(std::io::Error::other(
            "Stop the old Lixer GUI/service before starting Alixer; two mixer engines must not run together",
        ));
    }
    if std::env::var_os("ALIXER_STATE_DIR").is_some()
        || std::env::var_os("LIXER_STATE_DIR").is_some()
    {
        return Ok(());
    }
    let destination = state_dir();
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    copy_legacy_state(&parent.join("lixer"), &destination)
}

fn copy_legacy_state(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, symlink};
    if destination.exists() || !source.is_dir() {
        return Ok(());
    }
    fn copy(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
        std::fs::DirBuilder::new().mode(0o700).create(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let target = destination.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                symlink(std::fs::read_link(entry.path())?, target)?;
            } else if kind.is_dir() {
                copy(&entry.path(), &target)?;
            } else if kind.is_file() {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }
    let temporary = destination.with_extension(format!("migration-{}", alixer_core::Id::new_v4()));
    let result = copy(source, &temporary).and_then(|_| std::fs::rename(&temporary, destination));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temporary);
        if destination.exists() {
            return Ok(());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_copies_preferences_and_plugin_assets_without_overwriting_new_state() {
        let root =
            std::env::temp_dir().join(format!("alixer-migration-{}", alixer_core::Id::new_v4()));
        let old = root.join("lixer");
        let new = root.join("alixer");
        std::fs::create_dir_all(old.join("plugins/effect")).unwrap();
        std::fs::write(old.join("ui.json"), r#"{"zoom":1.5}"#).unwrap();
        std::fs::write(old.join("plugins/effect/state.ttl"), "plugin state").unwrap();
        copy_legacy_state(&old, &new).unwrap();
        assert_eq!(
            std::fs::read(new.join("ui.json")).unwrap(),
            std::fs::read(old.join("ui.json")).unwrap()
        );
        assert!(old.join("plugins/effect/state.ttl").is_file());
        assert!(new.join("plugins/effect/state.ttl").is_file());
        std::fs::write(new.join("ui.json"), "new preferences").unwrap();
        copy_legacy_state(&old, &new).unwrap();
        assert_eq!(
            std::fs::read_to_string(new.join("ui.json")).unwrap(),
            "new preferences"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
