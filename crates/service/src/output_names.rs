use alixer_core::{Scene, StripKind};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("wireplumber/wireplumber.conf.d/99-alixer-output-names.conf")
}

fn rules(scene: &Scene) -> serde_json::Value {
    let rules: Vec<_> = scene
        .strips
        .iter()
        .filter(|s| s.kind == StripKind::Output)
        .filter_map(|s| Some((s.output_key.as_ref()?, s.output_name_override.as_ref()?)))
        .map(|(key, name)| {
            // Always anchor a regex, including keys beginning with '~'.
            let mut pattern = String::from("~^");
            for c in key.chars() {
                if r"\.^$|?*+()[]{}".contains(c) {
                    pattern.push('\\');
                }
                pattern.push(c);
            }
            pattern.push('$');
            serde_json::json!({"matches":[{"node.name":pattern}],
                "actions":{"update-props":{"node.description":name,"node.nick":name}}})
        })
        .collect();
    serde_json::json!({"monitor.alsa.rules":rules,"monitor.bluez.rules":rules})
}

pub fn write(scene: &Scene, path: &Path) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&rules(scene))?;
    if std::fs::read(path).ok().as_deref() != Some(bytes.as_slice()) {
        std::fs::create_dir_all(path.parent().context("Missing configuration directory")?)?;
        let temporary = path.with_extension("conf.tmp");
        std::fs::write(&temporary, bytes).context("Could not save system-wide output names")?;
        std::fs::rename(&temporary, path)
            .context("Could not install WirePlumber output-name rules")?;
    }
    // Old rules must not override new names or reintroduce names after reset.
    if path
        .file_name()
        .is_some_and(|n| n == "99-alixer-output-names.conf")
    {
        let legacy = path.with_file_name("99-lixer-output-names.conf");
        if legacy.exists() {
            std::fs::remove_file(legacy)?;
        }
    }
    Ok(())
}

pub fn apply() -> Result<()> {
    let result = std::process::Command::new("systemctl")
        .args(["--user", "--no-block", "restart", "wireplumber.service"])
        .output()
        .context("Could not restart WirePlumber; restart it manually to apply output names")?;
    if !result.status.success() {
        bail!(
            "Could not restart WirePlumber: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alixer_core::{Edit, Id, Strip};
    #[test]
    fn names_generate_escaped_system_rules_and_reset_removes_them() {
        let root = std::env::temp_dir().join(format!("alixer-output-names-{}", Id::new_v4()));
        let path = root.join("99-alixer-output-names.conf");
        std::fs::create_dir_all(&root).unwrap();
        let legacy = root.join("99-lixer-output-names.conf");
        std::fs::write(&legacy, "legacy rules").unwrap();
        let mut s = Strip::new(
            "Original",
            StripKind::Output,
            vec!["FL".into(), "FR".into()],
        );
        s.output_key = Some("alsa_output.usb-[speaker].1".into());
        let id = s.id;
        let mut scene = Scene {
            strips: vec![s],
            ..Default::default()
        };
        scene
            .edit(Edit::Rename {
                strip: id,
                name: "Desk \"speakers\"\nUSB".into(),
            })
            .unwrap();
        write(&scene, &path).unwrap();
        assert!(!legacy.exists());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            config["monitor.alsa.rules"][0]["matches"][0]["node.name"],
            r"~^alsa_output\.usb-\[speaker\]\.1$"
        );
        assert_eq!(
            config["monitor.alsa.rules"][0]["actions"]["update-props"]["node.description"],
            "Desk \"speakers\"\nUSB"
        );
        assert_eq!(config["monitor.alsa.rules"], config["monitor.bluez.rules"]);
        scene.edit(Edit::ResetOutputName { strip: id }).unwrap();
        write(&scene, &path).unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["monitor.alsa.rules"], serde_json::json!([]));
        assert_eq!(scene.strip(id).unwrap().name, "Original");
        assert_eq!(
            scene.strip(id).unwrap().output_key.as_deref(),
            Some("alsa_output.usb-[speaker].1")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
