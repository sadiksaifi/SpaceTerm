//! Target resolution: from the selected server's one snapshot to `Resolved`, as section 4.3 of
//! the CLI spec describes. Explicit selectors are matched independently, then intersected; the
//! current Terminal Session fills untouched levels, single candidates fill the rest.
use crate::contract::{
    Actual, Candidate, CandidatesData, Code, Details, Failure, Level, MismatchData, Source,
    TargetDetails,
};
use spaceterm_server::hierarchy::{Id, Inventory};
use std::path::PathBuf;

/// A full ID or a unique prefix of at least 6 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdPrefix(String);

impl IdPrefix {
    pub const MIN: usize = 6;

    pub fn parse(text: &str) -> Option<Self> {
        let valid = (Self::MIN..=64).contains(&text.len())
            && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        valid.then(|| Self(text.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, id: &Id) -> bool {
        let mut hex = String::with_capacity(64);
        for byte in id {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex.starts_with(&self.0)
    }
}

/// One `--workspace`, `--worktree`, `--tab` or `--pane` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    /// The level containing the calling Pane.
    Current,
    /// `id:HEX`.
    Id(IdPrefix),
    /// `path:P`, Worktrees only.
    Path(PathBuf),
    /// A human handle.
    Text(String),
}

/// The selector was malformed. It carries no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSelector;

impl Selector {
    pub fn parse(text: &str) -> Result<Self, InvalidSelector> {
        if text == "current" {
            return Ok(Self::Current);
        }
        if let Some(id) = text.strip_prefix("id:") {
            return IdPrefix::parse(id).map(Self::Id).ok_or(InvalidSelector);
        }
        if let Some(path) = text.strip_prefix("path:") {
            if path.is_empty() {
                return Err(InvalidSelector);
            }
            return Ok(Self::Path(PathBuf::from(path)));
        }
        if text.is_empty() {
            return Err(InvalidSelector);
        }
        Ok(Self::Text(text.to_owned()))
    }
}

/// The explicit selectors of one command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeArgs {
    pub workspace: Option<Selector>,
    pub worktree: Option<Selector>,
    pub tab: Option<Selector>,
    pub pane: Option<Selector>,
}

/// One Workspace Inventory revision and the Repository snapshot. Every resolution step reads
/// only this.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub server: [u8; 32],
    pub inventory: Inventory,
    pub repositories: Option<spaceterm_server::repository::Snapshot>,
}

/// The calling Pane, when `SPACETERM_PANE`, `SPACETERM_SERVER_ID` and `SPACETERM_IDENTITY`
/// match this executable and the snapshot's server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Current {
    pub server: [u8; 32],
    pub pane: Id,
}

/// The level a command acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Server,
    Workspace,
    Worktree,
    Tab,
    Pane,
}

/// The command's effect, which decides the destructive restriction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Passive,
    Mutating,
    /// Needs confirmation. Its acted-on level accepts only `id:`, an exact handle or `current`,
    /// and never fills from a single candidate.
    Confirmed,
}

/// Where each resolved level came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    pub workspace: Option<Source>,
    pub worktree: Option<Source>,
    pub tab: Option<Source>,
    pub pane: Option<Source>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub server: [u8; 32],
    pub workspace: Option<Id>,
    /// Worktrees have path identity.
    pub worktree: Option<PathBuf>,
    pub tab: Option<Id>,
    pub pane: Option<Id>,
    pub sources: Sources,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetError {
    NotFound {
        level: Level,
    },
    Ambiguous {
        level: Level,
        candidates: Vec<Candidate>,
    },
    /// The resolved `level` is not under the explicit parent; `actual` is its real parent.
    Mismatch {
        level: Level,
        actual: Actual,
    },
    ContextUnavailable,
    TargetRequired {
        level: Level,
    },
}

/// The operational facts a target failure reports in `data`.
pub enum TargetData {
    None,
    Candidates(CandidatesData),
    Mismatch(MismatchData),
}

