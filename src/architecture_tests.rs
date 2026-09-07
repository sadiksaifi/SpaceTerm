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
        "terminal/native_services/file_preview.rs",
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
        "terminal/native_services/file_preview.rs",
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
        root.join("desktop_profile.rs"),
        root.join("desktop_profile/keybindings.rs"),
        root.join("ui/mod.rs"),
        root.join("ui/workspace_manager.rs"),
        root.join("ui/workspace_picker.rs"),
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
        root.join("platform/askpass.rs"),
        root.join("platform/ssh_askpass.rs"),
        root.join("ui/native_remote_workspace_flow_backend.rs"),
        root.join("ui/remote_workspace_flow.rs"),
    ];
    for directory in [
        "ssh",
        "platform/local_filesystem",
        "terminal/session",
        "terminal/native_services",
    ] {
        collect_rust_sources(&root.join(directory), &mut files);
    }
    // Discover every shared owner of an isolated suite so new mounts cannot escape the gate.
    let mut sources = Vec::new();
    collect_rust_sources(&root, &mut sources);
    for path in sources {
        if path == root.join("architecture_tests.rs")
            || path == root.join("platform/mod.rs")
            || path.starts_with(root.join("platform/macos_adapter_tests"))
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        if source.contains("macos_adapter_tests/") {
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
                !source.contains("macos_adapter_tests"),
                "{} mounts native tests in shared facilities",
                path.display()
            );
        }
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
    let session = format!(
        "{}\n{}",
        std::fs::read_to_string(root.join("terminal/session.rs")).unwrap(),
        std::fs::read_to_string(root.join("terminal/session/launch.rs")).unwrap()
    );
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
    let lines: Vec<_> = source.lines().collect();
    let mut portable = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if native_suite_gate(line)
            && lines
                .get(index + 1)
                .is_some_and(|next| native_suite_declaration(next.trim(), "#[path = \"", "\"]"))
            && lines.get(index + 2).is_some_and(|next| {
                matches!(
                    next.trim(),
                    "mod macos_adapter_tests;" | "pub(crate) mod macos_adapter_tests;"
                )
            })
        {
            index += 3;
        } else if native_suite_gate(line)
            && lines
                .get(index + 1)
                .is_some_and(|next| next.trim() == "mod macos_adapter_tests {")
            && lines
                .get(index + 2)
                .is_some_and(|next| native_suite_declaration(next.trim(), "include!(\"", "\");"))
            && lines.get(index + 3).is_some_and(|next| next.trim() == "}")
        {
            index += 4;
        } else {
            portable.push(lines[index]);
            index += 1;
        }
    }
    portable.join("\n")
}

fn native_suite_gate(line: &str) -> bool {
    let compact: String = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    matches!(
        compact.as_str(),
        "#[cfg(all(test,target_os=\"macos\",feature=\"macos-native-tests\"))]"
    )
}

fn positive_macos_source_gate(line: &str) -> bool {
    let compact: String = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    matches!(
        compact.as_str(),
        "#[cfg(target_os=\"macos\")]"
            | "#[cfg(all(target_os=\"macos\",not(test)))]"
            | "#[cfg(all(target_os=\"macos\",test))]"
            | "#[cfg(all(test,target_os=\"macos\",feature=\"macos-native-tests\"))]"
    )
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
    // These enum values are supplied desktop policy facts, not Adapter selection.
    let source = source
        .replace("ModalKeybindingProfile::MacOs", "ExplicitModalProfile")
        .replace(
            "CommandPaletteKeybindingProfile::MacOs",
            "ExplicitCommandPaletteProfile",
        )
        .replace("TextInputKeybindingProfile::MacOs", "ExplicitTextProfile");
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
        "#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod native_evidence;",
        "mod native_evidence {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "include!(\"../platform/macos_adapter_tests/session.rs\"); use libc::kill;",
    ] {
        assert!(
            native_verification_dependency(&portable_verification_source(source)).is_some(),
            "accepted {source}"
        );
    }
    for wiring in [
        "#[cfg(all(test, target_os = \"macos\", feature = \"macos-native-tests\"))]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;",
        "#[cfg(all(test, target_os = \"macos\", feature = \"macos-native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
    ] {
        assert!(native_verification_dependency(&portable_verification_source(wiring)).is_none());
    }
    for wiring in [
        "#[cfg(test)]\n#[path = \"../platform/macos_adapter_tests/session.rs\"]\nmod macos_adapter_tests;",
        "#[cfg(target_os = \"macos\")]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "#[cfg(all(target_os = \"macos\", feature = \"macos-native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
        "#[cfg(any(test, target_os = \"macos\", feature = \"macos-native-tests\"))]\nmod macos_adapter_tests {\ninclude!(\"../platform/macos_adapter_tests/session.rs\");\n}",
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
        if path == root.join("architecture_tests.rs")
            || path.starts_with(root.join("platform/macos_adapter_tests"))
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        if !source.contains("macos_adapter_tests/") {
            continue;
        }
        assert!(
            !portable_verification_source(&source).contains("macos_adapter_tests/"),
            "{} mounts native evidence without the complete gate",
            path.display()
        );
    }
}

