//! Workspace Attachments and the personal view of each attached Workspace.
//!
//! The view reconciles against validated replicas. It never creates shared
//! organization, acquires control or starts a Terminal Session: every change it
//! reports is personal or follows authoritative membership, and every
//! activation it performs is navigation or restoration, never explicit use.

use std::collections::{BTreeMap, BTreeSet};

use spaceterm_core::authority::{OrganizationRevision, ServerEpoch};
use spaceterm_core::identity::{
    ClientId, PairingId, PaneId, ServerId, TabId, WorkspaceId, WorktreeId,
};
use spaceterm_core::organization::{OrganizationSnapshot, SplitAxis, TabRecord, WorkspaceRecord};

use super::arrangement::{
    ArrangementChange, ArrangementMetrics, FocusDirection, PaneArrangement, PaneEdge, PaneSize,
    TabOrder, TabStep, ZoomState,
};
use super::persistence::{PersistedView, PersistedViewParts, PersistedWorkspace};
use super::replica_bridge::{ReplicaObservation, ReplicaReadiness};
use super::types::{
    AttachmentIncarnation, LayoutRevision, MAX_TAB_PANES, MAX_VIEW_ATTACHMENTS, MAX_VIEW_PANES,
    MAX_VIEW_TABS, SplitId, ViewCounter, ViewError, ViewResource, ViewRevision,
};

/// One local attachment of a Workspace under an exact pairing and stable Client.
/// The incarnation distinguishes a later reattachment from an earlier one, so a
/// detach or revocation task bound to the old attachment cannot remove it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceAttachment {
    workspace: WorkspaceId,
    pairing: PairingId,
    client: ClientId,
    incarnation: AttachmentIncarnation,
}

impl WorkspaceAttachment {
    pub fn new(
        workspace: WorkspaceId,
        pairing: PairingId,
        client: ClientId,
        incarnation: AttachmentIncarnation,
    ) -> Result<Self, ViewError> {
        if pairing.server() != workspace.server() || client.server() != workspace.server() {
            return Err(ViewError::WrongServer);
        }
        Ok(Self {
            workspace,
            pairing,
            client,
            incarnation,
        })
    }

    pub const fn server(self) -> ServerId {
        self.workspace.server()
    }

    pub const fn workspace(self) -> WorkspaceId {
        self.workspace
    }

    pub const fn pairing(self) -> PairingId {
        self.pairing
    }

    pub const fn client(self) -> ClientId {
        self.client
    }

    pub const fn incarnation(self) -> AttachmentIncarnation {
        self.incarnation
    }

    fn binds(self, replica: &ReplicaObservation<'_>) -> bool {
        self.server() == replica.server()
            && self.pairing == replica.pairing()
            && self.client == replica.client()
    }
}

/// Whether a Workspace's personal view represents its complete membership.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ViewAvailability {
    Available,
    /// Authoritative membership exceeds a client representation limit. The
    /// view keeps a pruned last accepted arrangement, presents nothing as
    /// complete and never truncates server membership.
    ResourceLimited {
        resource: ViewResource,
    },
}

/// Why a personal activation, focus or Active Tab changed. Neither cause is
/// explicit use: only explicit-use intents can request control or startup.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActivationCause {
    /// A deliberate personal navigation, including attaching.
    Navigation,
    /// A fallback after removal, or initial activation of new structure.
    Restoration,
}

/// Why an attachment left the view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AttachmentRemoval {
    Detached,
    PairingRemoved,
    WorkspaceDeleted,
    /// A durable detach or revocation marker matched a restored attachment.
    Suppressed,
}

/// Why projected content and native context of one Pane must be cleared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InvalidationReason {
    TargetDeleted,
    Detached,
    PairingRemoved,
    Suppressed,
    ResourceLimited,
}

/// Content and native-context invalidation for one represented Pane. It carries
/// identifiers and a typed reason only, never terminal content.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProjectionInvalidation {
    pub workspace: WorkspaceId,
    pub tab: TabId,
    pub pane: PaneId,
    pub reason: InvalidationReason,
}

/// One personal view change. Attached means the whole Workspace view is new.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewChange {
    Attached {
        attachment: WorkspaceAttachment,
    },
    AttachmentRemoved {
        attachment: WorkspaceAttachment,
        reason: AttachmentRemoval,
    },
    WorkspaceActivated {
        workspace: Option<WorkspaceId>,
        cause: ActivationCause,
    },
    AvailabilityChanged {
        workspace: WorkspaceId,
        availability: ViewAvailability,
    },
    TabAdded {
        workspace: WorkspaceId,
        tab: TabId,
    },
    TabRemoved {
        workspace: WorkspaceId,
        tab: TabId,
    },
    TabsReordered {
        workspace: WorkspaceId,
    },
    TabActivated {
        workspace: WorkspaceId,
        tab: Option<TabId>,
        cause: ActivationCause,
    },
    PanesAdded {
        workspace: WorkspaceId,
        tab: TabId,
        panes: Vec<PaneId>,
    },
    PanesRemoved {
        workspace: WorkspaceId,
        tab: TabId,
        panes: Vec<PaneId>,
    },
    PaneFocused {
        workspace: WorkspaceId,
        tab: TabId,
        pane: PaneId,
        cause: ActivationCause,
    },
    ZoomChanged {
        workspace: WorkspaceId,
        tab: TabId,
        zoom: ZoomState,
    },
    LayoutChanged {
        workspace: WorkspaceId,
        tab: TabId,
        layout: LayoutRevision,
    },
}

/// Result of one view operation. An unchanged view keeps its revision and
/// reports no changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewUpdate {
    pub revision: ViewRevision,
    pub changes: Vec<ViewChange>,
    pub invalidations: Vec<ProjectionInvalidation>,
}

impl ViewUpdate {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.invalidations.is_empty()
    }
}

