//! Structural regression gates for the shared application and UI boundary.

const TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";

#[test]
fn application_directory_discovery_stays_in_its_platform_module() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let owner = root.join("platform/app_directories.rs");
    let mut files = Vec::new();
    collect_rust_sources(&root, &mut files);
    for path in files {
        if path == owner || path == root.join("architecture_tests.rs") || is_test_source(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let production = source.split(TEST_MODULE).next().unwrap();
        for forbidden in [
            "XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE",
            "XDG_DATA_HOME_ENVIRONMENT_VARIABLE",
            "XDG_STATE_HOME_ENVIRONMENT_VARIABLE",
            "XDG_CACHE_HOME_ENVIRONMENT_VARIABLE",
            "XDG_RUNTIME_DIR_ENVIRONMENT_VARIABLE",
            "Library/Application Support/spaceterm",
            "Library/Caches/spaceterm",
            "Library/Logs/spaceterm",
            "std::env::var_os(\"XDG_CONFIG_HOME\")",
            "std::env::var_os(\"XDG_DATA_HOME\")",
            "std::env::var_os(\"XDG_STATE_HOME\")",
            "std::env::var_os(\"XDG_CACHE_HOME\")",
            "std::env::var_os(\"XDG_RUNTIME_DIR\")",
            "spaceterm_directories::",
        ] {
            assert!(
                !production.contains(forbidden),
                "{} bypasses AppDirectories with {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn appearance_policy_and_terminal_consumers_keep_their_injected_boundaries() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let groups: &[(&str, &[&str])] = &[
        (
            "src/appearance",
            &[
                "use gpui",
                "gpui::",
                "cocoa::",
                "objc::",
                "gpui_linux",
                "zbus::",
                "std::env::",
                "crate::ui::",
            ],
        ),
        (
            "src/settings",
            &[
                "gpui::",
                "cocoa::",
                "objc::",
                "gpui_linux",
                "zbus::",
                "std::env::",
                "crate::ui::",
            ],
        ),
        (
            "src/terminal",
            &[
                "crate::settings",
                "appearance_runtime",
                "InstalledAppearance",
                "InstalledChrome",
            ],
        ),
        (
            "crates/spaceterm-ui/src",
            &[
                "crate::appearance::Theme",
                "crate::settings",
                "spaceterm::",
                "ACTIVE_THEME",
            ],
        ),
    ];
    for (directory, forbidden) in groups {
        let mut files = Vec::new();
        collect_rust_sources(&root.join(directory), &mut files);
        for path in files {
            if is_test_source(&path) {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for token in *forbidden {
                assert!(
                    !source.contains(token),
                    "{} crosses the appearance boundary with {token}",
                    path.display()
                );
            }
        }
    }
    let renderer = include_str!("ui/terminal_element.rs");
    for token in [
        "appearance_runtime",
        "InstalledAppearance",
        "InstalledChrome",
        "crate::settings",
    ] {
        assert!(
            !renderer.contains(token),
            "terminal rendering must receive resolved appearance, not discover {token}"
        );
    }
    for source in [include_str!("appearance.rs"), include_str!("settings.rs")] {
        for token in ["gpui::", "cocoa::", "objc::", "std::env::", "crate::ui::"] {
            assert!(
                !source.contains(token),
                "portable appearance policy imports {token}"
            );
        }
    }
}

#[test]
fn local_filesystem_policy_cannot_reintroduce_native_identity_or_host_selection() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in [
        "domain/workspace_collection.rs",
        "platform/local_filesystem.rs",
        "terminal/workspace_terminal_session_factory.rs",
        "terminal/native_services/hyperlink.rs",
        "terminal/native_services/file_preview.rs",
        "terminal/native_services.rs",
        "terminal/emulator.rs",
        "ui/workspace_manager.rs",
        "ui/tab_view.rs",
        "ui/tab_manager.rs",
    ] {
        let source =
            portable_verification_source(&std::fs::read_to_string(root.join(name)).unwrap());
        let source = source.split(TEST_MODULE).next().unwrap();
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
            "macos_local_identity",
            "linux_local_identity",
            "unix_local_identity",
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
        "terminal/native_services/file_preview.rs",
        "terminal/workspace_terminal_session_factory.rs",
        "ui/directory_picker/local_source.rs",
        "ui/workspace_manager.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let source = source.split(TEST_MODULE).next().unwrap();
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

/// Scan complete portable surfaces, including their test bodies and test-only helpers.
#[test]
fn portable_verification_cannot_select_native_adapters_or_host_mechanics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![
        root.join("main.rs"),
        root.join("application_modules.rs"),
        root.join("app.rs"),
        root.join("desktop_profile.rs"),
        root.join("keybindings.rs"),
        root.join("desktop_profile/default_keymap.rs"),
        root.join("ui/mod.rs"),
        root.join("ui/workspace_manager.rs"),
        root.join("ui/terminal_pane.rs"),
        root.join("terminal/conformance.rs"),
        root.join("terminal/testing.rs"),
        root.join("terminal/key_input.rs"),
        root.join("terminal/emulator.rs"),
        root.join("terminal/metadata.rs"),
        root.join("terminal/native_services/testing.rs"),
        root.join("terminal/native_services/hyperlink.rs"),
        root.join("terminal/native_services/file_preview.rs"),
        root.join("terminal/workspace_terminal_session_factory.rs"),
        root.join("terminal/session.rs"),
        root.join("platform/native_pty.rs"),
        root.join("platform/testing.rs"),
        root.join("platform/local_filesystem.rs"),
        root.join("platform/shell_integration.rs"),
        root.join("platform/shell_launch.rs"),
        root.join("platform/app_paths.rs"),
        root.join("ui/native_remote_workspace_flow_backend.rs"),
        root.join("ui/remote_workspace_flow.rs"),
    ];
    for directory in [
        "ui",
        "desktop_profile",
        "keybindings",
        "ssh",
        "platform/local_filesystem",
        "terminal/session",
        "terminal/native_services",
    ] {
        collect_rust_sources(&root.join(directory), &mut files);
    }
    for name in [
        "terminal/attention_runtime.rs",
        "terminal/attention_notification.rs",
        "terminal/secure_input.rs",
        "terminal/wheel_phase.rs",
        "platform/window_visibility.rs",
    ] {
        files.push(root.join(name));
    }
    // Discover every shared owner of an isolated suite so new mounts cannot escape the gate.
    let mut sources = Vec::new();
    collect_rust_sources(&root, &mut sources);
    for path in sources {
        if path == root.join("architecture_tests.rs")
            || path == root.join("platform/mod.rs")
            || is_native_suite_source(&root, &path)
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        if NativeSuitePlatform::ALL
            .iter()
            .any(|platform| source.contains(&format!("{}/", platform.suite())))
        {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        if matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("conformance.rs" | "testing.rs")
        ) {
            assert!(
                !NativeSuitePlatform::ALL
                    .iter()
                    .any(|platform| source.contains(platform.suite())),
                "{} mounts native tests in shared facilities",
                path.display()
            );
        }
        let source = portable_verification_source(&source);
        if let Some(forbidden) = native_verification_dependency(&source) {
            panic!("{} contains {forbidden}", path.display());
        }
        let production = source.split(TEST_MODULE).next().unwrap();
        if path.starts_with(root.join("ssh")) {
            for forbidden in [
                "std::fs::",
                "NativeHostConfigFilesystem",
                "getpeereid",
                "Apple OpenSSH",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "{} contains {forbidden}",
                    path.display()
                );
            }
        }
        if path.starts_with(root.join("terminal/native_services")) {
            let policy = source.replace("crate::platform::local_filesystem::", "");
            assert!(
                !policy.contains("crate::platform::"),
                "{} selects a platform capability",
                path.display()
            );
        }
        // Shared test bodies must supply environment and resource facts explicitly.
        let test_body = if is_test_source(&path) {
            Some(source.as_str())
        } else {
            source.split(TEST_MODULE).nth(1)
        };
        if let Some(test_body) = test_body {
            for forbidden in [
                "std::env::var",
                "std::env::current_exe",
                "ShellEnvironment::capture(",
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

fn is_test_source(path: &std::path::Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == "tests" || stem.ends_with("_tests"))
        || path.components().any(|part| part.as_os_str() == "tests")
}

/// The Operating-System family whose isolated Adapter suite a shared owner mounts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeSuitePlatform {
    Macos,
    Linux,
    Unix,
}

impl NativeSuitePlatform {
    const ALL: [Self; 3] = [Self::Macos, Self::Linux, Self::Unix];

    const fn suite(self) -> &'static str {
        match self {
            Self::Macos => "macos_adapter_tests",
            Self::Linux => "linux_adapter_tests",
            Self::Unix => "unix_adapter_tests",
        }
    }
}

fn is_native_suite_source(root: &std::path::Path, path: &std::path::Path) -> bool {
    NativeSuitePlatform::ALL
        .iter()
        .any(|platform| path.starts_with(root.join("platform").join(platform.suite())))
}

pub(crate) fn portable_verification_source(source: &str) -> String {
    let source = without_host_lint_allowances(source);
    let lines: Vec<_> = source.lines().collect();
    let mut portable = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let mounted = source_attribute(&lines, index).and_then(|(attribute, length)| {
            let platform = native_suite_gate(&attribute)?;
            let suite = platform.suite();
            let end = index + length;
            let path_mount = lines.get(end).is_some_and(|next| {
                native_suite_declaration(next.trim(), "#[path = \"", "\"]", platform)
            }) && lines.get(end + 1).is_some_and(|next| {
                let next = next.trim();
                next == format!("mod {suite};") || next == format!("pub(crate) mod {suite};")
            });
            let include_mount = lines
                .get(end)
                .is_some_and(|next| next.trim() == format!("mod {suite} {{"))
                && lines.get(end + 1).is_some_and(|next| {
                    native_suite_declaration(next.trim(), "include!(\"", "\");", platform)
                })
                && lines.get(end + 2).is_some_and(|next| next.trim() == "}");
            if path_mount {
                Some(length + 2)
            } else if include_mount {
                Some(length + 3)
            } else {
                None
            }
        });
        if let Some(length) = mounted {
            index += length;
        } else {
            portable.push(lines[index]);
            index += 1;
        }
    }
    portable.join("\n")
}

/// Removes only the exact lint attribute that lets a shared facility stay unused on desktops
/// whose Adapters do not consume it. The attribute selects no code, so it is not host selection.
fn without_host_lint_allowances(source: &str) -> String {
    let lines: Vec<_> = source.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let attribute = source_attribute(&lines, index)
            .and_then(|(compact, length)| is_host_lint_allowance(&compact).then_some(length));
        if let Some(length) = attribute {
            index += length;
        } else {
            kept.push(lines[index]);
            index += 1;
        }
    }
    kept.join("\n")
}

/// Read a complete source attribute without depending on rustfmt's line wrapping. Consumers
/// still accept only exact approved attributes; unrecognized attributes leave the source intact.
fn source_attribute(lines: &[&str], index: usize) -> Option<(String, usize)> {
    let first = lines.get(index)?.trim();
    if !first.starts_with("#[") && !first.starts_with("#![") {
        return None;
    }
    let length = lines[index..]
        .iter()
        .position(|line| line.trim_end().ends_with(']'))?
        + 1;
    let compact = lines[index..index + length]
        .concat()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    Some((compact, length))
}

fn is_host_lint_allowance(compact: &str) -> bool {
    let compact = compact.replace(",)", ")");
    let Some(rest) = compact
        .strip_prefix("#[")
        .or_else(|| compact.strip_prefix("#!["))
    else {
        return false;
    };
    rest.strip_prefix("cfg_attr(not(target_os=\"macos\"),allow(dead_code,reason=\"")
        .and_then(|reason| reason.strip_suffix("\"))]"))
        .is_some_and(|reason| !reason.is_empty() && !reason.contains(['"', '\\']))
}

fn native_suite_gate(line: &str) -> Option<NativeSuitePlatform> {
    let compact: String = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    match compact.replace(",)", ")").as_str() {
        "#[cfg(all(test,target_os=\"macos\",feature=\"native-tests\"))]" => {
            Some(NativeSuitePlatform::Macos)
        }
        "#[cfg(all(test,target_os=\"linux\",feature=\"native-tests\"))]" => {
            Some(NativeSuitePlatform::Linux)
        }
        "#[cfg(all(test,any(target_os=\"macos\",target_os=\"linux\"),feature=\"native-tests\"))]" => {
            Some(NativeSuitePlatform::Unix)
        }
        _ => None,
    }
}

fn native_suite_declaration(
    line: &str,
    prefix: &str,
    suffix: &str,
    platform: NativeSuitePlatform,
) -> bool {
    let Some(path) = line
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
    else {
        return false;
    };
    let Some((directory, file)) = path.split_once(&format!("{}/", platform.suite())) else {
        return false;
    };
    matches!(
        directory,
        "" | "../" | "platform/" | "../platform/" | "../../platform/"
    ) && file.ends_with(".rs")
        && file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
}

fn native_verification_dependency(source: &str) -> Option<&'static str> {
    // These enum values are supplied desktop policy facts, not Adapter selection.
    let source = source
        .replace("ModalKeybindingProfile::MacOs", "ExplicitModalProfile")
        .replace("MenuKeybindingProfile::MacOs", "ExplicitMenuProfile")
        .replace(
            "CommandPaletteKeybindingProfile::MacOs",
            "ExplicitCommandPaletteProfile",
        )
        .replace(
            "ComboBoxKeybindingProfile::MacOs",
            "ExplicitComboBoxProfile",
        )
        .replace("TextInputKeybindingProfile::MacOs", "ExplicitTextProfile")
        .replace("ModalKeybindingProfile::Linux", "ExplicitModalProfile")
        .replace("MenuKeybindingProfile::Linux", "ExplicitMenuProfile")
        .replace(
            "CommandPaletteKeybindingProfile::Linux",
            "ExplicitCommandPaletteProfile",
        )
        .replace(
            "ComboBoxKeybindingProfile::Linux",
            "ExplicitComboBoxProfile",
        )
        .replace("TextInputKeybindingProfile::Linux", "ExplicitTextProfile");
    let compact: String = source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    [
        "testing_desktop_profile",
        "macos_",
        "Macos",
        "MacOs",
        "MacOS",
        "linux_",
        "Linux",
        "platform::unix_",
        "std::os::unix",
        "std::os::linux",
        "gpui_linux",
        "gpui_wgpu",
        "zbus::",
        "accesskit_unix",
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
        "#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod native_evidence;",
        "mod native_evidence {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "include!(\"../platform/macos_adapter_tests/session.rs\"); use libc::kill;",
        "use crate::platform::linux_pty::LinuxPtyHost;",
        "use crate::platform::unix_pty::UnixNativePtyAdapterFactory;",
        "let profile = crate::keybindings::TerminalConventions::Linux;",
        "use std::os::linux::fs::MetadataExt;",
        "let connection = zbus::Connection::session();",
        "#[cfg(all(test, target_os = \"linux\", feature = \"native-tests\"))]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;",
        "#[cfg(all(test, target_os = \"macos\", feature = \"native-tests\"))]\nmod linux_adapter_tests {\ninclude!(\"../platform/linux_adapter_tests/session.rs\");\n}",
        "#[cfg(all(test, target_os = \"linux\", feature = \"native-tests\"))]\n#[path = \"../platform/unix_adapter_tests/session.rs\"]\nmod unix_adapter_tests;",
        "#[cfg_attr(not(target_os = \"macos\"), path = \"linux.rs\")]\nmod host;",
        "#[cfg_attr(not(target_os = \"macos\"), allow(dead_code, reason = \"x\"))] use libc::kill;",
        "#[cfg_attr(not(target_os = \"linux\"), allow(dead_code, reason = \"host\"))]\nstruct Host;",
        "#[cfg_attr(not(target_os = \"macos\"), allow(dead_code, reason = \"a\\\"))] use libc::kill; //\"))]",
    ] {
        assert!(
            native_verification_dependency(&portable_verification_source(source)).is_some(),
            "accepted {source}"
        );
    }
    for wiring in [
        "#[cfg(all(\n    test,\n    any(target_os = \"macos\", target_os = \"linux\"),\n    feature = \"native-tests\",\n))]\n#[path = \"../platform/unix_adapter_tests/session.rs\"]\nmod unix_adapter_tests;",
        "#[cfg_attr(\n    not(target_os = \"macos\"),\n    allow(\n        dead_code,\n        reason = \"only a native Adapter consumes it\",\n    )\n)]\nstruct Endpoint;",
        "#[cfg(all(test, target_os = \"macos\", feature = \"native-tests\"))]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;",
        "#[cfg(all(test, target_os = \"macos\", feature = \"native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "#[cfg(all(test, target_os = \"macos\", feature = \"native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"platform/macos_adapter_tests/traffic_lights.rs\");\n}",
        "#[cfg(all(test, target_os = \"linux\", feature = \"native-tests\"))]\n#[path = \"../platform/linux_adapter_tests/session.rs\"]\nmod linux_adapter_tests;",
        "#[cfg(all(test, any(target_os = \"macos\", target_os = \"linux\"), feature = \"native-tests\"))]\n#[path = \"../platform/unix_adapter_tests/session.rs\"]\nmod unix_adapter_tests;",
        "#[cfg(all(test, any(target_os = \"macos\", target_os = \"linux\"), feature = \"native-tests\"))]\nmod unix_adapter_tests {\ninclude!(\"../platform/unix_adapter_tests/session.rs\");\n}",
        "#[cfg_attr(not(target_os = \"macos\"), allow(dead_code, reason = \"only a native Adapter consumes it\"))]\nstruct Endpoint;",
        "#[cfg_attr(\n    not(target_os = \"macos\"),\n    allow(dead_code, reason = \"only a native Adapter consumes it\")\n)]\nstruct Endpoint;",
        "#![cfg_attr(not(target_os = \"macos\"), allow(dead_code, reason = \"only a native Adapter consumes it\"))]",
        "let profile = TextInputKeybindingProfile::Linux;",
    ] {
        assert!(
            native_verification_dependency(&portable_verification_source(wiring)).is_none(),
            "rejected {wiring}"
        );
    }
    for wiring in [
        "#[cfg(test)]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;",
        "#[cfg(target_os = \"macos\")]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "#[cfg(all(target_os = \"macos\", feature = \"native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "#[cfg(any(test, target_os = \"macos\", feature = \"native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
    ] {
        assert!(native_verification_dependency(&portable_verification_source(wiring)).is_some());
    }
}

#[test]
fn every_native_suite_mount_requires_test_target_and_feature_gates() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_sources(&root, &mut files);
    for path in files {
        if path == root.join("architecture_tests.rs") || is_native_suite_source(&root, &path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let portable = portable_verification_source(&source);
        for platform in NativeSuitePlatform::ALL {
            let mount = format!("{}/", platform.suite());
            assert!(
                !source.contains(&mount) || !portable.contains(&mount),
                "{} mounts native evidence without the complete gate",
                path.display()
            );
        }
    }
}

fn shared_presentation_violation(source: &str) -> Option<&'static str> {
    [
        "⌘",
        "⇧",
        "⌥",
        "⌃",
        "cmd-",
        "Command-Period",
        "Finder",
        "Quick Look",
        "this Mac",
        "macOS",
        "Ctrl+",
        "Super+",
        "GNOME",
        "Nautilus",
        "Sushi",
        "Linux",
        "Menlo",
        ".SystemUIFont",
    ]
    .into_iter()
    .find(|forbidden| source.contains(forbidden))
}

