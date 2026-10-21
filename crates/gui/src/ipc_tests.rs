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
