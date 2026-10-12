//! View tests exercise the exported view interfaces and the crate-private
//! reconciliation primitives that the lane's state module composes.
//!
//! The shared fixtures below build real, validated core snapshots with retained
//! creation defaults, so reconciliation runs against the frozen core contracts
//! rather than hand-made view inputs. Client-core's replica is represented by
//! the snapshot plus the facts it would map from its connection state.

mod arrangement;
mod attachments;
mod persistence;
mod presentation;
mod reconcile;
mod startup;

use std::collections::BTreeMap;

use spaceterm_core::authority::{
    ConnectionBinding, ConnectionGeneration, ControlGeneration, CreationOrdinal, FieldVersion,
    OrganizationRevision, ServerEpoch, SessionGeneration,
};
use spaceterm_core::identity::{
    ClientId, PairingId, PaneId, RepositoryId, ServerId, TabId, WorkspaceId, WorktreeId,
};
use spaceterm_core::organization::{
    AcceptedPaneCreation, ControlRecord, NameMode, NameText, OrganizationDelta,
    OrganizationSnapshot, OrganizationState, OrganizationWrite, PaneCreation, PaneRecord,
    PaneRuntimeRecord, RepositoryRecord, ServerDirectory, ServerSettingsRecord, SessionPhase,
    SplitAxis, SplitEdge, TabRecord, Versioned, WorkspaceName, WorkspaceRecord,
    WorktreeLocationTemplate, WorktreeRecord,
};
use spaceterm_core::organization_retained::{
    OrganizationRetainedFacts, OrganizationRetainedParts, PaneGenerationHighWater,
    RepositoryInventoryVersion, TabCreationDefault, TabMembershipVersion, WorkspaceNameBasis,
    WorktreeInventoryFacts,
};
use spaceterm_core::remote_access::RemoteAccessPolicy;

use crate::view::{
    ArrangementMetrics, ArrangementNode, ArrangementRecord, AttachmentIncarnation, ClientViewState,
    PaneNodeRef, PaneSize, PaneTreeRef, PersistedView, PersistedViewParts, PersistedWorkspace,
    ReplicaFacts, ReplicaObservation, ReplicaReadiness, RestorationFilter, ServerViewStatus,
    SplitId, TabOrderRecord, ViewAvailability, ViewError, ViewRevision, ViewUpdate,
    WorkspaceAttachment, WorkspaceView,
};

pub(super) const A: u8 = 7;
pub(super) const B: u8 = 8;

pub(super) fn server(byte: u8) -> ServerId {
    ServerId::new([byte; 16])
}

pub(super) fn ws(byte: u8, local: u64) -> WorkspaceId {
    WorkspaceId::new(server(byte), local).unwrap()
}

pub(super) fn tab(byte: u8, local: u64) -> TabId {
    TabId::new(server(byte), local).unwrap()
}

pub(super) fn pane(byte: u8, local: u64) -> PaneId {
    PaneId::new(server(byte), local).unwrap()
}

pub(super) fn worktree(byte: u8, local: u64) -> WorktreeId {
    WorktreeId::new(server(byte), local).unwrap()
}

pub(super) fn pairing(byte: u8, local: u64) -> PairingId {
    PairingId::new(server(byte), local).unwrap()
}

pub(super) fn client(byte: u8, local: u64) -> ClientId {
    ClientId::new(server(byte), local).unwrap()
}

/// Facts for a Ready replica of `byte` for one pairing and stable Client.
pub(super) fn ready_client(byte: u8, pairing_local: u64, client_local: u64) -> ReplicaFacts {
    ReplicaFacts {
        pairing: pairing(byte, pairing_local),
        client: client(byte, client_local),
        connection: Some(ConnectionBinding::new(
            client(byte, client_local),
            ConnectionGeneration::new(1),
        )),
        readiness: ReplicaReadiness::Ready,
        status: ServerViewStatus::Connected,
    }
}

/// Facts for a Ready replica of `byte` under pairing `pairing_local` and Client 1.
pub(super) fn ready_as(byte: u8, pairing_local: u64) -> ReplicaFacts {
    ready_client(byte, pairing_local, 1)
}

