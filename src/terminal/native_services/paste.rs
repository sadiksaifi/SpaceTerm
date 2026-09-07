use libghostty_vt::paste;
use std::time::{Duration, Instant};

const PASTE_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const MAX_PASTE_BYTES: usize = 1024 * 1024;

const BRACKETED_PASTE_END: &str = "\x1b[201~";
const STRIPPED_CONTROLS: [u8; 16] = [
    0x00, 0x08, 0x05, 0x04, 0x1b, 0x7f, 0x03, 0x1c, 0x15, 0x1a, 0x11, 0x13, 0x17, 0x16, 0x12, 0x0f,
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PasteConfirmationId(u64);

impl PasteConfirmationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PasteRisk {
    pub(crate) multiline: bool,
    pub(crate) control_bytes: bool,
    pub(crate) closing_fence: bool,
}

impl PasteRisk {
    const fn requires_confirmation(self, bracketed_paste: bool) -> bool {
        self.closing_fence || (self.multiline && !bracketed_paste)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PasteConfirmation {
    pub(crate) id: PasteConfirmationId,
    pub(crate) byte_len: usize,
    pub(crate) line_count: usize,
    pub(crate) risk: PasteRisk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteRequestOutcome {
    Written,
    ConfirmationRequired(PasteConfirmation),
    Rejected(PasteRejection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteRejection {
    Empty,
    TooLarge { limit: usize },
    ConfirmationPending,
    TerminalUnfocused,
}

impl std::fmt::Display for PasteRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the paste is empty"),
            Self::TooLarge { limit } => {
                write!(formatter, "the paste exceeds the {limit}-byte safety limit")
            }
            Self::ConfirmationPending => {
                formatter.write_str("another unsafe paste is awaiting confirmation")
            }
            Self::TerminalUnfocused => {
                formatter.write_str("the terminal no longer owns input focus")
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteDecision {
    Confirm,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteResolution {
    Written,
    Cancelled,
    Stale,
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::terminal) struct PreparedPaste {
    text: String,
    risk: PasteRisk,
}

impl std::fmt::Debug for PreparedPaste {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedPaste")
            .field("byte_len", &self.text.len())
            .field("risk", &self.risk)
            .finish_non_exhaustive()
    }
}

impl PreparedPaste {
    pub(in crate::terminal) fn prepare(text: String) -> Result<Self, PasteRejection> {
        if text.is_empty() {
            return Err(PasteRejection::Empty);
        }
        if text.len() > MAX_PASTE_BYTES {
            return Err(PasteRejection::TooLarge {
                limit: MAX_PASTE_BYTES,
            });
        }

        let text = normalize_newlines(text);
        let bytes = text.as_bytes();
        let risk = PasteRisk {
            multiline: text.contains('\n'),
            control_bytes: bytes.iter().any(|byte| STRIPPED_CONTROLS.contains(byte)),
            closing_fence: text.contains(BRACKETED_PASTE_END),
        };
        debug_assert_eq!(
            paste::is_safe(&text),
            !risk.multiline && !risk.closing_fence
        );

        Ok(Self { text, risk })
    }

    pub(in crate::terminal) const fn requires_confirmation(&self, bracketed_paste: bool) -> bool {
        self.risk.requires_confirmation(bracketed_paste)
    }

    pub(in crate::terminal) fn confirmation(&self, id: PasteConfirmationId) -> PasteConfirmation {
        PasteConfirmation {
            id,
            byte_len: self.text.len(),
            line_count: self.text.bytes().filter(|byte| *byte == b'\n').count() + 1,
            risk: self.risk,
        }
    }

    pub(in crate::terminal) fn into_text(self) -> String {
        self.text
    }
}

fn normalize_newlines(text: String) -> String {
    if !text.as_bytes().contains(&b'\r') {
        return text;
    }

    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    normalized
}

struct PendingPaste {
    id: PasteConfirmationId,
    payload: PreparedPaste,
    deadline: Instant,
}

#[derive(Default)]
pub(in crate::terminal) struct PasteConfirmationSchedule {
    next_id: u64,
    pending: Option<PendingPaste>,
}

impl PasteConfirmationSchedule {
    pub(in crate::terminal) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    pub(in crate::terminal) fn create(
        &mut self,
        payload: PreparedPaste,
        now: Instant,
    ) -> Option<PasteConfirmation> {
        if self.pending.is_some() {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = PasteConfirmationId::new(self.next_id);
        let confirmation = payload.confirmation(id);
        self.pending = Some(PendingPaste {
            id,
            payload,
            deadline: now + PASTE_CONFIRMATION_TIMEOUT,
        });
        Some(confirmation)
    }

    pub(in crate::terminal) fn take(
        &mut self,
        id: PasteConfirmationId,
        now: Instant,
    ) -> Option<PreparedPaste> {
        let pending = self.pending.take()?;
        if pending.id == id && now < pending.deadline {
            Some(pending.payload)
        } else {
            None
        }
    }

    pub(in crate::terminal) fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    pub(in crate::terminal) fn cancel(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_paste_debug_never_exposes_payload_contents() {
        let paste = PreparedPaste::prepare("private clipboard content".to_owned()).unwrap();
        assert_eq!(
            format!("{paste:?}"),
            "PreparedPaste { byte_len: 25, risk: PasteRisk { multiline: false, control_bytes: false, closing_fence: false }, .. }",
        );
    }

    #[test]
    fn paste_confirmation_schedule_expires_without_exposing_payload() {
        let now = Instant::now();
        let mut schedule = PasteConfirmationSchedule::default();
        let payload = PreparedPaste::prepare("first\nsecond".to_owned()).unwrap();
        let confirmation = schedule.create(payload, now).unwrap();

        assert!(schedule.expire(now + PASTE_CONFIRMATION_TIMEOUT));
        assert_eq!(schedule.take(confirmation.id, now), None);
    }

    #[test]
    fn preparation_normalizes_newlines_and_classifies_multiline_input() {
        let prepared = PreparedPaste::prepare("one\r\ntwo\rthree".to_owned()).unwrap();

        assert_eq!(prepared.text, "one\ntwo\nthree");
        assert_eq!(
            prepared.risk,
            PasteRisk {
                multiline: true,
                control_bytes: false,
                closing_fence: false,
            }
        );
        assert!(prepared.requires_confirmation(false));
        assert!(!prepared.requires_confirmation(true));
    }

    #[test]
    fn preparation_classifies_and_trusts_every_control_replaced_by_the_ghostty_encoder() {
        for byte in STRIPPED_CONTROLS {
            let prepared = PreparedPaste::prepare(String::from_utf8(vec![b'a', byte]).unwrap())
                .expect("control-bearing input remains encodable after sanitization");
            assert!(prepared.risk.control_bytes, "control byte {byte:#04x}");
            assert!(!prepared.requires_confirmation(false));
        }
    }

    #[test]
    fn closing_fence_is_unsafe_even_before_bracketed_mode_is_known() {
        let prepared = PreparedPaste::prepare("safe\x1b[201~unsafe".to_owned()).unwrap();

        assert!(prepared.risk.closing_fence);
        assert!(prepared.requires_confirmation(false));
        assert!(prepared.requires_confirmation(true));
    }

    #[test]
    fn oversized_and_empty_payloads_are_rejected_without_retaining_content() {
        assert_eq!(
            PreparedPaste::prepare(String::new()),
            Err(PasteRejection::Empty)
        );
        assert_eq!(
            PreparedPaste::prepare("x".repeat(MAX_PASTE_BYTES + 1)),
            Err(PasteRejection::TooLarge {
                limit: MAX_PASTE_BYTES,
            })
        );
    }

    #[test]
    fn converted_file_paths_still_require_the_unified_unsafe_paste_policy() {
        let insertion = crate::terminal::file_insertion::prepare_file_insertion(
            crate::terminal::native_services::file_insertion::FileInsertionPolicy::fixture(),
            &[std::path::PathBuf::from("/tmp/line\nbreak")],
        )
        .unwrap();
        let prepared = PreparedPaste::prepare(insertion.text).unwrap();
        assert!(prepared.requires_confirmation(false));
        assert!(prepared.risk.multiline);
    }
}
