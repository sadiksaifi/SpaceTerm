//! One bounded presentation exercise for the existing IPC and WSS fixtures.
//! The producer writes metadata without cell output while hidden and exits on request.
use super::support::{self, Pane, Screen, has_row, text_key};
use super::terminal_semantics::{Failure, stage};
use spaceterm_server::{
    ServerError,
    connection::link::ServerLink,
    hierarchy::{Id, Inventory},
    pane::{
        FocusTarget, Lease,
        metadata::{
            CommandState, DirectoryProvenance, Freshness, Progress, Snapshot, TitleGlyph,
            TitleProvenance,
        },
    },
    protocol::{Channel, Operation, ReplyBody},
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
pub fn observe_hidden_inventory(link: &ServerLink, pane: Id) -> Result<()> {
    let until = Instant::now() + WAIT;
    loop {
        let (inventory, _) = stage("observer-inventory", support::inventory(link))?;
        if presentation(&inventory, pane)?.is_some_and(|snapshot| {
            snapshot.title == HIDDEN_TITLE
                && snapshot.directory == "/"
                && snapshot.progress == Progress::Normal(73)
        }) {
            return Ok(());
        }
        pause(until)?;
    }
}

fn presentation(inventory: &Inventory, pane: Id) -> Result<Option<&Snapshot>> {
    let snapshot = inventory
        .workspaces
        .iter()
        .flat_map(|work| &work.tabs)
        .flat_map(|tab| &tab.panes)
        .find(|candidate| candidate.id == pane)
        .and_then(|pane| pane.presentation.as_deref());
    if let Some(snapshot) = snapshot {
        stage("inventory-presentation-validation", snapshot.validate())?;
    }
    Ok(snapshot)
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

fn send(pane: &Pane, text: &str) -> Result<()> {
    stage("presentation-input", pane.key(1, text_key(text)))?;
    Ok(())
}

fn screen(pane: &Pane, predicate: impl Fn(&Screen) -> bool) -> Result<Screen> {
    stage("presentation-screen", pane.await_screen(WAIT, predicate))?
        .ok_or_else(|| "presentation deadline elapsed".into())
}

/// Waits until the Pane's presentation satisfies `predicate`. `hidden` holds the number of frames
/// the channel had received when it was hidden; a later frame fails.
fn metadata(
    pane: &Pane,
    hidden: Option<u64>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Result<Box<Snapshot>> {
    let lease = pane.lease();
    let until = Instant::now() + WAIT;
    loop {
        if let Some(frames) = hidden {
            require(pane.frames() == frames, "hidden Pane published a Screen")?;
        }
        let (inventory, _) = stage("presentation-inventory", support::inventory(pane.link()))?;
        let pane = inventory
            .close
            .as_ref()
            .filter(|close| close.boot == lease.server)
            .and_then(|close| {
                close
                    .sessions
                    .iter()
                    .find(|pane| pane.session == Some(lease.session))
            })
            .ok_or("inventory no longer contains the exact test Session")?
            .id;
        if let Some(snapshot) = presentation(&inventory, pane)?
            && predicate(snapshot)
        {
            return Ok(Box::new(snapshot.clone()));
        }
        pause(until)?;
    }
}

/// Starts a producer only in a fixture-owned Pane. Leaves it hidden for reconnect.
pub fn begin(pane: &Pane) -> Result<Witness> {
    let program = stage("presentation-script", serde_json::to_string(PRODUCER))?;
    let script = format!("python3 -c 'exec({})'", program.replace('\'', "'\\''"));
    stage("presentation-producer", pane.command(&script))?;
    screen(pane, |screen| has_row(screen, READY))?;
    let initial = metadata(pane, None, |snapshot| {
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
    stage("hide-presentation", pane.view(false))?;
    // The server applies the view before it answers this later request, so every frame it sent
    // before hiding the channel arrives before the reply.
    stage("hide-presentation", support::round_trip(pane.link()))?;
    let frames = pane.frames();
    send(pane, "h")?;
    let changed = metadata(pane, Some(frames), |snapshot| {
        snapshot.title == HIDDEN_TITLE
            && snapshot.directory == "/"
            && snapshot.progress == Progress::Normal(73)
    })?;
    Ok(Witness { metadata: changed })
}

/// Completes the same producer after actual transport reconnection and lease renewal.
pub fn finish(pane: &Pane, previous: Lease, witness: Witness) -> Result<()> {
    let lease = pane.lease();
    require(
        lease.session == previous.session && lease.connection != previous.connection,
        "reconnect did not retain the Session with a fresh connection",
    )?;
    let resumed = metadata(pane, None, |snapshot| {
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
    stage("show-presentation", pane.view(true))?;
    let before = screen(pane, |screen| {
        has_row(screen, READY) && screen.history.total > screen.history.oldest
    })?;
    // Requests name a channel, not a lease, so a client can no longer present a lease with a
    // changed fence. The disconnected lease's channel ended with its connection; a channel this
    // connection never bound stands in for it. Channels start at 1, so `u32::MAX` was never bound.
    require(
        matches!(
            pane.request(Operation::ClearScrollback {
                channel: Channel(u32::MAX),
            }),
            Err(ServerError::StaleControl)
        ),
        "unbound channel admitted clear",
    )?;
    // The stale clear must have preserved the actual retained history.
    require(
        stage("history-top", pane.scrollback(before.history))?
            .iter()
            .any(|row| row.trim_end() == HISTORY),
        "refused clear lost retained history",
    )?;
    require(
        matches!(
            stage(
                "clear-admission",
                pane.request(Operation::ClearScrollback {
                    channel: pane.channel(),
                })
            )?,
            ReplyBody::Done
        ),
        "clear admission reply changed",
    )?;
    screen(pane, |screen| {
        screen.generation != before.generation
            && screen.history.total == screen.history.oldest
            && !screen
                .rows
                .iter()
                .any(|row| row.contains("ST-PRESENTATION"))
            && !has_row(screen, READY)
    })?;
    stage(
        "presentation-exit-focus",
        pane.focus(1, FocusTarget::Terminal),
    )?;
    send(pane, "x")?;
    screen(pane, |screen| has_row(screen, "ST-PRESENTATION-EXIT"))?;
    eprintln!(
        "PASS metadata-only hidden Pane, OSC title/directory/progress, origin facts, reconnect, unbound channel refusal, screen and history clear"
    );
    Ok(())
}
