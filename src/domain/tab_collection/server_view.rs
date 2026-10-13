//! A read-only adapter that presents one server Workspace's Tabs.
//!
//! It keeps one payload for each Tab in this client's personal order, keyed by the
//! server-qualified Tab ID. Membership, the Root Tab and each Tab's Worktree come from records.
//! Order and activation come from the personal [`WorkspaceView`]. It never allocates a shared ID,
//! chooses a Root, or deletes membership: a Tab leaves only when a newer personal view no longer
//! represents it.

use std::collections::BTreeMap;

use spaceterm_client::view::WorkspaceView;
use spaceterm_core::identity::{TabId, WorkspaceId, WorktreeId};
use spaceterm_core::organization::{TabRecord, WorkspaceRecord};

use crate::domain::tab::server_view::ServerViewError;

/// One server Workspace's presented Tabs and the payload that presents each one.
pub(crate) struct ServerTabCollection<T> {
    workspace: WorkspaceId,
    root_tab: TabId,
    /// Tabs in personal order that the records also hold. Only these are presented.
    order: Vec<TabId>,
    active: Option<TabId>,
    /// Each presented Tab's Worktree, as recorded.
    scopes: BTreeMap<TabId, Option<WorktreeId>>,
    /// Every Tab the personal view represents, so a record missing from one publication does not
    /// drop presentation the next one restores.
    represented: Vec<TabId>,
    payloads: BTreeMap<TabId, T>,
}

impl<T> ServerTabCollection<T> {
    /// Presents the Workspace in `record` with `tabs` from the same publication and `view`.
    pub(crate) fn new(
        record: &WorkspaceRecord,
        tabs: &[TabRecord],
        view: &WorkspaceView,
    ) -> Result<Self, ServerViewError> {
        let mut collection = Self {
            workspace: record.id,
            root_tab: record.root_tab,
            order: Vec::new(),
            active: None,
            scopes: BTreeMap::new(),
            represented: Vec::new(),
            payloads: BTreeMap::new(),
        };
        collection.apply(record, tabs, view)?;
        Ok(collection)
    }

    /// Applies a newer publication of the same Workspace. It returns the payloads of Tabs the
    /// personal view no longer represents, in Tab order. A refused input changes nothing.
    pub(crate) fn apply(
        &mut self,
        record: &WorkspaceRecord,
        tabs: &[TabRecord],
        view: &WorkspaceView,
    ) -> Result<Vec<(TabId, T)>, ServerViewError> {
        if record.id != self.workspace || view.workspace() != self.workspace {
            return Err(ServerViewError::Mismatch);
        }
        let scopes: BTreeMap<TabId, Option<WorktreeId>> = tabs
            .iter()
            .filter(|tab| tab.workspace == self.workspace)
            .map(|tab| (tab.id, tab.worktree))
            .collect();
        let represented = view.tab_order().to_vec();
        let order: Vec<TabId> = represented
            .iter()
            .copied()
            .filter(|tab| scopes.contains_key(tab))
            .collect();
        let active = view.active_tab().filter(|tab| order.contains(tab));
        let released: Vec<TabId> = self
            .payloads
            .keys()
            .copied()
            .filter(|tab| !represented.contains(tab))
            .collect();
        self.root_tab = record.root_tab;
        self.scopes = order
            .iter()
            .map(|tab| (*tab, scopes.get(tab).copied().flatten()))
            .collect();
        self.order = order;
        self.active = active;
        self.represented = represented;
        Ok(released
            .into_iter()
            .filter_map(|tab| self.payloads.remove(&tab).map(|payload| (tab, payload)))
            .collect())
    }

    pub(crate) const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    /// The Root Tab the server records. Personal order never chooses it.
    #[cfg(test)]
    pub(crate) const fn root_tab(&self) -> TabId {
        self.root_tab
    }

    /// Presented Tabs in personal order.
    #[cfg(test)]
    pub(crate) fn order(&self) -> &[TabId] {
        &self.order
    }

    pub(crate) const fn active_tab(&self) -> Option<TabId> {
        self.active
    }

    /// The Active Tab's Worktree, or None for the Workspace's own Tabs and for no Active Tab.
    pub(crate) fn active_scope(&self) -> Option<WorktreeId> {
        self.active
            .and_then(|tab| self.scopes.get(&tab).copied().flatten())
    }

