//! Bridge from a client-core replica to the facts the view reconciles against.
//!
//! The view never reads the connection worker's concrete type. Client-core maps
//! its connection state into [`ReplicaFacts`] and lends the validated snapshot and
//! the accepted creation placements of the exact publication that produced it.
//! Only a Ready observation can attach a Workspace or change shared structure in
//! the view; Retained and Synchronizing observations permit navigation only.

use std::collections::{BTreeMap, BTreeSet};

use spaceterm_core::authority::{
    ConnectionBinding, CreationOrdinal, OrganizationRevision, ServerEpoch,
};
use spaceterm_core::identity::{ClientId, PairingId, PaneId, ServerId, TabId, WorkspaceId};
use spaceterm_core::organization::{
    AcceptedPaneCreation, MAX_ACCEPTED_PANE_CREATIONS, OrganizationSnapshot, PaneCreation,
    PaneRuntimeRecord, TabRecord, WorkspaceRecord,
};

use super::arrangement::{CreationLayout, PanePlacement};
use super::types::{MAX_TAB_PANES, ViewError, ViewResource};

/// How far a replica has been reconciled for the current connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReplicaReadiness {
    /// A retained replica: navigation and personal changes only.
    Retained,
    /// Connected and reconciling: no acquisition, startup, input or native effects.
    Synchronizing,
    /// Authenticated pairing, membership, Session generations and control are
    /// reconciled for the current connection. A connected socket alone is not Ready.
    Ready,
}

/// Connection status of one Server as the view presents it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ServerViewStatus {
    Connecting,
    Connected,
    Offline,
    Incompatible,
    Stopped,
}

/// Client-core's facts about the replica it lends to the view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplicaFacts {
    /// The current pairing of this client with the replica's Server.
    pub pairing: PairingId,
    /// This client's stable Client identity on that Server.
    pub client: ClientId,
    /// The authenticated connection binding, when one exists.
    pub connection: Option<ConnectionBinding>,
    pub readiness: ReplicaReadiness,
    pub status: ServerViewStatus,
}

/// A validated, same-Server observation of one replica.
///
/// Construction checks that the pairing, Client and connection belong to the
/// snapshot's Server, that a Ready replica is connected and carries retained
/// creation defaults, and that every accepted creation placement belongs to the
/// exact publication of the snapshot. A stale delta's placement can never be
/// applied to an unrelated snapshot.
#[derive(Clone, Debug)]
pub struct ReplicaObservation<'a> {
    organization: &'a OrganizationSnapshot,
    server: ServerId,
    pairing: PairingId,
    client: ClientId,
    epoch: ServerEpoch,
    revision: OrganizationRevision,
    connection: Option<ConnectionBinding>,
    readiness: ReplicaReadiness,
    status: ServerViewStatus,
    accepted_creations: &'a [AcceptedPaneCreation],
    index: ReplicaIndex,
}

/// Positions into the snapshot's record vectors, so reconciliation, explicit
/// uses and presentation never scan every record of the Server for each
/// Workspace, Tab or Pane.
#[derive(Clone, Debug, Default)]
struct ReplicaIndex {
    workspaces: BTreeMap<WorkspaceId, usize>,
    tabs: BTreeMap<TabId, usize>,
    panes: BTreeMap<PaneId, usize>,
    /// Each Pane's runtime record. The snapshot holds exactly one per Pane.
    runtime: BTreeMap<PaneId, usize>,
    /// Each Workspace's Tabs ordered by creation ordinal, then identity.
    workspace_tabs: BTreeMap<WorkspaceId, Vec<usize>>,
    /// Each Tab's Panes in record order.
    tab_panes: BTreeMap<TabId, Vec<usize>>,
}

impl<'a> ReplicaObservation<'a> {
    pub fn new(
        organization: &'a OrganizationSnapshot,
        accepted_creations: &'a [AcceptedPaneCreation],
        facts: ReplicaFacts,
    ) -> Result<Self, ViewError> {
        let state = organization.state();
        let server = state.server;
        if facts.pairing.server() != server || facts.client.server() != server {
            return Err(ViewError::WrongServer);
        }
        if let Some(connection) = facts.connection {
            if connection.client.server() != server {
                return Err(ViewError::WrongServer);
            }
            // A binding for another stable Client is not this client's connection.
            if connection.client != facts.client {
                return Err(ViewError::WrongMembership);
            }
        }
        if facts.readiness == ReplicaReadiness::Ready {
            if facts.status != ServerViewStatus::Connected || facts.connection.is_none() {
                return Err(ViewError::NotReady);
            }
            // Wave 2 readiness requires the normalized creation defaults.
            if state.retained.is_none() {
                return Err(ViewError::InvalidCreationTopology);
            }
        }
        if accepted_creations.len() > MAX_ACCEPTED_PANE_CREATIONS {
            return Err(ViewError::InvalidCreationTopology);
        }
        let epoch = organization.epoch();
        let revision = state.revision;
        let mut accepted = BTreeSet::new();
        for creation in accepted_creations {
            if creation.server() != server {
                return Err(ViewError::WrongServer);
            }
            if creation.epoch() != epoch || creation.revision() != revision {
                return Err(ViewError::StaleObservation);
            }
            if !accepted.insert(creation.pane()) {
                return Err(ViewError::InvalidCreationTopology);
            }
        }
        Ok(Self {
            organization,
            server,
            pairing: facts.pairing,
            client: facts.client,
            epoch,
            revision,
            connection: facts.connection,
            readiness: facts.readiness,
            status: facts.status,
            accepted_creations,
            index: ReplicaIndex::new(organization),
        })
    }

