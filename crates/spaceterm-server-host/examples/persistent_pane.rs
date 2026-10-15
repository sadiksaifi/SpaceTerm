//! Real-host smoke check, using the same native credentials and authenticated transport as desktop.
use spaceterm::runtime::PhysicalKey;
use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::{
    error::Error,
    time::{Duration, Instant},
};

fn command(connection: &mut PaneConnection, lease: Lease, text: &str) -> Result<(), ServerError> {
    connection.exchange(&Request::Key {
        lease,
        key: Key {
            physical: PhysicalKey::Unidentified as u32,
            action: 0,
            logical: "text-input".into(),
            text: Some(text.into()),
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
    })?;
    connection.exchange(&Request::Key {
        lease,
        key: Key {
            physical: PhysicalKey::Enter as u32,
            action: 0,
            logical: "enter".into(),
            text: None,
            unshifted: None,
            modifiers: 0,
            consumed: 0,
            option_as_alt: false,
        },
    })?;
    Ok(())
}
fn attach(connection: &mut PaneConnection, create: bool) -> Result<Lease, ServerError> {
    match connection.exchange(&Request::Attach {
        create,
        resume: None,
        style: None,
        size: PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        },
    })? {
        Reply::Attached { lease } => Ok(lease),
        _ => Err(ServerError::Protocol),
    }
}
fn await_output(
    connection: &mut PaneConnection,
    lease: Lease,
    marker: &str,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Reply::Screen(screen) = connection.exchange(&Request::Poll { lease })? {
            let output = screen
                .rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            if output.lines().any(|line| line.trim_end() == marker) {
                return Ok(output);
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err("terminal output did not arrive".into())
}
fn main() -> Result<(), Box<dyn Error>> {
    let server = LocalServer::for_identity("spaceterm-development")?;
    let mut first = server.connect_pane()?;
    let original = attach(&mut first, true)?;
    command(
        &mut first,
        original,
        "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 2; printf 'CONTINUED\\n'; printf 'PID=%s\\n' \"$$\"",
    )?;
    await_output(&mut first, original, "STARTED")?;
    drop(first);
    std::thread::sleep(Duration::from_secs(3));
    let mut second = server.connect_pane()?;
    let resumed = attach(&mut second, false)?;
    assert_eq!(
        (original.server, original.session, original.control),
        (resumed.server, resumed.session, resumed.control)
    );
    let output = await_output(&mut second, resumed, "CONTINUED")?;
    let pid: i32 = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("PID="))
        .ok_or("missing shell identity")?
        .parse()?;
    if pid <= 1 {
        return Err("invalid shell identity".into());
    }
    assert!(matches!(
        second.exchange(&Request::Resize {
            lease: original,
            size: PaneSize {
                columns: 90,
                rows: 25,
                cell_width: 8,
                cell_height: 18
            }
        }),
        Err(ServerError::StaleControl)
    ));
    let shutdown = std::env::args().any(|argument| argument == "--shutdown");
    let close = if shutdown {
        Request::Shutdown
    } else {
        Request::Close { lease: resumed }
    };
    assert!(matches!(second.exchange(&close)?, Reply::Closed));
    // SAFETY: signal zero observes only the test shell's existence; it never signals a process.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    println!(
        "Authenticated reconnect preserved the running command; stale resize refused; {} reaped the shell.",
        if shutdown {
            "server shutdown"
        } else {
            "explicit close"
        }
    );
    Ok(())
}