/// One personal edit of a Pane arrangement. Structural insertion and removal
/// follow the replica only, so no edit creates or closes a shared Pane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrangementEdit {
    Focus(PaneId),
    FocusDirection(FocusDirection),
    FocusPrevious,
    FocusNext,
    ToggleZoom,
    Move {
        pane: PaneId,
        target: PaneId,
        edge: PaneEdge,
        target_size: PaneSize,
        metrics: ArrangementMetrics,
    },
    SetSplitAxis {
        split: SplitId,
        axis: SplitAxis,
        available: PaneSize,
        metrics: ArrangementMetrics,
    },
    Resize {
        split: SplitId,
        available: PaneSize,
        metrics: ArrangementMetrics,
        ratio: f32,
    },
}

/// A detach bound to the exact attachment incarnation it was prepared against
/// and to the view revision it was prepared at. Client-core durably records its
/// suppression marker before applying it; unrelated view changes made while the
/// marker is written do not invalidate it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DetachPlan {
    revision: ViewRevision,
    attachment: WorkspaceAttachment,
}

impl DetachPlan {
    pub const fn revision(self) -> ViewRevision {
        self.revision
    }

    pub const fn attachment(self) -> WorkspaceAttachment {
        self.attachment
    }
}

/// Exact attachment bindings that must not be restored, such as durable detach
/// markers and pending self-unpair barriers held by client-core.
pub trait AttachmentSuppression {
    fn suppresses(&self, attachment: &WorkspaceAttachment) -> bool;

    /// The highest attachment incarnation any durable detach marker has named,
    /// or None before any. Client-core keeps it durable and monotonic, even
    /// after the markers clear. A restored view never issues an incarnation at
    /// or below it, so a marker or plan for a detached attachment can never
    /// match a later one, even when the retained view predates the detach.
    fn incarnation_floor(&self) -> Option<AttachmentIncarnation>;
}

/// Suppresses exact attachments and every attachment of exact pairings, and
/// carries an incarnation floor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestorationFilter {
    attachments: BTreeSet<WorkspaceAttachment>,
    pairings: BTreeSet<PairingId>,
    floor: Option<AttachmentIncarnation>,
}

impl RestorationFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Suppresses one attachment incarnation and raises the incarnation floor
    /// to it. A later incarnation of the same Workspace is not suppressed.
    pub fn suppress_attachment(&mut self, attachment: WorkspaceAttachment) {
        self.raise_incarnation_floor(attachment.incarnation());
        self.attachments.insert(attachment);
    }

    /// Raises the incarnation floor, such as to a value client-core retained
    /// after the markers that named it cleared. A lower value changes nothing.
    pub fn raise_incarnation_floor(&mut self, incarnation: AttachmentIncarnation) {
        self.floor = self.floor.max(Some(incarnation));
    }

    /// Suppresses every attachment of one pairing. Another pairing with the
    /// same Server is not suppressed.
    pub fn suppress_pairing(&mut self, pairing: PairingId) {
        self.pairings.insert(pairing);
    }
}

impl AttachmentSuppression for RestorationFilter {
    fn suppresses(&self, attachment: &WorkspaceAttachment) -> bool {
        self.attachments.contains(attachment) || self.pairings.contains(&attachment.pairing())
    }

    fn incarnation_floor(&self) -> Option<AttachmentIncarnation> {
        self.floor
    }
}

/// Personal view of one attached Workspace: Tab order, activation history and
/// one arrangement per represented Tab. Worktree membership is read from the
/// replica, never retained.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceView {
    attachment: WorkspaceAttachment,
    availability: ViewAvailability,
    tabs: TabOrder,
    /// Exactly one arrangement for each Tab in `tabs`.
    arrangements: BTreeMap<TabId, PaneArrangement>,
    /// Each represented Tab's Worktree in the newest replica that contained
    /// it: the last Ready replica, or a Retained or Synchronizing one not older
    /// than the newest reconciled publication. Memory only: it chooses the
    /// Active Tab fallback after the Tab's record is gone and is never
    /// retained.
    scopes: BTreeMap<TabId, Option<WorktreeId>>,
}

impl WorkspaceView {
    pub const fn attachment(&self) -> WorkspaceAttachment {
        self.attachment
    }

    pub const fn workspace(&self) -> WorkspaceId {
        self.attachment.workspace
    }

    pub const fn availability(&self) -> ViewAvailability {
        self.availability
    }

    pub fn tab_order(&self) -> &[TabId] {
        self.tabs.order()
    }

    pub const fn active_tab(&self) -> Option<TabId> {
        self.tabs.active()
    }

    pub fn activation_history(&self) -> &[TabId] {
        self.tabs.activation_history()
    }

    pub fn arrangement(&self, tab: TabId) -> Option<&PaneArrangement> {
        self.arrangements.get(&tab)
    }

    /// One Worktree's represented Tabs in personal order, by the snapshot's
    /// membership. A snapshot of another Server has none.
    pub fn tabs_in_worktree(
        &self,
        organization: &OrganizationSnapshot,
        worktree: Option<WorktreeId>,
    ) -> Vec<TabId> {
        if organization.state().server != self.workspace().server() {
            return Vec::new();
        }
        self.tabs.tabs_in(&organization.state().tabs, worktree)
    }

    /// The Active Tab's Worktree in the snapshot. None also means the
    /// Workspace's own scope or an Active Tab the snapshot does not contain.
    pub fn active_worktree(&self, organization: &OrganizationSnapshot) -> Option<WorktreeId> {
        let active = self.tabs.active()?;
        organization
            .state()
            .tabs
            .iter()
            .find(|record| record.id == active && record.workspace == self.workspace())?
            .worktree
    }

    fn represented(&self) -> Counts {
        Counts {
            tabs: self.tabs.order().len(),
            panes: self
                .arrangements
                .values()
                .map(PaneArrangement::pane_count)
                .sum(),
        }
    }

