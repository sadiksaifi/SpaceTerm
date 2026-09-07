//! Original native corpus oracles, separate from the portable release gate.
use crate::platform::macos_keyboard::{MacosKeyboardBridge, NativeKeyEvent, NativeModifiers};
use crate::terminal::KeyTranslation;
use crate::terminal::{KeyAction, OptionAsAltPolicy, PhysicalKey};
fn require(
    condition: bool,
    field: &'static str,
    detail: impl std::fmt::Display,
) -> Result<(), String> {
    condition
        .then_some(())
        .ok_or_else(|| format!("step 1 `{field}` mismatch: {detail}"))
}
fn require_eq<T>(field: &'static str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    require(
        actual == expected,
        field,
        format!("expected {expected:?}, observed {actual:?}"),
    )
}

fn check_pty_initialization() -> Result<(), String> {
    let observation = crate::platform::macos_pty::conformance_initialization_observation();
    for expected in [
        "argv=[\"/bin/zsh\", \"-l\"]",
        "cwd=/tmp",
        "term=xterm-256color",
        "colorterm=truecolor",
        "program=ghostty",
        "spaceterm=1",
        "controlling-tty=true",
    ] {
        require(
            observation.contains(expected),
            "pty-initialization",
            format!("expected `{expected}` in `{observation}`"),
        )?;
    }
    Ok(())
}
fn check_pty_shutdown() -> Result<(), String> {
    require_eq(
        "pty-shutdown",
        crate::platform::macos_pty::conformance_shutdown_observation(),
        "first=true duplicate=true signals=1 disposition=Graceful revoked=true".to_owned(),
    )
}
fn check_macos_keyboard_bridge() -> Result<(), String> {
    let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::Both);
    let translation = bridge.translate(NativeKeyEvent {
        action: KeyAction::Press,
        native_key_code: 0,
        characters: Some("å".to_owned()),
        characters_ignoring_modifiers: Some("a".to_owned()),
        unmodified_characters: Some("a".to_owned()),
        characters_without_option: Some("a".to_owned()),
        modifiers: NativeModifiers {
            alt: true,
            alt_left: true,
            ..NativeModifiers::default()
        },
    });
    let KeyTranslation::Encoded(input) = translation else {
        return Err(format!(
            "expected encoded macOS key, observed {translation:?}"
        ));
    };
    require_eq("physical-key", input.physical_key, PhysicalKey::A)?;
    require_eq("option-as-alt-text", input.text.as_deref(), Some("a"))?;
    require(
        input.modifiers.alt && !input.consumed_modifiers.alt,
        "modifier-routing",
        format!("observed {input:?}"),
    )
}
#[test]
fn pty_initialization_oracle() {
    check_pty_initialization().unwrap();
}
#[test]
fn pty_shutdown_oracle() {
    check_pty_shutdown().unwrap();
}
#[test]
fn keyboard_enrichment_oracle() {
    check_macos_keyboard_bridge().unwrap();
}

pub(crate) fn local_filesystem() -> crate::platform::local_filesystem::LocalFilesystemAuthority {
    crate::platform::local_filesystem::LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        std::sync::Arc::new(super::macos_local_identity::MacosLocalIdentity),
    )
}
