//! A Pane's semantic revisions, and their encoding into pushed updates.
//!
//! The Pane keeps its latest model and revision while its controlling channel demands them. The
//! connection's outbox holds the acknowledged model and the revision in flight, and its I/O
//! thread pulls one message at a time from [`Captured::encode`]: topology splices from the
//! common prefix and suffix of the two row sequences, then the rows that are new or changed, then
//! the tail. Nothing here starts a thread or holds an encoded document; the encoder holds at most
//! the messages one [`wire::Packer`] call returns.
//! [ADR 0062](../../../docs/adr/0062-push-terminal-semantics-as-binary-updates.md) owns the
//! exchange.
use super::*;
use crate::terminal::accessibility::AccessibilityRowView;
use pane::accessibility as wire;
use spaceterm_server::diagnostics::TerminalCounters;
use spaceterm_server::connection::outbox::{SemanticEncoding, SemanticTicket, Semantics};
use std::collections::VecDeque;
use std::ops::Range;

pub(super) struct Accessibility {
    latest: Option<Arc<TerminalAccessibilityModel>>,
    revision: u64,
    /// Whether the controlling channel demands semantics.
    demanded: bool,
    /// The revision last handed to the controlling channel. `None` hands the latest model again.
    handed: Option<u64>,
    resetting: bool,
    minimum_generation: Option<PresentationGeneration>,
    /// Counts each encoded revision and its bytes.
    counters: Arc<TerminalCounters>,
}

impl Accessibility {
    pub(super) fn new(counters: Arc<TerminalCounters>) -> Self {
        Self {
            latest: None,
            revision: 0,
            demanded: false,
            handed: None,
            resetting: false,
            minimum_generation: None,
            counters,
        }
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn demanded(&self) -> bool {
        self.demanded
    }

    /// Starts demand. The next [`Self::take_changed`] hands the latest model, which the channel
    /// sends as its new generation's complete document.
    pub(super) fn start(&mut self) {
        self.demanded = true;
        self.handed = None;
    }

    /// Ends demand and drops the latest model. Models published before the Terminal Session
    /// released its store are ignored; the next start hands the first model built after it.
    pub(super) fn end(&mut self) {
        self.demanded = false;
        self.latest = None;
        self.handed = None;
    }

    /// The latest model, if demand stands and the channel was not handed this revision yet.
    pub(super) fn take_changed(&mut self) -> Option<Arc<dyn Semantics>> {
        if !self.demanded || self.handed == Some(self.revision) {
            return None;
        }
        let model = Arc::clone(self.latest.as_ref()?);
        self.handed = Some(self.revision);
        Some(Arc::new(Captured {
            revision: self.revision,
            model,
            counters: Arc::clone(&self.counters),
        }))
    }

    /// Takes the Terminal Session's newest model while demand stands. The revision advances when
    /// the document, the visible lines, the cursor, the Selection or the screen changed.
    pub(super) fn update(&mut self, model: Arc<TerminalAccessibilityModel>) {
        if !self.demanded
            || self.resetting
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
                || old.selection_refs() != model.selection_refs()
                || old.active_screen() != model.active_screen()
        }) {
            self.revision = self.revision.saturating_add(1);
        }
        self.latest = Some(model);
    }

    /// Forgets the model and demand when control moves to another lease. Models the Terminal
    /// Session published before the reset are ignored until one binds to a later screen.
    pub(super) fn reset(&mut self) {
        self.end();
        self.resetting = true;
    }

    pub(super) fn bind_reset_screen(&mut self, generation: PresentationGeneration) {
        if self.resetting {
            self.minimum_generation = Some(generation);
            self.resetting = false;
        }
    }
}

fn screen(screen: AccessibilityScreen) -> wire::Screen {
    match screen {
        AccessibilityScreen::Primary => wire::Screen::Primary,
        AccessibilityScreen::Alternate => wire::Screen::Alternate,
    }
}

