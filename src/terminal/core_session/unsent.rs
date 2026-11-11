//! What the person typed into a Pane that the Core refused or never confirmed. The client keeps
//! the text so the person can copy it, and never sends it again by itself: it can hold Return,
//! and a new shell starts in another state. Pointer and wheel events, a copy shortcut and
//! modifier-only keys type nothing, so they are not kept.
use spaceterm_core::pane::Key;
use spaceterm_core::protocol::InputEvent;

use crate::terminal::PhysicalKey;

/// The most text kept. Typing during a restart is short; a large paste keeps its start.
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Which Input serials on the bound channel carried input typed at a hidden prompt, until the
/// Core confirms them. A kept serial is checked against this record, never against the Pane's
/// current prompt, which a failure resets before the input is kept.
#[derive(Debug, Default)]
pub(crate) struct HiddenSerials {
    serials: std::collections::BTreeSet<u32>,
}

impl HiddenSerials {
    /// The worker sent `serial`, typed at a hidden prompt when `hidden`.
    pub(super) fn sent(&mut self, serial: u32, hidden: bool) {
        if hidden {
            self.serials.insert(serial);
        }
    }

    /// The Core applied every Input through `serial`.
    pub(super) fn confirmed(&mut self, serial: u32) {
        self.serials.retain(|sent| *sent > serial);
    }

    /// Whether `serial` was typed at a hidden prompt.
    pub(super) fn hidden(&self, serial: u32) -> bool {
        self.serials.contains(&serial)
    }

    /// Whether any input the Core has not confirmed was typed at a hidden prompt. Unconfirmed
    /// input comes back as one list, so one hidden serial hides all of it.
    pub(super) fn any(&self) -> bool {
        !self.serials.is_empty()
    }

    /// A new channel numbers its Input from 1.
    pub(super) fn clear(&mut self) {
        self.serials.clear();
    }
}

/// Whether the Core received what the person typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The Core refused it, or it never left this client.
    NotSent,
    /// It left this client, and the Core never said whether it applied it.
    Unknown,
}

/// What a Pane tells the person about input it kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnsentInput {
    /// `NotSent` only when the Core received none of it.
    pub(crate) delivery: Delivery,
    /// The text to copy. Empty when everything was typed at a hidden prompt, such as a password,
    /// which is never kept.
    pub(crate) text: String,
}

/// What interrupted a Terminal Session the Pane attached to again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interruption {
    /// The Core refused input for an idle restart that then did not happen.
    Restart,
    /// The connection to the Core was lost.
    Connection,
}

/// What a Pane's status says once when it attaches again to the same Terminal Session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnsentNotice {
    pub(crate) interruption: Interruption,
    pub(crate) input: UnsentInput,
}

/// What changed about kept input since the Pane last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnsentChange {
    /// The Terminal Session the Pane showed kept input for ended or went to another client, so
    /// the Pane drops what it holds. The ended Pane offers what was kept since.
    pub(crate) session_left: bool,
    /// What to say after that, for the Terminal Session the Pane is attached to now.
    pub(crate) notice: Option<UnsentNotice>,
}

impl UnsentNotice {
    /// `self` followed by `later`, which a later attachment kept before the person saw `self`.
    /// The text runs on in order. Any unknown part makes all of it unknown, and any lost
    /// connection makes the cause a lost connection.
    pub(crate) fn then(self, later: Self) -> Self {
        let mut text = self.input.text;
        text.push_str(&later.input.text);
        let delivery = if self.input.delivery == Delivery::Unknown
            || later.input.delivery == Delivery::Unknown
        {
            Delivery::Unknown
        } else {
            Delivery::NotSent
        };
        let interruption = if self.interruption == Interruption::Restart
            && later.interruption == Interruption::Restart
        {
            Interruption::Restart
        } else {
            Interruption::Connection
        };
        Self {
            interruption,
            input: UnsentInput { delivery, text },
        }
    }
}

/// Input kept since the Pane last attached to a Terminal Session.
#[derive(Debug, Default)]
pub(crate) struct Unsent {
    text: String,
    not_sent: bool,
    unknown: bool,
    /// The Core refused input because it was granted an idle restart.
    restart_refused: bool,
    /// The Terminal Session the input was for ended, or another client controls it.
    left: bool,
}

