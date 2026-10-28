//! Real owning-host Git reads through two authenticated clients on protocol major 3.
#[path = "support/lifecycle.rs"]
#[expect(
    dead_code,
    reason = "Git reads start no terminal programs and claim no Panes."
)]
mod lifecycle;
use spaceterm_server::{
    ServerConnection,
    hierarchy::{CloseTarget, Mutation},
    protocol::Operation,
    repository::{ChangeTotal, DirectoryFacts, RepositoryHead},
};
use std::error::Error;
#[path = "support/repository.rs"]
mod repository;
#[expect(
    dead_code,
    reason = "Git reads use only requests on the shared connection."
)]
mod support;
use repository::*;
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("3".into()).parse()?;
    let a = ServerConnection::for_development_client("repository-smoke-a", major)?;
    let b = ServerConnection::for_development_client("repository-smoke-b", major)?;
    lifecycle::require_idle(&a)?;
    a.setup()?;
    b.setup()?;
    let mut server = lifecycle::Server::start()?;
    let repository = Repository::new()?;
    let first = connect(&a)?;
    let second = connect(&b)?;
    let workspace = mutate(
        &first,
        Mutation::CreateWorkspace {
            name: "Repository reads smoke".into(),
            directory: None,
        },
    )?;
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(repository.0.clone()),
        },
    )?;
    let tab = mutate(
        &first,
        Mutation::CreateTab {
            workspace,
            source: None,
        },
    )?;
    // Tab membership follows the Worktree listing, which the server may publish after the head.
    let initial = await_facts(&second, &repository.0, |f| {
        f.repository
            .as_ref()
            .is_some_and(|r| r.head == RepositoryHead::Branch("main".into()))
            && f.worktrees.is_some()
    })?;
    assert_eq!(
        initial
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .ok_or("Workspace")?
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .ok_or("Tab")?
            .worktree
            .as_ref(),
        Some(&repository.0)
    );
    let query = initial
        .repositories
        .as_ref()
        .and_then(|s| s.directories.iter().find(|f| f.directory == repository.0))
        .and_then(|f| f.repository.as_ref())
        .and_then(|r| r.pull_request.as_ref())
        .ok_or("parsed Pull Request target")?;
    assert_eq!(
        (
            &*query.repository.host,
            &*query.repository.owner,
            &*query.repository.name
        ),
        ("github.com", "example", "repository")
    );
    let inactive = mutate(
        &second,
        Mutation::CreateWorkspace {
            name: "Other active work".into(),
            directory: None,
        },
    )?;
    let linked = repository.0.join("linked");
    repository.git(&[
        "worktree",
        "add",
        "-b",
        "external",
        linked.to_str().ok_or("fixture path")?,
    ])?;
    std::fs::write(linked.join("external-change"), "changed outside SpaceTerm")?;
    let listed = |f: &DirectoryFacts| {
        f.worktrees
            .as_ref()
            .is_some_and(|l| l.worktrees.iter().any(|r| r.root == linked && !r.missing))
    };
    await_facts(&first, &repository.0, listed)?;
    await_facts(&second, &repository.0, listed)?;
    let changes = |f: &DirectoryFacts| {
        f.repository.as_ref().is_some_and(|r| {
            r.changes
                .as_ref()
                .is_ok_and(|changes| changes.total == ChangeTotal::Exact(1))
        })
    };
    await_facts(&first, &linked, changes)?;
    await_facts(&second, &linked, changes)?;
    // Leave only one Tab rooted in the linked Worktree. After it disappears, a fresh host
    // must use its retained locator rather than another Workspace or a live snapshot.
    mutate(
        &first,
        Mutation::PinWorkspace {
            workspace,
            version: 1,
            directory: Some(linked.clone()),
        },
    )?;
    let tab = mutate(
        &first,
        Mutation::CreateTab {
            workspace,
            source: None,
        },
    )?;
    let current = inventory(&first)?.0;
    for other in &current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("Workspace")?
        .tabs
    {
        if other.id != tab {
            close(
                &first,
                CloseTarget::Tab {
                    workspace,
                    tab: other.id,
                },
            )?;
        }
    }
    close(
        &first,
        CloseTarget::Workspace {
            workspace: inactive,
        },
    )?;
    std::fs::remove_dir_all(&linked)?;
    let missing = |f: &DirectoryFacts| {
        f.worktrees
            .as_ref()
            .is_some_and(|l| l.worktrees.iter().any(|r| r.root == linked && r.missing))
    };
    await_facts(&first, &repository.0, missing)?;
    await_facts(&second, &repository.0, missing)?;
    repository.git(&["worktree", "prune"])?;
    await_facts(&second, &repository.0, |f| {
        f.worktrees.as_ref().is_some_and(|l| l.worktrees.len() == 1)
    })?;
    assert!(!linked.exists());
    // Forced restart proves saved membership survives without restoring a program.
    server.kill()?;
    first.close();
    second.close();
    let mut restarted = lifecycle::Server::start()?;
    let first = connect(&a)?;
    let restored = await_facts(&first, &repository.0, |f| f.repository.is_some())?;
    assert_eq!(
        restored
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .ok_or("retained Workspace")?
            .tabs
            .iter()
            .find(|t| t.id == tab)
            .ok_or("retained Tab")?
            .worktree
            .as_ref(),
        Some(&linked)
    );
    assert!(!linked.exists());
    close(&first, CloseTarget::Workspace { workspace })?;
    support::done(&first, Operation::Shutdown)?;
    restarted.0.wait()?;
    println!(
        "Owning-host Git facts, inactive Workspace updates, external Worktrees, missing/pruned directories, credential-free query targets and restored membership passed through two clients on protocol {major}."
    );
    Ok(())
}