impl TargetError {
    /// The bounded failure, and the candidates or actual parent for `data`.
    pub fn into_parts(self) -> (Failure, TargetData) {
        match self {
            Self::NotFound { level } => (
                Failure::with(Code::NotFound, Details::Target(TargetDetails::level(level))),
                TargetData::None,
            ),
            Self::Ambiguous { level, candidates } => {
                let details = TargetDetails {
                    level,
                    parent: None,
                    count: Some(candidates.len().try_into().unwrap_or(u32::MAX)),
                    ids: candidates.iter().filter_map(|c| c.id.clone()).collect(),
                };
                (
                    Failure::with(Code::AmbiguousTarget, Details::Target(details)),
                    TargetData::Candidates(CandidatesData { candidates }),
                )
            }
            Self::Mismatch { level, actual } => (
                Failure::with(
                    Code::ScopeMismatch,
                    Details::Target(TargetDetails {
                        parent: Some(actual.level),
                        ..TargetDetails::level(level)
                    }),
                ),
                TargetData::Mismatch(MismatchData { actual }),
            ),
            Self::ContextUnavailable => (Failure::new(Code::ContextUnavailable), TargetData::None),
            Self::TargetRequired { level } => (
                Failure::with(
                    Code::TargetRequired,
                    Details::Target(TargetDetails::level(level)),
                ),
                TargetData::None,
            ),
        }
    }
}

use spaceterm_server::{labels, organize, repository};
use std::path::Path;

#[derive(Clone)]
struct Node {
    level: usize,
    workspace: Id,
    worktree: Option<PathBuf>,
    tab: Option<Id>,
    pane: Option<Id>,
    candidate: Candidate,
    handles: Vec<labels::Label>,
    branch: Option<String>,
    main: bool,
}

impl Node {
    fn compatible(&self, other: &Self) -> bool {
        self.workspace == other.workspace
            && (self.level < 1 || other.level < 1 || self.worktree == other.worktree)
            && (self.level < 2 || other.level < 2 || self.tab == other.tab)
            && (self.level < 3 || other.level < 3 || self.pane == other.pane)
    }

    fn id(&self) -> Option<Id> {
        match self.level {
            0 => Some(self.workspace),
            2 => self.tab,
            3 => self.pane,
            _ => None,
        }
    }
}

pub(crate) fn hex(id: &Id) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn level(index: usize) -> Level {
    [Level::Workspace, Level::Worktree, Level::Tab, Level::Pane][index]
}

fn candidate(
    level: Level,
    id: Option<Id>,
    path: Option<&Path>,
    label: String,
    directory: Option<&Path>,
    worktree: Option<&Path>,
    panes: usize,
) -> Candidate {
    Candidate {
        level,
        id: id.as_ref().map(hex),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        label,
        directory: directory.map(|p| p.to_string_lossy().into_owned()),
        worktree: worktree.map(|p| p.to_string_lossy().into_owned()),
        panes: panes.try_into().unwrap_or(u32::MAX),
    }
}

fn nodes(snapshot: &Snapshot) -> Vec<Node> {
    let mut nodes = Vec::new();
    let repos = snapshot
        .repositories
        .as_ref()
        .or(snapshot.inventory.repositories.as_ref());
    for w in &snapshot.inventory.workspaces {
        let root = w
            .tabs
            .iter()
            .find(|t| t.id == w.root_tab)
            .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane));
        let directory = w
            .pinned_directory
            .as_deref()
            .or_else(|| root.map(|p| p.repository_directory()));
        nodes.push(Node {
            level: 0,
            workspace: w.id,
            worktree: None,
            tab: None,
            pane: None,
            candidate: candidate(
                Level::Workspace,
                Some(w.id),
                None,
                w.name.clone(),
                directory,
                None,
                w.tabs.iter().map(|t| t.panes.len()).sum(),
            ),
            handles: Vec::new(),
            branch: None,
            main: false,
        });
        let trees =
            repos.and_then(|repos| organize::workspace_worktrees(&snapshot.inventory, repos, w.id));
        let mut paths = Vec::new();
        if let Some(trees) = trees {
            for tree in &trees.worktrees {
                paths.push((tree.root.clone(), Some(tree), tree.root == trees.main_root));
            }
        }
        for tab in &w.tabs {
            if let Some(path) = &tab.worktree
                && !paths.iter().any(|(p, _, _)| p == path)
            {
                paths.push((
                    path.clone(),
                    None,
                    tab.worktree_repository.as_ref() == Some(path),
                ));
            }
        }
        for (path, record, main) in paths {
            let branch = record.and_then(|r| match &r.head {
                repository::WorktreeHead::Branch(b) => Some(b.clone()),
                _ => None,
            });
            let label = branch.clone().unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            nodes.push(Node {
                level: 1,
                workspace: w.id,
                worktree: Some(path.clone()),
                tab: None,
                pane: None,
                candidate: candidate(
                    Level::Worktree,
                    None,
                    Some(&path),
                    label,
                    Some(&path),
                    Some(&path),
                    w.tabs
                        .iter()
                        .filter(|t| t.worktree.as_ref() == Some(&path))
                        .map(|t| t.panes.len())
                        .sum(),
                ),
                handles: Vec::new(),
                branch,
                main,
            });
        }
        for tab in &w.tabs {
            let handles: Vec<_> = tab.panes.iter().map(labels::pane).collect();
            let label = labels::tab(tab);
            let directory = tab
                .panes
                .iter()
                .find(|p| p.id == tab.root_pane)
                .map(|p| p.presented_directory());
            nodes.push(Node {
                level: 2,
                workspace: w.id,
                worktree: tab.worktree.clone(),
                tab: Some(tab.id),
                pane: None,
                candidate: candidate(
                    Level::Tab,
                    Some(tab.id),
                    None,
                    label.line,
                    directory,
                    tab.worktree.as_deref(),
                    tab.panes.len(),
                ),
                handles: handles.clone(),
                branch: None,
                main: false,
            });
            for (pane, label) in tab.panes.iter().zip(handles) {
                nodes.push(Node {
                    level: 3,
                    workspace: w.id,
                    worktree: tab.worktree.clone(),
                    tab: Some(tab.id),
                    pane: Some(pane.id),
                    candidate: candidate(
                        Level::Pane,
                        Some(pane.id),
                        None,
                        label.line.clone(),
                        Some(pane.presented_directory()),
                        tab.worktree.as_deref(),
                        1,
                    ),
                    handles: vec![label],
                    branch: None,
                    main: false,
                });
            }
        }
    }
    nodes
}

