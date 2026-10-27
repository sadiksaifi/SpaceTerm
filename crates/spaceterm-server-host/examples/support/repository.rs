//! Shared owning-host Git fixtures and authenticated inventory checks.
use spaceterm_server::{
    ServerConnection, ServerError,
    connection::link::ServerLink,
    hierarchy::{CloseTarget, Id, Inventory, Mutation, Outcome},
    repository::DirectoryFacts,
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
/// The handle's shared connection, once the server accepts it.
pub fn connect(server: &ServerConnection) -> Result<ServerLink, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        match server.link() {
            Ok(link) => return Ok(link),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub fn inventory(link: &ServerLink) -> Result<(Inventory, u64), Box<dyn Error>> {
    Ok(super::support::inventory(link)?)
}
pub fn mutate(link: &ServerLink, mutation: Mutation) -> Result<Id, Box<dyn Error>> {
    let (_, sequence) = inventory(link)?;
    let result = super::support::mutate(link, sequence, mutation)?;
    let Outcome::Accepted { target } = result.outcome else {
        return Err("mutation rejected".into());
    };
    link.acknowledge(sequence).map_err(ServerError::from)?;
    Ok(target)
}
pub fn close(link: &ServerLink, target: CloseTarget) -> Result<(), Box<dyn Error>> {
    let current = inventory(link)?.0;
    let confirmation = current.close_confirmation(target).ok_or("close target")?;
    mutate(
        link,
        Mutation::Close {
            target,
            confirmation,
        },
    )?;
    Ok(())
}
pub fn await_facts(
    link: &ServerLink,
    directory: &Path,
    predicate: impl Fn(&DirectoryFacts) -> bool,
) -> Result<Inventory, Box<dyn Error>> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let (current, _) = inventory(link)?;
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
