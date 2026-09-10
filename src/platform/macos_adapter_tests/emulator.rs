use super::*;
#[test]
fn osc8_relative_file_targets_bind_to_the_directory_at_emission() {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-emulator-local-link-first-{}",
        std::process::id()
    ));
    let second_directory = std::env::temp_dir().join(format!(
        "spaceterm-emulator-local-link-second-{}",
        std::process::id()
    ));
    _ = fs::remove_dir_all(&directory);
    _ = fs::remove_dir_all(&second_directory);
    fs::create_dir_all(&directory).unwrap();
    fs::create_dir_all(&second_directory).unwrap();
    let file = directory.join("preview.txt");
    let second_file = second_directory.join("preview.txt");
    fs::write(&file, b"preview").unwrap();
    fs::write(&second_file, b"preview").unwrap();
    let mut emulator = TerminalEmulator::new_with_local_filesystem(
        geometry(16, 2, 10.0, 20.0),
        TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            directory.to_str().unwrap(),
            crate::terminal::metadata::LocalMachine::new(None, Some("mac.local"), None),
        ),
        "zsh",
        identity::TERM_FALLBACK,
        Instant::now(),
        crate::platform::macos_adapter_tests::local_filesystem(),
    )
    .unwrap();

    emulator.feed(b"\x1b]8;;file:prev");
    emulator.feed(b"iew.txt\x07first\x1b]8;;\x07 \x1b]7;FiLe://local");
    emulator.feed(
        format!(
            "host{}\x07\x1b]8;;file:preview.txt\x07second\x1b]8;;\x07",
            second_directory.to_str().unwrap()
        )
        .as_bytes(),
    );
    let replacement = directory.join("replacement.txt");
    fs::write(&replacement, b"replacement").unwrap();
    fs::rename(&replacement, &file).unwrap();
    let snapshot = emulator.snapshot().unwrap().unwrap();
    let values = snapshot.rows[0]
        .iter()
        .map(|cell| cell.hyperlink.as_ref().map(|link| link.value.as_str()))
        .collect::<Vec<_>>();

    assert!(
        values[..5]
            .iter()
            .all(|value| { *value == Some(file.canonicalize().unwrap().to_str().unwrap()) })
    );
    assert!(
        values[6..12]
            .iter()
            .all(|value| { *value == Some(second_file.canonicalize().unwrap().to_str().unwrap()) })
    );
    let first_link = snapshot.rows[0][0].hyperlink.as_ref().unwrap();
    assert_eq!(
        first_link.value,
        file.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        first_link
            .activation_url(crate::terminal::metadata::TerminalLocalFileCapabilities::Enabled),
        None
    );
    fs::remove_dir_all(directory).unwrap();
    fs::remove_dir_all(second_directory).unwrap();
}