fn matching(
    nodes: &[Node],
    index: usize,
    selector: &Selector,
    current: Option<usize>,
    destructive: bool,
) -> Result<Vec<usize>, TargetError> {
    let all: Vec<_> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.level == index)
        .map(|(i, _)| i)
        .collect();
    let select = |predicate: &dyn Fn(&Node) -> bool| {
        all.iter()
            .copied()
            .filter(|i| predicate(&nodes[*i]))
            .collect::<Vec<_>>()
    };
    match selector {
        Selector::Current => {
            let current = current.ok_or(TargetError::ContextUnavailable)?;
            Ok(select(&|n| n.compatible(&nodes[current])))
        }
        Selector::Id(prefix) => Ok(select(&|n| n.id().is_some_and(|id| prefix.matches(&id)))),
        Selector::Path(path) => Ok(select(&|n| index == 1 && n.worktree.as_ref() == Some(path))),
        Selector::Text(text) => {
            let exact = select(&|n| match index {
                0 => n.candidate.label == *text,
                1 => n.branch.as_ref() == Some(text),
                _ => n
                    .handles
                    .iter()
                    .any(|l| l.activity == *text || l.line == *text),
            });
            if !exact.is_empty() {
                return Ok(exact);
            }
            if index == 1 {
                // A branch literally named main-worktree always wins over the reserved handle.
                if !destructive && text == "main-worktree" {
                    let main = select(&|n| n.main);
                    if !main.is_empty() {
                        return Ok(main);
                    }
                }
                if !destructive {
                    return Ok(select(&|n| {
                        n.worktree
                            .as_deref()
                            .and_then(Path::file_name)
                            .is_some_and(|p| p == text.as_str())
                    }));
                }
            } else if !destructive {
                let lower = text.to_lowercase();
                let folded = select(&|n| {
                    if index == 0 {
                        n.candidate.label.to_lowercase() == lower
                    } else {
                        n.handles
                            .iter()
                            .any(|l| l.activity.to_lowercase().contains(&lower))
                    }
                });
                if !folded.is_empty() {
                    return Ok(folded);
                }
                if let Some(prefix) = IdPrefix::parse(text) {
                    return Ok(select(&|n| n.id().is_some_and(|id| prefix.matches(&id))));
                }
            }
            Ok(Vec::new())
        }
    }
}

fn mismatch(node: &Node, selected: usize) -> TargetError {
    TargetError::Mismatch {
        level: level(selected),
        actual: Actual {
            level: node.candidate.level,
            id: node.candidate.id.clone(),
            path: node.candidate.path.clone(),
            label: node.candidate.label.clone(),
        },
    }
}