    /// What would stay represented after pruning to the replica's membership.
    fn pruned(&self, replica: &ReplicaObservation<'_>) -> Counts {
        let mut counts = Counts::default();
        for (tab, arrangement) in &self.arrangements {
            if !replica
                .tab_record(*tab)
                .is_some_and(|record| record.workspace == self.workspace())
            {
                continue;
            }
            let panes = arrangement
                .pane_ids()
                .into_iter()
                .filter(|pane| replica.pane_tab(*pane) == Some(*tab))
                .count();
            if panes > 0 {
                counts.tabs += 1;
                counts.panes += panes;
            }
        }
        counts
    }

    fn old_active_scope(&self) -> Option<Option<WorktreeId>> {
        self.tabs
            .active()
            .and_then(|tab| self.scopes.get(&tab).copied())
    }

    fn record_scopes(&mut self, replica: &ReplicaObservation<'_>) {
        let workspace = self.workspace();
        self.scopes = replica
            .workspace_tabs(workspace)
            .filter(|record| self.arrangements.contains_key(&record.id))
            .map(|record| (record.id, record.worktree))
            .collect();
    }

    /// Notes the Worktree of each represented Tab an incomplete replica still
    /// contains. A missing Tab keeps its last known Worktree: missing inventory
    /// is not a fact.
    fn note_scopes(&mut self, replica: &ReplicaObservation<'_>) {
        let workspace = self.workspace();
        for record in replica.workspace_tabs(workspace) {
            if self.arrangements.contains_key(&record.id) {
                self.scopes.insert(record.id, record.worktree);
            }
        }
    }

    /// Builds the default view: Tabs in creation order, each arrangement cloned
    /// from its normalized creation tree, the Root Tab active and each Root Pane
    /// focused. This initialization is restoration, not explicit use.
    fn from_replica(
        attachment: WorkspaceAttachment,
        record: &WorkspaceRecord,
        replica: &ReplicaObservation<'_>,
    ) -> Result<Self, ViewError> {
        let workspace = attachment.workspace;
        let tabs = TabOrder::from_creation(
            workspace,
            &replica.organization().state().tabs,
            record.root_tab,
        )?;
        let mut arrangements = BTreeMap::new();
        for tab in replica.workspace_tabs(workspace) {
            arrangements.insert(tab.id, default_arrangement(tab, replica)?);
        }
        let mut view = Self {
            attachment,
            availability: ViewAvailability::Available,
            tabs,
            arrangements,
            scopes: BTreeMap::new(),
        };
        view.record_scopes(replica);
        Ok(view)
    }

    /// Applies complete authoritative membership that fits the view's limits.
    fn reconcile_available(
        &mut self,
        record: &WorkspaceRecord,
        replica: &ReplicaObservation<'_>,
        batch: &mut Batch,
    ) -> Result<(), ViewError> {
        let workspace = self.workspace();
        if self.availability != ViewAvailability::Available {
            self.availability = ViewAvailability::Available;
            batch.change(ViewChange::AvailabilityChanged {
                workspace,
                availability: self.availability,
            });
        }
        let records = &replica.organization().state().tabs;
        let old_scope = self.old_active_scope();
        let retained = self.tabs.retain(records, old_scope);
        for tab in retained.removed {
            self.remove_tab(tab, batch);
        }
        if self.tabs.order().is_empty() {
            // Every represented Tab is gone: start again from the defaults.
            self.tabs = TabOrder::from_creation(workspace, records, record.root_tab)?;
            self.arrangements.clear();
            for tab in replica.workspace_tabs(workspace) {
                self.arrangements
                    .insert(tab.id, default_arrangement(tab, replica)?);
                batch.change(ViewChange::TabAdded {
                    workspace,
                    tab: tab.id,
                });
            }
            batch.change(ViewChange::TabActivated {
                workspace,
                tab: self.tabs.active(),
                cause: ActivationCause::Restoration,
            });
            self.record_scopes(replica);
            return Ok(());
        }
        if retained.active_changed {
            batch.change(ViewChange::TabActivated {
                workspace,
                tab: self.tabs.active(),
                cause: ActivationCause::Restoration,
            });
        }
        let surviving: Vec<TabId> = self.tabs.order().to_vec();
        for tab in surviving {
            let layout = replica.creation_layout(tab)?;
            let placements = replica.placements(tab);
            let arrangement = self
                .arrangements
                .get_mut(&tab)
                .ok_or(ViewError::InvalidPersistedState)?;
            let change =
                arrangement.reconcile(&layout, &placements, |pane| replica.pane_ordinal(pane))?;
            batch.arrangement(workspace, arrangement, change);
        }
        for tab in replica.workspace_tabs(workspace) {
            if self.tabs.append(tab.id)? {
                self.arrangements
                    .insert(tab.id, default_arrangement(tab, replica)?);
                batch.change(ViewChange::TabAdded {
                    workspace,
                    tab: tab.id,
                });
            }
        }
        self.record_scopes(replica);
        Ok(())
    }

