//! Wire invariants that depend on more than one field. No domain authority is inferred.

use crate::{
    ProtocolError,
    codec::Payload,
    limits,
    wire::{v1, v2},
};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn validate(value: &Payload) -> Result<(), ProtocolError> {
    macro_rules! profile {
        ($message:expr, $profile:ident) => {{
            use $profile::message::Kind;
            match &$message.kind {
                Some(Kind::Outcome(outcome)) => {
                    if let Some($profile::outcome::Result::Conflict(conflict)) = &outcome.result {
                        conflict_value!($profile, conflict)?;
                    }
                }
                Some(Kind::OperationLookupResult(result)) => {
                    if let Some($profile::operation_lookup_result::Disposition::Completed(outcome)) = &result.disposition
                        && let Some($profile::outcome::Result::Conflict(conflict)) = &outcome.result {
                        conflict_value!($profile, conflict)?;
                    }
                }
                Some(Kind::SemanticPage(page)) => {
                    let mut rows = BTreeSet::new();
                    for row in &page.rows {
                        if !rows.insert(row.id) {
                            return Err(ProtocolError::Malformed);
                        }
                        row_cells!($profile, row)?;
                    }
                }
                Some(Kind::RuntimeCommand(command)) => {
                    if let Some($profile::runtime_command::Action::Key(key)) = &command.action {
                        if char::from_u32(key.unshifted_codepoint).is_none()
                            || (key.code == 0 && key.text.as_deref().is_none_or(str::is_empty))
                        {
                            return Err(ProtocolError::Malformed);
                        }
                    }
                }
                Some(Kind::ResourceChunk(chunk)) => {
                    let end = chunk
                        .offset
                        .checked_add(chunk.data.len() as u64)
                        .ok_or(ProtocolError::LimitExceeded)?;
                    if end > chunk.total_bytes
                        || (chunk.complete
                            && (end != chunk.total_bytes || chunk.digest.len() != 32))
                        || chunk
                            .resource
                            .as_ref()
                            .and_then(|resource| resource.session.as_ref())
                            != chunk
                                .fence
                                .as_ref()
                                .and_then(|fence| fence.session.as_ref())
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    if matches!(chunk.format, 1 | 2)
                        && chunk.total_bytes > limits::MAX_GRAPHICS_TRANSMISSION_BYTES as u64
                    {
                        return Err(ProtocolError::LimitExceeded);
                    }
                    if chunk.format == 1 {
                        let expected = u64::from(chunk.width)
                            .checked_mul(u64::from(chunk.height))
                            .and_then(|pixels| pixels.checked_mul(4))
                            .ok_or(ProtocolError::LimitExceeded)?;
                        if chunk.width == 0 || chunk.height == 0 || expected != chunk.total_bytes {
                            return Err(ProtocolError::Malformed);
                        }
                    }
                }
                Some(Kind::NativeRequest(request)) => match &request.action {
                    Some($profile::native_request::Action::Clipboard(clipboard))
                        if clipboard.read
                            && (clipboard.deadline_ms == 0
                                || clipboard.deadline_ms
                                    > limits::OSC52_READ_DEADLINE_MILLIS as u32) =>
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    Some($profile::native_request::Action::UnsafePaste(paste))
                        if paste.expires_after_ms == 0
                            || paste.expires_after_ms
                                > (limits::PASTE_EXPIRY_SECONDS * 1000) as u32 =>
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    _ => {}
                },
                _ => {}
            }
            Ok(())
        }};
    }
    macro_rules! conflict_value {
        ($profile:ident, $value:expr) => {{
            // Keep every frozen oneof value readable. Additive values without a
            // oneof member must select exactly one canonical alternative.
            if $value.current_value.is_none()
                && usize::from($value.directory_value.is_some())
                    + usize::from($value.remote_access.is_some())
                    != 1
            {
                Err(ProtocolError::Malformed)
            } else {
                Ok(())
            }
        }};
    }
    macro_rules! row_cells {
        (v1, $row:expr) => {
            validate_cells(
                $row.cells.iter().map(|cell| {
                    (
                        cell.grapheme.as_str(),
                        cell.width,
                        cell.spacer,
                        cell.wrap_head,
                    )
                }),
                $row.soft_wrap,
            )
        };
        (v2, $row:expr) => {
            validate_cells(
                $row.runs
                    .iter()
                    .flat_map(|run| run.graphemes.iter())
                    .map(|cell| (cell.text.as_str(), cell.width, cell.spacer, cell.wrap_head)),
                $row.soft_wrap,
            )
        };
    }
    match value {
        Payload::Major1(value) => profile!(value, v1),
        Payload::Major2(value) => profile!(value, v2),
    }
}

fn validate_cells<'a>(
    cells: impl Iterator<Item = (&'a str, u32, bool, bool)>,
    soft_wrap: bool,
) -> Result<(), ProtocolError> {
    let mut cells = cells.peekable();
    let mut pending_tail = false;
    while let Some((text, width, spacer, wrap_head)) = cells.next() {
        if spacer {
            if width != 0 || !text.is_empty() {
                return Err(ProtocolError::Malformed);
            }
            if wrap_head {
                if pending_tail || !soft_wrap || cells.peek().is_some() {
                    return Err(ProtocolError::Malformed);
                }
            } else if !pending_tail {
                return Err(ProtocolError::Malformed);
            }
            pending_tail = false;
        } else {
            if pending_tail
                || wrap_head
                || !(1..=2).contains(&width)
                || text.is_empty()
                || text.graphemes(true).nth(1).is_some()
            {
                return Err(ProtocolError::Malformed);
            }
            pending_tail = width == 2;
        }
    }
    if pending_tail {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}
