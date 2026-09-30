//! Original native corpus oracles, separate from the portable release gate.
mod selected_file;
mod shortcut_glyphs;

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
fn keyboard_enrichment_oracle() {
    check_macos_keyboard_bridge().unwrap();
}
