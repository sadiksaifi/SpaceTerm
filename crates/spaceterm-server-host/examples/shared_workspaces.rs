//! Authenticated shared edits, replay, independent Pane control, and forced restart on protocol major 2.
#[path = "support/lifecycle.rs"]
#[expect(
    dead_code,
    reason = "Shared edits use the handle's shared connection instead of the fixture connection helper."
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
    attachments::AttachmentPermit,
    connection::link::ServerLink,
    hierarchy::{Axis, CloseTarget, Id, Inventory, Mutation, Outcome},
    pane::*,
    protocol::{Operation, PaneEvent},
};
use std::{
    error::Error,
    time::{Duration, Instant},
};
use support::{Claimed, Pane};

fn connect(server: &ServerConnection) -> Result<ServerLink, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match server.link() {
            Ok(link) => return Ok(link),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn inventory(link: &ServerLink) -> Result<(Inventory, u64), Box<dyn Error>> {
    Ok(support::inventory(link)?)
}
fn mutate(link: &ServerLink, mutation: Mutation) -> Result<Id, Box<dyn Error>> {
    let (_, sequence) = inventory(link)?;
    let result = support::mutate(link, sequence, mutation)?;
    let Outcome::Accepted { target } = result.outcome else {
        return Err("mutation rejected".into());
    };
    link.acknowledge(sequence)?;
    Ok(target)
}
fn select(link: &ServerLink, pane: Id, explicit: bool) -> Result<Claimed, ServerError> {
    support::select(link, pane, explicit, size())
}
/// Claims `pane` under `permit`'s attachment, as the desktop does. Releases of earlier
/// attachments go first.
fn permitted(
    link: &ServerLink,
    choices: &spaceterm_server::attachments::Attachments,
    permit: &AttachmentPermit,
    pane: Id,
    claim: AttachmentClaim,
) -> Result<Claimed, ServerError> {
    if !permit.active() {
        return Err(ServerError::SessionUnavailable);
    }
    choices.release_pending(link)?;
    support::claim_in(link, permit.attachment(), pane, claim, size())
}
fn attached(claimed: Claimed, failure: &'static str) -> Result<Pane, Box<dyn Error>> {
    claimed.attached().ok_or_else(|| failure.into())
}
fn attachment_workflow(a: &ServerConnection, b: &ServerConnection) -> Result<(), Box<dyn Error>> {
    let first = connect(a)?;
    let second = connect(b)?;
    let workspace = mutate(
        &first,
        Mutation::CreateWorkspace {
            name: "Attachments smoke".into(),
            directory: None,
        },
    )?;
    let current = inventory(&first)?.0;
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
    let select = AttachmentClaim::Select { explicit: true };
    let started = attached(
        permitted(&first, &choices, &permit, pane, select.clone())?,
        "attachment first use",
    )?;
    started.command(r"stty -echo; sleep 1; printf '\nDETACH-CONTINUED\n'")?;
    let lease = started.lease();
    // Input has no reply, so a request behind it confirms the server took the command.
    support::round_trip(&first)?;
    // Do not send the release yet, as when the desktop loses its connection before detach.
    first.close();
    drop(started);
    choices.detach(workspace)?;
    assert!(!permit.active());
    let original = choices
        .pending()
        .into_iter()
        .find(|a| a.workspace == workspace)
        .ok_or("original release")?;
    let other = attached(
        support::claim(
            &second,
            pane,
            AttachmentClaim::TakeOver {
                target: lease.into(),
            },
            size(),
        )?,
        "other client's takeover",
    )?;
    let first = connect(a)?;
    choices.release_pending(&first)?;
    other.await_output("DETACH-CONTINUED")?;
    assert!(choices.pending().is_empty());
    choices.reconcile(&inventory(&first)?.0)?;
    assert!(!choices.is_attached(workspace));
    choices.attach(workspace)?;
    let later = choices.permit(pane).ok_or("reattachment authority")?;
    assert!(matches!(
        permitted(&first, &choices, &later, pane, select)?,
        Claimed::Takeover(_)
    ));
    let returned = attached(
        permitted(
            &first,
            &choices,
            &later,
            pane,
            AttachmentClaim::TakeOver {
                target: other.lease().into(),
            },
        )?,
        "explicit take back",
    )?;
    assert_eq!(lease.session, returned.lease().session);
    // Retrying a release whose acknowledgement was lost cannot revoke this later grant.
    support::done(
        &first,
        Operation::ReleaseAttachment {
            attachment: original,
        },
    )?;
    returned.command(r"printf 'REATTACH-CONTINUES\n'")?;
    returned.await_output("REATTACH-CONTINUES")?;
    choices.detach(workspace)?;
    choices.release_pending(&first)?;
    assert!(!choices.is_attached(workspace));
    close(&first, CloseTarget::Workspace { workspace })?;
    choices.reconcile(&inventory(&first)?.0)?;
    assert!(!choices.attach(workspace)?);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("2".into()).parse()?;
    let a = ServerConnection::for_development_client("shared-smoke-a", major)?;
    let b = ServerConnection::for_development_client("shared-smoke-b", major)?;
    require_idle(&a)?;
    a.setup()?;
    b.setup()?;
    let mut host = Server::start()?;
    attachment_workflow(&a, &b)?;
    let first = connect(&a)?;
    let second = connect(&b)?;
    let workspace = mutate(
        &first,
        Mutation::CreateWorkspace {
            name: "Shared smoke".into(),
            directory: None,
        },
    )?;
    let (published, _) = inventory(&second)?;
    let w = published
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("publication")?;
    let tab = w.root_tab;
    let pane = w.tabs[0].root_pane;
    assert!(matches!(
        select(&second, pane, false)?,
        Claimed::Takeover(_)
    ));
    let pane_a = attached(select(&first, pane, true)?, "first use")?;
    let split = mutate(
        &second,
        Mutation::SplitPane {
            workspace,
            tab,
            source: pane,
            axis: Axis::Right,
        },
    )?;
    let pane_b = attached(select(&second, split, true)?, "independent control")?;
    assert!(matches!(
        select(&first, split, false)?,
        Claimed::Takeover(_)
    ));
    pane_a.resize(size())?;
    pane_b.resize(size())?;
    pane_a.command("stty -echo; printf '\\033[2J\\033[HSHARED-A\\n'")?;
    pane_a.await_output("SHARED-A")?;
    pane_b.command("stty -echo; printf '\\033[2J\\033[HSHARED-B\\n'")?;
    pane_b.await_output("SHARED-B")?;
    mutate(
        &first,
        Mutation::RenameWorkspace {
            workspace,
            version: 0,
            name: "Accepted name".into(),
        },
    )?;
    let (_, conflict_sequence) = inventory(&second)?;
    let rejected = Mutation::RenameWorkspace {
        workspace,
        version: 0,
        name: "Rejected draft".into(),
    };
    assert!(
        matches!(support::mutate(&second, conflict_sequence, rejected.clone())?, o if o.outcome == Outcome::Conflict && o.mutation == rejected)
    );
    second.acknowledge(conflict_sequence)?;
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some("/".into()),
        },
    )?;
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 1,
            directory: None,
        },
    )?;
    let (_, sequence) = inventory(&first)?;
    let creation = Mutation::CreateTab {
        workspace,
        source: None,
    };
    // Discard the result at the application boundary and reconnect with its original identity.
    support::mutate(&first, sequence, creation.clone())?;
    first.close();
    second.close();
    drop((pane_a, pane_b));
    host.kill()?;
    let mut host = Server::start()?;
    let first = connect(&a)?;
    let before = inventory(&first)?.0;
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
    assert!(matches!(select(&first, pane, false)?, Claimed::Restored(_)));
    let saved = support::resolve(&first, sequence)?;
    let replayed = support::mutate(&first, sequence, creation)?;
    assert_eq!(saved, replayed);
    assert_eq!(inventory(&first)?.0, before);
    first.acknowledge(sequence)?;
    let later_tab = w.tabs[1].id;
    let later_pane = w.tabs[1].root_pane;
    let tab_target = CloseTarget::Tab { workspace, tab };
    let stale = before.close_confirmation(tab_target).ok_or("close facts")?;
    let restored = attached(select(&first, pane, true)?, "restored first use")?;
    let second = connect(&b)?;
    let (_, sequence) = inventory(&second)?;
    assert!(matches!(support::mutate(
        &second, sequence, Mutation::Close { target: tab_target, confirmation: stale }
    )?, o if o.outcome == Outcome::StaleConfirmation));
    second.acknowledge(sequence)?;
    let Claimed::Attached(_split) = select(&second, split, true)? else {
        return Err("second restored first use".into());
    };
    // Close another client's Root Pane without taking it over. The surviving split is the Root.
    close(
        &second,
        CloseTarget::Pane {
            workspace,
            tab,
            pane,
        },
    )?;
    let current = inventory(&first)?.0;
    let w = current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("surviving Workspace")?;
    assert_eq!(w.tabs[0].root_pane, split);
    assert!(matches!(
        restored.ending()?,
        support::Ending::Pane(PaneEvent::Ended)
    ));
    // Close the other client's Tab, preserving the later Tab and promoting it to Root.
    close(&first, tab_target)?;
    let current = inventory(&second)?.0;
    let w = current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("surviving Tab")?;
    assert_eq!(w.root_tab, later_tab);
    let Claimed::Attached(_later) = select(&second, later_pane, true)? else {
        return Err("last Tab first use".into());
    };
    let survivor = mutate(
        &first,
        Mutation::CreateWorkspace {
            name: "Survivor".into(),
            directory: None,
        },
    )?;
    let current = inventory(&first)?.0;
    let survivor_pane = current
        .workspaces
        .iter()
        .find(|w| w.id == survivor)
        .ok_or("survivor")?
        .tabs[0]
        .root_pane;
    let survivor_shell = attached(select(&first, survivor_pane, true)?, "survivor first use")?;
    // Pin to a real disposable Git Worktree and verify that deletion never reaches its files.
    let repository = Repository::new(major)?;
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 2,
            directory: Some(repository.worktree()),
        },
    )?;
    let current = inventory(&first)?.0;
    let target = CloseTarget::Workspace { workspace };
    let confirmation = current
        .close_confirmation(target)
        .ok_or("Kill confirmation")?;
    let (_, sequence) = inventory(&first)?;
    let kill = Mutation::Close {
        target,
        confirmation,
    };
    support::mutate(&first, sequence, kill.clone())?;
    // Lose the acknowledgement, reconnect, and replay across a forced restart with the original
    // authority. The deleted Workspace stays absent and the retained outcome remains identical.
    survivor_shell.resize(size())?;
    // Input has no reply, so a request behind it confirms the server took it before the restart.
    support::round_trip(&first)?;
    first.close();
    second.close();
    drop(survivor_shell);
    host.kill()?;
    let mut host = Server::start()?;
    let first = connect(&a)?;
    let saved = support::resolve(&first, sequence)?;
    let replayed = support::mutate(&first, sequence, kill)?;
    assert_eq!(saved, replayed);
    assert_eq!(saved.outcome, Outcome::Accepted { target: workspace });
    first.acknowledge(sequence)?;
    assert_eq!(
        std::fs::read(repository.worktree().join("keep"))?,
        b"repository contents"
    );
    assert!(repository.worktree().join(".git").is_file());
    assert!(
        !inventory(&first)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == workspace)
    );
    assert!(
        inventory(&first)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == survivor)
    );
    let (_, sequence) = inventory(&first)?;
    assert!(
        matches!(support::mutate(&first, sequence, Mutation::CreateTab { workspace, source: None })?, o if o.outcome == Outcome::Missing)
    );
    first.acknowledge(sequence)?;
    assert!(matches!(
        select(&first, later_pane, true),
        Err(ServerError::SessionUnavailable)
    ));
    close(
        &first,
        CloseTarget::Workspace {
            workspace: survivor,
        },
    )?;
    support::done(&first, Operation::Shutdown)?;
    host.0.wait()?;
    println!(
        "Shared edits, detach/reattach, exact control release, cross-client Kill, Root succession, lost replies, restart and Git Worktree preservation passed on protocol {major}."
    );
    Ok(())
}

fn close(link: &ServerLink, target: CloseTarget) -> Result<Id, Box<dyn Error>> {
    let current = inventory(link)?.0;
    let confirmation = current.close_confirmation(target).ok_or("close target")?;
    mutate(
        link,
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
