//! Per-user activation, using package-owned assets when installed under a prefix.
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const SCRIPT: &str = include_str!("../../../integration/alixer-guard.lua");
const CONF: &str = include_str!("../../../integration/90-alixer.conf");
const UNIT: &str = include_str!("../../../integration/alixer.service");

pub fn configure(enable: bool) -> Result<()> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    configure_at(enable, &config, &data, &std::env::current_exe()?)
}

fn managed_unit(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let (before, after) = UNIT.split_once("@ALIXERD@").unwrap();
    text.starts_with(before) && text.ends_with(after)
}
fn managed_script(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| {
        text.starts_with("-- Alixer admission/fallback guard.")
            && text.contains("alixer/prevent-fallback")
    })
}
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn configure_at(enable: bool, config: &Path, data: &Path, exe: &Path) -> Result<()> {
    let script = data.join("wireplumber/scripts/alixer-guard.lua");
    let conf = config.join("wireplumber/wireplumber.conf.d/90-alixer.conf");
    let unit = config.join("systemd/user/alixer.service");
    if !enable {
        remove_if_present(&conf)?;
        if managed_script(&script) {
            remove_if_present(&script)?;
        }
        return Ok(());
    }
    let prefix = exe
        .parent()
        .and_then(Path::parent)
        .context("Cannot determine install prefix")?;
    let packaged_unit = prefix.join("lib/systemd/user/alixer.service");
    let packaged_script = prefix.join("share/wireplumber/scripts/alixer-guard.lua");
    let packaged_conf = prefix.join("share/alixer/wireplumber/90-alixer.conf");
    let packaged = packaged_unit.is_file() || packaged_script.is_file() || packaged_conf.is_file();
    if packaged {
        if !packaged_unit.is_file() || !packaged_script.is_file() || !packaged_conf.is_file() {
            bail!(
                "Alixer package is incomplete; reinstall it to restore its user service and WirePlumber assets"
            );
        }
        for (path, owned) in [
            (&unit, managed_unit(&unit)),
            (&script, managed_script(&script)),
        ] {
            if path.exists() && !owned {
                bail!(
                    "Custom override {} shadows the installed Alixer package; move it aside before enabling integration",
                    path.display()
                );
            }
        }
        let text = std::fs::read(&packaged_conf)?;
        std::fs::create_dir_all(conf.parent().unwrap())?;
        std::fs::write(&conf, text)?;
        remove_if_present(&unit)?;
        remove_if_present(&script)?;
    } else {
        // Standalone developer builds retain their existing self-contained setup.
        std::fs::create_dir_all(script.parent().unwrap())?;
        std::fs::create_dir_all(conf.parent().unwrap())?;
        std::fs::create_dir_all(unit.parent().unwrap())?;
        std::fs::write(&script, SCRIPT)?;
        std::fs::write(&conf, CONF)?;
        std::fs::write(
            &unit,
            UNIT.replace(
                "@ALIXERD@",
                &exe.with_file_name("alixerd").to_string_lossy(),
            ),
        )?;
    }
    for legacy in [
        data.join("wireplumber/scripts/lixer-guard.lua"),
        config.join("wireplumber/wireplumber.conf.d/90-lixer.conf"),
        config.join("systemd/user/lixer.service"),
    ] {
        remove_if_present(&legacy)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_activation_uses_system_assets_and_replaces_development_copies() {
        let root =
            std::env::temp_dir().join(format!("alixer-install-{}", alixer_core::Id::new_v4()));
        let config = root.join("config");
        let data = root.join("data");
        let exe = root.join("usr/bin/alixerctl");
        configure_at(true, &config, &data, &exe).unwrap();
        assert!(config.join("systemd/user/alixer.service").is_file());
        assert!(data.join("wireplumber/scripts/alixer-guard.lua").is_file());
        for (path, content) in [
            (
                root.join("usr/lib/systemd/user/alixer.service"),
                UNIT.replace("@ALIXERD@", "/usr/bin/alixerd"),
            ),
            (
                root.join("usr/share/wireplumber/scripts/alixer-guard.lua"),
                SCRIPT.into(),
            ),
            (
                root.join("usr/share/alixer/wireplumber/90-alixer.conf"),
                CONF.into(),
            ),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        configure_at(true, &config, &data, &exe).unwrap();
        assert!(!config.join("systemd/user/alixer.service").exists());
        assert!(!data.join("wireplumber/scripts/alixer-guard.lua").exists());
        assert!(
            config
                .join("wireplumber/wireplumber.conf.d/90-alixer.conf")
                .is_file()
        );
        configure_at(false, &config, &data, &exe).unwrap();
        assert!(
            !config
                .join("wireplumber/wireplumber.conf.d/90-alixer.conf")
                .exists()
        );
        assert!(root.join("usr/lib/systemd/user/alixer.service").is_file());
        std::fs::create_dir_all(config.join("systemd/user")).unwrap();
        let custom = config.join("systemd/user/alixer.service");
        std::fs::write(&custom, "custom unit").unwrap();
        assert!(configure_at(true, &config, &data, &exe).is_err());
        assert_eq!(std::fs::read_to_string(custom).unwrap(), "custom unit");
        std::fs::remove_dir_all(root).unwrap();
    }
}
