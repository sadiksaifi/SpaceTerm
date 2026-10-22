//! One bounded presentation exercise for the existing IPC, SSH and WSS fixtures.
//! The producer writes metadata without cell output while hidden and exits on request.
use super::terminal_semantics::{Failure, stage};
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::{
    ServerError,
    pane::{
        InteractiveInput, Key, Lease, PaneConnection, Reply, Request, Screen,
        metadata::{
            CommandState, DirectoryProvenance, Freshness, Progress, Snapshot, TitleGlyph,
            TitleProvenance,
        },
    },
};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Failure>;
const WAIT: Duration = Duration::from_secs(15);
const TITLE: &str = "cargo test · api";
const HIDDEN_TITLE: &str = "cargo test · hidden";
const HISTORY: &str = "ST-PRESENTATION-HISTORY-000";
const READY: &str = "ST-PRESENTATION-READY";
const PRODUCER: &str = r#"import os,select,termios,time,tty
original = termios.tcgetattr(0)
def emit(text):
    os.write(1,text.encode())
try:
    tty.setraw(0)
    emit("\x1b[2J\x1b[3J\x1b[H")
    for i in range(100):
        emit("ST-PRESENTATION-HISTORY-%03d\r\n" % i)
    emit("\x1b]2;cargo test · api\x07\x1b]7;file://localhost/tmp\x07\x1b]9;4;1;42\x07")
    emit("ST-PRESENTATION-READY\r\n")
    until = time.monotonic() + 90
    while time.monotonic() < until:
        if not select.select([0],[],[],min(1,until-time.monotonic()))[0]:
            continue
        key = os.read(0,1)
        if key == b"h":
            emit("\x1b]2;cargo test · hidden\x07\x1b]7;file://localhost/\x07\x1b]9;4;1;73\x07")
        elif key == b"x" or not key:
            break
finally:
    termios.tcsetattr(0,termios.TCSANOW,original)
emit("ST-PRESENTATION-EXIT\r\n")
"#;

pub struct Witness {
    metadata: Box<Snapshot>,
}

/// A separate admitted client observes the Pane without selecting or painting it.
pub fn observe_hidden_inventory(
    connection: &mut PaneConnection,
    pane: spaceterm_server::hierarchy::Id,
) -> Result<()> {
    let until = Instant::now() + WAIT;
    loop {
        let Reply::Inventory { inventory, .. } = stage(
            "observer-inventory",
            connection.exchange(&Request::Inventory),
        )?
        else {
            return Err("observer inventory reply missing".into());
        };
        if inventory
            .workspaces
            .iter()
            .flat_map(|work| &work.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|candidate| candidate.id == pane)
            .and_then(|pane| pane.presentation.as_deref())
            .is_some_and(|snapshot| {
                snapshot.title == HIDDEN_TITLE
                    && snapshot.directory == "/"
                    && snapshot.progress == Progress::Normal(73)
            })
        {
            return Ok(());
        }
        pause(until)?;
    }
}

fn require(condition: bool, failure: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(failure.into())
    }
}

fn pause(until: Instant) -> Result<()> {
    require(Instant::now() < until, "presentation deadline elapsed")?;
    std::thread::sleep(Duration::from_millis(30));
    Ok(())
}

fn send(connection: &mut PaneConnection, lease: Lease, text: &str) -> Result<()> {
    stage(
        "presentation-input",
        connection.exchange(&Request::Input {
            lease,
            generation: 1,
            input: InteractiveInput::Key(Key {
                physical: PhysicalKey::Unidentified as u32,
                action: 0,
                logical: "text-input".into(),
                text: Some(text.into()),
                unshifted: None,
                modifiers: 0,
                consumed: 0,
                option_as_alt: false,
            }),
        }),
    )?;
    Ok(())
}

fn screen(
    connection: &mut PaneConnection,
    lease: Lease,
    predicate: impl Fn(&Screen) -> bool,
) -> Result<Box<Screen>> {
    let until = Instant::now() + WAIT;
    loop {
        if let Reply::Screen(screen) = stage(
            "presentation-screen",
            connection.exchange(&Request::Poll { lease }),
        )? && predicate(&screen)
        {
            return Ok(screen);
        }
        pause(until)?;
    }
}

fn has_row(screen: &Screen, marker: &str) -> bool {
    screen.rows.iter().any(|row| {
        row.iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            == marker
    })
}

