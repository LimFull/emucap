use super::*;

/// The core's pointer-to-pixel mapping in `MouseTracker::Update` and its bottom-screen offset.
fn core_bottom_pixel(pointer: (i16, i16), width: u32, height: u32) -> (i64, i64) {
    let axis = |value: i16, size: u32| {
        ((f32::from(value) + 32767.0) / (32767.0 * 2.0) * size as f32) as i64
    };
    let left = (i64::from(width) - TOUCH_WIDTH as i64) / 2;
    let top = i64::from(height) - TOUCH_HEIGHT as i64;
    (axis(pointer.0, width) - left, axis(pointer.1, height) - top)
}

#[test]
fn every_bottom_screen_pixel_round_trips_through_the_core_pointer_mapping() {
    for x in 0..TOUCH_WIDTH {
        for y in 0..TOUCH_HEIGHT {
            let pointer = touch_pointer(x, y, 400, 480);
            assert_ne!(pointer, (0, 0), "the core ignores a centered pointer");
            assert_eq!(core_bottom_pixel(pointer, 400, 480), (x as i64, y as i64));
        }
    }
}

#[test]
fn buttons_accept_3ds_names_and_reject_others() {
    let buttons = normalize_buttons(Some(&json!(["A", "zr", "circle_up", "enter"]))).unwrap();
    assert_eq!(
        buttons.into_iter().collect::<Vec<_>>(),
        ["a", "circle_up", "start", "zr"]
    );
    for invalid in [json!(["home"]), json!(["c"]), json!("a"), json!([1])] {
        assert!(matches!(
            normalize_buttons(Some(&invalid)),
            Err(AzaharError::BadParams(_))
        ));
    }
}

#[test]
fn virtual_access_is_bounded_by_length_and_address_space() {
    assert_eq!(
        virtual_address(&json!({"memory_type":"virtual", "address":"0x08000000"}), 4).unwrap(),
        0x0800_0000
    );
    assert_eq!(
        virtual_address(&json!({"address":0xffff_fffc_u64}), 4).unwrap(),
        0xffff_fffc
    );
    for (params, length) in [
        (json!({"address":0xffff_fffd_u64}), 4),
        (json!({"address":0}), 0),
        (json!({"address":0}), MAX_MEMORY_BYTES + 1),
        (json!({"memory_type":"ram", "address":0}), 1),
        (json!({}), 1),
    ] {
        assert!(matches!(
            virtual_address(&params, length),
            Err(AzaharError::BadParams(_))
        ));
    }
}

#[test]
fn content_must_be_a_directly_loadable_title() {
    let dir = tempfile::tempdir().unwrap();
    for (name, ok) in [
        ("game.3ds", true),
        ("game.CCI", true),
        ("game.cia", false),
        ("game.zip", false),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, b"title").unwrap();
        assert_eq!(validate_content(&path).is_ok(), ok, "{name}");
    }
    assert!(validate_content(&dir.path().join("missing.3ds")).is_err());
}

#[test]
fn advertised_exceptions_validate_for_the_3ds_adapter() {
    let advertisement = crate::contracts::ContractAdvertisement::Reported(
        serde_json::from_value(crate::contracts::advertisement_value(ACTIVE_EXCEPTIONS)).unwrap(),
    );
    let methods = METHODS.iter().map(|m| (*m).to_string()).collect::<Vec<_>>();
    let status = crate::contracts::validate_advertisement(
        &advertisement,
        Some(ADAPTER),
        Some(SYSTEM),
        &methods,
    );
    assert_eq!(status.state, "validated", "{:?}", status.errors);
}

#[test]
fn breakpoint_snapshots_are_bounded_virtual_ranges() {
    use super::debug::parse_snapshots;
    assert_eq!(
        parse_snapshots(Some(&json!([
            "virtual:0x08000000:0x10",
            "virtual:0x100000:4"
        ])))
        .unwrap(),
        vec![(0x0800_0000, 0x10), (0x10_0000, 4)]
    );
    for invalid in [
        json!(["ram:0:4"]),
        json!(["virtual:0:0"]),
        json!(["virtual:0xfffffffe:4"]),
        json!(["virtual:0"]),
        json!(["virtual:0:0x4000", "virtual:0x4000:1"]),
        json!("virtual:0:4"),
    ] {
        assert!(parse_snapshots(Some(&invalid)).is_err(), "{invalid}");
    }
}

#[test]
fn every_breakpoint_kind_is_advertised_with_pause_choices() {
    let kinds = super::debug::breakpoint_kinds();
    let names = kinds
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["exec", "read", "write", "access"]);
    assert!(kinds
        .as_array()
        .unwrap()
        .iter()
        .all(|kind| kind["pause_on_hit"] == json!([true, false])));
}
