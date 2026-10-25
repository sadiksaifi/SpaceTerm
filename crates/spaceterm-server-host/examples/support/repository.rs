//! Shared owning-host Git fixtures and authenticated inventory checks.
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::{CloseTarget, Id, Inventory, Mutation, Outcome},
    pane::{PaneConnection, Reply, Request},
    repository::DirectoryFacts,
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
pub fn connect(server: &ServerConnection) -> Result<PaneConnection, Box<dyn Error>> {
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
pub fn inventory(connection: &mut PaneConnection) -> Result<(Inventory, u64), Box<dyn Error>> {
    let Reply::Inventory {
        inventory,
        next_sequence,
    } = connection.exchange(&Request::Inventory)?
    else {
        return Err("inventory response".into());
    };
    Ok((inventory, next_sequence))
}
pub fn mutate(connection: &mut PaneConnection, mutation: Mutation) -> Result<Id, Box<dyn Error>> {
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
pub fn close(connection: &mut PaneConnection, target: CloseTarget) -> Result<(), Box<dyn Error>> {
    let current = inventory(connection)?.0;
    let confirmation = current.close_confirmation(target).ok_or("close target")?;
    mutate(
        connection,
        Mutation::Close {
            target,
            confirmation,
        },
    )?;
    Ok(())
}
pub fn await_facts(
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
pub struct Repository(pub PathBuf);
impl Repository {
    pub fn new() -> Result<Self, Box<dyn Error>> {
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
    pub fn git(&self, args: &[&str]) -> Result<(), Box<dyn Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()?;
        if !output.status.success() {
            let command = args
                .iter()
                .find(|arg| {
                    matches!(
                        **arg,
                        "init" | "commit" | "remote" | "worktree" | "add" | "config"
                    )
                })
                .copied()
                .unwrap_or("unknown");
            let error = String::from_utf8_lossy(&output.stderr);
            let reason = if error.contains("not a working tree") {
                "not-a-worktree"
            } else if error.contains("contains modified or untracked files") {
                "dirty-worktree"
            } else if error.contains("already exists") {
                "already-exists"
            } else if error.contains("not a git repository") {
                "not-a-repository"
            } else if error.contains("not checked out") {
                "not-checked-out"
            } else {
                "unclassified"
            };
            return Err(format!(
                "fixture Git {command} failed ({reason}, exit {:?})",
                output.status.code()
            )
            .into());
        }
        Ok(())
    }
}
impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