#[test]
fn macos_source_modules_are_target_gated_while_portable_policy_is_not() {
    let source = include_str!("platform/mod.rs");
    let lines = source.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let declaration = line.trim();
        if (declaration.starts_with("mod macos_")
            || declaration.starts_with("pub(crate) mod macos_")
            || declaration == "pub(crate) use macos_composition::main;")
            && !declaration.contains("macos_adapter_tests")
        {
            assert!(
                index > 0 && positive_macos_source_gate(lines[index - 1]),
                "{declaration} is not explicitly owned by the macOS source set"
            );
        }
    }
    let askpass = lines
        .iter()
        .position(|line| line.trim() == "pub(crate) mod ssh_askpass;")
        .unwrap();
    assert!(!lines[askpass - 1].contains("target_os"));
    assert!(source.contains("compile_error!(\"SpaceTerm currently supports macOS only\")"));

    for invalid in [
        "#[cfg(not(target_os = \"macos\"))]",
        "#[cfg(any(target_os = \"macos\", test))]",
        "// target_os = \"macos\"",
        "#[cfg(feature = \"macos-native-tests\")] // target_os = \"macos\"",
    ] {
        assert!(!positive_macos_source_gate(invalid), "accepted {invalid}");
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
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        if let Some(forbidden) = shared_presentation_violation(production) {
            panic!("{} contains host presentation {forbidden}", path.display());
        }
    }

    let reusable =
        std::fs::read_to_string(root.join("../crates/spaceterm-ui/src/command_palette.rs"))
            .unwrap();
    let production = reusable.split("#[cfg(test)]\nmod tests").next().unwrap();
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
fn shared_presentation_guard_rejects_adversarial_host_fixtures() {
    for source in [
        "button.child(\"⌘N\")",
        "tooltip.keyboard_equivalent(\"cmd-t\")",
        "let label = \"Choose with Finder\";",
        "let label = \"Quick Look\";",
        "let description = \"Pinned to a folder on this Mac\";",
        "let failure = \"macOS integration\";",
    ] {
        assert!(
            shared_presentation_violation(source).is_some(),
            "accepted {source}"
        );
    }
    assert!(shared_presentation_violation("profile.shortcut(&CreateTab)").is_none());
}

fn just_recipe<'a>(justfile: &'a str, name: &str) -> Option<(Vec<&'a str>, String)> {
    let prefix = format!("{name}:");
    let lines = justfile.lines().collect::<Vec<_>>();
    let (index, dependencies) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| line.strip_prefix(&prefix).map(|rest| (index, rest)))?;
    let body = lines[index + 1..]
        .iter()
        .take_while(|line| line.is_empty() || line.starts_with(char::is_whitespace))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    Some((dependencies.split_whitespace().collect(), body))
}

fn portable_validation_violation(justfile: &str) -> Option<String> {
    let expected = [
        "portable-fmt-check",
        "portable-check",
        "portable-test",
        "portable-clippy",
        "diff-check",
    ];
    let (direct, _) = just_recipe(justfile, "portable-validate")?;
    if direct != expected {
        return Some("portable-validate dependencies changed".to_owned());
    }

    let mut pending = vec!["portable-validate"];
    let mut visited = std::collections::BTreeSet::new();
    while let Some(recipe) = pending.pop() {
        if !visited.insert(recipe) {
            continue;
        }
        if recipe.starts_with("macos-")
            || matches!(
                recipe,
                "scripts-check" | "performance-tools-check" | "package" | "mounted-dmg"
            )
        {
            return Some(format!("portable lane reaches native recipe {recipe}"));
        }
        let Some((dependencies, body)) = just_recipe(justfile, recipe) else {
            return Some(format!("portable lane references missing recipe {recipe}"));
        };
        for forbidden in [
            "xcrun",
            "AppKit",
            "package-macos",
            "mounted-dmg",
            "performance",
        ] {
            if body.contains(forbidden) {
                return Some(format!(
                    "portable lane invokes {forbidden} through {recipe}"
                ));
            }
        }
        pending.extend(dependencies);
    }
    None
}