pub(super) fn ready(byte: u8) -> ReplicaFacts {
    ready_as(byte, 1)
}

/// Facts for a retained replica of an unreachable Server.
pub(super) fn offline(byte: u8) -> ReplicaFacts {
    ReplicaFacts {
        connection: None,
        readiness: ReplicaReadiness::Retained,
        status: ServerViewStatus::Offline,
        ..ready(byte)
    }
}

/// Facts for a connected replica that has not finished reconciling.
pub(super) fn synchronizing(byte: u8) -> ReplicaFacts {
    ReplicaFacts {
        readiness: ReplicaReadiness::Synchronizing,
        ..ready(byte)
    }
}

pub(super) fn metrics() -> ArrangementMetrics {
    ArrangementMetrics {
        minimum_leaf: PaneSize::new(10.0, 10.0).unwrap(),
        divider: 1.0,
    }
}

pub(super) fn space() -> PaneSize {
    PaneSize::new(1000.0, 1000.0).unwrap()
}

/// Attaches one Workspace of a Ready replica with no accepted placements.
pub(super) fn attach(
    state: &mut ClientViewState,
    org: &Org,
    facts: ReplicaFacts,
    workspace: u64,
) -> Result<ViewUpdate, ViewError> {
    let snapshot = org.snapshot();
    state.attach(ws(org.byte, workspace), &observe(&snapshot, &[], facts))
}

/// Reconciles against the builder's current snapshot.
pub(super) fn reconcile(
    state: &mut ClientViewState,
    org: &Org,
    accepted: &[AcceptedPaneCreation],
    facts: ReplicaFacts,
) -> Result<ViewUpdate, ViewError> {
    let snapshot = org.snapshot();
    state.reconcile(&observe(&snapshot, accepted, facts))
}

/// A retained one-Pane arrangement.
pub(super) fn single(byte: u8, pane_local: u64) -> ArrangementRecord {
    ArrangementRecord {
        root: 0,
        nodes: vec![ArrangementNode::Pane(pane(byte, pane_local))],
        split_high_water: 0,
        focused: pane(byte, pane_local),
        focus_history: Vec::new(),
        zoomed: None,
    }
}

/// A retained right-leaning chain `H(p1, H(p2, ... pn))` in compact pre-order.
pub(super) fn chain(byte: u8, first_pane: u64, count: u64) -> ArrangementRecord {
    let mut nodes = Vec::new();
    for offset in 0..count - 1 {
        let index = u32::try_from(nodes.len()).unwrap();
        nodes.push(ArrangementNode::Split {
            id: SplitId::new(offset + 1).unwrap(),
            axis: SplitAxis::Horizontal,
            ratio: 0.5,
            first: index + 1,
            second: index + 2,
        });
        nodes.push(ArrangementNode::Pane(pane(byte, first_pane + offset)));
    }
    nodes.push(ArrangementNode::Pane(pane(byte, first_pane + count - 1)));
    ArrangementRecord {
        root: 0,
        nodes,
        split_high_water: count - 1,
        focused: pane(byte, first_pane),
        focus_history: Vec::new(),
        zoomed: None,
    }
}

/// A retained attachment of `byte` under pairing 1 and Client 1 with Tabs in
/// the given order, the first one active.
pub(super) fn retained_workspace(
    byte: u8,
    workspace: u64,
    incarnation: u64,
    tabs: Vec<(u64, ArrangementRecord)>,
) -> PersistedWorkspace {
    let order: Vec<TabId> = tabs.iter().map(|(local, _)| tab(byte, *local)).collect();
    let active = order.first().copied();
    PersistedWorkspace {
        attachment: WorkspaceAttachment::new(
            ws(byte, workspace),
            pairing(byte, 1),
            client(byte, 1),
            AttachmentIncarnation::new(incarnation).unwrap(),
        )
        .unwrap(),
        availability: ViewAvailability::Available,
        tabs: TabOrderRecord {
            order,
            active,
            activation_history: active.into_iter().collect(),
        },
        arrangements: tabs.into_iter().map(|(_, record)| record).collect(),
    }
}