    /// A presented Tab's Worktree.
    pub(crate) fn scope_of(&self, tab: TabId) -> Result<Option<WorktreeId>, ServerViewError> {
        self.scopes
            .get(&tab)
            .copied()
            .ok_or(ServerViewError::NotPresented)
    }

    /// One Worktree's presented Tabs, in personal order.
    pub(crate) fn tabs_in(&self, scope: Option<WorktreeId>) -> Vec<TabId> {
        self.order
            .iter()
            .copied()
            .filter(|tab| self.scopes.get(tab).copied().flatten() == scope)
            .collect()
    }

    /// The Tabs the strip shows: the Active Worktree's, in personal order.
    pub(crate) fn visible(&self) -> Vec<TabId> {
        self.tabs_in(self.active_scope())
    }

    /// Presented Tabs that have no payload yet, in personal order.
    pub(crate) fn missing(&self) -> Vec<TabId> {
        self.order
            .iter()
            .copied()
            .filter(|tab| !self.payloads.contains_key(tab))
            .collect()
    }

    /// Presents a represented Tab with `payload`. It returns the payload it replaces, if any.
    pub(crate) fn insert(&mut self, tab: TabId, payload: T) -> Result<Option<T>, ServerViewError> {
        if !self.order.contains(&tab) {
            return Err(ServerViewError::NotPresented);
        }
        Ok(self.payloads.insert(tab, payload))
    }

    /// A presented Tab's payload.
    pub(crate) fn tab(&self, tab: TabId) -> Option<&T> {
        if !self.order.contains(&tab) {
            return None;
        }
        self.payloads.get(&tab)
    }

    /// Presented Tabs with payloads, in personal order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (TabId, &T)> {
        self.order
            .iter()
            .filter_map(|tab| self.payloads.get(tab).map(|payload| (*tab, payload)))
    }

    /// Every payload, presented or retained, for teardown.
    pub(crate) fn payloads(&self) -> impl Iterator<Item = (TabId, &T)> {
        self.payloads.iter().map(|(tab, payload)| (*tab, payload))
    }
}

