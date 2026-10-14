//! Committed organization publication. Connections share the current immutable
//! snapshot and a bounded delta journal; they never own authoritative state.

use crate::{contracts::*, failure::failure};
use spaceterm_core::{
    authority::{OrganizationRevision, StoreRevision},
    organization::*,
    organization_retained::*,
    remote_access::RemoteAccessPolicy,
    storage::CommitReceipt,
};
use spaceterm_protocol::{
    codec::Payload,
    convert::organization::{self, PublicationPart},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
    sync::{Arc, Mutex, Weak},
};

pub const MAX_JOURNAL_DELTAS: usize = 256;
pub const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HISTORICAL_SNAPSHOTS: usize = 2;

pub enum PublicationPlan {
    Current,
    Snapshot(Arc<OrganizationSnapshot>),
    Deltas(Vec<Arc<OrganizationDelta>>),
}
struct JournalEntry {
    delta: Arc<OrganizationDelta>,
    bytes: usize,
}
struct SnapshotPin {
    snapshot: Mutex<Option<Arc<OrganizationSnapshot>>>,
}
impl SnapshotPin {
    fn cancel(&self) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.take();
        }
    }
    fn is_current(&self) -> bool {
        self.snapshot.lock().is_ok_and(|v| v.is_some())
    }
}

