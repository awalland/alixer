use super::*;

#[test]
fn large_plugin_catalog_uses_its_own_response_limit() {
    let ports: Vec<_> = (0..16)
        .map(|index| {
            serde_json::json!({
                "index": index, "symbol": format!("control_{index}"),
                "name": format!("Plugin control parameter {index}"),
                "input": true, "audio": false, "default": 0.0,
                "min": 0.0, "max": 1.0, "toggle": false, "integer": false, "latency": false,
            })
        })
        .collect();
    let catalog = serde_json::Value::Array((0..512).map(|index| serde_json::json!({
        "uri": format!("urn:alixer:test:plugin:{index}"), "name": format!("Test plugin {index}"),
        "ports": ports, "required": [],
    })).collect());
    let frame = serde_json::to_vec(&Reply {
        version: VERSION,
        id: 1,
        revision: 0,
        result: Ok(Payload::Plugins(catalog.clone())),
    })
    .unwrap();
    assert!(frame.len() > MAX_FRAME);
    assert!(frame.len() + 1 < MAX_CATALOG_FRAME);
    assert_eq!(reply_frame_limit(&Command::Snapshot), MAX_FRAME);
    let (client, mut server) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || {
        let mut request = String::new();
        BufReader::new(server.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        server.write_all(&frame).unwrap();
        server.write_all(b"\n").unwrap();
    });
    assert_eq!(fetch_catalog(client).unwrap(), catalog);
    worker.join().unwrap();
}

#[test]
fn catalog_can_take_longer_than_the_control_connection_deadline() {
    let (client, mut server) = UnixStream::pair().unwrap();
    // Match the regular GUI connection's deadline; fetch_catalog must override it.
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let worker = std::thread::spawn(move || {
        let mut request = String::new();
        BufReader::new(server.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<Request>(&request).unwrap().command,
            Command::Plugins
        ));
        std::thread::sleep(Duration::from_millis(2200));
        serde_json::to_writer(
            &mut server,
            &Reply {
                version: VERSION,
                id: 1,
                revision: 0,
                result: Ok(Payload::Plugins(serde_json::json!([]))),
            },
        )
        .unwrap();
        server.write_all(b"\n").unwrap();
    });
    assert_eq!(fetch_catalog(client).unwrap(), serde_json::json!([]));
    worker.join().unwrap();
}

#[test]
fn ui_worker_reconnects_and_refreshes_revision_after_daemon_restart() {
    use std::os::unix::net::UnixListener;
    let dir = std::env::temp_dir().join(format!("alixer-reconnect-{}", Id::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("control.sock");
    let (commands, commands_rx) = mpsc::channel();
    let (events_tx, events) = mpsc::channel();
    let worker_path = path.clone();
    let worker = std::thread::spawn(move || {
        ipc_worker(worker_path, commands_rx, events_tx, egui::Context::default());
    });
    let mut app = test_app().0;
    app.rx = events;
    fn wait_for(app: &mut App, predicate: impl Fn(&App) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            app.receive_events();
            if predicate(app) { return; }
            assert!(std::time::Instant::now() < deadline, "UI did not reconnect");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Startup before the daemon exists must recover, too.
    wait_for(&mut app, |app| !app.connection_error.is_empty());
    for revision in [99, 3, 7] {
        let listener = UnixListener::bind(&path).unwrap();
        // Queue an edit before connecting; the worker must first read the new
        // daemon's revision rather than reuse the preceding process's revision.
        commands.send(Command::Edit(Edit::AddBus {
            name: "After restart".into(), aux: false, channels: vec!["FL".into()],
        })).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(server.try_clone().unwrap());
        for expected_snapshot in [true, false] {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            let result = if expected_snapshot {
                assert!(matches!(request.command, Command::Snapshot));
                Ok(Payload::Snapshot(Snapshot {
                    scene: Scene { revision, ..Default::default() },
                    active_streams: Some(vec![]), observation: Observation::default(),
                    conflicts: vec![], status: "Connected".into(), peaks: vec![],
                    channel_peaks: vec![], plugin_meters: vec![],
                }))
            } else {
                assert!(matches!(request.command, Command::Edit(_)));
                assert_eq!(request.revision, Some(revision));
                // Ordinary command errors should remain visible after reconnect.
                Err("Example command error".into())
            };
            serde_json::to_writer(&mut server, &Reply {
                version: VERSION, id: request.id, revision, result,
            }).unwrap();
            server.write_all(b"\n").unwrap();
        }
        wait_for(&mut app, |app| app.connection_error.is_empty()
            && app.snapshot.as_ref().is_some_and(|s| s.scene.revision == revision)
            && app.error == "Example command error");
        drop(reader);
        drop(server);
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        wait_for(&mut app, |app| !app.connection_error.is_empty());
    }
    drop(app);
    worker.join().unwrap();
    std::fs::remove_dir(dir).unwrap();
}
