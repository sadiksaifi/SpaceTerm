//! One fixture-only terminal exercise shared by the native IPC and WSS checks.
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
            "wss-connect-pane",
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
        let failure = stage::<(), _>("wss-connect-pane", Err(error)).unwrap_err();
        assert_eq!(
            failure.to_string(),
            "wss-connect-pane: fixture-operation-unavailable"
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
        // stderr is separate from the WSS driver's invitation and response framing.
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
fn pause(until: Instant) -> Result<()> {
    require(Instant::now() < until, "terminal exercise deadline elapsed")?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
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
fn semantics(pane: &Pane, until: Instant) -> Result<accessibility::Snapshot> {
    let mut revision = None;
    let mut bytes = Vec::new();
    let mut total = None;
    loop {
        match stage(
            "accessibility-transfer",
            pane.request(Operation::Accessibility {
                channel: pane.channel(),
                revision,
                offset: bytes.len() as u64,
                base: None,
            }),
        )? {
            ReplyBody::Accessibility(Ok(chunk)) => {
                require(
                    chunk.offset == bytes.len()
                        && revision.is_none_or(|saved| saved == chunk.revision)
                        && total.is_none_or(|saved| saved == chunk.total)
                        && chunk.total <= accessibility::MAX_BYTES
                        && !chunk.bytes.is_empty()
                        && chunk.bytes.len() <= accessibility::CHUNK_BYTES
                        && chunk.offset + chunk.bytes.len() <= chunk.total,
                    "accessibility chunk binding or bounds changed",
                )?;
                revision = Some(chunk.revision);
                total = Some(chunk.total);
                bytes.extend(chunk.bytes);
                if bytes.len() == chunk.total {
                    let snapshot: accessibility::Snapshot = api(serde_json::from_slice(&bytes))?;
                    require(
                        Some(snapshot.revision) == revision,
                        "accessibility revision changed",
                    )?;
                    return Ok(snapshot);
                }
            }
            ReplyBody::Accessibility(Err(accessibility::Error::Pending)) => pause(until)?,
            ReplyBody::Accessibility(Err(accessibility::Error::Stale)) => {
                revision = None;
                total = None;
                bytes.clear();
                pause(until)?;
            }
            _ => return Err("accessibility transfer unavailable".into()),
        }
    }
}
/// The caller owns the Pane. Every route of protocol major 2 carries Scrollback pages,
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
    // Visual and semantic publications are independent. As in the runtime's
    // server_accessibility_fetches_history_and_rejects_a_disconnected_or_replaced_lease
    // regression, wait for the fixture's own semantic witness instead of its first snapshot.
    let (model, first, last) = loop {
        let model = semantics(pane, until)?;
        let mut fixture_rows = vec![None; ROWS];
        for row in &model.rows {
            let text = row
                .cells
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>();
            let text = text.trim_end();
            if let Some(index) = text
                .strip_prefix(PREFIX)
                .and_then(|n| n.parse::<usize>().ok())
            {
                require(
                    index < ROWS && text == expected[index] && fixture_rows[index].is_none(),
                    "accessible fixture row changed or duplicated",
                )?;
                fixture_rows[index] = Some(row);
            }
        }
        let matching = fixture_rows.iter().filter(|row| row.is_some()).count();
        if matching == ROWS && model.visible_lines.start > 0 {
            let first = fixture_rows[0].ok_or("accessible first row missing")?;
            let last = fixture_rows[ROWS - 1].ok_or("accessible last row missing")?;
            let endpoints = ((first.id, first.revision), (last.id, last.revision));
            break (model, endpoints.0, endpoints.1);
        }
        if Instant::now() >= until {
            eprintln!(
                "accessibility fixture deadline: rows={}, matching={}, topology={}, visible={}..{}",
                model.rows.len(),
                matching,
                model.topology.len(),
                model.visible_lines.start,
                model.visible_lines.end
            );
            return Err(Failure::from(
                "accessibility did not expose full retained fixture history",
            )
            .at("accessibility-fixture"));
        }
        pause(until).map_err(|failure| failure.at("accessibility-fixture"))?;
    };
    let first_index = model
        .topology
        .iter()
        .position(|id| *id == first.0)
        .ok_or("accessible first row topology missing")?;
    let last_index = model
        .topology
        .iter()
        .position(|id| *id == last.0)
        .ok_or("accessible last row topology missing")?;
    require(
        last_index.checked_sub(first_index) == Some(ROWS - 1),
        "accessible row order changed",
    )?;
    let selection = accessibility::SelectionRefs {
        start: accessibility::CellRef {
            row: first.0,
            row_revision: first.1,
            column: 0,
        },
        end: accessibility::CellRef {
            row: last.0,
            row_revision: last.1,
            column: expected[ROWS - 1].len() as u16 - 1,
        },
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
    let until = Instant::now() + DEADLINE;
    loop {
        if semantics(pane, until)?.selection == Some(selection) {
            break;
        }
        pause(until)?;
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
        pane.request(Operation::EndAccessibility {
            channel: pane.channel(),
        }),
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
