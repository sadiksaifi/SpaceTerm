//! Pushed server terminal semantics for native assistive technology (ADR 0062).
//!
//! The server owns row and cell identities and pushes each semantic revision as one or more
//! messages relative to the revision this client acknowledged. The Pane worker stages a
//! revision's messages in channel order and applies the revision only at its tail, so assistive
//! technology never reads part of one. This client retains the rows of the revision it applied
//! last, so deltas, resize and Scrollback keep the identities native navigation holds.
use super::inbox::Ack;
use super::*;
use crate::terminal::accessibility::{
    AccessibilityCellRef, AccessibilityRetainedRow, AccessibilityRevision, AccessibilityRowId,
    AccessibilityScreen, AccessibilitySelectionRefs, AccessibilitySelectionRequest,
    AccessibilityUpdate, AppliedTopology, TerminalAccessibilityModel, TerminalAccessibilityState,
};
use pane::accessibility as wire;
use std::ops::Range;

/// The rows this client retains for one connection epoch. A new epoch starts empty, so nothing
/// a former connection presented can resolve a Selection or reach a later revision.
#[derive(Default)]
pub(super) struct RetainedSemantics {
    rows: TerminalAccessibilityState,
    last: Option<Applied>,
    /// This client's scrolled view and Selection, which the published model follows.
    view: super::scrollback::AccessibilityView,
}

/// What the revision applied last presents.
#[derive(Clone)]
struct Applied {
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

    /// Applies a whole revision atomically, bound to the presentation `generation`. Returns
    /// whether it changed what the document presents.
    fn apply(
        &mut self,
        revision: AccessibilityRevision,
        generation: PresentationGeneration,
    ) -> Result<bool, ServerError> {
        let applied = Applied {
            screen: revision.screen,
            screen_generation: revision.screen_generation,
            visible_lines: revision.visible_lines.clone(),
            cursor: revision.cursor,
            selection: revision.selection,
        };
        let model = self
            .rows
            .apply_revision(revision, generation)
            .map_err(|_| ServerError::Protocol)?;
        self.last = Some(applied);
        Ok(model.is_some())
    }

    /// Follows this client's view. Returns whether it changed.
    pub(super) fn set_view(&mut self, view: super::scrollback::AccessibilityView) -> bool {
        std::mem::replace(&mut self.view, view) != view
    }

    /// The visible lines and Selection of `applied` as this client presents them: moved up by
    /// how far the view is scrolled back, with this client's Selection in place of the server's
    /// while it has one.
    fn project(&self, applied: &Applied) -> (Range<usize>, Option<AccessibilitySelectionRefs>) {
        let visible = applied.visible_lines.clone();
        let shift = usize::try_from(self.view.scrolled)
            .unwrap_or(usize::MAX)
            .min(visible.start);
        let shown = visible.start - shift..visible.end - shift;
        let Some((start, end)) = self.view.selection else {
            return (shown, applied.selection);
        };
        // Line 0 of the selection is the first row of the screen.
        let cell = |(line, column): (i64, u16)| {
            let line = usize::try_from(i64::try_from(visible.start).ok()? + line).ok()?;
            let row = self.rows.latest()?.row(line)?;
            Some(AccessibilityCellRef {
                row: row.id,
                row_revision: row.revision,
                column,
            })
        };
        let selection = cell(start)
            .zip(cell(end))
            .map(|(start, end)| AccessibilitySelectionRefs {
                start,
                end,
                rectangle: false,
            });
        (shown, selection)
    }

    /// Binds the retained document to `generation` and this client's view. `None` when nothing
    /// is retained or the binding presents what the latest model already does.
    fn rebind(
        &mut self,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        let applied = self.last.clone()?;
        let (visible_lines, selection) = self.project(&applied);
        self.rows.apply(
            AccessibilityUpdate {
                revision: 0,
                screen: applied.screen,
                screen_generation: applied.screen_generation,
                complete: true,
                more: false,
                topology: None,
                visible_lines,
                cursor: applied.cursor,
                selection,
                changed_rows: Vec::new(),
            },
            generation,
        )
    }
}

/// One channel's semantic demand and the revision it is receiving.
///
/// Each demand start names a generation greater than every earlier one on the channel. Messages
/// of any other generation, and every message while demand is ended, are discarded without an
/// acknowledgement.
pub(super) struct Semantics {
    /// The generation of the last start on this channel, or 0 before any.
    generation: u64,
    demanded: bool,
    /// The revision applied last in the current generation, the only base a delta may name.
    applied: Option<u64>,
    staging: Option<Staging>,
    /// A revision applied since the last publication changed what the document presents.
    unpublished: bool,
}

impl Semantics {
    pub(super) fn new() -> Self {
        Self {
            generation: 0,
            demanded: false,
            applied: None,
            staging: None,
            unpublished: false,
        }
    }

    /// Starts or ends the channel's demand to match what assistive technology asked for.
    /// Demand never follows visibility: a hidden Pane keeps it and its document.
    pub(super) fn follow(
        &mut self,
        wanted: bool,
        send: impl FnOnce(wire::Demand) -> Result<(), ServerError>,
    ) -> Result<(), ServerError> {
        if wanted == self.demanded {
            return Ok(());
        }
        self.demanded = wanted;
        self.staging = None;
        if !wanted {
            return send(wire::Demand::End);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ServerError::Protocol)?;
        // The document stays presented until the new generation's complete document applies.
        self.applied = None;
        send(wire::Demand::Start {
            generation: self.generation,
        })
    }

    /// Whether demand stands on the channel.
    pub(super) fn demanded(&self) -> bool {
        self.demanded
    }

