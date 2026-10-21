use alixer_protocol::*;
use anyhow::{Context, Result, bail};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};
fn exchange(command: Command, revision: Option<u64>) -> Result<Reply> {
    let frame_limit = reply_frame_limit(&command);
    let mut stream = UnixStream::connect(runtime_dir().join("control.sock"))
        .context("Cannot reach alixerd; start alixer.service")?;
    stream.set_read_timeout(Some(if matches!(command, Command::Plugins) {
        PLUGIN_SCAN_TIMEOUT
    } else {
        std::time::Duration::from_secs(15)
    }))?;
    serde_json::to_writer(
        &mut stream,
        &Request {
            version: VERSION,
            id: 1,
            revision,
            command,
        },
    )?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream)
        .take((frame_limit + 1) as u64)
        .read_line(&mut line)?;
    if line.len() > frame_limit {
        bail!("Service response exceeded limit");
    }
    let reply: Reply = serde_json::from_str(&line)?;
    if reply.version != VERSION {
        bail!(
            "Service protocol {} is incompatible with client protocol {VERSION}; restart alixerd after rebuilding",
            reply.version
        );
    }
    Ok(reply)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("snapshot") {
        "enable" => {
            integration(true)?;
            println!(
                "Integration installed. Run systemctl --user daemon-reload, restart WirePlumber, then enable --now alixer.service."
            );
        }
        "disable" => {
            integration(false)?;
            println!(
                "Integration removed. Stop alixer.service and restart WirePlumber to resume desktop routing."
            );
        }
        "diagnose" => {
            println!(
                "socket: {}\nstate: {}",
                runtime_dir().display(),
                state_dir().display()
            );
            match alixer_lv2::catalog() {
                Ok(p) => println!("LV2: {} supported metadata entries", p.len()),
                Err(e) => println!("LV2: {e:#}"),
            }
            match exchange(Command::Snapshot, None) {
                Ok(r) => println!("{}", serde_json::to_string_pretty(&r)?),
                Err(e) => println!("service: {e:#}"),
            }
        }
        name => {
            let command = match name {
                "snapshot" => Command::Snapshot,
                "plugins" => Command::Plugins,
                "apply-output-names" => Command::ApplyOutputNames,
                "edit" => Command::Edit(serde_json::from_str(
                    args.get(1).context("Usage: alixerctl edit '<Edit JSON>'")?,
                )?),
                "resolve" => Command::Resolve {
                    conflict: args.get(1).context("Missing conflict UUID")?.parse()?,
                    restore: args.get(2).map(String::as_str) == Some("restore"),
                },
                _ => bail!(
                    "Commands: snapshot, plugins, apply-output-names, edit JSON, resolve UUID keep|restore, diagnose, enable, disable"
                ),
            };
            let revision = if matches!(command, Command::Edit(_) | Command::Resolve { .. }) {
                Some(exchange(Command::Snapshot, None)?.revision)
            } else {
                None
            };
            let reply = exchange(command, revision)?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
            if let Err(e) = reply.result {
                bail!("{e}");
            }
        }
    }
    Ok(())
}
fn integration(enable: bool) -> Result<()> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let root = config.join("wireplumber");
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    let script = data.join("wireplumber/scripts/alixer-guard.lua");
    let conf = root.join("wireplumber.conf.d/90-alixer.conf");
    if enable {
        std::fs::create_dir_all(script.parent().unwrap())?;
        std::fs::create_dir_all(conf.parent().unwrap())?;
        std::fs::write(
            script,
            include_str!("../../../integration/alixer-guard.lua"),
        )?;
        std::fs::write(conf, include_str!("../../../integration/90-alixer.conf"))?;
        let unit = config.join("systemd/user/alixer.service");
        std::fs::create_dir_all(unit.parent().unwrap())?;
        let daemon = std::env::current_exe()?.with_file_name("alixerd");
        let text = include_str!("../../../integration/alixer.service")
            .replace("@ALIXERD@", &daemon.to_string_lossy());
        std::fs::write(unit, text)?;
        // Replace the old installation rather than loading two fallback guards.
        for legacy in [
            data.join("wireplumber/scripts/lixer-guard.lua"),
            root.join("wireplumber.conf.d/90-lixer.conf"),
            config.join("systemd/user/lixer.service"),
        ] {
            if legacy.exists() {
                std::fs::remove_file(legacy)?;
            }
        }
    } else {
        for path in [script, conf] {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}
