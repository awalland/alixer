//! GUI-only preferences; no service connection is needed to restore the layout.
use alixer_core::Id;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::{
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct Preferences {
    path: PathBuf,
    pub zoom: f32,
    hidden_strips: BTreeSet<Id>,
    dirty: Option<Instant>,
}
impl Preferences {
    pub fn load(path: PathBuf) -> Self {
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .unwrap_or_default();
        let zoom = saved["zoom"]
            .as_f64()
            .map(|v| v as f32)
            .filter(|v| v.is_finite() && (0.1..=10.0).contains(v))
            .unwrap_or(1.0);
        Self {
            path,
            zoom,
            hidden_strips: saved["hidden_strips"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()?.parse().ok())
                .collect(),
            dirty: None,
        }
    }
    pub fn is_hidden(&self, strip: Id) -> bool {
        self.hidden_strips.contains(&strip)
    }
    pub fn set_hidden(&mut self, strip: Id, hidden: bool) {
        let changed = if hidden {
            self.hidden_strips.insert(strip)
        } else {
            self.hidden_strips.remove(&strip)
        };
        if changed {
            self.dirty = Some(Instant::now());
        }
    }
    pub fn update(&mut self, zoom: f32) -> io::Result<bool> {
        if zoom.is_finite() && (0.1..=10.0).contains(&zoom) && zoom != self.zoom {
            self.zoom = zoom;
            self.dirty = Some(Instant::now());
        }
        if self
            .dirty
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.flush()?;
        }
        Ok(self.dirty.is_some())
    }
    pub fn flush(&mut self) -> io::Result<()> {
        if self.dirty.is_none() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(
            &temporary,
            serde_json::to_vec(
                &serde_json::json!({"version":1,"zoom":self.zoom,"hidden_strips":self.hidden_strips}),
            )?,
        )?;
        std::fs::rename(temporary, &self.path)?;
        self.dirty = None;
        Ok(())
    }
}
impl Drop for Preferences {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            eprintln!("Could not save UI preferences: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collapsed_strips_survive_restart_and_restore_without_changing_zoom() {
        let root = std::env::temp_dir().join(format!("alixer-collapse-{}", Id::new_v4()));
        let path = root.join("ui.json");
        let id = Id::new_v4();
        {
            let mut prefs = Preferences::load(path.clone());
            prefs.update(1.5).unwrap();
            prefs.set_hidden(id, true);
        }
        {
            let mut prefs = Preferences::load(path.clone());
            assert_eq!(prefs.zoom, 1.5);
            assert!(prefs.is_hidden(id));
            prefs.set_hidden(id, false);
        }
        assert!(!Preferences::load(path.clone()).is_hidden(id));
        std::fs::write(
            &path,
            format!(r#"{{"zoom":1.5,"hidden_strips":["bad","{id}"]}}"#),
        )
        .unwrap();
        assert!(Preferences::load(path).is_hidden(id));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn zoom_survives_restart_and_invalid_preferences_fall_back() {
        let root = std::env::temp_dir().join(format!("alixer-zoom-{}", alixer_core::Id::new_v4()));
        let path = root.join("ui.json");
        {
            let mut prefs = Preferences::load(path.clone());
            assert_eq!(prefs.zoom, 1.0);
            prefs.update(1.75).unwrap();
        }
        assert_eq!(Preferences::load(path.clone()).zoom, 1.75);
        for invalid in ["broken", "{\"zoom\":0}", "{\"zoom\":100}"] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(Preferences::load(path.clone()).zoom, 1.0);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
