use std::{fmt, mem};

pub(crate) const MAX_OSC52_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_OSC52_ENCODED_BYTES: usize = MAX_OSC52_CONTENT_BYTES.div_ceil(3) * 4;
const OSC52_PREFIX: &[u8] = b"\x1b]52;";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::terminal) enum Osc52Access {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52Target {
    Default,
    Standard,
    Selection,
    Primary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::terminal) enum Osc52Terminator {
    Bell,
    StringTerminator,
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::terminal) enum Osc52Operation {
    Read {
        target: Osc52Target,
        terminator: Osc52Terminator,
    },
    Write {
        target: Osc52Target,
        text: String,
    },
}

impl fmt::Debug for Osc52Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Osc52Operation")
            .field("access", &self.access())
            .field("target", &self.target())
            .field("byte_len", &self.byte_len())
            .finish_non_exhaustive()
    }
}

impl Osc52Operation {
    pub(in crate::terminal) const fn access(&self) -> Osc52Access {
        match self {
            Self::Read { .. } => Osc52Access::Read,
            Self::Write { .. } => Osc52Access::Write,
        }
    }

    pub(in crate::terminal) const fn target(&self) -> Osc52Target {
        match self {
            Self::Read { target, .. } | Self::Write { target, .. } => *target,
        }
    }

    pub(in crate::terminal) fn byte_len(&self) -> usize {
        match self {
            Self::Read { .. } => 0,
            Self::Write { text, .. } => text.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::terminal) enum Osc52Rejection {
    Malformed,
    Oversized,
    UnsupportedTarget,
    InvalidBase64,
    InvalidUtf8,
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::terminal) enum Osc52Effect {
    Terminal(Vec<u8>),
    Operation(Osc52Operation),
    Rejected(Osc52Rejection),
}

impl fmt::Debug for Osc52Effect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terminal(_) => formatter.debug_struct("Terminal").finish_non_exhaustive(),
            Self::Operation(operation) => {
                formatter.debug_tuple("Operation").field(operation).finish()
            }
            Self::Rejected(rejection) => {
                formatter.debug_tuple("Rejected").field(rejection).finish()
            }
        }
    }
}

#[derive(Debug)]
enum FilterState {
    Ground,
    Prefix,
    Osc52 { escape_pending: bool },
    DiscardOversized { escape_pending: bool },
    OtherString { bell: bool, escape_pending: bool },
}

pub(in crate::terminal) struct Osc52Filter<Context = ()> {
    state: FilterState,
    candidate: Vec<u8>,
    context: Context,
}

impl<Context> fmt::Debug for Osc52Filter<Context> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Osc52Filter")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<Context: Default> Default for Osc52Filter<Context> {
    fn default() -> Self {
        Self {
            state: FilterState::Ground,
            candidate: Vec::new(),
            context: Context::default(),
        }
    }
}

