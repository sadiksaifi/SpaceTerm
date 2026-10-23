//! The client's bounded connected intent, separate from the transport's durable Original Operation.
use super::development::{SharedAcceptance, SharedCloseIntent, SharedRefusalReason};
use crate::terminal::shared_workspace::SharedWorkspaceSnapshot;
use spaceterm_server::hierarchy::{
    Axis, CloseTarget, Id, Inventory, Mutation, Outcome, Pane, RetainedOutcome, Tab, Workspace,
};
use spaceterm_server::worktree::Operation;
use std::collections::{BTreeMap, VecDeque};

const CAPACITY: usize = 32;
type Reply = async_channel::Sender<Result<RetainedOutcome, SharedRefusalReason>>;

struct Edit {
    mutation: Mutation,
    creation: Option<Creation>,
    dispatched: bool,
    projected: bool,
    reply: Option<Reply>,
}

#[derive(Clone, Copy)]
struct Creation {
    workspace: Option<Id>,
    tab: Option<Id>,
    pane: Id,
}

pub(super) struct Completion {
    pub outcome: RetainedOutcome,
    pub inventory: Option<Inventory>,
    pub optimistic: bool,
}

#[derive(Default)]
pub(super) struct PendingEdits {
    edits: VecDeque<Edit>,
    completed: VecDeque<Completion>,
    close_intents: VecDeque<SharedCloseIntent>,
    /// Delivered outcomes keep admission capacity while their rejected drafts await review.
    unread_conflicts: usize,
    presentations: VecDeque<SharedAcceptance>,
    aliases: BTreeMap<Id, Id>,
    observed: Option<u64>,
    pub disconnected: bool,
}

impl PendingEdits {
    pub fn full(&self) -> bool {
        self.edits.len() + self.completed.len() + self.close_intents.len() + self.unread_conflicts
            >= CAPACITY
    }
    pub fn has_pending(&self) -> bool {
        !self.edits.is_empty()
    }

    pub fn retain_conflict(&mut self) {
        self.unread_conflicts += 1;
    }

    pub fn release_conflicts(&mut self, count: usize) {
        self.unread_conflicts -= count;
    }

    pub fn push(
        &mut self,
        mutation: Mutation,
        reply: Option<Reply>,
    ) -> Result<(), SharedRefusalReason> {
        if self.full() {
            return Err(SharedRefusalReason::Pending);
        }
        let new_id = || -> Result<Id, SharedRefusalReason> {
            let mut id = [0; 32];
            getrandom::fill(&mut id).map_err(|_| SharedRefusalReason::Value)?;
            Ok(id)
        };
        let creation = match &mutation {
            Mutation::CreateWorkspace { .. } => Some(Creation {
                workspace: Some(new_id()?),
                tab: Some(new_id()?),
                pane: new_id()?,
            }),
            Mutation::CreateTab { .. } | Mutation::Worktree(Operation::OpenTab { .. }) => {
                Some(Creation {
                    workspace: None,
                    tab: Some(new_id()?),
                    pane: new_id()?,
                })
            }
            Mutation::SplitPane { .. } => Some(Creation {
                workspace: None,
                tab: None,
                pane: new_id()?,
            }),
            _ => None,
        };
        if let Some(ids) = creation {
            self.presentations.push_back(SharedAcceptance {
                mutation: mutation.clone(),
                target: ids.workspace.or(ids.tab).unwrap_or(ids.pane),
            });
        }
        self.edits.push_back(Edit {
            mutation,
            creation,
            dispatched: false,
            projected: false,
            reply,
        });
        Ok(())
    }

    pub fn canonical(&self, mut id: Id) -> Id {
        // Dependencies are rewritten at settlement. The bound also prevents malformed alias cycles.
        for _ in 0..CAPACITY * 3 {
            let Some(next) = self.aliases.get(&id) else {
                break;
            };
            id = *next;
        }
        id
    }

    pub fn provisional(&self, id: Id) -> bool {
        self.edits
            .iter()
            .filter_map(|edit| edit.creation)
            .any(|ids| ids.workspace == Some(id) || ids.tab == Some(id) || ids.pane == id)
    }

