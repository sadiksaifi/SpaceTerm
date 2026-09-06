//! Structural regression gates for the shared application and UI boundary.
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