fn metadata(
    connection: &mut PaneConnection,
    lease: Lease,
    revision: Option<u64>,
    hidden: bool,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Result<Box<Snapshot>> {
    let until = Instant::now() + WAIT;
    loop {
        if hidden {
            require(
                matches!(
                    stage(
                        "hidden-screen-demand",
                        connection.exchange(&Request::Poll { lease })
                    )?,
                    Reply::Unchanged
                ),
                "hidden Pane published a Screen",
            )?;
        }
        match stage(
            "presentation-metadata",
            connection.exchange(&Request::Metadata { lease, revision }),
        )? {
            Reply::Metadata(snapshot) if predicate(&snapshot) => return Ok(snapshot),
            Reply::Metadata(_) | Reply::Unchanged => {}
            _ => return Err("unexpected metadata reply".into()),
        }
        pause(until)?;
    }
}

/// Starts a producer only in a fixture-owned Pane. Leaves it hidden for reconnect.
pub fn begin(
    connection: &mut PaneConnection,
    lease: Lease,
    command: impl Fn(&mut PaneConnection, Lease, &str) -> std::result::Result<(), ServerError>,
) -> Result<Witness> {
    let program = stage("presentation-script", serde_json::to_string(PRODUCER))?;
    let script = format!("python3 -c 'exec({})'", program.replace('\'', "'\\''"));
    stage("presentation-producer", command(connection, lease, &script))?;
    screen(connection, lease, |screen| has_row(screen, READY))?;
    let initial = metadata(connection, lease, None, false, |snapshot| {
        snapshot.title == TITLE
            && snapshot.directory == "/tmp"
            && snapshot.progress == Progress::Normal(42)
    })?;
    require(
        initial.title_provenance == TitleProvenance::TerminalControl
            && initial.directory_provenance == DirectoryProvenance::Osc7
            && initial.freshness == Freshness::Live
            && initial.title_glyph == TitleGlyph::Reported,
        "OSC metadata provenance changed",
    )?;
    require(
        initial.command.as_ref().is_some_and(|command| {
            command.state == CommandState::Running && command.line.contains("python3")
        }),
        "owning shell running-command metadata missing",
    )?;
    require(
        initial.user.as_ref().is_some_and(|value| !value.is_empty())
            && initial.host.as_ref().is_some_and(|value| !value.is_empty())
            && initial
                .home
                .as_ref()
                .is_some_and(|value| value.starts_with('/')),
        "owning machine presentation facts missing",
    )?;
    stage(
        "hide-presentation",
        connection.exchange(&Request::PresentationDemand {
            lease,
            visible: false,
        }),
    )?;
    send(connection, lease, "h")?;
    let changed = metadata(
        connection,
        lease,
        Some(initial.revision),
        true,
        |snapshot| {
            snapshot.title == HIDDEN_TITLE
                && snapshot.directory == "/"
                && snapshot.progress == Progress::Normal(73)
        },
    )?;
    require(
        changed.revision != initial.revision,
        "metadata-only update did not advance revision",
    )?;
    // The same revision must suppress retransmission even though the Pane is hidden.
    require(
        matches!(
            stage(
                "metadata-revision",
                connection.exchange(&Request::Metadata {
                    lease,
                    revision: Some(changed.revision),
                })
            )?,
            Reply::Unchanged
        ),
        "unchanged metadata revision was republished",
    )?;
    Ok(Witness { metadata: changed })
}

/// Completes the same producer after actual transport reconnection and lease renewal.
pub fn finish(
    connection: &mut PaneConnection,
    lease: Lease,
    previous: Lease,
    witness: Witness,
) -> Result<()> {
    require(
        lease.session == previous.session && lease.connection != previous.connection,
        "reconnect did not retain the Session with a fresh connection",
    )?;
    let resumed = metadata(connection, lease, None, false, |snapshot| {
        snapshot.title == witness.metadata.title
            && snapshot.directory == witness.metadata.directory
            && snapshot.progress == witness.metadata.progress
    })?;
    require(
        resumed.user == witness.metadata.user
            && resumed.host == witness.metadata.host
            && resumed.home == witness.metadata.home,
        "reconnect changed owning machine presentation facts",
    )?;
    stage(
        "show-presentation",
        connection.exchange(&Request::PresentationDemand {
            lease,
            visible: true,
        }),
    )?;
    let before = screen(connection, lease, |screen| {
        has_row(screen, READY)
            && screen
                .reading
                .as_ref()
                .is_some_and(|reading| reading.total_rows > reading.visible_rows)
    })?;
    // Reject each exact fence independently, as well as the disconnected lease.
    let mut wrong = vec![previous];
    for field in 0..4 {
        let mut stale = lease;
        match field {
            0 => stale.server[0] ^= 1,
            1 => stale.session[0] ^= 1,
            2 => stale.control[0] ^= 1,
            _ => stale.connection[0] ^= 1,
        }
        wrong.push(stale);
    }
    for stale in wrong {
        for request in [
            Request::Metadata {
                lease: stale,
                revision: None,
            },
            Request::ClearScreenAndScrollback { lease: stale },
        ] {
            require(
                matches!(
                    connection.exchange(&request),
                    Err(ServerError::StaleControl)
                ),
                "wrong lease admitted metadata or clear",
            )?;
        }
    }
    // Stale clears must have preserved the actual retained history.
    stage(
        "history-top",
        connection.exchange(&Request::Read {
            lease,
            operation: spaceterm_server::pane::reading::Operation::Scroll(
                spaceterm_server::pane::reading::Scroll::Top,
            ),
        }),
    )?;
    screen(connection, lease, |screen| has_row(screen, HISTORY))?;
    require(
        matches!(
            stage(
                "clear-admission",
                connection.exchange(&Request::ClearScreenAndScrollback { lease })
            )?,
            Reply::Unchanged
        ),
        "clear admission reply changed",
    )?;
    screen(connection, lease, |screen| {
        screen.generation != before.generation
            && screen.reading.as_ref().is_some_and(|reading| {
                reading.total_rows == reading.visible_rows && reading.offset_rows == 0
            })
            && !screen.rows.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains("ST-PRESENTATION")
            })
            && !has_row(screen, READY)
    })?;
    stage(
        "presentation-exit-focus",
        connection.exchange(&Request::Focus {
            lease,
            generation: 1,
            target: spaceterm_server::pane::FocusTarget::Terminal,
        }),
    )?;
    send(connection, lease, "x")?;
    screen(connection, lease, |screen| {
        has_row(screen, "ST-PRESENTATION-EXIT")
    })?;
    eprintln!(
        "PASS metadata-only hidden Pane, OSC title/directory/progress, origin facts, reconnect, exact lease refusals, screen and history clear"
    );
    Ok(())
}
