//! The inventory as work protocol 1.0 carries it, in [`Kind::Inventory`](super::Kind::Inventory)
//! frames.
//!
//! Work 1.1 added [`DirectoryFacts::inside_git_directory`](crate::repository::DirectoryFacts)
//! and carries the inventory in [`Kind::GitDirectoryInventory`](super::Kind::GitDirectoryInventory)
//! frames instead. A minor may not add a field to an existing type (ADR 0035), so these types
//! keep the 1.0 field order exactly, and a Core sends them to a connection that selected 1.0.
use crate::hierarchy::{CloseInventory, Id, Workspace};
use crate::repository::{
    GitToolStatus, PinCheckState, Repository, RepositoryReadError, WorktreeInventory,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The work minor of major 1 that first carries
/// [`DirectoryFacts::inside_git_directory`](crate::repository::DirectoryFacts), in
/// [`Kind::GitDirectoryInventory`](super::Kind::GitDirectoryInventory) frames.
pub const MINOR_GIT_DIRECTORY_INVENTORY: u16 = 1;

#[derive(Serialize, Deserialize)]
pub(super) struct InventoryRevision {
    inventory: Inventory,
    next_sequence: u64,
}

#[derive(Serialize, Deserialize)]
struct Inventory {
    repositories: Option<Snapshot>,
    revision: u64,
    workspaces: Vec<Workspace>,
    close: Option<CloseInventory>,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    boot: Id,
    revision: u64,
    git: GitToolStatus,
    directories: Vec<DirectoryFacts>,
    pinned_worktrees: Vec<PathBuf>,
    pin_check: PinCheckState,
}

#[derive(Serialize, Deserialize)]
struct DirectoryFacts {
    directory: PathBuf,
    repository: Option<Repository>,
    worktrees: Option<WorktreeInventory>,
    failure: Option<RepositoryReadError>,
}

impl From<super::InventoryRevision> for InventoryRevision {
    fn from(revision: super::InventoryRevision) -> Self {
        let super::InventoryRevision {
            inventory,
            next_sequence,
        } = revision;
        let crate::hierarchy::Inventory {
            repositories,
            revision,
            workspaces,
            close,
        } = inventory;
        Self {
            inventory: Inventory {
                repositories: repositories.map(Snapshot::from),
                revision,
                workspaces,
                close,
            },
            next_sequence,
        }
    }
}

/// A 1.0 Core never read `inside_git_directory`, so its directories read as outside a Git
/// directory.
impl From<InventoryRevision> for super::InventoryRevision {
    fn from(revision: InventoryRevision) -> Self {
        let InventoryRevision {
            inventory,
            next_sequence,
        } = revision;
        let Inventory {
            repositories,
            revision,
            workspaces,
            close,
        } = inventory;
        Self {
            inventory: crate::hierarchy::Inventory {
                repositories: repositories.map(crate::repository::Snapshot::from),
                revision,
                workspaces,
                close,
            },
            next_sequence,
        }
    }
}

impl From<crate::repository::Snapshot> for Snapshot {
    fn from(snapshot: crate::repository::Snapshot) -> Self {
        let crate::repository::Snapshot {
            boot,
            revision,
            git,
            directories,
            pinned_worktrees,
            pin_check,
        } = snapshot;
        Self {
            boot,
            revision,
            git,
            directories: directories
                .into_iter()
                .map(|facts| {
                    let crate::repository::DirectoryFacts {
                        directory,
                        repository,
                        worktrees,
                        failure,
                        inside_git_directory: _,
                    } = facts;
                    DirectoryFacts {
                        directory,
                        repository,
                        worktrees,
                        failure,
                    }
                })
                .collect(),
            pinned_worktrees,
            pin_check,
        }
    }
}

impl From<Snapshot> for crate::repository::Snapshot {
    fn from(snapshot: Snapshot) -> Self {
        let Snapshot {
            boot,
            revision,
            git,
            directories,
            pinned_worktrees,
            pin_check,
        } = snapshot;
        Self {
            boot,
            revision,
            git,
            directories: directories
                .into_iter()
                .map(|facts| {
                    let DirectoryFacts {
                        directory,
                        repository,
                        worktrees,
                        failure,
                    } = facts;
                    crate::repository::DirectoryFacts {
                        directory,
                        repository,
                        worktrees,
                        failure,
                        inside_git_directory: false,
                    }
                })
                .collect(),
            pinned_worktrees,
            pin_check,
        }
    }
}