/// Validated core snapshots and reconciled personal views for server-backed tests.
///
/// The builder follows client-view's own fixture: every snapshot carries retained creation
/// defaults, so reconciliation runs against the frozen core contracts.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use spaceterm_client::view::{
        ClientViewState, PaneArrangement, ReplicaFacts, ReplicaObservation, ReplicaReadiness,
        ServerViewStatus, WorkspaceView,
    };
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

    pub(crate) const A: u8 = 7;
    pub(crate) const B: u8 = 8;

    pub(crate) fn server(byte: u8) -> ServerId {
        ServerId::new([byte; 16])
    }

    pub(crate) fn ws(byte: u8, local: u64) -> WorkspaceId {
        WorkspaceId::new(server(byte), local).unwrap()
    }

    pub(crate) fn tab(byte: u8, local: u64) -> TabId {
        TabId::new(server(byte), local).unwrap()
    }

    pub(crate) fn pane(byte: u8, local: u64) -> PaneId {
        PaneId::new(server(byte), local).unwrap()
    }

    pub(crate) fn worktree(byte: u8, local: u64) -> WorktreeId {
        WorktreeId::new(server(byte), local).unwrap()
    }

    fn pairing(byte: u8) -> PairingId {
        PairingId::new(server(byte), 1).unwrap()
    }

    fn client(byte: u8) -> ClientId {
        ClientId::new(server(byte), 1).unwrap()
    }

    /// Facts for a Ready replica of `byte` under pairing 1 and Client 1.
    pub(crate) fn ready(byte: u8) -> ReplicaFacts {
        ReplicaFacts {
            pairing: pairing(byte),
            client: client(byte),
            connection: Some(ConnectionBinding::new(
                client(byte),
                ConnectionGeneration::new(1),
            )),
            readiness: ReplicaReadiness::Ready,
            status: ServerViewStatus::Connected,
        }
    }

    /// Facts for a retained replica of an unreachable server.
    pub(crate) fn offline(byte: u8) -> ReplicaFacts {
        ReplicaFacts {
            connection: None,
            readiness: ReplicaReadiness::Retained,
            status: ServerViewStatus::Offline,
            ..ready(byte)
        }
    }

    /// A Ready replica and one client's personal view of it.
    #[derive(Clone)]
    pub(crate) struct Replica {
        pub(crate) org: Org,
        pub(crate) personal: ClientViewState,
    }

    impl Replica {
        /// Attaches Workspace `workspace` of `org` on a Ready replica.
        pub(crate) fn attached(org: Org, workspace: u64) -> Self {
            let mut personal = ClientViewState::new();
            let snapshot = org.snapshot();
            personal
                .attach(
                    ws(org.byte, workspace),
                    &ReplicaObservation::new(&snapshot, &[], ready(org.byte)).unwrap(),
                )
                .unwrap();
            Self { org, personal }
        }

        /// Reconciles the personal view with the builder's current snapshot on a Ready replica.
        pub(crate) fn reconcile_ready(&mut self, accepted: &[AcceptedPaneCreation]) {
            let facts = ready(self.org.byte);
            self.reconcile(accepted, facts);
        }

        pub(crate) fn reconcile(&mut self, accepted: &[AcceptedPaneCreation], facts: ReplicaFacts) {
            let snapshot = self.org.snapshot();
            self.personal
                .reconcile(&ReplicaObservation::new(&snapshot, accepted, facts).unwrap())
                .unwrap();
        }

        pub(crate) fn snapshot(&self) -> Arc<OrganizationSnapshot> {
            Arc::new(self.org.snapshot())
        }

        pub(crate) fn workspace_id(&self, workspace: u64) -> WorkspaceId {
            ws(self.org.byte, workspace)
        }

        pub(crate) fn workspace_record(&self, workspace: u64) -> WorkspaceRecord {
            let id = self.workspace_id(workspace);
            self.org
                .workspaces
                .iter()
                .find(|record| record.id == id)
                .cloned()
                .unwrap()
        }

        pub(crate) fn tab_records(&self) -> Vec<TabRecord> {
            self.org.snapshot().state().tabs.clone()
        }

        pub(crate) fn tab_record(&self, tab_local: u64) -> TabRecord {
            let id = tab(self.org.byte, tab_local);
            self.org
                .tabs
                .iter()
                .find(|record| record.id == id)
                .cloned()
                .unwrap()
        }

        pub(crate) fn view(&self, workspace: u64) -> &WorkspaceView {
            self.personal
                .workspace(self.workspace_id(workspace))
                .unwrap()
        }

        pub(crate) fn arrangement(&self, workspace: u64, tab_local: u64) -> PaneArrangement {
            self.view(workspace)
                .arrangement(tab(self.org.byte, tab_local))
                .unwrap()
                .clone()
        }
    }

    /// Builder for a valid authoritative organization of one server.
    #[derive(Clone)]
    pub(crate) struct Org {
        byte: u8,
        revision: u64,
        next_ordinal: u64,
        workspaces: Vec<WorkspaceRecord>,
        tabs: Vec<TabRecord>,
        panes: Vec<PaneRecord>,
        defaults: BTreeMap<TabId, Vec<PaneId>>,
        worktrees: Vec<u64>,
    }

    impl Org {
        pub(crate) fn new(byte: u8) -> Self {
            Self {
                byte,
                revision: 1,
                next_ordinal: 1,
                workspaces: Vec::new(),
                tabs: Vec::new(),
                panes: Vec::new(),
                defaults: BTreeMap::new(),
                worktrees: Vec::new(),
            }
        }

        pub(crate) fn server(&self) -> ServerId {
            server(self.byte)
        }

        fn ordinal(&mut self) -> CreationOrdinal {
            let ordinal = CreationOrdinal::new(self.next_ordinal);
            self.next_ordinal += 1;
            ordinal
        }

        fn directory(&self) -> ServerDirectory {
            ServerDirectory::new(self.server(), b"/work".to_vec()).unwrap()
        }

        fn versioned<T>(value: T) -> Versioned<T> {
            Versioned {
                value,
                version: FieldVersion::new(1),
            }
        }

        /// A Workspace with its Root Tab and that Tab's Root Pane.
        pub(crate) fn add_workspace(
            &mut self,
            workspace: u64,
            root_tab: u64,
            root_pane: u64,
        ) -> &mut Self {
            let ordinal = self.ordinal();
            self.workspaces.push(WorkspaceRecord {
                id: ws(self.byte, workspace),
                name: Self::versioned(WorkspaceName {
                    mode: NameMode::Custom,
                    display: NameText::new("work".to_owned()).unwrap(),
                }),
                pinned_directory: Self::versioned(None),
                root_tab: tab(self.byte, root_tab),
                creation_ordinal: ordinal,
            });
            self.add_tab(workspace, root_tab, root_pane, None)
        }

        /// A Tab with its initial Root Pane.
        pub(crate) fn add_tab(
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
        pub(crate) fn split(
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

        fn add_pane(
            &mut self,
            tab_local: u64,
            pane_local: u64,
            creation: PaneCreation,
        ) -> &mut Self {
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
        pub(crate) fn delete_pane(&mut self, pane_local: u64) -> &mut Self {
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
            self
        }

        /// Rewrites one Pane's normalized creation link.
        pub(crate) fn relink(&mut self, pane_local: u64, creation: PaneCreation) -> &mut Self {
            let id = pane(self.byte, pane_local);
            self.panes
                .iter_mut()
                .find(|record| record.id == id)
                .unwrap()
                .creation = creation;
            self
        }

        pub(crate) fn set_root_pane(&mut self, tab_local: u64, pane_local: u64) -> &mut Self {
            let id = tab(self.byte, tab_local);
            let root = pane(self.byte, pane_local);
            self.tabs
                .iter_mut()
                .find(|record| record.id == id)
                .unwrap()
                .root_pane = root;
            self
        }

        pub(crate) fn set_root_tab(&mut self, workspace: u64, tab_local: u64) -> &mut Self {
            let id = ws(self.byte, workspace);
            let root = tab(self.byte, tab_local);
            self.workspaces
                .iter_mut()
                .find(|record| record.id == id)
                .unwrap()
                .root_tab = root;
            self
        }

        pub(crate) fn delete_tab(&mut self, tab_local: u64) -> &mut Self {
            let id = tab(self.byte, tab_local);
            self.tabs.retain(|record| record.id != id);
            self.panes.retain(|record| record.tab != id);
            self.defaults.remove(&id);
            self
        }

        /// Advances the publication revision.
        pub(crate) fn publish(&mut self) -> &mut Self {
            self.revision += 1;
            self
        }

        pub(crate) fn snapshot(&self) -> OrganizationSnapshot {
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
                .map(|record| PaneRuntimeRecord {
                    pane: record.id,
                    session_generation: SessionGeneration::new(0),
                    phase: SessionPhase::NotStarted,
                    control_generation: ControlGeneration::new(0),
                    control: ControlRecord::Unclaimed,
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
                        TabCreationDefault::new(record.id, self.defaults[&record.id].clone())
                            .unwrap()
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
                    .map(|record| {
                        WorktreeInventoryFacts::new(record.id, true, FieldVersion::new(1))
                    })
                    .collect(),
                remote_access: Self::versioned(RemoteAccessPolicy::disabled(server)),
            })
            .unwrap();
            let state = OrganizationState {
                server,
                revision: OrganizationRevision::new(self.revision),
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
            OrganizationSnapshot::new(ServerEpoch::new(1), state, runtime).unwrap()
        }

        /// The original placement of a Pane created in the current publication.
        pub(crate) fn accepted(
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
                ServerEpoch::new(1),
                OrganizationRevision::new(self.revision - 1),
                OrganizationRevision::new(self.revision),
                vec![OrganizationWrite::PutPane(record.clone())],
                Vec::new(),
            )
            .unwrap();
            AcceptedPaneCreation::new(&delta, record.tab, id, record.creation_ordinal, original)
                .unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use spaceterm_client::view::TabStep;

    use super::ServerTabCollection;
    use super::testing::{A, Org, Replica, offline, tab, worktree};
    use crate::domain::tab::server_view::ServerViewError;

    fn collection(replica: &Replica) -> ServerTabCollection<&'static str> {
        ServerTabCollection::new(
            &replica.workspace_record(1),
            &replica.tab_records(),
            replica.view(1),
        )
        .unwrap()
    }

    fn apply(
        collection: &mut ServerTabCollection<&'static str>,
        replica: &Replica,
    ) -> Vec<(spaceterm_core::identity::TabId, &'static str)> {
        collection
            .apply(
                &replica.workspace_record(1),
                &replica.tab_records(),
                replica.view(1),
            )
            .unwrap()
    }

    #[test]
    fn desktop_server_collection_follows_personal_order_and_recorded_roots() {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1)
            .add_tab(1, 2, 2, None)
            .add_tab(1, 3, 3, None);
        let mut replica = Replica::attached(org, 1);
        let workspace = replica.workspace_id(1);
        let snapshot = replica.snapshot();
        replica
            .personal
            .move_tab(workspace, tab(A, 3), 0, &snapshot)
            .unwrap();
        let mut tabs = collection(&replica);
        assert_eq!(tabs.order(), &[tab(A, 3), tab(A, 1), tab(A, 2)]);
        assert_eq!(tabs.root_tab(), tab(A, 1));
        assert_eq!(tabs.missing(), tabs.order().to_vec());
        for (local, name) in [(1, "one"), (2, "two"), (3, "three")] {
            assert_eq!(tabs.insert(tab(A, local), name), Ok(None));
        }

        // The server deletes the Root Tab and records Tab 2 as the Root. Personal order and the
        // remaining payloads survive; only the deleted Tab's payload is released.
        replica.org.delete_tab(1).set_root_tab(1, 2).publish();
        replica.reconcile_ready(&[]);
        let released = apply(&mut tabs, &replica);
        assert_eq!(released, vec![(tab(A, 1), "one")]);
        assert_eq!(tabs.order(), &[tab(A, 3), tab(A, 2)]);
        assert_eq!(tabs.root_tab(), tab(A, 2));
        assert_eq!(
            tabs.iter().collect::<Vec<_>>(),
            vec![(tab(A, 3), &"three"), (tab(A, 2), &"two")]
        );
        assert_eq!(
            tabs.insert(tab(A, 1), "again"),
            Err(ServerViewError::NotPresented)
        );
    }

    #[test]
    fn desktop_server_collection_filters_the_strip_by_the_active_worktree() {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1)
            .add_tab(1, 2, 2, Some(4))
            .add_tab(1, 3, 3, Some(4))
            .add_tab(1, 5, 5, None);
        let mut replica = Replica::attached(org, 1);
        let workspace = replica.workspace_id(1);
        let tabs = collection(&replica);
        assert_eq!(tabs.active_scope(), None);
        assert_eq!(tabs.visible(), vec![tab(A, 1), tab(A, 5)]);
        assert_eq!(
            tabs.tabs_in(Some(worktree(A, 4))),
            vec![tab(A, 2), tab(A, 3)]
        );

        let snapshot = replica.snapshot();
        replica
            .personal
            .activate_worktree(workspace, Some(worktree(A, 4)), &snapshot)
            .unwrap();
        replica
            .personal
            .activate_adjacent_tab(workspace, TabStep::Next, &snapshot)
            .unwrap();
        let tabs = collection(&replica);
        assert_eq!(tabs.active_scope(), Some(worktree(A, 4)));
        assert_eq!(tabs.visible(), vec![tab(A, 2), tab(A, 3)]);
        assert_eq!(tabs.scope_of(tab(A, 5)), Ok(None));
    }

    #[test]
    fn desktop_server_collection_keeps_payloads_while_the_replica_is_offline() {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1).add_tab(1, 2, 2, None);
        let mut replica = Replica::attached(org, 1);
        let mut tabs = collection(&replica);
        tabs.insert(tab(A, 1), "one").unwrap();
        tabs.insert(tab(A, 2), "two").unwrap();
        // A retained replica never deletes: the Tab stays even though its record is gone here.
        replica.org.delete_tab(2).publish();
        replica.reconcile(&[], offline(A));
        let mut records = replica.tab_records();
        records.retain(|record| record.id != tab(A, 2));
        let released = tabs
            .apply(&replica.workspace_record(1), &records, replica.view(1))
            .unwrap();
        assert!(released.is_empty());
        assert_eq!(tabs.order(), &[tab(A, 1)]);
        assert_eq!(tabs.payloads().count(), 2);
    }
}
