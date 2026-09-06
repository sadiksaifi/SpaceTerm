//! Structural regression gates for the shared application and UI boundary.
#[test]
fn local_filesystem_policy_cannot_reintroduce_native_identity_or_host_selection() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in [
        "domain/workspace_collection.rs",
        "platform/local_filesystem.rs",
        "platform/local_filesystem/picker.rs",
        "terminal/workspace_terminal_session_factory.rs",
        "terminal/native_services/hyperlink.rs",
        "terminal/native_services/quick_look.rs",
        "terminal/native_services.rs",
        "terminal/emulator.rs",
        "ui/workspace_manager.rs",
        "ui/workspace_picker.rs",
        "ui/pane_host.rs",
        "ui/tab_manager.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let source = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in [
            "std::os::unix",
            "MetadataExt",
            ".dev()",
            ".ino()",
            "raw_os_error",
            "libc::",
            "AsRawFd",
            "FromRawFd",
            "OpenOptionsExt",
            "PermissionsExt",
            "NativeWorkspacePickerFilesystem",
            "macos_local_identity",
            "target_os",
            "identity.device",
            "identity.inode",
            "identity.file",
        ] {
            assert!(!source.contains(forbidden), "{name} contains {forbidden}");
        }
    }
    for name in [
        "domain/workspace_collection.rs",
        "terminal/native_services/hyperlink.rs",
        "terminal/native_services/quick_look.rs",
        "terminal/workspace_terminal_session_factory.rs",
        "ui/workspace_manager.rs",
        "ui/workspace_picker.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let source = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in [
            "same_file::",
            "fs::metadata(",
            "fs::read_dir(",
            ".canonicalize()",
            "LocalFilesystemAuthority::new(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} bypasses Local Filesystem Authority with {forbidden}"
            );
        }
    }
}

#[test]
fn shared_startup_and_ui_cannot_select_native_implementations() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        root.join("main.rs"),
        root.join("app.rs"),
        root.join("desktop_profile.rs"),
    ];
    files.extend(
        std::fs::read_dir(root.join("ui"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs")),
    );
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let source = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in [
            "platform::macos_",
            "Macos",
            "use cocoa::",
            "use objc::",
            "target_os",
            "extern \"C\"",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn portable_ssh_runtime_cannot_encode_host_mechanics_or_host_selected_facts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = std::fs::read_dir(root.join("ssh"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect::<Vec<_>>();
    files.extend([
        root.join("platform/app_paths.rs"),
        root.join("platform/askpass.rs"),
        root.join("platform/ssh_askpass.rs"),
        root.join("ui/native_remote_workspace_flow_backend.rs"),
        root.join("ui/remote_workspace_flow.rs"),
        root.join("ui/ssh_askpass_dialog.rs"),
    ]);
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let source = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in [
            "std::os::unix",
            "std::process::Child",
            "std::process::Command",
            "std::process::{Child",
            "std::process::{Command",
            "std::fs::",
            "NativeHostConfigFilesystem",
            "libc::",
            "CommandExt",
            "AsRawFd",
            "FromRawFd",
            "MetadataExt",
            "PermissionsExt",
            "getpeereid",
            "/usr/bin/ssh",
            "macOS",
            "Macos",
            "MACOS",
            "Apple OpenSSH",
            "macos_temporary",
            "target_os",
            "extern \"C\"",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains {forbidden}",
                path.display()
            );
        }
    }
}