    /// Prunes the last accepted arrangement to surviving membership without
    /// representing new Tabs or Panes.
    fn reconcile_limited(
        &mut self,
        resource: ViewResource,
        replica: &ReplicaObservation<'_>,
        batch: &mut Batch,
    ) -> Result<(), ViewError> {
        let workspace = self.workspace();
        let limited = ViewAvailability::ResourceLimited { resource };
        let became_limited = self.availability == ViewAvailability::Available;
        if self.availability != limited {
            self.availability = limited;
            batch.change(ViewChange::AvailabilityChanged {
                workspace,
                availability: limited,
            });
        }
        let mut emptied = BTreeSet::new();
        for (tab, arrangement) in &mut self.arrangements {
            let tab = *tab;
            if !replica
                .tab_record(tab)
                .is_some_and(|record| record.workspace == workspace)
            {
                continue;
            }
            match arrangement.prune(|pane| replica.pane_tab(pane) == Some(tab))? {
                Some(change) => batch.arrangement(workspace, arrangement, change),
                None => {
                    emptied.insert(tab);
                }
            }
        }
        let kept: Vec<TabRecord> = replica
            .workspace_tabs(workspace)
            .filter(|record| !emptied.contains(&record.id))
            .cloned()
            .collect();
        let old_scope = self.old_active_scope();
        let retained = self.tabs.retain(&kept, old_scope);
        for tab in retained.removed {
            self.remove_tab(tab, batch);
        }
        if retained.active_changed {
            batch.change(ViewChange::TabActivated {
                workspace,
                tab: self.tabs.active(),
                cause: ActivationCause::Restoration,
            });
        }
        if became_limited {
            for tab in self.tabs.order() {
                if let Some(arrangement) = self.arrangements.get(tab) {
                    batch.invalidate(
                        workspace,
                        *tab,
                        arrangement,
                        InvalidationReason::ResourceLimited,
                    );
                }
            }
        }
        self.record_scopes(replica);
        Ok(())
    }

    fn remove_tab(&mut self, tab: TabId, batch: &mut Batch) {
        let workspace = self.workspace();
        if let Some(arrangement) = self.arrangements.remove(&tab) {
            batch.invalidate(
                workspace,
                tab,
                &arrangement,
                InvalidationReason::TargetDeleted,
            );
        }
        self.scopes.remove(&tab);
        batch.change(ViewChange::TabRemoved { workspace, tab });
    }

    fn invalidate_all(&self, batch: &mut Batch, reason: InvalidationReason) {
        for tab in self.tabs.order() {
            if let Some(arrangement) = self.arrangements.get(tab) {
                batch.invalidate(self.workspace(), *tab, arrangement, reason);
            }
        }
    }

    fn export(&self) -> Result<PersistedWorkspace, ViewError> {
        let arrangements = self
            .tabs
            .order()
            .iter()
            .map(|tab| {
                self.arrangements
                    .get(tab)
                    .map(PaneArrangement::export)
                    .ok_or(ViewError::InvalidPersistedState)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PersistedWorkspace {
            attachment: self.attachment,
            availability: self.availability,
            tabs: self.tabs.export(),
            arrangements,
        })
    }

    fn import(workspace: &PersistedWorkspace) -> Result<Self, ViewError> {
        let attachment = workspace.attachment;
        let tabs = TabOrder::import(attachment.workspace(), &workspace.tabs)?;
        if workspace.arrangements.len() != tabs.order().len() {
            return Err(ViewError::InvalidPersistedState);
        }
        let mut arrangements = BTreeMap::new();
        for (tab, record) in tabs.order().iter().zip(&workspace.arrangements) {
            arrangements.insert(*tab, PaneArrangement::import(*tab, record)?);
        }
        Ok(Self {
            attachment,
            availability: workspace.availability,
            tabs,
            arrangements,
            scopes: BTreeMap::new(),
        })
    }
}

/// The client's attachments and personal views. Empty is valid: a new view
/// attaches nothing and creates no Workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientViewState {
    revision: ViewRevision,
    /// The last attachment incarnation issued, or zero before any.
    incarnation_high_water: u64,
    attachments: Vec<WorkspaceAttachment>,
    /// Parallel to `attachments`.
    views: Vec<WorkspaceView>,
    /// None exactly when there are no attachments.
    active: Option<WorkspaceId>,
    /// The newest Ready publication reconciled for each Server. Memory only.
    marks: BTreeMap<ServerId, (ServerEpoch, OrganizationRevision)>,
}

impl ClientViewState {
    /// The empty view. It applies no suppression facts, so a launch uses
    /// [`Self::restore`] even when no document exists.
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn revision(&self) -> ViewRevision {
        self.revision
    }

    /// Attachments in personal Workspace order.
    pub fn attachments(&self) -> &[WorkspaceAttachment] {
        &self.attachments
    }