fn bind(
    nodes: &[Node],
    fixed: &mut [Option<usize>; 4],
    sources: &mut [Option<Source>; 4],
    selected: usize,
    source: Source,
    explicit: &[bool; 4],
) -> Result<(), TargetError> {
    let node = &nodes[selected];
    for index in (0..node.level).rev() {
        if explicit[index] && fixed[index].is_some_and(|i| !node.compatible(&nodes[i])) {
            let actual = nodes
                .iter()
                .find(|n| n.level == index && n.compatible(node));
            return Err(match actual {
                Some(actual) => mismatch(actual, node.level),
                None => TargetError::Mismatch {
                    level: level(node.level),
                    actual: Actual {
                        level: level(index),
                        id: None,
                        path: None,
                        label: String::new(),
                    },
                },
            });
        }
    }
    for index in 0..=node.level {
        if index == 1 && node.worktree.is_none() {
            if fixed[index].is_some() {
                return Err(TargetError::Mismatch {
                    level: level(node.level),
                    actual: Actual {
                        level: Level::Worktree,
                        id: None,
                        path: None,
                        label: String::new(),
                    },
                });
            }
            continue;
        }
        let ancestor = nodes
            .iter()
            .enumerate()
            .find(|(_, n)| n.level == index && n.compatible(node))
            .map(|(i, _)| i);
        if let Some(ancestor) = ancestor {
            if let Some(previous) = fixed[index]
                && previous != ancestor
            {
                return Err(mismatch(&nodes[ancestor], node.level));
            }
            if fixed[index].is_none() {
                fixed[index] = Some(ancestor);
                sources[index] = Some(source);
            }
        }
    }
    for other in fixed.iter().flatten() {
        if !node.compatible(&nodes[*other]) {
            return Err(mismatch(node, node.level));
        }
    }
    // The selected level carries its own source even when a child inferred it earlier.
    sources[node.level] = Some(source);
    Ok(())
}

pub fn resolve(
    snapshot: &Snapshot,
    args: &ScopeArgs,
    current: Option<&Current>,
    want: Want,
    effect: Effect,
) -> Result<Resolved, TargetError> {
    let nodes = nodes(snapshot);
    let selectors = [&args.workspace, &args.worktree, &args.tab, &args.pane];
    let explicit = selectors.map(|selector| selector.is_some());
    let current = current
        .filter(|c| c.server == snapshot.server)
        .and_then(|c| nodes.iter().position(|n| n.pane == Some(c.pane)));
    let acted = match want {
        Want::Server => None,
        Want::Workspace => Some(0),
        Want::Worktree => Some(1),
        Want::Tab => Some(2),
        Want::Pane => Some(3),
    };
    let mut fixed = [None; 4];
    let mut sources = [None; 4];
    let mut pending: [Option<Vec<usize>>; 4] = Default::default();
    for (index, selector) in selectors.iter().enumerate() {
        if let Some(selector) = selector {
            let matches = matching(
                &nodes,
                index,
                selector,
                current,
                effect == Effect::Confirmed && acted == Some(index),
            )?;
            if matches.is_empty() {
                return Err(TargetError::NotFound {
                    level: level(index),
                });
            }
            if matches.len() == 1 {
                bind(
                    &nodes,
                    &mut fixed,
                    &mut sources,
                    matches[0],
                    Source::Flag,
                    &explicit,
                )?;
            } else {
                pending[index] = Some(matches);
            }
        }
    }
    // Keep each first nonempty handle match set. Narrowing never restarts handle search.
    loop {
        let mut progress = false;
        for candidates in &mut pending {
            if let Some(candidates) = candidates {
                candidates.retain(|candidate| {
                    fixed
                        .iter()
                        .flatten()
                        .all(|other| nodes[*candidate].compatible(&nodes[*other]))
                });
                if candidates.len() == 1 {
                    bind(
                        &nodes,
                        &mut fixed,
                        &mut sources,
                        candidates[0],
                        Source::Flag,
                        &explicit,
                    )?;
                    progress = true;
                }
            }
            if candidates.as_ref().is_some_and(|c| c.len() == 1) {
                *candidates = None;
            }
        }
        if !progress {
            break;
        }
    }
    for (index, candidates) in pending.into_iter().enumerate() {
        if let Some(candidates) = candidates {
            return Err(TargetError::Ambiguous {
                level: level(index),
                candidates: candidates
                    .into_iter()
                    .map(|i| nodes[i].candidate.clone())
                    .collect(),
            });
        }
    }
    if acted.is_some()
        && let Some(current) = current
    {
        for index in 0..4 {
            if fixed[index].is_none() && selectors[index..].iter().all(|s| s.is_none()) {
                let context = nodes
                    .iter()
                    .enumerate()
                    .find(|(_, n)| n.level == index && n.compatible(&nodes[current]))
                    .map(|(i, _)| i);
                if let Some(context) = context
                    && fixed
                        .iter()
                        .flatten()
                        .all(|i| nodes[context].compatible(&nodes[*i]))
                {
                    bind(
                        &nodes,
                        &mut fixed,
                        &mut sources,
                        context,
                        Source::Current,
                        &explicit,
                    )?;
                }
            }
        }
    }
    if effect == Effect::Confirmed
        && let Some(index) = acted
        && fixed[index].is_none()
    {
        return Err(TargetError::TargetRequired {
            level: level(index),
        });
    }
    let depth = selectors
        .iter()
        .rposition(|s| s.is_some())
        .into_iter()
        .chain(acted)
        .max();
    if let Some(depth) = depth {
        for index in 0..=depth {
            if fixed[index].is_some() {
                continue;
            }
            let candidates: Vec<_> = nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    n.level == index && fixed.iter().flatten().all(|i| n.compatible(&nodes[*i]))
                })
                .map(|(i, _)| i)
                .collect();
            // Non-repository Tabs and Workspaces have no Worktree level.
            if index == 1 && candidates.is_empty() && acted != Some(1) {
                continue;
            }
            if candidates.len() == 1 {
                bind(
                    &nodes,
                    &mut fixed,
                    &mut sources,
                    candidates[0],
                    Source::Only,
                    &explicit,
                )?;
            } else {
                return Err(TargetError::Ambiguous {
                    level: level(index),
                    candidates: candidates
                        .into_iter()
                        .map(|i| nodes[i].candidate.clone())
                        .collect(),
                });
            }
        }
    }
    Ok(Resolved {
        server: snapshot.server,
        workspace: fixed[0].map(|i| nodes[i].workspace),
        worktree: fixed[1].and_then(|i| nodes[i].worktree.clone()),
        tab: fixed[2].and_then(|i| nodes[i].tab),
        pane: fixed[3].and_then(|i| nodes[i].pane),
        sources: Sources {
            workspace: sources[0],
            worktree: sources[1],
            tab: sources[2],
            pane: sources[3],
        },
    })
}