/// A view restored, without connectivity, from retained Workspaces.
pub(super) fn restored(
    revision: u64,
    incarnation_high_water: u64,
    workspaces: Vec<PersistedWorkspace>,
) -> ClientViewState {
    let active = workspaces
        .last()
        .map(|workspace| workspace.attachment.workspace());
    let view = PersistedView::new(PersistedViewParts {
        revision: ViewRevision::new(revision),
        incarnation_high_water,
        workspaces,
        active,
    })
    .unwrap();
    let (state, update) = ClientViewState::restore(Some(&view), &RestorationFilter::new()).unwrap();
    assert!(update.is_empty());
    state
}

pub(super) fn observe<'a>(
    snapshot: &'a OrganizationSnapshot,
    accepted: &'a [AcceptedPaneCreation],
    facts: ReplicaFacts,
) -> ReplicaObservation<'a> {
    ReplicaObservation::new(snapshot, accepted, facts).unwrap()
}

/// Renders an arrangement as `H(1,V(2,3))` with Pane local identities.
pub(super) fn shape(view: &WorkspaceView, tab: TabId) -> String {
    fn render(tree: PaneTreeRef<'_>, out: &mut String) {
        match tree.node() {
            PaneNodeRef::Pane(pane) => out.push_str(&pane.local().to_string()),
            PaneNodeRef::Split {
                axis,
                first,
                second,
                ..
            } => {
                out.push(if axis == SplitAxis::Horizontal {
                    'H'
                } else {
                    'V'
                });
                out.push('(');
                render(first, out);
                out.push(',');
                render(second, out);
                out.push(')');
            }
        }
    }

    let mut out = String::new();
    render(view.arrangement(tab).unwrap().tree(), &mut out);
    out
}

/// Split identities and ratios of one arrangement in visual pre-order.
pub(super) fn ratios(view: &WorkspaceView, tab: TabId) -> Vec<(u64, f32)> {
    view.arrangement(tab)
        .unwrap()
        .export()
        .nodes
        .iter()
        .filter_map(|node| match *node {
            ArrangementNode::Split { id, ratio, .. } => Some((id.get(), ratio)),
            ArrangementNode::Pane(_) => None,
        })
        .collect()
}

pub(super) fn attached(state: &ClientViewState) -> Vec<(u8, u64)> {
    state
        .attachments()
        .iter()
        .map(|attachment| {
            (
                attachment.server().bytes()[0],
                attachment.workspace().local(),
            )
        })
        .collect()
}

/// Runtime facts published for one Pane.
#[derive(Clone, Copy)]
pub(super) struct Runtime {
    pub session: u64,
    pub phase: SessionPhase,
    pub control_generation: u64,
    pub control: ControlRecord,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            session: 0,
            phase: SessionPhase::NotStarted,
            control_generation: 0,
            control: ControlRecord::Unclaimed,
        }
    }
}

/// Builder for a valid authoritative organization of one Server.
///
/// Creation ordinals are allocated globally in call order, and each Tab's
/// normalized replay order follows Pane creation.
#[derive(Clone)]
pub(super) struct Org {
    byte: u8,
    epoch: u64,
    revision: u64,
    next_ordinal: u64,
    workspaces: Vec<WorkspaceRecord>,
    tabs: Vec<TabRecord>,
    panes: Vec<PaneRecord>,
    defaults: BTreeMap<TabId, Vec<PaneId>>,
    worktrees: Vec<u64>,
    runtime: BTreeMap<PaneId, Runtime>,
    label: String,
    directory: Vec<u8>,
}

impl Org {
    pub fn new(byte: u8) -> Self {
        Self {
            byte,
            epoch: 1,
            revision: 1,
            next_ordinal: 1,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            defaults: BTreeMap::new(),
            worktrees: Vec::new(),
            runtime: BTreeMap::new(),
            label: "work".to_owned(),
            directory: b"/work".to_vec(),
        }
    }

    pub fn server(&self) -> ServerId {
        server(self.byte)
    }

