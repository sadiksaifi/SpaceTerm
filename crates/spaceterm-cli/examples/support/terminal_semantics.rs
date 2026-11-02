//! One fixture-only terminal exercise shared by the native IPC and QUIC checks.
use super::support::{Pane, Screen, has_row, no_op, text_key};
use spaceterm_server::{
    ServerError,
    pane::{FocusTarget, accessibility, graphics},
    protocol::{Operation, ReplyBody},
};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Failure>;

/// Fixture diagnostics contain only static stages and bounded failure classifications.
#[derive(Clone, Copy, Debug)]
pub struct Failure {
    stage: &'static str,
    classification: &'static str,
    cause: Option<ServerError>,
}
impl Failure {
    pub fn at(mut self, stage: &'static str) -> Self {
        self.stage = stage;
        self
    }
}
impl From<&'static str> for Failure {
    fn from(classification: &'static str) -> Self {
        Self {
            stage: "semantic-assertion",
            classification,
            cause: None,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage, self.classification)?;
        if let Some(cause) = self.cause {
            write!(f, " ({cause:?})")?;
        }
        Ok(())
    }
}
impl std::error::Error for Failure {}

pub fn stage<T, E: 'static>(stage: &'static str, result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|error| {
        let any = &error as &dyn std::any::Any;
        if let Some(failure) = any.downcast_ref::<Failure>() {
            return failure.at(stage);
        }
        let server = any.downcast_ref::<ServerError>().or_else(|| {
            any.downcast_ref::<Box<dyn std::error::Error>>()?
                .downcast_ref::<ServerError>()
        });
        let classification = match server {
            Some(ServerError::StaleControl) => "stale-control",
            Some(ServerError::Unsupported) => "unsupported",
            Some(ServerError::Controlled) => "controlled-by-another-client",
            Some(ServerError::Protocol) => "transport-or-protocol",
            Some(ServerError::StorageUnavailable) => "storage-unavailable",
            Some(ServerError::UnsafeStorage) => "unsafe-storage",
            Some(ServerError::AuthenticationRefused) => "authority-refused",
            Some(ServerError::DirectoryUnavailable) => "directory-unavailable",
            Some(ServerError::SessionUnavailable) => "session-unavailable",
            Some(ServerError::NotRunning) => "server-not-running",
            Some(ServerError::Capacity) => "capacity",
            Some(_) => "server-unavailable",
            None => "fixture-operation-unavailable",
        };
        Failure {
            stage,
            classification,
            cause: server.copied(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_retains_the_bounded_direct_cause_when_context_changes() {
        let failure = stage::<(), _>(
            "quic-connect-pane",
            Err(ServerError::Direct(
                spaceterm_server::direct::DirectError::Transport,
            )),
        )
        .unwrap_err();
        let failure = stage::<(), _>("reconnect-pane", Err(failure)).unwrap_err();
        assert_eq!(
            failure.to_string(),
            "reconnect-pane: server-unavailable (Direct(Transport))"
        );
    }

    #[test]
    fn stage_never_formats_an_unclassified_native_error() {
        let error: Box<dyn std::error::Error> =
            std::io::Error::other("private native detail").into();
        let failure = stage::<(), _>("quic-connect-pane", Err(error)).unwrap_err();
        assert_eq!(
            failure.to_string(),
            "quic-connect-pane: fixture-operation-unavailable"
        );
    }
}
const ROWS: usize = 5000;
const DEADLINE: Duration = Duration::from_secs(60);
const PREFIX: &str = "ST-NATIVE-";
const DONE: &str = "ST-SEMANTICS-DONE";
const TIMING_FLAG: &str = "SPACETERM_NATIVE_INPUT_TIMING";
const TIMING_PRODUCER: &str = r#"import os,select,sys,termios,time
original = termios.tcgetattr(0)
settings = termios.tcgetattr(0)
settings[3] &= ~(termios.ICANON | termios.ECHO)
settings[6][termios.VMIN] = 1
settings[6][termios.VTIME] = 0
complete = False
try:
    termios.tcsetattr(0,termios.TCSANOW,settings)
    print("ST-INPUT-TIMING-READY",flush=True)
    until = time.monotonic() + 60
    received = bytearray()
    while len(received) < 53:
        left = until - time.monotonic()
        if left <= 0 or not select.select([0],[],[],left)[0]:
            break
        part = os.read(0,53-len(received))
        if not part:
            break
        received.extend(part)
    complete = received == bytes(97+i%26 for i in range(53))
finally:
    termios.tcsetattr(0,termios.TCSANOW,original)
print("ST-INPUT-TIMING-DONE" if complete else "ST-INPUT-TIMING-REFUSED",flush=True)
"#;

pub fn input_timing_requested() -> bool {
    std::env::var(TIMING_FLAG).as_deref() == Ok("1")
}

/// `queue` is the time a call took to queue its message on the connection. `exchange` is the time
/// until the reply to a request arrived.
#[derive(Default)]
struct Timings {
    queue: Vec<u128>,
    exchange: Vec<u128>,
}
impl Timings {
    fn note(&mut self, queue: Duration, exchange: Duration) {
        self.queue.push(queue.as_micros());
        self.exchange.push(exchange.as_micros());
    }
    fn emit(mut self, operation: &'static str, elapsed: Duration) {
        self.queue.sort_unstable();
        self.exchange.sort_unstable();
        let count = self.queue.len();
        let p95 = (count * 95).div_ceil(100) - 1;
        // stderr is separate from the QUIC driver's invitation and response framing.
        eprintln!(
            "DIAGNOSTIC input-timing operation={operation} samples={count} elapsed_us={} queue_total_us={} exchange_total_us={} queue_max_us={} exchange_max_us={} queue_p95_us={} exchange_p95_us={}",
            elapsed.as_micros(),
            self.queue.iter().sum::<u128>(),
            self.exchange.iter().sum::<u128>(),
            self.queue[count - 1],
            self.exchange[count - 1],
            self.queue[p95],
            self.exchange[p95],
        );
    }
}

/// Times a request that changes nothing, then each key followed by such a request. A key has no
/// reply, and the server handles one connection's input and requests in order, so the request's
/// reply marks when the server took the key.
fn input_timing(pane: &Pane) -> Result<()> {
    let script = format!(
        "python3 -c 'exec({})'",
        api(serde_json::to_string(TIMING_PRODUCER))?
    );
    stage("input-timing-producer", pane.command(&script))?;
    screen_at("input-timing-ready", pane, |screen| {
        has_row(screen, "ST-INPUT-TIMING-READY")
    })?;
    let round_trip = |label: &'static str, timings: &mut Timings, queue: Duration| {
        let started = Instant::now();
        let pending = stage(label, pane.link().request(no_op()))?;
        let queued = started.elapsed();
        let started = Instant::now();
        require(
            matches!(stage(label, pending.wait())?, ReplyBody::Done),
            "input timing round-trip response changed",
        )?;
        timings.note(queue + queued, started.elapsed());
        Ok::<_, Failure>(())
    };
    let deadline = Instant::now() + DEADLINE;
    let started = Instant::now();
    let mut requests = Timings::default();
    for _ in 0..8 {
        require(Instant::now() < deadline, "input timing deadline elapsed")?;
        round_trip("input-timing-round-trip", &mut requests, Duration::ZERO)?;
    }
    requests.emit("round-trip", started.elapsed());
    let started = Instant::now();
    let mut keys = Timings::default();
    for index in 0..53 {
        require(Instant::now() < deadline, "input timing deadline elapsed")?;
        let queued = Instant::now();
        stage(
            "input-timing-key",
            pane.key(1, text_key(&char::from(b'a' + index % 26).to_string())),
        )?;
        round_trip("input-timing-key", &mut keys, queued.elapsed())?;
    }
    keys.emit("key", started.elapsed());
    screen_at("input-timing-done", pane, |screen| {
        has_row(screen, "ST-INPUT-TIMING-DONE")
    })?;
    Ok(())
}

pub struct Report {
    pub copied_rows: usize,
    pub accessible_rows: usize,
    pub graphics_supported: bool,
    pub graphics_bytes: usize,
}

fn api<T, E: 'static>(result: std::result::Result<T, E>) -> Result<T> {
    stage("terminal-protocol", result)
}
fn require(condition: bool, error: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(error.into()) }
}
pub fn screen(pane: &Pane, matches: impl Fn(&Screen) -> bool) -> Result<Screen> {
    screen_at("fixture-output", pane, matches)
}
fn screen_at(
    label: &'static str,
    pane: &Pane,
    matches: impl Fn(&Screen) -> bool,
) -> Result<Screen> {
    stage(label, pane.await_screen(DEADLINE, matches))?
        .ok_or_else(|| Failure::from("terminal exercise deadline elapsed").at(label))
}
/// The demand generation this fixture starts. Its channel names no earlier one.
const DEMAND: u64 = 1;

