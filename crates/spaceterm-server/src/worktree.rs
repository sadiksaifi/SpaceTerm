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
    Context {
        root: PathBuf,
    },
    Settings,
    SeedLocation {
        template: String,
    },
    Branches {
        root: PathBuf,
    },
    CheckRemoval {
        root: PathBuf,
        path: PathBuf,
    },
    Location {
        root: PathBuf,
        branch: String,
        location: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ReadReply {
    Context {
        settings: Settings,
        home: PathBuf,
        branches: Branches,
    },
    Settings(Settings),
    Branches(Branches),
    Location(PathBuf),
    Removal(Removal),
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
        self.workspaces
            .iter()
            .flat_map(|w| {
                w.tabs
                    .iter()
                    .filter(|t| {
                        (t.worktree.as_deref() == Some(path)
                            && t.worktree_repository.as_deref().is_none_or(|r| r == root))
                            || t.panes.iter().any(|p| {
                                p.confirmed_directory
                                    .as_deref()
                                    .unwrap_or(&p.starting_directory)
                                    .starts_with(path)
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
