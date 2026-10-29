//! Named keys, as `pane send --key` types them: `Enter`, `Tab`, `Escape`, `BSpace`, the arrows,
//! `Home`, `End`, `PageUp`, `PageDown`, `F1` to `F12`, `C-a` to `C-z` and `M-<char>`.
//!
//! Each name becomes the [`Key`] events a person's keyboard sends for it. `physical` carries the
//! server terminal's physical key numbering.
use super::Key;

/// The control modifier bit of [`Key::modifiers`].
const CONTROL: u16 = 1 << 2;

/// The physical key numbers of the keys a name can press.
mod physical {
    pub const A: u32 = 20;
    pub const BACKSPACE: u32 = 53;
    pub const ENTER: u32 = 58;
    pub const TAB: u32 = 64;
    pub const END: u32 = 69;
    pub const HOME: u32 = 71;
    pub const PAGE_DOWN: u32 = 73;
    pub const PAGE_UP: u32 = 74;
    pub const ARROW_DOWN: u32 = 75;
    pub const ARROW_LEFT: u32 = 76;
    pub const ARROW_RIGHT: u32 = 77;
    pub const ARROW_UP: u32 = 78;
    pub const ESCAPE: u32 = 120;
    pub const F1: u32 = 121;
}

/// The name is not one `pane send --key` accepts. It carries no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownKey;

/// The key events one name presses, in order. `M-<char>` is Escape and then the character, as a
/// terminal sends Meta.
pub fn named(name: &str) -> Result<Vec<Key>, UnknownKey> {
    let press = |physical: u32, logical: &str| press(physical, logical, 0, None);
    Ok(match name {
        "Enter" => vec![press(physical::ENTER, "enter")],
        "Tab" => vec![press(physical::TAB, "tab")],
        "Escape" => vec![press(physical::ESCAPE, "escape")],
        "BSpace" => vec![press(physical::BACKSPACE, "backspace")],
        "Up" => vec![press(physical::ARROW_UP, "arrowup")],
        "Down" => vec![press(physical::ARROW_DOWN, "arrowdown")],
        "Left" => vec![press(physical::ARROW_LEFT, "arrowleft")],
        "Right" => vec![press(physical::ARROW_RIGHT, "arrowright")],
        "Home" => vec![press(physical::HOME, "home")],
        "End" => vec![press(physical::END, "end")],
        "PageUp" => vec![press(physical::PAGE_UP, "pageup")],
        "PageDown" => vec![press(physical::PAGE_DOWN, "pagedown")],
        _ => {
            if let Some(digits) = name.strip_prefix('F')
                && let Ok(number @ 1..=12) = digits.parse::<u32>()
                && digits == number.to_string()
            {
                return Ok(vec![press(
                    physical::F1 + number - 1,
                    &format!("f{number}"),
                )]);
            }
            if let Some(letter) = name.strip_prefix("C-")
                && let [letter @ b'a'..=b'z'] = letter.as_bytes()
            {
                let character = char::from(*letter);
                return Ok(vec![self::press(
                    physical::A + u32::from(letter - b'a'),
                    &character.to_string(),
                    CONTROL,
                    Some(character),
                )]);
            }
            if let Some(rest) = name.strip_prefix("M-") {
                let mut characters = rest.chars();
                if let (Some(character), None) = (characters.next(), characters.next())
                    && !character.is_control()
                {
                    return Ok(vec![press(physical::ESCAPE, "escape"), text(&character.to_string())]);
                }
            }
            return Err(UnknownKey);
        }
    })
}

/// Text a person types, as one text-input key. The server turns its control characters into
/// the keys that type them.
pub fn text(text: &str) -> Key {
    Key {
        physical: 0,
        action: 0,
        logical: "text-input".to_owned(),
        text: Some(text.to_owned()),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}

fn press(physical: u32, logical: &str, modifiers: u16, unshifted: Option<char>) -> Key {
    Key {
        physical,
        action: 0,
        logical: logical.to_owned(),
        text: None,
        unshifted,
        modifiers,
        consumed: 0,
        option_as_alt: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_name_presses_keys_and_nothing_else_does() {
        let names = [
            "Enter", "Tab", "Escape", "BSpace", "Up", "Down", "Left", "Right", "Home", "End",
            "PageUp", "PageDown", "F1", "F12", "C-a", "C-z", "M-x", "M-.",
        ];
        for name in names {
            assert!(!named(name).unwrap().is_empty(), "{name}");
        }
        assert_eq!(named("M-x").unwrap().len(), 2);
        for name in [
            "enter", "F0", "F13", "F01", "C-A", "C-ab", "C-", "M-", "M-xy", "M-\u{7}", "", "Space",
        ] {
            assert!(named(name).is_err(), "{name}");
        }
    }
}
