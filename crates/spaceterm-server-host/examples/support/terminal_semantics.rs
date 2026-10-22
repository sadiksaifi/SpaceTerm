//! One fixture-only terminal exercise shared by the native IPC, SSH and WSS checks.
use spaceterm_server::{
    ServerError,
    pane::{
        FocusTarget, Lease, PaneConnection, Reply, Request, Screen, accessibility, graphics,
        reading,
    },
};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, &'static str>;
const ROWS: usize = 5000;
const DEADLINE: Duration = Duration::from_secs(60);
const PREFIX: &str = "ST-NATIVE-";
const DONE: &str = "ST-SEMANTICS-DONE";

pub struct Report {
    pub copied_rows: usize,
    pub accessible_rows: usize,
    pub graphics_supported: bool,
    pub graphics_bytes: usize,
}

fn api<T, E>(result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|_| "terminal protocol refused or unavailable")
}
fn require(condition: bool, error: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(error) }
}
fn pause(until: Instant) -> Result<()> {
    require(Instant::now() < until, "terminal exercise deadline elapsed")?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}
pub fn screen(
    connection: &mut PaneConnection,
    lease: Lease,
    matches: impl Fn(&Screen) -> bool,
) -> Result<Box<Screen>> {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Reply::Screen(screen) = api(connection.exchange(&Request::Poll { lease }))?
            && matches(&screen)
        {
            return Ok(screen);
        }
        pause(until)?;
    }
}
fn read(
    connection: &mut PaneConnection,
    lease: Lease,
    operation: reading::Operation,
) -> Result<()> {
    let until = Instant::now() + DEADLINE;
    loop {
        match api(connection.exchange(&Request::Read {
            lease,
            operation: operation.clone(),
        }))? {
            Reply::Reading { accepted: true } => return Ok(()),
            Reply::Reading { accepted: false } => pause(until)?,
            _ => return Err("terminal reading response changed"),
        }
    }
}
fn semantics(connection: &mut PaneConnection, lease: Lease) -> Result<accessibility::Snapshot> {
    let until = Instant::now() + DEADLINE;
    let mut revision = None;
    let mut bytes = Vec::new();
    let mut total = None;
    loop {
        match api(connection.exchange(&Request::Accessibility {
            lease,
            revision,
            offset: bytes.len(),
            base: None,
        }))? {
            Reply::Accessibility(Ok(chunk)) => {
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
            Reply::Accessibility(Err(accessibility::Error::Pending)) => pause(until)?,
            Reply::Accessibility(Err(accessibility::Error::Stale)) => {
                revision = None;
                total = None;
                bytes.clear();
                pause(until)?;
            }
            _ => return Err("accessibility transfer unavailable"),
        }
    }
}
fn copy(connection: &mut PaneConnection, lease: Lease) -> Result<String> {
    let until = Instant::now() + DEADLINE;
    let generation = loop {
        match api(connection.exchange(&Request::ReadingStatus { lease }))? {
            Reply::ReadingReady {
                selection_revision,
                selection_present: true,
                ..
            } => {
                break selection_revision;
            }
            Reply::ReadingReady { .. } | Reply::SelectionPending => pause(until)?,
            _ => return Err("selection preparation unavailable"),
        }
    };
    let mut bytes = Vec::new();
    let mut total = None;
    let mut chunks = 0;
    loop {
        // Poll drains the worker's completed copy preparation before requesting a chunk.
        api(connection.exchange(&Request::Poll { lease }))?;
        match api(connection.exchange(&Request::SelectionChunk {
            lease,
            generation,
            offset: bytes.len(),
        }))? {
            Reply::SelectionChunk {
                generation: actual,
                offset,
                total: length,
                bytes: part,
            } => {
                require(
                    actual == generation
                        && offset == bytes.len()
                        && total.is_none_or(|saved| saved == length)
                        && length <= reading::MAX_SELECTION_BYTES
                        && !part.is_empty()
                        && part.len() <= reading::SELECTION_CHUNK_BYTES
                        && offset + part.len() <= length,
                    "selection chunk binding or bounds changed",
                )?;
                total = Some(length);
                bytes.extend(part);
                chunks += 1;
                if bytes.len() == length {
                    require(
                        chunks > 2,
                        "full history copy did not cross chunk boundaries",
                    )?;
                    let selection: reading::Selection = api(serde_json::from_slice(&bytes))?;
                    return Ok(selection.plain_text);
                }
            }
            Reply::SelectionPending => pause(until)?,
            _ => return Err("selection transfer unavailable"),
        }
    }
}

/// The caller owns the Pane and supplies its existing protocol command helper.
/// No terminal content, remote path or native error is returned as evidence.
pub fn exercise(
    connection: &mut PaneConnection,
    lease: Lease,
    command: impl Fn(&mut PaneConnection, Lease, &str) -> std::result::Result<(), ServerError>,
) -> Result<Report> {
    require(
        connection.supports_reading(),
        "required reading capability missing",
    )?;
    require(
        connection.supports_accessibility(),
        "required accessibility capability missing",
    )?;
    let graphics = connection.supports_graphics();
    let script = if graphics {
        "stty -echo; python3 -c 'import base64,sys; w=sys.stdout.write; w(\"\\033[3J\\033[2J\\033[H\"); [w(\"ST-NATIVE-%05d\\n\"%i) for i in range(5000)]; d=base64.b64encode(bytes([255,0,0,255])*320*240).decode(); [w(\"\\033_G\"+(\"a=T,f=32,s=320,v=240,i=541,q=2,\" if i==0 else \"\")+\"m=%d;\"%int(i+4096<len(d))+d[i:i+4096]+\"\\033\\\\\") for i in range(0,len(d),4096)]; w(\"\\nST-SEMANTICS-DONE\\n\"); sys.stdout.flush()'"
    } else {
        "stty -echo; printf '\\033[3J\\033[2J\\033[H'; i=0; while [ \"$i\" -lt 5000 ]; do printf 'ST-NATIVE-%05d\\n' \"$i\"; i=$((i+1)); done; printf 'ST-SEMANTICS-DONE\\n'"
    };
    api(command(connection, lease, script))?;
    // Exercise backpressure using the same real delayed consumer on each route.
    std::thread::sleep(Duration::from_millis(300));
    let complete = screen(connection, lease, |screen| {
        screen.rows.iter().any(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                == DONE
        })
    })?;
    require(
        complete
            .reading
            .as_ref()
            .is_some_and(|s| s.total_rows >= ROWS as u64),
        "bulk Scrollback did not retain its rows",
    )?;
    let mut graphics_bytes = 0;
    if graphics {
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
        while graphics_bytes < 320 * 240 * 4 {
            let Reply::GraphicsChunk(Ok(chunk)) =
                api(connection.exchange(&Request::GraphicsChunk {
                    lease,
                    image: image.key,
                    offset: graphics_bytes,
                }))?
            else {
                return Err("bulk graphics transfer unavailable");
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
    }
    read(
        connection,
        lease,
        reading::Operation::Query {
            generation: 1,
            text: PREFIX.into(),
        },
    )?;
    screen(connection, lease, |s| {
        s.reading
            .as_ref()
            .and_then(|s| s.find.as_ref())
            .is_some_and(|f| f.generation == 1 && f.total == ROWS)
    })?;
    read(
        connection,
        lease,
        reading::Operation::Navigate {
            generation: 1,
            backwards: false,
        },
    )?;
    let found = screen(connection, lease, |s| {
        s.reading
            .as_ref()
            .and_then(|s| s.find.as_ref())
            .is_some_and(|f| {
                f.generation == 1
                    && f.total == ROWS
                    && f.current
                        .is_some_and(|current| (1..=ROWS).contains(&current))
                    && f.spans.iter().any(|span| span.3)
            })
    })?;
    let first = found
        .reading
        .as_ref()
        .and_then(|s| s.find.as_ref())
        .and_then(|f| f.current)
        .ok_or("Find current match missing")?;
    read(
        connection,
        lease,
        reading::Operation::Navigate {
            generation: 1,
            backwards: false,
        },
    )?;
    screen(connection, lease, |s| {
        s.reading
            .as_ref()
            .and_then(|s| s.find.as_ref())
            .is_some_and(|f| f.total == ROWS && f.current == Some(first % ROWS + 1))
    })?;
    read(
        connection,
        lease,
        reading::Operation::EndFind { generation: 1 },
    )?;
    read(
        connection,
        lease,
        reading::Operation::Scroll(reading::Scroll::Top),
    )?;
    screen(connection, lease, |s| {
        s.rows.iter().any(|row| {
            row.iter()
                .map(|c| c.text.as_str())
                .collect::<String>()
                .trim_end()
                == "ST-NATIVE-00000"
        })
    })?;
    read(
        connection,
        lease,
        reading::Operation::Scroll(reading::Scroll::Bottom),
    )?;
    screen(connection, lease, |s| {
        s.reading
            .as_ref()
            .is_some_and(|r| r.offset_rows + r.visible_rows == r.total_rows)
    })?;

    let model = semantics(connection, lease)?;
    let expected: Vec<_> = (0..ROWS).map(|i| format!("{PREFIX}{i:05}")).collect();
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
    require(
        fixture_rows.iter().all(Option::is_some) && model.visible_lines.start > 0,
        "accessibility did not expose full retained fixture history",
    )?;
    let first = fixture_rows[0].ok_or("accessible first row missing")?;
    let last = fixture_rows[ROWS - 1].ok_or("accessible last row missing")?;
    let first_index = model
        .topology
        .iter()
        .position(|id| *id == first.id)
        .ok_or("accessible first row topology missing")?;
    let last_index = model
        .topology
        .iter()
        .position(|id| *id == last.id)
        .ok_or("accessible last row topology missing")?;
    require(
        last_index.checked_sub(first_index) == Some(ROWS - 1),
        "accessible row order changed",
    )?;
    let selection = accessibility::SelectionRefs {
        start: accessibility::CellRef {
            row: first.id,
            row_revision: first.revision,
            column: 0,
        },
        end: accessibility::CellRef {
            row: last.id,
            row_revision: last.revision,
            column: expected[ROWS - 1].len() as u16 - 1,
        },
        rectangle: false,
    };
    api(connection.exchange(&Request::Focus {
        lease,
        generation: 1,
        target: FocusTarget::Terminal,
    }))?;
    require(
        matches!(
            api(connection.exchange(&Request::AccessibilitySelection {
                lease,
                focus_generation: 1,
                selection: Some(selection),
            }))?,
            Reply::Reading { accepted: true }
        ),
        "accessible history selection refused",
    )?;
    let until = Instant::now() + DEADLINE;
    loop {
        if semantics(connection, lease)?.selection == Some(selection) {
            break;
        }
        pause(until)?;
    }
    require(
        copy(connection, lease)? == expected.join("\n"),
        "full history Copy contents changed",
    )?;
    api(connection.exchange(&Request::AccessibilitySelection {
        lease,
        focus_generation: 1,
        selection: None,
    }))?;
    api(connection.exchange(&Request::EndAccessibilityDemand { lease }))?;
    Ok(Report {
        copied_rows: ROWS,
        accessible_rows: ROWS,
        graphics_supported: graphics,
        graphics_bytes,
    })
}