/// The fixture's copy of the semantic document the server pushes: each line's identity, row
/// revision and text, and what the revision applied last presents.
#[derive(Clone, Default)]
struct Document {
    revision: Option<u64>,
    ids: Vec<accessibility::RowId>,
    rows: Vec<Option<(u64, String)>>,
    tail: Option<accessibility::Tail>,
}

impl Document {
    /// Applies one revision's messages, in order, to a copy of this document. Each message
    /// repeats its revision's header; the last carries the tail.
    fn apply(&self, messages: &[accessibility::Update]) -> Result<Self> {
        let header = messages
            .first()
            .ok_or("accessibility revision empty")?
            .header;
        require(
            header.base == self.revision.filter(|_| header.base.is_some()),
            "accessibility delta names another base",
        )?;
        let mut next = if header.base.is_some() {
            self.clone()
        } else {
            Self::default()
        };
        let mut fragment: Option<(u32, u64, String)> = None;
        for message in messages {
            api(message.validate())?;
            require(
                message.header == header,
                "accessibility revision interleaved",
            )?;
            for splice in &message.splices {
                let (at, remove) = (splice.at as usize, splice.remove as usize);
                require(
                    at + remove <= next.ids.len(),
                    "accessibility splice out of range",
                )?;
                let ids = splice.insert.iter().flat_map(|run| {
                    (0..run.count).map(move |offset| accessibility::RowId {
                        screen: header.screen,
                        screen_generation: header.screen_generation,
                        node_serial: run.node_serial,
                        page_row: run.page_row + offset,
                    })
                });
                let inserted = splice.insert.iter().map(|run| usize::from(run.count)).sum();
                next.ids.splice(at..at + remove, ids);
                next.rows
                    .splice(at..at + remove, std::iter::repeat_n(None, inserted));
            }
            for row in &message.rows {
                let mut text = match fragment.take() {
                    Some((line, revision, text)) => {
                        require(
                            line == row.line && revision == row.revision,
                            "accessibility fragment broken",
                        )?;
                        text
                    }
                    None => String::new(),
                };
                text.push_str(&row.text);
                if row.continues {
                    fragment = Some((row.line, row.revision, text));
                    continue;
                }
                *next
                    .rows
                    .get_mut(row.line as usize)
                    .ok_or("accessibility row out of range")? = Some((row.revision, text));
            }
        }
        let tail = messages
            .last()
            .and_then(|message| message.tail.clone())
            .ok_or("accessibility revision has no tail")?;
        require(
            fragment.is_none() && next.rows.iter().all(Option::is_some),
            "accessibility revision left a row without contents",
        )?;
        next.revision = Some(header.revision);
        next.tail = Some(tail);
        Ok(next)
    }