#[test]
fn shared_ui_and_failure_presentation_contain_no_host_shortcuts_or_wording() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_sources(&root.join("ui"), &mut files);
    files.push(root.join("terminal/failure.rs"));
    files.push(root.join("appearance/resolution.rs"));
    files.push(root.join("../crates/spaceterm-ui/src/appearance.rs"));
    for path in files {
        if is_test_source(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let production = source.split(TEST_MODULE).next().unwrap();
        if let Some(forbidden) = shared_presentation_violation(production) {
            panic!("{} contains host presentation {forbidden}", path.display());
        }
    }

    let reusable =
        std::fs::read_to_string(root.join("../crates/spaceterm-ui/src/command_palette.rs"))
            .unwrap();
    let production = reusable.split(TEST_MODULE).next().unwrap();
    let profile_start = production
        .find("/// A platform-selected complete Command Palette keybinding set")
        .unwrap();
    let portable_start = production.find("pub(crate) fn init").unwrap();
    let portable = format!(
        "{}{}",
        &production[..profile_start],
        &production[portable_start..]
    );
    if let Some(forbidden) = shared_presentation_violation(&portable) {
        panic!("reusable Command Palette contains host presentation {forbidden}");
    }
}

#[test]
fn local_interaction_policy_cannot_discover_the_host_or_embed_desktop_branding() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in [
        "local_path.rs",
        "directory_selection.rs",
        "platform/local_filesystem.rs",
        "platform/shell_integration.rs",
        "platform/shell_launch.rs",
        "ssh/startup_environment.rs",
        "terminal/metadata.rs",
        "terminal/emulator.rs",
        "terminal/native_services.rs",
        "terminal/native_services/file_insertion.rs",
        "terminal/native_services/hyperlink.rs",
        "terminal/native_services/file_preview.rs",
        "ui/terminal_context_menu.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let source = portable_verification_source(&source);
        for forbidden in [
            "std::env::var",
            "std::env::current_exe",
            "std::env::current_dir",
            "env::split_paths",
            "env::join_paths",
            "target_os",
            "cfg!(unix)",
            "Finder",
            "finder",
            "QuickLook",
            "Quick Look",
            "quick_look",
            "Nautilus",
            "nautilus",
            "Sushi",
            "sushi",
            "NautilusPreviewer",
            "xdg-open",
            "gio ",
        ] {
            assert!(!source.contains(forbidden), "{name} contains {forbidden}");
        }
    }
    for name in [
        "platform/shell_integration.rs",
        "platform/shell_launch.rs",
        "ssh/startup_environment.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let production = source.split(TEST_MODULE).next().unwrap();
        // Test-only launch construction precedes the test module and supplies a fixture path.
        for forbidden in [
            "/usr/bin",
            "/usr/local",
            "/usr/share",
            "/bin/zsh",
            "/bin/bash",
        ] {
            assert!(
                !production.contains(forbidden),
                "{name} contains fixed host path {forbidden}"
            );
        }
    }
    let ssh = std::fs::read_to_string(root.join("ssh/command.rs")).unwrap();
    assert!(!ssh.contains("/usr/bin/false"));
    for name in [
        "platform/macos_composition.rs",
        "platform/linux_composition.rs",
    ] {
        let composition = std::fs::read_to_string(root.join(name)).unwrap();
        assert!(!composition.contains("GpuiDirectorySelection"), "{name}");
        assert!(!composition.contains("SystemDirectorySelection"), "{name}");
    }
    let platform = std::fs::read_to_string(root.join("platform/mod.rs")).unwrap();
    assert!(!platform.contains("mod directory_selection"));
    for name in [
        "terminal/metadata.rs",
        "terminal/emulator.rs",
        "ui/terminal_pane.rs",
    ] {
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        assert!(
            !source.contains(".is_absolute()"),
            "{name} uses ambient path semantics"
        );
    }
    let pasteboard = std::fs::read_to_string(root.join("platform/macos_pasteboard.rs")).unwrap();
    let production = pasteboard
        .split("#[cfg(all(test, feature = \"native-tests\"))]")
        .next()
        .unwrap();
    assert!(!production.contains("LocalPathSemantics::Posix"));
}