pub fn target_steps(snapshot: &Snapshot, target: &Resolved) -> Vec<crate::contract::TargetStep> {
    let all = nodes(snapshot);
    let sources = [
        target.sources.workspace,
        target.sources.worktree,
        target.sources.tab,
        target.sources.pane,
    ];
    let mut steps = Vec::new();
    for (index, source) in sources.into_iter().enumerate() {
        let Some(source) = source else { continue };
        let node = all.iter().find(|n| {
            n.level == index
                && match index {
                    0 => Some(n.workspace) == target.workspace,
                    1 => Some(n.workspace) == target.workspace && n.worktree == target.worktree,
                    2 => n.tab == target.tab,
                    _ => n.pane == target.pane,
                }
        });
        if let Some(node) = node {
            steps.push(crate::contract::TargetStep {
                level: level(index),
                id: node.candidate.id.clone(),
                path: node.candidate.path.clone(),
                label: Some(node.candidate.label.clone()),
                source,
            });
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_parse_their_four_forms() {
        assert_eq!(Selector::parse("current"), Ok(Selector::Current));
        assert_eq!(
            Selector::parse("id:0BE4F3"),
            Ok(Selector::Id(IdPrefix("0be4f3".into())))
        );
        assert_eq!(Selector::parse("id:0be4f"), Err(InvalidSelector));
        assert_eq!(
            Selector::parse("path:/srv/api"),
            Ok(Selector::Path("/srv/api".into()))
        );
        assert_eq!(
            Selector::parse("feature/x"),
            Ok(Selector::Text("feature/x".into()))
        );
        assert!(!IdPrefix::parse("0be4f3").unwrap().matches(&[0x0b; 32]));
        assert!(IdPrefix::parse("0b0b0b").unwrap().matches(&[0x0b; 32]));
    }
}

mod current;
#[cfg(test)]
pub(crate) mod fixtures;
mod output;
#[cfg(test)]
#[path = "scope/tests.rs"]
mod resolution_tests;

pub fn current_from_context(
    snapshot: &Snapshot,
    executable_identity: &str,
    identity: Option<&str>,
    server: Option<&str>,
    pane: Option<&str>,
) -> Option<Current> {
    current::from_context(snapshot, executable_identity, identity, server, pane)
}
pub fn verbose(steps: &[crate::contract::TargetStep]) -> String {
    output::verbose(steps)
}
pub fn data_text(data: &TargetData) -> String {
    output::data_text(data)
}
