use super::*;
struct PollOnly(crate::live::link::Capabilities);
impl EmulatorLink for PollOnly {
    fn capabilities(&self) -> &crate::live::link::Capabilities {
        &self.0
    }
    fn call(&mut self, _: &str, _: Value) -> Result<Value, LinkError> {
        panic!("unsupported timing must have no effects")
    }
}
#[test]
fn unsupported_frame_tap_on_poll_only_runtime_has_no_effects() {
    let mut caps = crate::live::link::Capabilities::empty();
    caps.methods = vec![
        "step_input_polls".into(),
        "set_input".into(),
        "pause".into(),
    ];
    caps.identity.host_features = vec!["scoped_controller_input".into()];
    let mut link = PollOnly(caps);
    assert!(tap_axes(&mut link, 0, &[], &BTreeMap::new(), 1, 0).is_err());
    assert!(tap_axes_in_unit(
        &mut link,
        0,
        &[],
        &BTreeMap::new(),
        1,
        0,
        StepUnit::Instructions
    )
    .is_err());
}
