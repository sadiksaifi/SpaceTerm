//! Permission Request filtering: a program in a Terminal Session asks SpaceTerm to set up
//! computer-use permissions with `OSC 7701 ; permissions=<list> ST`.
//!
//! `<list>` is a comma-separated set of `screen-recording` and `accessibility`. The filter removes
//! every Permission Request from the output before the Terminal Emulator sees it. A request only
//! offers a Permission Setup; the person decides whether to start one.

use std::mem;

use crate::platform::computer_use_access::ComputerUsePermission;

const PREFIX: &[u8] = b"\x1b]7701;";
/// The longest body worth parsing. Longer requests are discarded unread.
const MAX_BODY_BYTES: usize = 256;

/// The permissions one Permission Request asks for, in the order they were named.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PermissionRequest {
    permissions: Vec<ComputerUsePermission>,
}

impl PermissionRequest {
    pub(crate) fn permissions(&self) -> &[ComputerUsePermission] {
        &self.permissions
    }

    #[cfg(test)]
    pub(crate) fn for_test(permissions: &[ComputerUsePermission]) -> Self {
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
    /// `candidate` holds the body read so far.
    Body { escape_pending: bool },
    /// The body outgrew `MAX_BODY_BYTES`; the rest of the request is dropped.
    Discard { escape_pending: bool },
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

impl PermissionRequestFilter {
    /// Passes every byte that is not part of a Permission Request to `terminal`, in order, and
    /// each well-formed request to `request` at its position in the output.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        mut terminal: impl FnMut(&[u8]),
        mut request: impl FnMut(PermissionRequest),
    ) {
        let mut passed = 0;
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            let filtering = !matches!(self.state, FilterState::Ground);
            match self.state {
                FilterState::Ground => {
                    if byte == 0x1b {
                        if passed < index {
                            terminal(&bytes[passed..index]);
                        }
                        self.candidate.push(byte);
                        self.state = FilterState::Prefix;
                    }
                }
                FilterState::Prefix => {
                    self.candidate.push(byte);
                    if !PREFIX.starts_with(&self.candidate) {
                        // Not a Permission Request. Return everything before this byte and read
                        // the byte again from Ground, since it may begin a request itself.
                        self.candidate.pop();
                        terminal(&mem::take(&mut self.candidate));
                        self.state = FilterState::Ground;
                        passed = index;
                        continue;
                    }
                    if self.candidate.len() == PREFIX.len() {
                        self.candidate.clear();
                        self.state = FilterState::Body {
                            escape_pending: false,
                        };
                    }
                }
                FilterState::Body { escape_pending } => {
                    if matches!(byte, 0x18 | 0x1a) {
                        self.candidate.clear();
                        self.state = FilterState::Ground;
                    } else if byte == 0x07 || (escape_pending && byte == b'\\') {
                        let mut body = mem::take(&mut self.candidate);
                        if escape_pending {
                            body.pop();
                        }
                        if let Some(parsed) = parse(&body) {
                            request(parsed);
                        }
                        self.state = FilterState::Ground;
                    } else if self.candidate.len() > MAX_BODY_BYTES {
                        self.candidate.clear();
                        self.state = FilterState::Discard {
                            escape_pending: byte == 0x1b,
                        };
                    } else {
                        self.candidate.push(byte);
                        self.state = FilterState::Body {
                            escape_pending: byte == 0x1b,
                        };
                    }
                }
                FilterState::Discard { escape_pending } => {
                    self.state = if byte == 0x07
                        || (escape_pending && byte == b'\\')
                        || matches!(byte, 0x18 | 0x1a)
                    {
                        FilterState::Ground
                    } else {
                        FilterState::Discard {
                            escape_pending: byte == 0x1b,
                        }
                    };
                }
            }
            index += 1;
            // A byte read while filtering, or one that starts filtering, is never terminal output.
            if filtering || !matches!(self.state, FilterState::Ground) {
                passed = index;
            }
        }
        if matches!(self.state, FilterState::Ground) && passed < bytes.len() {
            terminal(&bytes[passed..]);
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
            b"screen-recording" => ComputerUsePermission::ScreenRecording,
            b"accessibility" => ComputerUsePermission::Accessibility,
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
    use ComputerUsePermission::{Accessibility, ScreenRecording};

    #[derive(Debug, PartialEq)]
    enum Output {
        Terminal(Vec<u8>),
        Request(Vec<ComputerUsePermission>),
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
                terminal(b"before"),
                Output::Request(vec![ScreenRecording, Accessibility]),
                terminal(b"after"),
            ]
        );
    }

    #[test]
    fn a_request_may_end_with_string_terminator() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=accessibility\x1b\\"]),
            [Output::Request(vec![Accessibility])]
        );
    }

    #[test]
    fn a_request_split_at_every_byte_is_still_recognized() {
        let raw = b"a\x1b]7701;permissions=screen-recording\x1b\\b";
        let chunks: Vec<&[u8]> = raw.chunks(1).collect();
        assert_eq!(
            filter(&chunks),
            [
                terminal(b"a"),
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
            [terminal(b"\x1b]77"), Output::Request(vec![Accessibility])]
        );
    }

    #[test]
    fn unsupported_names_are_ignored_and_duplicates_collapse() {
        assert_eq!(
            filter(&[b"\x1b]7701;permissions=camera,accessibility,accessibility\x07"]),
            [Output::Request(vec![Accessibility])]
        );
    }

    #[test]
    fn requests_without_a_supported_permission_are_removed_silently() {
        assert_eq!(
            filter(&[b"x\x1b]7701;permissions=camera\x07\x1b]7701;other\x07y"]),
            [terminal(b"xy")]
        );
    }

    #[test]
    fn a_cancelled_request_is_removed() {
        assert_eq!(
            filter(&[b"x\x1b]7701;permissions=accessibility\x18y"]),
            [terminal(b"xy")]
        );
    }

    #[test]
    fn an_oversized_request_is_discarded_through_its_terminator() {
        let mut raw = b"x\x1b]7701;permissions=".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_BODY_BYTES * 2));
        raw.extend_from_slice(b",accessibility\x1b\\y");
        assert_eq!(filter(&[&raw]), [terminal(b"xy")]);
    }

    #[test]
    fn a_body_at_the_limit_is_parsed() {
        let mut body = b"permissions=accessibility,".to_vec();
        body.resize(MAX_BODY_BYTES, b'a');
        let mut raw = PREFIX.to_vec();
        raw.extend_from_slice(&body);
        raw.push(0x07);
        assert_eq!(filter(&[&raw]), [Output::Request(vec![Accessibility])]);
    }
}
