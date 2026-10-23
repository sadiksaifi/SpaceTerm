//! Explicit Worktree operations. Server paths are descriptions, never client file authority.
use crate::hierarchy::{ConfirmedPane, Id};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod path_template;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BranchName {
    Local(String),
    Remote(String),
}
impl BranchName {
    pub fn full_name(&self) -> String {
        match self {
            Self::Local(name) => format!("refs/heads/{name}"),
            Self::Remote(name) => format!("refs/remotes/{name}"),
        }
    }
}
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum Branch {
    New { name: String, base: BranchName },
    Existing { name: String },
    Remote { remote: String, name: String },
}
impl std::fmt::Debug for Branch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Branch(..)")
    }
}
impl Branch {
    pub fn name(&self) -> &str {
        match self {
            Self::New { name, .. } | Self::Existing { name } | Self::Remote { name, .. } => name,
        }
    }
    pub fn is_bounded(&self) -> bool {
        let valid = |s: &str| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control);
        match self {
            Self::New { name, base } => {
                valid(name)
                    && match base {
                        BranchName::Local(s) | BranchName::Remote(s) => valid(s),
                    }
            }
            Self::Existing { name } => valid(name),
            Self::Remote { name, remote } => valid(name) && valid(remote),
        }
    }
}
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Branches {
    pub local: Vec<String>,
    pub remote: Vec<String>,
}
impl std::fmt::Debug for Branches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Branches")
            .field("local", &self.local.len())
            .field("remote", &self.remote.len())
            .finish()
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    Create {
        workspace: Id,
        root: PathBuf,
        path: PathBuf,
        branch: Branch,
    },
    Remove {
        root: PathBuf,
        path: PathBuf,
        boot: Id,
        confirmation: Id,
    },
    SetLocation {
        version: u64,
        template: String,
    },
    OpenTab {
        workspace: Id,
        source: Option<Id>,
        root: PathBuf,
        path: PathBuf,
    },
}
impl Operation {
    pub fn is_bounded(&self) -> bool {
        let path = |p: &std::path::Path| {
            p.is_absolute()
                && p.as_os_str().len() <= 4096
                && !p.as_os_str().as_encoded_bytes().contains(&0)
        };
        match self {
            Self::SetLocation { template, .. } => template.len() <= 1024,
            Self::Create {
                root,
                path: p,
                branch,
                ..
            } => path(root) && path(p) && branch.is_bounded(),
            Self::Remove { root, path: p, .. } | Self::OpenTab { root, path: p, .. } => {
                path(root) && path(p)
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Error {
    Missing,
    Main,
    Locked,
    Pinned,
    Changed,
    Location,
    Branch,
    Unavailable,
    Interrupted,
    Template(path_template::WorktreePathTemplateError),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub template: String,
    pub version: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            template: path_template::DEFAULT_WORKTREE_PATH_TEMPLATE.into(),
            version: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Read {
    Poll {
        id: u64,
    },
    Context {
        root: PathBuf,
    },
    Settings,
    /// The owning server's Git installation, even when it owns no Workspaces.
    Git,
    /// Checks a draft without changing Server Settings or creating folders.
    ValidateLocationTemplate {
        template: String,
    },
    SeedLocation {
        template: String,
    },
    CheckRemoval {
        root: PathBuf,
        path: PathBuf,
    },
    Location {
        root: PathBuf,
        location: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReadReply {
    Pending {
        id: u64,
    },
    Context {
        settings: Settings,
        home: PathBuf,
        branches: Branches,
    },
    Settings(Settings),
    Git(crate::repository::GitToolStatus),
    LocationTemplate,
    Location(PathBuf),
    Removal(Removal),
}

#[cfg(feature = "client")]
impl crate::pane::PaneConnection {
    /// Poll one bounded read on this connection. Uncertain exchanges are never retried.
    pub fn read_worktree(
        &mut self,
        mut read: Read,
    ) -> Result<Result<ReadReply, Error>, crate::ServerError> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match self.exchange(&crate::pane::Request::WorktreeRead(read))? {
                crate::pane::Reply::WorktreeRead(Ok(ReadReply::Pending { id })) => {
                    if std::time::Instant::now() >= until {
                        return Ok(Err(Error::Unavailable));
                    }
                    read = Read::Poll { id };
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                crate::pane::Reply::WorktreeRead(result) => return Ok(result),
                _ => return Err(crate::ServerError::Protocol),
            }
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AffectedTab {
    pub workspace: Id,
    pub tab: Id,
    pub panes: Vec<ConfirmedPane>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Removal {
    pub boot: Id,
    pub confirmation: Id,
    pub root: PathBuf,
    pub path: PathBuf,
    pub changes: bool,
    pub submodules: bool,
    pub tabs: Vec<AffectedTab>,
}

impl crate::hierarchy::Inventory {
    pub fn affected_worktree_tabs(
        &self,
        root: &std::path::Path,
        path: &std::path::Path,
    ) -> Vec<AffectedTab> {
        self.affected_worktree_tabs_in(root, path, |directory| directory.starts_with(path))
    }

    /// The owning server supplies physical containment; clients can only compare known paths.
    pub fn affected_worktree_tabs_in(
        &self,
        root: &std::path::Path,
        path: &std::path::Path,
        contains: impl Fn(&std::path::Path) -> bool,
    ) -> Vec<AffectedTab> {
        self.workspaces
            .iter()
            .flat_map(|w| {
                w.tabs
                    .iter()
                    .filter(|t| {
                        (t.worktree.as_deref() == Some(path)
                            && t.worktree_repository.as_deref().is_none_or(|r| r == root))
                            || t.panes.iter().any(|p| {
                                contains(
                                    p.confirmed_directory
                                        .as_deref()
                                        .unwrap_or(&p.starting_directory),
                                )
                            })
                    })
                    .map(|t| AffectedTab {
                        workspace: w.id,
                        tab: t.id,
                        panes: t
                            .panes
                            .iter()
                            .filter_map(|p| {
                                self.close
                                    .as_ref()
                                    .and_then(|live| live.sessions.iter().find(|s| s.id == p.id))
                                    .cloned()
                            })
                            .collect(),
                    })
            })
            .collect()
    }
}

impl Removal {
    /// Clients compare the exact reviewed Tabs and Sessions without local filesystem authority.
    pub fn matches_inventory(&self, inventory: &crate::hierarchy::Inventory) -> bool {
        let live = inventory.close.as_ref();
        if live.is_none_or(|c| c.boot != self.boot) {
            return false;
        }
        if self.tabs.iter().any(|reviewed| {
            let tab = inventory
                .workspaces
                .iter()
                .find(|w| w.id == reviewed.workspace)
                .and_then(|w| w.tabs.iter().find(|t| t.id == reviewed.tab));
            tab.is_none_or(|tab| {
                let panes: Vec<_> = tab
                    .panes
                    .iter()
                    .filter_map(|p| {
                        live.and_then(|c| c.sessions.iter().find(|s| s.id == p.id))
                            .cloned()
                    })
                    .collect();
                panes != reviewed.panes
            })
        }) {
            return false;
        }
        inventory
            .affected_worktree_tabs(&self.root, &self.path)
            .iter()
            .all(|t| self.tabs.contains(t))
    }
}
