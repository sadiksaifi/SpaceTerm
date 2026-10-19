//! Real owning-host Git reads through two authenticated clients on either supported major.
#[path = "support/lifecycle.rs"]
#[expect(
    dead_code,
    reason = "Git reads do not start terminal programs or use legacy Pane connections."
)]
mod lifecycle;
use spaceterm_server::{
    LocalServer,
    hierarchy::{CloseTarget, Mutation},
    pane::Request,
    repository::{ChangeTotal, DirectoryFacts, RepositoryHead},
};
use std::error::Error;
#[path = "support/repository.rs"]
mod repository;
use repository::*;
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("1".into()).parse()?;
    let a = LocalServer::for_development_client("repository-smoke-a", major)?;
    let b = LocalServer::for_development_client("repository-smoke-b", major)?;
    lifecycle::require_idle(&a)?;
    a.setup()?;
    b.setup()?;
    let mut server = lifecycle::Server::start()?;
    let repository = Repository::new()?;
    let mut first = connect(&a)?;
    let mut second = connect(&b)?;
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Repository reads smoke".into(),
            directory: None,
        },
    )?;
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(repository.0.clone()),
        },
    )?;
    let tab = mutate(
        &mut first,
        Mutation::CreateTab {
            workspace,
            source: None,
        },
    )?;
    let initial = await_facts(&mut second, &repository.0, |f| {
        f.repository
            .as_ref()
            .is_some_and(|r| r.head == RepositoryHead::Branch("main".into()))
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
        &mut second,
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
    await_facts(&mut first, &repository.0, listed)?;
    await_facts(&mut second, &repository.0, listed)?;
    let changes = |f: &DirectoryFacts| {
        f.repository
            .as_ref()
            .is_some_and(|r| r.changes.total == ChangeTotal::Exact(1))
    };
    await_facts(&mut first, &linked, changes)?;
    await_facts(&mut second, &linked, changes)?;
    // Leave only one Tab rooted in the linked Worktree. After it disappears, a fresh host
    // must use its retained locator rather than another Workspace or a live snapshot.
    mutate(
        &mut first,
        Mutation::PinWorkspace {
            workspace,
            version: 1,
            directory: Some(linked.clone()),
        },
    )?;
    let tab = mutate(
        &mut first,
        Mutation::CreateTab {
            workspace,
            source: None,
        },
    )?;
    let current = inventory(&mut first)?.0;
    for other in &current
        .workspaces
        .iter()
        .find(|w| w.id == workspace)
        .ok_or("Workspace")?
        .tabs
    {
        if other.id != tab {
            close(
                &mut first,
                CloseTarget::Tab {
                    workspace,
                    tab: other.id,
                },
            )?;
        }
    }
    close(
        &mut first,
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
    await_facts(&mut first, &repository.0, missing)?;
    await_facts(&mut second, &repository.0, missing)?;
    repository.git(&["worktree", "prune"])?;
    await_facts(&mut second, &repository.0, |f| {
        f.worktrees.as_ref().is_some_and(|l| l.worktrees.len() == 1)
    })?;
    assert!(!linked.exists());
    // Forced restart proves saved membership survives without restoring a program.
    server.kill()?;
    drop(first);
    drop(second);
    let mut restarted = lifecycle::Server::start()?;
    let mut first = connect(&a)?;
    let restored = await_facts(&mut first, &repository.0, |f| f.repository.is_some())?;
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
    close(&mut first, CloseTarget::Workspace { workspace })?;
    first.exchange(&Request::Shutdown)?;
    restarted.0.wait()?;
    println!(
        "Owning-host Git facts, inactive Workspace updates, external Worktrees, missing/pruned directories, credential-free query targets and restored membership passed through two clients on protocol {major}."
    );
    Ok(())
}
