//! Committed organization publication. Connections share the current immutable
//! snapshot and a bounded delta journal; they never own authoritative state.

use crate::{contracts::*, failure::failure};
use spaceterm_core::{
    authority::{OrganizationRevision, StoreRevision},
    organization::{OrganizationDelta, OrganizationSnapshot, OrganizationWrite},
    storage::CommitReceipt,
};
use spaceterm_protocol::{
    codec::Payload,
    convert::organization::{self, PublicationPart},
};
use std::{
    collections::{BTreeMap, VecDeque},
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
        let bytes = delta.as_deref().map(delta_bytes).transpose()?;
        self.store = Some(receipt.store_revision);
        self.snapshot = snapshot;
        match (delta, bytes) {
            (Some(delta), Some(bytes)) if bytes <= MAX_JOURNAL_BYTES => {
                while self.journal.len() >= MAX_JOURNAL_DELTAS
                    || self.journal_bytes > MAX_JOURNAL_BYTES - bytes
                {
                    if let Some(entry) = self.journal.pop_front() {
                        self.journal_bytes -= entry.bytes;
                    } else {
                        break;
                    }
                }
                // Clone only after length admission. The journal does not retain an
                // upstream Vec or String's excess allocation capacity.
                self.journal.push_back(JournalEntry {
                    delta: Arc::new(delta.as_ref().clone()),
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
    use spaceterm_core::{authority::ServerEpoch, identity::ServerId};

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
