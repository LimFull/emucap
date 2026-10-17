use super::FeatureCapabilities;
use serde_json::json;

#[test]
fn flycast_requires_native_renderer_fence_independent_of_build_label() {
    for features in [
        json!(null),
        json!([]),
        json!(["controlled_start"]),
        json!("native_renderer_fence"),
        json!({"native_renderer_fence": true}),
    ] {
        let hello = json!({
            "adapter": "flycast-native", "build": "future-build+fence-claimed",
            "host_features": features,
        });
        let error = FeatureCapabilities::from_hello(&hello, &[], &[]).unwrap_err();
        assert!(error.to_string().contains("flycast-patch-required"));
    }
    assert!(
        FeatureCapabilities::from_hello(&json!({"adapter":"flycast-native"}), &[], &[],).is_err()
    );
    assert!(FeatureCapabilities::from_hello(
        &json!({"adapter":"flycast-native", "host_features":["native_renderer_fence"]}),
        &[],
        &[],
    )
    .is_ok());
}

#[test]
fn renderer_fence_requirement_is_specific_to_flycast() {
    for hello in [json!({}), json!({"adapter":"other-native"})] {
        assert!(FeatureCapabilities::from_hello(&hello, &[], &[]).is_ok());
    }
}

#[test]
fn scoped_input_requires_axes_and_native_cleanup() {
    let hello = json!({"host_features":["scoped_controller_input"],
        "input_axes":{"semantics":"complete_persistent_controller_state"},
        "temporal_cancellation_capability":{"methods":["step"],"control_service_ms":25,"stop_host_ms":2000}});
    let methods = ["set_input", "step", "cancel_operation"].map(String::from);
    assert!(FeatureCapabilities::from_hello(&hello, &methods, &[]).is_ok());
    for missing in &methods {
        let incomplete = methods
            .iter()
            .filter(|m| *m != missing)
            .cloned()
            .collect::<Vec<_>>();
        assert!(FeatureCapabilities::from_hello(&hello, &incomplete, &[]).is_err());
    }
    let mut malformed = hello;
    malformed["input_axes"] = json!(null);
    assert!(FeatureCapabilities::from_hello(&malformed, &methods, &[]).is_err());
}
