use super::*;
use std::os::unix::fs::symlink;

fn native_link(
    value: &str,
    directory: &std::path::Path,
    hostname: Option<&str>,
    local: crate::terminal::TerminalLocalFileCapabilities,
) -> Option<crate::terminal::HyperlinkTarget> {
    crate::terminal::HyperlinkTarget::resolve_osc8(
        value,
        directory,
        hostname,
        local,
        &crate::platform::macos_adapter_tests::local_filesystem(),
    )
}

#[test]
fn local_native_actions_are_inert_after_the_file_is_replaced() {
    let directory =
        std::env::temp_dir().join(format!("spaceterm-local-replaced-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    let replacement = directory.join("replacement.txt");
    fs::write(&file, b"first").unwrap();
    let local = native_link("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();

    fs::write(&replacement, b"replacement").unwrap();
    fs::rename(&replacement, &file).unwrap();

    assert_eq!(local.activation_url(LOCAL_FILES), None);
    assert_eq!(FilePreviewTarget::from_link(&local, LOCAL_FILES), None);
    assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).open_link);
    assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).file_preview);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn local_native_actions_are_inert_after_the_path_becomes_a_different_symlink() {
    let directory =
        std::env::temp_dir().join(format!("spaceterm-local-symlink-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    let other = directory.join("other.txt");
    fs::write(&file, b"first").unwrap();
    fs::write(&other, b"other").unwrap();
    let local = native_link("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();

    fs::remove_file(&file).unwrap();
    symlink(&other, &file).unwrap();

    assert_eq!(local.activation_url(LOCAL_FILES), None);
    assert_eq!(FilePreviewTarget::from_link(&local, LOCAL_FILES), None);
    assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).open_link);
    assert!(NativeContextActions::from_presence(LOCAL_FILES, false, Some(&local)).file_preview);
    fs::remove_dir_all(directory).unwrap();
}
