//! Chord capture: the keystrokes of one window, taken ahead of key bindings and menu key
//! equivalents while one focus target holds focus.
use std::rc::Rc;

use gpui::{AnyWindowHandle, Context, FocusHandle, Keystroke, Modifiers, Subscription, Window};

/// Keys GPUI reports when a modifier is pressed and released on its own. A modifier alone never
/// completes a chord, so capture keeps waiting for the key it modifies.
const MODIFIER_KEYS: [&str; 5] = ["shift", "control", "alt", "platform", "function"];

/// One keystroke a capture took, classified by what a capturing control does with it.
#[derive(Clone, Debug, PartialEq)]
pub enum CapturedKey {
    /// A key chord, without the text it would type. Nothing else receives it.
    Chord(Keystroke),
    /// Escape without modifiers. Nothing else receives it.
    Escape,
    /// Backspace or Delete without modifiers. Nothing else receives it.
    Erase,
    /// Tab or Shift-Tab. The keystroke continues to focus traversal, so a keyboard reader can
    /// always leave the capturing control.
    Traverse,
    /// Focus left the capturing control without a blur reaching its owner, for example to another
    /// window's sheet. The keystroke continues to wherever focus went.
    FocusLost,
}

/// Captures key chords in one window while one focus target is focused.
///
/// A capture sees every keystroke before key bindings and menu key equivalents resolve, so a chord
/// that already means something, such as closing the window, is captured rather than performed.
/// Its owner decides what each [`CapturedKey`] means and ends the capture by dropping it.
pub struct ChordCapture {
    _interceptor: Subscription,
}

impl ChordCapture {
    /// Starts capturing the keystrokes of `window` while `focus` is focused, reporting each one to
    /// `on_key` on the owning view.
    pub fn start<V: 'static>(
        focus: FocusHandle,
        window: &Window,
        cx: &mut Context<V>,
        on_key: impl Fn(&mut V, CapturedKey, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        let owner = cx.weak_entity();
        let capturing: AnyWindowHandle = window.window_handle();
        let on_key = Rc::new(on_key);
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != capturing {
                return;
            }
            let Some(key) = classify(&event.keystroke, focus.is_focused(window)) else {
                return;
            };
            if !matches!(key, CapturedKey::FocusLost | CapturedKey::Traverse) {
                cx.stop_propagation();
            }
            let on_key = Rc::clone(&on_key);
            let _ = owner.update(cx, |owner, cx| on_key(owner, key, window, cx));
        });
        Self {
            _interceptor: interceptor,
        }
    }
}

/// What one keystroke means to a capture, or `None` for a modifier pressed on its own.
fn classify(keystroke: &Keystroke, focused: bool) -> Option<CapturedKey> {
    if !focused {
        return Some(CapturedKey::FocusLost);
    }
    let key = keystroke.key.as_str();
    if MODIFIER_KEYS.contains(&key) {
        return None;
    }
    let modifiers = keystroke.modifiers;
    let bare = !modifiers.modified();
    Some(match key {
        "escape" if bare => CapturedKey::Escape,
        "backspace" | "delete" if bare => CapturedKey::Erase,
        "tab" if bare || modifiers == Modifiers::shift() => CapturedKey::Traverse,
        _ => CapturedKey::Chord(Keystroke {
            key_char: None,
            ..keystroke.clone()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source: &str) -> Keystroke {
        Keystroke::parse(source).expect("keystroke")
    }

    #[test]
    fn a_modifier_alone_is_not_captured() {
        assert_eq!(classify(&key("shift"), true), None);
        assert_eq!(classify(&key("alt"), true), None);
    }

    #[test]
    fn bare_control_keys_are_classified_and_modified_ones_are_chords() {
        assert_eq!(classify(&key("escape"), true), Some(CapturedKey::Escape));
        assert_eq!(classify(&key("backspace"), true), Some(CapturedKey::Erase));
        assert_eq!(
            classify(&key("shift-tab"), true),
            Some(CapturedKey::Traverse)
        );
        assert_eq!(
            classify(&key("cmd-backspace"), true),
            Some(CapturedKey::Chord(key("cmd-backspace")))
        );
        assert_eq!(
            classify(&key("ctrl-tab"), true),
            Some(CapturedKey::Chord(key("ctrl-tab")))
        );
    }

    #[test]
    fn a_chord_drops_the_text_it_would_type() {
        let typed = Keystroke {
            key_char: Some("k".to_owned()),
            ..key("alt-k")
        };

        assert_eq!(
            classify(&typed, true),
            Some(CapturedKey::Chord(key("alt-k")))
        );
    }

    #[test]
    fn a_keystroke_without_focus_reports_focus_lost() {
        assert_eq!(classify(&key("cmd-k"), false), Some(CapturedKey::FocusLost));
    }
}
