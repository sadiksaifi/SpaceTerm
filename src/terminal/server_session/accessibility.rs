//! Demand-driven server terminal semantics for native assistive technology.
//!
//! The server owns row and cell identities. This client retains the rows of its last applied
//! snapshot, so deltas, resize and fetched history keep the identities native navigation holds.
use super::*;
use crate::terminal::accessibility::{
    AccessibilityCell, AccessibilityCellRef, AccessibilityRowId, AccessibilityRowUpdate,
    AccessibilityScreen, AccessibilitySelectionRefs, AccessibilitySelectionRequest,
    AccessibilityUpdate, TerminalAccessibilityModel, TerminalAccessibilityState,
};
use pane::accessibility as wire;
use std::ops::Range;

/// Bounds one poll's transfer work, so semantics never starve input or Screens.
const CHUNKS_PER_POLL: usize = 16;

/// The rows this client retains for one connection epoch. A new epoch starts empty, so nothing
/// a former lease presented can resolve a Selection or reach a later snapshot.
#[derive(Default)]
pub(super) struct RetainedSemantics {
    rows: TerminalAccessibilityState,
    last: Option<Applied>,
}

#[derive(Clone)]
struct Applied {
    revision: u64,
    screen: AccessibilityScreen,
    screen_generation: usize,
    visible_lines: Range<usize>,
    cursor: Option<AccessibilityCellRef>,
    selection: Option<AccessibilitySelectionRefs>,
}

impl RetainedSemantics {
    /// Resolves a native Selection request against the model this client published last.
    /// `None` means the request names content this client no longer presents.
    pub(super) fn resolve_selection(
        &self,
        request: &AccessibilitySelectionRequest,
    ) -> Option<Option<wire::SelectionRefs>> {
        let selection = self.rows.resolve_selection(request)?;
        Some(selection.map(|(start, end)| wire::SelectionRefs {
            start: encode_ref(start),
            end: encode_ref(end),
            rectangle: false,
        }))
    }

    fn base(&self) -> Option<u64> {
        self.last.as_ref().map(|applied| applied.revision)
    }

    /// Applies a complete snapshot, or reports that its delta needs rows this client lacks.
    fn apply(
        &mut self,
        snapshot: wire::Snapshot,
        generation: PresentationGeneration,
    ) -> Result<Option<Arc<TerminalAccessibilityModel>>, Rejected> {
        let update = decode_snapshot(snapshot)?;
        let delivered = update
            .changed_rows
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>();
        if update
            .topology
            .iter()
            .flatten()
            .any(|id| !delivered.contains(id) && !self.rows.retains_row(*id))
        {
            return Err(Rejected::MissingBase);
        }
        let applied = Applied {
            revision: update.revision,
            screen: update.screen,
            screen_generation: update.screen_generation,
            visible_lines: update.visible_lines.clone(),
            cursor: update.cursor,
            selection: update.selection,
        };
        let model = self.rows.apply(update, generation);
        self.last = Some(applied);
        Ok(model)
    }

