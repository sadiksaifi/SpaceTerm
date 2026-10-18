//! Authenticated Git writes, stale confirmations, original-operation recovery and Stop on both majors.
#[path = "support/lifecycle.rs"]
#[expect(dead_code, reason = "This exercise uses shared hierarchy connections.")]
mod lifecycle;
#[path = "support/repository.rs"]
mod repository;
use repository::*;
use spaceterm_server::{
    LocalServer,
    hierarchy::{CloseTarget, Mutation, Outcome, RetainedOutcome},
    pane::{PaneConnection, Reply, Request},
    worktree::{Branch, BranchName, Error as WorktreeError, Operation, Read, ReadReply},
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};
fn finish(c: &mut PaneConnection, sequence: u64) -> Result<RetainedOutcome, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        match c.exchange(&Request::Resolve { sequence })? {
            Reply::Outcome(outcome) => return Ok(outcome),
            Reply::MutationPending => {}
            _ => return Err("original write outcome unavailable".into()),
        }
        if Instant::now() > until {
            return Err("write did not settle".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn reconcile_fixture_originals(c: &mut PaneConnection) -> Result<(), Box<dyn Error>> {
    let next = inventory(c)?.1;
    if next > 1024 {
        return Err("fixture has too many retained operation sequences".into());
    }
    for sequence in 1..next {
        match c.exchange(&Request::Resolve { sequence }) {
            Ok(Reply::Outcome(_)) | Ok(Reply::MutationPending) => {
                let outcome = finish(c, sequence)?;
                if outcome.outcome == Outcome::Pending {
                    return Err("fixture original did not settle".into());
                }
                eprintln!("Worktree smoke: retiring original operation {sequence}");
                c.exchange(&Request::Acknowledge { sequence })?;
            }
            Err(spaceterm_server::ServerError::SessionUnavailable) => (),
            Err(error) => return Err(error.into()),
            _ => return Err("fixture original response".into()),
        }
    }
    Ok(())
}

fn write(c: &mut PaneConnection, operation: Operation) -> Result<RetainedOutcome, Box<dyn Error>> {
    let sequence = inventory(c)?.1;
    let reply = c.exchange(&Request::Mutate {
        sequence,
        mutation: Mutation::Worktree(operation),
    })?;
    let outcome = match reply {
        Reply::Outcome(o) => o,
        Reply::MutationPending => finish(c, sequence)?,
        _ => return Err("write response".into()),
    };
    c.exchange(&Request::Acknowledge { sequence })?;
    Ok(outcome)
}
fn review(c: &mut PaneConnection, root: &Path, path: &Path) -> Result<Operation, Box<dyn Error>> {
    let review = match c.read_worktree(Read::CheckRemoval {
        root: root.into(),
        path: path.into(),
    })? {
        Ok(ReadReply::Removal(review)) => review,
        Err(error) => {
            return Err(format!("removal review failed: {error:?}").into());
        }
        _ => return Err("removal review response".into()),
    };
    Ok(Operation::Remove {
        root: root.into(),
        path: path.into(),
        boot: review.boot,
        confirmation: review.confirmation,
    })
}
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("2".into()).parse()?;
    let a = LocalServer::for_development_client("worktree-writes-a", major)?;
    let b = LocalServer::for_development_client("worktree-writes-b", major)?;
    eprintln!("Worktree smoke: idle guard");
    lifecycle::require_idle(&a)?;
    lifecycle::prepare_companion()?;
    eprintln!("Worktree smoke: first pairing");
    a.setup_and_start()?;
    eprintln!("Worktree smoke: second pairing");
    b.setup_and_start()?;
    eprintln!("Worktree smoke: stop setup server");
    connect(&a)?.exchange(&Request::Shutdown)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut server = lifecycle::Server::start()?;
    eprintln!("Worktree smoke: create fixture repository");
    let fixture = Repository::new()?;
    let mut first = connect(&a)?;
    let mut second = connect(&b)?;
    reconcile_fixture_originals(&mut first)?;
    reconcile_fixture_originals(&mut second)?;
    // A failed earlier fixture can retain a dormant Workspace after its temporary repository drops.
    // Remove only this exercise's missing fixture Workspaces, and refuse to end any live Session.
    let abandoned = inventory(&mut first)?.0;
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
                .close_confirmation(CloseTarget::Workspace {
                    workspace: workspace.id,
                })
                .ok_or("abandoned fixture confirmation")?;
            if confirmation.panes.iter().any(|pane| pane.session.is_some()) {
                return Err("abandoned fixture has live work; refusing cleanup".into());
            }
            close(
                &mut first,
                CloseTarget::Workspace {
                    workspace: workspace.id,
                },
            )?;
            eprintln!("Worktree smoke: removed a dormant abandoned fixture Workspace");
        }
    }
    assert!(first.worktree_writes());
    eprintln!("Worktree smoke: create fixture Workspace");
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Worktree writes smoke".into(),
        },
    )?;
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(fixture.0.clone()),
        },
    )?;
    let path = fixture.0.join("created");
    let operation = Operation::Create {
        workspace,
        root: fixture.0.clone(),
        path: path.clone(),
        branch: Branch::New {
            name: "created".into(),
            base: BranchName::Local("main".into()),
        },
    };
    eprintln!("Worktree smoke: submit create and lose reply");
    let sequence = inventory(&mut first)?.1;
    first.exchange(&Request::Mutate {
        sequence,
        mutation: Mutation::Worktree(operation.clone()),
    })?;
    // Lose the reply without cancelling the server's started write.
    drop(first);
    let mut first = connect(&a)?;
    let created = finish(&mut first, sequence)?;
    assert!(matches!(created.outcome, Outcome::Accepted { .. }));
    assert!(path.is_dir());
    assert!(
        matches!(first.exchange(&Request::Mutate {sequence,mutation:Mutation::Worktree(operation)})?,Reply::Outcome(o) if o==created)
    );
    first.exchange(&Request::Acknowledge { sequence })?;
    await_facts(&mut second, &fixture.0, |f| {
        f.worktrees
            .as_ref()
            .is_some_and(|l| l.worktrees.iter().any(|r| r.root == path))
    })?;
    eprintln!("Worktree smoke: review removal");
    let removal = review(&mut first, &fixture.0, &path)?;
    eprintln!("Worktree smoke: external removal");
    fixture.git(&["worktree", "remove", path.to_str().ok_or("path")?])?;
    fixture.git(&["worktree", "add", path.to_str().ok_or("path")?, "created"])?;
    assert_eq!(
        write(&mut first, removal)?.outcome,
        Outcome::WorktreeFailed(WorktreeError::Changed)
    );
    assert!(path.is_dir());
    let removal = review(&mut first, &fixture.0, &path)?;
    assert!(matches!(
        write(&mut first, removal)?.outcome,
        Outcome::Accepted { .. }
    ));
    assert!(!path.exists());
    await_facts(&mut second, &fixture.0, |f| {
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
    let sequence = inventory(&mut first)?.1;
    let mutation = Mutation::Worktree(Operation::Create {
        workspace,
        root: fixture.0.clone(),
        path: path.clone(),
        branch: Branch::New {
            name: "interrupted".into(),
            base: BranchName::Local("main".into()),
        },
    });
    first.exchange(&Request::Mutate {
        sequence,
        mutation: mutation.clone(),
    })?;
    let until = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        if Instant::now() > until {
            return Err("Git filter did not start".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    server.kill()?;
    drop(first);
    drop(second);
    let mut restarted = lifecycle::Server::start()?;
    // Recovery stays fenced while the original Git child still holds the inherited write lock.
    std::thread::sleep(Duration::from_millis(200));
    assert!(a.connect_pane().is_err());
    std::fs::write(&release, "")?;
    let until = Instant::now() + Duration::from_secs(10);
    let mut first = loop {
        match connect(&a) {
            Ok(c) => break c,
            Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e),
        }
    };
    let recovered = finish(&mut first, sequence)?;
    assert_eq!(
        recovered.outcome,
        Outcome::WorktreeFailed(WorktreeError::Interrupted)
    );
    let recovered_inventory =
        await_facts(&mut first, &fixture.0, |facts| facts.worktrees.is_some())?;
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
    assert!(
        matches!(first.exchange(&Request::Mutate {sequence,mutation})?,Reply::Outcome(o) if o==recovered)
    );
    assert!(!path.exists());
    first.exchange(&Request::Acknowledge { sequence })?;
    eprintln!("Worktree smoke: stop during write");
    // Stop must wait for a Git write whose account filter is still running.
    std::fs::remove_file(&started)?;
    std::fs::remove_file(&release)?;
    let stopped_path = fixture.0.join("stopped");
    let sequence = inventory(&mut first)?.1;
    let mutation = Mutation::Worktree(Operation::Create {
        workspace,
        root: fixture.0.clone(),
        path: stopped_path.clone(),
        branch: Branch::New {
            name: "stopped".into(),
            base: BranchName::Local("main".into()),
        },
    });
    first.exchange(&Request::Mutate {
        sequence,
        mutation: mutation.clone(),
    })?;
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
    first.exchange(&Request::Shutdown)?;
    release_worker
        .join()
        .map_err(|_| "filter release failed")??;
    restarted.0.wait()?;
    assert_eq!(std::fs::read(stopped_path.join("payload"))?, b"payload\n");
    drop(first);
    let mut restarted = lifecycle::Server::start()?;
    let mut first = connect(&a)?;
    let stopped = finish(&mut first, sequence)?;
    assert!(matches!(stopped.outcome, Outcome::Accepted { .. }));
    fixture.git(&[
        "worktree",
        "remove",
        "--force",
        stopped_path.to_str().ok_or("path")?,
    ])?;
    assert!(
        matches!(first.exchange(&Request::Mutate {sequence,mutation})?,Reply::Outcome(o) if o==stopped)
    );
    assert!(!stopped_path.exists());
    first.exchange(&Request::Acknowledge { sequence })?;
    close(&mut first, CloseTarget::Workspace { workspace })?;
    first.exchange(&Request::Shutdown)?;
    restarted.0.wait()?;
    println!(
        "Worktree creation/removal, two-client Git reconciliation, replacement refusal, lost replies, forced interruption, Stop during a started write and original-operation replay passed on protocol {major}."
    );
    Ok(())
}
