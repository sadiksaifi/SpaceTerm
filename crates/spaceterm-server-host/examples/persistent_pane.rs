//! Real-host reconnect check using account-local authority and authenticated private IPC.
mod support;
use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::{error::Error, time::Duration};
use support::{attach, await_output, command};

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