    pub fn organization(&self) -> &'a OrganizationSnapshot {
        self.organization
    }

    pub const fn readiness(&self) -> ReplicaReadiness {
        self.readiness
    }

    pub const fn server(&self) -> ServerId {
        self.server
    }

    pub const fn pairing(&self) -> PairingId {
        self.pairing
    }

    pub const fn client(&self) -> ClientId {
        self.client
    }

    pub const fn epoch(&self) -> ServerEpoch {
        self.epoch
    }

    pub const fn revision(&self) -> OrganizationRevision {
        self.revision
    }

    pub const fn connection(&self) -> Option<ConnectionBinding> {
        self.connection
    }

    pub const fn status(&self) -> ServerViewStatus {
        self.status
    }

    pub fn accepted_creations(&self) -> &'a [AcceptedPaneCreation] {
        self.accepted_creations
    }

    /// The Tab's normalized surviving creation tree, validated against its
    /// exact current Pane membership.
    pub fn creation_layout(&self, tab: TabId) -> Result<CreationLayout, ViewError> {
        if tab.server() != self.server {
            return Err(ViewError::WrongServer);
        }
        let retained = self
            .organization
            .state()
            .retained
            .as_ref()
            .ok_or(ViewError::InvalidCreationTopology)?;
        let defaults = retained.creation_defaults();
        let default = defaults
            .binary_search_by_key(&tab, |default| default.tab())
            .map(|index| &defaults[index])
            .map_err(|_| ViewError::TargetMissing)?;
        let panes = &self.organization.state().panes;
        let members = self
            .index
            .tab_panes
            .get(&tab)
            .map_or(&[][..], Vec::as_slice);
        if members.len() > MAX_TAB_PANES {
            return Err(ViewError::ResourceLimit {
                resource: ViewResource::TabPanes,
            });
        }
        let links: BTreeMap<PaneId, PaneCreation> = members
            .iter()
            .map(|&index| (panes[index].id, panes[index].creation))
            .collect();
        CreationLayout::from_tab_links(default, &links)
    }

    /// Accepted placements for new Panes of one Tab in this exact publication.
    pub fn placements(&self, tab: TabId) -> Vec<PanePlacement> {
        self.accepted_creations
            .iter()
            .filter(|creation| creation.tab() == tab)
            .map(|creation| PanePlacement::from_accepted(*creation))
            .collect()
    }

    pub(crate) fn workspace_record(&self, workspace: WorkspaceId) -> Option<&'a WorkspaceRecord> {
        let index = *self.index.workspaces.get(&workspace)?;
        self.organization.state().workspaces.get(index)
    }

    /// The Workspace's Tab records by creation ordinal, then identity.
    pub(crate) fn workspace_tabs(
        &self,
        workspace: WorkspaceId,
    ) -> impl Iterator<Item = &'a TabRecord> + '_ {
        let tabs = &self.organization.state().tabs;
        self.index
            .workspace_tabs
            .get(&workspace)
            .into_iter()
            .flatten()
            .map(move |&index| &tabs[index])
    }

    pub(crate) fn tab_record(&self, tab: TabId) -> Option<&'a TabRecord> {
        let index = *self.index.tabs.get(&tab)?;
        self.organization.state().tabs.get(index)
    }

    pub(crate) fn tab_pane_count(&self, tab: TabId) -> usize {
        self.index.tab_panes.get(&tab).map_or(0, Vec::len)
    }

    /// The Tab that currently contains a Pane.
    pub(crate) fn pane_tab(&self, pane: PaneId) -> Option<TabId> {
        let index = *self.index.panes.get(&pane)?;
        self.organization
            .state()
            .panes
            .get(index)
            .map(|record| record.tab)
    }

    pub(crate) fn pane_ordinal(&self, pane: PaneId) -> Option<CreationOrdinal> {
        let index = *self.index.panes.get(&pane)?;
        self.organization
            .state()
            .panes
            .get(index)
            .map(|record| record.creation_ordinal)
    }

    /// The runtime record of a Pane.
    pub(crate) fn pane_runtime(&self, pane: PaneId) -> Option<&'a PaneRuntimeRecord> {
        let index = *self.index.runtime.get(&pane)?;
        self.organization.runtime().get(index)
    }
}

impl ReplicaIndex {
    fn new(organization: &OrganizationSnapshot) -> Self {
        let state = organization.state();
        let mut index = Self::default();
        for (position, record) in state.workspaces.iter().enumerate() {
            index.workspaces.insert(record.id, position);
        }
        for (position, record) in state.tabs.iter().enumerate() {
            index.tabs.insert(record.id, position);
            index
                .workspace_tabs
                .entry(record.workspace)
                .or_insert_with(Vec::new)
                .push(position);
        }
        for tabs in index.workspace_tabs.values_mut() {
            tabs.sort_by_key(|&position| {
                let record = &state.tabs[position];
                (record.creation_ordinal, record.id)
            });
        }
        for (position, record) in state.panes.iter().enumerate() {
            index.panes.insert(record.id, position);
            index
                .tab_panes
                .entry(record.tab)
                .or_insert_with(Vec::new)
                .push(position);
        }
        for (position, record) in organization.runtime().iter().enumerate() {
            index.runtime.insert(record.pane, position);
        }
        index
    }
}