pub(super) fn decode_selection(
    selection: wire::SelectionRefs,
) -> Result<crate::terminal::accessibility::AccessibilitySelectionRefs, ()> {
    // The native selection setter accepts linear endpoints only.
    if selection.rectangle {
        return Err(());
    }
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

/// A model at one semantic revision, captured for the channel's outbox.
pub(super) struct Captured {
    revision: u64,
    model: Arc<TerminalAccessibilityModel>,
    counters: Arc<TerminalCounters>,
}

impl Semantics for Captured {
    fn revision(&self) -> u64 {
        self.revision
    }

    fn encode(
        self: Arc<Self>,
        ticket: SemanticTicket,
        base: Option<Arc<dyn Semantics>>,
    ) -> Option<Box<dyn SemanticEncoding>> {
        let base = base.and_then(|base| {
            (base as Arc<dyn std::any::Any + Send + Sync>)
                .downcast::<Self>()
                .ok()
        });
        Revision::new(self, ticket, base).map(|revision| Box::new(revision) as _)
    }
}

/// Removes `remove` rows at line `at`, then inserts the new model's rows `insert` there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Splice {
    at: usize,
    remove: usize,
    insert: Range<usize>,
}

/// New lines whose rows are the base's rows from `base`, positionally.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    lines: Range<usize>,
    base: usize,
}

/// How the new topology derives from the base's: at most three splices and the kept stretches
/// between them, both in line order.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    splices: Vec<Splice>,
    kept: Vec<Kept>,
}

impl Plan {
    /// Splices from the common prefix and suffix of the two row sequences. Within the rest, it
    /// finds where the new rows meet the base again, so Scrollback that trims at the top while
    /// output appends at the bottom costs two small splices instead of the whole document.
    fn new(base: &TerminalAccessibilityModel, new: &TerminalAccessibilityModel) -> Self {
        let (base_rows, new_rows) = (count(base), count(new));
        let mut plan = Self::default();
        let prefix = common(base, 0, new, 0, base_rows.min(new_rows));
        let limit = base_rows.min(new_rows) - prefix;
        let mut suffix = 0;
        while suffix < limit && id(base, base_rows - 1 - suffix) == id(new, new_rows - 1 - suffix) {
            suffix += 1;
        }
        plan.keep(0..prefix, 0);
        let (base_end, new_end) = (base_rows - suffix, new_rows - suffix);
        plan.middle(base, prefix..base_end, new, prefix..new_end);
        plan.keep(new_end..new_rows, base_end);
        plan
    }

    fn middle(
        &mut self,
        base: &TerminalAccessibilityModel,
        old: Range<usize>,
        new_model: &TerminalAccessibilityModel,
        new: Range<usize>,
    ) {
        if old.is_empty() || new.is_empty() {
            self.splice(new.start, old.len(), new);
            return;
        }
        let first = id(new_model, new.start);
        if let Some(found) = (old.start + 1..old.end).find(|line| id(base, *line) == first) {
            // Rows left the topology before the first new one: Scrollback trimmed them.
            self.splice(new.start, found - old.start, new.start..new.start);
            let kept = common(
                base,
                found,
                new_model,
                new.start,
                (old.end - found).min(new.len()),
            );
            self.keep(new.start..new.start + kept, found);
            self.splice(
                new.start + kept,
                old.end - found - kept,
                new.start + kept..new.end,
            );
            return;
        }
        let first = id(base, old.start);
        if let Some(found) = (new.start + 1..new.end).find(|line| id(new_model, *line) == first) {
            // Rows joined the topology before the first old one.
            self.splice(new.start, 0, new.start..found);
            let kept = common(
                base,
                old.start,
                new_model,
                found,
                old.len().min(new.end - found),
            );
            self.keep(found..found + kept, old.start);
            self.splice(found + kept, old.len() - kept, found + kept..new.end);
            return;
        }
        self.splice(new.start, old.len(), new);
    }

