//! Real account-local client input check. Refuses to interfere with an already running server.
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "the input check uses its own key, command and screen helpers"
)]
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::*,
    pane::*,
    protocol::{InputEvent, Operation},
};
use std::{
    error::Error,
    time::{Duration, Instant},
};
use support::{Claimed, Pane, Screen, has_row};

struct WorkspaceCleanup<'a> {
    client: &'a ServerConnection,
    boot: Id,
    workspace: Option<Id>,
}
impl WorkspaceCleanup<'_> {
    fn cleanup(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(workspace) = self.workspace else {
            return Ok(());
        };
        let link = support::connect(self.client)?;
        let (inventory, next_sequence) = support::inventory(&link)?;
        if inventory.close.as_ref().map(|live| live.boot) != Some(self.boot) {
            return Err("cleanup refused a replacement server".into());
        }
        if inventory.workspaces.iter().any(|w| w.id == workspace) {
            let target = CloseTarget::Workspace { workspace };
            let confirmation = inventory
                .close_confirmation(target)
                .ok_or("cleanup authority missing")?;
            let result = support::mutate(
                &link,
                next_sequence,
                Mutation::Close {
                    target,
                    confirmation,
                },
            )?;
            if result.outcome != (Outcome::Accepted { target: workspace }) {
                return Err("test Workspace cleanup refused".into());
            }
        }
        self.workspace = None;
        Ok(())
    }
}
impl Drop for WorkspaceCleanup<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("test Workspace cleanup failed: {error}");
        }
    }
}

fn key(text: &str, ime: bool) -> Key {
    Key {
        physical: PhysicalKey::Unidentified as u32,
        action: 0,
        logical: if ime {
            "input-method-commit"
        } else {
            "text-input"
        }
        .into(),
        text: Some(text.into()),
        unshifted: None,
        modifiers: 0,
        consumed: 0,
        option_as_alt: false,
    }
}
/// Sends `key` under `generation` and waits for the server to discard it as stale.
fn refused(pane: &Pane, generation: u64, key: Key) -> Result<(), Box<dyn Error>> {
    let serial = pane.key(generation, key)?;
    assert_eq!(pane.discarded(serial)?, Refusal::Stale);
    Ok(())
}
fn command(pane: &Pane, generation: u64, text: &str) -> Result<(), ServerError> {
    pane.input(
        generation,
        vec![
            InputEvent::Key(key(text, false)),
            InputEvent::Key(Key {
                physical: PhysicalKey::Enter as u32,
                action: 0,
                logical: "enter".into(),
                text: None,
                unshifted: None,
                modifiers: 0,
                consumed: 0,
                option_as_alt: false,
            }),
        ],
    )
    .map(drop)
}
fn await_marker(pane: &Pane, marker: &str) -> Result<Screen, Box<dyn Error>> {
    pane.await_screen(Duration::from_secs(10), |screen| has_row(screen, marker))?
        .ok_or_else(|| format!("PTY marker missing: {marker}").into())
}
fn capture(
    pane: &Pane,
    generation: u64,
    bytes: usize,
    modes: &str,
) -> Result<Screen, Box<dyn Error>> {
    let marker = format!("READY-{generation}-{bytes}");
    command(
        pane,
        generation,
        &format!(
            "stty raw -echo; printf '{modes}\\033[2J\\033[H{marker}\\r\\n'; dd bs=1 count={bytes} 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'"
        ),
    )?;
    await_marker(pane, &marker)
}
fn unsafe_paste(pane: &Pane, generation: u64) -> Result<u64, Box<dyn Error>> {
    let PasteOutcome::ConfirmationRequired(p) = pane.paste(generation, "a\nb")? else {
        return Err("unsafe paste did not require confirmation".into());
    };
    assert_eq!((p.byte_len, p.line_count, p.multiline), (3, 2, true));
    Ok(p.id)
}
fn native_effects(pane: &Pane) -> Result<(), Box<dyn Error>> {
    use spaceterm_server::pane::effects::{Completion, Effect};
    pane.focus(3, FocusTarget::Terminal)?;
    command(
        pane,
        3,
        "stty raw -echo; printf '\\033[2J\\033[H\\033]52;c;?\\007'; dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let request = await_effect(pane, 3)?;
    assert!(matches!(request.effect, Effect::ClipboardRead { .. }));
    pane.effect_done(3, request.id, Completion::Text("abc".into()))?;
    await_marker(pane, "1b5d35323b633b59574a6a07")?;
    command(
        pane,
        3,
        "sleep 0.1; printf '\\033]7701;permissions=accessibility\\007\\007'",
    )?;
    pane.focus(4, FocusTarget::Unfocused)?;
    let mut permission = false;
    let mut bell = false;
    for _ in 0..2 {
        match await_effect(pane, 4)?.effect {
            Effect::PermissionSetup {
                accessibility: true,
                ..
            } => permission = true,
            Effect::Bell => bell = true,
            other => return Err(format!("unexpected effect: {other:?}").into()),
        }
    }
    assert!(permission && bell);
    pane.focus(5, FocusTarget::Terminal)?;
    command(
        pane,
        5,
        "stty raw -echo; printf '\\033[2J\\033[H\\033]52;c;?\\007'; dd bs=1 count=8 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let request = await_effect(pane, 5)?;
    pane.focus(6, FocusTarget::Unfocused)?;
    pane.focus(7, FocusTarget::Terminal)?;
    // The server reports no outcome for a completion. The empty clipboard reply below shows that
    // the completion of generation 5 never reached the PTY.
    pane.effect_done(5, request.id, Completion::Text("must not enter PTY".into()))?;
    await_marker(pane, "1b5d35323b633b07")?;
    Ok(())
}

/// Waits for the next native effect request the server pushes under focus generation
/// `generation`. As the desktop does, it drops a request raised under another generation, such as
/// the shell integration's report of an earlier command.
fn await_effect(pane: &Pane, generation: u64) -> Result<effects::Request, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let request = pane.await_effect(wait)?.ok_or("native effect timed out")?;
        if request.generation == generation {
            return Ok(request);
        }
    }
}

