//! `pane read`: observe a Pane's screen and Scrollback, optionally after a wait.
//!
//! Reading opens an observer channel, which never claims, resizes, starts a shell, focuses or
//! receives effects, and scrolls nobody's view. A capture freezes one history and one screen,
//! then pages history rows behind it. A history epoch that changes while paging is captured
//! again once; a second change fails with `history_reset`.
use super::channel::{self, Channel, Event, page_styles, render};
use super::cursor::Cursor;
use super::{Deadline, Done, PaneTarget, hex, link_failure, session_failure, wait};
use crate::args::{ReadArgs, ReadFormat};
use crate::contract::{
    Code, Coverage, CursorPosition, Details, Failure, Format, Gap, GapReason, HistoryDetails,
    HistoryRef, Next, PaneRead, Row, Rows, Screen, ScreenSize, SessionStateName, UsageDetails,
    UsageKind, Wait, Warning, WarningCode, WarningDetails,
};
use spaceterm_server::connection::link::RequestError;
use spaceterm_server::protocol::limits::MAX_PAGE_ROWS;
use spaceterm_server::protocol::screen::History;
use spaceterm_server::protocol::{ObserveReply, Operation, Refusal, ReplyBody, SessionState};
use std::time::{Duration, Instant};

/// `--rows` without a value.
pub const DEFAULT_ROWS: u64 = 200;
/// `--max-bytes` without a value: 1 MiB.
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
/// A plain read gives up after this long.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// `--wait-regex` patterns compile to at most this much.
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

/// Which rows a capture returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The screen only.
    Screen,
    /// The last `rows` history rows, then the screen.
    Tail(u64),
    /// Up to `rows` rows ending before `row`.
    Before { row: u64, rows: u64 },
    /// Rows from `row` to the screen, then the screen.
    After { row: u64, rows: u64 },
    /// Rows `start..end`, then the screen. A run's output.
    Range { start: u64, end: u64 },
}

/// One frozen history and screen.
#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) history: History,
    pub(crate) screen: Screen,
}

/// A page's first row, and each row's text and whether it soft-wraps.
pub(crate) type PageRows = (u64, Vec<(String, bool)>);

/// Where a capture reads from.
pub(crate) trait Source {
    /// The current history and screen, frozen together.
    fn snapshot(&mut self, format: Format) -> Result<Snapshot, Failure>;
    /// Up to `rows` rows from `start` of `epoch`, and where they start. `None` when the epoch
    /// ended or the alternate screen hides history.
    fn page(
        &mut self,
        epoch: u64,
        start: u64,
        rows: u16,
        format: Format,
    ) -> Result<Option<PageRows>, Failure>;
}

/// What a capture read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    pub(crate) history: History,
    pub(crate) rows: Vec<Row>,
    pub(crate) screen: Option<Screen>,
    /// The first row read, or where reading would have started.
    pub(crate) first: u64,
    /// The cursor `--after` continues from.
    pub(crate) after: u64,
    pub(crate) missing_rows: u64,
    pub(crate) truncated: bool,
}

