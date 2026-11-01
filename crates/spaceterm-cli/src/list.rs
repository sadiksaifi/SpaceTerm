//! Inventory presentation. Labels and directories are operational data, never diagnostics.
use crate::{
    contract::*,
    scope::{Resolved, Snapshot},
};
use spaceterm_server::{
    hierarchy::{self, Id},
    labels, organize, repository,
};

/// Live session and control facts collected with the inventory, without taking Pane control.
pub struct PaneFacts {
    pub pane: Id,
    pub session: PaneSession,
    pub control: PaneControl,
}

/// Reads Session state through observation. It never claims, resizes or starts a shell.
pub fn collect(
    snapshot: &Snapshot,
    target: &Resolved,
    actor: Id,
    link: &spaceterm_server::connection::link::ServerLink,
    deadline: std::time::Instant,
) -> Result<Vec<PaneFacts>, Failure> {
    use spaceterm_server::protocol::{ObserveReply, ReplyBody, SessionState};
    use std::sync::Arc;
    let close = snapshot
        .inventory
        .close
        .as_ref()
        .ok_or_else(|| Failure::new(Code::ProtocolError))?;
    let mut facts = Vec::new();
    for workspace in &snapshot.inventory.workspaces {
        if target.workspace.is_some_and(|id| id != workspace.id) {
            continue;
        }
        for tab in &workspace.tabs {
            if target.tab.is_some_and(|id| id != tab.id)
                || target
                    .worktree
                    .as_ref()
                    .is_some_and(|p| tab.worktree.as_ref() != Some(p))
            {
                continue;
            }
            for pane in &tab.panes {
                if target.pane.is_some_and(|id| id != pane.id) {
                    continue;
                }
                let expected = close
                    .sessions
                    .iter()
                    .find(|p| p.id == pane.id)
                    .ok_or_else(|| Failure::new(Code::ProtocolError))?
                    .session;
                let channel = link
                    .open_channel(Arc::new(|_| {}))
                    .map_err(|e| from_server_error(e.into()))?;
                let request = channel
                    .observe(pane.id, expected)
                    .map_err(|e| from_server_error(e.into()))?;
                let reply = match request
                    .wait_for(deadline.saturating_duration_since(std::time::Instant::now()))
                {
                    Some(reply) => reply.map_err(|error| match error {
                        spaceterm_server::connection::link::RequestError::Refused(
                            spaceterm_server::pane::Refusal::Stale,
                        ) => Failure::new(Code::Changed),
                        error => from_server_error(error.into()),
                    })?,
                    None => {
                        request.cancel();
                        return Err(Failure::new(Code::Timeout));
                    }
                };
                let session = match reply {
                    ReplyBody::Observe(ObserveReply::Observing { session, .. })
                        if expected == Some(session) =>
                    {
                        PaneSession {
                            id: Some(hex(&session)),
                            state: SessionStateLabel::Running,
                        }
                    }
                    ReplyBody::Observe(ObserveReply::NoSession { state }) if expected.is_none() => {
                        PaneSession {
                            id: None,
                            state: match state {
                                SessionState::NotStarted => SessionStateLabel::NotStarted,
                                SessionState::Ended(_) => SessionStateLabel::Ended,
                            },
                        }
                    }
                    ReplyBody::Observe(_) => return Err(Failure::new(Code::Changed)),
                    _ => return Err(Failure::new(Code::ProtocolError)),
                };
                let control = match close.controls.iter().find(|c| c.pane == pane.id) {
                    Some(control) => PaneControl {
                        state: if control.owner == actor {
                            ControlState::Own
                        } else {
                            ControlState::Other
                        },
                        client_name: None,
                    },
                    None => PaneControl {
                        state: ControlState::None,
                        client_name: None,
                    },
                };
                facts.push(PaneFacts {
                    pane: pane.id,
                    session,
                    control,
                });
                drop(channel);
            }
        }
    }
    Ok(facts)
}

