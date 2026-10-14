//! Checked, connection-scoped Copy, literal Find and topology reads.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadQuery {
    Topology(TopologyPageRequest),
    Copy(CopyRequest),
    Find(FindRequest),
    Cancel { stamp: QueryStamp, kind: i32 },
}
impl ReadQuery {
    pub fn stamp(&self) -> QueryStamp {
        match self {
            Self::Topology(request) => request.stamp(),
            Self::Copy(request) => request.stamp(),
            Self::Find(request) => request.stamp(),
            Self::Cancel { stamp, .. } => *stamp,
        }
    }
    pub fn kind(&self) -> i32 {
        match self {
            Self::Topology(_) => 3,
            Self::Find(_) => 4,
            Self::Copy(_) => 5,
            Self::Cancel { kind, .. } => *kind,
        }
    }
}
fn reason(value: ContentReadFailure) -> i32 {
    match value {
        ContentReadFailure::StaleAuthority => 1,
        ContentReadFailure::Cancelled => 2,
        ContentReadFailure::Retired => 3,
        ContentReadFailure::Capacity => 4,
        ContentReadFailure::MissingResource => 5,
        ContentReadFailure::InvalidRange => 6,
        ContentReadFailure::Exhausted => 7,
    }
}
macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::super::$p::*;
            use super::*;
            use crate::wire::$p as wire;

            pub fn encode_cell(cell: CellRef) -> wire::CellRef {
                wire::CellRef {
                    session: Some(encode_session(cell.session)),
                    screen_generation: cell.row.screen_generation,
                    row_revision: cell.row_revision,
                    column: u32::from(cell.column),
                    physical_row: Some(encode_row_id(cell.row)),
                    ..Default::default()
                }
            }
            pub fn decode_cell(value: &wire::CellRef) -> Result<CellRef, ProtocolError> {
                let row = decode_row_id(required(&value.physical_row)?)?;
                if value.row_id != 0 || value.screen_generation != row.screen_generation {
                    return Err(ProtocolError::Malformed);
                }
                Ok(CellRef {
                    session: decode_session(required(&value.session)?)?,
                    row,
                    row_revision: value.row_revision,
                    column: narrow(value.column)?,
                })
            }
            pub fn encode_topology_row(row: TopologyRow) -> wire::TopologyRow {
                wire::TopologyRow {
                    row: Some(encode_row_id(row.row())),
                    revision: row.revision(),
                    columns: u32::from(row.columns()),
                    soft_wrapped: row.soft_wrapped(),
                }
            }
            pub fn decode_topology_row(
                row: &wire::TopologyRow,
            ) -> Result<TopologyRow, ProtocolError> {
                TopologyRow::new(
                    decode_row_id(required(&row.row)?)?,
                    row.revision,
                    narrow(row.columns)?,
                    row.soft_wrapped,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn decode_query(value: &wire::ContentQuery) -> Result<ReadQuery, ProtocolError> {
                let stamp = decode_stamp(required(&value.stamp)?)?;
                if !matches!(value.kind, 3..=5)
                    || !value.continuation.is_empty()
                    || value.page_continuation.is_some()
                    || value.case_sensitive
                    || value.regex
                    || value
                        .fence
                        .as_ref()
                        .is_some_and(|fence| *fence != encode_fence(stamp.fence()))
                    || (value.expected_revision != 0
                        && value.expected_revision != stamp.base().revision.get())
                    || (value.request_generation != 0
                        && value.request_generation != stamp.generation().get())
                {
                    return Err(ProtocolError::Malformed);
                }
                if value.cancel {
                    if value.start.is_some()
                        || value.end.is_some()
                        || !value.query.is_empty()
                        || value.copy_options.is_some()
                        || value.find_continuation.is_some()
                        || value.topology_after.is_some()
                        || value.limits.is_some()
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    return Ok(ReadQuery::Cancel {
                        stamp,
                        kind: value.kind,
                    });
                }
                match value.kind {
                    3 if value.start.is_none()
                        && value.end.is_none()
                        && value.query.is_empty()
                        && value.copy_options.is_none()
                        && value.find_continuation.is_none() =>
                    {
                        let limits = required(&value.limits)?;
                        let limits =
                            PageLimits::new(narrow(limits.rows)?, limits.cells, limits.bytes)
                                .map_err(|_| ProtocolError::LimitExceeded)?;
                        let after = value
                            .topology_after
                            .as_ref()
                            .map(decode_topology_row)
                            .transpose()?;
                        let continuation = after
                            .map(|row| TopologyContinuation::new(stamp, row))
                            .transpose()
                            .map_err(|_| ProtocolError::Malformed)?;
                        Ok(ReadQuery::Topology(
                            TopologyPageRequest::new(stamp, continuation, limits)
                                .map_err(|_| ProtocolError::Malformed)?,
                        ))
                    }
                    4 if value.start.is_none()
                        && value.end.is_none()
                        && value.copy_options.is_none()
                        && value.topology_after.is_none()
                        && value.limits.is_none() =>
                    {
                        let query = BoundedFindQuery::new(value.query.clone())
                            .map_err(|_| ProtocolError::LimitExceeded)?;
                        let continuation = value
                            .find_continuation
                            .as_ref()
                            .map(|value| {
                                let continuation = FindContinuation::new(
                                    decode_stamp(required(&value.stamp)?)?,
                                    &query,
                                    decode_cell(required(&value.next)?)?,
                                )
                                .map_err(|_| ProtocolError::Malformed)?;
                                if value.query_fingerprint.as_slice()
                                    != continuation.query_fingerprint()
                                {
                                    return Err(ProtocolError::Malformed);
                                }
                                Ok(continuation)
                            })
                            .transpose()?;
                        Ok(ReadQuery::Find(
                            FindRequest::new(stamp, query, continuation)
                                .map_err(|_| ProtocolError::Malformed)?,
                        ))
                    }
                    5 if value.query.is_empty()
                        && value.find_continuation.is_none()
                        && value.topology_after.is_none()
                        && value.limits.is_none() =>
                    {
                        let options = required(&value.copy_options)?;
                        let selection = ContentSelection::new(
                            stamp,
                            decode_cell(required(&value.start)?)?,
                            decode_cell(required(&value.end)?)?,
                            options.rectangle,
                        )
                        .map_err(|_| ProtocolError::Malformed)?;
                        Ok(ReadQuery::Copy(
                            CopyRequest::new(
                                stamp,
                                selection,
                                CopyOptions {
                                    unwrap_soft_wraps: options.unwrap_soft_wraps,
                                    trim_trailing_spaces: options.trim_trailing_spaces,
                                    include_html: options.include_html,
                                },
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        ))
                    }
                    _ => Err(ProtocolError::Malformed),
                }
            }
            pub fn encode_query(query: &ReadQuery) -> wire::ContentQuery {
                let stamp = query.stamp();
                let mut value = wire::ContentQuery {
                    fence: Some(encode_fence(stamp.fence())),
                    kind: query.kind(),
                    stamp: Some(Box::new(encode_stamp(stamp))),
                    ..Default::default()
                };
                match query {
                    ReadQuery::Topology(request) => {
                        value.limits = Some(wire::PageLimits {
                            rows: u32::from(request.limits().rows()),
                            cells: request.limits().cells(),
                            bytes: request.limits().bytes(),
                        });
                        value.topology_after = request
                            .continuation()
                            .map(|value| encode_topology_row(value.after()));
                    }
                    ReadQuery::Copy(request) => {
                        value.start = Some(encode_cell(request.selection().start()));
                        value.end = Some(encode_cell(request.selection().end()));
                        value.copy_options = Some(wire::CopyOptions {
                            unwrap_soft_wraps: request.options().unwrap_soft_wraps,
                            trim_trailing_spaces: request.options().trim_trailing_spaces,
                            include_html: request.options().include_html,
                            rectangle: request.selection().rectangle(),
                        });
                    }
                    ReadQuery::Find(request) => {
                        value.query = request.query().as_str().to_owned();
                        value.find_continuation = request
                            .continuation()
                            .map(|value| Box::new(encode_find_continuation(value)));
                    }
                    ReadQuery::Cancel { .. } => value.cancel = true,
                }
                value
            }
            pub fn encode_failure(
                stamp: QueryStamp,
                failure: ContentReadFailure,
            ) -> wire::ContentFailure {
                wire::ContentFailure {
                    stamp: Some(encode_stamp(stamp)),
                    reason: reason(failure),
                }
            }
            pub fn encode_topology_result(value: &TopologyPageResult) -> wire::ContentReadResponse {
                wire::ContentReadResponse {
                    result: Some(match value {
                        ContentReadResult::Ready(page) => {
                            wire::content_read_response::Result::Topology(wire::TopologyPage {
                                stamp: Some(encode_stamp(page.stamp())),
                                rows: page
                                    .rows()
                                    .iter()
                                    .copied()
                                    .map(encode_topology_row)
                                    .collect(),
                                after: page
                                    .continuation()
                                    .map(|value| encode_topology_row(value.after())),
                            })
                        }
                        ContentReadResult::ResyncRequired { request, current } => {
                            wire::content_read_response::Result::ResyncRequired(
                                wire::ContentResyncRequired {
                                    stamp: Some(encode_stamp(*request)),
                                    current: Some(encode_base(*current)),
                                },
                            )
                        }
                        ContentReadResult::Rejected { request, reason } => {
                            wire::content_read_response::Result::Rejected(encode_failure(
                                *request, *reason,
                            ))
                        }
                    }),
                }
            }
            fn check_stamp(
                value: &wire::QueryStamp,
                expected: QueryStamp,
            ) -> Result<(), ProtocolError> {
                if decode_stamp(value)? != expected {
                    return Err(ProtocolError::Malformed);
                }
                Ok(())
            }
            fn decode_failure(
                value: &wire::ContentFailure,
                stamp: QueryStamp,
            ) -> Result<ContentReadFailure, ProtocolError> {
                check_stamp(required(&value.stamp)?, stamp)?;
                Ok(match value.reason {
                    1 => ContentReadFailure::StaleAuthority,
                    2 => ContentReadFailure::Cancelled,
                    3 => ContentReadFailure::Retired,
                    4 => ContentReadFailure::Capacity,
                    5 => ContentReadFailure::MissingResource,
                    6 => ContentReadFailure::InvalidRange,
                    7 => ContentReadFailure::Exhausted,
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn decode_topology_result(
                value: &wire::ContentReadResponse,
                request: &TopologyPageRequest,
            ) -> Result<TopologyPageResult, ProtocolError> {
                let stamp = request.stamp();
                Ok(match required(&value.result)? {
                    wire::content_read_response::Result::Topology(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        if value.rows.len() > usize::from(request.limits().rows()) {
                            return Err(ProtocolError::LimitExceeded);
                        }
                        let rows = value
                            .rows
                            .iter()
                            .map(decode_topology_row)
                            .collect::<Result<Vec<_>, _>>()?;
                        let continuation = value
                            .after
                            .as_ref()
                            .map(|row| {
                                TopologyContinuation::new(stamp, decode_topology_row(row)?)
                                    .map_err(|_| ProtocolError::Malformed)
                            })
                            .transpose()?;
                        ContentReadResult::Ready(
                            TopologyPage::new(request, rows, continuation)
                                .map_err(|_| ProtocolError::Malformed)?,
                        )
                    }
                    wire::content_read_response::Result::ResyncRequired(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        let current = decode_base(required(&value.current)?)?;
                        if current.session != stamp.base().session {
                            return Err(ProtocolError::Malformed);
                        }
                        ContentReadResult::ResyncRequired {
                            request: stamp,
                            current,
                        }
                    }
                    wire::content_read_response::Result::Rejected(value) => {
                        ContentReadResult::Rejected {
                            request: stamp,
                            reason: decode_failure(value, stamp)?,
                        }
                    }
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn decode_copy_event(
                value: &wire::CopyEvent,
                request: &CopyRequest,
            ) -> Result<CopyStreamItem, ProtocolError> {
                let stamp = request.stamp();
                Ok(match required(&value.item)? {
                    wire::copy_event::Item::Chunk(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        let part = match value.part {
                            1 => CopyPart::Plain,
                            2 => CopyPart::Html,
                            _ => return Err(ProtocolError::Malformed),
                        };
                        CopyStreamItem::Chunk(
                            CopyChunk::new(
                                request,
                                ResourceTransferId::new(value.transfer)
                                    .map_err(|_| ProtocolError::Malformed)?,
                                part,
                                value.offset,
                                value.total,
                                value.data.clone(),
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        )
                    }
                    wire::copy_event::Item::Complete(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        let html = match (&value.html_bytes, &value.html_digest) {
                            (Some(bytes), Some(digest)) => Some((
                                *bytes,
                                digest
                                    .as_slice()
                                    .try_into()
                                    .map_err(|_| ProtocolError::Malformed)?,
                            )),
                            (None, None) => None,
                            _ => return Err(ProtocolError::Malformed),
                        };
                        CopyStreamItem::Complete(
                            CopyCompletion::new(
                                request,
                                ResourceTransferId::new(value.transfer)
                                    .map_err(|_| ProtocolError::Malformed)?,
                                value.plain_bytes,
                                value
                                    .plain_digest
                                    .as_slice()
                                    .try_into()
                                    .map_err(|_| ProtocolError::Malformed)?,
                                html,
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        )
                    }
                    wire::copy_event::Item::ContentFailure(value) => CopyStreamItem::Failed {
                        stamp,
                        reason: decode_failure(value, stamp)?,
                    },
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn decode_find_event(
                value: &wire::FindEvent,
                request: &FindRequest,
            ) -> Result<FindStreamItem, ProtocolError> {
                let stamp = request.stamp();
                Ok(match required(&value.item)? {
                    wire::find_event::Item::Chunk(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        if value.matches.len() > MAX_FIND_CHUNK_MATCHES {
                            return Err(ProtocolError::LimitExceeded);
                        }
                        let matches = value
                            .matches
                            .iter()
                            .map(|value| {
                                FindMatch::new(
                                    stamp,
                                    decode_cell(required(&value.start)?)?,
                                    decode_cell(required(&value.end)?)?,
                                )
                                .map_err(|_| ProtocolError::Malformed)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let continuation = value
                            .continuation
                            .as_ref()
                            .map(|value| {
                                check_stamp(required(&value.stamp)?, stamp)?;
                                let continuation = FindContinuation::new(
                                    stamp,
                                    request.query(),
                                    decode_cell(required(&value.next)?)?,
                                )
                                .map_err(|_| ProtocolError::Malformed)?;
                                if value.query_fingerprint.as_slice()
                                    != continuation.query_fingerprint()
                                {
                                    return Err(ProtocolError::Malformed);
                                }
                                Ok(continuation)
                            })
                            .transpose()?;
                        FindStreamItem::Chunk(Box::new(
                            FindChunk::new(request, value.index, matches, continuation)
                                .map_err(|_| ProtocolError::Malformed)?,
                        ))
                    }
                    wire::find_event::Item::Complete(value) => {
                        check_stamp(required(&value.stamp)?, stamp)?;
                        FindStreamItem::Complete {
                            stamp,
                            chunks: value.chunks,
                        }
                    }
                    wire::find_event::Item::ContentFailure(value) => FindStreamItem::Failed {
                        stamp,
                        reason: decode_failure(value, stamp)?,
                    },
                    _ => return Err(ProtocolError::Malformed),
                })
            }
            pub fn encode_copy_event(value: &CopyStreamItem) -> wire::CopyEvent {
                wire::CopyEvent {
                    item: Some(match value {
                        CopyStreamItem::Chunk(chunk) => {
                            wire::copy_event::Item::Chunk(wire::CopyChunk {
                                stamp: Some(encode_stamp(chunk.stamp())),
                                transfer: chunk.transfer().get(),
                                part: if chunk.part() == CopyPart::Plain {
                                    1
                                } else {
                                    2
                                },
                                offset: chunk.offset(),
                                total: chunk.total(),
                                data: chunk.bytes().to_vec(),
                            })
                        }
                        CopyStreamItem::Complete(value) => {
                            wire::copy_event::Item::Complete(wire::CopyCompletion {
                                stamp: Some(encode_stamp(value.stamp())),
                                transfer: value.transfer().get(),
                                plain_bytes: value.plain_bytes(),
                                plain_digest: value.plain_digest().to_vec(),
                                html_bytes: value.html().map(|value| value.0),
                                html_digest: value.html().map(|value| value.1.to_vec()),
                            })
                        }
                        CopyStreamItem::Failed { stamp, reason } => {
                            wire::copy_event::Item::ContentFailure(encode_failure(*stamp, *reason))
                        }
                    }),
                }
            }
            fn encode_find_continuation(value: FindContinuation) -> wire::FindContinuation {
                wire::FindContinuation {
                    stamp: Some(encode_stamp(value.stamp())),
                    query_fingerprint: value.query_fingerprint().to_vec(),
                    next: Some(encode_cell(value.next())),
                }
            }
            pub fn encode_find_event(value: &FindStreamItem) -> wire::FindEvent {
                wire::FindEvent {
                    item: Some(match value {
                        FindStreamItem::Chunk(chunk) => {
                            wire::find_event::Item::Chunk(Box::new(wire::FindChunk {
                                stamp: Some(encode_stamp(chunk.stamp())),
                                index: chunk.index(),
                                matches: chunk
                                    .matches()
                                    .iter()
                                    .map(|value| wire::FindMatch {
                                        start: Some(encode_cell(value.start())),
                                        end: Some(encode_cell(value.end())),
                                    })
                                    .collect(),
                                continuation: chunk.continuation().map(encode_find_continuation),
                            }))
                        }
                        FindStreamItem::Complete { stamp, chunks } => {
                            wire::find_event::Item::Complete(wire::FindCompletion {
                                stamp: Some(encode_stamp(*stamp)),
                                chunks: *chunks,
                            })
                        }
                        FindStreamItem::Failed { stamp, reason } => {
                            wire::find_event::Item::ContentFailure(encode_failure(*stamp, *reason))
                        }
                    }),
                }
            }
        }
    };
}
profile!(v1);
profile!(v2);
