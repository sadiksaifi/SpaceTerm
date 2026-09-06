use std::time::{Duration, Instant};
use std::{fmt, mem};

const OSC52_AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const MAX_OSC52_CONTENT_BYTES: usize = 1024 * 1024;
const MAX_OSC52_ENCODED_BYTES: usize = MAX_OSC52_CONTENT_BYTES.div_ceil(3) * 4;
const OSC52_PREFIX: &[u8] = b"\x1b]52;";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Osc52ClipboardError {
    UnsupportedTarget,
    Unavailable,
}

pub(crate) trait Osc52Clipboard: Send {
    fn read(&mut self, target: Osc52Target) -> Result<String, Osc52ClipboardError>;
    fn write(&mut self, target: Osc52Target, text: &str) -> Result<(), Osc52ClipboardError>;
}

/// Constructs clipboard ownership on the Terminal Session worker.
///
/// Implementations perform synchronous operations so authorized clipboard replies retain
/// their order among emulator replies without waiting on the GPUI application thread.
pub(crate) trait Osc52ClipboardFactory: Send + Sync {
    fn create(&self) -> Box<dyn Osc52Clipboard>;
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct UnavailableOsc52Clipboard;

#[cfg(test)]
impl Osc52Clipboard for UnavailableOsc52Clipboard {
    fn read(&mut self, _target: Osc52Target) -> Result<String, Osc52ClipboardError> {
        Err(Osc52ClipboardError::Unavailable)
    }

    fn write(&mut self, _target: Osc52Target, _text: &str) -> Result<(), Osc52ClipboardError> {
        Err(Osc52ClipboardError::Unavailable)
    }
}

#[cfg(test)]
pub(crate) struct UnavailableOsc52ClipboardFactory;

#[cfg(test)]
impl Osc52ClipboardFactory for UnavailableOsc52ClipboardFactory {
    fn create(&self) -> Box<dyn Osc52Clipboard> {
        Box::new(UnavailableOsc52Clipboard)
    }
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

    pub(in crate::terminal) const fn from_counter(value: u64) -> Self {
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
    pub(in crate::terminal) const fn for_access(self, access: Osc52Access) -> Osc52AccessPolicy {
        match access {
            Osc52Access::Read => self.read,
            Osc52Access::Write => self.write,
        }
    }
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

    pub(in crate::terminal) fn read_reply(&self, text: &str) -> Option<Vec<u8>> {
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
}

pub(in crate::terminal) struct Osc52Filter {
    state: FilterState,
    candidate: Vec<u8>,
}

impl fmt::Debug for Osc52Filter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Osc52Filter")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
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
    pub(in crate::terminal) fn feed(&mut self, bytes: &[u8]) -> Vec<Osc52Effect> {
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

struct PendingOsc52Authorization {
    id: Osc52AuthorizationId,
    operation: Osc52Operation,
    deadline: Instant,
}

#[derive(Default)]
pub(in crate::terminal) struct Osc52AuthorizationSchedule {
    next_id: u64,
    pending: Option<PendingOsc52Authorization>,
}

impl Osc52AuthorizationSchedule {
    pub(in crate::terminal) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    pub(in crate::terminal) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(in crate::terminal) fn create(
        &mut self,
        operation: Osc52Operation,
        now: Instant,
    ) -> Option<Osc52AuthorizationRequest> {
        if self.pending.is_some() {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = Osc52AuthorizationId::from_counter(self.next_id);
        let request = Osc52AuthorizationRequest {
            id,
            access: operation.access(),
            target: operation.target(),
            byte_len: operation.byte_len(),
        };
        self.pending = Some(PendingOsc52Authorization {
            id,
            operation,
            deadline: now + OSC52_AUTHORIZATION_TIMEOUT,
        });
        Some(request)
    }

    pub(in crate::terminal) fn take(
        &mut self,
        id: Osc52AuthorizationId,
        now: Instant,
    ) -> Option<Osc52Operation> {
        let pending = self.pending.as_ref()?;
        if pending.id != id || now >= pending.deadline {
            return None;
        }
        self.pending.take().map(|pending| pending.operation)
    }

    pub(in crate::terminal) fn expire(&mut self, now: Instant) -> Option<Osc52AuthorizationId> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.pending.take().map(|pending| pending.id)
        } else {
            None
        }
    }

    pub(in crate::terminal) fn cancel(&mut self) {
        self.pending = None;
    }
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
    fn osc52_authorization_allows_one_pending_request_and_rejects_stale_ids() {
        let now = Instant::now();
        let mut schedule = Osc52AuthorizationSchedule::default();
        let operation = Osc52Operation::Read {
            target: Osc52Target::Standard,
            terminator: Osc52Terminator::StringTerminator,
        };
        let request = schedule.create(operation.clone(), now).unwrap();

        assert!(schedule.create(operation, now).is_none());
        assert_eq!(schedule.take(Osc52AuthorizationId::new(999), now), None);
        assert!(schedule.is_pending());
        assert_eq!(
            schedule.expire(now + OSC52_AUTHORIZATION_TIMEOUT),
            Some(request.id)
        );
        assert!(!schedule.is_pending());
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
