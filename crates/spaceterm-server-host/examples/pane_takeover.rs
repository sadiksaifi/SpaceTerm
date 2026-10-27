//! Real-host two-client check. Admission uses owning-account authority without client credentials.
#[expect(
    dead_code,
    reason = "the takeover check uses only Pane claims and terminal helpers"
)]
mod support;
use spaceterm_server::{
    ServerConnection, ServerError,
    connection::link::ServerLink,
    pane::{control::Controller, *},
    protocol::{Operation, Takeover, interim::Interim},
};
use std::error::Error;
use support::{Claimed, attach};

fn size(columns: u16) -> PaneSize {
    PaneSize {
        columns,
        rows: 24,
        cell_width: 8,
        cell_height: 18,
    }
}
/// Selects the first Pane, as the single-Pane `Select` request of protocol major 1 did.
fn select(link: &ServerLink) -> Result<Claimed, ServerError> {
    let pane = support::first_pane(link, false)?;
    support::select(link, pane, false, size(80))
}
/// The Takeover offer in `claimed` and the Controlling Client it names.
fn offer(claimed: Claimed) -> Result<Takeover, Box<dyn Error>> {
    match claimed {
        Claimed::Takeover(takeover) => Ok(takeover),
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
        .unwrap_or_else(|| "2".into())
        .parse()?;
    let first = ServerConnection::for_development_client("takeover-first", major)?;
    let second = ServerConnection::for_development_client("takeover-second", major)?;
    for client in [&first, &second] {
        assert_eq!(client.refresh()?.protocol_major, major);
    }
    let a = support::connect(&first)?;
    let original = attach(&a, true, size(80))?;
    original.command(
        "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 2; printf 'CONTINUED\\n'",
    )?;
    original.await_output("STARTED")?;
    let b = support::connect(&second)?;
    let takeover = offer(select(&b)?)?;
    let first_name = controlled_by(takeover.controller, "takeover-first")?;
    println!("takeover-second observed the Controlling Client {first_name:?}.");
    let pane = support::first_pane(&b, false)?;
    let Claimed::Attached(transferred) = support::claim(
        &b,
        pane,
        AttachmentClaim::TakeOver {
            target: takeover.target,
        },
        size(90),
    )?
    else {
        return Err("takeover failed".into());
    };
    assert_eq!(original.lease().session, transferred.lease().session);
    assert_ne!(original.lease().control, transferred.lease().control);
    // The former controller's channel still names its old lease, which the server now refuses.
    // Resize is input, so the server reports its refusal as a discard.
    assert_eq!(original.poll().err(), Some(ServerError::StaleControl));
    let resize = original.resize(size(70))?;
    assert_eq!(original.discarded(resize)?, Refusal::Stale);
    assert_eq!(
        original
            .request(Operation::Close {
                channel: original.channel(),
            })
            .err(),
        Some(ServerError::StaleControl)
    );
    // The former device reconnects without recovering its former grant.
    a.close();
    drop(original);
    let a = support::connect(&first)?;
    let second_name = controlled_by(offer(select(&a)?)?.controller, "takeover-second")?;
    println!("takeover-first observed the Controlling Client {second_name:?} after Take Over.");
    transferred.await_output("CONTINUED")?;
    transferred.command("stty size")?;
    transferred.await_output("24 90")?;
    let (lease, channel) = (transferred.lease(), transferred.channel());
    transferred.release();
    let released = offer(support::select(&b, pane, false, size(90))?)?.controller;
    if released != Controller::Nobody {
        return Err(
            format!("expected no Controlling Client after Release, observed {released:?}").into(),
        );
    }
    println!("takeover-second observed no Controlling Client after Release.");
    let Claimed::Attached(reclaimed) = select(&a)? else {
        return Err("unclaimed acquisition failed".into());
    };
    assert_eq!(lease.session, reclaimed.lease().session);
    assert!(matches!(select(&b)?, Claimed::Takeover(_)));
    assert_eq!(
        support::interim(&b, Interim::Poll { channel }).err(),
        Some(ServerError::StaleControl)
    );
    reclaimed.close()?;
    println!(
        "Protocol {major}: two account-local clients transferred the same running Session, named each other as the Controlling Client, refused stale content/resize/close, preserved takeover on reconnect, and acquired released control."
    );
    Ok(())
}
