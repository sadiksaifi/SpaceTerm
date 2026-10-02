//! Linux shortcut labels in the desktop's text convention, such as `Ctrl+Shift+T`.
use gpui::{Modifiers, SharedString};

use super::keyboard_layout::KeyboardLayout;
use crate::desktop_profile::ShortcutFormatter;

/// Presents chords as the person presses them. A shifted symbol that GPUI dispatches without
/// Shift, such as `ctrl-!` on US English, reads as its key with Shift: `Ctrl+Shift+1`.
pub(super) struct LinuxShortcutFormatter;

impl ShortcutFormatter for LinuxShortcutFormatter {
    fn format_chord(
        &self,
        modifiers: Modifiers,
        key: &str,
        layout: &KeyboardLayout,
    ) -> SharedString {
        let (shift, key) = match layout.unshifted(false, key) {
            Some(base) if !modifiers.shift && layout.is_shifted_symbol(false, key) => (true, base),
            _ => (modifiers.shift, key),
        };
        let modifiers = Modifiers { shift, ..modifiers };
        let prefix = self.format_modifiers(modifiers);
        let name = key_name(key);
        if prefix.is_empty() {
            name.into()
        } else {
            format!("{prefix}+{name}").into()
        }
    }

    fn format_modifiers(&self, modifiers: Modifiers) -> SharedString {
        [
            (modifiers.control, "Ctrl"),
            (modifiers.shift, "Shift"),
            (modifiers.alt, "Alt"),
            (modifiers.platform, "Super"),
        ]
        .into_iter()
        .filter_map(|(enabled, name)| enabled.then_some(name))
        .collect::<Vec<_>>()
        .join("+")
        .into()
    }
}

fn key_name(key: &str) -> String {
    match key {
        "enter" => "Enter".into(),
        "tab" => "Tab".into(),
        "escape" => "Esc".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        "insert" => "Insert".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "Page Up".into(),
        "pagedown" => "Page Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "space" => "Space".into(),
        // Letters and function keys read in uppercase; symbols are unaffected.
        key => key.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::Shortcut;

    fn format(source: &str) -> String {
        let layout = KeyboardLayout::us_english();
        let shortcut = Shortcut::parse(source).unwrap().resolve(&layout);
        LinuxShortcutFormatter
            .format_chord(shortcut.modifiers(), shortcut.key(), &layout)
            .to_string()
    }

    #[test]
    fn linux_shortcut_text_names_modifiers_and_keys_in_desktop_order() {
        for (source, label) in [
            ("ctrl-shift-t", "Ctrl+Shift+T"),
            ("ctrl-shift-alt-n", "Ctrl+Shift+Alt+N"),
            ("ctrl-shift-1", "Ctrl+Shift+1"),
            ("ctrl-shift-,", "Ctrl+Shift+,"),
            ("ctrl-shift-[", "Ctrl+Shift+["),
            ("ctrl-shift-=", "Ctrl+Shift+="),
            ("ctrl-shift-enter", "Ctrl+Shift+Enter"),
            ("ctrl-shift-left", "Ctrl+Shift+Left"),
            ("ctrl-shift-pageup", "Ctrl+Shift+Page Up"),
            ("shift-insert", "Shift+Insert"),
            ("escape", "Esc"),
            ("f11", "F11"),
            ("cmd-space", "Super+Space"),
        ] {
            assert_eq!(format(source), label, "{source}");
        }
        let formatter = LinuxShortcutFormatter;
        assert_eq!(
            formatter
                .format_modifiers(Modifiers::control_shift())
                .as_ref(),
            "Ctrl+Shift"
        );
    }
}