    pub const fn active_workspace(&self) -> Option<WorkspaceId> {
        self.active
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceView> {
        self.position(id).map(|index| &self.views[index])
    }

    /// Workspace views in attachment order.
    pub fn workspaces(&self) -> &[WorkspaceView] {
        &self.views
    }

    /// Attaches an existing Workspace of a Ready replica with a new checked
    /// incarnation and activates it. Attaching an attached Workspace under the
    /// same pairing and Client only activates it. A Workspace attached under
    /// another pairing or Client must lose that attachment first.
    pub fn attach(
        &mut self,
        workspace: WorkspaceId,
        replica: &ReplicaObservation<'_>,
    ) -> Result<ViewUpdate, ViewError> {
        if workspace.server() != replica.server() {
            return Err(ViewError::WrongServer);
        }
        if replica.readiness() != ReplicaReadiness::Ready {
            return Err(ViewError::NotReady);
        }
        self.check_mark(replica)?;
        let record = replica
            .workspace_record(workspace)
            .ok_or(ViewError::TargetMissing)?;
        if let Some(view) = self.workspace(workspace) {
            if !view.attachment.binds(replica) {
                return Err(ViewError::WrongMembership);
            }
            let update = self.activate_workspace(workspace)?;
            self.raise_mark(replica);
            return Ok(update);
        }
        if self.attachments.len() >= MAX_VIEW_ATTACHMENTS {
            return Err(ViewError::ResourceLimit {
                resource: ViewResource::Attachments,
            });
        }
        let revision = self.next_revision()?;
        let incarnation = self
            .incarnation_high_water
            .checked_add(1)
            .and_then(AttachmentIncarnation::new)
            .ok_or(ViewCounter::AttachmentIncarnation.exhausted())?;
        let attachment =
            WorkspaceAttachment::new(workspace, replica.pairing(), replica.client(), incarnation)?;
        let full = authoritative(replica, workspace)
            .map_err(|resource| ViewError::ResourceLimit { resource })?;
        if let Some(resource) = self.represented().plus(full).exceeded() {
            return Err(ViewError::ResourceLimit { resource });
        }
        let view = WorkspaceView::from_replica(attachment, record, replica)?;
        self.incarnation_high_water = incarnation.get();
        self.attachments.push(attachment);
        self.views.push(view);
        self.active = Some(workspace);
        self.revision = revision;
        self.raise_mark(replica);
        Ok(ViewUpdate {
            revision,
            changes: vec![
                ViewChange::Attached { attachment },
                ViewChange::WorkspaceActivated {
                    workspace: Some(workspace),
                    cause: ActivationCause::Navigation,
                },
            ],
            invalidations: Vec::new(),
        })
    }

    /// Activates an attached Workspace. Activation works offline and emits no
    /// shared creation, control or startup.
    pub fn activate_workspace(&mut self, workspace: WorkspaceId) -> Result<ViewUpdate, ViewError> {
        if self.position(workspace).is_none() {
            return Err(ViewError::TargetMissing);
        }
        if self.active == Some(workspace) {
            return Ok(self.unchanged());
        }
        let revision = self.next_revision()?;
        self.active = Some(workspace);
        self.revision = revision;
        Ok(ViewUpdate {
            revision,
            changes: vec![ViewChange::WorkspaceActivated {
                workspace: Some(workspace),
                cause: ActivationCause::Navigation,
            }],
            invalidations: Vec::new(),
        })
    }

    /// Activates a represented Tab without changing the Active Workspace.
    pub fn activate_tab(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
    ) -> Result<ViewUpdate, ViewError> {
        self.edit_tabs(workspace, |tabs| {
            let changed = tabs.activate(tab)?;
            Ok(changed.then_some(TabEdit::Activated))
        })
    }

    /// Activates the Tab one step from the Active Tab within its Worktree.
    pub fn activate_adjacent_tab(
        &mut self,
        workspace: WorkspaceId,
        step: TabStep,
        organization: &OrganizationSnapshot,
    ) -> Result<ViewUpdate, ViewError> {
        let records = scoped_tabs(workspace, organization)?;
        self.edit_tabs(workspace, |tabs| {
            let tab = tabs
                .neighbor_of_active(step, records)
                .ok_or(ViewError::TargetMissing)?;
            Ok(tabs.activate(tab)?.then_some(TabEdit::Activated))
        })
    }

    /// Activates a Worktree's most recently activated represented Tab.
    pub fn activate_worktree(
        &mut self,
        workspace: WorkspaceId,
        worktree: Option<WorktreeId>,
        organization: &OrganizationSnapshot,
    ) -> Result<ViewUpdate, ViewError> {
        if worktree.is_some_and(|worktree| worktree.server() != workspace.server()) {
            return Err(ViewError::WrongServer);
        }
        let records = scoped_tabs(workspace, organization)?;
        self.edit_tabs(workspace, |tabs| {
            let tab = tabs
                .most_recent_in(records, worktree)
                .ok_or(ViewError::TargetMissing)?;
            Ok(tabs.activate(tab)?.then_some(TabEdit::Activated))
        })
    }

    /// Moves a Tab to `position` within its Worktree's personal sequence.
    pub fn move_tab(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        position: usize,
        organization: &OrganizationSnapshot,
    ) -> Result<ViewUpdate, ViewError> {
        let records = scoped_tabs(workspace, organization)?;
        self.edit_tabs(workspace, |tabs| {
            Ok(tabs
                .move_tab(tab, position, records)?
                .then_some(TabEdit::Reordered))
        })
    }

    /// Moves a Tab one step within its Worktree's personal sequence.
    pub fn step_tab(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        step: TabStep,
        organization: &OrganizationSnapshot,
    ) -> Result<ViewUpdate, ViewError> {
        let records = scoped_tabs(workspace, organization)?;
        self.edit_tabs(workspace, |tabs| {
            Ok(tabs
                .step_tab(tab, step, records)?
                .then_some(TabEdit::Reordered))
        })
    }

    /// Applies one personal arrangement edit. A failed edit leaves the view
    /// unchanged; an edit that changes nothing keeps the revision.
    pub fn arrange(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        edit: ArrangementEdit,
    ) -> Result<ViewUpdate, ViewError> {
        if tab.server() != workspace.server() {
            return Err(ViewError::WrongServer);
        }
        let index = self.position(workspace).ok_or(ViewError::TargetMissing)?;
        let current = self.views[index]
            .arrangements
            .get(&tab)
            .ok_or(ViewError::TargetMissing)?;
        let mut next = current.clone();
        match edit {
            ArrangementEdit::Focus(pane) => {
                next.focus_pane(pane)?;
            }
            ArrangementEdit::FocusDirection(direction) => {
                next.focus_in_direction(direction)?;
            }
            ArrangementEdit::FocusPrevious => {
                next.focus_previous();
            }
            ArrangementEdit::FocusNext => {
                next.focus_next();
            }
            ArrangementEdit::ToggleZoom => {
                next.toggle_zoom();
            }
            ArrangementEdit::Move {
                pane,
                target,
                edge,
                target_size,
                metrics,
            } => {
                next.move_pane(pane, target, edge, target_size, metrics)?;
            }
            ArrangementEdit::SetSplitAxis {
                split,
                axis,
                available,
                metrics,
            } => {
                next.set_split_axis(split, axis, available, metrics)?;
            }
            ArrangementEdit::Resize {
                split,
                available,
                metrics,
                ratio,
            } => {
                next.resize_split(split, available, metrics, ratio)?;
            }
        }
        if next == *current {
            return Ok(self.unchanged());
        }
        let revision = self.next_revision()?;
        let mut changes = Vec::new();
        if (next.focused_pane() != current.focused_pane()
            || next.focus_history() != current.focus_history())
            && let Some(pane) = next.focused_pane()
        {
            changes.push(ViewChange::PaneFocused {
                workspace,
                tab,
                pane,
                cause: ActivationCause::Navigation,
            });
        }
        if next.zoom() != current.zoom() {
            changes.push(ViewChange::ZoomChanged {
                workspace,
                tab,
                zoom: next.zoom(),
            });
        }
        if next.layout_revision() != current.layout_revision() {
            changes.push(ViewChange::LayoutChanged {
                workspace,
                tab,
                layout: next.layout_revision(),
            });
        }
        self.views[index].arrangements.insert(tab, next);
        self.revision = revision;
        Ok(ViewUpdate {
            revision,
            changes,
            invalidations: Vec::new(),
        })
    }

    /// Reconciles every attachment bound to the observation's exact Server,
    /// pairing and Client, including inactive Workspaces and Tabs.
    ///
    /// Retained and Synchronizing observations change no structure and no
    /// revision: missing inventory there is not deletion. One that is not older
    /// than the newest reconciled publication of its Server only notes the
    /// memory-only Worktree of each represented Tab it contains, so a restored
    /// view keeps its Active Tab fallback before the first Ready observation. A
    /// Ready observation older than the newest reconciled publication of its
    /// Server is rejected; an equal one is structurally idempotent. Attachments whose Workspaces are authoritatively
    /// absent are removed; new Workspaces are never attached. The whole update
    /// is staged and committed at once, so a failure changes nothing.
    pub fn reconcile(&mut self, replica: &ReplicaObservation<'_>) -> Result<ViewUpdate, ViewError> {
        if replica.readiness() != ReplicaReadiness::Ready {
            if self.check_mark(replica).is_ok() {
                for view in &mut self.views {
                    if view.attachment.binds(replica) {
                        view.note_scopes(replica);
                    }
                }
            }
            return Ok(self.unchanged());
        }
        self.check_mark(replica)?;
        let scope: Vec<usize> = (0..self.attachments.len())
            .filter(|index| self.attachments[*index].binds(replica))
            .collect();
        // Attachments outside this observation keep their represented size.
        // Every present Workspace in scope reserves at least its pruned size,
        // so admitting one Workspace's growth can never push a later one, or
        // the whole view, past a limit.
        let mut used = Counts::default();
        for (index, view) in self.views.iter().enumerate() {
            if !scope.contains(&index) {
                used = used.plus(view.represented());
            }
        }
        let pruned: Vec<Option<Counts>> = scope
            .iter()
            .map(|&index| {
                let view = &self.views[index];
                replica
                    .workspace_record(view.workspace())
                    .map(|_| view.pruned(replica))
            })
            .collect();
        let mut reserved = pruned
            .iter()
            .flatten()
            .fold(Counts::default(), |total, counts| total.plus(*counts));
        let mut batch = Batch::default();
        let mut staged: Vec<(usize, WorkspaceView)> = Vec::new();
        let mut removed: BTreeSet<WorkspaceId> = BTreeSet::new();
        for (&index, own) in scope.iter().zip(&pruned) {
            let view = &self.views[index];
            let workspace = view.workspace();
            let (Some(record), Some(own)) = (replica.workspace_record(workspace), own) else {
                batch.change(ViewChange::AttachmentRemoved {
                    attachment: view.attachment,
                    reason: AttachmentRemoval::WorkspaceDeleted,
                });
                view.invalidate_all(&mut batch, InvalidationReason::TargetDeleted);
                removed.insert(workspace);
                continue;
            };
            reserved = reserved.minus(*own);
            let mut next = view.clone();
            let fit = authoritative(replica, workspace).and_then(|full| {
                match used.plus(reserved).plus(full).exceeded() {
                    Some(resource) => Err(resource),
                    None => Ok(full),
                }
            });
            match fit {
                Ok(full) => {
                    next.reconcile_available(record, replica, &mut batch)?;
                    used = used.plus(full);
                }
                Err(resource) => {
                    next.reconcile_limited(resource, replica, &mut batch)?;
                    used = used.plus(next.represented());
                }
            }
            staged.push((index, next));
        }
        if batch.is_empty() {
            // Memory-only Worktree scopes may still have changed.
            for (index, view) in staged {
                self.views[index] = view;
            }
            self.raise_mark(replica);
            return Ok(self.unchanged());
        }
        let revision = self.next_revision()?;
        for (index, view) in staged {
            self.views[index] = view;
        }
        if let Some(change) = self.drop_attachments(&removed) {
            batch.change(change);
        }
        self.revision = revision;
        self.raise_mark(replica);
        Ok(batch.into_update(revision))
    }

    /// Prepares a detach without changing the view.
    pub fn prepare_detach(&self, workspace: WorkspaceId) -> Result<DetachPlan, ViewError> {
        let view = self.workspace(workspace).ok_or(ViewError::TargetMissing)?;
        Ok(DetachPlan {
            revision: self.revision,
            attachment: view.attachment,
        })
    }

    /// Applies a detach to the exact attachment incarnation it was prepared
    /// against. Unrelated changes after preparation, such as a reconcile,
    /// navigation or another attachment while client-core writes the marker,
    /// do not reject it. A plan prepared at a revision newer than this view,
    /// such as before a restore from an older document, is stale. Detach ends no
    /// Terminal Session; client-core releases only the grants it captured with
    /// its durable marker.
    pub fn apply_detach(&mut self, plan: DetachPlan) -> Result<ViewUpdate, ViewError> {
        if plan.revision > self.revision
            || self
                .workspace(plan.attachment.workspace())
                .is_none_or(|view| view.attachment != plan.attachment)
        {
            return Err(ViewError::StaleView);
        }
        self.remove_where(
            |attachment| *attachment == plan.attachment,
            AttachmentRemoval::Detached,
            InvalidationReason::Detached,
        )
    }

    /// Removes every attachment of one exact pairing. Attachments of another
    /// pairing with the same Server stay.
    pub fn remove_pairing(
        &mut self,
        server: ServerId,
        pairing: PairingId,
    ) -> Result<ViewUpdate, ViewError> {
        if pairing.server() != server {
            return Err(ViewError::WrongServer);
        }
        self.remove_where(
            |attachment| attachment.pairing() == pairing,
            AttachmentRemoval::PairingRemoved,
            InvalidationReason::PairingRemoved,
        )
    }

    /// Removes attachments matched by durable suppression markers and raises
    /// the incarnation high-water mark to the markers' floor, so no detached
    /// incarnation is issued again. Raising it changes no revision.
    pub fn filter_suppressed(
        &mut self,
        suppression: &dyn AttachmentSuppression,
    ) -> Result<ViewUpdate, ViewError> {
        let update = self.remove_where(
            |attachment| suppression.suppresses(attachment),
            AttachmentRemoval::Suppressed,
            InvalidationReason::Suppressed,
        )?;
        if let Some(floor) = suppression.incarnation_floor() {
            self.incarnation_high_water = self.incarnation_high_water.max(floor.get());
        }
        Ok(update)
    }

    /// Builds the launch view without connectivity from the retained view, or
    /// from the empty view when `view` is None, then removes every attachment
    /// the suppression markers match and applies their incarnation floor.
    /// Pass None when the load found no document or when the launch has no
    /// readable document. The floor applies either way, so a detached
    /// incarnation that never reached a published document is not issued
    /// again. Restored Workspaces keep their retained availability until a
    /// Ready reconcile of their Server.
    pub fn restore(
        view: Option<&PersistedView>,
        suppression: &dyn AttachmentSuppression,
    ) -> Result<(Self, ViewUpdate), ViewError> {
        let mut state = Self::new();
        if let Some(view) = view {
            let parts = view.parts();
            state.revision = parts.revision;
            state.incarnation_high_water = parts.incarnation_high_water;
            state.active = parts.active;
            state.attachments.reserve_exact(parts.workspaces.len());
            state.views.reserve_exact(parts.workspaces.len());
            for workspace in &parts.workspaces {
                state.attachments.push(workspace.attachment);
                state.views.push(WorkspaceView::import(workspace)?);
            }
        }
        let update = state.filter_suppressed(suppression)?;
        Ok((state, update))
    }

    /// The retained form of the personal view, including each Workspace's
    /// availability. Worktree scopes and reconciliation marks are not retained.
    pub fn persisted(&self) -> Result<PersistedView, ViewError> {
        let workspaces = self
            .views
            .iter()
            .map(WorkspaceView::export)
            .collect::<Result<Vec<_>, _>>()?;
        PersistedView::new(PersistedViewParts {
            revision: self.revision,
            incarnation_high_water: self.incarnation_high_water,
            workspaces,
            active: self.active,
        })
    }

    fn position(&self, workspace: WorkspaceId) -> Option<usize> {
        self.attachments
            .iter()
            .position(|attachment| attachment.workspace() == workspace)
    }

    fn unchanged(&self) -> ViewUpdate {
        ViewUpdate {
            revision: self.revision,
            changes: Vec::new(),
            invalidations: Vec::new(),
        }
    }

    fn next_revision(&self) -> Result<ViewRevision, ViewError> {
        self.revision
            .checked_successor()
            .ok_or(ViewCounter::ViewRevision.exhausted())
    }

    fn represented(&self) -> Counts {
        self.views.iter().fold(Counts::default(), |total, view| {
            total.plus(view.represented())
        })
    }

    fn check_mark(&self, replica: &ReplicaObservation<'_>) -> Result<(), ViewError> {
        match self.marks.get(&replica.server()) {
            Some(mark) if (replica.epoch(), replica.revision()) < *mark => {
                Err(ViewError::StaleObservation)
            }
            _ => Ok(()),
        }
    }

    fn raise_mark(&mut self, replica: &ReplicaObservation<'_>) {
        let observed = (replica.epoch(), replica.revision());
        let mark = self.marks.entry(replica.server()).or_insert(observed);
        if observed > *mark {
            *mark = observed;
        }
    }

    fn edit_tabs(
        &mut self,
        workspace: WorkspaceId,
        edit: impl FnOnce(&mut TabOrder) -> Result<Option<TabEdit>, ViewError>,
    ) -> Result<ViewUpdate, ViewError> {
        let index = self.position(workspace).ok_or(ViewError::TargetMissing)?;
        let mut tabs = self.views[index].tabs.clone();
        let Some(kind) = edit(&mut tabs)? else {
            return Ok(self.unchanged());
        };
        let revision = self.next_revision()?;
        let change = match kind {
            TabEdit::Activated => ViewChange::TabActivated {
                workspace,
                tab: tabs.active(),
                cause: ActivationCause::Navigation,
            },
            TabEdit::Reordered => ViewChange::TabsReordered { workspace },
        };
        self.views[index].tabs = tabs;
        self.revision = revision;
        Ok(ViewUpdate {
            revision,
            changes: vec![change],
            invalidations: Vec::new(),
        })
    }

    fn remove_where(
        &mut self,
        matches: impl Fn(&WorkspaceAttachment) -> bool,
        reason: AttachmentRemoval,
        invalidation: InvalidationReason,
    ) -> Result<ViewUpdate, ViewError> {
        let mut batch = Batch::default();
        let mut removed = BTreeSet::new();
        for view in &self.views {
            if matches(&view.attachment) {
                batch.change(ViewChange::AttachmentRemoved {
                    attachment: view.attachment,
                    reason,
                });
                view.invalidate_all(&mut batch, invalidation);
                removed.insert(view.workspace());
            }
        }
        if removed.is_empty() {
            return Ok(self.unchanged());
        }
        let revision = self.next_revision()?;
        if let Some(change) = self.drop_attachments(&removed) {
            batch.change(change);
        }
        self.revision = revision;
        Ok(batch.into_update(revision))
    }

    /// Removes attachments. A removed Active Workspace falls back to the next
    /// surviving attachment at its old position, then the preceding one.
    fn drop_attachments(&mut self, removed: &BTreeSet<WorkspaceId>) -> Option<ViewChange> {
        if removed.is_empty() {
            return None;
        }
        let old_active = self
            .active
            .filter(|active| removed.contains(active))
            .and_then(|active| self.position(active));
        let survives =
            |attachment: &WorkspaceAttachment| !removed.contains(&attachment.workspace());
        let fallback = old_active.map(|position| {
            self.attachments[position + 1..]
                .iter()
                .find(|attachment| survives(attachment))
                .or_else(|| {
                    self.attachments[..position]
                        .iter()
                        .rev()
                        .find(|attachment| survives(attachment))
                })
                .map(|attachment| attachment.workspace())
        });
        self.attachments.retain(survives);
        self.views.retain(|view| survives(&view.attachment));
        let fallback = fallback?;
        self.active = fallback;
        Some(ViewChange::WorkspaceActivated {
            workspace: fallback,
            cause: ActivationCause::Restoration,
        })
    }
}

enum TabEdit {
    Activated,
    Reordered,
}

/// Changes staged before commit.
#[derive(Default)]
struct Batch {
    changes: Vec<ViewChange>,
    invalidations: Vec<ProjectionInvalidation>,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.invalidations.is_empty()
    }

