//! Permission Request filtering: a program in a Terminal Session asks SpaceTerm to set up System
//! Permissions with `OSC 7701 ; permissions=<list> ST`.
//!
//! `<list>` is a comma-separated set of `screen-recording` and `accessibility`. The filter removes
//! every Permission Request from the output before the Terminal Emulator sees it. A request only
//! offers a Permission Setup; the person decides whether to start one.

use std::mem;

use crate::platform::permission_access::SystemPermission;

const PREFIX: &[u8] = b"\x1b]7701;";
/// The longest body worth parsing. Longer requests are discarded unread.
const MAX_BODY_BYTES: usize = 256;

/// The permissions one Permission Request asks for, in the order they were named.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PermissionRequest {
    permissions: Vec<SystemPermission>,
}

impl PermissionRequest {
    pub(crate) fn permissions(&self) -> &[SystemPermission] {
        &self.permissions
    }

    /// Adds the permissions `other` names that this request does not, after its own.
    pub(crate) fn merge(&mut self, other: PermissionRequest) {
        for permission in other.permissions {
            if !self.permissions.contains(&permission) {
                self.permissions.push(permission);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(permissions: &[SystemPermission]) -> Self {
        Self {
            permissions: permissions.to_vec(),
        }
    }
}

#[derive(Debug)]
enum FilterState {
    Ground,
    /// `candidate` holds a prefix of `PREFIX`.
    Prefix,
    /// `candidate` holds the body read so far. `escape_pending` follows an ESC that either ends
    /// the request as part of ST or abandons it.
    Body {
        escape_pending: bool,
    },
    /// The body outgrew `MAX_BODY_BYTES`; the rest of the request is dropped.
    Discard {
        escape_pending: bool,
    },
}

/// Removes Permission Requests from terminal output that may arrive split across reads.
#[derive(Debug)]
pub(crate) struct PermissionRequestFilter {
    state: FilterState,
    candidate: Vec<u8>,
}

impl Default for PermissionRequestFilter {
    fn default() -> Self {
        Self {
            state: FilterState::Ground,
            candidate: Vec::new(),
        }
    }
}

/// What the terminal receives in place of a removed request. A request begins with ESC, which
/// ends any string or sequence the terminal was reading; ST ends it the same way and is otherwise
/// ignored, so removing the request leaves the terminal's reading of the output around it intact.
const REMOVED_REQUEST: &[u8] = b"\x1b\\";

impl PermissionRequestFilter {
    /// Passes every byte that is not part of a Permission Request to `terminal`, in order, and
    /// each well-formed request to `request` at its position in the output.
    ///
    /// Output between requests reaches `terminal` in one call, except for a prefix split across
    /// reads.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&[u8]),
        mut request: impl FnMut(PermissionRequest),
    ) {
        // Terminal output from `passed` up to the current byte has not been passed on yet.
        let mut passed = 0;
        // Where the prefix in `candidate` starts, when it starts in these bytes rather than in an
        // earlier read.
        let mut prefix_start = None;
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            match self.state {
                FilterState::Ground => {
                    if byte == 0x1b {
                        self.candidate.push(byte);
                        self.state = FilterState::Prefix;
                        prefix_start = Some(index);
                    }
                }
                FilterState::Prefix => {
                    self.candidate.push(byte);
                    if !PREFIX.starts_with(&self.candidate) {
                        // Not a Permission Request, so its bytes are terminal output. The last
                        // byte is read again from Ground, since it may begin a request itself.
                        self.candidate.pop();
                        if prefix_start.is_none() {
                            // The prefix began in an earlier read, which passed on everything
                            // before it.
                            terminal(&self.candidate);
                            passed = index;
                        }
                        self.candidate.clear();
                        self.state = FilterState::Ground;
                        continue;
                    }
                    if self.candidate.len() == PREFIX.len() {
                        if let Some(start) = prefix_start.take()
                            && passed < start
                        {
                            terminal(&bytes[passed..start]);
                        }
                        self.candidate.clear();
                        self.state = FilterState::Body {
                            escape_pending: false,
                        };
                        passed = index + 1;
                    }
                }
                FilterState::Body { escape_pending } | FilterState::Discard { escape_pending } => {
                    let discarding = matches!(self.state, FilterState::Discard { .. });
                    if escape_pending && byte != b'\\' {
                        // The ESC begins another sequence and ends the request unterminated. A
                        // terminal would dispatch the string it holds; the filter discards the
                        // request so a truncated one starts nothing. Read the ESC and this byte
                        // again as output.
                        self.candidate.clear();
                        self.candidate.push(0x1b);
                        self.state = FilterState::Prefix;
                        if index > 0 {
                            passed = index - 1;
                            prefix_start = Some(index - 1);
                        }
                        continue;
                    }
                    if escape_pending || matches!(byte, 0x07 | 0x18 | 0x1a) {
                        // ST or BEL ends the request; CAN or SUB cancels it.
                        let body = mem::take(&mut self.candidate);
                        self.state = FilterState::Ground;
                        terminal(REMOVED_REQUEST);
                        if !discarding
                            && !matches!(byte, 0x18 | 0x1a)
                            && let Some(parsed) = parse(&body)
                        {
                            request(parsed);
                        }
                    } else if byte == 0x1b {
                        self.state = if discarding {
                            FilterState::Discard {
                                escape_pending: true,
                            }
                        } else {
                            FilterState::Body {
                                escape_pending: true,
                            }
                        };
                    } else if !discarding {
                        if self.candidate.len() == MAX_BODY_BYTES {
                            self.candidate.clear();
                            self.state = FilterState::Discard {
                                escape_pending: false,
                            };
                        } else {
                            self.candidate.push(byte);
                        }
                    }
                    passed = index + 1;
                }
            }
            index += 1;
        }
        let end = match self.state {
            FilterState::Ground => bytes.len(),
            // The prefix waits in `candidate` for the next read.
            FilterState::Prefix => prefix_start.unwrap_or(passed),
            FilterState::Body { .. } | FilterState::Discard { .. } => passed,
        };
        if passed < end {
            terminal(&bytes[passed..end]);
        }
    }
}

/// Reads `permissions=<list>`. Unknown permission names are ignored, so a newer tool can ask an
/// older SpaceTerm for what it supports; a request that names nothing supported is dropped.
fn parse(body: &[u8]) -> Option<PermissionRequest> {
    let list = body.strip_prefix(b"permissions=")?;
    let mut permissions = Vec::new();
    for name in list.split(|byte| *byte == b',') {
        let permission = match name {
            b"screen-recording" => SystemPermission::ScreenRecording,
            b"accessibility" => SystemPermission::Accessibility,
            _ => continue,
        };
        if !permissions.contains(&permission) {
            permissions.push(permission);
        }
    }
    (!permissions.is_empty()).then_some(PermissionRequest { permissions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use SystemPermission::{Accessibility, ScreenRecording};

    #[derive(Debug, PartialEq)]
    enum Output {
        Terminal(Vec<u8>),
        Request(Vec<SystemPermission>),
    }

    /// Feeds `chunks` in order and merges adjacent terminal output.
    fn filter(chunks: &[&[u8]]) -> Vec<Output> {
        let mut filter = PermissionRequestFilter::default();
        let output = std::cell::RefCell::new(Vec::new());
        for chunk in chunks {
            filter.feed(
                chunk,
                |bytes| {
                    let mut output = output.borrow_mut();
                    if let Some(Output::Terminal(previous)) = output.last_mut() {
                        previous.extend_from_slice(bytes);
                    } else {
                        output.push(Output::Terminal(bytes.to_vec()));
                    }
                },
                |request| {
                    output
                        .borrow_mut()
                        .push(Output::Request(request.permissions().to_vec()));
                },
            );
        }
        output.into_inner()
    }

    fn terminal(bytes: &[u8]) -> Output {
        Output::Terminal(bytes.to_vec())
    }

    #[test]
    fn plain_output_passes_through() {
        assert_eq!(filter(&[b"hello", b" world"]), [terminal(b"hello world")]);
    }

    #[test]
    fn a_request_is_removed_and_reported_in_place() {
        assert_eq!(
            filter(&[b"before\x1b]7701;permissions=screen-recording,accessibility\x07after"]),
            [
                terminal(b"before\x1b\\"),
                Output::Request(vec![ScreenRecording, Accessibility]),
                terminal(b"after"),
            ]
        );
    }

    #[test]
    fn a_request_may_end_with_string_terminator() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=accessibility\x1b\\"]),
            [
                terminal(REMOVED_REQUEST),
                Output::Request(vec![Accessibility])
            ]
        );
    }

    #[test]
    fn a_request_split_at_every_byte_is_still_recognized() {
        let raw = b"a\x1b]7701;permissions=screen-recording\x1b\\b";
        let chunks: Vec<&[u8]> = raw.chunks(1).collect();
        assert_eq!(
            filter(&chunks),
            [
                terminal(b"a\x1b\\"),
                Output::Request(vec![ScreenRecording]),
                terminal(b"b"),
            ]
        );
    }

    #[test]
    fn other_sequences_pass_through_unchanged() {
        let raw: &[u8] = b"\x1b[31mred\x1b]7;file://host/tmp\x07\x1b]77;x\x07\x1b]7701x";
        assert_eq!(filter(&[raw]), [terminal(raw)]);
    }

    #[test]
    fn an_escape_that_breaks_a_prefix_may_begin_a_request() {
        assert_eq!(
            filter(&[b"\x1b]77\x1b]7701;permissions=accessibility\x07"]),
            [
                terminal(b"\x1b]77\x1b\\"),
                Output::Request(vec![Accessibility])
            ]
        );
    }

    #[test]
    fn unsupported_names_are_ignored_and_duplicates_collapse() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=camera,accessibility,accessibility\x07"]),
            [
                terminal(REMOVED_REQUEST),
                Output::Request(vec![Accessibility])
            ]
        );
    }

    #[test]
    fn requests_without_a_supported_permission_are_removed_silently() {
        assert_eq!(
            filter(&[b"x\x1b]7701;permissions=camera\x07\x1b]7701;other\x07y"]),
            [terminal(b"x\x1b\\\x1b\\y")]
        );
    }

    #[test]
    fn a_cancelled_request_is_removed() {
        for cancellation in [0x18, 0x1a] {
            let raw = [
                b"x\x1b]7701;permissions=accessibility".as_slice(),
                &[cancellation],
                b"y",
            ]
            .concat();
            assert_eq!(filter(&[&raw]), [terminal(b"x\x1b\\y")]);
        }
    }

    #[test]
    fn an_oversized_request_is_discarded_through_its_terminator() {
        let mut raw = b"x\x1b]7701;permissions=".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_BODY_BYTES * 2));
        raw.extend_from_slice(b",accessibility\x1b\\y");
        assert_eq!(filter(&[&raw]), [terminal(b"x\x1b\\y")]);
    }

    #[test]
    fn a_body_at_the_limit_is_parsed() {
        let mut body = b"permissions=accessibility,".to_vec();
        body.resize(256, b'a');
        let mut raw = b"\x1b]7701;".to_vec();
        raw.extend_from_slice(&body);
        raw.push(0x07);
        assert_eq!(
            filter(&[&raw]),
            [terminal(b"\x1b\\"), Output::Request(vec![Accessibility])]
        );
    }

    #[test]
    fn a_body_past_the_limit_is_discarded() {
        let mut body = b"permissions=accessibility,".to_vec();
        body.resize(257, b'a');
        let mut raw = b"\x1b]7701;".to_vec();
        raw.extend_from_slice(&body);
        raw.push(0x07);
        assert_eq!(filter(&[&raw]), [terminal(b"\x1b\\")]);
    }

    /// An unterminated request must not hide the output after it: the next ESC abandons it, as it
    /// would any string in the terminal.
    #[test]
    fn an_escape_abandons_an_unterminated_request() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=acc", b"\x1b[32mprompt$ ", b"ls\r\n"]),
            [terminal(b"\x1b[32mprompt$ ls\r\n")]
        );
    }

    #[test]
    fn an_escape_split_from_its_sequence_abandons_the_request() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=accessibility\x1b", b"[0mok"]),
            [terminal(b"\x1b[0mok")]
        );
    }

    #[test]
    fn an_escape_abandons_an_oversized_request() {
        let mut raw = b"\x1b]7701;".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_BODY_BYTES * 2));
        raw.extend_from_slice(b"\x1b[1mbold");
        assert_eq!(filter(&[&raw]), [terminal(b"\x1b[1mbold")]);
    }

    #[test]
    fn an_escape_that_abandons_a_request_may_begin_another() {
        assert_eq!(
            filter(&[b"\x1b]7701;x\x1b]7701;permissions=accessibility\x07"]),
            [
                terminal(REMOVED_REQUEST),
                Output::Request(vec![Accessibility])
            ]
        );
    }

    /// A string the terminal was reading when a request began ends where the request began, as it
    /// would if the request were left in place.
    #[test]
    fn a_removed_request_ends_an_open_string() {
        assert_eq!(
            filter(&[
                b"\x1b]0;title",
                b"\x1b]7701;permissions=accessibility\x07 hello"
            ]),
            [
                terminal(b"\x1b]0;title\x1b\\"),
                Output::Request(vec![Accessibility]),
                terminal(b" hello"),
            ]
        );
    }

    #[test]
    fn unrelated_output_block_reaches_terminal_in_one_call() {
        let raw = b"\x1b[31mred\x1b[0m \x1b]7;file://host/tmp\x07\x1b[1mbold\x1b[0m".repeat(50);
        let mut filter = PermissionRequestFilter::default();
        let mut calls = 0;
        let mut output = Vec::new();
        filter.feed(
            &raw,
            |bytes| {
                calls += 1;
                output.extend_from_slice(bytes);
            },
            |_| {},
        );
        assert_eq!(calls, 1);
        assert_eq!(output, raw);
    }
}