/// Reads `mode` from `source`. `epoch` fences a cursor's history epoch.
pub(crate) fn capture(
    source: &mut impl Source,
    mode: Mode,
    epoch: Option<u64>,
    max_bytes: usize,
    format: Format,
) -> Result<Capture, Failure> {
    let mut latest = None;
    for _ in 0..2 {
        let snapshot = source.snapshot(format)?;
        let history = snapshot.history;
        latest = Some(history);
        if epoch.is_some_and(|epoch| epoch != history.epoch) {
            return Err(history_reset(history));
        }
        let needs_history = !matches!(mode, Mode::Screen | Mode::Tail(_));
        if needs_history && snapshot.screen.alternate_screen {
            return Err(Failure::new(Code::HistoryHidden));
        }
        let (requested, end) = match mode {
            Mode::Screen => (history.total, history.total),
            Mode::Tail(rows) => (history.total.saturating_sub(rows), history.total),
            Mode::Before { row, rows } => {
                let end = row.min(history.total);
                (end.saturating_sub(rows), end)
            }
            Mode::After { row, .. } => (row, history.total.max(row)),
            Mode::Range { start, end } => (start, end.min(history.total).max(start)),
        };
        // Tail and before ask only for retained rows; the others report what was discarded.
        let reports_gap = matches!(mode, Mode::After { .. } | Mode::Range { .. });
        let start = requested.max(history.oldest).min(end);
        let mut missing_rows = if reports_gap {
            start.saturating_sub(requested)
        } else {
            0
        };
        let end = match mode {
            Mode::After { rows, .. } => end.min(start.saturating_add(rows)),
            _ => end,
        };
        let Some(read) = read_rows(source, history.epoch, start, end, format)? else {
            continue;
        };
        let (first, mut rows, discarded) = read;
        if reports_gap {
            missing_rows += discarded;
        }
        let row_capped = matches!(mode, Mode::After { .. }) && end < history.total;
        let screen = match mode {
            Mode::Before { .. } => None,
            _ => Some(snapshot.screen),
        };
        let screen_bytes = screen.as_ref().map_or(0, |screen| {
            screen.rows.iter().map(|row| row.len() + 1).sum::<usize>()
        });
        let budget = max_bytes.saturating_sub(screen_bytes);
        let oldest_first = matches!(mode, Mode::After { .. } | Mode::Range { .. });
        let byte_capped = cap(&mut rows, budget, oldest_first);
        let truncated = row_capped || byte_capped;
        let first = rows.first().map_or(first, |row| row.row);
        let after = match mode {
            Mode::Before { row, .. } => row,
            Mode::After { .. } if truncated => rows.last().map_or(start, |row| row.row + 1),
            Mode::Range { end, .. } => rows.last().map_or(end.min(start), |row| row.row + 1),
            _ => history.total,
        };
        // `--after` omits the screen once rows were capped, so it never skips them.
        let screen = screen.filter(|_| !(matches!(mode, Mode::After { .. }) && truncated));
        return Ok(Capture {
            history,
            rows,
            screen,
            first,
            after,
            missing_rows,
            truncated,
        });
    }
    Err(history_reset(latest.unwrap_or_default()))
}

/// Reads rows `start..end` in pages. Returns the first row read, the rows, and how many rows
/// the server discarded while reading. `None` when the epoch ended.
fn read_rows(
    source: &mut impl Source,
    epoch: u64,
    start: u64,
    end: u64,
    format: Format,
) -> Result<Option<(u64, Vec<Row>, u64)>, Failure> {
    let mut rows = Vec::new();
    let mut next = start;
    let mut first = None;
    let mut discarded = 0;
    while next < end {
        let count = u16::try_from((end - next).min(u64::from(MAX_PAGE_ROWS))).unwrap_or(1);
        let Some((page_start, page)) = source.page(epoch, next, count, format)? else {
            return Ok(None);
        };
        discarded += page_start.saturating_sub(next);
        first.get_or_insert(page_start);
        let read = page.len() as u64;
        for (offset, (text, wrapped)) in page.into_iter().enumerate() {
            let row = page_start + offset as u64;
            if row < end {
                rows.push(Row { row, text, wrapped });
            }
        }
        if read == 0 {
            break;
        }
        next = page_start + read;
    }
    Ok(Some((first.unwrap_or(start), rows, discarded)))
}

/// Drops rows beyond `budget` bytes: the newest when `oldest_first`, else the oldest.
fn cap(rows: &mut Vec<Row>, budget: usize, oldest_first: bool) -> bool {
    let mut used = 0;
    let keep = if oldest_first {
        rows.iter()
            .take_while(|row| {
                used += row.text.len() + 1;
                used <= budget
            })
            .count()
    } else {
        rows.iter()
            .rev()
            .take_while(|row| {
                used += row.text.len() + 1;
                used <= budget
            })
            .count()
    };
    let dropped = rows.len() - keep;
    if oldest_first {
        rows.truncate(keep);
    } else {
        rows.drain(..dropped);
    }
    dropped > 0
}

