use super::*;
#[test]
fn raw_axes_preserve_every_signed_byte_pair() {
    for x in -128..=127 {
        for y in -128..=127 {
            let state = ControllerState::decode(
                &json!({"buttons":["a","z"],"axes":{"stick_x":x,"stick_y":y}}),
            )
            .unwrap();
            assert_eq!(state.raw(), (0xa0, x, y));
            assert!(state.engaged);
        }
    }
}
#[test]
fn native_neutral_and_composition_are_distinct() {
    let native = ControllerState::decode(&json!({"buttons":[]})).unwrap();
    let neutral = ControllerState::decode(&json!({"buttons":[],"axes":{"stick_x":0}})).unwrap();
    assert!(!native.engaged);
    assert!(neutral.engaged);
    let right = ControllerState::decode(&json!({"buttons":["right"]})).unwrap();
    assert_eq!(right.raw(), (0, 80, 0));
    assert!(neutral.overlay(right).is_err());
    for invalid in [
        json!({"buttons":[],"axes":{"stick_x":128}}),
        json!({"buttons":[],"axes":{"stick_x":1.2}}),
        json!({"buttons":[],"axes":{"other":0}}),
        json!({"buttons":[],"port":1}),
    ] {
        assert!(ControllerState::decode(&invalid).is_err());
    }
    let diagonal = ControllerState::decode(&json!({"buttons":["analog_up","right"]})).unwrap();
    assert_eq!(diagonal.raw(), (0, 56, 56));
    let opposing =
        ControllerState::decode(&json!({"buttons":["left","right","up","down"]})).unwrap();
    assert_eq!(opposing.raw(), (0, 56, -56));
    let dpad =
        ControllerState::decode(&json!({"buttons":["dpad_up"],"axes":{"stick_x":37}})).unwrap();
    assert_eq!(dpad.raw(), (8, 37, 0));
}
