//! Authenticated shared edits, replay, independent Pane control, and forced restart on the initial major.
#[path = "support/lifecycle.rs"]
#[expect(
    dead_code,
    reason = "Shared edits use the multiplexed transport instead of the legacy connection helper."
)]
mod lifecycle;
#[expect(
    dead_code,
    reason = "The shared-Pane exercise does not use the legacy attach helper."
)]
mod support;
use lifecycle::{Server, require_idle, size};
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::{Axis, CloseTarget, Id, Inventory, Mutation, Outcome},
    pane::*,
};
use std::{
    error::Error,
    time::{Duration, Instant},
};

fn connect(server: &ServerConnection) -> Result<PaneConnection, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match server.connect_shared_pane() {
            Ok(connection) => return Ok(connection),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

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
fn attachment_workflow(a: &ServerConnection, b: &ServerConnection) -> Result<(), Box<dyn Error>> {
    let mut first = connect(a)?;
    let mut second = connect(b)?;
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Attachments smoke".into(),
            directory: None,
        },
    )?;
    let current = inventory(&mut first)?.0;
    let pane = current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("attachment Workspace")?
        .tabs[0]
        .root_pane;
    let choices = a.attachments()?;
    choices.reconcile(&current)?;
    choices.attach(workspace)?;
    let permit = choices.permit(pane).ok_or("attachment authority")?;
    let select = Request::SelectPane {
        pane,
        explicit: true,
        size: size(),
        style: None,
    };
    let Reply::Attached { lease } = permit.exchange(&mut first, &select)? else {
        return Err("attachment first use".into());
    };
    support::command(
        &mut first,
        lease,
        r"stty -echo; sleep 1; printf '\nDETACH-CONTINUED\n'",
    )?;
    // Do not send the release yet, as when the desktop loses its connection before detach.
    drop(first);
    choices.detach(workspace)?;
    assert!(!permit.active());
    let original = choices
        .pending()
        .into_iter()
        .find(|a| a.workspace == workspace)
        .ok_or("original release")?;
    let Reply::Attached { lease: other } = second.exchange(&Request::TakeOver {
        target: lease.into(),
        size: size(),
        style: None,
    })?
    else {
        return Err("other client's takeover".into());
    };
    let mut first = connect(a)?;
    choices.release_pending(&mut first)?;
    support::await_output(&mut second, other, "DETACH-CONTINUED")?;
    assert!(choices.pending().is_empty());
    choices.reconcile(&inventory(&mut first)?.0)?;
    assert!(!choices.is_attached(workspace));
    choices.attach(workspace)?;
    let later = choices.permit(pane).ok_or("reattachment authority")?;
    assert!(matches!(
        later.exchange(&mut first, &select)?,
        Reply::Takeover { .. }
    ));
    let Reply::Attached { lease: returned } = later.exchange(
        &mut first,
        &Request::TakeOver {
            target: other.into(),
            size: size(),
            style: None,
        },
    )?
    else {
        return Err("explicit take back".into());
    };
    assert_eq!(lease.session, returned.session);
    // Retrying a release whose acknowledgement was lost cannot revoke this later grant.
    first.exchange(&Request::ReleaseAttachment {
        attachment: original,
    })?;
    support::command(&mut first, returned, r"printf 'REATTACH-CONTINUES\n'")?;
    support::await_output(&mut first, returned, "REATTACH-CONTINUES")?;
    choices.detach(workspace)?;
    choices.release_pending(&mut first)?;
    assert!(!choices.is_attached(workspace));
    close(&mut first, CloseTarget::Workspace { workspace })?;
    choices.reconcile(&inventory(&mut first)?.0)?;
    assert!(!choices.attach(workspace)?);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("1".into()).parse()?;
    let a = ServerConnection::for_development_client("shared-smoke-a", major)?;
    let b = ServerConnection::for_development_client("shared-smoke-b", major)?;
    require_idle(&a)?;
    a.setup()?;
    b.setup()?;
    let mut host = Server::start()?;
    attachment_workflow(&a, &b)?;
    let mut first = connect(&a)?;
    let mut second = connect(&b)?;
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Shared smoke".into(),
            directory: None,
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
    let creation = Mutation::CreateTab {
        workspace,
        source: None,
    };
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
    let later_tab = w.tabs[1].id;
    let later_pane = w.tabs[1].root_pane;
    let tab_target = CloseTarget::Tab { workspace, tab };
    let stale = before.close_confirmation(tab_target).ok_or("close facts")?;
    let Reply::Attached { lease } = select(&mut first, pane, true)? else {
        return Err("restored first use".into());
    };
    let mut second = connect(&b)?;
    let (_, sequence) = inventory(&mut second)?;
    assert!(matches!(second.exchange(&Request::Mutate {
        sequence, mutation: Mutation::Close { target: tab_target, confirmation: stale }
    })?, Reply::Outcome(o) if o.outcome == Outcome::StaleConfirmation));
    second.exchange(&Request::Acknowledge { sequence })?;
    let Reply::Attached { .. } = select(&mut second, split, true)? else {
        return Err("second restored first use".into());
    };
    // Close another client's Root Pane without taking it over. The surviving split is the Root.
    close(
        &mut second,
        CloseTarget::Pane {
            workspace,
            tab,
            pane,
        },
    )?;
    let current = inventory(&mut first)?.0;
    let w = current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("surviving Workspace")?;
    assert_eq!(w.tabs[0].root_pane, split);
    assert!(matches!(
        first.exchange(&Request::Poll { lease }),
        Err(ServerError::SessionUnavailable)
    ));
    // Close the other client's Tab, preserving the later Tab and promoting it to Root.
    close(&mut first, tab_target)?;
    let current = inventory(&mut second)?.0;
    let w = current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("surviving Tab")?;
    assert_eq!(w.root_tab, later_tab);
    let Reply::Attached { .. } = select(&mut second, later_pane, true)? else {
        return Err("last Tab first use".into());
    };
    let survivor = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Survivor".into(),
            directory: None,
        },
    )?;
    let current = inventory(&mut first)?.0;
    let survivor_pane = current
        .workspaces
        .iter()
        .find(|w| w.id == survivor)
        .ok_or("survivor")?
        .tabs[0]
        .root_pane;
    let Reply::Attached {
        lease: survivor_lease,
    } = select(&mut first, survivor_pane, true)?
    else {
        return Err("survivor first use".into());
    };
    // Pin to a real disposable Git Worktree and verify that deletion never reaches its files.
    let repository = Repository::new(major)?;
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 2,
            directory: Some(repository.worktree()),
        },
    )?;
    let current = inventory(&mut first)?.0;
    let target = CloseTarget::Workspace { workspace };
    let confirmation = current
        .close_confirmation(target)
        .ok_or("Kill confirmation")?;
    let (_, sequence) = inventory(&mut first)?;
    let kill = Mutation::Close {
        target,
        confirmation,
    };
    first.exchange(&Request::Mutate {
        sequence,
        mutation: kill.clone(),
    })?;
    // Lose the acknowledgement, reconnect, and replay across a forced restart with the original
    // authority. The deleted Workspace stays absent and the retained outcome remains identical.
    first.exchange(&Request::Resize {
        lease: survivor_lease,
        size: size(),
    })?;
    drop(first);
    drop(second);
    host.kill()?;
    let mut host = Server::start()?;
    let mut first = connect(&a)?;
    let Reply::Outcome(saved) = first.exchange(&Request::Resolve { sequence })? else {
        return Err("Kill outcome".into());
    };
    let Reply::Outcome(replayed) = first.exchange(&Request::Mutate {
        sequence,
        mutation: kill,
    })?
    else {
        return Err("Kill replay".into());
    };
    assert_eq!(saved, replayed);
    assert_eq!(saved.outcome, Outcome::Accepted { target: workspace });
    first.exchange(&Request::Acknowledge { sequence })?;
    assert_eq!(
        std::fs::read(repository.worktree().join("keep"))?,
        b"repository contents"
    );
    assert!(repository.worktree().join(".git").is_file());
    assert!(
        !inventory(&mut first)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == workspace)
    );
    assert!(
        inventory(&mut first)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == survivor)
    );
    let (_, sequence) = inventory(&mut first)?;
    assert!(
        matches!(first.exchange(&Request::Mutate { sequence, mutation: Mutation::CreateTab { workspace, source: None } })?, Reply::Outcome(o) if o.outcome == Outcome::Missing)
    );
    first.exchange(&Request::Acknowledge { sequence })?;
    assert!(matches!(
        select(&mut first, later_pane, true),
        Err(ServerError::SessionUnavailable)
    ));
    close(
        &mut first,
        CloseTarget::Workspace {
            workspace: survivor,
        },
    )?;
    first.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    println!(
        "Shared edits, detach/reattach, exact control release, cross-client Kill, Root succession, lost replies, restart and Git Worktree preservation passed on protocol {major}."
    );
    Ok(())
}