pub(crate) fn history_reset(history: History) -> Failure {
    Failure::with(
        Code::HistoryReset,
        Details::History(HistoryDetails {
            epoch: history.epoch,
            oldest: history.oldest,
            total: history.total,
        }),
    )
}

/// A capture source over a live channel.
pub(crate) struct Live<'a> {
    pub(crate) channel: &'a mut Channel,
    pub(crate) deadline: Deadline,
}

impl Source for Live<'_> {
    fn snapshot(&mut self, format: Format) -> Result<Snapshot, Failure> {
        // Apply every frame that already arrived, so the snapshot is current.
        let now = Deadline::after(Some(Duration::ZERO));
        while let Some(event) = self.channel.next(now)? {
            match event {
                Event::Ended(event) => return Err(channel::ended(&event)),
                Event::Closed(error) => return Err(link_failure(error)),
                _ => {}
            }
        }
        let screen = &self.channel.screen;
        Ok(Snapshot {
            history: screen.history,
            screen: Screen {
                rows: screen.render(format),
                cursor: screen
                    .cursor
                    .map_or(CursorPosition { row: 0, column: 0 }, |(row, column)| {
                        CursorPosition { row, column }
                    }),
                size: ScreenSize {
                    columns: screen.columns,
                    rows: u16::try_from(screen.rows.len()).unwrap_or(u16::MAX),
                },
                alternate_screen: screen.alternate,
            },
        })
    }

    fn page(
        &mut self,
        epoch: u64,
        start: u64,
        rows: u16,
        format: Format,
    ) -> Result<Option<PageRows>, Failure> {
        let pending = self
            .channel
            .link()
            .request(Operation::ScrollbackPage {
                channel: self.channel.number(),
                epoch,
                start,
                rows,
            })
            .map_err(link_failure)?;
        match super::wait_raw(pending, self.deadline)? {
            Ok(ReplyBody::Page(page)) => {
                let styles = page_styles(&page.styles);
                Ok(Some((
                    page.start,
                    page.rows
                        .iter()
                        .map(|row| (render(row, &styles, format), row.wrapped))
                        .collect(),
                )))
            }
            // The epoch ended, or the alternate screen hides history.
            Err(RequestError::Refused(Refusal::Stale)) => Ok(None),
            Err(error) => Err(super::request_failure(error)),
            Ok(_) => Err(Failure::new(Code::ProtocolError)),
        }
    }
}

/// The Rows of a capture, with cursors for this Pane and Session.
pub(crate) fn rows(
    capture: Capture,
    target: &PaneTarget,
    session: [u8; 32],
    format: Format,
    coverage: Coverage,
) -> Rows {
    let cursor = |row| {
        Cursor {
            server: Cursor::short(&target.server),
            pane: Cursor::short(&target.pane),
            session: Cursor::short(&session),
            epoch: capture.history.epoch,
            row,
        }
        .encode()
    };
    Rows {
        session: hex(&session),
        history: HistoryRef {
            epoch: capture.history.epoch,
            oldest: capture.history.oldest,
            total: capture.history.total,
        },
        next: Next {
            before: cursor(capture.first),
            after: cursor(capture.after),
        },
        gap: (capture.missing_rows > 0).then_some(Gap {
            reason: GapReason::Discarded,
            missing_rows: capture.missing_rows,
        }),
        truncated: capture.truncated,
        rows: capture.rows,
        screen: capture.screen,
        format,
        coverage,
    }
}

/// What a wait waits for.
enum Waiting {
    Text(String),
    Pattern(regex::Regex),
    Idle(Duration),
}