    /// Binds the retained document to a newer presentation, so the Pane never drops semantics
    /// that were published just before a later Screen.
    fn rebind(
        &mut self,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        let applied = self.last.clone()?;
        self.rows.apply(
            AccessibilityUpdate {
                revision: applied.revision,
                screen: applied.screen,
                screen_generation: applied.screen_generation,
                complete: true,
                more: false,
                topology: None,
                visible_lines: applied.visible_lines,
                cursor: applied.cursor,
                selection: applied.selection,
                changed_rows: Vec::new(),
            },
            generation,
        )
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Rejected {
    MissingBase,
    Malformed,
}

struct Transfer {
    revision: Option<u64>,
    base: Option<u64>,
    total: Option<usize>,
    bytes: Vec<u8>,
}

/// Fetches semantics for one lease while native assistive technology asks for them.
pub(super) struct Fetcher {
    lease: Lease,
    server_revision: Option<u64>,
    failed_revision: Option<u64>,
    transfer: Option<Transfer>,
    requested: bool,
}

impl Fetcher {
    pub(super) fn new(lease: Lease) -> Self {
        Self {
            lease,
            server_revision: None,
            failed_revision: None,
            transfer: None,
            requested: false,
        }
    }

    pub(super) fn note_revision(&mut self, revision: u64) {
        self.server_revision = Some(revision);
    }

    /// Republishes the retained document for a newer Screen while semantics are demanded.
    pub(super) fn rebind(
        &self,
        state: &DevelopmentPaneState,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        if !state.accessibility_demand.load(Ordering::Acquire) {
            return None;
        }
        state
            .accessibility
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rebind(generation)
    }

    /// Advances the current transfer, returning a model to publish when one completes.
    pub(super) fn poll(
        &mut self,
        state: &DevelopmentPaneState,
        generation: PresentationGeneration,
        exchange: &mut impl FnMut(&Request) -> Result<Reply, ServerError>,
    ) -> Result<Option<Arc<TerminalAccessibilityModel>>, ServerError> {
        if !state.accessibility_demand.load(Ordering::Acquire) {
            self.transfer = None;
            if std::mem::take(&mut self.requested) {
                expect_unchanged(exchange(&Request::EndAccessibilityDemand {
                    lease: self.lease,
                })?)?;
            }
            return Ok(None);
        }
        if self.transfer.is_none() {
            let base = state
                .accessibility
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .base();
            let current = base.is_some() && self.server_revision.is_none_or(|r| Some(r) == base);
            let failed =
                self.failed_revision.is_some() && self.server_revision == self.failed_revision;
            if current || failed {
                return Ok(None);
            }
            self.transfer = Some(Transfer {
                revision: None,
                base,
                total: None,
                bytes: Vec::new(),
            });
        }
        for _ in 0..CHUNKS_PER_POLL {
            let Some(transfer) = self.transfer.as_mut() else {
                return Ok(None);
            };
            let reply = exchange(&Request::Accessibility {
                lease: self.lease,
                revision: transfer.revision,
                offset: transfer.bytes.len(),
                base: transfer.base,
            })?;
            self.requested = true;
            let Reply::Accessibility(result) = reply else {
                return Err(ServerError::Protocol);
            };
            let chunk = match result {
                Ok(chunk) => chunk,
                Err(wire::Error::Pending) => return Ok(None),
                Err(wire::Error::Stale) => {
                    self.transfer = None;
                    return Ok(None);
                }
                Err(wire::Error::TooLarge) => {
                    self.failed_revision = transfer.revision.or(self.server_revision);
                    self.transfer = None;
                    return Ok(None);
                }
            };
            if !transfer.accepts(&chunk) {
                return Err(ServerError::Protocol);
            }
            transfer.revision = Some(chunk.revision);
            transfer.total = Some(chunk.total);
            transfer.bytes.extend_from_slice(&chunk.bytes);
            if transfer.bytes.len() < chunk.total {
                continue;
            }
            let Some(transfer) = self.transfer.take() else {
                return Ok(None);
            };
            let snapshot = serde_json::from_slice::<wire::Snapshot>(&transfer.bytes)
                .map_err(|_| ServerError::Protocol)?;
            if snapshot.revision != chunk.revision {
                return Err(ServerError::Protocol);
            }
            self.server_revision = Some(
                self.server_revision
                    .map_or(chunk.revision, |known| known.max(chunk.revision)),
            );
            let mut retained = state
                .accessibility
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            return match retained.apply(snapshot, generation) {
                Ok(model) => {
                    self.failed_revision = None;
                    Ok(model)
                }
                Err(Rejected::MissingBase) => {
                    // The next transfer carries every row, because no base is named.
                    *retained = RetainedSemantics::default();
                    Ok(None)
                }
                Err(Rejected::Malformed) => Err(ServerError::Protocol),
            };
        }
        Ok(None)
    }
}

impl Transfer {
    fn accepts(&self, chunk: &wire::Chunk) -> bool {
        let expected_len = wire::CHUNK_BYTES.min(chunk.total.saturating_sub(chunk.offset));
        chunk.total > 0
            && chunk.total <= wire::MAX_BYTES
            && chunk.offset == self.bytes.len()
            && chunk.bytes.len() == expected_len
            && self
                .revision
                .is_none_or(|revision| revision == chunk.revision)
            && self.total.is_none_or(|total| total == chunk.total)
    }
}

fn decode_screen(screen: wire::Screen) -> AccessibilityScreen {
    match screen {
        wire::Screen::Primary => AccessibilityScreen::Primary,
        wire::Screen::Alternate => AccessibilityScreen::Alternate,
    }
}

fn encode_screen(screen: AccessibilityScreen) -> wire::Screen {
    match screen {
        AccessibilityScreen::Primary => wire::Screen::Primary,
        AccessibilityScreen::Alternate => wire::Screen::Alternate,
    }
}

fn encode_ref(reference: AccessibilityCellRef) -> wire::CellRef {
    wire::CellRef {
        row: wire::RowId {
            screen: encode_screen(reference.row.screen),
            screen_generation: reference.row.screen_generation as u64,
            node_serial: reference.row.node_serial,
            page_row: reference.row.page_row,
        },
        row_revision: reference.row_revision,
        column: reference.column,
    }
}

/// Every identity must name the snapshot's screen, so a snapshot cannot mix screens.
fn decode_snapshot(snapshot: wire::Snapshot) -> Result<AccessibilityUpdate, Rejected> {
    let screen = decode_screen(snapshot.screen);
    let screen_generation =
        usize::try_from(snapshot.screen_generation).map_err(|_| Rejected::Malformed)?;
    let row_id = |id: wire::RowId| {
        (id.screen == snapshot.screen && id.screen_generation == snapshot.screen_generation)
            .then_some(AccessibilityRowId {
                screen,
                screen_generation,
                node_serial: id.node_serial,
                page_row: id.page_row,
            })
            .ok_or(Rejected::Malformed)
    };
    let cell_ref = |reference: wire::CellRef| {
        Ok(AccessibilityCellRef {
            row: row_id(reference.row)?,
            row_revision: reference.row_revision,
            column: reference.column,
        })
    };
    let topology = snapshot
        .topology
        .into_iter()
        .map(row_id)
        .collect::<Result<Vec<_>, _>>()?;
    let changed_rows = snapshot
        .rows
        .into_iter()
        .map(|row| {
            Ok(AccessibilityRowUpdate {
                id: row_id(row.id)?,
                revision: row.revision,
                soft_wrapped: row.soft_wrapped,
                cells: row
                    .cells
                    .into_iter()
                    .map(|cell| {
                        AccessibilityCell::at_column(cell.text, cell.column, cell.width_cells)
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, Rejected>>()?;
    let cursor = snapshot.cursor.map(cell_ref).transpose()?;
    let selection = snapshot
        .selection
        .map(|selection| {
            Ok::<_, Rejected>(AccessibilitySelectionRefs {
                start: cell_ref(selection.start)?,
                end: cell_ref(selection.end)?,
                rectangle: selection.rectangle,
            })
        })
        .transpose()?;
    if snapshot.visible_lines.start > snapshot.visible_lines.end
        || snapshot.visible_lines.end > topology.len()
    {
        return Err(Rejected::Malformed);
    }
    Ok(AccessibilityUpdate {
        revision: snapshot.revision,
        screen,
        screen_generation,
        complete: true,
        more: false,
        topology: Some(topology),
        visible_lines: snapshot.visible_lines,
        cursor,
        selection,
        changed_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn lease() -> Lease {
        Lease {
            server: [1; 32],
            session: [2; 32],
            control: [3; 32],
            connection: [4; 32],
        }
    }

    fn row_id(page_row: u16) -> wire::RowId {
        wire::RowId {
            screen: wire::Screen::Primary,
            screen_generation: 1,
            node_serial: 7,
            page_row,
        }
    }

    fn row(page_row: u16, revision: u64, text: &str) -> wire::Row {
        wire::Row {
            id: row_id(page_row),
            revision,
            soft_wrapped: false,
            cells: text
                .chars()
                .enumerate()
                .map(|(column, character)| wire::Cell {
                    text: character.to_string(),
                    column: column as u16,
                    width_cells: 1,
                })
                .collect(),
        }
    }

    fn snapshot(revision: u64, topology: &[u16], rows: Vec<wire::Row>) -> wire::Snapshot {
        wire::Snapshot {
            revision,
            screen: wire::Screen::Primary,
            screen_generation: 1,
            topology: topology.iter().copied().map(row_id).collect(),
            rows,
            visible_lines: 0..topology.len(),
            cursor: None,
            selection: None,
        }
    }

    /// A server that answers each semantic request with one complete chunk of `snapshot`.
    struct Server {
        snapshot: Option<wire::Snapshot>,
        requests: Rc<RefCell<Vec<Request>>>,
    }

    impl Server {
        fn new(snapshot: wire::Snapshot) -> Self {
            Self {
                snapshot: Some(snapshot),
                requests: Rc::default(),
            }
        }

        fn exchange(&mut self) -> impl FnMut(&Request) -> Result<Reply, ServerError> + '_ {
            |request| {
                self.requests.borrow_mut().push(request.clone());
                match request {
                    Request::Accessibility { offset, .. } => {
                        let snapshot = self.snapshot.as_ref().ok_or(ServerError::Protocol)?;
                        let bytes =
                            serde_json::to_vec(snapshot).map_err(|_| ServerError::Protocol)?;
                        let end = (offset + wire::CHUNK_BYTES).min(bytes.len());
                        Ok(Reply::Accessibility(Ok(wire::Chunk {
                            revision: snapshot.revision,
                            offset: *offset,
                            total: bytes.len(),
                            bytes: bytes[*offset..end].to_vec(),
                        })))
                    }
                    Request::EndAccessibilityDemand { .. } => Ok(Reply::Unchanged),
                    _ => Err(ServerError::Protocol),
                }
            }
        }

        fn bases(&self) -> Vec<Option<u64>> {
            self.requests
                .borrow()
                .iter()
                .filter_map(|request| match request {
                    Request::Accessibility { base, .. } => Some(*base),
                    _ => None,
                })
                .collect()
        }

        fn ends(&self) -> usize {
            self.requests
                .borrow()
                .iter()
                .filter(|request| matches!(request, Request::EndAccessibilityDemand { .. }))
                .count()
        }
    }

    fn demanded() -> DevelopmentPaneState {
        let state = DevelopmentPaneState::new();
        state.accessibility_demand.store(true, Ordering::Release);
        state
    }

    fn rows(model: &TerminalAccessibilityModel) -> Vec<(u16, String)> {
        model
            .rows()
            .map(|row| (row.id.page_row, row.text.to_owned()))
            .collect()
    }

    #[test]
    fn a_delta_keeps_unchanged_row_identities_through_growth() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(1, &[0, 1], vec![row(0, 1, "ab"), row(1, 1, "cd")]));
        let first = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap()
            .unwrap();
        let first_ids = first.rows().map(|row| row.id).collect::<Vec<_>>();

        fetcher.note_revision(2);
        server.snapshot = Some(snapshot(
            2,
            &[0, 1, 2],
            vec![row(1, 2, "cX"), row(2, 1, "ef")],
        ));
        let second = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(server.bases(), [None, Some(1)]);
        assert_eq!(
            rows(&second),
            [(0, "ab".into()), (1, "cX".into()), (2, "ef".into())]
        );
        assert_eq!(
            second.rows().next().map(|row| row.id),
            first_ids.first().copied()
        );
    }

    #[test]
    fn a_delta_missing_retained_rows_refetches_every_row() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(4, &[0, 1], vec![row(1, 1, "cd")]));
        let missing = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap();
        assert!(missing.is_none());

        server.snapshot = Some(snapshot(4, &[0, 1], vec![row(0, 1, "ab"), row(1, 1, "cd")]));
        let full = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(server.bases(), [None, None]);
        assert_eq!(rows(&full), [(0, "ab".into()), (1, "cd".into())]);
    }

    #[test]
    fn a_current_revision_issues_no_request() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(1, &[0], vec![row(0, 1, "ab")]));
        fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap();
        fetcher.note_revision(1);
        fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap();

        assert_eq!(server.bases().len(), 1);
    }

    #[test]
    fn selection_resolves_only_within_the_epoch_that_fetched_it() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(1, &[0, 1], vec![row(0, 1, "ab"), row(1, 1, "cd")]));
        let model = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap()
            .unwrap();
        let request = model.selection_request(0..2).unwrap();
        let resolved = state
            .accessibility
            .lock()
            .unwrap()
            .resolve_selection(&request);
        assert_eq!(
            resolved,
            Some(Some(wire::SelectionRefs {
                start: wire::CellRef {
                    row: row_id(0),
                    row_revision: 1,
                    column: 0,
                },
                end: wire::CellRef {
                    row: row_id(0),
                    row_revision: 1,
                    column: 1,
                },
                rectangle: false,
            }))
        );

        state.invalidate();

        assert_eq!(
            state
                .accessibility
                .lock()
                .unwrap()
                .resolve_selection(&request),
            None
        );
    }

    #[test]
    fn a_snapshot_naming_another_screen_is_a_protocol_failure() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut mixed = snapshot(1, &[0], vec![row(0, 1, "ab")]);
        mixed.topology[0].screen = wire::Screen::Alternate;
        let mut server = Server::new(mixed);

        assert_eq!(
            fetcher
                .poll(
                    &state,
                    PresentationGeneration::default(),
                    &mut server.exchange()
                )
                .err(),
            Some(ServerError::Protocol)
        );
    }