fn attached(claimed: Claimed, failure: &'static str) -> Result<Pane, Box<dyn Error>> {
    claimed.attached().ok_or_else(|| failure.into())
}

fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "3".into())
        .parse()?;
    let first = ServerConnection::for_development_client("input-first", major)?;
    let second = ServerConnection::for_development_client("input-second", major)?;
    require_idle(&first)?;
    first.setup()?;
    second.setup()?;
    let mut server = Server::start()?;
    let a = connect(&first)?;
    let (inventory, next_sequence) = support::inventory(&a)?;
    let boot = inventory.close.as_ref().ok_or("server boot missing")?.boot;
    let created = support::mutate(
        &a,
        next_sequence,
        Mutation::CreateWorkspace {
            name: "Interactive input check".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    )?;
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("creation refused".into());
    };
    let mut cleanup = WorkspaceCleanup {
        client: &first,
        boot,
        workspace: Some(workspace),
    };
    let (inventory, _) = support::inventory(&a)?;
    let pane = inventory
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("created Workspace missing")?
        .tabs[0]
        .root_pane;
    let original = attached(support::select(&a, pane, true, size())?, "Pane missing")?;
    refused(&original, 1, key("discarded", false))?;
    original.focus(1, FocusTarget::Terminal)?;
    capture(&original, 1, 15, "\\033[?1004h")?;
    original.focus(2, FocusTarget::FocusedPane)?;
    refused(&original, 2, key("discarded", false))?;
    original.key(2, key("界", true))?;
    original.focus(3, FocusTarget::Unfocused)?;
    refused(&original, 3, key("discarded", true))?;
    original.focus(4, FocusTarget::Terminal)?;
    original.key(4, key("K", false))?;
    original.paste(4, "ok")?;
    await_marker(&original, "1b5b491b5b4fe7958c1b5b494b6f6b")?;
    // Disable focus reporting before the next byte capture.
    command(
        &original,
        4,
        "printf '\\033[?1004l\\033[2J\\033[HFOCUS-DONE\\n'",
    )?;
    await_marker(&original, "FOCUS-DONE")?;
    capture(&original, 4, 5, "")?;
    let cancelled = unsafe_paste(&original, 4)?;
    original.focus(4, FocusTarget::PasteConfirmation { id: cancelled })?;
    original.focus(5, FocusTarget::Unfocused)?;
    original.focus(6, FocusTarget::Terminal)?;
    assert_eq!(
        original.resolve_paste(6, cancelled, true)?,
        PasteOutcome::Stale
    );
    let disconnected = unsafe_paste(&original, 6)?;
    a.close();
    let original = original.lease();
    let a = connect(&first)?;
    let resumed = attached(
        support::select(&a, pane, false, size())?,
        "reconnect did not resume",
    )?;
    assert_eq!(resumed.lease().session, original.session);
    assert_ne!(resumed.lease().connection, original.connection);
    resumed.focus(1, FocusTarget::Terminal)?;
    assert_eq!(
        resumed.resolve_paste(1, disconnected, true)?,
        PasteOutcome::Stale
    );
    let transferred = unsafe_paste(&resumed, 1)?;
    let b = connect(&second)?;
    let taken = attached(
        support::claim(
            &b,
            pane,
            AttachmentClaim::TakeOver {
                target: resumed.lease().into(),
            },
            size(),
        )?,
        "takeover failed",
    )?;
    let lease = taken.lease();
    assert_eq!(lease.session, resumed.lease().session);
    assert_ne!(lease.control, resumed.lease().control);
    assert_eq!(
        resumed.resolve_paste(1, transferred, true).err(),
        Some(ServerError::StaleControl)
    );
    taken.focus(1, FocusTarget::Terminal)?;
    assert_eq!(
        taken.resolve_paste(1, transferred, true)?,
        PasteOutcome::Stale
    );
    taken.paste(1, "ok")?;
    let id = unsafe_paste(&taken, 1)?;
    taken.focus(1, FocusTarget::PasteConfirmation { id })?;
    assert_eq!(taken.resolve_paste(1, id, true)?, PasteOutcome::Written);
    assert_eq!(taken.resolve_paste(1, id, true)?, PasteOutcome::Stale);
    await_marker(&taken, "6f6b610d62")?;
    taken.focus(2, FocusTarget::Terminal)?;
    let screen = capture(&taken, 2, 28, "\\033[?1000h\\033[?1006h")?;
    assert!(screen.modes.is_some_and(|modes| modes.mouse_tracking));
    let pointer = Pointer {
        screen_generation: screen.generation,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        x: 4.0,
        y: 5.0,
        modifiers: 0,
        shift_selection: true,
    };
    taken.input(2, vec![InputEvent::Pointer(pointer)])?;
    taken.input(
        2,
        vec![InputEvent::Pointer(Pointer {
            phase: PointerPhase::Release,
            ..pointer
        })],
    )?;
    taken.input(
        2,
        vec![InputEvent::Wheel(Wheel {
            screen_generation: screen.generation,
            horizontal_steps: 0,
            vertical_steps: 1,
            phase: WheelPhase::GestureChanged,
            x: 4.0,
            y: 5.0,
            modifiers: 0,
            shift_selection: true,
        })],
    )?;
    await_marker(
        &taken,
        "1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c36343b313b314d",
    )?;
    taken.resize(PaneSize {
        columns: 91,
        rows: 31,
        cell_width: 9,
        cell_height: 19,
    })?;
    taken.request(Operation::Style {
        channel: taken.channel(),
        style: PresentationStyle {
            light: true,
            foreground: [20; 3],
            background: [240; 3],
            palette: [[0; 3]; 16],
            cursor: [20; 3],
            cursor_text: None,
            bold_as_bright: false,
        },
    })?;
    command(
        &taken,
        2,
        "stty size; stty raw -echo; printf '\\033[?996n'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let screen = await_marker(&taken, "1b5b3f3939373b326e")?;
    let geometry = screen.geometry.ok_or("screen geometry missing")?;
    let colors = screen.colors.as_ref().ok_or("screen colors missing")?;
    assert_eq!(
        (geometry.columns, geometry.rows, colors.background),
        (91, 31, [240; 3])
    );
    assert!(has_row(&screen, "31 91"));
    native_effects(&taken)?;
    taken.close()?;
    cleanup.cleanup()?;
    support::done(&b, Operation::Shutdown)?;
    let _ = server.kill();
    println!(
        "Protocol {major}: account-local clients verified PTY keys/IME, focus reports, paste approval/focus/reconnect/takeover fences, pointer/wheel, measured resize, controlling appearance, clipboard replies/refusal, Permission Requests and background attention."
    );
    Ok(())
}