    /// Retains an unapproved close only while its exact target depends on creation.
    pub fn defer_close(&mut self, intent: SharedCloseIntent) -> Result<bool, SharedRefusalReason> {
        if self
            .close_intents
            .iter()
            .any(|pending| pending.server == intent.server)
        {
            return Ok(true);
        }
        if !close_references(intent.server)
            .iter()
            .any(|id| self.provisional(*id))
        {
            return Ok(false);
        }
        if self.full() {
            return Err(SharedRefusalReason::Pending);
        }
        self.close_intents.push_back(intent);
        Ok(true)
    }

    pub fn take_close_intent(&mut self) -> Option<SharedCloseIntent> {
        let intent = self.close_intents.front()?;
        if close_references(intent.server)
            .iter()
            .any(|id| self.provisional(*id))
        {
            return None;
        }
        self.close_intents.pop_front()
    }

    pub fn closing_workspace(&self, workspace: Id, inventory: Option<&Inventory>) -> bool {
        let Some(inventory) =
            inventory.filter(|inventory| inventory.workspaces.iter().any(|w| w.id == workspace))
        else {
            return self
                .edits
                .iter()
                .any(|edit| closes_workspace(&edit.mutation, workspace));
        };
        // Separate approvals can hide every Tab without any one approval authorizing the
        // Workspace's removal. Parking follows that combined presentation, never broader consent.
        let mut projected = inventory.clone();
        for edit in &self.edits {
            project(&mut projected, &edit.mutation, edit.creation);
        }
        !projected.workspaces.iter().any(|w| w.id == workspace)
    }

    pub fn closing(&self, pane: Id) -> bool {
        self.edits.iter().any(|edit| matches!(&edit.mutation, Mutation::Close { confirmation, .. } if confirmation.panes.iter().any(|p| p.id == pane)))
    }

    pub fn next_dispatch(&self) -> Option<Mutation> {
        let edit = self.edits.front().filter(|edit| !edit.dispatched)?;
        // A provisional dependency never crosses the transport boundary.
        let mut mutation = edit.mutation.clone();
        remap(&mut mutation, |id| self.canonical(id));
        (!references(&mutation).iter().any(|id| self.provisional(*id))).then_some(mutation)
    }

    pub fn dispatched(&mut self, mutation: Mutation) {
        if let Some(edit) = self.edits.front_mut() {
            edit.dispatched = true;
            edit.mutation = mutation;
        }
    }

    pub fn observe(&mut self, snapshot: &SharedWorkspaceSnapshot) {
        if let Some(outcome) = snapshot
            .outcome
            .as_ref()
            .filter(|outcome| self.observed != Some(outcome.sequence))
        {
            // The worker may publish a validated committed outcome before its acknowledgement
            // finishes. Membership can settle now; dispatch still waits for transport availability.
            let ours = self
                .edits
                .front()
                .is_some_and(|edit| edit.dispatched && edit.mutation == outcome.mutation);
            if ours && !matches!(outcome.outcome, Outcome::Pending) {
                self.observed = Some(outcome.sequence);
                let mut edit = self.edits.pop_front().expect("matched the front operation");
                let awaited = edit.reply.is_some();
                if let Some(reply) = edit.reply.take() {
                    let _ = reply.try_send(Ok(outcome.clone()));
                }
                if let (Some(ids), Outcome::Accepted { target }, Some(inventory)) =
                    (edit.creation, &outcome.outcome, &snapshot.inventory)
                {
                    self.resolve(ids, *target, inventory);
                    if !edit.projected {
                        self.aliases.remove(&ids.pane);
                        if let Some(id) = ids.tab {
                            self.aliases.remove(&id);
                        }
                        if let Some(id) = ids.workspace {
                            self.aliases.remove(&id);
                        }
                    }
                } else if let Some(ids) = edit.creation {
                    self.reject_dependencies(ids);
                }
                // An awaited Settings or Worktree write has its own consumer and needs no
                // retained presentation completion when no Workspace Window exists.
                if !awaited || edit.creation.is_some() {
                    self.completed.push_back(Completion {
                        outcome: outcome.clone(),
                        inventory: snapshot.inventory.clone(),
                        optimistic: edit.creation.is_some(),
                    });
                }
            }
        }
        if let Some(rejected) = &snapshot.rejected
            && self
                .edits
                .front()
                .is_some_and(|edit| edit.dispatched && &edit.mutation == rejected)
        {
            let edit = self.edits.pop_front().expect("matched rejected operation");
            if let Some(reply) = edit.reply {
                let _ = reply.try_send(Err(SharedRefusalReason::Offline));
            }
            if let Some(ids) = edit.creation {
                self.reject_dependencies(ids);
            }
            self.disconnected = true;
        }
        if !snapshot.connected {
            self.disconnect();
        }
    }

