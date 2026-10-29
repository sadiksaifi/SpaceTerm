use super::*;
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

pub(super) fn hex(id: &Id) -> String {
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
            let handles: Vec<_> = tab
                .panes
                .iter()
                .map(|p| {
                    p.presentation
                        .as_deref()
                        .map(labels::pane_label)
                        .unwrap_or_else(|| labels::Label::from_parts("Terminal", ""))
                })
                .collect();
            let label = labels::tab_label(tab, &handles);
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

pub(super) fn resolve(
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

pub(super) fn target_steps(
    snapshot: &Snapshot,
    target: &Resolved,
) -> Vec<crate::contract::TargetStep> {
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