    fn ordinal(&mut self) -> CreationOrdinal {
        let ordinal = CreationOrdinal::new(self.next_ordinal);
        self.next_ordinal += 1;
        ordinal
    }

    fn directory(&self) -> ServerDirectory {
        ServerDirectory::new(self.server(), self.directory.clone()).unwrap()
    }

    fn versioned<T>(value: T) -> Versioned<T> {
        Versioned {
            value,
            version: FieldVersion::new(1),
        }
    }

    /// Names and directories are display and launch facts the view never retains.
    pub fn relabel(&mut self, label: &str, directory: &[u8]) -> &mut Self {
        self.label = label.to_owned();
        self.directory = directory.to_vec();
        let name = WorkspaceName {
            mode: NameMode::Custom,
            display: NameText::new(label.to_owned()).unwrap(),
        };
        let directory = self.directory();
        for workspace in &mut self.workspaces {
            workspace.name = Self::versioned(name.clone());
            workspace.pinned_directory = Self::versioned(Some(directory.clone()));
        }
        for pane in &mut self.panes {
            pane.starting_directory = directory.clone();
            pane.confirmed_restoration_directory = Self::versioned(Some(directory.clone()));
        }
        self
    }

    pub fn set_runtime(&mut self, pane_local: u64, runtime: Runtime) -> &mut Self {
        self.runtime.insert(pane(self.byte, pane_local), runtime);
        self
    }

    /// A Workspace with its Root Tab and that Tab's Root Pane.
    pub fn add_workspace(&mut self, workspace: u64, root_tab: u64, root_pane: u64) -> &mut Self {
        let ordinal = self.ordinal();
        self.workspaces.push(WorkspaceRecord {
            id: ws(self.byte, workspace),
            name: Self::versioned(WorkspaceName {
                mode: NameMode::Custom,
                display: NameText::new(self.label.clone()).unwrap(),
            }),
            pinned_directory: Self::versioned(None),
            root_tab: tab(self.byte, root_tab),
            creation_ordinal: ordinal,
        });
        self.add_tab(workspace, root_tab, root_pane, None)
    }

    /// A Tab with its initial Root Pane.
    pub fn add_tab(
        &mut self,
        workspace: u64,
        tab_local: u64,
        root_pane: u64,
        worktree_local: Option<u64>,
    ) -> &mut Self {
        let ordinal = self.ordinal();
        if let Some(local) = worktree_local
            && !self.worktrees.contains(&local)
        {
            self.worktrees.push(local);
        }
        self.tabs.push(TabRecord {
            id: tab(self.byte, tab_local),
            workspace: ws(self.byte, workspace),
            worktree: worktree_local.map(|local| worktree(self.byte, local)),
            root_pane: pane(self.byte, root_pane),
            creation_ordinal: ordinal,
        });
        self.add_pane(tab_local, root_pane, PaneCreation::Initial)
    }

    /// A Pane split from `source` in the same Tab.
    pub fn split(
        &mut self,
        tab_local: u64,
        pane_local: u64,
        source: u64,
        axis: SplitAxis,
        edge: SplitEdge,
    ) -> &mut Self {
        self.add_pane(
            tab_local,
            pane_local,
            PaneCreation::SplitOf {
                source: pane(self.byte, source),
                axis,
                edge,
            },
        )
    }

    /// Splits `count` Panes in a right-leaning chain after `source`.
    pub fn chain(&mut self, tab_local: u64, source: u64, first: u64, count: u64) -> &mut Self {
        let mut previous = source;
        for local in first..first + count {
            self.split(
                tab_local,
                local,
                previous,
                SplitAxis::Horizontal,
                SplitEdge::After,
            );
            previous = local;
        }
        self
    }

    fn add_pane(&mut self, tab_local: u64, pane_local: u64, creation: PaneCreation) -> &mut Self {
        let ordinal = self.ordinal();
        let directory = self.directory();
        self.panes.push(PaneRecord {
            id: pane(self.byte, pane_local),
            tab: tab(self.byte, tab_local),
            creation_ordinal: ordinal,
            creation,
            starting_directory: directory,
            confirmed_restoration_directory: Self::versioned(None),
        });
        self.defaults
            .entry(tab(self.byte, tab_local))
            .or_default()
            .push(pane(self.byte, pane_local));
        self
    }