    /// Stages one message of a revision in channel order. When it carries the revision's tail,
    /// applies the whole revision to `retained`, bound to `generation`, and returns its
    /// acknowledgement. A message that breaks the exchange is a protocol failure.
    pub(super) fn receive(
        &mut self,
        update: wire::Update,
        retained: &Mutex<RetainedSemantics>,
        generation: PresentationGeneration,
    ) -> Result<Option<Ack>, ServerError> {
        let header = update.header;
        if !self.demanded || header.demand_generation != self.generation {
            return Ok(None);
        }
        let staging = match &mut self.staging {
            Some(staging) if staging.header == header => staging,
            Some(_) => return Err(ServerError::Protocol),
            None => {
                if header.base.is_some() && header.base != self.applied {
                    return Err(ServerError::Protocol);
                }
                let base = match header.base {
                    Some(_) => Some(
                        Base::of(&retained.lock().unwrap_or_else(|e| e.into_inner()))
                            .ok_or(ServerError::Protocol)?,
                    ),
                    None => None,
                };
                self.staging.insert(Staging::new(header, base)?)
            }
        };
        staging.stage(update.splices, update.rows)?;
        let Some(tail) = update.tail else {
            return Ok(None);
        };
        let Some(staging) = self.staging.take() else {
            return Ok(None);
        };
        let mut retained = retained.lock().unwrap_or_else(|e| e.into_inner());
        let revision = staging.finish(tail, retained.rows.applied())?;
        self.unpublished |= retained.apply(revision, generation)?;
        self.applied = Some(header.revision);
        Ok(Some(Ack::Semantics {
            demand_generation: header.demand_generation,
            revision: header.revision,
        }))
    }

    /// Discards an unfinished revision at the channel's last event. Nothing of it is published.
    pub(super) fn cancel(&mut self) {
        self.staging = None;
    }

    /// The model to hand to the view now, while demand stands: the retained document bound to
    /// the presentation `generation` and this client's current scrolled view and Selection.
    /// `None` when it presents what the last publication did. The worker calls this after it
    /// presents a screen or applies revisions, so the model it sends last always follows the
    /// newest presentation and view, whatever order a drain applied frames and tails in.
    pub(super) fn publish(
        &mut self,
        retained: &Mutex<RetainedSemantics>,
        generation: PresentationGeneration,
    ) -> Option<Arc<TerminalAccessibilityModel>> {
        let unpublished = std::mem::take(&mut self.unpublished);
        if !self.demanded {
            return None;
        }
        let mut retained = retained.lock().unwrap_or_else(|e| e.into_inner());
        // A revision applied with this generation and view is already the latest model.
        retained.rebind(generation).or_else(|| {
            unpublished
                .then(|| retained.rows.latest().cloned().map(Arc::new))
                .flatten()
        })
    }
}

/// The document a delta applies to: the topology of the revision applied last.
struct Base {
    screen: AccessibilityScreen,
    screen_generation: usize,
    topology: Arc<[AccessibilityRowId]>,
}

impl Base {
    fn of(retained: &RetainedSemantics) -> Option<Self> {
        let applied = retained.rows.applied()?;
        Some(Self {
            screen: applied.screen,
            screen_generation: applied.screen_generation,
            topology: Arc::clone(applied.topology()),
        })
    }
}

/// A stretch of the staged topology: lines kept from the base, or consecutive page rows of one
/// page node the revision inserted.
#[derive(Clone, Copy)]
enum Segment {
    Base { start: u32, len: u32 },
    Run(wire::Run),
}

impl Segment {
    fn len(&self) -> u32 {
        match self {
            Self::Base { len, .. } => *len,
            Self::Run(run) => u32::from(run.count),
        }
    }

    /// The first `at` lines and the rest.
    fn split(self, at: u32) -> (Self, Self) {
        match self {
            Self::Base { start, len } => (
                Self::Base { start, len: at },
                Self::Base {
                    start: start + at,
                    len: len - at,
                },
            ),
            Self::Run(run) => {
                let at = at as u16;
                (
                    Self::Run(wire::Run { count: at, ..run }),
                    Self::Run(wire::Run {
                        page_row: run.page_row + at,
                        count: run.count - at,
                        ..run
                    }),
                )
            }
        }
    }
}

/// A row whose fragments are still arriving.
struct Fragment {
    line: u32,
    revision: u64,
    soft_wrapped: bool,
    text: String,
    cells: Vec<wire::CellSpan>,
    /// The first column the next fragment may start at.
    next_column: u16,
}

/// The messages of one revision received so far. Nothing of it reaches the retained document
/// before its tail.
struct Staging {
    header: wire::Header,
    screen: AccessibilityScreen,
    screen_generation: usize,
    base: Option<Base>,
    segments: Vec<Segment>,
    len: u32,
    spliced: bool,
    /// Set by the first row, after which the topology is final.
    lines: Option<Lines>,
    rows: Vec<AccessibilityRetainedRow>,
    fragment: Option<Fragment>,
    last_line: Option<u32>,
}

/// Where each segment of the final topology starts, and the next inserted line that has not
/// received its row.
struct Lines {
    starts: Vec<u32>,
    /// The segment holding `next_inserted`.
    run: usize,
    next_inserted: Option<u32>,
}

impl Staging {
    fn new(header: wire::Header, base: Option<Base>) -> Result<Self, ServerError> {
        let screen = match header.screen {
            wire::Screen::Primary => AccessibilityScreen::Primary,
            wire::Screen::Alternate => AccessibilityScreen::Alternate,
        };
        let screen_generation =
            usize::try_from(header.screen_generation).map_err(|_| ServerError::Protocol)?;
        let len = base.as_ref().map_or(Ok(0), |base| {
            u32::try_from(base.topology.len()).map_err(|_| ServerError::Protocol)
        })?;
        Ok(Self {
            header,
            screen,
            screen_generation,
            segments: (len > 0)
                .then_some(Segment::Base { start: 0, len })
                .into_iter()
                .collect(),
            base,
            len,
            spliced: false,
            lines: None,
            rows: Vec::new(),
            fragment: None,
            last_line: None,
        })
    }

    fn stage(
        &mut self,
        splices: Vec<wire::Splice>,
        rows: Vec<wire::RowUpdate>,
    ) -> Result<(), ServerError> {
        for splice in splices {
            // Every splice of a revision precedes its rows.
            check(self.lines.is_none())?;
            self.splice(splice)?;
        }
        for row in rows {
            self.row(row)?;
        }
        Ok(())
    }

    fn splice(&mut self, splice: wire::Splice) -> Result<(), ServerError> {
        let end = splice
            .at
            .checked_add(splice.remove)
            .filter(|end| *end <= self.len)
            .ok_or(ServerError::Protocol)?;
        let inserted = splice
            .insert
            .iter()
            .map(|run| u32::from(run.count))
            .sum::<u32>();
        self.len = (self.len - splice.remove)
            .checked_add(inserted)
            .ok_or(ServerError::Protocol)?;
        let first = self.split(splice.at);
        let last = self.split(end);
        self.segments
            .splice(first..last, splice.insert.into_iter().map(Segment::Run));
        self.spliced = true;
        Ok(())
    }

