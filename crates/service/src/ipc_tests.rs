use super::*;

fn request(stream: &mut UnixStream, command: Command) {
    serde_json::to_writer(
        &mut *stream,
        &Request {
            version: VERSION,
            id: 1,
            revision: None,
            command,
        },
    )
    .unwrap();
    stream.write_all(b"\n").unwrap();
}
fn reply(stream: &mut UnixStream) -> Reply {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn scanning_plugins_does_not_occupy_the_control_loop() {
    let (tx, rx) = mpsc::sync_channel(32);
    let public = Arc::new(Mutex::new(Snapshot {
        active_streams: None,
        scene: Scene::default(),
        observation: Observation::default(),
        conflicts: vec![],
        status: "Test".into(),
        peaks: vec![],
        channel_peaks: vec![],
        plugin_meters: vec![],
    }));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (mut catalog_client, catalog_server) = UnixStream::pair().unwrap();
    let worker_tx = tx.clone();
    let worker_public = public.clone();
    let scan = std::thread::spawn(move || {
        serve_with_catalog(catalog_server, worker_tx, worker_public, || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(serde_json::json!([]))
        })
    });
    request(&mut catalog_client, Command::Plugins);
    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        rx.try_recv().is_err(),
        "Plugin scan must not enter the control queue"
    );

    let (mut control_client, control_server) = UnixStream::pair().unwrap();
    let control = std::thread::spawn(move || serve(control_server, tx, public));
    request(&mut control_client, Command::Snapshot);
    let incoming = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(incoming.request.command, Command::Snapshot));
    incoming
        .reply
        .send(Reply {
            version: VERSION,
            id: 1,
            revision: 0,
            result: Ok(Payload::Accepted),
        })
        .unwrap();
    assert!(matches!(
        reply(&mut control_client).result,
        Ok(Payload::Accepted)
    ));
    release_tx.send(()).unwrap();
    assert!(matches!(
        reply(&mut catalog_client).result,
        Ok(Payload::Plugins(_))
    ));
    drop(control_client);
    drop(catalog_client);
    control.join().unwrap().unwrap();
    scan.join().unwrap().unwrap();
}