    fn resolve(&mut self, ids: Creation, target: Id, inventory: &Inventory) {
        if let Some(workspace) = ids.workspace {
            if let Some(authority) = inventory.workspaces.iter().find(|w| w.id == target) {
                self.aliases.insert(workspace, target);
                if let Some(tab) = authority.tabs.iter().find(|t| t.id == authority.root_tab) {
                    if let Some(id) = ids.tab {
                        self.aliases.insert(id, tab.id);
                    }
                    self.aliases.insert(ids.pane, tab.root_pane);
                }
            }
        } else if let Some(tab) = ids.tab {
            if let Some(authority) = inventory
                .workspaces
                .iter()
                .flat_map(|w| &w.tabs)
                .find(|t| t.id == target)
            {
                self.aliases.insert(tab, target);
                self.aliases.insert(ids.pane, authority.root_pane);
            }
        } else {
            self.aliases.insert(ids.pane, target);
        }
        let aliases = &self.aliases;
        for edit in &mut self.edits {
            remap(&mut edit.mutation, |id| {
                aliases.get(&id).copied().unwrap_or(id)
            });
        }
        // Only an unapproved intent follows creation identities. Approved Close mutations
        // remain immutable, including the Sessions and control grants they name.
        for intent in &mut self.close_intents {
            let id = |id| aliases.get(&id).copied().unwrap_or(id);
            intent.server = match intent.server {
                CloseTarget::Pane {
                    workspace,
                    tab,
                    pane,
                } => CloseTarget::Pane {
                    workspace: id(workspace),
                    tab: id(tab),
                    pane: id(pane),
                },
                CloseTarget::Tab { workspace, tab } => CloseTarget::Tab {
                    workspace: id(workspace),
                    tab: id(tab),
                },
                CloseTarget::Workspace { workspace } => CloseTarget::Workspace {
                    workspace: id(workspace),
                },
                CloseTarget::Worktree { workspace, tab } => CloseTarget::Worktree {
                    workspace: id(workspace),
                    tab: id(tab),
                },
            };
        }
        for presentation in &mut self.presentations {
            remap(&mut presentation.mutation, |id| {
                aliases.get(&id).copied().unwrap_or(id)
            });
            presentation.target = aliases
                .get(&presentation.target)
                .copied()
                .unwrap_or(presentation.target);
        }
    }

    fn reject_dependencies(&mut self, ids: Creation) {
        let mut rejected = vec![ids.pane];
        rejected.extend(ids.workspace);
        rejected.extend(ids.tab);
        // Every dependency points backwards in this FIFO, so one pass rejects the transitive chain.
        self.edits.retain(|edit| {
            if references(&edit.mutation)
                .iter()
                .any(|id| rejected.contains(id))
            {
                if let Some(reply) = &edit.reply {
                    let _ = reply.try_send(Err(SharedRefusalReason::Value));
                }
                if let Some(ids) = edit.creation {
                    rejected.push(ids.pane);
                    rejected.extend(ids.workspace);
                    rejected.extend(ids.tab);
                }
                false
            } else {
                true
            }
        });
        self.close_intents.retain(|intent| {
            !close_references(intent.server)
                .iter()
                .any(|id| rejected.contains(id))
        });
        self.presentations
            .retain(|event| !rejected.contains(&event.target));
    }

    pub fn disconnect(&mut self) {
        let before = self.edits.len();
        let had_close_intents = !self.close_intents.is_empty();
        self.close_intents.clear();
        self.edits.retain(|edit| {
            if edit.dispatched {
                return true;
            }
            if let Some(reply) = &edit.reply {
                let _ = reply.try_send(Err(SharedRefusalReason::Offline));
            }
            false
        });
        if self.edits.len() != before || had_close_intents {
            self.presentations.retain(|event| {
                self.edits
                    .iter()
                    .filter_map(|e| e.creation)
                    .any(|ids| ids.workspace.or(ids.tab).unwrap_or(ids.pane) == event.target)
            });
            self.disconnected = true;
        }
    }