    /// The index of the segment that starts at `line`, splitting the one that holds it.
    fn split(&mut self, line: u32) -> usize {
        let mut start = 0;
        for index in 0..self.segments.len() {
            let len = self.segments[index].len();
            if line == start {
                return index;
            }
            if line < start + len {
                let (before, after) = self.segments[index].split(line - start);
                self.segments[index] = before;
                self.segments.insert(index + 1, after);
                return index + 1;
            }
            start += len;
        }
        self.segments.len()
    }

    /// Fixes the topology at the first row or at the tail.
    fn lines(&mut self) -> &mut Lines {
        if self.lines.is_none() {
            let mut starts = Vec::with_capacity(self.segments.len());
            let mut start = 0;
            let mut inserted = 0_usize;
            for segment in &self.segments {
                starts.push(start);
                start += segment.len();
                if matches!(segment, Segment::Run(_)) {
                    inserted += segment.len() as usize;
                }
            }
            // Every inserted line receives a row, and a complete document inserts every line.
            self.rows.reserve_exact(inserted);
            let run = self.next_run(0);
            let next_inserted = starts.get(run).copied();
            self.lines = Some(Lines {
                starts,
                run,
                next_inserted,
            });
        }
        self.lines.as_mut().expect("the lines were just set")
    }

    /// The first segment at or after `from` that the revision inserted.
    fn next_run(&self, from: usize) -> usize {
        from + self.segments[from..]
            .iter()
            .take_while(|segment| matches!(segment, Segment::Base { .. }))
            .count()
    }

    fn row(&mut self, row: wire::RowUpdate) -> Result<(), ServerError> {
        self.lines();
        if let Some(mut fragment) = self.fragment.take() {
            // A row's fragments are consecutive and continue its columns and text.
            let cells = row.cells.as_deref().ok_or(ServerError::Protocol)?;
            check(
                row.line == fragment.line
                    && row.revision == fragment.revision
                    && row.soft_wrapped == fragment.soft_wrapped
                    && cells
                        .first()
                        .is_none_or(|cell| cell.column >= fragment.next_column)
                    && fragment.text.len() + row.text.len() <= wire::MAX_ROW_TEXT_BYTES,
            )?;
            fragment.text.push_str(&row.text);
            fragment.cells.extend_from_slice(cells);
            fragment.next_column = cells
                .last()
                .map_or(fragment.next_column, |cell| cell.column + cell.width);
            if row.continues {
                self.fragment = Some(fragment);
                return Ok(());
            }
            let Fragment {
                line,
                revision,
                soft_wrapped,
                text,
                cells,
                ..
            } = fragment;
            let id = self.id(line);
            self.rows.push(AccessibilityRetainedRow::with_cells(
                id,
                revision,
                soft_wrapped,
                text,
                cells
                    .iter()
                    .map(|cell| (cell.column, cell.width, cell.bytes)),
            ));
            return Ok(());
        }
        check(row.line < self.len && self.last_line.is_none_or(|last| row.line > last))?;
        self.cover(row.line)?;
        self.last_line = Some(row.line);
        if row.continues {
            check(row.text.len() <= wire::MAX_ROW_TEXT_BYTES)?;
            let cells = row.cells.ok_or(ServerError::Protocol)?;
            self.fragment = Some(Fragment {
                line: row.line,
                revision: row.revision,
                soft_wrapped: row.soft_wrapped,
                next_column: cells.last().map_or(0, |cell| cell.column + cell.width),
                text: row.text,
                cells,
            });
            return Ok(());
        }
        let id = self.id(row.line);
        self.rows.push(match row.cells {
            None => AccessibilityRetainedRow::simple(id, row.revision, row.soft_wrapped, row.text),
            Some(cells) => AccessibilityRetainedRow::with_cells(
                id,
                row.revision,
                row.soft_wrapped,
                row.text,
                cells
                    .iter()
                    .map(|cell| (cell.column, cell.width, cell.bytes)),
            ),
        });
        Ok(())
    }

    /// Notes that `line` receives a row. Refuses a row past an inserted line that received none.
    fn cover(&mut self, line: u32) -> Result<(), ServerError> {
        let lines = self.lines.as_ref().expect("rows follow the final topology");
        let Some(next) = lines.next_inserted else {
            return Ok(());
        };
        check(line <= next)?;
        if line < next {
            return Ok(());
        }
        let run = lines.run;
        let (run, next_inserted) = if line + 1 < lines.starts[run] + self.segments[run].len() {
            (run, Some(line + 1))
        } else {
            let run = self.next_run(run + 1);
            (run, lines.starts.get(run).copied())
        };
        let lines = self.lines.as_mut().expect("rows follow the final topology");
        lines.run = run;
        lines.next_inserted = next_inserted;
        Ok(())
    }

    /// The identity of the row at `line` of the final topology.
    fn id(&self, line: u32) -> AccessibilityRowId {
        let starts = &self
            .lines
            .as_ref()
            .expect("rows follow the final topology")
            .starts;
        let index = starts.partition_point(|start| *start <= line) - 1;
        self.id_in(self.segments[index], line - starts[index])
    }

    fn id_in(&self, segment: Segment, offset: u32) -> AccessibilityRowId {
        match segment {
            Segment::Base { start, .. } => self
                .base
                .as_ref()
                .map(|base| base.topology[(start + offset) as usize])
                .expect("only a delta keeps base lines"),
            Segment::Run(run) => AccessibilityRowId {
                screen: self.screen,
                screen_generation: self.screen_generation,
                node_serial: run.node_serial,
                page_row: run.page_row + offset as u16,
            },
        }
    }