impl Unsent {
    /// Keeps the text of `events`. `hidden` says the program was reading hidden input, so the
    /// text is not kept, only the fact that some was lost.
    pub(super) fn keep<'a>(
        &mut self,
        delivery: Delivery,
        events: impl IntoIterator<Item = &'a InputEvent>,
        hidden: bool,
    ) {
        for event in events {
            let typed = match event {
                InputEvent::Key(key) => self.key(key, hidden),
                InputEvent::Paste { text, .. } => {
                    if !hidden {
                        self.push(text);
                    }
                    !text.is_empty()
                }
                _ => false,
            };
            if typed {
                match delivery {
                    Delivery::NotSent => self.not_sent = true,
                    Delivery::Unknown => self.unknown = true,
                }
            }
        }
    }

    /// Keeps input the Core refused because it was granted an idle restart.
    pub(super) fn refused_by_restart<'a>(
        &mut self,
        events: impl IntoIterator<Item = &'a InputEvent>,
        hidden: bool,
    ) {
        self.keep(Delivery::NotSent, events, hidden);
        self.restart_refused = true;
    }

    /// The Terminal Session the input was for ended or belongs to another client. The ended
    /// Pane says what was kept, including `unshown`, a notice the Pane never showed; a later
    /// attachment says nothing.
    pub(super) fn left_session(&mut self, unshown: Option<UnsentNotice>) {
        if let Some(earlier) = unshown {
            let input = earlier.input;
            self.text = input.text + &self.text;
            match input.delivery {
                Delivery::NotSent => self.not_sent = true,
                Delivery::Unknown => self.unknown = true,
            }
        }
        self.left = true;
    }

    /// The Pane attached to a Terminal Session, which starts a new record. Returns what to tell
    /// the person when that Session survived what interrupted it: an idle restart grant that
    /// refused input, or a lost connection. After a lost connection the Pane cannot tell what
    /// reached the Core before it, so all of it may not have been sent.
    pub(super) fn attached(&mut self) -> Option<UnsentNotice> {
        let kept = std::mem::take(self);
        if kept.left {
            return None;
        }
        let mut input = kept.input()?;
        let interruption = if kept.restart_refused {
            Interruption::Restart
        } else {
            input.delivery = Delivery::Unknown;
            Interruption::Connection
        };
        Some(UnsentNotice {
            interruption,
            input,
        })
    }

    /// What was kept, if the person typed anything.
    pub(crate) fn input(&self) -> Option<UnsentInput> {
        (self.not_sent || self.unknown).then(|| UnsentInput {
            delivery: if self.unknown {
                Delivery::Unknown
            } else {
                Delivery::NotSent
            },
            text: self.text.clone(),
        })
    }

    /// Whether `key` typed something, after keeping its text. Return and Tab keep their
    /// characters and Backspace removes the last one. A key with Control or Command is a command,
    /// not text.
    fn key(&mut self, key: &Key, hidden: bool) -> bool {
        // 0 is a press and 1 a repeat. A release types nothing.
        if key.action > 1 || key.modifiers & 0b1100 != 0 {
            return false;
        }
        let physical = key.physical;
        let text = if physical == PhysicalKey::Enter as u32
            || physical == PhysicalKey::NumpadEnter as u32
        {
            "\n"
        } else if physical == PhysicalKey::Tab as u32 {
            "\t"
        } else if physical == PhysicalKey::Backspace as u32 {
            if !hidden {
                self.text.pop();
            }
            return true;
        } else {
            match key.text.as_deref() {
                Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => text,
                _ => return false,
            }
        };
        if !hidden {
            self.push(text);
        }
        true
    }

    fn push(&mut self, text: &str) {
        let room = MAX_TEXT_BYTES.saturating_sub(self.text.len());
        let mut end = text.len().min(room);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(physical: PhysicalKey, text: Option<&str>, action: u8, modifiers: u16) -> InputEvent {
        InputEvent::Key(Key {
            physical: physical as u32,
            action,
            logical: String::new(),
            text: text.map(str::to_owned),
            unshifted: None,
            modifiers,
            consumed: 0,
            option_as_alt: false,
        })
    }

    fn typed(text: &str) -> Vec<InputEvent> {
        text.chars()
            .flat_map(|c| {
                let text = c.to_string();
                [
                    key(PhysicalKey::A, Some(&text), 0, 0),
                    key(PhysicalKey::A, None, 2, 0),
                ]
            })
            .collect()
    }

    /// The kept text reads as the person typed it: Return is a new line, Backspace edits, and a
    /// Control key or a release adds nothing.
    #[test]
    fn kept_keys_read_as_the_typed_text() {
        let mut unsent = Unsent::default();
        let mut events = typed("gti");
        events.push(key(PhysicalKey::Backspace, None, 0, 0));
        events.push(key(PhysicalKey::Backspace, None, 0, 0));
        events.extend(typed("it push"));
        events.push(key(PhysicalKey::C, Some("c"), 0, 0b100));
        events.push(key(PhysicalKey::Enter, Some("\r"), 0, 0));
        unsent.keep(Delivery::NotSent, &events, false);
        assert_eq!(
            unsent.input(),
            Some(UnsentInput {
                delivery: Delivery::NotSent,
                text: "git push\n".into()
            })
        );
    }

    /// Input whose delivery is unknown makes all of it unknown, and text typed at a hidden prompt
    /// is never kept.
    #[test]
    fn unknown_delivery_and_hidden_input_are_told_apart() {
        let mut unsent = Unsent::default();
        unsent.keep(Delivery::NotSent, &typed("ls"), false);
        unsent.keep(Delivery::Unknown, &typed("secret"), true);
        assert_eq!(
            unsent.input(),
            Some(UnsentInput {
                delivery: Delivery::Unknown,
                text: "ls".into()
            })
        );
        let mut moved = Unsent::default();
        moved.keep(
            Delivery::Unknown,
            &[InputEvent::CopyOrForward { modifiers: 0 }],
            false,
        );
        assert_eq!(moved.input(), None, "a copy shortcut is not typing");
    }

    /// After an ordinary connection loss the same Terminal Session hears what was kept, only as
    /// possibly unsent, even for input that never left this client.
    #[test]
    fn a_session_reattached_after_a_lost_connection_hears_what_may_not_have_been_sent() {
        let mut unsent = Unsent::default();
        unsent.keep(Delivery::NotSent, &typed("cd "), false);
        unsent.keep(Delivery::Unknown, &typed("src"), false);
        assert_eq!(
            unsent.attached().map(|notice| notice.input),
            Some(UnsentInput {
                delivery: Delivery::Unknown,
                text: "cd src".into()
            })
        );
        assert_eq!(unsent.attached(), None, "it says so once");
    }

    /// A notice the Pane has not shown yet runs on with the next one, and one never shown before
    /// the Session ended goes to the ended Pane.
    #[test]
    fn kept_input_is_merged_and_never_overwritten() {
        let notice = |interruption, delivery, text: &str| UnsentNotice {
            interruption,
            input: UnsentInput {
                delivery,
                text: text.into(),
            },
        };
        let restart = notice(Interruption::Restart, Delivery::NotSent, "ls ");
        assert_eq!(
            restart
                .clone()
                .then(notice(Interruption::Restart, Delivery::NotSent, "-la")),
            notice(Interruption::Restart, Delivery::NotSent, "ls -la")
        );
        assert_eq!(
            restart
                .clone()
                .then(notice(Interruption::Connection, Delivery::Unknown, "-la")),
            notice(Interruption::Connection, Delivery::Unknown, "ls -la")
        );

        let mut unsent = Unsent::default();
        unsent.keep(Delivery::NotSent, &typed("-la"), false);
        unsent.left_session(Some(restart));
        assert_eq!(
            unsent.input(),
            Some(UnsentInput {
                delivery: Delivery::NotSent,
                text: "ls -la".into()
            })
        );
    }

    /// A Terminal Session that survived the refusing grant hears about it on attachment; one that
    /// ended does not. Every attachment starts a new record.
    #[test]
    fn only_a_surviving_session_hears_about_its_refused_input() {
        let mut unsent = Unsent::default();
        unsent.refused_by_restart(&typed("ls"), false);
        assert_eq!(
            unsent.attached(),
            Some(UnsentNotice {
                interruption: Interruption::Restart,
                input: UnsentInput {
                    delivery: Delivery::NotSent,
                    text: "ls".into()
                }
            })
        );
        assert_eq!(unsent.input(), None);

        unsent.refused_by_restart(&typed("ls"), false);
        unsent.left_session(None);
        assert_eq!(unsent.input().map(|input| input.text), Some("ls".into()));
        assert_eq!(unsent.attached(), None, "a new Session says nothing");
        assert_eq!(unsent.input(), None);
    }
}
