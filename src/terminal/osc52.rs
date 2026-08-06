use std::mem;

pub(crate) const MAX_OSC52_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_OSC52_ENCODED_BYTES: usize = MAX_OSC52_CONTENT_BYTES.div_ceil(3) * 4;
const OSC52_PREFIX: &[u8] = b"\x1b]52;";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52ClipboardError {
    #[cfg_attr(
        test,
        expect(dead_code, reason = "native adapter owns unsupported target result")
    )]
    UnsupportedTarget,
    Unavailable,
}

pub(crate) trait Osc52Clipboard: Send {
    fn read(&mut self, target: Osc52Target) -> Result<String, Osc52ClipboardError>;
    fn write(&mut self, target: Osc52Target, text: &str) -> Result<(), Osc52ClipboardError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52Access {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52Target {
    Standard,
    Selection,
    Primary,
}

impl Osc52Target {
    const fn selector(self) -> u8 {
        match self {
            Self::Standard => b'c',
            Self::Selection => b's',
            Self::Primary => b'p',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Osc52AuthorizationId(u64);

impl Osc52AuthorizationId {
    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) const fn from_counter(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Osc52AuthorizationRequest {
    pub(crate) id: Osc52AuthorizationId,
    pub(crate) access: Osc52Access,
    pub(crate) target: Osc52Target,
    pub(crate) byte_len: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52AuthorizationDecision {
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "policy remains deny-by-default until configuration selects ask or allow"
    )
)]
pub(crate) enum Osc52AccessPolicy {
    Deny,
    Ask,
    Allow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Osc52AuthorizationPolicy {
    pub(crate) read: Osc52AccessPolicy,
    pub(crate) write: Osc52AccessPolicy,
}

impl Default for Osc52AuthorizationPolicy {
    fn default() -> Self {
        Self {
            read: Osc52AccessPolicy::Deny,
            write: Osc52AccessPolicy::Deny,
        }
    }
}

impl Osc52AuthorizationPolicy {
    pub(super) const fn for_access(self, access: Osc52Access) -> Osc52AccessPolicy {
        match access {
            Osc52Access::Read => self.read,
            Osc52Access::Write => self.write,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Osc52Terminator {
    Bell,
    StringTerminator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Osc52Operation {
    Read {
        target: Osc52Target,
        terminator: Osc52Terminator,
    },
    Write {
        target: Osc52Target,
        text: String,
    },
}

impl Osc52Operation {
    pub(super) const fn access(&self) -> Osc52Access {
        match self {
            Self::Read { .. } => Osc52Access::Read,
            Self::Write { .. } => Osc52Access::Write,
        }
    }

    pub(super) const fn target(&self) -> Osc52Target {
        match self {
            Self::Read { target, .. } | Self::Write { target, .. } => *target,
        }
    }

    pub(super) fn byte_len(&self) -> usize {
        match self {
            Self::Read { .. } => 0,
            Self::Write { text, .. } => text.len(),
        }
    }

    pub(super) fn read_reply(&self, text: &str) -> Option<Vec<u8>> {
        let Self::Read { target, terminator } = self else {
            return None;
        };
        if text.len() > MAX_OSC52_CONTENT_BYTES {
            return None;
        }
        let encoded = encode_base64(text.as_bytes());
        let mut reply = Vec::with_capacity(encoded.len() + 10);
        reply.extend_from_slice(b"\x1b]52;");
        reply.push(target.selector());
        reply.push(b';');
        reply.extend_from_slice(&encoded);
        match terminator {
            Osc52Terminator::Bell => reply.push(0x07),
            Osc52Terminator::StringTerminator => reply.extend_from_slice(b"\x1b\\"),
        }
        Some(reply)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Osc52Rejection {
    Malformed,
    Oversized,
    UnsupportedTarget,
    InvalidBase64,
    InvalidUtf8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Osc52Effect {
    Terminal(Vec<u8>),
    Operation(Osc52Operation),
    Rejected(Osc52Rejection),
}

#[derive(Debug)]
enum FilterState {
    Ground,
    Prefix,
    Osc52 { escape_pending: bool },
    DiscardOversized { escape_pending: bool },
}

#[derive(Debug)]
pub(super) struct Osc52Filter {
    state: FilterState,
    candidate: Vec<u8>,
}

impl Default for Osc52Filter {
    fn default() -> Self {
        Self {
            state: FilterState::Ground,
            candidate: Vec::new(),
        }
    }
}

impl Osc52Filter {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<Osc52Effect> {
        let mut effects = Vec::new();
        let mut terminal = Vec::with_capacity(bytes.len());

        for &byte in bytes {
            match self.state {
                FilterState::Ground => {
                    if byte == 0x1b {
                        flush_terminal(&mut effects, &mut terminal);
                        self.candidate.push(byte);
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
                        terminal.extend(mem::take(&mut self.candidate));
                        self.state = FilterState::Ground;
                    }
                }
                FilterState::Osc52 { mut escape_pending } => {
                    self.candidate.push(byte);
                    let complete = byte == 0x07 || (escape_pending && byte == b'\\');
                    escape_pending = byte == 0x1b;
                    if complete {
                        let raw = mem::take(&mut self.candidate);
                        effects.push(Osc52Effect::Terminal(raw.clone()));
                        effects.push(match parse_osc52(&raw) {
                            Ok(operation) => Osc52Effect::Operation(operation),
                            Err(rejection) => Osc52Effect::Rejected(rejection),
                        });
                        self.state = FilterState::Ground;
                    } else if self.candidate.len() > MAX_OSC52_ENCODED_BYTES + 16 {
                        self.candidate.clear();
                        self.state = FilterState::DiscardOversized { escape_pending };
                    } else {
                        self.state = FilterState::Osc52 { escape_pending };
                    }
                }
                FilterState::DiscardOversized { mut escape_pending } => {
                    let complete = byte == 0x07 || (escape_pending && byte == b'\\');
                    escape_pending = byte == 0x1b;
                    if complete {
                        effects.push(Osc52Effect::Rejected(Osc52Rejection::Oversized));
                        self.state = FilterState::Ground;
                    } else {
                        self.state = FilterState::DiscardOversized { escape_pending };
                    }
                }
            }
        }

        flush_terminal(&mut effects, &mut terminal);
        effects
    }
}

fn flush_terminal(effects: &mut Vec<Osc52Effect>, terminal: &mut Vec<u8>) {
    if !terminal.is_empty() {
        effects.push(Osc52Effect::Terminal(mem::take(terminal)));
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
        b"" | b"c" => Osc52Target::Standard,
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

fn decode_base64(input: &[u8]) -> Result<Vec<u8>, Osc52Rejection> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if !input.len().is_multiple_of(4) {
        return Err(Osc52Rejection::InvalidBase64);
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for (index, chunk) in input.chunks_exact(4).enumerate() {
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

fn encode_base64(input: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[usize::from(a >> 2)]);
        output.push(ALPHABET[usize::from((a & 0x03) << 4 | b >> 4)]);
        output.push(if chunk.len() > 1 {
            ALPHABET[usize::from((b & 0x0f) << 2 | c >> 6)]
        } else {
            b'='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[usize::from(c & 0x3f)]
        } else {
            b'='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn read_replies_are_bounded_base64_and_reuse_the_request_terminator() {
        let operation = Osc52Operation::Read {
            target: Osc52Target::Standard,
            terminator: Osc52Terminator::Bell,
        };
        assert_eq!(
            operation.read_reply("hello").unwrap(),
            b"\x1b]52;c;aGVsbG8=\x07"
        );
        assert!(
            operation
                .read_reply(&"x".repeat(MAX_OSC52_CONTENT_BYTES + 1))
                .is_none()
        );
    }

    #[test]
    fn authorization_is_deny_by_default_for_reads_and_writes() {
        assert_eq!(
            Osc52AuthorizationPolicy::default(),
            Osc52AuthorizationPolicy {
                read: Osc52AccessPolicy::Deny,
                write: Osc52AccessPolicy::Deny,
            }
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