    /// Deletes a Pane that is no other Pane's creation source.
    pub fn delete_pane(&mut self, pane_local: u64) -> &mut Self {
        let id = pane(self.byte, pane_local);
        assert!(
            !self.panes.iter().any(|record| matches!(
                record.creation,
                PaneCreation::SplitOf { source, .. } if source == id
            )),
            "relink dependents before deleting a source"
        );
        self.panes.retain(|record| record.id != id);
        for order in self.defaults.values_mut() {
            order.retain(|candidate| *candidate != id);
        }
        self.runtime.remove(&id);
        self
    }

    /// Rewrites one Pane's normalized creation link.
    pub fn relink(&mut self, pane_local: u64, creation: PaneCreation) -> &mut Self {
        let id = pane(self.byte, pane_local);
        let record = self
            .panes
            .iter_mut()
            .find(|record| record.id == id)
            .unwrap();
        record.creation = creation;
        self
    }

    pub fn set_root_pane(&mut self, tab_local: u64, pane_local: u64) -> &mut Self {
        let id = tab(self.byte, tab_local);
        let byte = self.byte;
        self.tabs
            .iter_mut()
            .find(|record| record.id == id)
            .unwrap()
            .root_pane = pane(byte, pane_local);
        self
    }

    pub fn set_root_tab(&mut self, workspace: u64, tab_local: u64) -> &mut Self {
        let id = ws(self.byte, workspace);
        let byte = self.byte;
        self.workspaces
            .iter_mut()
            .find(|record| record.id == id)
            .unwrap()
            .root_tab = tab(byte, tab_local);
        self
    }

    pub fn set_worktree(&mut self, tab_local: u64, worktree_local: Option<u64>) -> &mut Self {
        if let Some(local) = worktree_local
            && !self.worktrees.contains(&local)
        {
            self.worktrees.push(local);
        }
        let id = tab(self.byte, tab_local);
        let byte = self.byte;
        self.tabs
            .iter_mut()
            .find(|record| record.id == id)
            .unwrap()
            .worktree = worktree_local.map(|local| worktree(byte, local));
        self
    }

