//! Bounded semantic transfers, serialized away from the server's dispatch thread.
use super::*;
use pane::accessibility as wire;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{collections::BTreeMap, io::Write, sync::mpsc};

type Encoded = Result<Vec<u8>, wire::Error>;
struct Transfer {
    revision: u64,
    base: Option<u64>,
    model: Arc<TerminalAccessibilityModel>,
    bytes: Encoded,
    delivered: bool,
}
struct Job {
    revision: u64,
    base: Option<u64>,
    model: Arc<TerminalAccessibilityModel>,
    result: mpsc::Receiver<Encoded>,
    obsolete: bool,
}
#[derive(Default)]
pub(super) struct Accessibility {
    latest: Option<Arc<TerminalAccessibilityModel>>,
    requested: bool,
    revision: u64,
    sent: u64,
    completed: Option<(u64, Arc<TerminalAccessibilityModel>)>,
    transfer: Option<Transfer>,
    job: Option<Job>,
    resetting: bool,
    minimum_generation: Option<PresentationGeneration>,
}
impl Accessibility {
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn take_changed_revision(&mut self) -> Option<u64> {
        if !self.requested || self.sent == self.revision {
            return None;
        }
        self.sent = self.revision;
        Some(self.revision)
    }
    pub(super) fn update(&mut self, model: Arc<TerminalAccessibilityModel>) {
        if self.resetting
            || self
                .minimum_generation
                .is_some_and(|minimum| model.generation() < minimum)
        {
            return;
        }
        self.minimum_generation = None;
        if self.latest.as_ref().is_none_or(|old| {
            !old.shares_document(&model)
                || old.visible_lines() != model.visible_lines()
                || old.cursor_cell() != model.cursor_cell()
                || old.selection_range() != model.selection_range()
                || old.active_screen() != model.active_screen()
        }) {
            self.revision = self.revision.saturating_add(1);
        }
        self.latest = Some(model);
    }
    pub(super) fn disconnect(&mut self) {
        self.requested = false;
        self.completed = None;
        self.transfer = None;
        // Keep an outstanding job until it finishes, so reconnect cannot start another serializer.
        if let Some(job) = &mut self.job {
            job.obsolete = true;
        }
    }
    pub(super) fn reset(&mut self) {
        self.disconnect();
        self.latest = None;
        self.resetting = true;
    }
    pub(super) fn bind_reset_screen(&mut self, generation: PresentationGeneration) {
        if self.resetting {
            self.minimum_generation = Some(generation);
            self.resetting = false;
        }
    }
    fn collect(&mut self) {
        let Some(job) = &self.job else {
            return;
        };
        let bytes = match job.result.try_recv() {
            Ok(bytes) => bytes,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(wire::Error::TooLarge),
        };
        if let Some(job) = self.job.take().filter(|job| !job.obsolete) {
            self.transfer = Some(Transfer {
                revision: job.revision,
                base: job.base,
                model: job.model,
                bytes,
                delivered: false,
            });
        }
    }
    pub(super) fn chunk(
        &mut self,
        revision: Option<u64>,
        offset: usize,
        base: Option<u64>,
    ) -> Result<wire::Chunk, wire::Error> {
        self.requested = true;
        self.collect();
        if revision.is_none() {
            if offset != 0 {
                return Err(wire::Error::Stale);
            }
            let model = self.latest.as_ref().ok_or(wire::Error::Pending)?;
            if self
                .transfer
                .as_ref()
                .is_none_or(|t| (t.delivered && t.revision != self.revision) || t.base != base)
            {
                if self.job.is_some() {
                    return Err(wire::Error::Pending);
                }
                let previous = self
                    .completed
                    .as_ref()
                    .filter(|(rev, _)| Some(*rev) == base)
                    .map(|(_, model)| Arc::clone(model));
                let (sender, result) = mpsc::sync_channel(1);
                let model = Arc::clone(model);
                let serializing = Arc::clone(&model);
                let revision = self.revision;
                std::thread::Builder::new()
                    .name("terminal-accessibility".into())
                    .spawn(move || {
                        let _ = sender.send(encode(&serializing, revision, previous.as_deref()));
                    })
                    .map_err(|_| wire::Error::TooLarge)?;
                self.job = Some(Job {
                    revision,
                    base,
                    model,
                    result,
                    obsolete: false,
                });
                return Err(wire::Error::Pending);
            }
        }
        let transfer = self.transfer.as_mut().ok_or(wire::Error::Pending)?;
        if revision.is_some_and(|revision| revision != transfer.revision) || transfer.base != base {
            return Err(wire::Error::Stale);
        }
        let bytes = transfer.bytes.as_ref().map_err(|error| *error)?;
        if offset >= bytes.len() {
            return Err(wire::Error::Stale);
        }
        let end = offset.saturating_add(wire::CHUNK_BYTES).min(bytes.len());
        if end == bytes.len() {
            transfer.delivered = true;
            self.completed = Some((transfer.revision, Arc::clone(&transfer.model)));
        }
        Ok(wire::Chunk {
            revision: transfer.revision,
            offset,
            total: bytes.len(),
            bytes: bytes[offset..end].to_vec(),
        })
    }
}
fn screen(screen: AccessibilityScreen) -> wire::Screen {
    match screen {
        AccessibilityScreen::Primary => wire::Screen::Primary,
        AccessibilityScreen::Alternate => wire::Screen::Alternate,
    }
}
fn row_id(id: AccessibilityRowId) -> wire::RowId {
    wire::RowId {
        screen: screen(id.screen),
        screen_generation: id.screen_generation as u64,
        node_serial: id.node_serial,
        page_row: id.page_row,
    }
}
fn cell_ref(reference: AccessibilityCellRef) -> wire::CellRef {
    wire::CellRef {
        row: row_id(reference.row),
        row_revision: reference.row_revision,
        column: reference.column,
    }
}
pub(super) fn decode_selection(
    selection: wire::SelectionRefs,
) -> Result<crate::terminal::accessibility::AccessibilitySelectionRefs, ()> {
    let decode = |reference: wire::CellRef| {
        Ok(AccessibilityCellRef {
            row: AccessibilityRowId {
                screen: match reference.row.screen {
                    wire::Screen::Primary => AccessibilityScreen::Primary,
                    wire::Screen::Alternate => AccessibilityScreen::Alternate,
                },
                screen_generation: usize::try_from(reference.row.screen_generation)
                    .map_err(|_| ())?,
                node_serial: reference.row.node_serial,
                page_row: reference.row.page_row,
            },
            row_revision: reference.row_revision,
            column: reference.column,
        })
    };
    Ok(crate::terminal::accessibility::AccessibilitySelectionRefs {
        start: decode(selection.start)?,
        end: decode(selection.end)?,
        rectangle: selection.rectangle,
    })
}