#[test]
fn validation_lanes_keep_portable_and_native_prerequisites_separate() {
    let justfile = include_str!("../Justfile");
    assert_eq!(portable_validation_violation(justfile), None);
    for required in [
        "macos-validate: macos-fmt-check macos-adapter-tests macos-clippy scripts-check performance-tools-check",
        "validate: portable-validate macos-validate",
        "cargo test --workspace --all-targets --no-default-features --locked",
        "cargo test --all-targets --features macos-native-tests --locked \"macos\"",
    ] {
        assert!(
            justfile.lines().any(|line| line.trim() == required),
            "missing validation contract: {required}"
        );
    }

    let injected = justfile.replacen(
        "portable-validate: portable-fmt-check portable-check portable-test portable-clippy diff-check",
        "portable-validate: portable-fmt-check portable-check portable-test portable-clippy diff-check scripts-check",
        1,
    );
    assert!(portable_validation_violation(&injected).is_some());

    let transitive = justfile.replacen(
        "portable-test:\n    cargo test",
        "portable-test: scripts-check\n    cargo test",
        1,
    );
    assert!(portable_validation_violation(&transitive).is_some());
}

#[test]
fn migrated_policy_and_callers_do_not_name_concrete_adapters() {
    let policy = [
        include_str!("terminal/native_services/local_authority.rs"),
        include_str!("terminal/native_services/clipboard.rs"),
        include_str!("terminal/native_services/file_insertion.rs"),
        include_str!("terminal/native_services/hyperlink.rs"),
        include_str!("terminal/native_services/osc52.rs"),
        include_str!("terminal/native_services/paste.rs"),
        include_str!("terminal/native_services/file_preview.rs"),
        include_str!("terminal/native_services/selection.rs"),
        include_str!("terminal/native_services/services.rs"),
    ];
    for source in policy {
        let source = portable_verification_source(source);
        // Local Filesystem Authority is portable policy, shared with Workspaces.
        let source = source.replace("crate::platform::local_filesystem::", "");
        assert!(!source.contains("crate::platform::"));
        assert!(!source.contains("target_os"));
        assert!(!source.contains("use cocoa::"));
        assert!(!source.contains("use objc::"));
    }
    for source in [
        include_str!("ui/terminal_pane.rs"),
        include_str!("terminal/session.rs"),
    ] {
        for concrete in [
            "macos_pasteboard",
            "macos_quick_look",
            "macos_services",
            "MacosOsc52Clipboard",
            "MacosQuickLook",
        ] {
            assert!(!source.contains(concrete));
        }
    }
}

#[test]
fn local_interaction_policy_cannot_discover_the_host_or_embed_desktop_branding() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in [
        "local_path.rs",
        "directory_selection.rs",
        "platform/local_filesystem.rs",
        "platform/local_filesystem/picker.rs",
        "platform/shell_integration.rs",
        "platform/shell_launch.rs",
        "ssh/startup_environment.rs",
        "terminal/metadata.rs",
        "terminal/emulator.rs",
        "terminal/native_services.rs",
        "terminal/native_services/file_insertion.rs",
        "terminal/native_services/hyperlink.rs",
        "terminal/native_services/file_preview.rs",
        "ui/workspace_picker.rs",
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
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
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
    let composition = std::fs::read_to_string(root.join("platform/macos_composition.rs")).unwrap();
    assert!(!composition.contains("GpuiDirectorySelection"));
    assert!(!composition.contains("SystemDirectorySelection"));
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
        .split("#[cfg(all(test, feature = \"macos-native-tests\"))]\nmod tests")
        .next()
        .unwrap();
    assert!(!production.contains("LocalPathSemantics::Posix"));
    let picker = std::fs::read_to_string(root.join("ui/workspace_picker.rs")).unwrap();
    let production = picker.split("#[cfg(test)]\nmod tests").next().unwrap();
    for forbidden in [
        "\"~/\"",
        "starts_with('/')",
        "ends_with('/')",
        "rfind('/')",
        "LocalPathSemantics::Posix",
    ] {
        assert!(
            !production.contains(forbidden),
            "picker embeds path dialect: {forbidden}"
        );
    }
}
