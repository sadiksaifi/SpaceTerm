//! Literal, non-overlapping matching with query-sized overlap and cell mapping.
use super::pages::{ContentScanBudget, native_failure, row_id};
use libghostty_vt::content as vt;
use spaceterm_core::{content::CellRef, content_queries::*};
use std::collections::VecDeque;

#[derive(Clone, Copy)]
struct MappedCell {
    reference: CellRef,
    width: u16,
}
/// A connection-scoped worker job, not terminal Find navigation state.
pub struct FindJob {
    request: FindRequest,
    pattern: Vec<u8>,
    prefix: Vec<usize>,
    matched: usize,
    overlap: VecDeque<MappedCell>,
    row: u32,
    column: u16,
    total_rows: u32,
    pending: Option<vt::Row>,
    cell: usize,
    byte: usize,
    fragment_complete: bool,
    blank_start: Option<(CellRef, u16)>,
    flushing: u16,
    chunks: u64,
    done: bool,
    completion_sent: bool,
}
#[expect(
    clippy::large_enum_variant,
    reason = "worker-local return values are boxed once at the runtime stream boundary"
)]
pub enum FindStep {
    Pending,
    Item(FindStreamItem),
    Complete,
}
impl FindJob {
    fn consume(
        &mut self,
        byte: u8,
        cell: MappedCell,
    ) -> Result<Option<FindMatch>, ContentReadFailure> {
        let byte = byte.to_ascii_lowercase();
        while self.matched > 0 && self.pattern[self.matched] != byte {
            self.matched = self.prefix[self.matched - 1];
            while self.overlap.len() > self.matched {
                self.overlap.pop_front();
            }
        }
        if self.pattern[self.matched] != byte {
            self.overlap.clear();
            return Ok(None);
        }
        self.overlap
            .try_reserve(1)
            .map_err(|_| ContentReadFailure::Capacity)?;
        self.overlap.push_back(cell);
        self.matched += 1;
        if self.matched != self.pattern.len() {
            return Ok(None);
        }
        let start = self
            .overlap
            .front()
            .ok_or(ContentReadFailure::InvalidRange)?
            .reference;
        let mut end = cell.reference;
        end.column = end
            .column
            .checked_add(cell.width - 1)
            .ok_or(ContentReadFailure::Exhausted)?;
        let found = FindMatch::new(self.request.stamp(), start, end)
            .map_err(|_| ContentReadFailure::InvalidRange)?;
        self.matched = 0;
        self.overlap.clear();
        Ok(Some(found))
    }
    fn boundary(&mut self) {
        self.matched = 0;
        self.overlap.clear();
        self.blank_start = None;
    }
    fn continuation(&self) -> Result<Option<FindContinuation>, ContentReadFailure> {
        if self.matched != 0 || self.byte != 0 || self.flushing != 0 || self.blank_start.is_some() {
            return Ok(None);
        }
        let Some(row) = &self.pending else {
            return Ok(None);
        };
        let mut cell = self.cell;
        while row.cells.get(cell).is_some_and(|value| value.wide >= 2) {
            cell += 1;
        }
        if cell >= row.cells.len() {
            return Ok(None);
        }
        let column = row
            .start_column
            .checked_add(u16::try_from(cell).map_err(|_| ContentReadFailure::Exhausted)?)
            .ok_or(ContentReadFailure::Exhausted)?;
        if column >= row.columns {
            return Ok(None);
        }
        let next = CellRef {
            session: self.request.stamp().base().session,
            row: row_id(row.id),
            row_revision: row.revision,
            column,
        };
        FindContinuation::new(self.request.stamp(), self.request.query(), next)
            .map(Some)
            .map_err(|_| ContentReadFailure::InvalidRange)
    }
}
impl crate::TerminalEmulator {
    pub fn begin_find(&mut self, request: &FindRequest) -> Result<FindJob, ContentReadFailure> {
        self.semantic_content
            .check(&self.terminal, request.stamp())?;
        let query = request.query().as_str();
        let mut pattern = Vec::new();
        pattern
            .try_reserve(query.len())
            .map_err(|_| ContentReadFailure::Capacity)?;
        pattern.extend(query.bytes().map(|value| value.to_ascii_lowercase()));
        let mut prefix = Vec::new();
        prefix
            .try_reserve(pattern.len())
            .map_err(|_| ContentReadFailure::Capacity)?;
        prefix.resize(pattern.len(), 0);
        for index in 1..pattern.len() {
            let mut matched = prefix[index - 1];
            while matched > 0 && pattern[index] != pattern[matched] {
                matched = prefix[matched - 1];
            }
            if pattern[index] == pattern[matched] {
                matched += 1;
            }
            prefix[index] = matched;
        }
        let (row, column) = if let Some(continuation) = request.continuation() {
            let next = continuation.next();
            let row = self.semantic_content.resolve_cell(&self.terminal, next)?;
            (row.document_row, next.column)
        } else {
            (0, 0)
        };
        let total_rows = self
            .semantic_content
            .reader
            .capture(&self.terminal)
            .map_err(native_failure)?
            .total_rows;
        Ok(FindJob {
            request: request.clone(),
            done: pattern.is_empty() || pattern.contains(&b'\n'),
            completion_sent: false,
            pattern,
            prefix,
            matched: 0,
            overlap: VecDeque::new(),
            row,
            column,
            total_rows,
            pending: None,
            cell: 0,
            byte: 0,
            fragment_complete: false,
            blank_start: None,
            flushing: 0,
            chunks: 0,
        })
    }
    pub fn step_find(
        &mut self,
        job: &mut FindJob,
        budget: ContentScanBudget,
    ) -> Result<FindStep, ContentReadFailure> {
        if job.completion_sent {
            return Ok(FindStep::Complete);
        }
        let result = (|| {
            self.semantic_content
                .check(&self.terminal, job.request.stamp())?;
            if job.done {
                job.completion_sent = true;
                return Ok(FindStep::Item(FindStreamItem::Complete {
                    stamp: job.request.stamp(),
                    chunks: job.chunks,
                }));
            }
            self.find_quantum(job, budget)
        })();
        match result {
            Ok(step) => Ok(step),
            Err(reason) => {
                job.done = true;
                job.completion_sent = true;
                job.pending = None;
                job.overlap.clear();
                Ok(FindStep::Item(FindStreamItem::Failed {
                    stamp: job.request.stamp(),
                    reason,
                }))
            }
        }
    }
    fn find_quantum(
        &self,
        job: &mut FindJob,
        budget: ContentScanBudget,
    ) -> Result<FindStep, ContentReadFailure> {
        if job.pending.is_none() {
            if job.row >= job.total_rows {
                job.done = true;
                return Ok(FindStep::Pending);
            }
            let native = self
                .semantic_content
                .reader
                .read_page(
                    &self.terminal,
                    budget.options(
                        job.row,
                        job.column,
                        job.row
                            .checked_add(1)
                            .ok_or(ContentReadFailure::Exhausted)?,
                    )?,
                )
                .map_err(native_failure)?;
            job.column = native.next_column;
            job.fragment_complete = native.complete;
            job.pending = native.rows.into_iter().next();
            job.cell = 0;
            job.byte = 0;
        }
        let mut matches = Vec::new();
        matches
            .try_reserve(MAX_FIND_CHUNK_MATCHES)
            .map_err(|_| ContentReadFailure::Capacity)?;
        let mut work = 0usize;
        while matches.len() < MAX_FIND_CHUNK_MATCHES
            && work < budget.bytes.min(MAX_FIND_CHUNK_BYTES)
        {
            let row = job
                .pending
                .as_ref()
                .ok_or(ContentReadFailure::InvalidRange)?;
            let Some(cell) = row.cells.get(job.cell) else {
                if job.fragment_complete {
                    if !row.soft_wrapped {
                        job.boundary();
                    } else {
                        job.blank_start = None;
                    }
                    job.row = job
                        .row
                        .checked_add(1)
                        .ok_or(ContentReadFailure::Exhausted)?;
                    job.column = 0;
                }
                job.pending = None;
                break;
            };
            let width = match cell.wide {
                0 => 1,
                1 => 2,
                2 | 3 => {
                    job.cell += 1;
                    continue;
                }
                _ => return Err(ContentReadFailure::InvalidRange),
            };
            let reference = CellRef {
                session: job.request.stamp().base().session,
                row: row_id(row.id),
                row_revision: row.revision,
                column: row.start_column
                    + u16::try_from(job.cell).map_err(|_| ContentReadFailure::Exhausted)?,
            };
            if !cell.has_text {
                let blank = job.blank_start.get_or_insert((reference, 0));
                blank.1 = blank
                    .1
                    .checked_add(1)
                    .ok_or(ContentReadFailure::Exhausted)?;
                job.cell += 1;
                continue;
            }
            if job.byte == 0 && job.flushing == 0 {
                job.flushing = job.blank_start.map_or(0, |(_, count)| count);
            }
            if job.flushing > 0 {
                let (mut blank, count) = job.blank_start.ok_or(ContentReadFailure::InvalidRange)?;
                blank.column = blank
                    .column
                    .checked_add(count - job.flushing)
                    .ok_or(ContentReadFailure::Exhausted)?;
                if let Some(found) = job.consume(
                    b' ',
                    MappedCell {
                        reference: blank,
                        width: 1,
                    },
                )? {
                    matches.push(found);
                }
                job.flushing -= 1;
                if job.flushing == 0 {
                    job.blank_start = None;
                }
                work += 1;
                continue;
            }
            let byte = cell
                .text
                .as_bytes()
                .get(job.byte)
                .copied()
                .ok_or(ContentReadFailure::InvalidRange)?;
            let text_len = cell.text.len();
            if let Some(found) = job.consume(byte, MappedCell { reference, width })? {
                matches.push(found);
            }
            job.byte += 1;
            work += 1;
            if job.byte == text_len {
                job.byte = 0;
                job.cell += 1;
            }
        }
        if matches.is_empty() {
            return Ok(FindStep::Pending);
        }
        let continuation = job.continuation()?;
        let index = job.chunks;
        job.chunks = job
            .chunks
            .checked_add(1)
            .ok_or(ContentReadFailure::Exhausted)?;
        let chunk = FindChunk::new(&job.request, index, matches, continuation)
            .map_err(|_| ContentReadFailure::Capacity)?;
        Ok(FindStep::Item(FindStreamItem::Chunk(Box::new(chunk))))
    }
}