    /// Checks the tail against the final topology and returns the whole revision.
    fn finish(
        mut self,
        tail: wire::Tail,
        applied: Option<AppliedTopology<'_>>,
    ) -> Result<AccessibilityRevision, ServerError> {
        self.lines();
        let len = self.len;
        let lines = self.lines.as_ref().expect("the lines were just set");
        // Every inserted line received its row, and no row is left unfinished.
        check(self.fragment.is_none() && lines.next_inserted.is_none())?;
        check(tail.visible_lines.end <= len)?;
        let points = [
            tail.cursor,
            tail.selection.map(|selection| selection.start),
            tail.selection.map(|selection| selection.end),
        ];
        check(points.iter().flatten().all(|point| point.line < len))?;
        // A document holds the rows of one screen and screen generation.
        let keeps_base = self
            .segments
            .iter()
            .any(|segment| matches!(segment, Segment::Base { .. }));
        if let Some(base) = &self.base
            && keeps_base
        {
            check(base.screen == self.screen && base.screen_generation == self.screen_generation)?;
        }
        let topology = (self.base.is_none() || self.spliced).then(|| {
            let mut segment = 0;
            let mut offset = 0;
            (0..len)
                .map(|_| {
                    while offset == self.segments[segment].len() {
                        segment += 1;
                        offset = 0;
                    }
                    offset += 1;
                    self.id_in(self.segments[segment], offset - 1)
                })
                .collect::<Arc<[_]>>()
        });
        let reference = |point: wire::Point| self.reference(point, applied.as_ref());
        let cursor = tail.cursor.map(reference).transpose()?;
        let selection = tail
            .selection
            .map(|selection| {
                Ok::<_, ServerError>(AccessibilitySelectionRefs {
                    start: reference(selection.start)?,
                    end: reference(selection.end)?,
                    rectangle: selection.rectangle,
                })
            })
            .transpose()?;
        Ok(AccessibilityRevision {
            screen: self.screen,
            screen_generation: self.screen_generation,
            topology,
            rows: self.rows,
            visible_lines: tail.visible_lines.start as usize..tail.visible_lines.end as usize,
            cursor,
            selection,
        })
    }

    /// The cell at `point`: the row this revision sent for its line, or the base row there.
    fn reference(
        &self,
        point: wire::Point,
        applied: Option<&AppliedTopology<'_>>,
    ) -> Result<AccessibilityCellRef, ServerError> {
        let starts = &self
            .lines
            .as_ref()
            .expect("the tail follows the final topology")
            .starts;
        let index = starts.partition_point(|start| *start <= point.line) - 1;
        let segment = self.segments[index];
        let offset = point.line - starts[index];
        let id = self.id_in(segment, offset);
        // With repeated identities the last row sent for one stands for all its lines.
        let sent = self
            .rows
            .iter()
            .rev()
            .find(|row| row.id() == id)
            .map(AccessibilityRetainedRow::revision);
        let revision = match (sent, segment) {
            (Some(revision), _) => revision,
            (None, Segment::Base { start, .. }) => applied
                .and_then(|applied| applied.row_revision((start + offset) as usize))
                .ok_or(ServerError::Protocol)?,
            (None, Segment::Run(_)) => return Err(ServerError::Protocol),
        };
        Ok(AccessibilityCellRef {
            row: id,
            row_revision: revision,
            column: point.column,
        })
    }
}