impl<Context: Copy> Osc52Filter<Context> {
    /// Retains the caller's receipt context from the initial ESC through frame completion.
    pub(in crate::terminal) fn feed_with_context(
        &mut self,
        bytes: &[u8],
        context: Context,
    ) -> Vec<(Osc52Effect, Context)> {
        let mut effects = Vec::new();
        let mut terminal = Vec::with_capacity(bytes.len());

        for &byte in bytes {
            match self.state {
                FilterState::Ground => {
                    if byte == 0x1b {
                        flush_terminal(&mut effects, &mut terminal, context);
                        self.candidate.push(byte);
                        self.context = context;
                        self.state = FilterState::Prefix;
                    } else {
                        terminal.push(byte);
                    }
                }
                FilterState::Prefix => {
                    self.candidate.push(byte);
                    if OSC52_PREFIX.starts_with(&self.candidate) {
                        if self.candidate.len() == OSC52_PREFIX.len() {
                            self.state = FilterState::Osc52 {
                                escape_pending: false,
                            };
                        }
                    } else {
                        let candidate = mem::take(&mut self.candidate);
                        self.state = if matches!(byte, 0x18 | 0x1a)
                            || (candidate.starts_with(b"\x1b]") && byte == 0x07)
                        {
                            FilterState::Ground
                        } else if candidate.starts_with(b"\x1b]") {
                            FilterState::OtherString {
                                bell: true,
                                escape_pending: byte == 0x1b,
                            }
                        } else if candidate
                            .get(1)
                            .is_some_and(|byte| matches!(*byte, b'P' | b'_' | b'^' | b'X'))
                        {
                            FilterState::OtherString {
                                bell: false,
                                escape_pending: byte == 0x1b,
                            }
                        } else {
                            FilterState::Ground
                        };
                        if matches!(self.state, FilterState::Ground) && byte == 0x1b {
                            terminal.extend_from_slice(&candidate[..candidate.len() - 1]);
                            self.candidate.push(byte);
                            self.context = context;
                            self.state = FilterState::Prefix;
                        } else {
                            terminal.extend(candidate);
                        }
                    }
                }
                FilterState::OtherString {
                    bell,
                    escape_pending,
                } => {
                    terminal.push(byte);
                    self.state = if (bell && byte == 0x07)
                        || (escape_pending && byte == b'\\')
                        || matches!(byte, 0x18 | 0x1a)
                    {
                        FilterState::Ground
                    } else {
                        FilterState::OtherString {
                            bell,
                            escape_pending: byte == 0x1b,
                        }
                    };
                }
                FilterState::Osc52 { mut escape_pending } => {
                    if matches!(byte, 0x18 | 0x1a) {
                        self.candidate.clear();
                        self.state = FilterState::Ground;
                        effects.push((
                            Osc52Effect::Rejected(Osc52Rejection::Malformed),
                            self.context,
                        ));
                        continue;
                    }
                    self.candidate.push(byte);
                    let complete = byte == 0x07 || (escape_pending && byte == b'\\');
                    escape_pending = byte == 0x1b;
                    if complete {
                        let raw = mem::take(&mut self.candidate);
                        effects.push((
                            match parse_osc52(&raw) {
                                Ok(operation) => Osc52Effect::Operation(operation),
                                Err(rejection) => Osc52Effect::Rejected(rejection),
                            },
                            self.context,
                        ));
                        self.state = FilterState::Ground;
                    } else if self.candidate.len() > MAX_OSC52_ENCODED_BYTES + 16 {
                        self.candidate.clear();
                        self.state = FilterState::DiscardOversized { escape_pending };
                    } else {
                        self.state = FilterState::Osc52 { escape_pending };
                    }
                }
                FilterState::DiscardOversized { mut escape_pending } => {
                    let complete = byte == 0x07
                        || (escape_pending && byte == b'\\')
                        || matches!(byte, 0x18 | 0x1a);
                    escape_pending = byte == 0x1b;
                    if complete {
                        effects.push((
                            Osc52Effect::Rejected(Osc52Rejection::Oversized),
                            self.context,
                        ));
                        self.state = FilterState::Ground;
                    } else {
                        self.state = FilterState::DiscardOversized { escape_pending };
                    }
                }
            }
        }

        flush_terminal(&mut effects, &mut terminal, context);
        effects
    }
}

fn flush_terminal<Context: Copy>(
    effects: &mut Vec<(Osc52Effect, Context)>,
    terminal: &mut Vec<u8>,
    context: Context,
) {
    if !terminal.is_empty() {
        effects.push((Osc52Effect::Terminal(mem::take(terminal)), context));
    }
}

fn parse_osc52(raw: &[u8]) -> Result<Osc52Operation, Osc52Rejection> {
    let (terminator, end) = if raw.ends_with(b"\x1b\\") {
        (Osc52Terminator::StringTerminator, raw.len() - 2)
    } else if raw.last() == Some(&0x07) {
        (Osc52Terminator::Bell, raw.len() - 1)
    } else {
        return Err(Osc52Rejection::Malformed);
    };
    let body = raw
        .get(OSC52_PREFIX.len()..end)
        .ok_or(Osc52Rejection::Malformed)?;
    let separator = body
        .iter()
        .position(|byte| *byte == b';')
        .ok_or(Osc52Rejection::Malformed)?;
    let target = match &body[..separator] {
        b"" => Osc52Target::Default,
        b"c" => Osc52Target::Standard,
        b"s" => Osc52Target::Selection,
        b"p" => Osc52Target::Primary,
        _ => return Err(Osc52Rejection::UnsupportedTarget),
    };
    let payload = &body[separator + 1..];
    if payload == b"?" {
        return Ok(Osc52Operation::Read { target, terminator });
    }
    if payload.len() > MAX_OSC52_ENCODED_BYTES {
        return Err(Osc52Rejection::Oversized);
    }
    let decoded = decode_base64(payload)?;
    if decoded.len() > MAX_OSC52_CONTENT_BYTES {
        return Err(Osc52Rejection::Oversized);
    }
    let text = String::from_utf8(decoded).map_err(|_| Osc52Rejection::InvalidUtf8)?;
    Ok(Osc52Operation::Write { target, text })
}