    fn splice(&mut self, at: usize, remove: usize, insert: Range<usize>) {
        if remove > 0 || !insert.is_empty() {
            self.splices.push(Splice { at, remove, insert });
        }
    }

    fn keep(&mut self, lines: Range<usize>, base: usize) {
        if !lines.is_empty() {
            self.kept.push(Kept { lines, base });
        }
    }
}

/// How many rows a model holds, found in logarithmic lookups.
fn count(model: &TerminalAccessibilityModel) -> usize {
    let mut high = 1;
    while model.row(high - 1).is_some() {
        high *= 2;
    }
    let (mut low, mut high) = (high / 2, high - 1);
    // Every line below `low` exists, and line `high` does not.
    while low < high {
        let middle = low + (high - low) / 2;
        if model.row(middle).is_some() {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

fn id(model: &TerminalAccessibilityModel, line: usize) -> Option<AccessibilityRowId> {
    model.row(line).map(|row| row.id)
}

/// How many rows from `old` in the base and `new` in the new model carry the same identities,
/// up to `limit`.
fn common(
    base: &TerminalAccessibilityModel,
    old: usize,
    new_model: &TerminalAccessibilityModel,
    new: usize,
    limit: usize,
) -> usize {
    (0..limit)
        .find(|offset| id(base, old + offset) != id(new_model, new + offset))
        .unwrap_or(limit)
}

/// The new model's rows `lines`, as runs of consecutive page rows of one page node.
fn runs(
    model: &TerminalAccessibilityModel,
    lines: Range<usize>,
) -> impl Iterator<Item = wire::Run> {
    let mut lines = lines.peekable();
    std::iter::from_fn(move || {
        let first = model.row(lines.next()?)?.id;
        let mut run = wire::Run {
            node_serial: first.node_serial,
            page_row: first.page_row,
            count: 1,
        };
        while let Some(line) = lines.peek()
            && run.count < u16::MAX
            && model.row(*line).is_some_and(|row| {
                row.id.node_serial == run.node_serial
                    && run.page_row.checked_add(run.count) == Some(row.id.page_row)
            })
        {
            lines.next();
            run.count += 1;
        }
        Some(run)
    })
}

fn row_update(line: usize, row: &AccessibilityRowView<'_>) -> wire::RowUpdate {
    wire::RowUpdate::new(
        line as u32,
        row.revision,
        row.soft_wrapped,
        row.text.to_owned(),
        row.cells().map(|cell| wire::CellSpan {
            column: cell.columns.start,
            width: cell.columns.end - cell.columns.start,
            bytes: cell.text.len() as u16,
        }),
    )
}

/// The visible lines, cursor and Selection, as lines and columns of the model's topology.
fn tail(model: &TerminalAccessibilityModel) -> wire::Tail {
    let visible = model.visible_lines();
    wire::Tail {
        visible_lines: visible.start as u32..visible.end as u32,
        // Only a cursor in a retained row revision names a cell, as a Selection endpoint does.
        cursor: model
            .cursor_cell()
            .filter(|_| model.cursor_ref().is_some())
            .map(|(line, column)| wire::Point {
                line: line as u32,
                column,
            }),
        selection: selection(model),
    }
}

fn selection(model: &TerminalAccessibilityModel) -> Option<wire::Selection> {
    // The model publishes Selection endpoints only while both name retained row revisions.
    let refs = model.selection_refs()?;
    let point = |reference: AccessibilityCellRef| {
        model.line_for_row(reference.row).map(|line| wire::Point {
            line: line as u32,
            column: reference.column,
        })
    };
    Some(wire::Selection {
        start: point(refs.start)?,
        end: point(refs.end)?,
        rectangle: refs.rectangle,
    })
}

/// One revision, encoded one message at a time.
struct Revision {
    model: Arc<Captured>,
    base: Option<Arc<Captured>>,
    plan: Plan,
    /// The new model's row count, or 0 when the base shares its document so no row is sent.
    lines: usize,
    packer: Option<wire::Packer>,
    tail: Option<wire::Tail>,
    /// Messages the last item completed, written before more items are added.
    ready: VecDeque<wire::Update>,
    /// The next splice of `plan` to add, then the next line to consider.
    splice: usize,
    line: usize,
    /// The kept stretch at or after `line`.
    kept: usize,
    /// The bytes of the messages written so far, channels excluded.
    bytes: usize,
}

impl Revision {
    /// `None` when the model equals its base: it shares the document and the tail.
    fn new(
        model: Arc<Captured>,
        ticket: SemanticTicket,
        base: Option<Arc<Captured>>,
    ) -> Option<Self> {
        let new = &model.model;
        let tail = tail(new);
        let shared = base
            .as_ref()
            .is_some_and(|base| base.model.shares_document(new));
        if shared
            && base
                .as_ref()
                .is_some_and(|base| self::tail(&base.model) == tail)
        {
            return None;
        }
        let (plan, lines) = match &base {
            _ if shared => (Plan::default(), 0),
            Some(base) => (Plan::new(&base.model, new), count(new)),
            None => {
                let lines = count(new);
                let mut plan = Plan::default();
                plan.splice(0, 0, 0..lines);
                (plan, lines)
            }
        };
        let header = wire::Header {
            demand_generation: ticket.demand_generation,
            revision: ticket.revision,
            base: base.as_ref().and(ticket.base),
            screen: screen(new.active_screen()),
            screen_generation: new.screen_generation() as u64,
        };
        Some(Self {
            plan,
            lines,
            packer: Some(wire::Packer::new(header)),
            tail: Some(tail),
            ready: VecDeque::new(),
            splice: 0,
            line: 0,
            kept: 0,
            bytes: 0,
            model,
            base,
        })
    }

    /// Whether the row at `line` is new or changed since the base.
    fn changed(&mut self, line: usize) -> bool {
        while self
            .plan
            .kept
            .get(self.kept)
            .is_some_and(|kept| kept.lines.end <= line)
        {
            self.kept += 1;
        }
        let Some(kept) = self
            .plan
            .kept
            .get(self.kept)
            .filter(|kept| kept.lines.contains(&line))
        else {
            return true;
        };
        let old = self
            .base
            .as_ref()
            .and_then(|base| base.model.row(kept.base + line - kept.lines.start));
        let new = self.model.model.row(line);
        old.zip(new).is_none_or(|(old, new)| {
            old.revision != new.revision || old.soft_wrapped != new.soft_wrapped
        })
    }
}

impl SemanticEncoding for Revision {
    /// Counts the revision and the bytes of all its messages once its tail is encoded.
    fn next(&mut self) -> wire::Update {
        let update = self.produce();
        self.bytes += update.encoded_len();
        if update.tail.is_some() {
            self.model.counters.record_accessibility_revision(self.bytes);
        }
        update
    }
}

impl Revision {
    fn produce(&mut self) -> wire::Update {
        loop {
            if let Some(update) = self.ready.pop_front() {
                return update;
            }
            if let Some(splice) = self.plan.splices.get(self.splice).cloned() {
                self.splice += 1;
                let packer = self.packer.as_mut().expect("the revision is unfinished");
                let runs = runs(&self.model.model, splice.insert);
                self.ready
                    .extend(packer.splice(splice.at as u32, splice.remove as u32, runs));
                continue;
            }
            if self.line < self.lines {
                let line = self.line;
                self.line += 1;
                if !self.changed(line) {
                    continue;
                }
                let Some(row) = self.model.model.row(line) else {
                    continue;
                };
                let packer = self.packer.as_mut().expect("the revision is unfinished");
                self.ready.extend(packer.row(row_update(line, &row)));
                continue;
            }
            let packer = self
                .packer
                .take()
                .expect("next is not called after the tail");
            return packer.finish(self.tail.take().expect("the tail is written once"));
        }
    }
}
