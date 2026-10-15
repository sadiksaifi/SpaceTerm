//! Authenticated shared edits, replay, independent Pane control, and forced restart on both majors.
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "The shared-Pane exercise does not use the legacy attach helper."
)]
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm_server::{
    LocalServer, ServerError,
    hierarchy::{Axis, Id, Inventory, Mutation, Outcome},
    pane::*,
};
use std::{error::Error, time::Duration};

fn inventory(connection: &mut PaneConnection) -> Result<(Inventory, u64), Box<dyn Error>> {
    let Reply::Inventory {
        inventory,
        next_sequence,
    } = connection.exchange(&Request::Inventory)?
    else {
        return Err("inventory response".into());
    };
    Ok((inventory, next_sequence))
}
fn mutate(connection: &mut PaneConnection, mutation: Mutation) -> Result<Id, Box<dyn Error>> {
    let (_, sequence) = inventory(connection)?;
    let Reply::Outcome(result) = connection.exchange(&Request::Mutate { sequence, mutation })?
    else {
        return Err("mutation response".into());
    };
    let Outcome::Accepted { target } = result.outcome else {
        return Err("mutation rejected".into());
    };
    connection.exchange(&Request::Acknowledge { sequence })?;
    Ok(target)
}
fn select(connection: &mut PaneConnection, pane: Id, explicit: bool) -> Result<Reply, ServerError> {
    connection.exchange(&Request::SelectPane {
        pane,
        explicit,
        size: size(),
        style: None,
    })
}
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("2".into()).parse()?;
    let a = LocalServer::for_development_client("shared-smoke-a", major)?;
    let b = LocalServer::for_development_client("shared-smoke-b", major)?;
    require_idle(&a)?;
    let directory = std::env::current_exe()?
        .parent()
        .ok_or("example directory")?
        .to_owned();
    let companion = directory.join("spaceterm-server");
    if !companion.try_exists()? {
        std::os::unix::fs::symlink("../spaceterm-server", &companion)?;
    }
    a.setup_and_start()?;
    b.setup_and_start()?;
    connect(&a)?.exchange(&Request::Shutdown)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut host = Server::start()?;
    let mut first = connect(&a)?;
    let mut second = connect(&b)?;
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Shared smoke".into(),
        },
    )?;
    let (published, _) = inventory(&mut second)?;
    let w = published
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("publication")?;
    let tab = w.root_tab;
    let pane = w.tabs[0].root_pane;
    assert!(matches!(
        select(&mut second, pane, false)?,
        Reply::Takeover { .. }
    ));
    let Reply::Attached { lease: lease_a } = select(&mut first, pane, true)? else {
        return Err("first use".into());
    };
    let split = mutate(
        &mut second,
        Mutation::SplitPane {
            workspace,
            tab,
            source: pane,
            axis: Axis::Right,
        },
    )?;
    let Reply::Attached { lease: lease_b } = select(&mut second, split, true)? else {
        return Err("independent control".into());
    };
    assert!(matches!(
        select(&mut first, split, false)?,
        Reply::Takeover { .. }
    ));
    first.exchange(&Request::Resize {
        lease: lease_a,
        size: size(),
    })?;
    second.exchange(&Request::Resize {
        lease: lease_b,
        size: size(),
    })?;
    support::command(
        &mut first,
        lease_a,
        "stty -echo; printf '\\033[2J\\033[HSHARED-A\\n'",
    )?;
    support::await_output(&mut first, lease_a, "SHARED-A")?;
    support::command(
        &mut second,
        lease_b,
        "stty -echo; printf '\\033[2J\\033[HSHARED-B\\n'",
    )?;
    support::await_output(&mut second, lease_b, "SHARED-B")?;
    mutate(
        &mut first,
        Mutation::RenameWorkspace {
            workspace,
            version: 0,
            name: "Accepted name".into(),
        },
    )?;
    let (_, conflict_sequence) = inventory(&mut second)?;
    let rejected = Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "Rejected draft".into(),
    };
    assert!(
        matches!(second.exchange(&Request::Mutate { sequence: conflict_sequence, mutation: rejected.clone() })?, Reply::Outcome(o) if o.outcome == Outcome::Conflict && o.mutation == rejected)
    );
    second.exchange(&Request::Acknowledge {
        sequence: conflict_sequence,
    })?;
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some("/".into()),
        },
    )?;
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 1,
            directory: None,
        },
    )?;
    let (_, sequence) = inventory(&mut first)?;
    let creation = Mutation::CreateTab { workspace };
    // Discard the result at the application boundary and reconnect with its original identity.
    first.exchange(&Request::Mutate {
        sequence,
        mutation: creation.clone(),
    })?;
    drop(first);
    drop(second);
    host.kill()?;
    let mut host = Server::start()?;
    let mut first = connect(&a)?;
    let before = inventory(&mut first)?.0;
    let w = before
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("durability")?;
    assert_eq!(w.name, "Accepted name");
    assert_eq!(w.pin_version, 2);
    assert_eq!(w.tabs.len(), 2);
    assert_eq!(w.tabs[0].panes.len(), 2);
    assert_eq!(w.root_tab, tab);
    assert_eq!(w.tabs[0].root_pane, pane);
    assert_eq!(w.tabs[0].panes[1].source, Some(pane));
    assert!(matches!(
        select(&mut first, pane, false)?,
        Reply::Restored { .. }
    ));
    let Reply::Outcome(saved) = first.exchange(&Request::Resolve { sequence })? else {
        return Err("retained outcome".into());
    };
    let Reply::Outcome(replayed) = first.exchange(&Request::Mutate {
        sequence,
        mutation: creation,
    })?
    else {
        return Err("replay outcome".into());
    };
    assert_eq!(saved, replayed);
    assert_eq!(inventory(&mut first)?.0, before);
    first.exchange(&Request::Acknowledge { sequence })?;
    // Cleanup only the exact Panes this exercise created. Shared close UI remains #546.
    let panes: Vec<_> = w.tabs.iter().flat_map(|t| &t.panes).map(|p| p.id).collect();
    for pane in panes {
        let Reply::Attached { lease } = select(&mut first, pane, true)? else {
            return Err("cleanup first use".into());
        };
        first.exchange(&Request::Close { lease })?;
    }
    assert!(
        !inventory(&mut first)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == workspace)
    );
    first.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    println!(
        "Shared creation, publication, conflicts, replay, independent control, Roots, defaults and restart passed on protocol {major}."
    );
    Ok(())
}
