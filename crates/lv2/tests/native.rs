use alixer_core::{Id, Insert};
use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Duration};
#[test]
fn native_plugin_controls_worker_and_state_roundtrip() {
    if std::env::var_os("ALIXER_LILV_LIBRARY").is_none() {
        eprintln!("Set ALIXER_LILV_LIBRARY to run the native LV2 integration test");
        return;
    }
    let root = std::env::temp_dir().join(format!("alixer-lv2-{}", Id::new_v4()));
    let bundle = root.join("plugins/gain.lv2");
    std::fs::create_dir_all(&bundle).unwrap();
    let result = Command::new("rustc")
        .args(["--edition=2024", "--crate-type=cdylib", "-o"])
        .arg(bundle.join("gain.so"))
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixture.rs"))
        .status()
        .unwrap();
    assert!(result.success());
    std::fs::write(bundle.join("manifest.ttl"),"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n<urn:alixer:test:gain> a lv2:Plugin ; lv2:binary <gain.so> ; rdfs:seeAlso <gain.ttl> .").unwrap();
    let ttl = r#"@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix atom: <http://lv2plug.in/ns/ext/atom#> .
<urn:alixer:test:gain> a lv2:Plugin ; doap:name "Alixer test gain" ;
lv2:requiredFeature <http://lv2plug.in/ns/ext/urid#map>, <http://lv2plug.in/ns/ext/worker#schedule> ;
lv2:extensionData <http://lv2plug.in/ns/ext/state#interface>, <http://lv2plug.in/ns/ext/worker#interface> ;
lv2:port [ a lv2:InputPort, lv2:AudioPort ; lv2:index 0 ; lv2:symbol "input" ; lv2:name "Input" ],
[ a lv2:OutputPort, lv2:AudioPort ; lv2:index 1 ; lv2:symbol "output" ; lv2:name "Output" ],
[ a lv2:InputPort, lv2:ControlPort ; lv2:index 2 ; lv2:symbol "gain" ; lv2:name "Gain" ; lv2:default 1.0 ; lv2:minimum 0.0 ; lv2:maximum 2.0 ],
[ a lv2:OutputPort, lv2:ControlPort ; lv2:index 3 ; lv2:symbol "worker" ; lv2:name "Worker" ],
[ a lv2:InputPort, atom:AtomPort ; atom:bufferType atom:Sequence ; lv2:portProperty lv2:connectionOptional ; lv2:index 4 ; lv2:symbol "events_in" ; lv2:name "Events input" ],
[ a lv2:OutputPort, atom:AtomPort ; atom:bufferType atom:Sequence ; lv2:portProperty lv2:connectionOptional ; lv2:index 5 ; lv2:symbol "events_out" ; lv2:name "Events output" ] .
"#;
    std::fs::write(bundle.join("gain.ttl"), ttl).unwrap();
    std::fs::write(
        bundle.join("mandatory.ttl"),
        ttl.replace("urn:alixer:test:gain", "urn:alixer:test:mandatory")
            .replace("lv2:portProperty lv2:connectionOptional ;", ""),
    )
    .unwrap();
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().append(true).open(bundle.join("manifest.ttl")).unwrap(),"<urn:alixer:test:mandatory> a lv2:Plugin ; lv2:binary <gain.so> ; rdfs:seeAlso <mandatory.ttl> .").unwrap();
    // This integration test is a separate single-test process; no concurrent env readers.
    unsafe {
        std::env::set_var("LV2_PATH", root.join("plugins"));
    }
    let catalog = alixer_lv2::catalog().unwrap();
    assert_eq!(catalog.len(), 2);
    let metadata = catalog
        .iter()
        .find(|p| p.uri == "urn:alixer:test:gain")
        .unwrap();
    assert!(metadata.compatible(1));
    assert!(metadata.unsupported.is_empty());
    assert!(metadata.ports[4].optional && !metadata.ports[4].control && !metadata.ports[4].audio);
    assert!(
        !catalog
            .iter()
            .find(|p| p.uri == "urn:alixer:test:mandatory")
            .unwrap()
            .compatible(1)
    );
    let mut slot = Insert {
        id: Id::new_v4(),
        uri: "urn:alixer:test:gain".into(),
        bypass: false,
        controls: BTreeMap::from([("gain".into(), 0.25)]),
        state: None,
    };
    let mut plugin = alixer_lv2::Instance::new(&slot, 1, 48000.0, 512).unwrap();
    assert_eq!(
        plugin.parameters.len(),
        1,
        "Only actual input control ports become parameters"
    );
    let mut audio = vec![vec![1.0; 512]];
    plugin.run(&mut audio, 512);
    assert_eq!(audio[0][0], 0.25);
    std::thread::sleep(Duration::from_millis(30));
    audio[0].fill(1.0);
    plugin.run(&mut audio, 512);
    assert_eq!(audio[0][0], 0.75);
    assert_eq!(
        f32::from_bits(plugin.meters["worker"].load(std::sync::atomic::Ordering::Relaxed)),
        2.0
    );
    slot.state = Some(plugin.save_state(&root.join("state")).unwrap());
    drop(plugin);
    let mut restored = alixer_lv2::Instance::new(&slot, 1, 48000.0, 512).unwrap();
    audio[0].fill(1.0);
    restored.run(&mut audio, 512);
    assert_eq!(audio[0][0], 0.75);
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
}