    pub fn take_completions(&mut self) -> Vec<Completion> {
        self.completed.drain(..).collect()
    }
    pub fn take_presentation(&mut self) -> Option<SharedAcceptance> {
        let latest = self.presentations.pop_back();
        if let Some(event) = &latest {
            for edit in &mut self.edits {
                if edit.creation.is_some_and(|ids| {
                    ids.workspace.or(ids.tab).unwrap_or(ids.pane) == event.target
                }) {
                    edit.projected = true;
                }
            }
        }
        self.presentations.clear();
        latest
    }
    pub fn forget_alias(&mut self, id: Id) {
        self.aliases.remove(&id);
    }

    pub fn project(&mut self, mut inventory: Inventory) -> Inventory {
        for edit in &mut self.edits {
            edit.projected = true;
            project(&mut inventory, &edit.mutation, edit.creation);
        }
        inventory
    }
}

fn close_references(target: CloseTarget) -> Vec<Id> {
    match target {
        CloseTarget::Pane {
            workspace,
            tab,
            pane,
        } => vec![workspace, tab, pane],
        CloseTarget::Tab { workspace, tab } | CloseTarget::Worktree { workspace, tab } => {
            vec![workspace, tab]
        }
        CloseTarget::Workspace { workspace } => vec![workspace],
    }
}

fn references(mutation: &Mutation) -> Vec<Id> {
    match mutation {
        Mutation::CreateWorkspace { .. } => vec![],
        Mutation::CreateTab { workspace, source } => {
            std::iter::once(*workspace).chain(*source).collect()
        }
        Mutation::SplitPane {
            workspace,
            tab,
            source,
            ..
        } => vec![*workspace, *tab, *source],
        Mutation::RenameWorkspace { workspace, .. } | Mutation::PinWorkspace { workspace, .. } => {
            vec![*workspace]
        }
        Mutation::Worktree(Operation::OpenTab {
            workspace, source, ..
        }) => std::iter::once(*workspace).chain(*source).collect(),
        // Close approvals and operations with external effects are never retargeted.
        _ => vec![],
    }
}

fn remap(mutation: &mut Mutation, id: impl Fn(Id) -> Id) {
    match mutation {
        Mutation::CreateTab { workspace, source }
        | Mutation::Worktree(Operation::OpenTab {
            workspace, source, ..
        }) => {
            *workspace = id(*workspace);
            if let Some(source) = source {
                *source = id(*source);
            }
        }
        Mutation::SplitPane {
            workspace,
            tab,
            source,
            ..
        } => {
            *workspace = id(*workspace);
            *tab = id(*tab);
            *source = id(*source);
        }
        Mutation::RenameWorkspace { workspace, .. } | Mutation::PinWorkspace { workspace, .. } => {
            *workspace = id(*workspace)
        }
        _ => {}
    }
}

fn new_pane(id: Id, source: Option<Id>, axis: Axis, directory: std::path::PathBuf) -> Pane {
    Pane {
        id,
        source,
        axis,
        starting_directory: directory,
        current_directory: None,
        repository_directory: None,
        presentation: None,
        confirmed_directory: None,
        creator: None,
    }
}

