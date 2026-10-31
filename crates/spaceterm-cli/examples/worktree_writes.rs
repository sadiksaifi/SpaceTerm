//! Authenticated Git writes, stale confirmations, original-operation recovery and Stop.
#[path = "support/lifecycle.rs"]
#[expect(dead_code, reason = "This exercise uses shared hierarchy connections.")]
mod lifecycle;
#[path = "support/repository.rs"]
mod repository;
use repository::*;
#[expect(
    dead_code,
    reason = "Worktree writes use only requests on the shared connection."
)]
mod support;
#[path = "support/worktree.rs"]
#[expect(
    dead_code,
    reason = "this exercise manages its interrupted originals explicitly"
)]
mod worktree_fixture;
use spaceterm_server::{
    ServerConnection,
    connection::link::ServerLink,
    hierarchy::{CloseTarget, Mutation, Outcome},
    protocol,
    worktree::{Branch, BranchName, Error as WorktreeError, Operation},
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};
use worktree_fixture::{finish, review, write};
fn reconcile_fixture_originals(c: &ServerLink) -> Result<(), Box<dyn Error>> {
    let next = inventory(c)?.1;
    if next > 1024 {
        return Err("fixture has too many retained operation sequences".into());
    }
    for sequence in 1..next {
        match finish(c, sequence) {
            Ok(outcome) => {
                if outcome.outcome == Outcome::Pending {
                    return Err("fixture original did not settle".into());
                }
                eprintln!("Worktree smoke: retiring original operation {sequence}");
                c.acknowledge(sequence)?;
            }
            Err(error)
                if error.downcast_ref::<spaceterm_server::ServerError>()
                    == Some(&spaceterm_server::ServerError::SessionUnavailable) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let major = spaceterm_server::protocol::MAJOR;
    let a = ServerConnection::for_development_client("worktree-writes-a")?;
    let b = ServerConnection::for_development_client("worktree-writes-b")?;
    eprintln!("Worktree smoke: idle guard");
    lifecycle::require_idle(&a)?;
    eprintln!("Worktree smoke: first client setup");
    a.setup()?;
    eprintln!("Worktree smoke: second client setup");
    b.setup()?;
    let mut server = lifecycle::Server::start()?;
    eprintln!("Worktree smoke: create fixture repository");
    let fixture = Repository::new()?;
    let first = connect(&a)?;
    let second = connect(&b)?;
    reconcile_fixture_originals(&first)?;
    reconcile_fixture_originals(&second)?;
    // A failed earlier fixture can retain a dormant Workspace after its temporary repository drops.
    // Remove only this exercise's missing fixture Workspaces, and refuse to end any live Session.
    let abandoned = inventory(&first)?.0;
    for workspace in &abandoned.workspaces {
        if workspace.name == "Worktree writes smoke"
            && workspace.pinned_directory.as_ref().is_some_and(|path| {
                path.parent() == std::fs::canonicalize(std::env::temp_dir()).ok().as_deref()
                    && path.file_name().is_some_and(|name| {
                        name.to_string_lossy().starts_with("spaceterm-git-smoke-")
                    })
                    && !path.exists()
            })
        {
            let confirmation = abandoned
                .close_confirmation(CloseTarget::KillWorkspace {
                    workspace: workspace.id,
                })
                .ok_or("abandoned fixture confirmation")?;
            if confirmation.panes.iter().any(|pane| pane.session.is_some()) {
                return Err("abandoned fixture has live work; refusing cleanup".into());
            }
            close(
                &first,
                CloseTarget::KillWorkspace {
                    workspace: workspace.id,
                },
            )?;
            eprintln!("Worktree smoke: removed a dormant abandoned fixture Workspace");
        }
    }
    eprintln!("Worktree smoke: create fixture Workspace");
    let workspace = mutate(
        &first,
        Mutation::CreateWorkspace {
            name: "Worktree writes smoke".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    )?;
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(fixture.0.clone()),
        },
    )?;
    let path = fixture.0.join("created");
    let operation = Operation::Create {
        control: spaceterm_server::hierarchy::CreateControl::Creator,
        workspace,
        root: fixture.0.clone(),
        path: path.clone(),
        branch: Branch::New {
            name: "created".into(),
            base: BranchName::Local("main".into()),
        },
    };
    eprintln!("Worktree smoke: submit create and lose reply");
    let sequence = inventory(&first)?.1;
    support::submit(&first, sequence, Mutation::Worktree(operation.clone()))?;
    // Confirm admission before losing the reply, without reading the original reply.
    support::round_trip(&first)?;
    first.close();
    let first = connect(&a)?;
    let created = finish(&first, sequence)?;
    assert!(matches!(created.outcome, Outcome::Accepted { .. }));
    assert!(path.is_dir());
    assert!(
        matches!(support::mutate(&first, sequence, Mutation::Worktree(operation))?, o if o == created)
    );
    first.acknowledge(sequence)?;
    await_facts(&second, &fixture.0, |f| {
        f.worktrees
            .as_ref()
            .is_some_and(|l| l.worktrees.iter().any(|r| r.root == path))
    })?;
    eprintln!("Worktree smoke: review removal");
    let removal = review(&first, &fixture.0, &path)?;
    eprintln!("Worktree smoke: external removal");
    fixture.git(&["worktree", "remove", path.to_str().ok_or("path")?])?;
    fixture.git(&["worktree", "add", path.to_str().ok_or("path")?, "created"])?;
    assert_eq!(
        write(&first, removal)?.outcome,
        Outcome::WorktreeFailed(WorktreeError::Changed)
    );
    assert!(path.is_dir());
    let removal = review(&first, &fixture.0, &path)?;
    assert!(matches!(
        write(&first, removal)?.outcome,
        Outcome::Accepted { .. }
    ));
    assert!(!path.exists());
    await_facts(&second, &fixture.0, |f| {
        f.worktrees
            .as_ref()
            .is_some_and(|l| l.worktrees.iter().all(|r| r.root != path))
    })?;
    eprintln!("Worktree smoke: forced interruption");
    // Retained original outcomes cannot recreate state after a forced server restart.
    let path = fixture.0.join("interrupted");
    std::fs::write(fixture.0.join(".gitattributes"), "payload filter=wait\n")?;
    std::fs::write(fixture.0.join("payload"), "payload\n")?;
    fixture.git(&["add", ".gitattributes", "payload"])?;
    fixture.git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@invalid",
        "commit",
        "-m",
        "filter fixture",
    ])?;
    let started = fixture.0.join("started");
    let release = fixture.0.join("release");
    struct Release(std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "");
        }
    }
    let _release = Release(release.clone());
    // Git invokes the account's configured filter, as it does in ordinary Worktree creation.
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let filter = format!(
        "touch {}; while [ ! -e {} ]; do sleep 0.02; done; cat",
        quote(&started),
        quote(&release)
    );
    fixture.git(&["config", "filter.wait.smudge", &filter])?;
    let sequence = inventory(&first)?.1;
    let mutation = Mutation::Worktree(Operation::Create {
        control: spaceterm_server::hierarchy::CreateControl::Creator,
        workspace,
        root: fixture.0.clone(),
        path: path.clone(),
        branch: Branch::New {
            name: "interrupted".into(),
            base: BranchName::Local("main".into()),
        },
    });
    let _reply = support::submit(&first, sequence, mutation.clone())?;
    let until = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        if Instant::now() > until {
            return Err("Git filter did not start".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    server.kill()?;
    first.close();
    second.close();
    let mut restarted = lifecycle::Server::start()?;
    // Recovery stays fenced while the original Git child still holds the inherited write lock.
    // Admission does not reach the host, so the fence refuses the first request.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        support::connect(&a)
            .and_then(|link| support::inventory(&link))
            .is_err()
    );
    std::fs::write(&release, "")?;
    let until = Instant::now() + Duration::from_secs(10);
    let first = loop {
        match connect(&a).and_then(|link| inventory(&link).map(|_| link)) {
            Ok(c) => break c,
            Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    };
    let recovered = finish(&first, sequence)?;
    assert_eq!(
        recovered.outcome,
        Outcome::WorktreeFailed(WorktreeError::Interrupted)
    );
    let recovered_inventory = await_facts(&first, &fixture.0, |facts| facts.worktrees.is_some())?;
    let listed = recovered_inventory
        .repositories
        .as_ref()
        .is_some_and(|snapshot| {
            snapshot
                .directories
                .iter()
                .filter(|facts| facts.directory == fixture.0)
                .filter_map(|facts| facts.worktrees.as_ref())
                .any(|listing| listing.worktrees.iter().any(|tree| tree.root == path))
        });
    if listed {
        fixture.git(&[
            "worktree",
            "remove",
            "--force",
            path.to_str().ok_or("path")?,
        ])?;
    } else if path.exists() {
        // Git can roll back its registration after losing the parent's output pipes.
        // This directory belongs only to this disposable fixture, after its original child exited.
        std::fs::remove_dir_all(&path)?;
    }
    eprintln!(
        "Worktree smoke: forced original reconciled (listed before fixture cleanup: {listed})"
    );
    assert!(matches!(support::mutate(&first, sequence, mutation)?, o if o == recovered));
    assert!(!path.exists());
    first.acknowledge(sequence)?;
    eprintln!("Worktree smoke: stop during write");
    // Stop must wait for a Git write whose account filter is still running.
    std::fs::remove_file(&started)?;
    std::fs::remove_file(&release)?;
    let stopped_path = fixture.0.join("stopped");
    let sequence = inventory(&first)?.1;
    let mutation = Mutation::Worktree(Operation::Create {
        control: spaceterm_server::hierarchy::CreateControl::Creator,
        workspace,
        root: fixture.0.clone(),
        path: stopped_path.clone(),
        branch: Branch::New {
            name: "stopped".into(),
            base: BranchName::Local("main".into()),
        },
    });
    let _reply = support::submit(&first, sequence, mutation.clone())?;
    let until = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        if Instant::now() > until {
            return Err("Stop fixture filter did not start".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let release_path = release.clone();
    let release_worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(release_path, "")
    });
    support::done(&first, protocol::Operation::Shutdown)?;
    release_worker
        .join()
        .map_err(|_| "filter release failed")??;
    restarted.0.wait()?;
    assert_eq!(std::fs::read(stopped_path.join("payload"))?, b"payload\n");
    first.close();
    let mut restarted = lifecycle::Server::start()?;
    let first = connect(&a)?;
    let stopped = finish(&first, sequence)?;
    assert!(matches!(stopped.outcome, Outcome::Accepted { .. }));
    fixture.git(&[
        "worktree",
        "remove",
        "--force",
        stopped_path.to_str().ok_or("path")?,
    ])?;
    assert!(matches!(support::mutate(&first, sequence, mutation)?, o if o == stopped));
    assert!(!stopped_path.exists());
    first.acknowledge(sequence)?;
    close(&first, CloseTarget::KillWorkspace { workspace })?;
    support::done(&first, protocol::Operation::Shutdown)?;
    restarted.0.wait()?;
    println!(
        "Worktree creation/removal, two-client Git reconciliation, replacement refusal, lost replies, forced interruption, Stop during a started write and original-operation replay passed on protocol {major}."
    );
    Ok(())
}
