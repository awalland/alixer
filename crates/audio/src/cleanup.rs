//! Explicit removal of Alixer-owned objects without relying on external PW tools.
use super::*;
use anyhow::{Context as _, bail};

type Properties = BTreeMap<String, String>;
type Entries = BTreeMap<(u32, String), String>;

fn owned(name: &str) -> bool {
    ["alixer.", "lixer."]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Requires the daemon to have stopped. Keeps unrelated nodes and metadata intact.
pub fn undeploy() -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context
        .connect_rc(None)
        .context("Cannot connect to PipeWire for cleanup")?;
    let registry = core.get_registry_rc()?;
    let nodes = Rc::new(RefCell::new(BTreeMap::<u32, Properties>::new()));
    let entries = Rc::new(RefCell::new(Entries::new()));
    let metadata: Rc<RefCell<Option<pw::metadata::Metadata>>> = Rc::default();
    let metadata_listener: Rc<RefCell<Option<Box<dyn Listener>>>> = Rc::default();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let nodes = nodes.clone();
            let entries = entries.clone();
            let metadata = metadata.clone();
            let metadata_listener = metadata_listener.clone();
            let registry = registry.clone();
            move |object| {
                let Some(props) = object.props.as_ref() else {
                    return;
                };
                if object.type_ == ObjectType::Node {
                    nodes.borrow_mut().insert(
                        object.id,
                        props.iter().map(|(k, v)| (k.into(), v.into())).collect(),
                    );
                } else if object.type_ == ObjectType::Metadata
                    && props.get("metadata.name") == Some("default")
                {
                    if let Ok(proxy) = registry.bind::<pw::metadata::Metadata, _>(object) {
                        let entries = entries.clone();
                        let listener = proxy
                            .add_listener_local()
                            .property(move |id, key, _, value| {
                                if let Some(key) = key {
                                    if let Some(value) = value {
                                        entries.borrow_mut().insert((id, key.into()), value.into());
                                    }
                                }
                                0
                            })
                            .register();
                        *metadata_listener.borrow_mut() = Some(Box::new(listener));
                        *metadata.borrow_mut() = Some(proxy);
                    }
                }
            }
        })
        .register();
    let failed = Rc::new(RefCell::new(None));
    let _core_listener = core
        .add_listener_local()
        .done({
            let mainloop = mainloop.clone();
            move |_, _| mainloop.quit()
        })
        .error({
            let failed = failed.clone();
            let mainloop = mainloop.clone();
            move |_, _, _, message| {
                *failed.borrow_mut() = Some(message.to_string());
                mainloop.quit();
            }
        })
        .register();
    let timed_out = Rc::new(Cell::new(false));
    let timer = mainloop.loop_().add_timer({
        let mainloop = mainloop.clone();
        let timed_out = timed_out.clone();
        move |_| {
            timed_out.set(true);
            mainloop.quit();
        }
    });
    timer
        .update_timer(Some(Duration::from_secs(5)), None)
        .into_result()?;
    let roundtrip = || -> Result<()> {
        core.sync(0)?;
        mainloop.run();
        if timed_out.get() {
            bail!("PipeWire cleanup timed out");
        }
        if let Some(error) = failed.borrow().as_ref() {
            bail!("PipeWire cleanup: {error}");
        }
        Ok(())
    };
    roundtrip()?;
    roundtrip()?; // Metadata binding and its initial properties arrive asynchronously.
    let nodes = nodes.borrow();
    if nodes.values().any(|props| {
        props.get("node.name").is_some_and(|name| {
            name.starts_with("alixer.engine.") || name.starts_with("lixer.engine.")
        })
    }) {
        bail!("Alixer is still processing audio; stop it before undeploying");
    }
    let managed: BTreeSet<u32> = nodes
        .iter()
        .filter_map(|(id, props)| {
            props
                .get("node.name")
                .filter(|name| owned(name))
                .map(|_| *id)
        })
        .collect();
    let node_ids: BTreeSet<String> = managed.iter().map(ToString::to_string).collect();
    let mut serials = BTreeSet::new();
    for id in &managed {
        if let Some(serial) = nodes[id].get("object.serial") {
            serials.insert(serial.clone());
        }
    }
    drop(nodes);
    if let Some(metadata) = metadata.borrow().as_ref() {
        for ((id, key), value) in entries.borrow().iter() {
            let managed_default = (key.starts_with("default.audio.")
                || key.starts_with("default.configured.audio."))
                && serde_json::from_str::<serde_json::Value>(value)
                    .ok()
                    .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(owned))
                    .unwrap_or(false);
            if owned(key)
                || managed_default
                || (key == "target.object" && (owned(value) || serials.contains(value)))
                || (key == "target.node" && (owned(value) || node_ids.contains(value)))
            {
                metadata.set_property(*id, key, None, None);
            }
        }
    }
    for id in managed {
        registry.destroy_global(id).into_result()?;
    }
    roundtrip()?;
    Ok(())
}
