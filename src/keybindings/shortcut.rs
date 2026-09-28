use std::fmt;

use gpui::{Keystroke, Modifiers};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::TerminalConvention;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Shortcut {
    modifiers: Modifiers,
    key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ShortcutRejection {
    #[error("malformed shortcut")]
    Malformed,
    #[error("multiple keystrokes are not supported")]
    Chord,
    #[error("a shortcut needs a key")]
    ModifierOnly,
    #[error("the function modifier is not supported")]
    FunctionModifier,
    #[error("unsupported shortcut key")]
    UnsupportedKey,
    #[error("shortcut is reserved for terminal input")]
    TerminalReserved(TerminalConvention),
}

impl Shortcut {
    pub fn parse(source: &str) -> Result<Self, ShortcutRejection> {
        if source.split_whitespace().count() > 1 {
            return Err(ShortcutRejection::Chord);
        }
        if source.is_empty() || source.chars().any(char::is_whitespace) {
            return Err(ShortcutRejection::Malformed);
        }
        let source = source.to_ascii_lowercase();
        let mut key = source.as_str();
        let mut modifiers = Modifiers::default();
        while let Some((modifier, rest)) = key.split_once('-') {
            match modifier {
                "ctrl" => modifiers.control = true,
                "alt" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "cmd" => modifiers.platform = true,
                "fn" => return Err(ShortcutRejection::FunctionModifier),
                _ if key == "-" => break,
                _ => return Err(ShortcutRejection::Malformed),
            }
            key = rest;
        }
        Self::from_parts(modifiers, key)
    }

    pub fn from_keystroke(keystroke: &Keystroke) -> Result<Self, ShortcutRejection> {
        Self::from_parts(keystroke.modifiers, &keystroke.key.to_ascii_lowercase())
    }

    fn from_parts(mut modifiers: Modifiers, key: &str) -> Result<Self, ShortcutRejection> {
        if modifiers.function || matches!(key, "fn" | "function") {
            return Err(ShortcutRejection::FunctionModifier);
        }
        if matches!(
            key,
            "ctrl" | "control" | "alt" | "shift" | "cmd" | "platform"
        ) {
            return Err(ShortcutRejection::ModifierOnly);
        }
        if key.is_empty() {
            return Err(ShortcutRejection::Malformed);
        }
        let printable =
            key.chars().count() == 1 && key.chars().all(|c| !c.is_control() && !c.is_whitespace());
        let named = matches!(
            key,
            "enter"
                | "space"
                | "tab"
                | "escape"
                | "backspace"
                | "delete"
                | "insert"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "left"
                | "right"
                | "up"
                | "down"
                | "f1"
                | "f2"
                | "f3"
                | "f4"
                | "f5"
                | "f6"
                | "f7"
                | "f8"
                | "f9"
                | "f10"
                | "f11"
                | "f12"
                | "f13"
                | "f14"
                | "f15"
                | "f16"
                | "f17"
                | "f18"
                | "f19"
                | "f20"
        );
        if !printable && !named {
            return Err(ShortcutRejection::UnsupportedKey);
        }
        // Match GPUI's native symbol form. Named keys and ASCII letters retain Shift.
        let key = if modifiers.shift {
            let shifted = match key {
                "`" | "~" => Some("~"),
                "1" | "!" => Some("!"),
                "2" | "@" => Some("@"),
                "3" | "#" => Some("#"),
                "4" | "$" => Some("$"),
                "5" | "%" => Some("%"),
                "6" | "^" => Some("^"),
                "7" | "&" => Some("&"),
                "8" | "*" => Some("*"),
                "9" | "(" => Some("("),
                "0" | ")" => Some(")"),
                "-" | "_" => Some("_"),
                "=" | "+" => Some("+"),
                "[" | "{" => Some("{"),
                "]" | "}" => Some("}"),
                "\\" | "|" => Some("|"),
                ";" | ":" => Some(":"),
                "'" | "\"" => Some("\""),
                "," | "<" => Some("<"),
                "." | ">" => Some(">"),
                "/" | "?" => Some("?"),
                _ => None,
            };
            if let Some(shifted) = shifted {
                modifiers.shift = false;
                shifted
            } else {
                key
            }
        } else {
            key
        };
        let shortcut = Self {
            modifiers,
            key: key.into(),
        };
        if let Some(convention) = super::terminal_conventions::reservation(&shortcut) {
            return Err(ShortcutRejection::TerminalReserved(convention));
        }
        Ok(shortcut)
    }

    #[cfg(test)]
    pub fn to_keystroke(&self) -> Keystroke {
        Keystroke {
            modifiers: self.modifiers,
            key: self.key.clone(),
            key_char: None,
        }
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (enabled, name) in [
            (self.modifiers.control, "ctrl"),
            (self.modifiers.alt, "alt"),
            (self.modifiers.shift, "shift"),
            (self.modifiers.platform, "cmd"),
        ] {
            if enabled {
                write!(f, "{name}-")?;
            }
        }
        f.write_str(&self.key)
    }
}

impl Serialize for Shortcut {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Shortcut {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_shortcuts_round_trip_without_character_metadata() {
        let shortcut = Shortcut::parse("CMD-Shift-ALT-CTRL-K").unwrap();
        assert_eq!(shortcut.to_string(), "ctrl-alt-shift-cmd-k");
        assert_eq!(
            Keystroke::parse(&shortcut.to_string()).unwrap(),
            shortcut.to_keystroke()
        );
        let mut keystroke = shortcut.to_keystroke();
        keystroke.key_char = Some("private input".into());
        assert_eq!(Shortcut::from_keystroke(&keystroke).unwrap(), shortcut);
        assert_eq!(
            serde_json::from_str::<Shortcut>(&serde_json::to_string(&shortcut).unwrap()).unwrap(),
            shortcut
        );
    }
    #[test]
    fn shifted_punctuation_has_one_identity_and_matches_native_dispatch() {
        for (base, symbol) in [
            ("`", "~"),
            ("1", "!"),
            ("2", "@"),
            ("3", "#"),
            ("4", "$"),
            ("5", "%"),
            ("6", "^"),
            ("7", "&"),
            ("8", "*"),
            ("9", "("),
            ("0", ")"),
            ("-", "_"),
            ("=", "+"),
            ("[", "{"),
            ("]", "}"),
            ("\\", "|"),
            (";", ":"),
            ("'", "\""),
            (",", "<"),
            (".", ">"),
            ("/", "?"),
        ] {
            let shortcut = Shortcut::parse(&format!("shift-cmd-{base}")).unwrap();
            let native = Keystroke::parse(&format!("cmd-{symbol}")).unwrap();
            assert_eq!(shortcut, Shortcut::from_keystroke(&native).unwrap());
            assert_eq!(shortcut.to_string(), format!("cmd-{symbol}"));
            let binding =
                gpui::KeyBinding::new(&shortcut.to_string(), crate::ui::NewWorkspace, None);
            assert!(native.should_match(&binding.keystrokes()[0]));
            assert_eq!(
                serde_json::from_str::<Shortcut>(
                    &serde_json::to_string(&format!("shift-cmd-{base}")).unwrap()
                )
                .unwrap(),
                shortcut
            );
        }
    }

    #[test]
    fn parser_rejects_each_invalid_input_class() {
        for (source, rejection) in [
            ("", ShortcutRejection::Malformed),
            ("cmd-", ShortcutRejection::Malformed),
            ("cmd-k-cmd", ShortcutRejection::Malformed),
            ("cmd-k->x", ShortcutRejection::Malformed),
            ("secondary-k", ShortcutRejection::Malformed),
            ("super-k", ShortcutRejection::Malformed),
            ("win-k", ShortcutRejection::Malformed),
            ("cmd-k cmd-c", ShortcutRejection::Chord),
            ("cmd", ShortcutRejection::ModifierOnly),
            ("ctrl-shift", ShortcutRejection::ModifierOnly),
            ("fn-cmd-k", ShortcutRejection::FunctionModifier),
            ("cmd-f21", ShortcutRejection::UnsupportedKey),
            ("cmd-unknown", ShortcutRejection::UnsupportedKey),
            ("cmd-\u{7}", ShortcutRejection::UnsupportedKey),
            (
                "k",
                ShortcutRejection::TerminalReserved(TerminalConvention::TextInput),
            ),
        ] {
            assert_eq!(Shortcut::parse(source), Err(rejection), "{source}");
            assert!(
                serde_json::from_str::<Shortcut>(&serde_json::to_string(source).unwrap()).is_err()
            );
        }
        for (key, function, rejection) in [
            ("shift", false, ShortcutRejection::ModifierOnly),
            ("", false, ShortcutRejection::Malformed),
            ("k", true, ShortcutRejection::FunctionModifier),
            ("f21", false, ShortcutRejection::UnsupportedKey),
        ] {
            assert_eq!(
                Shortcut::from_keystroke(&Keystroke {
                    key: key.into(),
                    modifiers: Modifiers {
                        function,
                        ..Modifiers::default()
                    },
                    key_char: None,
                }),
                Err(rejection)
            );
        }
    }

    #[test]
    fn canonical_spelling_preserves_punctuation_and_supported_named_keys() {
        for key in [
            "-",
            "+",
            "=",
            "'",
            "`",
            "é",
            "enter",
            "space",
            "tab",
            "escape",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
            "f1",
            "f20",
        ] {
            let source = format!("cmd-{key}");
            let shortcut = Shortcut::parse(&source).unwrap();
            assert_eq!(shortcut.to_string(), source);
            assert_eq!(shortcut.to_keystroke(), Keystroke::parse(&source).unwrap());
            assert_eq!(
                Shortcut::from_keystroke(&shortcut.to_keystroke()).unwrap(),
                shortcut
            );
        }
        assert_eq!(Shortcut::parse("CMD-K").unwrap().to_string(), "cmd-k");
        assert_eq!(
            Shortcut::parse("SHIFT-CmD-TAB").unwrap().to_string(),
            "shift-cmd-tab"
        );
    }
}