struct Rows<'a> {
    model: &'a TerminalAccessibilityModel,
    previous: Option<&'a TerminalAccessibilityModel>,
}
impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let previous = self
            .previous
            .into_iter()
            .flat_map(|model| model.rows())
            .map(|row| (row.id, row.revision))
            .collect::<BTreeMap<_, _>>();
        let mut rows = serializer.serialize_seq(None)?;
        for row in self.model.rows() {
            if previous.get(&row.id) == Some(&row.revision) {
                continue;
            }
            if row.text.len() > wire::MAX_BYTES {
                return Err(serde::ser::Error::custom("accessibility-capacity"));
            }
            rows.serialize_element(&wire::Row {
                id: row_id(row.id),
                revision: row.revision,
                soft_wrapped: row.soft_wrapped,
                cells: row
                    .cells()
                    .map(|cell| wire::Cell {
                        text: cell.text.to_owned(),
                        column: cell.columns.start,
                        width_cells: cell.columns.end - cell.columns.start,
                    })
                    .collect(),
            })?;
        }
        rows.end()
    }
}
#[derive(Serialize)]
struct Snapshot<'a> {
    revision: u64,
    screen: wire::Screen,
    screen_generation: u64,
    topology: Vec<wire::RowId>,
    rows: Rows<'a>,
    visible_lines: std::ops::Range<usize>,
    cursor: Option<wire::CellRef>,
    selection: Option<wire::SelectionRefs>,
}
struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > wire::MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::ErrorKind::OutOfMemory.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(
    model: &TerminalAccessibilityModel,
    revision: u64,
    previous: Option<&TerminalAccessibilityModel>,
) -> Encoded {
    let cursor = model.cursor_ref().map(cell_ref);
    let selection = model.selection_refs().map(|refs| wire::SelectionRefs {
        start: cell_ref(refs.start),
        end: cell_ref(refs.end),
        rectangle: refs.rectangle,
    });
    let snapshot = Snapshot {
        revision,
        screen: screen(model.active_screen()),
        screen_generation: model.screen_generation() as u64,
        topology: model.rows().map(|row| row_id(row.id)).collect(),
        rows: Rows { model, previous },
        visible_lines: model.visible_lines(),
        cursor,
        selection,
    };
    let mut output = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut output, &snapshot).map_err(|_| wire::Error::TooLarge)?;
    Ok(output.0)
}
