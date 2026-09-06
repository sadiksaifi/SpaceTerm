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

/// Scan complete portable surfaces, including their test bodies and test-only helpers.
#[test]
fn portable_verification_cannot_select_native_adapters_or_host_mechanics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        root.join("terminal/conformance.rs"),
        root.join("terminal/testing.rs"),
        root.join("terminal/key_input.rs"),
        root.join("terminal/emulator.rs"),
        root.join("terminal/metadata.rs"),
        root.join("terminal/native_services/testing.rs"),
        root.join("terminal/native_services/hyperlink.rs"),
        root.join("terminal/workspace_terminal_session_factory.rs"),
        root.join("terminal/session.rs"),
        root.join("platform/native_pty.rs"),
        root.join("platform/testing.rs"),
        root.join("platform/local_filesystem.rs"),
        root.join("platform/shell_integration.rs"),
        root.join("platform/shell_launch.rs"),
        root.join("platform/app_paths.rs"),
        root.join("platform/askpass.rs"),
        root.join("platform/ssh_askpass.rs"),
        root.join("ui/native_remote_workspace_flow_backend.rs"),
        root.join("ui/remote_workspace_flow.rs"),
    ];
    for directory in ["ssh", "platform/local_filesystem"] {
        collect_rust_sources(&root.join(directory), &mut files);
    }
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let source = portable_verification_source(&source);
        if let Some(forbidden) = native_verification_dependency(&source) {
            panic!("{} contains {forbidden}", path.display());
        }
        // Shared test bodies must supply environment and resource facts explicitly.
        let test_body = source.split("#[cfg(test)]\nmod tests").nth(1);
        if let Some(test_body) = test_body {
            for forbidden in [
                "std::env::var",
                "std::env::current_exe",
                "ShellEnvironment::capture(",
                "configured_mode(",
                "CARGO_MANIFEST_DIR",
                "user_shell(",
                "local_hostname(",
            ] {
                assert!(
                    !test_body.contains(forbidden),
                    "{} tests contain {forbidden}",
                    path.display()
                );
            }
        }
    }
    let session = std::fs::read_to_string(root.join("terminal/session.rs")).unwrap();
    for (start, end) in [
        ("fn test_launch_planner(", "impl TerminalSession"),
        ("fn start_deferred_with(", "fn start_deferred_with_context("),
        ("fn start_with(", "fn write_input("),
    ] {
        let helper = session
            .split(start)
            .nth(1)
            .unwrap()
            .split(end)
            .next()
            .unwrap();
        assert!(
            !helper.contains("local_hostname()"),
            "{start} discovers the host name"
        );
        assert!(
            !helper.contains("launch_host"),
            "{start} selects host launch facts"
        );
    }
}

fn collect_rust_sources(directory: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

pub(crate) fn portable_verification_source(source: &str) -> String {
    source
        .lines()
        .filter(|line| {
            let line = line.trim();
            // Only exact declarations mount isolated suites. Other code on the line is still scanned.
            !native_suite_declaration(line, "#[path = \"", "\"]")
                && line != "mod macos_adapter_tests;"
                && line != "mod macos_adapter_tests {"
                && !native_suite_declaration(line, "include!(\"", "\");")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn native_suite_declaration(line: &str, prefix: &str, suffix: &str) -> bool {
    let Some(path) = line
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
    else {
        return false;
    };
    let Some((directory, file)) = path.split_once("macos_adapter_tests/") else {
        return false;
    };
    matches!(directory, "" | "../" | "../platform/" | "../../platform/")
        && file.ends_with(".rs")
        && file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
}

fn native_verification_dependency(source: &str) -> Option<&'static str> {
    let compact: String = source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    [
        "macos_",
        "Macos",
        "MacOs",
        "MacOS",
        "std::os::unix",
        "std::os::windows",
        "libc::",
        "launch_host::",
        "current_exe()",
        "user_shell()",
        "UnixListener",
        "UnixStream",
        "PermissionsExt",
        "MetadataExt",
        "raw_os_error",
        "CommandExt",
        "AsRawFd",
        "FromRawFd",
        "std::process::Command",
        "std::process::{Command",
        "std::process::{Child",
        "std::process::Child",
        "target_os",
        "cocoa::",
        "objc::",
        "extern\"C\"",
        "/usr/bin/ssh",
        "/opt/homebrew/",
    ]
    .into_iter()
    .find(|forbidden| compact.contains(forbidden))
}

#[test]
fn portable_verification_guard_rejects_native_dependencies_and_allows_suite_wiring() {
    for source in [
        "use crate::platform::macos_pty::MacosNativePtyAdapterFactory;",
        "use std :: os :: unix :: fs::PermissionsExt;",
        "unsafe { libc :: kill(1, 0) };",
        "let shell = user_shell();",
        "let resources = crate::platform::launch_host::resource_root();",
        "std::process::Command::new(\"/bin/zsh\");",
        "std::env::current_exe();",
        "include!(\"../platform/macos_adapter_tests/session.rs\"); use libc::kill;",
    ] {
        assert!(
            native_verification_dependency(&portable_verification_source(source)).is_some(),
            "accepted {source}"
        );
    }
    let wiring = "#[cfg(test)]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;";
    assert!(native_verification_dependency(&portable_verification_source(wiring)).is_none());
}