pub struct PublicationHub {
    snapshot: Arc<OrganizationSnapshot>,
    store: Option<StoreRevision>,
    journal: VecDeque<JournalEntry>,
    journal_bytes: usize,
    transfers: u64,
    pins: BTreeMap<OrganizationRevision, Weak<SnapshotPin>>,
}
impl PublicationHub {
    pub fn new(snapshot: Arc<OrganizationSnapshot>) -> Result<Self, ServerFailure> {
        if snapshot.epoch().get() == 0 {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        Ok(Self {
            snapshot,
            store: None,
            journal: VecDeque::new(),
            journal_bytes: 0,
            transfers: 0,
            pins: BTreeMap::new(),
        })
    }
    pub fn snapshot(&self) -> Arc<OrganizationSnapshot> {
        self.snapshot.clone()
    }
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }
    pub fn journal_bytes(&self) -> usize {
        self.journal_bytes
    }
    pub fn historical_pins(&self) -> usize {
        self.pins
            .iter()
            .filter(|(revision, pin)| {
                **revision != self.snapshot.state().revision && pin.strong_count() != 0
            })
            .count()
    }
    pub fn committed(
        &mut self,
        receipt: &CommitReceipt,
        snapshot: Arc<OrganizationSnapshot>,
        delta: Option<Arc<OrganizationDelta>>,
    ) -> Result<(), ServerFailure> {
        let invalid = || failure(ServerFailureClass::RecoveryRequired);
        if receipt.organization_revision != snapshot.state().revision
            || snapshot.state().server != self.snapshot.state().server
            || self
                .store
                .is_some_and(|store| store.checked_successor() != Some(receipt.store_revision))
            || snapshot.epoch() != self.snapshot.epoch()
            || snapshot.state().revision < self.snapshot.state().revision
        {
            return Err(invalid());
        }
        if snapshot.state().revision == self.snapshot.state().revision {
            if delta.is_some() || *snapshot != *self.snapshot {
                return Err(invalid());
            }
            self.store = Some(receipt.store_revision);
            return Ok(());
        }
        if self.snapshot.state().revision.checked_successor() != Some(snapshot.state().revision) {
            return Err(invalid());
        }
        if let Some(delta) = &delta
            && (delta.server() != snapshot.state().server
                || delta.epoch() != snapshot.epoch()
                || delta.base() != self.snapshot.state().revision
                || delta.revision() != snapshot.state().revision)
        {
            return Err(invalid());
        }
        // Resolve unchanged record fields against this commit, before retaining
        // the delta. Later encoding must not consult a newer snapshot.
        let retained = delta
            .as_deref()
            .map(|delta| complete_delta(delta, &snapshot))
            .transpose()?
            .flatten();
        self.store = Some(receipt.store_revision);
        self.snapshot = snapshot;
        match retained {
            Some((delta, bytes)) => {
                while self.journal.len() >= MAX_JOURNAL_DELTAS
                    || self.journal_bytes > MAX_JOURNAL_BYTES - bytes
                {
                    if let Some(entry) = self.journal.pop_front() {
                        self.journal_bytes -= entry.bytes;
                    } else {
                        break;
                    }
                }
                self.journal.push_back(JournalEntry {
                    delta: Arc::new(delta),
                    bytes,
                });
                self.journal_bytes += bytes;
            }
            _ => {
                self.journal.clear();
                self.journal_bytes = 0;
            }
        }
        self.bound_pins();
        Ok(())
    }
    pub fn plan_since(&self, base: Option<OrganizationRevision>) -> PublicationPlan {
        let Some(mut cursor) = base else {
            return PublicationPlan::Snapshot(self.snapshot.clone());
        };
        if cursor == self.snapshot.state().revision {
            return PublicationPlan::Current;
        }
        let mut deltas = Vec::new();
        for entry in &self.journal {
            if entry.delta.base() == cursor {
                cursor = entry.delta.revision();
                deltas.push(entry.delta.clone());
            }
        }
        if cursor == self.snapshot.state().revision && !deltas.is_empty() {
            PublicationPlan::Deltas(deltas)
        } else {
            PublicationPlan::Snapshot(self.snapshot.clone())
        }
    }
    fn bound_pins(&mut self) {
        self.pins.retain(|_, pin| pin.strong_count() != 0);
        while self.historical_pins() > MAX_HISTORICAL_SNAPSHOTS {
            let oldest = self
                .pins
                .keys()
                .copied()
                .find(|r| *r != self.snapshot.state().revision);
            let Some(oldest) = oldest else {
                break;
            };
            if let Some(pin) = self.pins.remove(&oldest).and_then(|pin| pin.upgrade()) {
                pin.cancel();
            }
        }
    }
    pub fn begin_snapshot(
        &mut self,
        major: u32,
        request: u64,
        generation: u64,
        maximum: usize,
    ) -> Result<SnapshotTransfer, ServerFailure> {
        if !matches!(major, 1 | 2)
            || request == 0
            || maximum == 0
            || maximum > spaceterm_protocol::limits::MAX_MANAGEMENT_BYTES
        {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        let transfer = self
            .transfers
            .checked_add(1)
            .ok_or_else(|| failure(ServerFailureClass::Exhausted))?;
        let revision = self.snapshot.state().revision;
        let pin = self
            .pins
            .get(&revision)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                Arc::new(SnapshotPin {
                    snapshot: Mutex::new(Some(self.snapshot.clone())),
                })
            });
        // The highest index and the completion marker bound every actual header,
        // including protobuf fields omitted from index-zero, incomplete parts.
        let part = PublicationPart {
            request,
            generation,
            transfer,
            index: spaceterm_core::management::MAX_PUBLICATION_PARTS - 1,
            total: spaceterm_core::management::MAX_PUBLICATION_PARTS,
            epoch: self.snapshot.epoch(),
            base: OrganizationRevision::new(0),
            revision,
        };
        let header = organization::snapshot_header_bound(&self.snapshot, major, part)
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        if header > maximum {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let mut ranges = Vec::new();
        let mut start = 0;
        let mut end = 0;
        let mut bytes = header;
        organization::snapshot_record_bounds(&self.snapshot, |record_bytes| {
            if record_bytes > maximum - header {
                return Err(spaceterm_protocol::ProtocolError::LimitExceeded);
            }
            if record_bytes > maximum - bytes {
                if ranges.len() + 1 >= spaceterm_core::management::MAX_PUBLICATION_PARTS as usize {
                    return Err(spaceterm_protocol::ProtocolError::LimitExceeded);
                }
                ranges.push(start..end);
                start = end;
                bytes = header;
            }
            bytes += record_bytes;
            end += 1;
            Ok(())
        })
        .map_err(|_| failure(ServerFailureClass::Capacity))?;
        ranges.push(start..end);
        self.transfers = transfer;
        self.pins.insert(revision, Arc::downgrade(&pin));
        self.bound_pins();
        Ok(SnapshotTransfer {
            pin,
            server: self.snapshot.state().server,
            major,
            part,
            ranges,
            index: 0,
            maximum,
        })
    }
}
#[derive(Clone)]
pub struct SnapshotPage {
    pin: Arc<SnapshotPin>,
    server: spaceterm_core::identity::ServerId,
    major: u32,
    range: Range<usize>,
    part: PublicationPart,
    maximum_encoded_bytes: usize,
}
impl std::fmt::Debug for SnapshotPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotPage")
            .field("part", &self.part)
            .finish_non_exhaustive()
    }
}
impl SnapshotPage {
    pub fn server(&self) -> spaceterm_core::identity::ServerId {
        self.server
    }
    pub fn part(&self) -> PublicationPart {
        self.part
    }
    pub fn maximum_encoded_bytes(&self) -> usize {
        self.maximum_encoded_bytes
    }
    pub fn is_current(&self) -> bool {
        self.pin.is_current()
    }
    pub(crate) fn payload(&self) -> Result<Payload, ServerFailure> {
        if !self.is_current() {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let snapshot = self
            .pin
            .snapshot
            .lock()
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        let snapshot = snapshot
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        snapshot_payload(self.major, snapshot, self.range.clone(), self.part)
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))
    }
}
pub struct SnapshotTransfer {
    pin: Arc<SnapshotPin>,
    server: spaceterm_core::identity::ServerId,
    major: u32,
    part: PublicationPart,
    ranges: Vec<Range<usize>>,
    index: usize,
    maximum: usize,
}
impl SnapshotTransfer {
    pub fn revision(&self) -> OrganizationRevision {
        self.part.revision
    }
    pub fn next_page(&mut self) -> Result<Option<SnapshotPage>, ServerFailure> {
        if !self.pin.is_current() {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let Some(range) = self.ranges.get(self.index) else {
            return Ok(None);
        };
        let part = PublicationPart {
            index: self.index as u32,
            total: self.ranges.len() as u32,
            ..self.part
        };
        self.index += 1;
        Ok(Some(SnapshotPage {
            pin: self.pin.clone(),
            server: self.server,
            major: self.major,
            range: range.clone(),
            part,
            maximum_encoded_bytes: self.maximum,
        }))
    }
}
fn snapshot_payload(
    major: u32,
    snapshot: &OrganizationSnapshot,
    range: Range<usize>,
    part: PublicationPart,
) -> Result<Payload, spaceterm_protocol::ProtocolError> {
    match major {
        1 => organization::v1::snapshot_payload(snapshot, range, part),
        2 => organization::v2::snapshot_payload(snapshot, range, part),
        _ => Err(spaceterm_protocol::ProtocolError::Incompatible),
    }
}
enum PublicationRecord<'a> {
    Workspace(&'a WorkspaceRecord, &'a WorkspaceNameBasis),
    Tab(&'a TabRecord, &'a TabCreationDefault, TabMembershipVersion),
    Pane(&'a PaneRecord, PaneGenerationHighWater, PaneRuntimeRecord),
    Repository(&'a RepositoryRecord, RepositoryInventoryVersion),
    Worktree(&'a WorktreeRecord, WorktreeInventoryFacts),
    Settings(&'a ServerSettingsRecord, &'a Versioned<RemoteAccessPolicy>),
}
impl PublicationRecord<'_> {
    fn write_count(&self) -> usize {
        match self {
            Self::Tab(..) => 3,
            _ => 2,
        }
    }
    fn variable_bytes(&self) -> usize {
        let directory = |v: &Option<ServerDirectory>| v.as_ref().map_or(0, |v| v.bytes().len());
        match self {
            Self::Workspace(v, basis) => {
                v.name.value.display.as_str().len()
                    + directory(&v.pinned_directory.value)
                    + basis.automatic_base().as_str().len()
                    + basis.assigned_automatic().as_str().len()
            }
            Self::Tab(_, default, _) => std::mem::size_of_val(default.replay_order()),
            Self::Pane(v, _, _) => {
                v.starting_directory.bytes().len()
                    + directory(&v.confirmed_restoration_directory.value)
            }
            Self::Repository(v, _) => v.directory.bytes().len(),
            Self::Worktree(v, _) => v.directory.bytes().len(),
            Self::Settings(v, remote) => {
                v.worktree_location.value.as_str().len()
                    + std::mem::size_of_val(remote.value.endpoints())
            }
        }
    }
    fn append(self, writes: &mut Vec<OrganizationWrite>, runtime: &mut Vec<PaneRuntimeRecord>) {
        use OrganizationWrite as W;
        use RetainedOrganizationWrite as R;
        match self {
            Self::Workspace(v, basis) => {
                writes.push(W::PutWorkspace(v.clone()));
                writes.push(W::Retained(R::PutWorkspaceNameBasis(basis.clone())));
            }
            Self::Tab(v, default, membership) => {
                writes.push(W::PutTab(v.clone()));
                writes.push(W::Retained(R::PutCreationDefault(default.clone())));
                writes.push(W::Retained(R::SetTabMembershipVersion(membership)));
            }
            Self::Pane(v, marks, rt) => {
                writes.push(W::PutPane(v.clone()));
                writes.push(W::Retained(R::SetPaneGenerationHighWater(marks)));
                runtime.push(rt);
            }
            Self::Repository(v, version) => {
                writes.push(W::PutRepository(v.clone()));
                writes.push(W::Retained(R::SetRepositoryInventoryVersion(version)));
            }
            Self::Worktree(v, inventory) => {
                writes.push(W::PutWorktree(v.clone()));
                writes.push(W::Retained(R::SetWorktreeInventoryFacts(inventory)));
            }
            Self::Settings(v, remote) => {
                writes.push(W::SetServerSettings(v.clone()));
                writes.push(W::Retained(R::SetRemoteAccess(remote.clone())));
            }
        }
    }
}
fn is_deletion(write: &OrganizationWrite) -> bool {
    use OrganizationWrite as W;
    use RetainedOrganizationWrite as R;
    matches!(
        write,
        W::DeleteWorkspace(_)
            | W::DeleteTab(_)
            | W::DeletePane(_)
            | W::DeleteRepository(_)
            | W::DeleteWorktree(_)
            | W::Retained(
                R::DeleteWorkspaceNameBasis(_)
                    | R::DeleteCreationDefault(_)
                    | R::DeletePaneGenerationHighWater(_)
                    | R::DeleteTabMembershipVersion(_)
                    | R::DeleteRepositoryInventoryVersion(_)
                    | R::DeleteWorktreeInventoryFacts(_)
            )
    )
}
fn checked_bytes(total: usize, additional: usize) -> Result<usize, ServerFailure> {
    total
        .checked_add(additional)
        .ok_or_else(|| failure(ServerFailureClass::Capacity))
}
fn complete_delta(
    delta: &OrganizationDelta,
    snapshot: &OrganizationSnapshot,
) -> Result<Option<(OrganizationDelta, usize)>, ServerFailure> {
    use OrganizationWrite as W;
    use RetainedOrganizationWrite as R;
    // Reject oversized upstream allocations before building even borrowed indices.
    if delta_bytes(delta)? > MAX_JOURNAL_BYTES {
        return Ok(None);
    }
    let mut workspaces = BTreeSet::new();
    let mut tabs = BTreeSet::new();
    let mut panes = BTreeSet::new();
    let mut repositories = BTreeSet::new();
    let mut worktrees = BTreeSet::new();
    let mut settings = false;
    for write in delta.writes() {
        match write {
            W::PutWorkspace(v) => {
                workspaces.insert(v.id);
            }
            W::Retained(R::PutWorkspaceNameBasis(v)) => {
                workspaces.insert(v.workspace());
            }
            W::PutTab(v) => {
                tabs.insert(v.id);
            }
            W::Retained(R::PutCreationDefault(v)) => {
                tabs.insert(v.tab());
            }
            W::Retained(R::SetTabMembershipVersion(v)) => {
                tabs.insert(v.id());
            }
            W::PutPane(v) => {
                panes.insert(v.id);
            }
            W::Retained(R::SetPaneGenerationHighWater(v)) => {
                panes.insert(v.pane());
            }
            W::PutRepository(v) => {
                repositories.insert(v.id);
            }
            W::Retained(R::SetRepositoryInventoryVersion(v)) => {
                repositories.insert(v.id());
            }
            W::PutWorktree(v) => {
                worktrees.insert(v.id);
            }
            W::Retained(R::SetWorktreeInventoryFacts(v)) => {
                worktrees.insert(v.worktree());
            }
            W::SetServerSettings(_) | W::Retained(R::SetRemoteAccess(_)) => settings = true,
            _ => {}
        }
    }
    panes.extend(delta.runtime().iter().map(|v| v.pane));
    let state = snapshot.state();
    let invalid = || failure(ServerFailureClass::RecoveryRequired);
    // Legacy, record-free publications remain usable without retained facts.
    let facts = state.retained.as_ref();
    let mut records = Vec::new();
    for id in workspaces {
        records.push(PublicationRecord::Workspace(
            state
                .workspaces
                .iter()
                .find(|v| v.id == id)
                .ok_or_else(invalid)?,
            facts
                .ok_or_else(invalid)?
                .workspace_names()
                .iter()
                .find(|v| v.workspace() == id)
                .ok_or_else(invalid)?,
        ));
    }
    for id in tabs {
        let facts = facts.ok_or_else(invalid)?;
        records.push(PublicationRecord::Tab(
            state.tabs.iter().find(|v| v.id == id).ok_or_else(invalid)?,
            facts
                .creation_defaults()
                .iter()
                .find(|v| v.tab() == id)
                .ok_or_else(invalid)?,
            *facts
                .tab_membership_versions()
                .iter()
                .find(|v| v.id() == id)
                .ok_or_else(invalid)?,
        ));
    }
    for id in panes {
        records.push(PublicationRecord::Pane(
            state
                .panes
                .iter()
                .find(|v| v.id == id)
                .ok_or_else(invalid)?,
            *facts
                .ok_or_else(invalid)?
                .pane_generations()
                .iter()
                .find(|v| v.pane() == id)
                .ok_or_else(invalid)?,
            *snapshot
                .runtime()
                .iter()
                .find(|v| v.pane == id)
                .ok_or_else(invalid)?,
        ));
    }
    for id in repositories {
        records.push(PublicationRecord::Repository(
            state
                .repositories
                .iter()
                .find(|v| v.id == id)
                .ok_or_else(invalid)?,
            *facts
                .ok_or_else(invalid)?
                .repository_inventory_versions()
                .iter()
                .find(|v| v.id() == id)
                .ok_or_else(invalid)?,
        ));
    }
    for id in worktrees {
        records.push(PublicationRecord::Worktree(
            state
                .worktrees
                .iter()
                .find(|v| v.id == id)
                .ok_or_else(invalid)?,
            *facts
                .ok_or_else(invalid)?
                .worktree_inventory()
                .iter()
                .find(|v| v.worktree() == id)
                .ok_or_else(invalid)?,
        ));
    }
    if settings {
        records.push(PublicationRecord::Settings(
            &state.settings,
            facts.ok_or_else(invalid)?.remote_access(),
        ));
    }
    let mut write_count = delta.writes().iter().filter(|v| is_deletion(v)).count();
    let mut runtime_count = 0usize;
    let mut bytes = checked_bytes(
        std::mem::size_of::<OrganizationDelta>() + std::mem::size_of::<JournalEntry>(),
        std::mem::size_of_val(delta.accepted_creations()),
    )?;
    for record in &records {
        write_count = checked_bytes(write_count, record.write_count())?;
        if matches!(record, PublicationRecord::Pane(..)) {
            runtime_count = checked_bytes(runtime_count, 1)?;
        }
        bytes = checked_bytes(bytes, record.variable_bytes())?;
    }
    for (count, width) in [
        (write_count, std::mem::size_of::<OrganizationWrite>()),
        (runtime_count, std::mem::size_of::<PaneRuntimeRecord>()),
    ] {
        bytes = checked_bytes(
            bytes,
            count
                .checked_mul(width)
                .ok_or_else(|| failure(ServerFailureClass::Capacity))?,
        )?;
    }
    if bytes > MAX_JOURNAL_BYTES {
        return Ok(None);
    }
    // Exact capacities and value clones discard all upstream excess capacity.
    let mut writes = Vec::with_capacity(write_count);
    let mut runtime = Vec::with_capacity(runtime_count);
    writes.extend(delta.writes().iter().filter(|v| is_deletion(v)).cloned());
    for record in records {
        record.append(&mut writes, &mut runtime);
    }
    let complete = OrganizationDelta::new(
        delta.server(),
        delta.epoch(),
        delta.base(),
        delta.revision(),
        writes,
        runtime,
    )
    .and_then(|v| v.with_accepted_creations(delta.accepted_creations().to_vec()))
    .map_err(|_| invalid())?;
    Ok(Some((complete, bytes)))
}

/// Encode one complete journal delta without retaining or consulting a snapshot.
/// The caller reserves `maximum` egress bytes before calling this helper. Capacity
/// requires an explicit resync followed by a new query, not a replacement snapshot.
pub(crate) fn delta_payload(
    major: u32,
    delta: &OrganizationDelta,
    query: organization::OrganizationQuery,
    maximum: usize,
) -> Result<Payload, ServerFailure> {
    use OrganizationWrite as W;
    use RetainedOrganizationWrite as R;
    use spaceterm_protocol::convert::identity::*;
    if !matches!(major, 1 | 2)
        || maximum == 0
        || maximum > spaceterm_protocol::limits::MAX_MANAGEMENT_BYTES
    {
        return Err(failure(ServerFailureClass::ScopeMismatch));
    }
    if delta.epoch().get() == 0 || delta.revision() <= delta.base() {
        return Err(failure(ServerFailureClass::RecoveryRequired));
    }
    if delta_bytes(delta)? > MAX_JOURNAL_BYTES || delta_wire_bound(delta)? > maximum {
        return Err(failure(ServerFailureClass::Capacity));
    }
    validate_delta_completeness(delta)?;
    let part = PublicationPart {
        request: query.request.get(),
        generation: query.generation,
        transfer: delta.revision().get(),
        index: 0,
        total: 1,
        epoch: delta.epoch(),
        base: delta.base(),
        revision: delta.revision(),
    };
    let invalid = || failure(ServerFailureClass::RecoveryRequired);
    macro_rules! encode {
        ($p:ident, $variant:ident) => {{
            use spaceterm_protocol::{convert::organization::$p as convert, wire::$p as wire};
            let mut result = wire::OrganizationDelta {
                server: Some(encode_global(delta.server().bytes())),
                epoch: delta.epoch().get(),
                base_revision: delta.base().get(),
                revision: delta.revision().get(),
                complete: true,
                part: Some(convert::encode_part(part)),
                ..Default::default()
            };
            for write in delta.writes() {
                match write {
                    W::PutWorkspace(v) => {
                        let basis = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::PutWorkspaceNameBasis(b))
                                    if b.workspace() == v.id =>
                                {
                                    Some(b)
                                }
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        result
                            .workspaces
                            .push(convert::encode_workspace_record(v, basis));
                    }
                    W::PutTab(v) => {
                        let default = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::PutCreationDefault(d)) if d.tab() == v.id => Some(d),
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        let membership = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::SetTabMembershipVersion(m)) if m.id() == v.id => {
                                    Some(*m)
                                }
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        result
                            .tabs
                            .push(convert::encode_tab_record(v, default, membership));
                    }
                    W::PutPane(v) => {
                        let runtime = delta
                            .runtime()
                            .iter()
                            .find(|rt| rt.pane == v.id)
                            .ok_or_else(invalid)?;
                        let marks = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::SetPaneGenerationHighWater(m))
                                    if m.pane() == v.id =>
                                {
                                    Some(*m)
                                }
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        if marks.session() != runtime.session_generation
                            || marks.control() != runtime.control_generation
                        {
                            return Err(invalid());
                        }
                        result.panes.push(convert::encode_pane_record(v, *runtime));
                    }
                    W::PutRepository(v) => {
                        let version = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::SetRepositoryInventoryVersion(m))
                                    if m.id() == v.id =>
                                {
                                    Some(m.version())
                                }
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        result.repositories.push(wire::RepositoryRecord {
                            id: Some(encode_repository(v.id)),
                            directory: Some(convert::encode_directory(&v.directory)),
                            inventory_version: version.get(),
                        });
                    }
                    W::PutWorktree(v) => {
                        let inventory = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::SetWorktreeInventoryFacts(m))
                                    if m.worktree() == v.id =>
                                {
                                    Some(*m)
                                }
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        result.worktrees.push(wire::Worktree {
                            id: Some(encode_worktree(v.id)),
                            repository: Some(encode_repository(v.repository)),
                            directory_value: Some(convert::encode_directory(&v.directory)),
                            listed: inventory.listed(),
                            version: inventory.version().get(),
                            main: v.main,
                            missing: v.missing,
                            ..Default::default()
                        });
                    }
                    W::SetServerSettings(v) => {
                        let remote = delta
                            .writes()
                            .iter()
                            .find_map(|w| match w {
                                W::Retained(R::SetRemoteAccess(m)) => Some(m),
                                _ => None,
                            })
                            .ok_or_else(invalid)?;
                        result.settings = Some(convert::encode_settings(v, remote));
                    }
                    W::DeleteWorkspace(v) => result.deleted_workspaces.push(encode_workspace(*v)),
                    W::DeleteTab(v) => result.deleted_tabs.push(encode_tab(*v)),
                    W::DeletePane(v) => result.deleted_panes.push(encode_pane(*v)),
                    W::DeleteRepository(v) => {
                        result.deleted_repositories.push(encode_repository(*v))
                    }
                    W::DeleteWorktree(v) => result.deleted_worktrees.push(encode_worktree(*v)),
                    W::Retained(_) => {}
                }
            }
            result.accepted_pane_creations = delta
                .accepted_creations()
                .iter()
                .map(|v| wire::AcceptedPaneCreation {
                    tab: Some(encode_tab(v.tab())),
                    pane: Some(encode_pane(v.pane())),
                    source: Some(encode_pane(v.source())),
                    edge: organization::edge_number(v.axis(), v.edge()),
                    creation_ordinal: v.ordinal().get(),
                })
                .collect();
            Payload::$variant(wire::Message {
                kind: Some(wire::message::Kind::OrganizationDelta(result)),
            })
        }};
    }
    let payload = match major {
        1 => encode!(v1, Major1),
        2 => encode!(v2, Major2),
        _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
    };
    if payload.encoded_len() > maximum {
        return Err(failure(ServerFailureClass::Capacity));
    }
    Ok(payload)
}
fn validate_delta_completeness(delta: &OrganizationDelta) -> Result<(), ServerFailure> {
    use OrganizationWrite as W;
    use RetainedOrganizationWrite as R;
    for write in delta.writes() {
        let W::Retained(value) = write else {
            continue;
        };
        let present = delta.writes().iter().any(|owner| match (value, owner) {
            (R::PutWorkspaceNameBasis(v), W::PutWorkspace(w)) => v.workspace() == w.id,
            (R::PutCreationDefault(v), W::PutTab(w)) => v.tab() == w.id,
            (R::SetTabMembershipVersion(v), W::PutTab(w)) => v.id() == w.id,
            (R::SetPaneGenerationHighWater(v), W::PutPane(w)) => v.pane() == w.id,
            (R::SetRepositoryInventoryVersion(v), W::PutRepository(w)) => v.id() == w.id,
            (R::SetWorktreeInventoryFacts(v), W::PutWorktree(w)) => v.worktree() == w.id,
            (R::SetRemoteAccess(_), W::SetServerSettings(_)) => true,
            (R::DeleteWorkspaceNameBasis(v), W::DeleteWorkspace(w)) => v == w,
            (R::DeleteCreationDefault(v), W::DeleteTab(w)) => v == w,
            (R::DeleteTabMembershipVersion(v), W::DeleteTab(w)) => v == w,
            (R::DeletePaneGenerationHighWater(v), W::DeletePane(w)) => v == w,
            (R::DeleteRepositoryInventoryVersion(v), W::DeleteRepository(w)) => v == w,
            (R::DeleteWorktreeInventoryFacts(v), W::DeleteWorktree(w)) => v == w,
            _ => false,
        });
        if !present {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
    }
    if delta.runtime().iter().any(|rt| {
        !delta
            .writes()
            .iter()
            .any(|write| matches!(write, W::PutPane(v) if v.id == rt.pane))
    }) {
        return Err(failure(ServerFailureClass::RecoveryRequired));
    }
    Ok(())
}
fn delta_wire_bound(delta: &OrganizationDelta) -> Result<usize, ServerFailure> {
    use OrganizationWrite as W;
    use RetainedOrganizationWrite as R;
    // Borrowed bounds cover either major's protobuf tags, nested length prefixes,
    // qualified identities and maximum-width varints. No DTO is allocated here.
    let directory = |v: &ServerDirectory| v.bytes().len() + 64;
    let mut bytes = 256usize;
    for write in delta.writes() {
        let additional = match write {
            W::PutWorkspace(v) => {
                256 + v.name.value.display.as_str().len()
                    + v.pinned_directory.value.as_ref().map_or(0, directory)
            }
            W::PutTab(_) => 256,
            W::PutPane(v) => {
                512 + directory(&v.starting_directory)
                    + v.confirmed_restoration_directory
                        .value
                        .as_ref()
                        .map_or(0, directory)
            }
            W::PutRepository(v) => 256 + directory(&v.directory),
            W::PutWorktree(v) => 256 + directory(&v.directory),
            W::SetServerSettings(v) => 192 + v.worktree_location.value.as_str().len(),
            W::Retained(R::PutWorkspaceNameBasis(v)) => {
                v.automatic_base().as_str().len() + v.assigned_automatic().as_str().len()
            }
            W::Retained(R::PutCreationDefault(v)) => v
                .replay_order()
                .len()
                .checked_mul(64)
                .ok_or_else(|| failure(ServerFailureClass::Capacity))?,
            W::Retained(R::SetRemoteAccess(v)) => 64 + v.value.endpoints().len() * 64,
            W::Retained(_) => 0,
            _ => 64,
        };
        bytes = checked_bytes(bytes, additional)?;
    }
    checked_bytes(
        bytes,
        delta
            .accepted_creations()
            .len()
            .checked_mul(192)
            .ok_or_else(|| failure(ServerFailureClass::Capacity))?,
    )
}

fn delta_bytes(delta: &OrganizationDelta) -> Result<usize, ServerFailure> {
    use OrganizationWrite as W;
    use spaceterm_core::organization_retained::RetainedOrganizationWrite as R;
    let mut bytes = [
        std::mem::size_of::<OrganizationDelta>(),
        std::mem::size_of::<JournalEntry>(),
        std::mem::size_of_val(delta.runtime()),
        std::mem::size_of_val(delta.accepted_creations()),
        std::mem::size_of_val(delta.writes()),
    ]
    .into_iter()
    .try_fold(0usize, |total, n| {
        total
            .checked_add(n)
            .ok_or_else(|| failure(ServerFailureClass::Capacity))
    })?;
    for write in delta.writes() {
        let additional = match write {
            W::PutWorkspace(v) => {
                v.name.value.display.as_str().len()
                    + v.pinned_directory
                        .value
                        .as_ref()
                        .map_or(0, |v| v.bytes().len())
            }
            W::PutPane(v) => {
                v.starting_directory.bytes().len()
                    + v.confirmed_restoration_directory
                        .value
                        .as_ref()
                        .map_or(0, |v| v.bytes().len())
            }
            W::PutRepository(v) => v.directory.bytes().len(),
            W::PutWorktree(v) => v.directory.bytes().len(),
            W::SetServerSettings(v) => v.worktree_location.value.as_str().len(),
            W::Retained(R::PutWorkspaceNameBasis(v)) => {
                v.automatic_base().as_str().len() + v.assigned_automatic().as_str().len()
            }
            W::Retained(R::PutCreationDefault(v)) => std::mem::size_of_val(v.replay_order()),
            W::Retained(R::SetRemoteAccess(v)) => std::mem::size_of_val(v.value.endpoints()),
            _ => 0,
        };
        bytes = bytes
            .checked_add(additional)
            .ok_or_else(|| failure(ServerFailureClass::Capacity))?;
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "../tests/support/publication.rs"]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{authority::*, identity::*};

    use super::fixtures::{large_snapshot, snapshot};
    fn receipt(store: u64, organization: u64) -> CommitReceipt {
        CommitReceipt {
            store_revision: spaceterm_core::authority::StoreRevision::new(store),
            organization_revision: OrganizationRevision::new(organization),
        }
    }
    fn delta(base: u64, revision: u64) -> Arc<OrganizationDelta> {
        Arc::new(
            OrganizationDelta::new(
                ServerId::new([1; 16]),
                ServerEpoch::new(1),
                OrganizationRevision::new(base),
                OrganizationRevision::new(revision),
                vec![],
                vec![],
            )
            .unwrap(),
        )
    }
    fn compact_snapshot(revision: u64) -> Arc<OrganizationSnapshot> {
        let value = fixtures::snapshot_with_workspaces(1);
        let mut state = value.state().clone();
        state.revision = OrganizationRevision::new(revision);
        state.workspaces[0].name.value.display = NameText::new("Shell".into()).unwrap();
        state.workspaces[0].pinned_directory.value = None;
        let directory = ServerDirectory::new(state.server, b"server-directory".to_vec()).unwrap();
        for pane in &mut state.panes {
            pane.starting_directory = directory.clone();
            pane.confirmed_restoration_directory.value = None;
        }
        let repository = RepositoryId::new(state.server, 1).unwrap();
        let worktree = WorktreeId::new(state.server, 1).unwrap();
        state.repositories.push(RepositoryRecord {
            id: repository,
            directory: directory.clone(),
        });
        state.worktrees.push(WorktreeRecord {
            id: worktree,
            repository,
            directory,
            main: true,
            missing: false,
        });
        state.tabs[0].worktree = Some(worktree);
        let mut facts = state.retained.take().unwrap().parts().clone();
        facts
            .repository_inventory_versions
            .push(RepositoryInventoryVersion::new(
                repository,
                FieldVersion::new(8),
            ));
        facts.worktree_inventory.push(WorktreeInventoryFacts::new(
            worktree,
            true,
            FieldVersion::new(9),
        ));
        state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
        Arc::new(OrganizationSnapshot::new(value.epoch(), state, value.runtime().to_vec()).unwrap())
    }
    fn publication_query() -> organization::OrganizationQuery {
        organization::OrganizationQuery {
            request: std::num::NonZeroU64::new(17).unwrap(),
            generation: 23,
            known_epoch: Some(ServerEpoch::new(1)),
            known_revision: Some(OrganizationRevision::new(7)),
        }
    }
    #[test]
    fn journal_deltas_encode_complete_commit_records_in_both_majors_without_old_snapshots() {
        use OrganizationWrite as W;
        use RetainedOrganizationWrite as R;
        let initial = compact_snapshot(7);
        let mut state = initial.state().clone();
        state.revision = OrganizationRevision::new(8);
        state.workspaces[0].name.value.display = NameText::new("Renamed".into()).unwrap();
        state.workspaces[0].name.version = FieldVersion::new(3);
        let mut facts = state.retained.take().unwrap().parts().clone();
        facts.tab_membership_versions[0] =
            TabMembershipVersion::new(state.tabs[0].id, FieldVersion::new(5));
        facts.repository_inventory_versions[0] =
            RepositoryInventoryVersion::new(state.repositories[0].id, FieldVersion::new(10));
        facts.worktree_inventory[0] =
            WorktreeInventoryFacts::new(state.worktrees[0].id, false, FieldVersion::new(11));
        facts.remote_access.version = FieldVersion::new(2);
        let writes = vec![
            W::PutWorkspace(state.workspaces[0].clone()),
            W::Retained(R::SetTabMembershipVersion(facts.tab_membership_versions[0])),
            W::Retained(R::SetRepositoryInventoryVersion(
                facts.repository_inventory_versions[0],
            )),
            W::Retained(R::SetWorktreeInventoryFacts(facts.worktree_inventory[0])),
            W::Retained(R::SetRemoteAccess(facts.remote_access.clone())),
        ];
        state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
        let mut runtime = initial.runtime().to_vec();
        runtime[0].phase = SessionPhase::Ended {
            reason: EndReason::Stop,
        };
        let changed_runtime = runtime[0];
        let committed =
            Arc::new(OrganizationSnapshot::new(initial.epoch(), state, runtime).unwrap());
        let upstream = Arc::new(
            OrganizationDelta::new(
                initial.state().server,
                initial.epoch(),
                initial.state().revision,
                committed.state().revision,
                writes,
                vec![changed_runtime],
            )
            .unwrap(),
        );
        let mut hub = PublicationHub::new(initial).unwrap();
        hub.committed(&receipt(1, 8), committed.clone(), Some(upstream))
            .unwrap();
        let mut newer = committed.state().clone();
        newer.revision = OrganizationRevision::new(9);
        newer.workspaces[0].name.value.display = NameText::new("Newer".into()).unwrap();
        newer.workspaces[0].name.version = FieldVersion::new(4);
        let next = Arc::new(
            OrganizationDelta::new(
                newer.server,
                committed.epoch(),
                committed.state().revision,
                newer.revision,
                vec![W::PutWorkspace(newer.workspaces[0].clone())],
                vec![],
            )
            .unwrap(),
        );
        hub.committed(
            &receipt(2, 9),
            Arc::new(
                OrganizationSnapshot::new(committed.epoch(), newer, committed.runtime().to_vec())
                    .unwrap(),
            ),
            Some(next),
        )
        .unwrap();
        assert_eq!(Arc::strong_count(&committed), 1);
        let PublicationPlan::Deltas(deltas) = hub.plan_since(Some(OrganizationRevision::new(7)))
        else {
            panic!("retained deltas")
        };
        let expected = committed.state();
        let facts = expected.retained.as_ref().unwrap();
        assert_eq!(
            hub.journal_bytes(),
            deltas
                .iter()
                .map(|v| delta_bytes(v).unwrap())
                .sum::<usize>()
        );
        macro_rules! check {
            ($major:literal, $p:ident, $variant:ident) => {{
                use spaceterm_protocol::{
                    convert::{identity::*, organization::$p as convert},
                    wire::$p as wire,
                };
                let payload = delta_payload($major, &deltas[0], publication_query(), 4096).unwrap();
                let bytes = payload.encode().unwrap();
                assert!(bytes.len() <= 4096);
                let Payload::$variant(message) = Payload::decode($major, &bytes).unwrap() else {
                    panic!("major")
                };
                let Some(wire::message::Kind::OrganizationDelta(value)) = message.kind else {
                    panic!("delta")
                };
                let part = convert::decode_part(value.part.as_ref().unwrap()).unwrap();
                assert_eq!(
                    (
                        part.request,
                        part.generation,
                        part.transfer,
                        part.index,
                        part.total
                    ),
                    (17, 23, 8, 0, 1)
                );
                assert_eq!(
                    (part.base, part.revision),
                    (OrganizationRevision::new(7), OrganizationRevision::new(8))
                );
                assert!(value.complete);
                assert_eq!(
                    convert::decode_workspace_record(&value.workspaces[0]).unwrap(),
                    (
                        expected.workspaces[0].clone(),
                        facts.workspace_names()[0].clone()
                    )
                );
                assert_eq!(
                    convert::decode_tab_record(&value.tabs[0]).unwrap(),
                    (
                        expected.tabs[0].clone(),
                        facts.creation_defaults()[0].clone(),
                        facts.tab_membership_versions()[0]
                    )
                );
                assert_eq!(
                    convert::decode_pane_record(&value.panes[0]).unwrap(),
                    (expected.panes[0].clone(), changed_runtime)
                );
                assert_eq!(
                    decode_repository(value.repositories[0].id.as_ref().unwrap()).unwrap(),
                    expected.repositories[0].id
                );
                assert_eq!(
                    convert::decode_directory(value.repositories[0].directory.as_ref().unwrap())
                        .unwrap(),
                    expected.repositories[0].directory
                );
                assert_eq!(value.repositories[0].inventory_version, 10);
                assert_eq!(
                    decode_worktree(value.worktrees[0].id.as_ref().unwrap()).unwrap(),
                    expected.worktrees[0].id
                );
                assert_eq!(
                    convert::decode_directory(value.worktrees[0].directory_value.as_ref().unwrap())
                        .unwrap(),
                    expected.worktrees[0].directory
                );
                assert!(!value.worktrees[0].listed);
                assert_eq!(value.worktrees[0].version, 11);
                assert_eq!(
                    convert::decode_settings(value.settings.as_ref().unwrap()).unwrap(),
                    (expected.settings.clone(), facts.remote_access().clone())
                );
            }};
        }
        check!(1, v1, Major1);
        check!(2, v2, Major2);
    }
    #[test]
    fn retained_delta_preserves_original_accepted_split_and_deletions_in_both_majors() {
        use OrganizationWrite as W;
        use RetainedOrganizationWrite as R;
        let initial = compact_snapshot(7);
        let mut state = initial.state().clone();
        state.revision = OrganizationRevision::new(8);
        let source = state.panes[1].id;
        let mut created = state.panes[1].clone();
        created.id = PaneId::new(state.server, 5).unwrap();
        created.creation_ordinal = CreationOrdinal::new(7);
        created.creation = PaneCreation::SplitOf {
            source: state.panes[0].id,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::Before,
        };
        state.panes.remove(1);
        state.panes.push(created.clone());
        let mut facts = state.retained.take().unwrap().parts().clone();
        facts.pane_generations.remove(1);
        let marks = PaneGenerationHighWater::new(
            created.id,
            SessionGeneration::new(0),
            ControlGeneration::new(0),
        );
        facts.pane_generations.push(marks);
        facts.creation_defaults[0] =
            TabCreationDefault::new(created.tab, state.panes.iter().map(|v| v.id).collect())
                .unwrap();
        let default = facts.creation_defaults[0].clone();
        state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
        let mut runtime = initial.runtime().to_vec();
        runtime.retain(|v| v.pane != source);
        let created_runtime = PaneRuntimeRecord {
            pane: created.id,
            session_generation: marks.session(),
            control_generation: marks.control(),
            phase: SessionPhase::NotStarted,
            control: ControlRecord::Unclaimed,
        };
        runtime.push(created_runtime);
        let committed =
            Arc::new(OrganizationSnapshot::new(initial.epoch(), state, runtime).unwrap());
        let upstream = OrganizationDelta::new(
            initial.state().server,
            initial.epoch(),
            initial.state().revision,
            committed.state().revision,
            vec![
                W::DeletePane(source),
                W::Retained(R::DeletePaneGenerationHighWater(source)),
                W::PutPane(created.clone()),
                W::Retained(R::PutCreationDefault(default)),
            ],
            vec![created_runtime],
        )
        .unwrap();
        let accepted = AcceptedPaneCreation::new(
            &upstream,
            created.tab,
            created.id,
            created.creation_ordinal,
            PaneCreation::SplitOf {
                source,
                axis: SplitAxis::Vertical,
                edge: SplitEdge::After,
            },
        )
        .unwrap();
        let upstream = upstream.with_accepted_creations(vec![accepted]).unwrap();
        let mut hub = PublicationHub::new(initial).unwrap();
        hub.committed(&receipt(1, 8), committed, Some(Arc::new(upstream)))
            .unwrap();
        let PublicationPlan::Deltas(deltas) = hub.plan_since(Some(OrganizationRevision::new(7)))
        else {
            panic!("delta")
        };
        assert_eq!(deltas[0].accepted_creations(), &[accepted]);
        assert!(deltas[0].writes().contains(&W::DeletePane(source)));
        assert!(
            deltas[0]
                .writes()
                .contains(&W::Retained(R::DeletePaneGenerationHighWater(source)))
        );
        macro_rules! check {
            ($major:literal, $p:ident, $variant:ident) => {{
                use spaceterm_protocol::{
                    convert::{identity::*, organization::$p as convert},
                    wire::$p as wire,
                };
                let payload = delta_payload($major, &deltas[0], publication_query(), 4096).unwrap();
                let Payload::$variant(message) = payload else {
                    panic!("major")
                };
                let Some(wire::message::Kind::OrganizationDelta(value)) = message.kind else {
                    panic!("delta")
                };
                assert_eq!(value.deleted_panes, vec![encode_pane(source)]);
                let actual = &value.accepted_pane_creations[0];
                assert_eq!(
                    decode_pane(actual.source.as_ref().unwrap()).unwrap(),
                    source
                );
                assert_eq!(
                    decode_pane(actual.pane.as_ref().unwrap()).unwrap(),
                    created.id
                );
                assert_eq!(
                    decode_tab(actual.tab.as_ref().unwrap()).unwrap(),
                    created.tab
                );
                assert_eq!(
                    organization::decode_edge(actual.edge).unwrap(),
                    (SplitAxis::Vertical, SplitEdge::After)
                );
                assert_eq!(actual.creation_ordinal, created.creation_ordinal.get());
                assert_eq!(
                    convert::decode_pane_record(&value.panes[0]).unwrap(),
                    (created.clone(), created_runtime)
                );
                let maximum = delta_wire_bound(&deltas[0]).unwrap() - 1;
                assert_eq!(
                    delta_payload($major, &deltas[0], publication_query(), maximum)
                        .unwrap_err()
                        .class,
                    ServerFailureClass::Capacity
                );
            }};
        }
        check!(1, v1, Major1);
        check!(2, v2, Major2);
    }
    #[test]
    fn enriched_record_bytes_can_exceed_journal_capacity_without_retaining_a_delta() {
        let initial = fixtures::snapshot_with_workspaces(140);
        let mut state = initial.state().clone();
        state.revision = OrganizationRevision::new(8);
        let committed = Arc::new(
            OrganizationSnapshot::new(initial.epoch(), state, initial.runtime().to_vec()).unwrap(),
        );
        let upstream = Arc::new(
            OrganizationDelta::new(
                initial.state().server,
                initial.epoch(),
                initial.state().revision,
                committed.state().revision,
                vec![],
                committed.runtime().to_vec(),
            )
            .unwrap(),
        );
        assert!(delta_bytes(&upstream).unwrap() < MAX_JOURNAL_BYTES);
        let mut hub = PublicationHub::new(initial).unwrap();
        hub.committed(&receipt(1, 8), committed.clone(), Some(upstream))
            .unwrap();
        assert_eq!((hub.journal_len(), hub.journal_bytes()), (0, 0));
        assert!(Arc::ptr_eq(&hub.snapshot(), &committed));
        assert!(matches!(
            hub.plan_since(Some(OrganizationRevision::new(7))),
            PublicationPlan::Snapshot(_)
        ));
    }
    #[test]
    fn delta_encoding_rejects_incomplete_runtime_instead_of_silently_omitting_it() {
        let initial = compact_snapshot(7);
        let delta = OrganizationDelta::new(
            initial.state().server,
            initial.epoch(),
            initial.state().revision,
            OrganizationRevision::new(8),
            vec![],
            vec![initial.runtime()[0]],
        )
        .unwrap();
        for major in [1, 2] {
            assert_eq!(
                delta_payload(major, &delta, publication_query(), 4096)
                    .unwrap_err()
                    .class,
                ServerFailureClass::RecoveryRequired
            );
        }
    }
    #[test]
    fn snapshot_gap_and_missing_base_require_resync() {
        let mut hub = PublicationHub::new(snapshot(0)).unwrap();
        hub.committed(&receipt(1, 1), snapshot(1), Some(delta(0, 1)))
            .unwrap();
        assert!(
            matches!(hub.plan_since(Some(OrganizationRevision::new(0))),PublicationPlan::Deltas(v) if v.len()==1)
        );
        assert!(matches!(hub.plan_since(None), PublicationPlan::Snapshot(_)));
        hub.committed(&receipt(2, 2), snapshot(2), None).unwrap();
        assert!(matches!(
            hub.plan_since(Some(OrganizationRevision::new(1))),
            PublicationPlan::Snapshot(_)
        ));
        assert!(
            hub.committed(&receipt(3, 4), snapshot(3), Some(delta(2, 3)))
                .is_err()
        );
        assert_eq!(
            hub.snapshot().state().revision,
            OrganizationRevision::new(2)
        );
    }
    #[test]
    fn publication_journal_is_bounded_and_acknowledgement_does_not_evict() {
        let mut hub = PublicationHub::new(snapshot(0)).unwrap();
        for n in 1..=257 {
            hub.committed(&receipt(n, n), snapshot(n), Some(delta(n - 1, n)))
                .unwrap();
        }
        assert_eq!(hub.journal_len(), 256);
        assert!(matches!(
            hub.plan_since(Some(OrganizationRevision::new(0))),
            PublicationPlan::Snapshot(_)
        ));
        let current = hub.snapshot();
        hub.committed(&receipt(258, 257), current.clone(), None)
            .unwrap();
        assert!(Arc::ptr_eq(&hub.snapshot(), &current));
        assert_eq!(hub.journal_len(), 256);
    }
    #[test]
    fn multipart_snapshot_installs_one_revision_and_management_bound_in_both_majors() {
        let mut hub = PublicationHub::new(snapshot(0)).unwrap();
        for major in [1, 2] {
            let mut transfer = hub.begin_snapshot(major, 1, 1, 64 * 1024).unwrap();
            let page = transfer.next_page().unwrap().unwrap();
            let bytes = page.payload().unwrap().encode().unwrap();
            assert!(bytes.len() <= 64 * 1024);
            let decoded = match page.payload().unwrap() {
                spaceterm_protocol::codec::Payload::Major1(v) => {
                    let spaceterm_protocol::wire::v1::message::Kind::OrganizationSnapshot(v) =
                        v.kind.unwrap()
                    else {
                        panic!("snapshot")
                    };
                    spaceterm_protocol::convert::organization::v1::decode_snapshot_part(&v).unwrap()
                }
                spaceterm_protocol::codec::Payload::Major2(v) => {
                    let spaceterm_protocol::wire::v2::message::Kind::OrganizationSnapshot(v) =
                        v.kind.unwrap()
                    else {
                        panic!("snapshot")
                    };
                    spaceterm_protocol::convert::organization::v2::decode_snapshot_part(&v).unwrap()
                }
            };
            assert_eq!(
                OrganizationSnapshot::new(decoded.part.epoch, decoded.state, decoded.runtime)
                    .unwrap(),
                *hub.snapshot()
            );
            assert!(transfer.next_page().unwrap().is_none());
        }
    }
    #[test]
    fn large_multipart_snapshot_is_installed_atomically_by_real_replica_in_both_majors() {
        use spaceterm_client::{
            limits::ClientLimits,
            replica::{
                OrganizationCursor, OrganizationResidency, PublicationStamp, Replica, ReplicaState,
                ReplicaUpdate, SnapshotPart,
            },
        };
        let snapshot = large_snapshot();
        for major in [1, 2] {
            let mut hub = PublicationHub::new(snapshot.clone()).unwrap();
            let mut replica = Replica::new(
                snapshot.state().server,
                Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
            );
            let state = replica.subscribe();
            let token = replica.begin_resync(snapshot.epoch()).unwrap();
            let mut transfer = hub
                .begin_snapshot(major, 1, token.get(), 64 * 1024)
                .unwrap();
            let mut count = 0;
            while let Some(page) = transfer.next_page().unwrap() {
                let bytes = page.payload().unwrap().encode().unwrap();
                assert!(bytes.len() <= 64 * 1024);
                let decoded = match Payload::decode(major, &bytes).unwrap() {
                    Payload::Major1(v) => {
                        let spaceterm_protocol::wire::v1::message::Kind::OrganizationSnapshot(v) =
                            v.kind.unwrap()
                        else {
                            panic!("snapshot")
                        };
                        organization::v1::decode_snapshot_part(&v).unwrap()
                    }
                    Payload::Major2(v) => {
                        let spaceterm_protocol::wire::v2::message::Kind::OrganizationSnapshot(v) =
                            v.kind.unwrap()
                        else {
                            panic!("snapshot")
                        };
                        organization::v2::decode_snapshot_part(&v).unwrap()
                    }
                };
                let p = decoded.part;
                let update = replica
                    .stage_snapshot(
                        token,
                        SnapshotPart {
                            stamp: PublicationStamp {
                                request: p.request,
                                request_generation: p.generation,
                                transfer: p.transfer,
                                index: p.index,
                                total: p.total,
                                cursor: OrganizationCursor {
                                    server: snapshot.state().server,
                                    epoch: p.epoch,
                                    revision: p.revision,
                                },
                            },
                            state: decoded.state,
                            runtime: decoded.runtime,
                        },
                    )
                    .unwrap();
                if p.index + 1 == p.total {
                    assert_eq!(update, ReplicaUpdate::Installed);
                } else {
                    assert_eq!(update, ReplicaUpdate::Staged);
                    assert!(!state.borrow().is_ready());
                }
                count += 1;
            }
            assert!(count > 1);
            let ReplicaState::Ready {
                snapshot: installed,
            } = state.borrow().clone()
            else {
                panic!("atomic install")
            };
            assert_eq!(installed.organization, *snapshot);
        }
    }
    #[test]
    fn snapshot_planning_includes_the_final_completion_header() {
        let snapshot = snapshot(0);
        for major in [1, 2] {
            let provisional = PublicationPart {
                request: 1,
                generation: 1,
                transfer: 1,
                index: 0,
                total: 4096,
                epoch: snapshot.epoch(),
                base: OrganizationRevision::new(0),
                revision: snapshot.state().revision,
            };
            let maximum = snapshot_payload(major, &snapshot, 0..0, provisional)
                .unwrap()
                .encode()
                .unwrap()
                .len();
            let mut hub = PublicationHub::new(snapshot.clone()).unwrap();
            match hub.begin_snapshot(major, 1, 1, maximum) {
                Ok(mut transfer) => assert!(
                    transfer
                        .next_page()
                        .unwrap()
                        .unwrap()
                        .payload()
                        .unwrap()
                        .encode()
                        .unwrap()
                        .len()
                        <= maximum
                ),
                Err(error) => assert_eq!(error.class, ServerFailureClass::Capacity),
            }
        }
    }
    #[test]
    fn snapshot_egress_rejects_a_foreign_server_namespace() {
        use crate::egress::{ConnectionEgress, EgressBudget};
        use spaceterm_core::{authority::*, identity::*};
        let mut hub = PublicationHub::new(snapshot(0)).unwrap();
        let page = hub
            .begin_snapshot(1, 1, 1, 64 * 1024)
            .unwrap()
            .next_page()
            .unwrap()
            .unwrap();
        let mut queue = ConnectionEgress::new(EgressBudget::new());
        queue
            .enqueue(OutboundMessage::SnapshotPart(page), None)
            .unwrap();
        let server = ServerId::new([2; 16]);
        let client = ClientId::new(server, 1).unwrap();
        let access = ConnectionAccessSnapshot {
            server,
            device: DeviceId::new([3; 16]),
            client,
            pairing: PairingId::new(server, 1).unwrap(),
            binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
            epoch: ServerEpoch::new(1),
            reconciled_revision: OrganizationRevision::new(0),
            effects_enabled: true,
        };
        assert!(queue.take_next(&access).is_none());
        assert_eq!(queue.usage().bytes, 0);
    }
    #[test]
    fn third_historical_snapshot_pin_cancels_the_oldest_transfer() {
        let mut hub = PublicationHub::new(snapshot(0)).unwrap();
        let initial = hub.snapshot();
        let mut oldest = hub.begin_snapshot(1, 1, 1, 64 * 1024).unwrap();
        let old_page = oldest.next_page().unwrap().unwrap();
        hub.committed(&receipt(1, 1), snapshot(1), Some(delta(0, 1)))
            .unwrap();
        let _second = hub.begin_snapshot(1, 2, 1, 64 * 1024).unwrap();
        hub.committed(&receipt(2, 2), snapshot(2), Some(delta(1, 2)))
            .unwrap();
        let _third = hub.begin_snapshot(1, 3, 1, 64 * 1024).unwrap();
        hub.committed(&receipt(3, 3), snapshot(3), Some(delta(2, 3)))
            .unwrap();
        assert!(oldest.next_page().is_err());
        assert_eq!(hub.historical_pins(), 2);
        assert!(!old_page.is_current());
        assert_eq!(Arc::strong_count(&initial), 1);
    }
}