fn close(connection: &mut PaneConnection, target: CloseTarget) -> Result<Id, Box<dyn Error>> {
    let current = inventory(connection)?.0;
    let confirmation = current.close_confirmation(target).ok_or("close target")?;
    mutate(
        connection,
        Mutation::Close {
            target,
            confirmation,
        },
    )
}

struct Repository(std::path::PathBuf);
impl Repository {
    fn new(major: u16) -> Result<Self, Box<dyn Error>> {
        use std::process::{Command, Stdio};
        let root =
            std::env::temp_dir().join(format!("spaceterm-close-{}-{major}", std::process::id()));
        std::fs::create_dir(&root)?;
        let repository = Self(root);
        for args in [
            vec!["init", "--quiet"],
            vec![
                "-c",
                "user.name=SpaceTerm test",
                "-c",
                "user.email=test@invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ],
            vec!["worktree", "add", "--quiet", "-b", "close-test", "worktree"],
        ] {
            if !Command::new("git")
                .args(args)
                .current_dir(&repository.0)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success()
            {
                return Err("repository fixture".into());
            }
        }
        std::fs::write(repository.worktree().join("keep"), b"repository contents")?;
        Ok(repository)
    }
    fn worktree(&self) -> std::path::PathBuf {
        self.0.join("worktree")
    }
}
impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