impl Waiting {
    fn of(args: &ReadArgs) -> Result<Option<Self>, Failure> {
        if let Some(text) = &args.wait_for {
            return Ok(Some(Self::Text(text.clone())));
        }
        if let Some(pattern) = &args.wait_regex {
            return regex::RegexBuilder::new(pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map(|pattern| Some(Self::Pattern(pattern)))
                .map_err(|_| {
                    Failure::with(
                        Code::InvalidArguments,
                        Details::Usage(UsageDetails {
                            argument: Some("--wait-regex".into()),
                            kind: UsageKind::InvalidValue,
                        }),
                    )
                });
        }
        Ok(args.wait_idle.map(Self::Idle))
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Text(wanted) => text.contains(wanted.as_str()),
            Self::Pattern(pattern) => pattern.is_match(text),
            Self::Idle(_) => false,
        }
    }
}

/// `pane read`.
pub fn read(
    link: &spaceterm_server::connection::link::ServerLink,
    target: &PaneTarget,
    args: &ReadArgs,
    deadline: Deadline,
) -> Done<PaneRead> {
    let empty = |target: &PaneTarget| PaneRead {
        target: target.steps.clone(),
        rows: None,
        wait: None,
    };
    let waiting = match Waiting::of(args) {
        Ok(waiting) => waiting,
        Err(failure) => return Done::failed(failure),
    };
    let deadline = if waiting.is_some() {
        deadline
    } else {
        deadline.or(DEFAULT_TIMEOUT)
    };
    let cursor = args.before.or(args.after);
    if let Some(cursor) = cursor
        && (cursor.server != Cursor::short(&target.server)
            || cursor.pane != Cursor::short(&target.pane))
    {
        return Done::failed(Failure::new(Code::CursorMismatch));
    }
    let mut channel = match Channel::open(link) {
        Ok(channel) => channel,
        Err(failure) => return Done::failed(failure),
    };
    let observed = channel
        .observe(target.pane, None)
        .and_then(|pending| wait(pending, deadline));
    let session = match observed {
        Ok(ReplyBody::Observe(ObserveReply::Observing { session, .. })) => session,
        Ok(ReplyBody::Observe(ObserveReply::NoSession { state })) => {
            let failure = match state {
                SessionState::NotStarted => {
                    session_failure(Code::SessionNotStarted, SessionStateName::NotStarted)
                }
                SessionState::Ended(_) => {
                    session_failure(Code::SessionEnded, SessionStateName::Ended)
                }
            };
            return Done {
                data: Some(empty(target)),
                warnings: Vec::new(),
                failure: Some(failure),
            };
        }
        Ok(_) => return Done::failed(Failure::new(Code::ProtocolError)),
        Err(failure) => return Done::failed(failure),
    };
    if let Some(cursor) = cursor
        && cursor.session != Cursor::short(&session)
    {
        return Done::failed(Failure::new(Code::CursorMismatch));
    }
    if let Some(prefix) = &args.session
        && !hex(&session).starts_with(prefix.as_str())
    {
        return Done::failed(Failure::new(Code::SessionChanged));
    }
    let format = match args.format {
        Some(ReadFormat::Ansi) => Format::Ansi,
        _ => Format::Text,
    };
    let started = Instant::now();
    let result = channel
        .show()
        .and_then(|()| channel.await_screen(deadline))
        .and_then(|()| {
            let start = channel.screen.history;
            let waited = match &waiting {
                Some(waiting) => Some(await_condition(&mut channel, waiting, format, deadline)?),
                None => None,
            };
            let mode = mode(args, waited.as_ref().map(|_| start.total));
            let epoch = cursor.map(|cursor| cursor.epoch).or_else(|| {
                waited
                    .as_ref()
                    .filter(|_| matches!(mode, Mode::After { .. }))
                    .map(|_| start.epoch)
            });
            let max_bytes = args
                .max_bytes
                .map_or(DEFAULT_MAX_BYTES, |bytes| bytes as usize);
            let captured = capture(
                &mut Live {
                    channel: &mut channel,
                    deadline,
                },
                mode,
                epoch,
                max_bytes,
                format,
            )?;
            Ok((captured, waited))
        });
    let (captured, waited) = match result {
        Ok(result) => result,
        Err(failure) => return Done::failed(failure),
    };
    let coalesced = waited.as_ref().is_some_and(|waited| waited.coalesced);
    let mut warnings = Vec::new();
    if captured.missing_rows > 0 {
        warnings.push(Warning::new(
            WarningCode::HistoryGap,
            WarningDetails::Gap {
                missing_rows: captured.missing_rows,
            },
        ));
    }
    if coalesced {
        warnings.push(Warning::new(
            WarningCode::CoverageCoalesced,
            WarningDetails::None,
        ));
    }
    let coverage = if coalesced {
        Coverage::Coalesced
    } else {
        Coverage::Complete
    };
    let wait = waited.map(|waited| Wait {
        matched: true,
        row: waited.row,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
    Done::ok(
        PaneRead {
            target: target.steps.clone(),
            rows: Some(rows(captured, target, session, format, coverage)),
            wait,
        },
        warnings,
    )
}

/// The capture mode of the arguments. After a wait without a mode flag, the rows written since
/// the wait began.
fn mode(args: &ReadArgs, waited_from: Option<u64>) -> Mode {
    let rows = args.rows.map_or(DEFAULT_ROWS, u64::from);
    if let Some(tail) = args.tail {
        return Mode::Tail(u64::from(tail));
    }
    if let Some(cursor) = args.before {
        return Mode::Before {
            row: cursor.row,
            rows,
        };
    }
    if let Some(cursor) = args.after {
        return Mode::After {
            row: cursor.row,
            rows,
        };
    }
    match waited_from {
        Some(row) => Mode::After { row, rows },
        None => Mode::Screen,
    }
}

/// A wait that held.
struct Waited {
    /// Where the match is: a history row, or `total` plus a screen row.
    row: Option<u64>,
    /// Some rows scrolled by unread.
    coalesced: bool,
}

/// Waits for `waiting` on every frame: the screen, and the history rows added since the wait
/// began.
fn await_condition(
    channel: &mut Channel,
    waiting: &Waiting,
    format: Format,
    deadline: Deadline,
) -> Result<Waited, Failure> {
    let start = channel.screen.history;
    let mut scanned = start.total;
    let mut coalesced = false;
    loop {
        let history = channel.screen.history;
        if history.epoch != start.epoch {
            coalesced = true;
            scanned = history.total;
        }
        if let Waiting::Idle(idle) = waiting {
            if channel.activity.elapsed() >= *idle {
                return Ok(Waited {
                    row: None,
                    coalesced,
                });
            }
        } else {
            if history.total > scanned {
                let mut source = Live { channel, deadline };
                match read_rows(&mut source, history.epoch, scanned, history.total, format)? {
                    Some((first, rows, _)) => {
                        coalesced |= first > scanned;
                        if let Some(row) = rows.iter().find(|row| waiting.matches(&row.text)) {
                            return Ok(Waited {
                                row: Some(row.row),
                                coalesced,
                            });
                        }
                    }
                    None => coalesced = true,
                }
                scanned = history.total;
            }
            if let Some(index) = channel
                .screen
                .render(format)
                .iter()
                .position(|row| waiting.matches(row))
            {
                return Ok(Waited {
                    row: Some(channel.screen.history.total + index as u64),
                    coalesced,
                });
            }
        }
        // An idle wait wakes when the quiet period would end.
        let wake = match waiting {
            Waiting::Idle(idle) => {
                let quiet = channel.activity + *idle;
                Deadline {
                    until: Some(deadline.until().map_or(quiet, |until| until.min(quiet))),
                    ..deadline
                }
            }
            _ => deadline,
        };
        match channel.next(wake)? {
            None if deadline.passed() => return Err(deadline.timeout()),
            None | Some(Event::Frame | Event::Progress) => {}
            Some(Event::Ended(_)) => return Err(Failure::new(Code::SessionChanged)),
            Some(Event::Closed(error)) => return Err(link_failure(error)),
            Some(_) => {}
        }
    }
}
