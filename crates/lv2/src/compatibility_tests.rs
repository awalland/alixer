use super::*;

#[test]
fn unsupported_error_does_not_blame_supported_features() {
    let plugin = Plugin {
        uri: "urn:test:unsupported".into(),
        name: "Test effect".into(),
        ports: vec![],
        required: vec![
            "http://lv2plug.in/ns/ext/urid#map".into(),
            "Unsupported port type: events".into(),
        ],
        unsupported: vec!["Unsupported port type: events".into()],
    };
    let error = check_compatible(&plugin, 2).unwrap_err().to_string();
    assert!(error.contains("Unsupported port type: events"));
    assert!(!error.contains("urid#map"));
    assert!(!plugin.compatible(2));
}

#[test]
fn supported_features_are_accepted_and_layout_errors_are_specific() {
    let plugin = Plugin {
        uri: "urn:test:supported".into(),
        name: "Test effect".into(),
        ports: vec![],
        required: vec!["http://lv2plug.in/ns/ext/urid#map".into()],
        unsupported: vec![],
    };
    assert!(check_compatible(&plugin, 0).is_ok());
    assert!(
        check_compatible(&plugin, 2)
            .unwrap_err()
            .to_string()
            .contains("0 audio inputs and 0 outputs")
    );
}
