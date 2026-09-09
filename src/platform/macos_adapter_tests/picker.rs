use super::*;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
#[test]
fn listing_should_hide_dot_prefixed_names_before_metadata_probes() {
    let root = TestDirectory::new("hidden");
    let visible = root.path.join("visible");
    fs::create_dir(&visible).unwrap();
    symlink(root.path.join("missing-target"), root.path.join(".hidden")).unwrap();
    let filesystem = crate::platform::macos_adapter_tests::local_filesystem();

    let result = filesystem.list_directories(&root.path, true).unwrap();

    assert_eq!(
        result,
        vec![DirectoryPickerDirectoryEntry {
            name: String::from("visible"),
            path: visible,
        }]
    );
}

#[test]
fn listing_should_omit_broken_visible_symlinks() {
    let root = TestDirectory::new("broken-symlink");
    symlink(root.path.join("missing-target"), root.path.join("broken")).unwrap();
    let filesystem = crate::platform::macos_adapter_tests::local_filesystem();

    let result = filesystem.list_directories(&root.path, true).unwrap();

    assert!(result.is_empty());
}

#[test]
fn listing_should_follow_directory_symlinks_without_changing_their_spelling() {
    let root = TestDirectory::new("symlink");
    let target = root.path.join("target");
    let link = root.path.join("linked");
    fs::create_dir(&target).unwrap();
    symlink(&target, &link).unwrap();
    let filesystem = crate::platform::macos_adapter_tests::local_filesystem();

    let result = filesystem.list_directories(&root.path, true).unwrap();

    assert!(result.contains(&DirectoryPickerDirectoryEntry {
        name: String::from("linked"),
        path: link,
    }));
}

#[test]
fn listing_should_omit_non_utf8_names() {
    let invalid_name =
        std::ffi::OsString::from_vec(vec![b'i', b'n', b'v', b'a', b'l', b'i', b'd', 0xff]);

    let result = visible_entry_name(invalid_name, false);

    assert_eq!(result, None);
}

#[test]
fn final_validation_should_preserve_the_exact_path_and_capture_retained_identity() {
    let root = TestDirectory::new("validation");
    let target = root.path.join("target");
    let selected = root.path.join("selected-spelling");
    fs::create_dir(&target).unwrap();
    symlink(&target, &selected).unwrap();
    let filesystem = crate::platform::macos_adapter_tests::local_filesystem();

    let result = filesystem.validate_directory(&selected).unwrap();

    assert_eq!(
        (result.path(), result.identity()),
        (
            selected.as_path(),
            filesystem.validate_directory(&target).unwrap().identity(),
        )
    );
}