    fn change(&mut self, change: ViewChange) {
        self.changes.push(change);
    }

    fn invalidate(
        &mut self,
        workspace: WorkspaceId,
        tab: TabId,
        arrangement: &PaneArrangement,
        reason: InvalidationReason,
    ) {
        for pane in arrangement.pane_ids() {
            self.invalidations.push(ProjectionInvalidation {
                workspace,
                tab,
                pane,
                reason,
            });
        }
    }

    /// Reports one authoritative arrangement change. Fallback focus and
    /// cleared zoom are restoration.
    fn arrangement(
        &mut self,
        workspace: WorkspaceId,
        arrangement: &PaneArrangement,
        change: ArrangementChange,
    ) {
        if change.is_empty() {
            return;
        }
        let tab = arrangement.tab();
        for pane in &change.removed {
            self.invalidations.push(ProjectionInvalidation {
                workspace,
                tab,
                pane: *pane,
                reason: InvalidationReason::TargetDeleted,
            });
        }
        if !change.removed.is_empty() {
            self.change(ViewChange::PanesRemoved {
                workspace,
                tab,
                panes: change.removed,
            });
        }
        if !change.inserted.is_empty() {
            self.change(ViewChange::PanesAdded {
                workspace,
                tab,
                panes: change.inserted,
            });
        }
        if change.focus_changed
            && let Some(pane) = arrangement.focused_pane()
        {
            self.change(ViewChange::PaneFocused {
                workspace,
                tab,
                pane,
                cause: ActivationCause::Restoration,
            });
        }
        if change.zoom_cleared {
            self.change(ViewChange::ZoomChanged {
                workspace,
                tab,
                zoom: arrangement.zoom(),
            });
        }
        self.change(ViewChange::LayoutChanged {
            workspace,
            tab,
            layout: arrangement.layout_revision(),
        });
    }

