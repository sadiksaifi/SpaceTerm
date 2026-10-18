//! Real-host two-client check. Admission uses owning-account authority without client credentials.
mod support;
use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::error::Error;
use support::{attach, await_output, command};

fn size(columns: u16) -> PaneSize {
    PaneSize {
        columns,
        rows: 24,
        cell_width: 8,
        cell_height: 18,
    }
}
fn select(connection: &mut PaneConnection, create: bool) -> Result<Reply, ServerError> {
    connection.exchange(&Request::Select {
        create,
        size: size(80),
        resume: None,
        style: None,
    })
}
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1".into())
        .parse()?;
    let first = LocalServer::for_development_client("takeover-first", major)?;
    let second = LocalServer::for_development_client("takeover-second", major)?;
    for client in [&first, &second] {
        assert_eq!(client.setup_and_start()?.protocol_major, major);
    }
    let mut a = first.connect_pane()?;
    let original = attach(&mut a, true)?;
    command(
        &mut a,
        original,
        "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 2; printf 'CONTINUED\\n'",
    )?;
    await_output(&mut a, original, "STARTED")?;
    let mut b = second.connect_pane()?;
    let Reply::Takeover { target } = select(&mut b, false)? else {
        return Err("expected Takeover Screen".into());
    };
    let Reply::Attached { lease: transferred } = b.exchange(&Request::TakeOver {
        target,
        size: size(90),
        style: None,
    })?
    else {
        return Err("takeover failed".into());
    };
    assert_eq!(original.session, transferred.session);
    assert_ne!(original.control, transferred.control);
    for request in [
        Request::Poll { lease: original },
        Request::Resize {
            lease: original,
            size: size(70),
        },
        Request::Close { lease: original },
    ] {
        assert!(matches!(
            a.exchange(&request),
            Err(ServerError::StaleControl)
        ));
    }
    // The former device reconnects without recovering its former grant.
    drop(a);
    let mut a = first.connect_pane()?;
    assert!(matches!(select(&mut a, false)?, Reply::Takeover { .. }));
    await_output(&mut b, transferred, "CONTINUED")?;
    command(&mut b, transferred, "stty size")?;
    await_output(&mut b, transferred, "24 90")?;
    b.exchange(&Request::Release { lease: transferred })?;
    let Reply::Attached { lease: reclaimed } = select(&mut a, false)? else {
        return Err("unclaimed acquisition failed".into());
    };
    assert_eq!(original.session, reclaimed.session);
    assert!(matches!(select(&mut b, false)?, Reply::Takeover { .. }));
    assert!(matches!(
        b.exchange(&Request::Poll { lease: transferred }),
        Err(ServerError::StaleControl)
    ));
    assert!(matches!(
        a.exchange(&Request::Close { lease: reclaimed })?,
        Reply::Closed
    ));
    println!(
        "Protocol {major}: two account-local clients transferred the same running Session, refused stale content/resize/close, preserved takeover on reconnect, and acquired released control."
    );
    Ok(())
}