#[test]
fn native_sources_do_not_fabricate_main_thread_authority() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for directory in ["src", "crates", "tests"] {
        collect_rust_sources(&root.join(directory), &mut files);
    }
    let forbidden = ["MainThreadMarker::new", "_unchecked"].concat();
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains(&forbidden),
            "{} fabricates main-thread authority",
            path.display()
        );
    }
}

#[test]
fn main_thread_runner_lists_every_native_platform_fixture() {
    // The runner has no test attribute discovery, so an unlisted fixture would never run.
    let platform = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform");
    let runner = std::fs::read_to_string(platform.join("native_main_thread_tests.rs")).unwrap();
    let runner: String = runner.split_whitespace().collect();
    let mut files = Vec::new();
    collect_rust_sources(&platform, &mut files);
    let mut fixture_count = 0;
    for path in files {
        let module = path.file_stem().unwrap().to_str().unwrap();
        // The runner owns the AppKit main thread; other desktops run fixtures under libtest.
        if !module.starts_with("macos_") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let Some((_, fixtures)) =
            source.split_once("#[cfg(all(test, feature = \"native-tests\"))]")
        else {
            continue;
        };
        for line in fixtures.lines() {
            let Some(name) = line
                .trim_start()
                .strip_prefix("pub(in crate::platform) fn ")
                .and_then(|rest| rest.split(['(', '<']).next())
            else {
                continue;
            };
            fixture_count += 1;
            assert!(
                runner.contains(&format!("{module}::tests::{name})")),
                "{module}::tests::{name} is not run by native_main_thread_tests.rs"
            );
        }
    }
    assert!(
        fixture_count > 0,
        "no native main-thread fixtures were found"
    );
}