    #[test]
    fn a_chunk_out_of_order_is_a_protocol_failure() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let result = fetcher.poll(&state, PresentationGeneration::default(), &mut |_| {
            Ok(Reply::Accessibility(Ok(wire::Chunk {
                revision: 1,
                offset: 1,
                total: 2,
                bytes: vec![b'{'],
            })))
        });

        assert_eq!(result.err(), Some(ServerError::Protocol));
    }

    #[test]
    fn ending_demand_tells_the_server_once() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(1, &[0], vec![row(0, 1, "ab")]));
        fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap();
        state.accessibility_demand.store(false, Ordering::Release);
        for _ in 0..2 {
            fetcher
                .poll(
                    &state,
                    PresentationGeneration::default(),
                    &mut server.exchange(),
                )
                .unwrap();
        }

        assert_eq!(server.ends(), 1);
    }

    #[test]
    fn a_newer_screen_rebinds_the_retained_document() {
        let state = demanded();
        let mut fetcher = Fetcher::new(lease());
        let mut server = Server::new(snapshot(1, &[0], vec![row(0, 1, "ab")]));
        let first = fetcher
            .poll(
                &state,
                PresentationGeneration::default(),
                &mut server.exchange(),
            )
            .unwrap()
            .unwrap();
        let next = PresentationGeneration::default().next();
        let rebound = fetcher.rebind(&state, next).unwrap();

        assert_eq!(
            (rebound.generation(), rebound.shares_document(&first)),
            (next, true)
        );
    }
}