    fn into_update(self, revision: ViewRevision) -> ViewUpdate {
        ViewUpdate {
            revision,
            changes: self.changes,
            invalidations: self.invalidations,
        }
    }
}

/// Represented Tabs and Panes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Counts {
    tabs: usize,
    panes: usize,
}

impl Counts {
    fn plus(self, other: Self) -> Self {
        Self {
            tabs: self.tabs.saturating_add(other.tabs),
            panes: self.panes.saturating_add(other.panes),
        }
    }

    fn minus(self, other: Self) -> Self {
        Self {
            tabs: self.tabs.saturating_sub(other.tabs),
            panes: self.panes.saturating_sub(other.panes),
        }
    }

    fn exceeded(self) -> Option<ViewResource> {
        if self.tabs > MAX_VIEW_TABS {
            Some(ViewResource::Tabs)
        } else if self.panes > MAX_VIEW_PANES {
            Some(ViewResource::Panes)
        } else {
            None
        }
    }
}

/// The Workspace's complete authoritative size, or the per-Tab limit it breaks.
fn authoritative(
    replica: &ReplicaObservation<'_>,
    workspace: WorkspaceId,
) -> Result<Counts, ViewResource> {
    let mut counts = Counts::default();
    for tab in replica.workspace_tabs(workspace) {
        let panes = replica.tab_pane_count(tab.id);
        if panes > MAX_TAB_PANES {
            return Err(ViewResource::TabPanes);
        }
        counts.tabs += 1;
        counts.panes += panes;
    }
    Ok(counts)
}

/// A Tab's default arrangement: its creation tree with its Root Pane focused.
fn default_arrangement(
    tab: &TabRecord,
    replica: &ReplicaObservation<'_>,
) -> Result<PaneArrangement, ViewError> {
    let layout = replica.creation_layout(tab.id)?;
    PaneArrangement::from_creation(tab.id, &layout, tab.root_pane)
}

/// The snapshot's Tab records, checked to belong to the Workspace's Server.
fn scoped_tabs(
    workspace: WorkspaceId,
    organization: &OrganizationSnapshot,
) -> Result<&[TabRecord], ViewError> {
    if organization.state().server != workspace.server() {
        return Err(ViewError::WrongServer);
    }
    Ok(&organization.state().tabs)
}
