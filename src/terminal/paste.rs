use libghostty_vt::paste;

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
    const fn requires_confirmation(self) -> bool {
        self.multiline || self.control_bytes || self.closing_fence
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PreparedPaste {
    text: String,
    risk: PasteRisk,
}

impl PreparedPaste {
    pub(super) fn prepare(text: String) -> Result<Self, PasteRejection> {
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

    pub(super) const fn requires_confirmation(&self) -> bool {
        self.risk.requires_confirmation()
    }

    pub(super) fn confirmation(&self, id: PasteConfirmationId) -> PasteConfirmation {
        PasteConfirmation {
            id,
            byte_len: self.text.len(),
            line_count: self.text.bytes().filter(|byte| *byte == b'\n').count() + 1,
            risk: self.risk,
        }
    }

    pub(super) fn into_text(self) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(prepared.requires_confirmation());
    }

    #[test]
    fn preparation_classifies_every_control_replaced_by_the_ghostty_encoder() {
        for byte in STRIPPED_CONTROLS {
            let prepared = PreparedPaste::prepare(String::from_utf8(vec![b'a', byte]).unwrap())
                .expect("control-bearing input remains encodable after confirmation");
            assert!(prepared.risk.control_bytes, "control byte {byte:#04x}");
            assert!(prepared.requires_confirmation());
        }
    }

    #[test]
    fn closing_fence_is_unsafe_even_before_bracketed_mode_is_known() {
        let prepared = PreparedPaste::prepare("safe\x1b[201~unsafe".to_owned()).unwrap();

        assert!(prepared.risk.closing_fence);
        assert!(prepared.requires_confirmation());
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
}
