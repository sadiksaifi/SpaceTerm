use gpui::{Modifiers, SharedString};

use crate::desktop_profile::ShortcutFormatter;

pub(crate) struct MacosShortcutFormatter;

impl ShortcutFormatter for MacosShortcutFormatter {
    fn format_chord(
        &self,
        modifiers: Modifiers,
        key: &str,
        _: &super::keyboard_layout::KeyboardLayout,
    ) -> SharedString {
        let key = match key {
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
            key => crate::desktop_profile::uppercase_shortcut_key(key),
        };
        format!("{}{key}", self.format_modifiers(modifiers)).into()
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
    use crate::keybindings::Shortcut;

    fn format(shortcut: &Shortcut) -> SharedString {
        MacosShortcutFormatter.format_chord(
            shortcut.modifiers(),
            shortcut.key(),
            &super::super::keyboard_layout::KeyboardLayout::default(),
        )
    }

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
            ("ß", "ß"),
        ] {
            let shortcut = Shortcut::parse(&format!("cmd-shift-alt-ctrl-{key}")).unwrap();
            assert_eq!(format(&shortcut).as_ref(), format!("⌃⌥⇧⌘{glyph}"));
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
            ("cmd-shift-alt-ctrl-+", "⌃⌥⌘+"),
            ("cmd-shift-alt-ctrl--", "⌃⌥⌘_"),
            ("shift-cmd-`", "⌘~"),
        ] {
            let layout = crate::platform::keyboard_layout::testing::us()
                .snapshot(&crate::platform::keyboard_layout::testing::UnknownLayout)
                .unwrap();
            let shortcut = Shortcut::parse(source).unwrap().resolve(&layout);
            assert_eq!(format(&shortcut).as_ref(), display);
        }
    }
}