pub(in crate::terminal) fn read_response(
    target: Osc52Target,
    terminator: Osc52Terminator,
    text: &str,
) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let selector: &[u8] = match target {
        Osc52Target::Default => b"",
        Osc52Target::Standard => b"c",
        Osc52Target::Selection => b"s",
        Osc52Target::Primary => b"p",
    };
    let mut response = Vec::with_capacity(text.len().div_ceil(3) * 4 + 10);
    response.extend_from_slice(b"\x1b]52;");
    response.extend_from_slice(selector);
    response.push(b';');
    for chunk in text.as_bytes().chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        response.push(ALPHABET[usize::from(a >> 2)]);
        response.push(ALPHABET[usize::from(((a & 3) << 4) | (b >> 4))]);
        response.push(if chunk.len() > 1 {
            ALPHABET[usize::from(((b & 15) << 2) | (c >> 6))]
        } else {
            b'='
        });
        response.push(if chunk.len() > 2 {
            ALPHABET[usize::from(c & 63)]
        } else {
            b'='
        });
    }
    response.extend_from_slice(match terminator {
        Osc52Terminator::Bell => b"\x07",
        Osc52Terminator::StringTerminator => b"\x1b\\",
    });
    response
}

fn decode_base64(input: &[u8]) -> Result<Vec<u8>, Osc52Rejection> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if !input.len().is_multiple_of(4) {
        return Err(Osc52Rejection::InvalidBase64);
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for (index, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
        let last = index + 1 == input.len() / 4;
        let padding = match (chunk[2] == b'=', chunk[3] == b'=') {
            (true, true) => 2,
            (false, true) => 1,
            (false, false) => 0,
            (true, false) => return Err(Osc52Rejection::InvalidBase64),
        };
        if padding != 0 && !last {
            return Err(Osc52Rejection::InvalidBase64);
        }
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        let c = if padding == 2 {
            0
        } else {
            base64_value(chunk[2])?
        };
        let d = if padding == 0 {
            base64_value(chunk[3])?
        } else {
            0
        };
        if (padding == 2 && b & 0x0f != 0) || (padding == 1 && c & 0x03 != 0) {
            return Err(Osc52Rejection::InvalidBase64);
        }
        output.push((a << 2) | (b >> 4));
        if padding < 2 {
            output.push((b << 4) | (c >> 2));
        }
        if padding == 0 {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Result<u8, Osc52Rejection> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(Osc52Rejection::InvalidBase64),
    }
}

#[cfg(test)]
mod tests {

    impl Osc52Filter {
        pub(in crate::terminal) fn feed(&mut self, bytes: &[u8]) -> Vec<Osc52Effect> {
            self.feed_with_context(bytes, ())
                .into_iter()
                .map(|(effect, ())| effect)
                .collect()
        }
    }

    use super::*;

    #[test]
    fn clipboard_sequence_is_consumed_once_without_reaching_the_emulator() {
        let mut filter = Osc52Filter::default();
        assert_eq!(
            filter.feed(b"before\x1b]52;c;aGVsbG8=\x07after"),
            vec![
                Osc52Effect::Terminal(b"before".to_vec()),
                Osc52Effect::Operation(Osc52Operation::Write {
                    target: Osc52Target::Standard,
                    text: "hello".to_owned(),
                }),
                Osc52Effect::Terminal(b"after".to_vec()),
            ],
        );
    }

    #[test]
    fn unrelated_empty_osc_does_not_hide_next_clipboard_operation() {
        let mut filter = Osc52Filter::default();
        assert_eq!(
            filter.feed(b"\x1b]\x07\x1b]52;c;?\x07"),
            vec![
                Osc52Effect::Terminal(b"\x1b]\x07".to_vec()),
                Osc52Effect::Operation(Osc52Operation::Read {
                    target: Osc52Target::Standard,
                    terminator: Osc52Terminator::Bell,
                }),
            ]
        );
    }

    #[test]
    fn clipboard_inside_other_control_strings_is_not_executed() {
        for prefix in [b"\x1bP".as_slice(), b"\x1b_", b"\x1b]0;"] {
            let raw = [prefix, b"\x1b]52;c;c2VjcmV0\x07\x1b\\"].concat();
            let mut filter = Osc52Filter::default();
            assert!(operations(&filter.feed(&raw)).is_empty());
        }
    }

    #[test]
    fn every_selector_and_terminator_round_trips_unicode_and_base64_padding() {
        for (target, selector) in [
            (Osc52Target::Default, b"".as_slice()),
            (Osc52Target::Standard, b"c"),
            (Osc52Target::Primary, b"p"),
            (Osc52Target::Selection, b"s"),
        ] {
            for (terminator, ending) in [
                (Osc52Terminator::Bell, b"\x07".as_slice()),
                (Osc52Terminator::StringTerminator, b"\x1b\\"),
            ] {
                for (text, base64) in [
                    ("", b"".as_slice()),
                    ("a", b"YQ=="),
                    ("ab", b"YWI="),
                    ("abc", b"YWJj"),
                    ("😀 text", b"8J+YgCB0ZXh0"),
                ] {
                    let response = read_response(target, terminator, text);
                    assert_eq!(
                        response,
                        [b"\x1b]52;".as_slice(), selector, b";", base64, ending].concat()
                    );
                    assert_eq!(
                        parse_osc52(&response),
                        Ok(Osc52Operation::Write {
                            target,
                            text: text.into()
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn decoded_text_limit_accepts_boundary_and_rejects_next_byte() {
        for (size, accepted) in [(1_048_576, true), (1_048_577, false)] {
            let sequence = read_response(
                Osc52Target::Standard,
                Osc52Terminator::Bell,
                &"x".repeat(size),
            );
            let mut filter = Osc52Filter::default();
            let effects = filter.feed(&sequence);
            if accepted {
                assert_eq!(
                    effects,
                    vec![Osc52Effect::Operation(Osc52Operation::Write {
                        target: Osc52Target::Standard,
                        text: "x".repeat(size),
                    })]
                );
            } else {
                assert_eq!(
                    effects,
                    vec![Osc52Effect::Rejected(Osc52Rejection::Oversized)]
                );
            }
        }
    }

    fn operations(effects: &[Osc52Effect]) -> Vec<Osc52Operation> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Osc52Effect::Operation(operation) => Some(operation.clone()),
                Osc52Effect::Terminal(_) | Osc52Effect::Rejected(_) => None,
            })
            .collect()
    }

    #[test]
    fn osc52_debug_exposes_only_operation_metadata_and_filter_state() {
        let operation = Osc52Operation::Write {
            target: Osc52Target::Standard,
            text: "private clipboard content".to_owned(),
        };
        assert_eq!(
            format!("{operation:?}"),
            "Osc52Operation { access: Write, target: Standard, byte_len: 25, .. }",
        );
        assert_eq!(
            format!("{:?}", Osc52Effect::Operation(operation)),
            "Operation(Osc52Operation { access: Write, target: Standard, byte_len: 25, .. })",
        );
        assert_eq!(
            format!(
                "{:?}",
                Osc52Effect::Terminal(b"private terminal content".to_vec())
            ),
            "Terminal { .. }",
        );

        let mut filter = Osc52Filter::default();
        let _ = filter.feed(b"\x1b]52;c;cHJpdmF0ZSBjbGlwYm9hcmQgY29udGVudA==");
        assert_eq!(
            format!("{filter:?}"),
            "Osc52Filter { state: Osc52 { escape_pending: false }, .. }",
        );
    }

    #[test]
    fn fragmented_reads_and_writes_preserve_target_and_terminator() {
        let mut filter = Osc52Filter::default();
        assert!(operations(&filter.feed(b"before\x1b]52;s;")).is_empty());
        let effects = filter.feed(b"aGVsbG8=\x1b\\after\x1b]52;p;?\x07");

        assert_eq!(
            operations(&effects),
            [
                Osc52Operation::Write {
                    target: Osc52Target::Selection,
                    text: "hello".to_owned(),
                },
                Osc52Operation::Read {
                    target: Osc52Target::Primary,
                    terminator: Osc52Terminator::Bell,
                },
            ]
        );
    }

    #[test]
    fn malformed_base64_utf8_and_targets_are_rejected() {
        let mut filter = Osc52Filter::default();
        let effects = filter.feed(b"\x1b]52;c;abc\x07\x1b]52;c;/w==\x07\x1b]52;x;aGVsbG8=\x07");
        let rejections = effects
            .iter()
            .filter_map(|effect| match effect {
                Osc52Effect::Rejected(rejection) => Some(*rejection),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rejections,
            [
                Osc52Rejection::InvalidBase64,
                Osc52Rejection::InvalidUtf8,
                Osc52Rejection::UnsupportedTarget,
            ]
        );
    }

    #[test]
    fn oversized_stream_is_discarded_boundedly_and_parsing_resumes_after_terminator() {
        let mut filter = Osc52Filter::default();
        let mut oversized = Vec::from(OSC52_PREFIX);
        oversized.extend_from_slice(b"c;");
        oversized.extend(std::iter::repeat_n(b'A', MAX_OSC52_ENCODED_BYTES + 32));

        let before_terminator = filter.feed(&oversized);
        assert!(before_terminator.is_empty());
        assert!(filter.candidate.is_empty());

        let after = filter.feed(b"\x07visible");
        assert!(matches!(
            after.as_slice(),
            [
                Osc52Effect::Rejected(Osc52Rejection::Oversized),
                Osc52Effect::Terminal(bytes)
            ] if bytes == b"visible"
        ));
    }
}