    fn text(&self, line: usize) -> &str {
        self.rows[line]
            .as_ref()
            .map_or("", |(_, text)| text.as_str())
    }
}

/// Receives the next whole revision, applies it to `document` and acknowledges it.
fn semantics(pane: &Pane, document: &Document, until: Instant) -> Result<Document> {
    let mut messages = Vec::new();
    loop {
        let wait = until.saturating_duration_since(Instant::now());
        let message = stage("accessibility-update", pane.await_accessibility(wait))?
            .ok_or_else(|| Failure::from("terminal exercise deadline elapsed"))?;
        require(
            message.header.demand_generation == DEMAND,
            "accessibility generation changed",
        )?;
        let last = message.tail.is_some();
        messages.push(*message);
        if last {
            break;
        }
    }
    let next = document.apply(&messages)?;
    let header = messages[0].header;
    stage(
        "accessibility-ack",
        pane.acknowledge_accessibility(header.demand_generation, header.revision),
    )?;
    Ok(next)
}
/// The caller owns the Pane. Every route carries Scrollback pages,
/// accessibility and graphics. No terminal content, remote path or native error is returned as evidence.
pub fn exercise(pane: &Pane) -> Result<Report> {
    let script = "stty -echo; python3 -c 'import base64,sys; w=sys.stdout.write; w(\"\\033[3J\\033[2J\\033[H\"); [w(\"ST-NATIVE-%05d\\n\"%i) for i in range(5000)]; d=base64.b64encode(bytes([255,0,0,255])*320*240).decode(); [w(\"\\033_G\"+(\"a=T,f=32,s=320,v=240,i=541,q=2,\" if i==0 else \"\")+\"m=%d;\"%int(i+4096<len(d))+d[i:i+4096]+\"\\033\\\\\") for i in range(0,len(d),4096)]; w(\"\\nST-SEMANTICS-DONE\\n\"); sys.stdout.flush()'";
    stage("fixture-output-command", pane.command(script))?;
    // Exercise backpressure using the same real delayed consumer on each route.
    std::thread::sleep(Duration::from_millis(300));
    let complete = screen(pane, |screen| has_row(screen, DONE))
        .map_err(|failure| failure.at("bulk-output"))?;
    let expected: Vec<_> = (0..ROWS).map(|i| format!("{PREFIX}{i:05}")).collect();
    // A client reads history through Scrollback pages; every fixture row arrives once, in order,
    // across many page boundaries.
    let history = stage("scrollback-pages", pane.scrollback(complete.history))?;
    require(
        history.len() > 2 * usize::from(spaceterm_server::protocol::limits::MAX_PAGE_ROWS),
        "bulk Scrollback did not span several pages",
    )?;
    let fixture: Vec<_> = history
        .iter()
        .chain(&complete.rows)
        .map(|row| row.trim_end())
        .filter(|row| row.starts_with(PREFIX))
        .collect();
    require(
        fixture == expected,
        "Scrollback pages changed or reordered retained rows",
    )?;
    let manifest = complete
        .graphics
        .as_ref()
        .and_then(|s| s.as_ref().ok())
        .ok_or("negotiated graphics manifest missing")?;
    api(manifest.validate())?;
    let image = manifest
        .images
        .iter()
        .find(|i| i.width == 320 && i.height == 240)
        .ok_or("bulk graphics image missing")?;
    let mut graphics_bytes = 0;
    while graphics_bytes < 320 * 240 * 4 {
        let ReplyBody::Graphics(Ok(chunk)) = stage(
            "graphics-chunk",
            pane.request(Operation::Graphics {
                channel: pane.channel(),
                image: image.key,
                offset: graphics_bytes as u64,
            }),
        )?
        else {
            return Err("bulk graphics transfer unavailable".into());
        };
        let (pixels, remainder) = chunk.bytes.as_chunks::<4>();
        require(
            chunk.image == image.key
                && chunk.offset == graphics_bytes
                && !chunk.bytes.is_empty()
                && chunk.bytes.len() <= graphics::CHUNK_BYTES
                && remainder.is_empty()
                && pixels.iter().all(|p| *p == [255, 0, 0, 255]),
            "graphics chunk binding or fixture pixels changed",
        )?;
        graphics_bytes += chunk.bytes.len();
    }
    require(
        graphics_bytes == 320 * 240 * 4,
        "graphics transfer length changed",
    )?;
    let until = Instant::now() + DEADLINE;
    stage(
        "accessibility-demand",
        pane.demand_accessibility(accessibility::Demand::Start { generation: DEMAND }),
    )?;
    // Visual and semantic publications are independent. As in the runtime's semantic
    // regressions, wait for the fixture's own semantic witness instead of the first revision.
    let mut document = Document::default();
    let (first, last) = loop {
        document = semantics(pane, &document, until)?;
        let mut fixture_rows = vec![None; ROWS];
        for line in 0..document.ids.len() {
            let text = document.text(line).trim_end();
            if let Some(index) = text
                .strip_prefix(PREFIX)
                .and_then(|n| n.parse::<usize>().ok())
            {
                require(
                    index < ROWS && text == expected[index] && fixture_rows[index].is_none(),
                    "accessible fixture row changed or duplicated",
                )?;
                fixture_rows[index] = Some(line);
            }
        }
        let matching = fixture_rows.iter().filter(|row| row.is_some()).count();
        let visible = document
            .tail
            .as_ref()
            .map_or(0..0, |tail| tail.visible_lines.clone());
        if matching == ROWS && visible.start > 0 {
            let first = fixture_rows[0].ok_or("accessible first row missing")?;
            let last = fixture_rows[ROWS - 1].ok_or("accessible last row missing")?;
            break (first, last);
        }
        if Instant::now() >= until {
            eprintln!(
                "accessibility fixture deadline: rows={}, matching={}, visible={}..{}",
                document.ids.len(),
                matching,
                visible.start,
                visible.end
            );
            return Err(Failure::from(
                "accessibility did not expose full retained fixture history",
            )
            .at("accessibility-fixture"));
        }
    };
    require(
        last.checked_sub(first) == Some(ROWS - 1),
        "accessible row order changed",
    )?;
    let cell = |line: usize, column: u16| {
        let (revision, _) = document.rows[line]
            .as_ref()
            .ok_or("accessible row missing")?;
        Ok::<_, Failure>(accessibility::CellRef {
            row: document.ids[line],
            row_revision: *revision,
            column,
        })
    };
    let last_column = expected[ROWS - 1].len() as u16 - 1;
    let selection = accessibility::SelectionRefs {
        start: cell(first, 0)?,
        end: cell(last, last_column)?,
        rectangle: false,
    };
    stage(
        "accessibility-selection-focus",
        pane.focus(1, FocusTarget::Terminal),
    )?;
    require(
        matches!(
            stage(
                "accessibility-selection",
                pane.request(Operation::AccessibilitySelection {
                    channel: pane.channel(),
                    focus_generation: 1,
                    selection: Some(selection),
                })
            )?,
            ReplyBody::Done
        ),
        "accessible history selection refused",
    )?;
    // The server pushes the Selection in a later revision's tail, by line and column.
    let expected_selection = accessibility::Selection {
        start: accessibility::Point {
            line: first as u32,
            column: 0,
        },
        end: accessibility::Point {
            line: last as u32,
            column: last_column,
        },
        rectangle: false,
    };
    let until = Instant::now() + DEADLINE;
    while document.tail.as_ref().and_then(|tail| tail.selection) != Some(expected_selection) {
        document = semantics(pane, &document, until)?;
    }
    stage(
        "accessibility-selection",
        pane.request(Operation::AccessibilitySelection {
            channel: pane.channel(),
            focus_generation: 1,
            selection: None,
        }),
    )?;
    stage(
        "accessibility-demand-end",
        pane.demand_accessibility(accessibility::Demand::End),
    )?;
    if input_timing_requested() {
        input_timing(pane)?;
    }
    Ok(Report {
        copied_rows: ROWS,
        accessible_rows: ROWS,
        graphics_supported: true,
        graphics_bytes,
    })
}
