//! Real-host two-client check. Admission uses owning-account authority without client credentials.
mod support;
use spaceterm_server::{ServerConnection, ServerError, pane::control::Controller, pane::*};
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
/// The Takeover offer in `reply` and the Controlling Client it names.
fn offer(reply: Reply) -> Result<(PaneTarget, Controller), Box<dyn Error>> {
    match reply {
        Reply::Takeover {
            target, controller, ..
        } => Ok((target, controller)),
        _ => Err("expected Takeover Screen".into()),
    }
}
/// The name of another client that controls the Pane, which must be the named Development client.
fn controlled_by(controller: Controller, development: &str) -> Result<String, Box<dyn Error>> {
    match controller {
        Controller::Client {
            name,
            this_client: false,
        } if name.as_str().ends_with(&format!("({development})")) => Ok(name.as_str().into()),
        other => {
            Err(format!("expected {development} to control the Pane, observed {other:?}").into())
        }
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    if !spaceterm_server::service::Service::for_identity("spaceterm-development")?
        .status()?
        .installed
    {
        return Err(ServerError::NotInstalled.into());
    }
    let major = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1".into())
        .parse()?;
    let first = ServerConnection::for_development_client("takeover-first", major)?;
    let second = ServerConnection::for_development_client("takeover-second", major)?;
    for client in [&first, &second] {
        assert_eq!(client.refresh()?.protocol_major, major);
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
    let (target, controller) = offer(select(&mut b, false)?)?;
    let first_name = controlled_by(controller, "takeover-first")?;
    println!("takeover-second observed the Controlling Client {first_name:?}.");
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
    let second_name = controlled_by(offer(select(&mut a, false)?)?.1, "takeover-second")?;
    println!("takeover-first observed the Controlling Client {second_name:?} after Take Over.");
    await_output(&mut b, transferred, "CONTINUED")?;
    command(&mut b, transferred, "stty size")?;
    await_output(&mut b, transferred, "24 90")?;
    b.exchange(&Request::Release { lease: transferred })?;
    let Reply::Inventory { inventory, .. } = b.exchange(&Request::Inventory)? else {
        return Err("inventory unavailable".into());
    };
    let pane = inventory
        .workspaces
        .first()
        .and_then(|workspace| workspace.tabs.first())
        .and_then(|tab| tab.panes.first())
        .ok_or("the Development Pane is missing")?
        .id;
    let (_, released) = offer(b.exchange(&Request::SelectPane {
        pane,
        explicit: false,
        size: size(90),
        style: None,
    })?)?;
    if released != Controller::Nobody {
        return Err(
            format!("expected no Controlling Client after Release, observed {released:?}").into(),
        );
    }
    println!("takeover-second observed no Controlling Client after Release.");
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
        "Protocol {major}: two account-local clients transferred the same running Session, named each other as the Controlling Client, refused stale content/resize/close, preserved takeover on reconnect, and acquired released control."
    );
    Ok(())
}
