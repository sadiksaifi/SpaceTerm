//! Read-only payloads for this client's attached, server-qualified Workspaces.
//!
//! The portable view owns order and activation. Records own names, pins and Roots. Applying one
//! server's publication never changes another server's inventory or manufactures replacement work.

use std::collections::{BTreeMap, BTreeSet};

use spaceterm_client::view::{ClientViewState, ViewRevision};
use spaceterm_core::{
    identity::{ServerId, WorkspaceId},
    organization::{OrganizationSnapshot, WorkspaceRecord},
};

#[derive(Default)]
pub(crate) struct ServerWorkspaceCollection<T> {
    records: BTreeMap<WorkspaceId, WorkspaceRecord>,
    payloads: BTreeMap<WorkspaceId, T>,
    order: Vec<WorkspaceId>,
    active: Option<WorkspaceId>,
    personal_revision: Option<ViewRevision>,
}

impl<T> ServerWorkspaceCollection<T> {
    pub(crate) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            payloads: BTreeMap::new(),
            order: Vec::new(),
            active: None,
            personal_revision: None,
        }
    }

    /// Updates only this snapshot's server records. The newest coherent personal view supplies
    /// global attachment order and activation, even when its active server has not arrived yet.
    pub(crate) fn apply(
        &mut self,
        organization: &OrganizationSnapshot,
        personal: &ClientViewState,
    ) -> Vec<T> {
        let server = organization.state().server;
        if self
            .personal_revision
            .is_none_or(|revision| personal.revision() >= revision)
        {
            self.order = personal
                .attachments()
                .iter()
                .map(|attachment| attachment.workspace())
                .collect();
            self.active = personal.active_workspace();
            self.personal_revision = Some(personal.revision());
        }
        let attached: BTreeSet<_> = self.order.iter().copied().collect();
        self.records
            .retain(|workspace, _| workspace.server() != server && attached.contains(workspace));
        for record in &organization.state().workspaces {
            if attached.contains(&record.id) {
                self.records.insert(record.id, record.clone());
            }
        }
        let removed: Vec<_> = self
            .payloads
            .keys()
            .copied()
            .filter(|workspace| !self.records.contains_key(workspace))
            .collect();
        removed
            .into_iter()
            .filter_map(|workspace| self.payloads.remove(&workspace))
            .collect()
    }

    /// A removed pairing is local authority loss, not server deletion.
    pub(crate) fn retain_servers(&mut self, servers: &BTreeSet<ServerId>) -> Vec<T> {
        self.records
            .retain(|workspace, _| servers.contains(&workspace.server()));
        self.order
            .retain(|workspace| servers.contains(&workspace.server()));
        self.active = self
            .active
            .filter(|workspace| servers.contains(&workspace.server()));
        let removed: Vec<_> = self
            .payloads
            .keys()
            .copied()
            .filter(|workspace| !servers.contains(&workspace.server()))
            .collect();
        removed
            .into_iter()
            .filter_map(|workspace| self.payloads.remove(&workspace))
            .collect()
    }

    pub(crate) fn has_attachments(&self) -> bool {
        !self.order.is_empty()
    }

    pub(crate) fn active(&self) -> Option<WorkspaceId> {
        self.active
            .filter(|workspace| self.records.contains_key(workspace))
    }
    pub(crate) fn order(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.order
            .iter()
            .copied()
            .filter(|workspace| self.records.contains_key(workspace))
    }
    pub(crate) fn record(&self, workspace: WorkspaceId) -> Option<&WorkspaceRecord> {
        self.records.get(&workspace)
    }
    pub(crate) fn payload(&self, workspace: WorkspaceId) -> Option<&T> {
        self.payloads.get(&workspace)
    }
    pub(crate) fn insert(&mut self, workspace: WorkspaceId, payload: T) -> Result<(), T> {
        if !self.records.contains_key(&workspace) || self.payloads.contains_key(&workspace) {
            return Err(payload);
        }
        self.payloads.insert(workspace, payload);
        Ok(())
    }
    pub(crate) fn payloads(&self) -> impl Iterator<Item = &T> {
        self.payloads.values()
    }
}
