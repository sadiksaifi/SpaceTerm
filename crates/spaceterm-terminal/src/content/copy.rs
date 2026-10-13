//! Incremental, revision-exact Copy. Lengths and residency have separate bounds.
use super::pages::{ContentScanBudget, native_failure};
use libghostty_vt::{
    content as vt,
    style::{RgbColor, StyleColor, Underline},
};
use sha2::{Digest, Sha256};
use spaceterm_core::content_queries::*;
use std::fmt::Write as _;

const HTML_END: &str = "</div>";
#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Count,
    Plain,
    Html,
    Done,
}
/// Worker-local scan state. It retains no selection, full result or row vector.
pub struct CopyJob {
    request: CopyRequest,
    transfer: ResourceTransferId,
    phase: Phase,
    first: u32,
    last: u32,
    start_column: u16,
    end_column: u16,
    row: u32,
    column: u16,
    row_ready: bool,
    row_has_text: bool,
    row_skipped: bool,
    row_started: bool,
    previous_wrap: bool,
    blanks: [u64; 2],
    newlines: [u64; 2],
    counts: [u64; 2],
    offsets: [u64; 2],
    digests: [Sha256; 2],
    palette: [RgbColor; 256],
    foreground: RgbColor,
    background: RgbColor,
    header: String,
    header_emitted: bool,
}
#[expect(
    clippy::large_enum_variant,
    reason = "worker-local return values are boxed once at the runtime stream boundary"
)]
pub enum CopyStep {
    Pending,
    Item(CopyStreamItem),
    Complete,
}
impl CopyJob {
    fn reset(&mut self, phase: Phase) {
        self.phase = phase;
        self.row = self.first;
        self.column = 0;
        self.row_ready = false;
        self.row_has_text = false;
        self.row_skipped = false;
        self.row_started = false;
        self.previous_wrap = false;
        self.blanks = [0; 2];
        self.newlines = [0; 2];
        self.header_emitted = false;
    }
    fn index(&self) -> usize {
        usize::from(self.phase == Phase::Html)
    }
    fn selected(&self, column: u16, width: u16) -> bool {
        let rectangle = self.request.selection().rectangle();
        let start = if rectangle || self.row == self.first {
            self.start_column
        } else {
            0
        };
        let end = if rectangle || self.row == self.last {
            self.end_column
        } else {
            u16::MAX
        };
        column <= end && column.saturating_add(width) > start
    }
    fn push_count(&mut self, index: usize, length: usize) -> Result<(), ContentReadFailure> {
        let limits = CopyLogicalLimits::supported().map_err(|_| ContentReadFailure::Exhausted)?;
        let bound = if index == 0 {
            limits.plain()
        } else {
            limits.html()
        };
        self.counts[index] = self.counts[index]
            .checked_add(length as u64)
            .filter(|value| *value <= bound)
            .ok_or(ContentReadFailure::Capacity)?;
        Ok(())
    }
    fn output(&mut self, bytes: String) -> Result<CopyStep, ContentReadFailure> {
        if bytes.is_empty() {
            return Ok(CopyStep::Pending);
        }
        if bytes.len() > MAX_RESOURCE_CHUNK_BYTES {
            return Err(ContentReadFailure::Capacity);
        }
        let index = self.index();
        let offset = self.offsets[index];
        self.offsets[index] = offset
            .checked_add(bytes.len() as u64)
            .ok_or(ContentReadFailure::Exhausted)?;
        self.digests[index].update(bytes.as_bytes());
        let part = if index == 0 {
            CopyPart::Plain
        } else {
            CopyPart::Html
        };
        Ok(CopyStep::Item(CopyStreamItem::Chunk(
            CopyChunk::new(
                &self.request,
                self.transfer,
                part,
                offset,
                self.counts[index],
                bytes.into_bytes(),
            )
            .map_err(|_| ContentReadFailure::Capacity)?,
        )))
    }
}
impl crate::TerminalEmulator {
    /// The connection owner supplies a non-reused transfer identity.
    pub fn begin_copy(
        &mut self,
        request: &CopyRequest,
        transfer: ResourceTransferId,
    ) -> Result<CopyJob, ContentReadFailure> {
        self.semantic_content
            .check(&self.terminal, request.stamp())?;
        let mut start = request.selection().start();
        let mut end = request.selection().end();
        let mut start_row = self.semantic_content.resolve_cell(&self.terminal, start)?;
        let mut end_row = self.semantic_content.resolve_cell(&self.terminal, end)?;
        if (start_row.document_row, start.column) > (end_row.document_row, end.column) {
            std::mem::swap(&mut start, &mut end);
            std::mem::swap(&mut start_row, &mut end_row);
        }
        // The pinned formatter includes the next grapheme when an unwrapped
        // linear selection ends on the spacer before a wide wrapped cell.
        if request.options().unwrap_soft_wraps && !request.selection().rectangle() {
            let options = libghostty_vt::content::Options {
                row: end_row.document_row,
                column: end.column,
                max_rows: 1,
                max_cells: 2,
                max_bytes: 32 * 1024,
                ..Default::default()
            };
            let page = self
                .semantic_content
                .reader
                .read_page(&self.terminal, options)
                .or_else(|error| {
                    if matches!(error, libghostty_vt::Error::InvalidValue) && end.column > 0 {
                        self.semantic_content.reader.read_page(
                            &self.terminal,
                            libghostty_vt::content::Options {
                                column: end.column - 1,
                                ..options
                            },
                        )
                    } else {
                        Err(error)
                    }
                })
                .map_err(native_failure)?;
            if page
                .rows
                .first()
                .and_then(|row| row.cells.first())
                .is_some_and(|cell| cell.wide == 3)
            {
                let next = end_row
                    .document_row
                    .checked_add(1)
                    .ok_or(ContentReadFailure::Exhausted)?;
                let page = self
                    .semantic_content
                    .reader
                    .read_topology(
                        &self.terminal,
                        libghostty_vt::content::Options {
                            row: next,
                            max_rows: 1,
                            ..Default::default()
                        },
                    )
                    .map_err(native_failure)?;
                end_row = page
                    .rows
                    .into_iter()
                    .next()
                    .ok_or(ContentReadFailure::InvalidRange)?;
                end.column = 0;
            }
        }
        let (start_column, end_column) = if request.selection().rectangle() {
            (start.column.min(end.column), start.column.max(end.column))
        } else {
            (start.column, end.column)
        };
        let foreground = self
            .terminal
            .fg_color()
            .map_err(native_failure)?
            .unwrap_or(RgbColor {
                r: 255,
                g: 255,
                b: 255,
            });
        let background = self
            .terminal
            .bg_color()
            .map_err(native_failure)?
            .unwrap_or(RgbColor { r: 0, g: 0, b: 0 });
        let mut header = String::new();
        header
            .try_reserve(256)
            .map_err(|_| ContentReadFailure::Capacity)?;
        write!(header, "<div style=\"font-family: monospace; white-space: pre;background-color: #{:02x}{:02x}{:02x};color: #{:02x}{:02x}{:02x};\">", background.r, background.g, background.b, foreground.r, foreground.g, foreground.b).map_err(|_| ContentReadFailure::Capacity)?;
        Ok(CopyJob {
            request: *request,
            transfer,
            phase: Phase::Count,
            first: start_row.document_row,
            last: end_row.document_row,
            start_column,
            end_column,
            row: start_row.document_row,
            column: 0,
            row_ready: false,
            row_has_text: false,
            row_skipped: false,
            row_started: false,
            previous_wrap: false,
            blanks: [0; 2],
            newlines: [0; 2],
            counts: [0; 2],
            offsets: [0; 2],
            digests: [Sha256::new(), Sha256::new()],
            palette: self.terminal.color_palette().map_err(native_failure)?.0,
            foreground,
            background,
            header,
            header_emitted: false,
        })
    }
    pub fn step_copy(
        &mut self,
        job: &mut CopyJob,
        budget: ContentScanBudget,
    ) -> Result<CopyStep, ContentReadFailure> {
        if job.phase == Phase::Done {
            return Ok(CopyStep::Complete);
        }
        let result = (|| {
            self.semantic_content
                .check(&self.terminal, job.request.stamp())?;
            self.copy_quantum(job, budget)
        })();
        match result {
            Ok(step) => Ok(step),
            Err(reason) => {
                job.phase = Phase::Done;
                Ok(CopyStep::Item(CopyStreamItem::Failed {
                    stamp: job.request.stamp(),
                    reason,
                }))
            }
        }
    }
    fn copy_quantum(
        &self,
        job: &mut CopyJob,
        budget: ContentScanBudget,
    ) -> Result<CopyStep, ContentReadFailure> {
        if !job.header_emitted {
            job.header_emitted = true;
            if job.request.options().include_html {
                if job.phase == Phase::Count {
                    job.push_count(1, job.header.len() + HTML_END.len())?;
                } else if job.phase == Phase::Html {
                    return job.output(job.header.clone());
                }
            }
        }
        if job.row > job.last {
            match job.phase {
                Phase::Count => {
                    job.reset(Phase::Plain);
                    return Ok(CopyStep::Pending);
                }
                Phase::Plain if job.request.options().include_html => {
                    if job.offsets[0] != job.counts[0] {
                        return Err(ContentReadFailure::InvalidRange);
                    }
                    job.reset(Phase::Html);
                    return Ok(CopyStep::Pending);
                }
                Phase::Html if job.offsets[1] + HTML_END.len() as u64 == job.counts[1] => {
                    return job.output(HTML_END.to_owned());
                }
                Phase::Plain | Phase::Html => {
                    if job.offsets[0] != job.counts[0]
                        || (job.request.options().include_html && job.offsets[1] != job.counts[1])
                    {
                        return Err(ContentReadFailure::InvalidRange);
                    }
                    job.phase = Phase::Done;
                    let html = job
                        .request
                        .options()
                        .include_html
                        .then(|| (job.counts[1], job.digests[1].clone().finalize().into()));
                    return Ok(CopyStep::Item(CopyStreamItem::Complete(
                        CopyCompletion::new(
                            &job.request,
                            job.transfer,
                            job.counts[0],
                            job.digests[0].clone().finalize().into(),
                            html,
                        )
                        .map_err(|_| ContentReadFailure::Capacity)?,
                    )));
                }
                Phase::Done => return Ok(CopyStep::Complete),
            }
        }
        let end_row = job
            .row
            .checked_add(1)
            .ok_or(ContentReadFailure::Exhausted)?;
        let native = self
            .semantic_content
            .reader
            .read_page(
                &self.terminal,
                budget.options(job.row, job.column, end_row)?,
            )
            .map_err(native_failure)?;
        let row = native
            .rows
            .into_iter()
            .next()
            .ok_or(ContentReadFailure::InvalidRange)?;
        let mut output = String::new();
        output
            .try_reserve(32 * 1024)
            .map_err(|_| ContentReadFailure::Capacity)?;
        // A completely empty physical row is deferred, even in rich output.
        // Inspect fragments first without retaining a full row.
        if !job.row_ready {
            for (index, cell) in row.cells.iter().enumerate() {
                let column = row.start_column
                    + u16::try_from(index).map_err(|_| ContentReadFailure::Exhausted)?;
                let starts_here = (job.request.selection().rectangle() || job.row == job.first)
                    && job.start_column > 0
                    && column == job.start_column;
                if starts_here && cell.wide == 3 {
                    job.row_skipped = true;
                }
                if job.selected(column, if cell.wide == 1 { 2 } else { 1 }) && cell.has_text {
                    job.row_has_text = true;
                }
            }
            job.column = native.next_column;
            if native.complete {
                if job.row_skipped {
                    job.row = end_row;
                    job.column = 0;
                    job.row_has_text = false;
                    job.row_skipped = false;
                } else if job.row_has_text {
                    job.row_ready = true;
                    job.column = 0;
                } else {
                    for value in &mut job.newlines {
                        *value += 1;
                    }
                    job.row = end_row;
                    job.column = 0;
                }
            }
            return Ok(CopyStep::Pending);
        }
        let formats: &[usize] = match job.phase {
            Phase::Count if job.request.options().include_html => &[0, 1],
            Phase::Count | Phase::Plain => &[0],
            Phase::Html => &[1],
            Phase::Done => return Ok(CopyStep::Complete),
        };
        if !job.row_started {
            for &format in formats {
                if !job.previous_wrap || job.newlines[format] > 0 {
                    job.blanks[format] = 0;
                }
                let count = job.newlines[format];
                if job.phase == Phase::Count {
                    job.push_count(format, count as usize)?;
                } else {
                    append_repeated(&mut output, '\n', count as usize)?;
                }
                job.newlines[format] = 0;
            }
            job.row_started = true;
        }
        for (index, cell) in row.cells.iter().enumerate() {
            let column = row.start_column
                + u16::try_from(index).map_err(|_| ContentReadFailure::Exhausted)?;
            let width = match cell.wide {
                0 => 1,
                1 => 2,
                2 | 3 => continue,
                _ => return Err(ContentReadFailure::InvalidRange),
            };
            if !job.selected(column, width) {
                continue;
            }
            for &format in formats {
                let blank = if format == 1 {
                    !cell.has_text && cell.style.is_default()
                } else {
                    !cell.has_text
                        || (job.request.options().trim_trailing_spaces
                            && cell.text.starts_with(' '))
                };
                if blank {
                    job.blanks[format] += 1;
                    continue;
                }
                let text = if format == 0 {
                    cell.text.clone()
                } else {
                    format_cell(cell, job)?
                };
                if job.phase == Phase::Count {
                    job.push_count(format, job.blanks[format] as usize)?;
                    job.push_count(format, text.len())?;
                    job.blanks[format] = 0;
                } else {
                    // Drain a potentially full-history run of deferred spaces
                    // without growing a chunk or retaining a whole result.
                    let capacity = MAX_RESOURCE_CHUNK_BYTES - output.len();
                    let spaces = (job.blanks[format] as usize).min(capacity);
                    append_repeated(&mut output, ' ', spaces)?;
                    job.blanks[format] -= spaces as u64;
                    if job.blanks[format] != 0
                        || output.len() + text.len() > MAX_RESOURCE_CHUNK_BYTES
                    {
                        job.column = column;
                        return job.output(output);
                    }
                    append(&mut output, &text)?;
                }
            }
        }
        job.column = native.next_column;
        if native.complete {
            let wrapped = !job.request.selection().rectangle()
                && job.request.options().unwrap_soft_wraps
                && row.soft_wrapped;
            if !wrapped {
                for &format in formats {
                    job.newlines[format] += 1;
                }
            }
            job.previous_wrap = wrapped;
            job.row = end_row;
            job.column = 0;
            job.row_ready = false;
            job.row_has_text = false;
            job.row_skipped = false;
            job.row_started = false;
        }
        if job.phase == Phase::Count {
            Ok(CopyStep::Pending)
        } else {
            job.output(output)
        }
    }
}
fn append_repeated(out: &mut String, ch: char, count: usize) -> Result<(), ContentReadFailure> {
    if out
        .len()
        .checked_add(count)
        .is_none_or(|len| len > MAX_RESOURCE_CHUNK_BYTES)
    {
        return Err(ContentReadFailure::Capacity);
    }
    out.try_reserve(count)
        .map_err(|_| ContentReadFailure::Capacity)?;
    out.extend(std::iter::repeat_n(ch, count));
    Ok(())
}
fn append(out: &mut String, text: &str) -> Result<(), ContentReadFailure> {
    if out
        .len()
        .checked_add(text.len())
        .is_none_or(|len| len > MAX_RESOURCE_CHUNK_BYTES)
    {
        return Err(ContentReadFailure::Capacity);
    }
    out.try_reserve(text.len())
        .map_err(|_| ContentReadFailure::Capacity)?;
    out.push_str(text);
    Ok(())
}
fn escape(out: &mut String, text: &str) -> Result<(), ContentReadFailure> {
    for ch in text.chars() {
        match ch {
            '&' => append(out, "&amp;")?,
            '<' => append(out, "&lt;")?,
            '>' => append(out, "&gt;")?,
            '"' => append(out, "&quot;")?,
            '\'' => append(out, "&#39;")?,
            _ => {
                let mut bytes = [0; 4];
                append(out, ch.encode_utf8(&mut bytes))?;
            }
        }
    }
    Ok(())
}
fn color(value: StyleColor, job: &CopyJob, default: RgbColor) -> RgbColor {
    match value {
        StyleColor::None => default,
        StyleColor::Palette(index) => job.palette[index.0 as usize],
        StyleColor::Rgb(value) => value,
    }
}
fn css_color(out: &mut String, name: &str, value: RgbColor) -> Result<(), ContentReadFailure> {
    write!(
        out,
        "{name}: #{:02x}{:02x}{:02x};",
        value.r, value.g, value.b
    )
    .map_err(|_| ContentReadFailure::Capacity)
}
fn format_cell(cell: &vt::Cell, job: &CopyJob) -> Result<String, ContentReadFailure> {
    let mut out = String::new();
    let maximum = cell
        .text
        .len()
        .checked_mul(6)
        .and_then(|len| len.checked_add(cell.hyperlink.as_ref().map_or(0, |link| 6 * link.len())))
        .and_then(|len| {
            len.checked_add(
                (COPY_STYLE_MARKUP_BYTES_PER_CELL + COPY_LINK_MARKUP_BYTES_PER_CELL) as usize,
            )
        })
        .ok_or(ContentReadFailure::Exhausted)?;
    out.try_reserve(maximum)
        .map_err(|_| ContentReadFailure::Capacity)?;
    if let Some(link) = &cell.hyperlink {
        append(&mut out, "<a href=\"")?;
        escape(&mut out, link)?;
        append(&mut out, "\">")?;
    }
    let style = cell.style;
    let styled = !style.is_default();
    if styled {
        append(&mut out, "<span style=\"")?;
        let mut fg = color(style.fg_color, job, job.foreground);
        let mut bg = color(style.bg_color, job, job.background);
        if style.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.fg_color != StyleColor::None || style.inverse {
            css_color(&mut out, "color", fg)?;
        }
        if style.bg_color != StyleColor::None || style.inverse {
            css_color(&mut out, "background-color", bg)?;
        }
        if style.bold {
            append(&mut out, "font-weight: bold;")?;
        }
        if style.italic {
            append(&mut out, "font-style: italic;")?;
        }
        if style.faint {
            append(&mut out, "opacity: 0.5;")?;
        }
        if style.invisible {
            append(&mut out, "visibility: hidden;")?;
        }
        if style.underline != Underline::None
            || style.strikethrough
            || style.overline
            || style.blink
        {
            append(&mut out, "text-decoration-line:")?;
            if style.underline != Underline::None {
                append(&mut out, " underline")?;
            }
            if style.strikethrough {
                append(&mut out, " line-through")?;
            }
            if style.overline {
                append(&mut out, " overline")?;
            }
            if style.blink {
                append(&mut out, " blink")?;
            }
            append(&mut out, ";")?;
            append(
                &mut out,
                match style.underline {
                    Underline::Double => "text-decoration-style: double;",
                    Underline::Curly => "text-decoration-style: wavy;",
                    Underline::Dotted => "text-decoration-style: dotted;",
                    Underline::Dashed => "text-decoration-style: dashed;",
                    _ => "",
                },
            )?;
            if style.underline_color != StyleColor::None {
                css_color(
                    &mut out,
                    "text-decoration-color",
                    color(style.underline_color, job, fg),
                )?;
            }
        }
        append(&mut out, "\">")?;
    }
    escape(&mut out, &cell.text)?;
    if styled {
        append(&mut out, "</span>")?;
    }
    if cell.hyperlink.is_some() {
        append(&mut out, "</a>")?;
    }
    if out.len() > maximum {
        return Err(ContentReadFailure::Capacity);
    }
    Ok(out)
}