fn check(condition: bool) -> Result<(), ServerError> {
    condition.then_some(()).ok_or(ServerError::Protocol)
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

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::protocol::{
        AccessibilityMessage, Channel, RawFrame, ServerMessage, frame,
    };

    /// A Pane worker's semantic side: its channel's demand and the document it retains.
    struct Client {
        semantics: Semantics,
        retained: Mutex<RetainedSemantics>,
        state: ServerPaneState,
        demands: Vec<wire::Demand>,
    }

    impl Client {
        /// A client whose assistive technology asked for semantics, with demand started.
        fn demanded() -> Self {
            let mut client = Self {
                semantics: Semantics::new(),
                retained: Mutex::default(),
                state: ServerPaneState::new(),
                demands: Vec::new(),
            };
            client.follow(true);
            client
        }

        fn follow(&mut self, wanted: bool) {
            let demands = &mut self.demands;
            self.semantics
                .follow(wanted, |demand| {
                    demands.push(demand);
                    Ok(())
                })
                .unwrap();
        }

        /// Delivers `messages` in order, bound to `generation`, and returns the
        /// acknowledgements they produced.
        fn deliver(
            &mut self,
            messages: impl IntoIterator<Item = wire::Update>,
            generation: PresentationGeneration,
        ) -> Result<Vec<Ack>, ServerError> {
            let mut acks = Vec::new();
            for message in messages {
                acks.extend(
                    self.semantics
                        .receive(message, &self.retained, generation)?,
                );
            }
            Ok(acks)
        }

        /// What the worker hands to the view after presenting `generation`.
        fn publish(
            &mut self,
            generation: PresentationGeneration,
        ) -> Option<Arc<TerminalAccessibilityModel>> {
            self.semantics.publish(&self.retained, generation)
        }
    }

    fn header(demand_generation: u64, revision: u64, base: Option<u64>) -> wire::Header {
        wire::Header {
            demand_generation,
            revision,
            base,
            screen: wire::Screen::Primary,
            screen_generation: 1,
        }
    }

    fn run(page_row: u16, count: u16) -> wire::Run {
        wire::Run {
            node_serial: 7,
            page_row,
            count,
        }
    }

    fn id(page_row: u16) -> AccessibilityRowId {
        AccessibilityRowId {
            screen: AccessibilityScreen::Primary,
            screen_generation: 1,
            node_serial: 7,
            page_row,
        }
    }

    /// A row of one cell per character of `text`, each one column wide.
    fn row(line: u32, revision: u64, text: &str) -> wire::RowUpdate {
        cells_row(line, revision, text.chars().map(|c| (c.to_string(), 1)))
    }

    /// A row of the given cells, each with its text and width, in column order.
    fn cells_row(
        line: u32,
        revision: u64,
        cells: impl IntoIterator<Item = (String, u16)>,
    ) -> wire::RowUpdate {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut column = 0;
        for (cell, width) in cells {
            spans.push(wire::CellSpan {
                column,
                width,
                bytes: cell.len() as u16,
            });
            text.push_str(&cell);
            column += width;
        }
        wire::RowUpdate::new(line, revision, false, text, spans)
    }

    fn tail(visible_lines: Range<u32>) -> wire::Tail {
        wire::Tail {
            visible_lines,
            cursor: None,
            selection: None,
        }
    }

    /// Packs one revision into bounded messages, as the server does.
    fn revision(
        header: wire::Header,
        splices: Vec<(u32, u32, Vec<wire::Run>)>,
        rows: Vec<wire::RowUpdate>,
        tail: wire::Tail,
    ) -> Vec<wire::Update> {
        let mut packer = wire::Packer::new(header);
        let mut messages = Vec::new();
        for (at, remove, runs) in splices {
            messages.extend(packer.splice(at, remove, runs));
        }
        for row in rows {
            messages.extend(packer.row(row));
        }
        messages.push(packer.finish(tail));
        messages
    }

    /// A complete document of `texts`, one row per line, at page rows 0 and up.
    fn complete(demand_generation: u64, revision_number: u64, texts: &[&str]) -> Vec<wire::Update> {
        let count = texts.len() as u16;
        revision(
            header(demand_generation, revision_number, None),
            if count == 0 {
                Vec::new()
            } else {
                vec![(0, 0, vec![run(0, count)])]
            },
            texts
                .iter()
                .enumerate()
                .map(|(line, text)| row(line as u32, 1, text))
                .collect(),
            tail(0..u32::from(count)),
        )
    }

    fn texts(model: &TerminalAccessibilityModel) -> Vec<String> {
        model.rows().map(|row| row.text.to_owned()).collect()
    }

    fn ack(demand_generation: u64, revision: u64) -> Ack {
        Ack::Semantics {
            demand_generation,
            revision,
        }
    }

    /// Three rows, the middle one 512 combining clusters, so the revision spans at least three
    /// messages and its long row crosses each boundary as fragments.
    fn long_revision(demand_generation: u64) -> Vec<wire::Update> {
        let cluster = format!("e{}", "\u{301}".repeat(64));
        revision(
            header(demand_generation, 1, None),
            vec![(0, 0, vec![run(0, 3)])],
            vec![
                row(0, 1, "first"),
                cells_row(1, 1, std::iter::repeat_n((cluster, 1), 512)),
                row(2, 1, "last"),
            ],
            tail(0..3),
        )
    }

    #[test]
    fn demand_starts_with_a_greater_generation_each_time_and_ends_once() {
        let mut client = Client::demanded();
        client.follow(true);
        client.follow(false);
        client.follow(false);
        client.follow(true);

        assert_eq!(
            client.demands,
            [
                wire::Demand::Start { generation: 1 },
                wire::Demand::End,
                wire::Demand::Start { generation: 2 },
            ]
        );
    }

    #[test]
    fn a_complete_document_then_a_delta_keep_unchanged_row_identities() {
        let mut client = Client::demanded();
        let generation = PresentationGeneration::default();
        let acks = client
            .deliver(complete(1, 1, &["ab", "cd"]), generation)
            .unwrap();
        assert_eq!(acks, [ack(1, 1)]);
        let first = client.publish(generation).unwrap();
        assert_eq!(texts(&first), ["ab", "cd"]);

        // Row 1 changes, and one row is appended. The cursor stays on the kept row 0.
        let mut delta_tail = tail(0..3);
        delta_tail.cursor = Some(wire::Point { line: 0, column: 1 });
        let acks = client
            .deliver(
                revision(
                    header(1, 2, Some(1)),
                    vec![(2, 0, vec![run(2, 1)])],
                    vec![row(1, 2, "cX"), row(2, 1, "ef")],
                    delta_tail,
                ),
                generation,
            )
            .unwrap();

        assert_eq!(acks, [ack(1, 2)]);
        let second = client.publish(generation).unwrap();
        assert_eq!(texts(&second), ["ab", "cX", "ef"]);
        assert_eq!(
            second
                .rows()
                .map(|row| (row.id, row.revision))
                .collect::<Vec<_>>(),
            [(id(0), 1), (id(1), 2), (id(2), 1)]
        );
        assert_eq!(second.cursor_cell(), Some((0, 1)));
        assert_eq!(
            second
                .cursor_ref()
                .map(|cell| (cell.row, cell.row_revision)),
            Some((id(0), 1)),
            "a cursor on a kept line names the retained row revision"
        );
    }

    #[test]
    fn restarting_demand_discards_every_message_of_the_earlier_generation_unacknowledged() {
        let old = long_revision(1);
        assert!(old.len() >= 3, "the revision must cross several messages");
        assert!(
            old.iter().any(
                |message| message.rows.first().is_some_and(|row| row.continues)
                    || message.rows.last().is_some_and(|row| row.continues)
            ),
            "a row must cross a message boundary as fragments"
        );
        // A restart before the first message stands for a whole old document delayed behind it.
        for boundary in 0..old.len() {
            let mut client = Client::demanded();
            let generation = PresentationGeneration::default();
            assert_eq!(
                client.deliver(old[..boundary].to_vec(), generation),
                Ok(Vec::new())
            );
            client.follow(false);
            client.follow(true);

            assert_eq!(
                client.deliver(old[boundary..].to_vec(), generation),
                Ok(Vec::new()),
                "messages of generation 1 are discarded after a restart at message {boundary}"
            );
            assert!(client.publish(generation).is_none());
            assert!(
                client.publish(generation.next()).is_none(),
                "nothing was applied"
            );

            let acks = client
                .deliver(complete(2, 1, &["fresh"]), generation)
                .unwrap();
            assert_eq!(
                acks,
                [ack(2, 1)],
                "the new generation's complete document applies, even with a revision number \
                 the old generation used"
            );
            assert_eq!(texts(&client.publish(generation).unwrap()), ["fresh"]);
        }
    }

    #[test]
    fn a_message_while_demand_is_ended_is_discarded() {
        let mut client = Client::demanded();
        client.follow(false);
        assert_eq!(
            client.deliver(complete(1, 1, &["late"]), PresentationGeneration::default()),
            Ok(Vec::new())
        );
        assert!(client.publish(PresentationGeneration::default()).is_none());
    }

    #[test]
    fn a_delta_whose_base_is_not_the_applied_revision_is_a_protocol_failure() {
        let generation = PresentationGeneration::default();
        let mut client = Client::demanded();
        client.deliver(complete(1, 1, &["ab"]), generation).unwrap();
        assert_eq!(
            client.deliver(
                revision(header(1, 3, Some(2)), Vec::new(), Vec::new(), tail(0..1)),
                generation
            ),
            Err(ServerError::Protocol)
        );

        // A new generation stages nothing until its complete document begins, so the revision
        // applied in the earlier generation is no base.
        let mut client = Client::demanded();
        client.deliver(complete(1, 1, &["ab"]), generation).unwrap();
        client.follow(false);
        client.follow(true);
        assert_eq!(
            client.deliver(
                revision(header(2, 2, Some(1)), Vec::new(), Vec::new(), tail(0..1)),
                generation
            ),
            Err(ServerError::Protocol)
        );
    }

    #[test]
    fn a_revision_stays_staged_across_hidden_turns_and_applies_whole_at_its_tail() {
        let generation = PresentationGeneration::default();
        let mut client = Client::demanded();
        client
            .deliver(complete(1, 1, &["old"]), generation)
            .unwrap();
        client.publish(generation);
        let mut messages = long_revision(1);
        for message in &mut messages {
            message.header.base = Some(1);
            message.header.revision = 2;
            for splice in &mut message.splices {
                if splice.at == 0 && splice.remove == 0 {
                    splice.remove = 1;
                }
            }
        }
        let last = messages.split_off(1);

        assert_eq!(client.deliver(messages, generation), Ok(Vec::new()));
        // While the channel is hidden no message arrives. The partial revision publishes
        // nothing and the retained document is untouched.
        let hidden = client.publish(generation.next()).unwrap();
        assert_eq!(texts(&hidden), ["old"]);
        assert!(client.publish(generation.next()).is_none());

        // Shown again, the rest of the revision arrives and applies at once.
        assert_eq!(client.deliver(last, generation.next()), Ok(vec![ack(1, 2)]));
        let shown = client.publish(generation.next()).unwrap();
        assert_eq!(shown.rows().count(), 3);
        assert_eq!(shown.row(2).unwrap().text, "last");
    }

    #[test]
    fn the_channels_end_mid_revision_discards_staging_and_publishes_nothing() {
        let generation = PresentationGeneration::default();
        let mut client = Client::demanded();
        let messages = long_revision(1);
        assert_eq!(
            client.deliver(messages[..1].to_vec(), generation),
            Ok(Vec::new())
        );

        client.semantics.cancel();

        assert!(client.publish(generation).is_none());
        assert!(client.publish(generation.next()).is_none());
        // The rest of the cancelled revision is no revision of its own.
        assert_eq!(
            client.deliver(messages[1..].to_vec(), generation),
            Err(ServerError::Protocol)
        );
    }

    #[test]
    fn a_revision_whose_frame_was_coalesced_away_binds_to_the_current_presentation() {
        let mut client = Client::demanded();
        let shown = PresentationGeneration::default().next().next();
        client.deliver(complete(1, 1, &["one"]), shown).unwrap();
        client.publish(shown);
        // The frame that carried revision 2 was coalesced into a later one, and the Pane is idle:
        // revision 2 applies to the presentation already shown.
        let acks = client
            .deliver(
                revision(
                    header(1, 2, Some(1)),
                    Vec::new(),
                    vec![row(0, 2, "two")],
                    tail(0..1),
                ),
                shown,
            )
            .unwrap();

        assert_eq!(acks, [ack(1, 2)]);
        let model = client.publish(shown).unwrap();
        assert_eq!(
            (model.generation(), texts(&model)),
            (shown, vec!["two".into()])
        );
        // A later frame binds the same document again.
        let rebound = client.publish(shown.next()).unwrap();
        assert!(rebound.shares_document(&model));
        assert_eq!(rebound.generation(), shown.next());
    }

    #[test]
    fn an_idle_first_demand_applies_without_a_new_frame() {
        let mut client = Client {
            semantics: Semantics::new(),
            retained: Mutex::default(),
            state: ServerPaneState::new(),
            demands: Vec::new(),
        };
        let shown = PresentationGeneration::default().next();
        client.follow(true);
        assert_eq!(client.demands, [wire::Demand::Start { generation: 1 }]);

        let acks = client.deliver(complete(1, 4, &["idle"]), shown).unwrap();

        assert_eq!(acks, [ack(1, 4)]);
        let model = client.publish(shown).unwrap();
        assert_eq!(model.generation(), shown);
        assert_eq!(texts(&model), ["idle"]);
    }

    #[test]
    fn rows_of_the_longest_graphemes_cross_messages_with_equal_text_cells_and_utf16_ranges() {
        let combining = format!("e{}", "\u{301}".repeat(64));
        let supplementary = format!("\u{1F600}{}", "\u{E0100}".repeat(64));
        let rows: [Vec<(String, u16)>; 3] = [
            vec![(combining, 1); 512],
            vec![(supplementary.clone(), 1); 512],
            vec![(supplementary, 2); 256],
        ];
        let mut client = Client::demanded();
        let messages = revision(
            header(1, 1, None),
            vec![(0, 0, vec![run(0, 5)])],
            std::iter::once(row(0, 1, "a"))
                .chain(
                    rows.iter()
                        .enumerate()
                        .map(|(index, cells)| cells_row(index as u32 + 1, 1, cells.clone())),
                )
                .chain(std::iter::once(row(4, 1, "z")))
                .collect(),
            tail(0..5),
        );
        assert!(
            messages.iter().all(|message| message
                .rows
                .iter()
                .all(|row| row.text.len() <= wire::MAX_CELL_TEXT_BYTES * 512)),
            "no fragment exceeds a row"
        );
        assert!(messages.len() > 3, "each long row crosses messages");

        client
            .deliver(messages, PresentationGeneration::default())
            .unwrap();
        let model = client.publish(PresentationGeneration::default()).unwrap();

        for (index, cells) in rows.iter().enumerate() {
            let retained = model.row(index + 1).unwrap();
            let text = cells
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<String>();
            assert_eq!(retained.text, text, "row {index} keeps its whole text");
            let mut utf16 = 0;
            let mut column = 0;
            let expected = cells
                .iter()
                .map(|(text, width)| {
                    let units = text.encode_utf16().count();
                    let cell = (text.as_str(), utf16..utf16 + units, column..column + width);
                    utf16 += units;
                    column += width;
                    cell
                })
                .collect::<Vec<_>>();
            let actual = retained
                .cells()
                .map(|cell| (cell.text, cell.utf16, cell.columns))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "row {index} keeps its cells");
            assert_eq!(retained.len_utf16, utf16);
        }
    }

    #[test]
    fn stateful_checks_refuse_what_no_single_message_shows_is_inconsistent() {
        let generation = PresentationGeneration::default();
        let base = || {
            let mut client = Client::demanded();
            client
                .deliver(complete(1, 1, &["a", "b", "c"]), generation)
                .unwrap();
            client
        };
        let delta = |splices: Vec<wire::Splice>, rows: Vec<wire::RowUpdate>, tail: wire::Tail| {
            wire::Update {
                header: header(1, 2, Some(1)),
                splices,
                rows,
                tail: Some(tail),
            }
        };
        let splice = |at, remove, insert| wire::Splice { at, remove, insert };
        let fragment = |line, text: &str, column, continues| wire::RowUpdate {
            line,
            revision: 2,
            soft_wrapped: false,
            text: text.into(),
            cells: Some(vec![wire::CellSpan {
                column,
                width: 1,
                bytes: text.len() as u16,
            }]),
            continues,
        };
        let cases: Vec<(&str, Vec<wire::Update>)> = vec![
            (
                "a splice past the topology",
                vec![delta(
                    vec![splice(2, 2, Vec::new())],
                    Vec::new(),
                    tail(0..1),
                )],
            ),
            (
                "an inserted line without a row",
                vec![delta(
                    vec![splice(1, 0, vec![run(9, 2)])],
                    vec![row(1, 2, "x")],
                    tail(0..5),
                )],
            ),
            (
                "an inserted line repeating a kept identity without a row",
                vec![delta(
                    vec![splice(3, 0, vec![run(0, 1)])],
                    Vec::new(),
                    tail(0..4),
                )],
            ),
            (
                "a row past an inserted line that received none",
                vec![delta(
                    vec![splice(1, 0, vec![run(0, 1)])],
                    vec![row(2, 2, "x")],
                    tail(0..4),
                )],
            ),
            (
                "a row past the topology",
                vec![delta(Vec::new(), vec![row(3, 2, "x")], tail(0..3))],
            ),
            (
                "visible lines past the topology",
                vec![delta(Vec::new(), Vec::new(), tail(0..4))],
            ),
            (
                "a cursor past the topology",
                vec![delta(
                    Vec::new(),
                    Vec::new(),
                    wire::Tail {
                        visible_lines: 0..3,
                        cursor: Some(wire::Point { line: 3, column: 0 }),
                        selection: None,
                    },
                )],
            ),
            (
                "rows out of order across messages",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![row(2, 2, "x")], tail(0..3))
                    },
                    delta(Vec::new(), vec![row(1, 2, "y")], tail(0..3)),
                ],
            ),
            (
                "a splice after a row",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![row(0, 2, "x")], tail(0..3))
                    },
                    delta(vec![splice(3, 0, vec![run(9, 1)])], Vec::new(), tail(0..4)),
                ],
            ),
            (
                "a tail while a fragment continues",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![fragment(0, "x", 0, true)], tail(0..3))
                    },
                    delta(Vec::new(), Vec::new(), tail(0..3)),
                ],
            ),
            (
                "a fragment continued on another line",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![fragment(0, "x", 0, true)], tail(0..3))
                    },
                    delta(Vec::new(), vec![fragment(1, "y", 1, false)], tail(0..3)),
                ],
            ),
            (
                "a fragment that goes back a column",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![fragment(0, "x", 3, true)], tail(0..3))
                    },
                    delta(Vec::new(), vec![fragment(0, "y", 2, false)], tail(0..3)),
                ],
            ),
            (
                "a message of another revision before the tail",
                vec![
                    wire::Update {
                        tail: None,
                        ..delta(Vec::new(), vec![row(0, 2, "x")], tail(0..3))
                    },
                    wire::Update {
                        header: header(1, 3, Some(1)),
                        ..delta(Vec::new(), Vec::new(), tail(0..3))
                    },
                ],
            ),
            (
                "a delta that keeps rows of another screen",
                vec![wire::Update {
                    header: wire::Header {
                        screen: wire::Screen::Alternate,
                        ..header(1, 2, Some(1))
                    },
                    ..delta(Vec::new(), Vec::new(), tail(0..3))
                }],
            ),
        ];
        for (case, messages) in cases {
            for message in &messages {
                assert!(
                    message.validate().is_ok(),
                    "{case}: each message is valid alone"
                );
            }
            let mut client = base();
            assert_eq!(
                client.deliver(messages, generation),
                Err(ServerError::Protocol),
                "{case}"
            );
            assert!(
                client
                    .publish(generation)
                    .is_some_and(|model| texts(&model) == ["a", "b", "c"]),
                "{case}: nothing of the refused revision was published"
            );
        }
    }

    #[test]
    fn selection_resolves_only_within_the_epoch_that_received_it() {
        let mut client = Client::demanded();
        client
            .deliver(
                complete(1, 1, &["ab", "cd"]),
                PresentationGeneration::default(),
            )
            .unwrap();
        let model = client.publish(PresentationGeneration::default()).unwrap();
        let request = model.selection_request(0..2).unwrap();
        let cell = |column| wire::CellRef {
            row: wire::RowId {
                screen: wire::Screen::Primary,
                screen_generation: 1,
                node_serial: 7,
                page_row: 0,
            },
            row_revision: 1,
            column,
        };
        assert_eq!(
            client.retained.lock().unwrap().resolve_selection(&request),
            Some(Some(wire::SelectionRefs {
                start: cell(0),
                end: cell(1),
                rectangle: false,
            }))
        );

        *client.state.accessibility.lock().unwrap() =
            std::mem::take(&mut *client.retained.lock().unwrap());
        client.state.invalidate();

        assert_eq!(
            client
                .state
                .accessibility
                .lock()
                .unwrap()
                .resolve_selection(&request),
            None
        );
    }

    #[test]
    fn the_published_model_follows_a_scrolled_view_and_the_local_selection() {
        let mut client = Client::demanded();
        // Two history lines above a two-line screen.
        let mut messages = complete(1, 1, &["old0", "old1", "scr0", "scr1"]);
        messages.last_mut().unwrap().tail = Some(tail(2..4));
        client
            .deliver(messages, PresentationGeneration::default())
            .unwrap();
        let first = client.publish(PresentationGeneration::default()).unwrap();
        assert_eq!(first.visible_lines(), 2..4);
        // Scrolled back one row, with the Selection from "old1" column 1 to "scr0" column 2.
        client
            .retained
            .lock()
            .unwrap()
            .set_view(super::super::scrollback::AccessibilityView {
                scrolled: 1,
                selection: Some(((-1, 1), (0, 2))),
            });
        let next = PresentationGeneration::default().next();
        let scrolled = client.publish(next).unwrap();
        assert_eq!(
            scrolled.visible_lines(),
            1..3,
            "VoiceOver reads the rows shown"
        );
        let selection = scrolled.selection_refs().unwrap();
        assert_eq!(
            (
                selection.start.row.page_row,
                selection.start.column,
                selection.end.row.page_row,
                selection.end.column
            ),
            (1, 1, 2, 2),
            "the mouse Selection reaches the accessible Selection"
        );
        // Scrolled further than the document reaches, the view stops at its first line.
        client
            .retained
            .lock()
            .unwrap()
            .set_view(super::super::scrollback::AccessibilityView {
                scrolled: 9,
                selection: None,
            });
        let top = client.publish(next.next()).unwrap();
        assert_eq!(top.visible_lines(), 0..2);
        assert!(top.selection_refs().is_none());
    }

    /// Binds a document of `retained` rows, spread over pages of 100, to one newer Screen and
    /// returns the allocations and row-identity lookups that re-bind made.
    fn rebind_cost(
        retained: usize,
    ) -> (
        crate::terminal::testing::Allocations,
        crate::terminal::accessibility::Probes,
    ) {
        let mut client = Client::demanded();
        let pages = retained.div_ceil(100);
        let point = |line: usize| wire::Point {
            line: line as u32,
            column: 0,
        };
        let messages = revision(
            header(1, 1, None),
            vec![(
                0,
                0,
                (0..pages)
                    .map(|page| wire::Run {
                        node_serial: 1 + page as u64,
                        page_row: 0,
                        count: 100.min(retained - page * 100) as u16,
                    })
                    .collect(),
            )],
            (0..retained).map(|line| row(line as u32, 1, "x")).collect(),
            wire::Tail {
                visible_lines: (retained - 24) as u32..retained as u32,
                cursor: Some(point(retained - 1)),
                selection: Some(wire::Selection {
                    start: point(retained - 2),
                    end: point(retained - 1),
                    rectangle: false,
                }),
            },
        );
        client
            .deliver(messages, PresentationGeneration::default())
            .unwrap();
        let first = client.publish(PresentationGeneration::default()).unwrap();
        let next = PresentationGeneration::default().next();

        let before = crate::terminal::accessibility::probes();
        let (rebound, allocations) =
            crate::terminal::testing::count_allocations(|| client.publish(next));
        let after = crate::terminal::accessibility::probes();
        let probes = crate::terminal::accessibility::Probes {
            map_lookups: after.map_lookups - before.map_lookups,
            search_steps: after.search_steps - before.search_steps,
        };

        let rebound = rebound.expect("a newer Screen republishes the retained document");
        assert!(rebound.shares_document(&first));
        assert_eq!(rebound.generation(), next);
        assert_eq!(rebound.cursor_cell(), Some((retained - 1, 0)));
        assert_eq!(rebound.selection_range(), first.selection_range());
        assert!(rebound.selection_range().is_some());
        (allocations, probes)
    }

    #[test]
    fn a_rebind_costs_the_same_at_any_retained_row_count() {
        let (small_allocations, small_probes) = rebind_cost(100);
        let (large_allocations, large_probes) = rebind_cost(10_000);

        assert_eq!(
            small_allocations, large_allocations,
            "a re-bind with no changed rows allocates nothing per retained row"
        );
        assert_eq!(
            small_probes.map_lookups, large_probes.map_lookups,
            "a re-bind makes the same retained-row lookups at any retained row count"
        );
        assert!(
            small_probes.search_steps > 0,
            "the cursor and Selection resolve through identity searches"
        );
        // The large document holds 100 pages where the small one holds 1, and every page holds
        // 100 rows. The cursor and the two Selection endpoints each search the pages once. A
        // binary search over 100 pages visits at most 7 of them, so this allows twice that per
        // search. A linear search over pages, or any pass over retained rows, adds at least 99
        // steps per search.
        let pages = 100_u32;
        let per_search = 2 * (u32::BITS - (pages - 1).leading_zeros()) as usize;
        assert!(
            large_probes.search_steps <= small_probes.search_steps + 3 * per_search,
            "a re-bind's identity searches grow logarithmically with the retained row count: \
             {} steps at 100 rows, {} at 10,000",
            small_probes.search_steps,
            large_probes.search_steps
        );
    }

    /// A complete document of 10,000 rows of 80 ASCII characters on pages of 64 rows, as the
    /// connection's I/O thread decodes it from the wire, with its encoded size.
    fn decoded_ten_thousand_rows() -> (Vec<wire::Update>, usize) {
        const ROWS: usize = 10_000;
        let messages = revision(
            header(1, 1 << 40, None),
            vec![(
                0,
                0,
                (0..ROWS.div_ceil(64))
                    .map(|page| wire::Run {
                        node_serial: 1_000_000 + page as u64,
                        page_row: 0,
                        count: 64.min(ROWS - page * 64) as u16,
                    })
                    .collect(),
            )],
            (0..ROWS)
                .map(|line| row(line as u32, 1 << 40, &format!("{line:080}")))
                .collect(),
            tail(ROWS as u32 - 24..ROWS as u32),
        );
        let mut encoded = 0;
        let decoded = messages
            .into_iter()
            .map(|update| {
                let wire = frame::encode_server(&ServerMessage::Accessibility(Box::new(
                    AccessibilityMessage {
                        channel: Channel(u32::MAX),
                        update,
                    },
                )))
                .unwrap();
                encoded += wire.bytes();
                let mut assembler = frame::Assembler::default();
                let mut decoded = None;
                for bytes in &wire.frames {
                    decoded = assembler.server(RawFrame::parse(bytes).unwrap()).unwrap();
                }
                match decoded {
                    Some(ServerMessage::Accessibility(message)) => message.update,
                    _ => panic!("the message decodes as it was encoded"),
                }
            })
            .collect();
        (decoded, encoded)
    }

    #[test]
    fn applying_ten_thousand_rows_allocates_under_twice_their_encoded_size_and_retains_under_3_mib()
    {
        let (messages, encoded) = decoded_ten_thousand_rows();
        // The decoded rows' text moves into the retained rows without a copy.
        let text: usize = messages
            .iter()
            .flat_map(|message| &message.rows)
            .map(|row| row.text.len())
            .sum();
        let mut client = Client::demanded();

        let (acks, allocations) = crate::terminal::testing::count_allocations(|| {
            client.deliver(messages, PresentationGeneration::default())
        });

        assert_eq!(acks.unwrap().len(), 1);
        let model = client.publish(PresentationGeneration::default()).unwrap();
        assert_eq!(model.rows().count(), 10_000);
        assert!(
            allocations.bytes < 2 * encoded,
            "staging and applying allocated {} bytes for {encoded} encoded bytes",
            allocations.bytes
        );
        // Everything retained was allocated while applying, or is the moved text.
        assert!(
            allocations.bytes + text < 3 * 1024 * 1024,
            "10,000 retained rows hold at most {} bytes",
            allocations.bytes + text
        );
    }
}
