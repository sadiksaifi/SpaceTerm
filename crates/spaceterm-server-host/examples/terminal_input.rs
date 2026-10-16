//! Real paired-client input check. Refuses to interfere with an already running server.
#[path = "support/lifecycle.rs"]
mod lifecycle;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::{LocalServer, ServerError, hierarchy::*, pane::*};
use std::{
    error::Error,
    time::{Duration, Instant},
};

struct WorkspaceCleanup<'a> {
    client: &'a LocalServer,
    boot: Id,
    workspace: Option<Id>,
}
impl WorkspaceCleanup<'_> {
    fn cleanup(&mut self) -> Result<(), Box<dyn Error>> {
        let Some(workspace) = self.workspace else {
            return Ok(());
        };
        let mut connection = self.client.connect_pane()?;
        let Reply::Inventory {
            inventory,
            next_sequence,
        } = connection.exchange(&Request::Inventory)?
        else {
            return Err("cleanup inventory missing".into());
        };
        if inventory.close.as_ref().map(|live| live.boot) != Some(self.boot) {
            return Err("cleanup refused a replacement server".into());
        }
        if inventory.workspaces.iter().any(|w| w.id == workspace) {
            let target = CloseTarget::Workspace { workspace };
            let confirmation = inventory
                .close_confirmation(target)
                .ok_or("cleanup authority missing")?;
            let Reply::Outcome(result) = connection.exchange(&Request::Mutate {
                sequence: next_sequence,
                mutation: Mutation::Close {
                    target,
                    confirmation,
                },
            })?
            else {
                return Err("cleanup outcome missing".into());
            };
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

fn focus(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
    target: FocusTarget,
) -> Result<(), ServerError> {
    assert!(matches!(
        c.exchange(&Request::Focus {
            lease,
            generation,
            target
        })?,
        Reply::Unchanged
    ));
    Ok(())
}
fn input(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
    input: InteractiveInput,
) -> Result<Reply, ServerError> {
    c.exchange(&Request::Input {
        lease,
        generation,
        input,
    })
}
fn key(text: &str, ime: bool) -> InteractiveInput {
    InteractiveInput::Key(Key {
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
    })
}
fn command(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
    text: &str,
) -> Result<(), ServerError> {
    input(c, lease, generation, key(text, false))?;
    input(
        c,
        lease,
        generation,
        InteractiveInput::Key(Key {
            physical: PhysicalKey::Enter as u32,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        }),
    )?;
    Ok(())
}
fn await_marker(
    c: &mut PaneConnection,
    lease: Lease,
    marker: &str,
) -> Result<Box<Screen>, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Reply::Screen(screen) = c.exchange(&Request::Poll { lease })?
            && screen.rows.iter().any(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    == marker
            })
        {
            return Ok(screen);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("PTY marker missing: {marker}").into())
}
fn capture(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
    bytes: usize,
    modes: &str,
) -> Result<Box<Screen>, Box<dyn Error>> {
    let marker = format!("READY-{generation}-{bytes}");
    command(
        c,
        lease,
        generation,
        &format!(
            "stty raw -echo; printf '{modes}\\033[2J\\033[H{marker}\\r\\n'; dd bs=1 count={bytes} 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'"
        ),
    )?;
    await_marker(c, lease, &marker)
}
fn unsafe_paste(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
) -> Result<u64, Box<dyn Error>> {
    let Reply::Paste(PasteOutcome::ConfirmationRequired(p)) = input(
        c,
        lease,
        generation,
        InteractiveInput::Paste {
            text: "a\nb".into(),
        },
    )?
    else {
        return Err("unsafe paste did not require confirmation".into());
    };
    assert_eq!((p.byte_len, p.line_count, p.multiline), (3, 2, true));
    Ok(p.id)
}
fn native_effects(c: &mut PaneConnection, lease: Lease) -> Result<(), Box<dyn Error>> {
    use spaceterm_server::pane::effects::{Completion, Effect};
    assert!(c.supports_native_effects());
    focus(c, lease, 3, FocusTarget::Terminal)?;
    command(
        c,
        lease,
        3,
        "stty raw -echo; printf '\\033]52;c;?\\007'; dd bs=1 count=12 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let request = await_effect(c, lease, 3)?;
    assert!(matches!(request.effect, Effect::ClipboardRead { .. }));
    c.exchange(&Request::CompleteClipboard {
        lease,
        generation: 3,
        id: request.id,
        completion: Completion::Text("abc".into()),
    })?;
    await_marker(c, lease, "1b5d35323b633b59574a6a07")?;
    command(
        c,
        lease,
        3,
        "sleep 0.1; printf '\\033]7701;permissions=accessibility\\007\\007'",
    )?;
    focus(c, lease, 4, FocusTarget::Unfocused)?;
    let mut permission = false;
    let mut bell = false;
    for _ in 0..2 {
        match await_effect(c, lease, 4)?.effect {
            Effect::PermissionSetup {
                accessibility: true,
                ..
            } => permission = true,
            Effect::Bell => bell = true,
            other => return Err(format!("unexpected effect: {other:?}").into()),
        }
    }
    assert!(permission && bell);
    focus(c, lease, 5, FocusTarget::Terminal)?;
    command(
        c,
        lease,
        5,
        "stty raw -echo; printf '\\033]52;c;?\\007'; dd bs=1 count=8 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let request = await_effect(c, lease, 5)?;
    focus(c, lease, 6, FocusTarget::Unfocused)?;
    focus(c, lease, 7, FocusTarget::Terminal)?;
    assert!(matches!(
        c.exchange(&Request::CompleteClipboard {
            lease,
            generation: 5,
            id: request.id,
            completion: Completion::Text("must not enter PTY".into())
        }),
        Err(ServerError::StaleControl)
    ));
    await_marker(c, lease, "1b5d35323b633b07")?;
    Ok(())
}

fn await_effect(
    c: &mut PaneConnection,
    lease: Lease,
    generation: u64,
) -> Result<effects::Request, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match c.exchange(&Request::NativeEffects { lease, generation })? {
            Reply::NativeEffects {
                request: Some(request),
                ..
            } => return Ok(request),
            Reply::NativeEffects { request: None, .. } => {}
            _ => return Err("native effects missing".into()),
        }
        if Instant::now() >= deadline {
            return Err("native effect timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "2".into())
        .parse()?;
    let first = LocalServer::for_development_client("input-first", major)?;
    let second = LocalServer::for_development_client("input-second", major)?;
    require_idle(&first)?;
    // Setup finds the sibling executable, as in the existing lifecycle examples.
    let directory = std::env::current_exe()?
        .parent()
        .ok_or("example directory")?
        .to_owned();
    let companion = directory.join("spaceterm-server");
    if !companion.try_exists()? {
        std::os::unix::fs::symlink("../spaceterm-server", &companion)?;
    }
    assert_eq!(first.setup_and_start()?.protocol_major, major);
    assert_eq!(second.setup_and_start()?.protocol_major, major);
    connect(&first)?.exchange(&Request::Shutdown)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut server = Server::start()?;
    let mut a = connect(&first)?;
    assert!(a.supports_interactive_input());
    let Reply::Inventory {
        next_sequence,
        inventory,
    } = a.exchange(&Request::Inventory)?
    else {
        return Err("inventory missing".into());
    };
    let boot = inventory.close.as_ref().ok_or("server boot missing")?.boot;
    let Reply::Outcome(created) = a.exchange(&Request::Mutate {
        sequence: next_sequence,
        mutation: Mutation::CreateWorkspace {
            name: "Interactive input check".into(),
        },
    })?
    else {
        return Err("creation outcome missing".into());
    };
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("creation refused".into());
    };
    let mut cleanup = WorkspaceCleanup {
        client: &first,
        boot,
        workspace: Some(workspace),
    };
    let Reply::Inventory { inventory, .. } = a.exchange(&Request::Inventory)? else {
        return Err("inventory missing".into());
    };
    let pane = inventory
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("created Workspace missing")?
        .tabs[0]
        .root_pane;
    let Reply::Attached { lease: original } = a.exchange(&Request::SelectPane {
        pane,
        explicit: true,
        size: size(),
        style: None,
    })?
    else {
        return Err("Pane missing".into());
    };
    assert!(matches!(
        input(&mut a, original, 1, key("discarded", false)),
        Err(ServerError::StaleControl)
    ));
    focus(&mut a, original, 1, FocusTarget::Terminal)?;
    capture(&mut a, original, 1, 15, "\\033[?1004h")?;
    focus(&mut a, original, 2, FocusTarget::FocusedPane)?;
    assert!(matches!(
        input(&mut a, original, 2, key("discarded", false)),
        Err(ServerError::StaleControl)
    ));
    input(&mut a, original, 2, key("界", true))?;
    focus(&mut a, original, 3, FocusTarget::Unfocused)?;
    assert!(matches!(
        input(&mut a, original, 3, key("discarded", true)),
        Err(ServerError::StaleControl)
    ));
    focus(&mut a, original, 4, FocusTarget::Terminal)?;
    input(&mut a, original, 4, key("K", false))?;
    input(
        &mut a,
        original,
        4,
        InteractiveInput::Paste { text: "ok".into() },
    )?;
    await_marker(&mut a, original, "1b5b491b5b4fe7958c1b5b494b6f6b")?;
    // Disable focus reporting before the next byte capture.
    command(
        &mut a,
        original,
        4,
        "printf '\\033[?1004l\\033[2J\\033[HFOCUS-DONE\\n'",
    )?;
    await_marker(&mut a, original, "FOCUS-DONE")?;
    capture(&mut a, original, 4, 5, "")?;
    let cancelled = unsafe_paste(&mut a, original, 4)?;
    focus(
        &mut a,
        original,
        4,
        FocusTarget::PasteConfirmation { id: cancelled },
    )?;
    focus(&mut a, original, 5, FocusTarget::Unfocused)?;
    focus(&mut a, original, 6, FocusTarget::Terminal)?;
    assert!(matches!(
        input(
            &mut a,
            original,
            6,
            InteractiveInput::ResolvePaste {
                id: cancelled,
                confirm: true
            }
        )?,
        Reply::Paste(PasteOutcome::Stale)
    ));
    let disconnected = unsafe_paste(&mut a, original, 6)?;
    drop(a);
    let mut a = connect(&first)?;
    let Reply::Attached { lease: resumed } = a.exchange(&Request::SelectPane {
        pane,
        explicit: false,
        size: size(),
        style: None,
    })?
    else {
        return Err("reconnect did not resume".into());
    };
    assert_eq!(resumed.session, original.session);
    assert_ne!(resumed.connection, original.connection);
    focus(&mut a, resumed, 1, FocusTarget::Terminal)?;
    assert!(matches!(
        input(
            &mut a,
            resumed,
            1,
            InteractiveInput::ResolvePaste {
                id: disconnected,
                confirm: true
            }
        )?,
        Reply::Paste(PasteOutcome::Stale)
    ));
    let transferred = unsafe_paste(&mut a, resumed, 1)?;
    let mut b = connect(&second)?;
    let Reply::Attached { lease } = b.exchange(&Request::TakeOver {
        target: resumed.into(),
        size: size(),
        style: None,
    })?
    else {
        return Err("takeover failed".into());
    };
    assert_eq!(lease.session, resumed.session);
    assert_ne!(lease.control, resumed.control);
    assert!(matches!(
        input(
            &mut a,
            resumed,
            1,
            InteractiveInput::ResolvePaste {
                id: transferred,
                confirm: true
            }
        ),
        Err(ServerError::StaleControl)
    ));
    focus(&mut b, lease, 1, FocusTarget::Terminal)?;
    assert!(matches!(
        input(
            &mut b,
            lease,
            1,
            InteractiveInput::ResolvePaste {
                id: transferred,
                confirm: true
            }
        )?,
        Reply::Paste(PasteOutcome::Stale)
    ));
    input(
        &mut b,
        lease,
        1,
        InteractiveInput::Paste { text: "ok".into() },
    )?;
    let id = unsafe_paste(&mut b, lease, 1)?;
    focus(&mut b, lease, 1, FocusTarget::PasteConfirmation { id })?;
    assert!(matches!(
        input(
            &mut b,
            lease,
            1,
            InteractiveInput::ResolvePaste { id, confirm: true }
        )?,
        Reply::Paste(PasteOutcome::Written)
    ));
    assert!(matches!(
        input(
            &mut b,
            lease,
            1,
            InteractiveInput::ResolvePaste { id, confirm: true }
        )?,
        Reply::Paste(PasteOutcome::Stale)
    ));
    await_marker(&mut b, lease, "6f6b610d62")?;
    focus(&mut b, lease, 2, FocusTarget::Terminal)?;
    let screen = capture(&mut b, lease, 2, 28, "\\033[?1000h\\033[?1006h")?;
    assert!(screen.mouse_tracking);
    let pointer = Pointer {
        screen_generation: screen.generation,
        phase: PointerPhase::Press,
        button: Some(PointerButton::Left),
        x: 4.0,
        y: 5.0,
        modifiers: 0,
        shift_selection: true,
    };
    input(&mut b, lease, 2, InteractiveInput::Pointer(pointer))?;
    input(
        &mut b,
        lease,
        2,
        InteractiveInput::Pointer(Pointer {
            phase: PointerPhase::Release,
            ..pointer
        }),
    )?;
    input(
        &mut b,
        lease,
        2,
        InteractiveInput::Wheel(Wheel {
            screen_generation: screen.generation,
            horizontal_steps: 0,
            vertical_steps: 1,
            phase: WheelPhase::GestureChanged,
            x: 4.0,
            y: 5.0,
            modifiers: 0,
            shift_selection: true,
        }),
    )?;
    await_marker(
        &mut b,
        lease,
        "1b5b3c303b313b314d1b5b3c303b313b316d1b5b3c36343b313b314d",
    )?;
    b.exchange(&Request::Resize {
        lease,
        size: PaneSize {
            columns: 91,
            rows: 31,
            cell_width: 9,
            cell_height: 19,
        },
    })?;
    b.exchange(&Request::Style {
        lease,
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
        &mut b,
        lease,
        2,
        "stty size; stty raw -echo; printf '\\033[?996n'; dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \\n'; stty sane; printf '\\r\\n'",
    )?;
    let screen = await_marker(&mut b, lease, "1b5b3f3939373b326e")?;
    assert_eq!(
        (screen.columns, screen.rows.len(), screen.background),
        (91, 31, [240; 3])
    );
    assert!(screen.rows.iter().any(|row| {
        row.iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            == "31 91"
    }));
    native_effects(&mut b, lease)?;
    b.exchange(&Request::Close { lease })?;
    cleanup.cleanup()?;
    b.exchange(&Request::Shutdown)?;
    let _ = server.kill();
    println!(
        "Protocol {major}: paired clients verified PTY keys/IME, focus reports, paste approval/focus/reconnect/takeover fences, pointer/wheel, measured resize, controlling appearance, clipboard replies/refusal, Permission Requests and background attention."
    );
    Ok(())
}
