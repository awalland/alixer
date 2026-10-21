#[cfg(test)]
mod ipc_tests;
mod output_names;
mod store;
use alixer_core::{
    reconcile::{self, Checkpoint},
    *,
};
use alixer_protocol::*;
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use store::{Saved, Store};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
struct Incoming {
    request: Request,
    reply: mpsc::Sender<Reply>,
}
fn private_dir(path: &std::path::Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)?;
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        bail!("Unsafe directory {}", path.display());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn main() -> Result<()> {
    migrate_legacy_state()?;
    let offline = std::env::args().any(|a| a == "--offline");
    let state = state_dir();
    private_dir(&state)?;
    let mut store = Store::open(&state.join("state.sqlite3"))?;
    let mut saved = store.load()?;
    // Device controls are authoritative across service downtime.
    saved.checkpoint.observation.outputs.clear();
    let runtime = runtime_dir();
    private_dir(&runtime)?;
    // flock covers startup and stale socket cleanup; a second service cannot unlink a live listener.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(runtime.join("lock"))?;
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("Alixer is already running");
    }
    let socket = runtime.join("control.sock");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let (tx, rx) = mpsc::sync_channel::<Incoming>(32);
    let initial = Snapshot {
        active_streams: Some(vec![]),
        scene: saved.scene.clone(),
        observation: Observation::default(),
        conflicts: saved.conflicts.clone(),
        status: if offline {
            "Offline mode".into()
        } else {
            "Connecting to PipeWire".into()
        },
        peaks: vec![],
        channel_peaks: vec![],
    };
    let public = Arc::new(Mutex::new(initial));
    let published = public.clone();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            match connection {
                Ok(stream) => {
                    let tx = tx.clone();
                    let public = published.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, tx, public) {
                            eprintln!("IPC: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("IPC listener: {e}");
                    break;
                }
            }
        }
    });
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let mut backend = if offline {
        None
    } else {
        Some(alixer_audio::Backend::start())
    };
    let mut actual = Observation::default();
    let mut status = if offline {
        "Offline mode".to_string()
    } else {
        "Connecting to PipeWire".to_string()
    };
    let mut peaks = vec![];
    let mut last_observation = String::new();
    let mut last_apply = String::new();
    let mut last_publish = Instant::now();
    let mut channel_peaks = vec![];
    let mut output_controls = vec![];
    if let Err(e) = output_names::write(&saved.scene, &output_names::config_path()) {
        eprintln!("Output names: {e:#}");
    }
    write_guard(&state, &saved)?;
    eprintln!("Alixer listening on {}", socket.display());
    while !STOP.load(Ordering::Relaxed) {
        if let Some(backend) = &backend {
            let mut latest = None;
            let mut state_changed = false;
            while let Ok(mut update) = backend.updates.try_recv() {
                if !update.states.is_empty() {
                    capture_states(&mut saved, std::mem::take(&mut update.states));
                    state_changed = true;
                }
                latest = Some(update);
            }
            if state_changed {
                store.save(&saved, None)?;
            }
            if let Some(update) = latest {
                if !update.states.is_empty() {
                    capture_states(&mut saved, update.states);
                    store.save(&saved, None)?;
                }
                status = update.status;
                peaks = update.peaks;
                channel_peaks = update.channel_peaks;
                if status.starts_with("Disconnected") {
                    last_apply.clear();
                }
                // A disconnect is not a new baseline and cannot erase a usable observation.
                if status.starts_with("Connected") || status.starts_with("Audio error") {
                    actual = update.observation;
                    let signature = serde_json::to_string(&actual)?;
                    if signature != last_observation {
                        let reconciled = reconcile::reconcile(
                            &saved.scene,
                            &saved.checkpoint,
                            &actual,
                            &saved.conflicts,
                        );
                        saved.scene = reconciled.scene;
                        saved.conflicts = reconciled.conflicts;
                        saved.checkpoint = Checkpoint {
                            observation: actual.clone(),
                            bindings: reconciled.bindings,
                        };
                        store.save(&saved, None)?;
                        write_guard(&state, &saved)?;
                        last_observation = signature;
                    }
                }
            }
        }
        while let Ok(incoming) = rx.try_recv() {
            let request = incoming.request;
            let result: Result<Payload> = (|| {
                if request.version != VERSION {
                    bail!("Unsupported protocol version {}", request.version);
                }
                match &request.command {
                    Command::Snapshot => Ok(Payload::Snapshot(Snapshot {
                        active_streams: Some(active_stream_ids(&actual, &saved.checkpoint)),
                        scene: saved.scene.clone(),
                        observation: actual.clone(),
                        conflicts: saved.conflicts.clone(),
                        status: status.clone(),
                        peaks: peaks.clone(),
                        channel_peaks: channel_peaks.clone(),
                    })),
                    Command::Plugins => {
                        unreachable!("Catalog discovery is handled by the IPC worker")
                    }
                    Command::Subscribe => Ok(Payload::Accepted),
                    Command::ApplyOutputNames => {
                        if offline {
                            bail!("Cannot restart WirePlumber in offline mode");
                        }
                        output_names::write(&saved.scene, &output_names::config_path())?;
                        output_names::apply()?;
                        Ok(Payload::Accepted)
                    }
                    Command::Edit(edit) => {
                        if request.revision != Some(saved.scene.revision) {
                            bail!("Stale revision; refresh before editing");
                        }
                        let mut next = saved.clone();
                        if let Edit::Gain { strip, .. } | Edit::Mute { strip, .. } = edit {
                            if let Some(key) = next
                                .scene
                                .strip(*strip)
                                .filter(|s| s.kind == alixer_core::StripKind::Output)
                                .and_then(|s| s.output_key.as_ref())
                            {
                                if !status.starts_with("Connected")
                                    || !actual.outputs.iter().any(|o| &o.key == key)
                                {
                                    bail!("Output is offline; device controls are unavailable");
                                }
                            }
                        }
                        match edit {
                            Edit::AddInsert { strip, uri } => {
                                let s = next.scene.strip(*strip).context("Unknown strip")?;
                                alixer_lv2::validate(
                                    &Insert {
                                        id: Id::new_v4(),
                                        uri: uri.clone(),
                                        bypass: false,
                                        controls: Default::default(),
                                        state: None,
                                    },
                                    s.channels.len(),
                                )?;
                            }
                            Edit::SetControl {
                                strip,
                                insert,
                                symbol,
                                value,
                            } => {
                                let s = next.scene.strip(*strip).context("Unknown strip")?;
                                let slot = s
                                    .inserts
                                    .iter()
                                    .find(|i| i.id == *insert)
                                    .context("Unknown insert")?;
                                let plugin = alixer_lv2::validate(slot, s.channels.len())?;
                                let port = plugin
                                    .ports
                                    .iter()
                                    .find(|p| &p.symbol == symbol && p.control && p.input)
                                    .context("Unknown control")?;
                                if !value.is_finite() || *value < port.min || *value > port.max {
                                    bail!("Control outside plugin range");
                                }
                            }
                            _ => {}
                        }
                        next.scene.edit(edit.clone())?;
                        store.save(&next, Some(&serde_json::to_string(&request.command)?))?;
                        saved = next;
                        if matches!(edit, Edit::Rename { .. } | Edit::ResetOutputName { .. }) {
                            output_names::write(&saved.scene, &output_names::config_path())?;
                        }
                        write_guard(&state, &saved)?;
                        let control = match edit {
                            Edit::Gain { strip, value } => Some((*strip, Some(*value), None)),
                            Edit::Mute { strip, value } => Some((*strip, None, Some(*value))),
                            _ => None,
                        };
                        if let Some((id, gain, mute)) = control {
                            if let Some(key) = saved
                                .scene
                                .strip(id)
                                .filter(|s| s.kind == alixer_core::StripKind::Output)
                                .and_then(|s| s.output_key.clone())
                            {
                                output_controls.push(alixer_audio::OutputControl {
                                    key,
                                    gain,
                                    mute,
                                    balance: saved
                                        .scene
                                        .strip(id)
                                        .map(|s| s.output_balance.clone())
                                        .unwrap_or_default(),
                                });
                            }
                        }
                        Ok(Payload::Accepted)
                    }
                    Command::Resolve { conflict, restore } => {
                        if request.revision != Some(saved.scene.revision) {
                            bail!("Stale revision; refresh before editing");
                        }
                        let c = saved
                            .conflicts
                            .iter()
                            .find(|c| c.id == *conflict)
                            .cloned()
                            .context("Unknown conflict")?;
                        let mut next = saved.clone();
                        if !restore {
                            if let Some(strip) = c.strip {
                                reconcile::replace_outputs(&mut next.scene, strip, &c.current);
                            }
                        }
                        next.conflicts.retain(|c| c.id != *conflict);
                        next.scene.revision += 1;
                        next.scene.validate()?;
                        store.save(&next, Some(&serde_json::to_string(&request.command)?))?;
                        saved = next;
                        Ok(Payload::Accepted)
                    }
                }
            })();
            let reply = Reply {
                version: VERSION,
                id: request.id,
                revision: saved.scene.revision,
                result: result.map_err(|e| format!("{e:#}")),
            };
            let _ = incoming.reply.send(reply);
        }
        if let Some(backend) = &backend {
            let suspended: BTreeSet<_> = saved.conflicts.iter().filter_map(|c| c.strip).collect();
            let key = format!(
                "{}:{:?}:{:?}",
                serde_json::to_string(&saved.scene)?,
                saved.checkpoint.bindings,
                suspended
            );
            if key != last_apply || !output_controls.is_empty() {
                backend.commands.send(alixer_audio::Apply {
                    scene: saved.scene.clone(),
                    bindings: saved.checkpoint.bindings.clone(),
                    suspended,
                    state_dir: state.clone(),
                    output_controls: std::mem::take(&mut output_controls),
                })?;
                last_apply = key;
            }
        }
        if last_publish.elapsed() >= Duration::from_millis(33) {
            *public
                .lock()
                .map_err(|_| anyhow::anyhow!("Snapshot lock poisoned"))? = Snapshot {
                active_streams: Some(active_stream_ids(&actual, &saved.checkpoint)),
                scene: saved.scene.clone(),
                observation: actual.clone(),
                conflicts: saved.conflicts.clone(),
                status: status.clone(),
                peaks: peaks.clone(),
                channel_peaks: channel_peaks.clone(),
            };
            last_publish = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(backend) = &mut backend {
        capture_states(&mut saved, backend.shutdown());
    }
    store.save(&saved, None)?;
    // No application mute changes: persistent ingress sinks become silent when this
    // process's DSP links disappear, preserving explicit outside mute independently.
    std::fs::remove_file(socket)?;
    Ok(())
}
fn active_stream_ids(actual: &Observation, checkpoint: &Checkpoint) -> Vec<Id> {
    actual.streams.iter().filter_map(|stream| {
        checkpoint.bindings.get(&reconcile::runtime_key(&stream.runtime)).copied()
    }).collect()
}

fn capture_states(saved: &mut Saved, states: Vec<alixer_audio::PluginState>) {
    for state in states {
        if let Some(strip) = saved.scene.strips.iter_mut().find(|s| s.id == state.strip) {
            if let Some(insert) = strip.inserts.iter_mut().find(|i| i.id == state.insert) {
                insert.state = Some(state.path);
            }
        }
    }
}
fn serve(
    stream: UnixStream,
    tx: mpsc::SyncSender<Incoming>,
    public: Arc<Mutex<Snapshot>>,
) -> Result<()> {
    serve_with_catalog(stream, tx, public, || {
        Ok(serde_json::to_value(alixer_lv2::catalog()?)?)
    })
}
fn serve_with_catalog(
    mut stream: UnixStream,
    tx: mpsc::SyncSender<Incoming>,
    public: Arc<Mutex<Snapshot>>,
    catalog: impl Fn() -> Result<serde_json::Value>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        let count = reader
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_line(&mut line)?;
        if count == 0 {
            return Ok(());
        }
        if count > MAX_FRAME {
            bail!("IPC frame exceeds limit");
        }
        let request: Request = serde_json::from_str(&line)?;
        if matches!(request.command, Command::Plugins) {
            // Lilv scans on this connection's worker, leaving reconciliation,
            // edits, snapshots and audio controls running on the main loop.
            let result = if request.version != VERSION {
                Err(format!("Unsupported protocol version {}", request.version))
            } else {
                catalog()
                    .map(Payload::Plugins)
                    .map_err(|e| format!("{e:#}"))
            };
            let mut reply = Reply {
                version: VERSION,
                id: request.id,
                revision: public
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Snapshot lock poisoned"))?
                    .scene
                    .revision,
                result,
            };
            let mut frame = serde_json::to_vec(&reply)?;
            if frame.len() + 1 > MAX_CATALOG_FRAME {
                reply.result = Err("Plugin catalog exceeds the 32 MiB response limit".into());
                frame = serde_json::to_vec(&reply)?;
            }
            stream.write_all(&frame)?;
            stream.write_all(b"\n")?;
            continue;
        }
        let subscribe = matches!(request.command, Command::Subscribe);
        if subscribe {
            if request.version != VERSION {
                bail!("Unsupported protocol version");
            }
            loop {
                let snapshot = public
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Snapshot lock poisoned"))?
                    .clone();
                let reply = Reply {
                    version: VERSION,
                    id: request.id,
                    revision: snapshot.scene.revision,
                    result: Ok(Payload::Snapshot(snapshot)),
                };
                serde_json::to_writer(&mut stream, &reply)?;
                stream.write_all(b"\n")?;
                std::thread::sleep(Duration::from_millis(33));
            }
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(Incoming {
            request,
            reply: reply_tx,
        })?;
        let reply = reply_rx.recv_timeout(Duration::from_secs(10))?;
        serde_json::to_writer(&mut stream, &reply)?;
        stream.write_all(b"\n")?;
    }
}
fn write_guard(dir: &std::path::Path, saved: &Saved) -> Result<()> {
    let streams:Vec<_>=saved.scene.strips.iter().filter_map(|s|s.identity.as_ref().map(|i|serde_json::json!({"application":i.application,"name":i.name,"target":format!("alixer.input.{}",s.id)}))).collect();
    let path = dir.join("guard.json.tmp");
    std::fs::write(&path, serde_json::to_vec(&streams)?)?;
    std::fs::rename(path, dir.join("guard.json"))?;
    Ok(())
}
