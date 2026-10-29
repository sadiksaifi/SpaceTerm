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
    /// Git could not finish creating the Worktree.
    Branch,
    /// Git refuses the new branch's name.
    InvalidName,
    /// A branch with the new branch's name already exists.
    BranchExists,
    /// Another Worktree has the branch checked out.
    BranchCheckedOut,
    /// The branch to check out or start from no longer exists.
    BranchMissing,
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

/// Presentation facts from the connected owning server. They grant no host maintenance authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFacts {
    #[serde(deserialize_with = "deserialize_server_fact")]
    pub machine: String,
    #[serde(deserialize_with = "deserialize_server_fact")]
    pub operating_system: String,
    #[serde(deserialize_with = "deserialize_server_fact")]
    pub version: String,
}

fn deserialize_server_fact<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    ServerFacts::bounded(&value).map_err(|_| serde::de::Error::custom("server fact is unavailable"))
}

impl ServerFacts {
    /// Removes control and invisible formatting characters and bounds each field to 256 bytes.
    /// A missing display value is a typed failure, never a guessed client-side fact.
    pub fn new(machine: &str, operating_system: &str, version: &str) -> Result<Self, Error> {
        Ok(Self {
            machine: Self::bounded(machine)?,
            operating_system: Self::bounded(operating_system)?,
            version: Self::bounded(version)?,
        })
    }

    fn bounded(value: &str) -> Result<String, Error> {
        let mut value = crate::pane::metadata::sanitize(value, 256);
        let mut end = value.len().min(256);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(Error::Unavailable);
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Read {
    Context {
        root: PathBuf,
    },
    Settings,
    /// The owning server's Git installation, even when it owns no Workspaces.
    Git,
    /// Live facts from the owning server, independent of Git and Workspaces.
    ServerFacts,
    /// Checks a draft without changing Server Settings or creating folders.
    ValidateLocationTemplate {
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
    Context {
        settings: Settings,
        home: PathBuf,
        branches: Branches,
    },
    Settings(Settings),
    Git(crate::repository::GitToolStatus),
    ServerFacts(ServerFacts),
    LocationTemplate,
    Location(PathBuf),
    Removal(Removal),
}

/// One bounded read on `link`. Its reply arrives when the read settles; one that outlasts the
/// limit is cancelled. Uncertain requests are never retried.
#[cfg(feature = "client")]
pub fn read_worktree(
    link: &crate::connection::link::ServerLink,
    read: Read,
) -> Result<Result<ReadReply, Error>, crate::ServerError> {
    let pending = link.request(crate::protocol::Operation::WorktreeRead(read))?;
    match pending.wait_for(READ_LIMIT) {
        Some(Ok(crate::protocol::ReplyBody::Worktree(result))) => Ok(result),
        Some(Ok(_)) => Err(crate::ServerError::Protocol),
        Some(Err(error)) => Err(error.into()),
        None => {
            pending.cancel();
            Ok(Err(Error::Unavailable))
        }
    }
}
/// How long a Worktree read may run before the client abandons it.
#[cfg(feature = "client")]
const READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_facts_sanitize_and_bound_display_values_without_splitting_utf8() {
        let facts = ServerFacts::new(
            &format!(" \u{202e}{}\u{1b}", "界".repeat(100)),
            "\u{2066}Linux\u{2069}",
            "\t0.1.0\n",
        )
        .unwrap();
        assert_eq!(facts.machine, "界".repeat(85));
        assert_eq!(facts.operating_system, "Linux");
        assert_eq!(facts.version, "0.1.0");
    }

    #[test]
    fn server_facts_refuse_empty_metadata_instead_of_inventing_values() {
        assert_eq!(
            ServerFacts::new("\u{202e}\u{1b} ", "Linux", "0.1.0"),
            Err(Error::Unavailable)
        );
        assert_eq!(
            ServerFacts::new("server", "Linux", " \n"),
            Err(Error::Unavailable)
        );
    }

    #[test]
    fn server_facts_apply_display_bounds_to_values_received_over_the_protocol() {
        let facts: ServerFacts = serde_json::from_value(serde_json::json!({
            "machine": format!("\u{202e}{}", "界".repeat(100)),
            "operating_system": "\u{2066}Linux\u{2069}",
            "version": "\t0.1.0\n",
        }))
        .unwrap();
        assert_eq!(facts.machine, "界".repeat(85));
        assert_eq!(facts.operating_system, "Linux");
        assert_eq!(facts.version, "0.1.0");
    }
}
