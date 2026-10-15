//! Real owning-host Git reads through two authenticated clients on either supported major.
#[path = "support/lifecycle.rs"]
#[expect(
    dead_code,
    reason = "Git reads do not start terminal programs or use legacy Pane connections."
)]
mod lifecycle;
use spaceterm_server::{
    LocalServer, ServerError,
    hierarchy::{CloseTarget, Id, Inventory, Mutation, Outcome},
    pane::{PaneConnection, Reply, Request},
    repository::{ChangeTotal, DirectoryFacts, RepositoryHead},
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn connect(server: &LocalServer) -> Result<PaneConnection, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match server.connect_shared_pane() {
            Ok(connection) => return Ok(connection),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50))
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
fn await_facts(
    connection: &mut PaneConnection,
    directory: &Path,
    predicate: impl Fn(&DirectoryFacts) -> bool,
) -> Result<Inventory, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let (current, _) = inventory(connection)?;
        if current.repositories.as_ref().is_some_and(|s| {
            s.directories
                .iter()
                .any(|f| f.directory == directory && predicate(f))
        }) {
            return Ok(current);
        }
        if Instant::now() >= until {
            return Err("server Git facts did not converge".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
struct Repository(PathBuf);
impl Repository {
    fn new() -> Result<Self, Box<dyn Error>> {
        let path = std::env::temp_dir().join(format!("spaceterm-git-smoke-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        let fixture = Self(std::fs::canonicalize(path)?);
        fixture.git(&["init", "-b", "main"])?;
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ])?;
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://user:never-publish@github.com/example/repository.git",
        ])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> Result<(), Box<dyn Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()?;
        if !output.status.success() {
            return Err("fixture Git command failed".into());
        }
        Ok(())
    }
}
impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let major = std::env::args().nth(1).unwrap_or("2".into()).parse()?;
    let a = LocalServer::for_development_client("repository-smoke-a", major)?;
    let b = LocalServer::for_development_client("repository-smoke-b", major)?;
    lifecycle::require_idle(&a)?;
    let examples = std::env::current_exe()?
        .parent()
        .ok_or("example directory")?
        .to_owned();
    let companion = examples.join("spaceterm-server");
    if !companion.try_exists()? {
        std::os::unix::fs::symlink("../spaceterm-server", companion)?;
    }
    a.setup_and_start()?;
    b.setup_and_start()?;
    connect(&a)?.exchange(&Request::Shutdown)?;
    std::thread::sleep(Duration::from_millis(200));
    let mut server = lifecycle::Server::start()?;
    let repository = Repository::new()?;
    let mut first = connect(&a)?;
    let mut second = connect(&b)?;
    let workspace = mutate(
        &mut first,
        Mutation::CreateWorkspace {
            name: "Repository reads smoke".into(),
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
    let tab = mutate(&mut first, Mutation::CreateTab { workspace })?;
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
        Some(&repository.0)
    );
    for workspace in [workspace, inactive] {
        let current = inventory(&mut first)?.0;
        let target = CloseTarget::Workspace { workspace };
        let confirmation = current.close_confirmation(target).ok_or("close target")?;
        mutate(
            &mut first,
            Mutation::Close {
                target,
                confirmation,
            },
        )?;
    }
    first.exchange(&Request::Shutdown)?;
    restarted.0.wait()?;
    println!(
        "Owning-host Git facts, inactive Workspace updates, external Worktrees, missing/pruned directories, credential-free query targets and restored membership passed through two clients on protocol {major}."
    );
    Ok(())
}