pub fn build(
    snapshot: &Snapshot,
    target: &Resolved,
    server: ListServer,
    live: &[PaneFacts],
) -> Result<List, Code> {
    let repos = snapshot
        .repositories
        .as_ref()
        .or(snapshot.inventory.repositories.as_ref());
    let mut workspaces = Vec::new();
    for workspace in &snapshot.inventory.workspaces {
        if target.workspace.is_some_and(|id| id != workspace.id) {
            continue;
        }
        let mut trees = Vec::new();
        if let Some(trees_snapshot) = repos.and_then(|repos| {
            organize::workspace_worktrees(&snapshot.inventory, repos, workspace.id)
        }) {
            for record in &trees_snapshot.worktrees {
                if target.worktree.as_ref().is_some_and(|p| p != &record.root) {
                    continue;
                }
                let tabs = workspace
                    .tabs
                    .iter()
                    .filter(|t| t.worktree.as_ref() == Some(&record.root))
                    .filter(|t| target.tab.is_none_or(|id| t.id == id))
                    .map(|t| tab(t, target, live))
                    .collect::<Result<Vec<_>, _>>()?;
                trees.push(ListWorktree {
                    worktree: WorktreeRef {
                        path: record.root.to_string_lossy().into_owned(),
                        branch: match &record.head {
                            repository::WorktreeHead::Branch(b) => Some(b.clone()),
                            _ => None,
                        },
                        main: record.root == trees_snapshot.main_root,
                        missing: record.missing,
                    },
                    tabs,
                });
            }
        }
        // Retained membership remains visible even when Git no longer lists its Worktree.
        for t in &workspace.tabs {
            if let Some(path) = &t.worktree
                && !trees
                    .iter()
                    .any(|t| t.worktree.path == path.to_string_lossy())
                && target.worktree.as_ref().is_none_or(|p| p == path)
            {
                trees.push(ListWorktree {
                    worktree: WorktreeRef {
                        path: path.to_string_lossy().into_owned(),
                        branch: None,
                        main: t.worktree_repository.as_ref() == Some(path),
                        missing: true,
                    },
                    tabs: workspace
                        .tabs
                        .iter()
                        .filter(|t| t.worktree.as_ref() == Some(path))
                        .filter(|t| target.tab.is_none_or(|id| t.id == id))
                        .map(|t| tab(t, target, live))
                        .collect::<Result<Vec<_>, _>>()?,
                });
            }
        }
        let tabs = workspace
            .tabs
            .iter()
            .filter(|t| t.worktree.is_none() && target.worktree.is_none())
            .filter(|t| target.tab.is_none_or(|id| t.id == id))
            .map(|t| tab(t, target, live))
            .collect::<Result<Vec<_>, _>>()?;
        let root = workspace
            .tabs
            .iter()
            .find(|t| t.id == workspace.root_tab)
            .and_then(|t| t.panes.iter().find(|p| p.id == t.root_pane));
        workspaces.push(ListWorkspace {
            reference: Ref {
                id: hex(&workspace.id),
                label: workspace.name.clone(),
                line: workspace.name.clone(),
            },
            directory: workspace
                .pinned_directory
                .as_deref()
                .or_else(|| root.map(|p| p.repository_directory()))
                .map(|p| p.to_string_lossy().into_owned()),
            worktrees: trees,
            tabs,
        });
    }
    Ok(List {
        revision: snapshot.inventory.revision,
        server,
        workspaces,
    })
}