fn project(inventory: &mut Inventory, mutation: &Mutation, creation: Option<Creation>) {
    if let Mutation::CreateWorkspace { name, directory } = mutation {
        if let Some(ids) = creation {
            let tab = ids.tab.expect("a Workspace projection has a Tab");
            inventory.workspaces.push(Workspace {
                id: ids
                    .workspace
                    .expect("a Workspace projection has an identity"),
                name: if name.trim().is_empty() {
                    "Workspace".into()
                } else {
                    name.clone()
                },
                name_version: 0,
                pin_version: 0,
                pinned_directory: directory.clone(),
                root_tab: tab,
                tabs: vec![Tab {
                    id: tab,
                    root_pane: ids.pane,
                    worktree_membership_fixed: false,
                    worktree: None,
                    worktree_repository: None,
                    panes: vec![new_pane(
                        ids.pane,
                        None,
                        Axis::Right,
                        directory.clone().unwrap_or_default(),
                    )],
                }],
            });
        }
        return;
    }
    if let Mutation::Close { confirmation, .. } = mutation {
        // Hide only the exact approved Panes. A later publication never broadens consent.
        for workspace in &mut inventory.workspaces {
            for tab in &mut workspace.tabs {
                tab.panes
                    .retain(|pane| !confirmation.panes.iter().any(|p| p.id == pane.id));
                if !tab.panes.iter().any(|p| p.id == tab.root_pane)
                    && let Some(first) = tab.panes.first()
                {
                    tab.root_pane = first.id;
                }
            }
            workspace.tabs.retain(|tab| !tab.panes.is_empty());
            if !workspace.tabs.iter().any(|t| t.id == workspace.root_tab)
                && let Some(first) = workspace.tabs.first()
            {
                workspace.root_tab = first.id;
            }
        }
        inventory
            .workspaces
            .retain(|workspace| !workspace.tabs.is_empty());
        return;
    }
    let workspace = match mutation {
        Mutation::RenameWorkspace { workspace, .. }
        | Mutation::PinWorkspace { workspace, .. }
        | Mutation::CreateTab { workspace, .. }
        | Mutation::SplitPane { workspace, .. }
        | Mutation::Worktree(Operation::OpenTab { workspace, .. }) => *workspace,
        _ => return,
    };
    let Some(workspace) = inventory.workspaces.iter_mut().find(|w| w.id == workspace) else {
        return;
    };
    match mutation {
        Mutation::RenameWorkspace { name, version, .. } => {
            workspace.name.clone_from(name);
            workspace.name_version = *version;
        }
        Mutation::PinWorkspace {
            directory, version, ..
        } => {
            workspace.pinned_directory.clone_from(directory);
            workspace.pin_version = *version;
        }
        Mutation::CreateTab { source, .. }
        | Mutation::Worktree(Operation::OpenTab { source, .. }) => {
            let Some(ids) = creation else {
                return;
            };
            let source_tab = source.and_then(|id| {
                workspace
                    .tabs
                    .iter()
                    .find(|t| t.panes.iter().any(|p| p.id == id))
            });
            let source_pane =
                source_tab.and_then(|t| t.panes.iter().find(|p| Some(p.id) == *source));
            let directory = workspace
                .pinned_directory
                .clone()
                .or_else(|| source_pane.map(|p| p.presented_directory().to_owned()))
                .unwrap_or_default();
            workspace.tabs.push(Tab {
                id: ids.tab.expect("a Tab projection has an identity"),
                root_pane: ids.pane,
                worktree_membership_fixed: false,
                worktree: source_tab.and_then(|t| t.worktree.clone()),
                worktree_repository: source_tab.and_then(|t| t.worktree_repository.clone()),
                panes: vec![new_pane(ids.pane, None, Axis::Right, directory)],
            });
        }
        Mutation::SplitPane {
            tab, source, axis, ..
        } => {
            let Some(ids) = creation else {
                return;
            };
            if let Some(tab) = workspace.tabs.iter_mut().find(|t| t.id == *tab) {
                let directory = tab
                    .panes
                    .iter()
                    .find(|p| p.id == *source)
                    .map(|p| p.presented_directory().to_owned())
                    .unwrap_or_default();
                tab.panes
                    .push(new_pane(ids.pane, Some(*source), *axis, directory));
            }
        }
        _ => {}
    }
}

pub(super) fn closes_workspace(mutation: &Mutation, workspace: Id) -> bool {
    let Mutation::Close {
        target,
        confirmation,
    } = mutation
    else {
        return false;
    };
    let server = match target {
        spaceterm_server::hierarchy::CloseTarget::Pane { workspace, .. }
        | spaceterm_server::hierarchy::CloseTarget::Tab { workspace, .. }
        | spaceterm_server::hierarchy::CloseTarget::Workspace { workspace }
        | spaceterm_server::hierarchy::CloseTarget::Worktree { workspace, .. } => *workspace,
    };
    server == workspace && confirmation.removes_workspace
}
