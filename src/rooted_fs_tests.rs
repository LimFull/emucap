use super::rooted_fs::{OutputSlot, RootedDirectory};
use std::io::Read;

#[test]
fn nested_regular_member_is_read_from_the_admitted_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/member.bin"), b"selected").unwrap();

    let rooted = RootedDirectory::open(&root).unwrap();
    let mut file = rooted
        .open_regular_member(std::path::Path::new("nested/member.bin"))
        .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"selected");
}

#[cfg(unix)]
#[test]
fn admitted_root_handle_cannot_be_redirected_by_path_replacement() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    let moved = parent.path().join("moved");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("member.bin"), b"admitted").unwrap();
    let rooted = RootedDirectory::open(&root).unwrap();

    std::fs::rename(&root, &moved).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("member.bin"), b"replacement").unwrap();

    let bytes = rooted
        .read_bounded_regular_member(std::path::Path::new("member.bin"), 32)
        .unwrap();
    assert_eq!(bytes, b"admitted");
}

#[cfg(unix)]
#[test]
fn intermediate_member_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    let outside = parent.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("member.bin"), b"outside").unwrap();
    symlink(&outside, root.join("nested")).unwrap();

    let rooted = RootedDirectory::open(&root).unwrap();
    assert!(rooted
        .open_regular_member(std::path::Path::new("nested/member.bin"))
        .is_err());
}

#[test]
fn output_slot_atomically_replaces_an_ordinary_leaf() {
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("state.bin");
    std::fs::write(&destination, b"old").unwrap();

    let slot = OutputSlot::admit(&destination).unwrap();
    slot.publish(b"replacement").unwrap();

    assert_eq!(std::fs::read(&destination).unwrap(), b"replacement");
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn output_slot_rejects_a_prepositioned_symlink_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("target.bin");
    let destination = parent.path().join("state.bin");
    std::fs::write(&target, b"protected").unwrap();
    symlink(&target, &destination).unwrap();

    assert!(OutputSlot::admit(&destination).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"protected");
}

#[test]
fn output_slot_requires_an_existing_parent() {
    let parent = tempfile::tempdir().unwrap();
    assert!(OutputSlot::admit(&parent.path().join("missing/state.bin")).is_err());
    assert!(!parent.path().join("missing").exists());
}

#[cfg(unix)]
#[test]
fn output_slot_ancestor_substitution_cannot_redirect_publication() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let selected = root.path().join("selected");
    let admitted = root.path().join("admitted");
    let protected = root.path().join("protected");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&protected).unwrap();
    std::fs::write(protected.join("state.bin"), b"protected").unwrap();
    let slot = OutputSlot::admit(&selected.join("state.bin")).unwrap();

    std::fs::rename(&selected, &admitted).unwrap();
    symlink(&protected, &selected).unwrap();
    slot.publish(b"snapshot").unwrap();

    assert_eq!(
        std::fs::read(protected.join("state.bin")).unwrap(),
        b"protected"
    );
    assert_eq!(
        std::fs::read(admitted.join("state.bin")).unwrap(),
        b"snapshot"
    );
}