fn tab(tab: &hierarchy::Tab, target: &Resolved, live: &[PaneFacts]) -> Result<ListTab, Code> {
    let label = labels::tab(tab);
    let panes = tab
        .panes
        .iter()
        .filter(|p| target.pane.is_none_or(|id| p.id == id))
        .map(|pane| {
            let label = labels::pane(pane);
            let facts = live
                .iter()
                .find(|f| f.pane == pane.id)
                .ok_or(Code::ProtocolError)?;
            Ok(ListPane {
                reference: Ref {
                    id: hex(&pane.id),
                    label: label.activity,
                    line: label.line,
                },
                directory: pane.presented_directory().to_string_lossy().into_owned(),
                session: facts.session.clone(),
                control: facts.control.clone(),
            })
        })
        .collect::<Result<Vec<_>, Code>>()?;
    Ok(ListTab {
        reference: Ref {
            id: hex(&tab.id),
            label: label.activity,
            line: label.line,
        },
        panes,
    })
}

fn hex(id: &Id) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// The text renderer consumes only successful operational data.
pub fn text(list: &List) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "{} ({} {})\n",
        list.server.name, list.server.os, list.server.version
    ));
    for workspace in &list.workspaces {
        output.push_str(&format!(
            "  {} --workspace id:{}\n",
            workspace.reference.line, workspace.reference.id
        ));
        if let Some(directory) = &workspace.directory {
            output.push_str(&format!("    {directory}\n"));
        }
        for tree in &workspace.worktrees {
            let name = tree
                .worktree
                .branch
                .as_deref()
                .unwrap_or(&tree.worktree.path);
            output.push_str(&format!(
                "    {}{} --worktree path:{}\n",
                name,
                if tree.worktree.missing {
                    " (missing)"
                } else {
                    ""
                },
                tree.worktree.path
            ));
            text_tabs(&tree.tabs, "      ", &mut output);
        }
        text_tabs(&workspace.tabs, "    ", &mut output);
    }
    output
}

fn text_tabs(tabs: &[ListTab], indent: &str, output: &mut String) {
    for tab in tabs {
        output.push_str(&format!(
            "{indent}{} --tab id:{}\n",
            tab.reference.line, tab.reference.id
        ));
        for pane in &tab.panes {
            let controller = match pane.control.state {
                ControlState::Own => "self",
                ControlState::Other => pane.control.client_name.as_deref().unwrap_or("other"),
                ControlState::None => "unclaimed",
            };
            output.push_str(&format!(
                "{indent}  {} [{}] --pane id:{}\n{indent}    {}\n",
                pane.reference.line, controller, pane.reference.id, pane.directory
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{
        self, Effect, ScopeArgs, Want,
        fixtures::{fixture, id},
    };

    fn facts(snapshot: &Snapshot) -> Vec<PaneFacts> {
        snapshot
            .inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| PaneFacts {
                pane: p.id,
                session: PaneSession {
                    id: Some(hex(&p.id)),
                    state: SessionStateLabel::Running,
                },
                control: PaneControl {
                    state: ControlState::Other,
                    client_name: Some("desktop".into()),
                },
            })
            .collect()
    }

    #[test]
    fn list_preserves_empty_and_missing_worktrees_and_live_control() {
        let mut snapshot = fixture();
        snapshot.inventory.workspaces[0].tabs[1].worktree = Some("/removed".into());
        let target = scope::resolve(
            &snapshot,
            &ScopeArgs::default(),
            None,
            Want::Server,
            Effect::Passive,
        )
        .unwrap();
        let list = build(
            &snapshot,
            &target,
            ListServer {
                name: "fixture".into(),
                os: "test".into(),
                version: "1".into(),
                sessions: 3,
            },
            &facts(&snapshot),
        )
        .unwrap();
        let workspace = &list.workspaces[0];
        assert!(
            workspace
                .worktrees
                .iter()
                .any(|w| w.worktree.path == "/trees/empty" && w.tabs.is_empty())
        );
        let missing = workspace
            .worktrees
            .iter()
            .find(|w| w.worktree.path == "/removed")
            .unwrap();
        assert!(missing.worktree.missing);
        assert_eq!(missing.tabs[0].panes[0].control.state, ControlState::Other);
        assert!(text(&list).contains("--pane id:"));
        assert!(text(&list).contains("/removed (missing)"));
        assert_eq!(workspace.reference.id, hex(&id(1)));
    }
}
