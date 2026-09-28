use gpui::{Modifiers, SharedString};

use crate::desktop_profile::ShortcutFormatter;
use crate::keybindings::Shortcut;

pub(crate) struct MacosShortcutFormatter;

impl ShortcutFormatter for MacosShortcutFormatter {
    fn format(&self, shortcut: &Shortcut) -> SharedString {
        let key = match shortcut.key() {
            "enter" => "↩".into(),
            "tab" => "⇥".into(),
            "escape" => "⎋".into(),
            "backspace" => "⌫".into(),
            "delete" => "⌦".into(),
            "left" => "←".into(),
            "right" => "→".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "home" => "↖".into(),
            "end" => "↘".into(),
            "pageup" => "⇞".into(),
            "pagedown" => "⇟".into(),
            "space" => "Space".into(),
            "insert" => "Insert".into(),
            key => key.to_uppercase(),
        };
        format!("{}{key}", self.format_modifiers(shortcut.modifiers())).into()
    }

    fn format_modifiers(&self, modifiers: Modifiers) -> SharedString {
        [
            (modifiers.control, "⌃"),
            (modifiers.alt, "⌥"),
            (modifiers.shift, "⇧"),
            (modifiers.platform, "⌘"),
        ]
        .into_iter()
        .filter_map(|(enabled, glyph)| enabled.then_some(glyph))
        .collect::<String>()
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_glyphs_use_native_modifier_order_and_key_spelling() {
        for (key, glyph) in [
            ("enter", "↩"),
            ("tab", "⇥"),
            ("escape", "⎋"),
            ("backspace", "⌫"),
            ("delete", "⌦"),
            ("left", "←"),
            ("right", "→"),
            ("up", "↑"),
            ("down", "↓"),
            ("home", "↖"),
            ("end", "↘"),
            ("pageup", "⇞"),
            ("pagedown", "⇟"),
            ("space", "Space"),
            ("insert", "Insert"),
            ("f5", "F5"),
            ("f20", "F20"),
            ("k", "K"),
            ("é", "É"),
            ("+", "+"),
            ("-", "-"),
        ] {
            let shortcut = Shortcut::parse(&format!("cmd-shift-alt-ctrl-{key}")).unwrap();
            assert_eq!(
                MacosShortcutFormatter.format(&shortcut).as_ref(),
                format!("⌃⌥⇧⌘{glyph}")
            );
        }
        assert_eq!(
            MacosShortcutFormatter
                .format_modifiers(Modifiers::default())
                .as_ref(),
            ""
        );
        for (source, display) in [
            ("cmd-k", "⌘K"),
            ("ctrl-1", "⌃1"),
            ("shift-cmd-enter", "⇧⌘↩"),
        ] {
            assert_eq!(
                MacosShortcutFormatter
                    .format(&Shortcut::parse(source).unwrap())
                    .as_ref(),
                display
            );
        }
    }
}