    pub fn delete_tab(&mut self, tab_local: u64) -> &mut Self {
        let id = tab(self.byte, tab_local);
        self.tabs.retain(|record| record.id != id);
        let removed: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|record| record.tab == id)
            .map(|record| record.id)
            .collect();
        self.panes.retain(|record| record.tab != id);
        for pane in removed {
            self.runtime.remove(&pane);
        }
        self.defaults.remove(&id);
        self
    }

    /// Kills a Workspace with all of its Tabs and Panes.
    pub fn delete_workspace(&mut self, workspace: u64) -> &mut Self {
        let id = ws(self.byte, workspace);
        self.workspaces.retain(|record| record.id != id);
        let tabs: Vec<u64> = self
            .tabs
            .iter()
            .filter(|record| record.workspace == id)
            .map(|record| record.id.local())
            .collect();
        for local in tabs {
            self.delete_tab(local);
        }
        self
    }

    /// Advances the publication revision.
    pub fn publish(&mut self) -> &mut Self {
        self.revision += 1;
        self
    }

    pub fn restart(&mut self) -> &mut Self {
        self.epoch += 1;
        self
    }

    pub fn revision(&self) -> OrganizationRevision {
        OrganizationRevision::new(self.revision)
    }

    pub fn snapshot(&self) -> OrganizationSnapshot {
        let server = self.server();
        let mut workspaces = self.workspaces.clone();
        workspaces.sort_by_key(|record| record.id);
        let mut tabs = self.tabs.clone();
        tabs.sort_by_key(|record| record.id);
        let mut panes = self.panes.clone();
        panes.sort_by_key(|record| record.id);
        let repositories = if self.worktrees.is_empty() {
            Vec::new()
        } else {
            vec![RepositoryRecord {
                id: RepositoryId::new(server, 1).unwrap(),
                directory: self.directory(),
            }]
        };
        let mut worktree_locals = self.worktrees.clone();
        worktree_locals.sort_unstable();
        let worktrees: Vec<WorktreeRecord> = worktree_locals
            .iter()
            .map(|local| WorktreeRecord {
                id: worktree(self.byte, *local),
                repository: RepositoryId::new(server, 1).unwrap(),
                directory: self.directory(),
                main: *local == 1,
                missing: false,
            })
            .collect();
        let runtime: Vec<PaneRuntimeRecord> = panes
            .iter()
            .map(|record| {
                let facts = self.runtime.get(&record.id).copied().unwrap_or_default();
                PaneRuntimeRecord {
                    pane: record.id,
                    session_generation: SessionGeneration::new(facts.session),
                    phase: facts.phase,
                    control_generation: ControlGeneration::new(facts.control_generation),
                    control: facts.control,
                }
            })
            .collect();
        let retained = OrganizationRetainedFacts::new(OrganizationRetainedParts {
            server,
            workspace_names: workspaces
                .iter()
                .map(|record| {
                    WorkspaceNameBasis::new(
                        record.id,
                        NameText::new("work".to_owned()).unwrap(),
                        NameText::new("work".to_owned()).unwrap(),
                    )
                })
                .collect(),
            creation_defaults: tabs
                .iter()
                .map(|record| {
                    TabCreationDefault::new(record.id, self.defaults[&record.id].clone()).unwrap()
                })
                .collect(),
            pane_generations: runtime
                .iter()
                .map(|record| {
                    PaneGenerationHighWater::new(
                        record.pane,
                        record.session_generation,
                        record.control_generation,
                    )
                })
                .collect(),
            tab_membership_versions: tabs
                .iter()
                .map(|record| TabMembershipVersion::new(record.id, FieldVersion::new(1)))
                .collect(),
            repository_inventory_versions: repositories
                .iter()
                .map(|record| RepositoryInventoryVersion::new(record.id, FieldVersion::new(1)))
                .collect(),
            worktree_inventory: worktrees
                .iter()
                .map(|record| WorktreeInventoryFacts::new(record.id, true, FieldVersion::new(1)))
                .collect(),
            remote_access: Self::versioned(RemoteAccessPolicy::disabled(server)),
        })
        .unwrap();
        let state = OrganizationState {
            server,
            revision: self.revision(),
            settings: ServerSettingsRecord {
                worktree_location: Self::versioned(
                    WorktreeLocationTemplate::new("../{repo}/{branch}".to_owned()).unwrap(),
                ),
            },
            workspaces,
            tabs,
            panes,
            repositories,
            worktrees,
            retained: Some(retained),
        };
        OrganizationSnapshot::new(ServerEpoch::new(self.epoch), state, runtime).unwrap()
    }

    /// The snapshot without retained creation defaults, as a Wave 1 replica.
    pub fn legacy_snapshot(&self) -> OrganizationSnapshot {
        let snapshot = self.snapshot();
        let mut state = snapshot.state().clone();
        state.retained = None;
        OrganizationSnapshot::new(snapshot.epoch(), state, snapshot.runtime().to_vec()).unwrap()
    }

    /// The original placement of a Pane created in the current publication.
    pub fn accepted(
        &self,
        pane_local: u64,
        source: u64,
        axis: SplitAxis,
        edge: SplitEdge,
    ) -> AcceptedPaneCreation {
        let id = pane(self.byte, pane_local);
        let record = self
            .panes
            .iter()
            .find(|record| record.id == id)
            .unwrap()
            .clone();
        let original = PaneCreation::SplitOf {
            source: pane(self.byte, source),
            axis,
            edge,
        };
        let delta = OrganizationDelta::new(
            self.server(),
            ServerEpoch::new(self.epoch),
            OrganizationRevision::new(self.revision - 1),
            self.revision(),
            vec![OrganizationWrite::PutPane(record.clone())],
            Vec::new(),
        )
        .unwrap();
        AcceptedPaneCreation::new(&delta, record.tab, id, record.creation_ordinal, original)
            .unwrap()
    }
}
