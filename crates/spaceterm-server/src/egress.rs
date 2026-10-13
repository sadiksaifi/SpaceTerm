//! Unsigned per-connection egress. Reservations include encoding scratch and the
//! in-flight transport copy, and survive dequeue until transport completion.

use crate::contracts::{ConnectionAccessSnapshot, OutboundMessage};
use spaceterm_protocol::limits::{
    MAX_EGRESS_BYTES, MAX_EGRESS_ENVELOPES, RESERVED_LIFECYCLE_BYTES, RESERVED_LIFECYCLE_ENVELOPES,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

pub const MAX_GLOBAL_EGRESS_BYTES: usize = 64 * 1024 * 1024;
pub const GLOBAL_LIFECYCLE_RESERVE: usize = 8 * 1024 * 1024;
const SEND_RESIDENCY_MULTIPLIER: usize = 3;
const NO_PROGRESS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EgressFailure {
    Capacity,
    ReservationMismatch,
    Closed,
    Exhausted,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueDisposition {
    Queued,
    Replaced,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EgressUsage {
    pub bytes: usize,
    pub envelopes: usize,
    normal_bytes: usize,
    normal_envelopes: usize,
}

pub struct EgressBudget {
    usage: Mutex<EgressUsage>,
}
impl EgressBudget {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            usage: Mutex::new(EgressUsage::default()),
        })
    }
    pub fn usage(&self) -> EgressUsage {
        self.usage.lock().map(|v| *v).unwrap_or(EgressUsage {
            bytes: MAX_GLOBAL_EGRESS_BYTES,
            envelopes: usize::MAX,
            normal_bytes: MAX_GLOBAL_EGRESS_BYTES,
            normal_envelopes: usize::MAX,
        })
    }
}
struct LocalBudget {
    usage: Mutex<EgressUsage>,
    global: Arc<EgressBudget>,
}
struct Charge {
    budget: Arc<LocalBudget>,
    bytes: usize,
    reliable: bool,
}
impl Drop for Charge {
    fn drop(&mut self) {
        if let (Ok(mut global), Ok(mut local)) =
            (self.budget.global.usage.lock(), self.budget.usage.lock())
        {
            for usage in [&mut *global, &mut *local] {
                usage.bytes -= self.bytes;
                usage.envelopes -= 1;
                if !self.reliable {
                    usage.normal_bytes -= self.bytes;
                    usage.normal_envelopes -= 1;
                }
            }
        }
    }
}
pub struct EgressReservation {
    charge: Arc<Charge>,
    maximum: usize,
}
struct Entry {
    message: Arc<OutboundMessage>,
    charge: Arc<Charge>,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SendId(u64);
pub struct PendingSend {
    id: SendId,
    message: Arc<OutboundMessage>,
    _charge: Arc<Charge>,
}
impl PendingSend {
    pub fn id(&self) -> SendId {
        self.id
    }
    pub fn message(&self) -> &OutboundMessage {
        &self.message
    }
    pub fn maximum_encoded_bytes(&self) -> usize {
        self._charge.bytes / SEND_RESIDENCY_MULTIPLIER
    }
    /// Convert once, transferring the connection/global charge to the DTO owner.
    pub fn snapshot_payload(self) -> Result<SnapshotSend, crate::contracts::ServerFailure> {
        let payload = match self.message() {
            OutboundMessage::SnapshotPart(page) => page.payload()?,
            _ => {
                return Err(crate::failure::failure(
                    crate::contracts::ServerFailureClass::ScopeMismatch,
                ));
            }
        };
        Ok(SnapshotSend {
            payload,
            encoded: None,
            pending: self,
        })
    }
}

/// The converted DTO and its encoding retain the original reservation even
/// after completion or queue teardown. The charge drops after both buffers.
pub struct SnapshotSend {
    payload: spaceterm_protocol::codec::Payload,
    encoded: Option<Vec<u8>>,
    pending: PendingSend,
}
impl SnapshotSend {
    pub fn id(&self) -> SendId {
        self.pending.id()
    }
    pub fn payload(&self) -> &spaceterm_protocol::codec::Payload {
        &self.payload
    }
    /// Borrow the one cached unsigned encoding, without another resident copy.
    pub fn encode(&mut self) -> Result<&[u8], spaceterm_protocol::ProtocolError> {
        if self.encoded.is_none() {
            self.encoded = Some(self.payload.encode()?);
        }
        self.encoded
            .as_deref()
            .ok_or(spaceterm_protocol::ProtocolError::Malformed)
    }
}

pub struct ConnectionEgress {
    budget: Arc<LocalBudget>,
    reliable: VecDeque<Entry>,
    normal: VecDeque<Entry>,
    in_flight: BTreeMap<SendId, Arc<Charge>>,
    sequence: u64,
    closed: bool,
    last_progress: Option<Instant>,
}
impl ConnectionEgress {
    pub fn new(global: Arc<EgressBudget>) -> Self {
        Self {
            budget: Arc::new(LocalBudget {
                usage: Mutex::new(EgressUsage::default()),
                global,
            }),
            reliable: VecDeque::new(),
            normal: VecDeque::new(),
            in_flight: BTreeMap::new(),
            sequence: 0,
            closed: false,
            last_progress: None,
        }
    }
    pub fn usage(&self) -> EgressUsage {
        self.budget.usage.lock().map(|v| *v).unwrap_or(EgressUsage {
            bytes: MAX_EGRESS_BYTES,
            envelopes: MAX_EGRESS_ENVELOPES,
            normal_bytes: MAX_EGRESS_BYTES,
            normal_envelopes: MAX_EGRESS_ENVELOPES,
        })
    }
    fn reserve(&self, maximum: usize, reliable: bool) -> Result<EgressReservation, EgressFailure> {
        if self.closed {
            return Err(EgressFailure::Closed);
        }
        let bytes = maximum
            .checked_mul(SEND_RESIDENCY_MULTIPLIER)
            .filter(|n| *n > 0)
            .ok_or(EgressFailure::Capacity)?;
        let mut global = self
            .budget
            .global
            .usage
            .lock()
            .map_err(|_| EgressFailure::Closed)?;
        let mut local = self
            .budget
            .usage
            .lock()
            .map_err(|_| EgressFailure::Closed)?;
        if bytes > MAX_EGRESS_BYTES.saturating_sub(local.bytes)
            || local.envelopes >= MAX_EGRESS_ENVELOPES
            || bytes > MAX_GLOBAL_EGRESS_BYTES.saturating_sub(global.bytes)
            || !reliable
                && (bytes
                    > (MAX_EGRESS_BYTES - RESERVED_LIFECYCLE_BYTES)
                        .saturating_sub(local.normal_bytes)
                    || local.normal_envelopes
                        >= MAX_EGRESS_ENVELOPES - RESERVED_LIFECYCLE_ENVELOPES
                    || bytes
                        > (MAX_GLOBAL_EGRESS_BYTES - GLOBAL_LIFECYCLE_RESERVE)
                            .saturating_sub(global.normal_bytes))
        {
            return Err(EgressFailure::Capacity);
        }
        for usage in [&mut *global, &mut *local] {
            usage.bytes += bytes;
            usage.envelopes += 1;
            if !reliable {
                usage.normal_bytes += bytes;
                usage.normal_envelopes += 1;
            }
        }
        drop(local);
        drop(global);
        Ok(EgressReservation {
            charge: Arc::new(Charge {
                budget: self.budget.clone(),
                bytes,
                reliable,
            }),
            maximum,
        })
    }
    pub fn reserve_reliable(
        &mut self,
        maximum_encoded_bytes: usize,
    ) -> Result<EgressReservation, EgressFailure> {
        self.reserve(maximum_encoded_bytes, true)
    }
    pub fn enqueue(
        &mut self,
        message: OutboundMessage,
        reservation: Option<EgressReservation>,
    ) -> Result<EnqueueDisposition, EgressFailure> {
        if self.closed {
            return Err(EgressFailure::Closed);
        }
        let (reliable, maximum) = policy(&message);
        if let Some(reservation) = &reservation
            && (!Arc::ptr_eq(&reservation.charge.budget, &self.budget)
                || reservation.charge.reliable != reliable
                || reservation.maximum < maximum)
        {
            return Err(EgressFailure::ReservationMismatch);
        }
        if reservation.is_none()
            && let OutboundMessage::ContentInvalidated(session) = &message
            && let Some(entry) = self.normal.iter_mut().find(|entry| {
                matches!(entry.message.as_ref(),OutboundMessage::ContentInvalidated(old) if old==session)
            })
        {
            // Replacement consumes the existing unsigned obligation. A
            // saturated queue cannot lose its only invalidation on failure.
            entry.message = Arc::new(message);
            return Ok(EnqueueDisposition::Replaced);
        }
        let reservation = match reservation {
            Some(reservation) => reservation,
            None => self.reserve(maximum, reliable)?,
        };
        if self.reliable.is_empty() && self.normal.is_empty() && self.in_flight.is_empty() {
            self.last_progress = Some(Instant::now());
        }
        let entry = Entry {
            message: Arc::new(message),
            charge: reservation.charge,
        };
        if reliable {
            self.reliable.push_back(entry);
        } else {
            self.normal.push_back(entry);
        }
        Ok(EnqueueDisposition::Queued)
    }
    pub fn take_next(&mut self, access: &ConnectionAccessSnapshot) -> Option<PendingSend> {
        if self.closed {
            return None;
        }
        loop {
            let Some(entry) = self
                .reliable
                .pop_front()
                .or_else(|| self.normal.pop_front())
            else {
                if self.in_flight.is_empty() {
                    self.last_progress = None;
                }
                return None;
            };
            let eligible = match entry.message.as_ref() {
                OutboundMessage::ContentInvalidated(session) => {
                    access.effects_enabled
                        && session.epoch == access.epoch
                        && session.pane.server() == access.server
                }
                OutboundMessage::SnapshotPart(page) => {
                    page.is_current()
                        && page.server() == access.server
                        && page.part().epoch == access.epoch
                }
                OutboundMessage::Outcome { binding, .. }
                | OutboundMessage::PendingRetirement { binding, .. } => {
                    binding.id.stream().client() == access.client
                        && binding.id.stream().pairing() == access.pairing
                }
                OutboundMessage::ResyncRequired { epoch, .. } => *epoch == access.epoch,
                OutboundMessage::Status(publication) => match publication {
                    spaceterm_core::management::StatusPublication::Snapshot { status, .. }
                    | spaceterm_core::management::StatusPublication::Changed { status, .. } => {
                        status.parts().server == access.server
                            && status.parts().epoch == access.epoch
                    }
                    spaceterm_core::management::StatusPublication::ResyncRequired { .. } => true,
                },
            };
            if !eligible {
                continue;
            }
            let Some(sequence) = self.sequence.checked_add(1) else {
                self.close();
                return None;
            };
            self.sequence = sequence;
            let id = SendId(sequence);
            self.in_flight.insert(id, entry.charge.clone());
            return Some(PendingSend {
                id,
                message: entry.message,
                _charge: entry.charge,
            });
        }
    }
    pub fn complete_send(&mut self, send: SendId) {
        if self.in_flight.remove(&send).is_some() {
            self.progress_at(Instant::now());
        }
    }
    /// Record actual transport write progress, not enqueue or receive activity.
    pub fn progress_at(&mut self, tick: Instant) {
        if self.reliable.is_empty() && self.normal.is_empty() && self.in_flight.is_empty() {
            self.last_progress = None;
        } else if self.last_progress.is_none_or(|previous| tick > previous) {
            self.last_progress = Some(tick);
        }
    }
    pub fn stalled_at(&self, tick: Instant) -> bool {
        self.last_progress.is_some_and(|previous| {
            tick.saturating_duration_since(previous) >= NO_PROGRESS_DEADLINE
        })
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.reliable.clear();
        self.normal.clear();
        self.in_flight.clear();
        self.last_progress = None;
    }
}
fn policy(message: &OutboundMessage) -> (bool, usize) {
    match message {
        OutboundMessage::SnapshotPart(page) => (false, page.maximum_encoded_bytes()),
        OutboundMessage::Outcome { .. } => (true, spaceterm_protocol::limits::MAX_MANAGEMENT_BYTES),
        OutboundMessage::PendingRetirement { .. } | OutboundMessage::ResyncRequired { .. } => {
            (true, 512)
        }
        OutboundMessage::Status(_) => (false, spaceterm_protocol::limits::MAX_MANAGEMENT_BYTES),
        OutboundMessage::ContentInvalidated(_) => (false, 512),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::{authority::*, identity::*};
    fn session(n: u64) -> SessionKey {
        SessionKey {
            epoch: ServerEpoch::new(1),
            pane: PaneId::new(ServerId::new([1; 16]), n).unwrap(),
            generation: SessionGeneration::new(1),
        }
    }
    fn access() -> ConnectionAccessSnapshot {
        let server = ServerId::new([1; 16]);
        let client = ClientId::new(server, 1).unwrap();
        ConnectionAccessSnapshot {
            server,
            device: DeviceId::new([2; 16]),
            pairing: PairingId::new(server, 1).unwrap(),
            client,
            binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
            epoch: ServerEpoch::new(1),
            reconciled_revision: OrganizationRevision::new(1),
            effects_enabled: true,
        }
    }
    #[test]
    fn lifecycle_reserve_survives_presentation_saturation_and_in_flight_send_remains_charged() {
        let global = EgressBudget::new();
        let mut queue = ConnectionEgress::new(global.clone());
        for n in 1..=120 {
            queue
                .enqueue(OutboundMessage::ContentInvalidated(session(n)), None)
                .unwrap();
        }
        assert_eq!(
            queue.enqueue(OutboundMessage::ContentInvalidated(session(121)), None),
            Err(EgressFailure::Capacity)
        );
        let reservation = queue.reserve_reliable(1024).unwrap();
        queue
            .enqueue(
                OutboundMessage::ResyncRequired {
                    epoch: ServerEpoch::new(1),
                    revision: OrganizationRevision::new(2),
                },
                Some(reservation),
            )
            .unwrap();
        assert_eq!(queue.usage().envelopes, 121);
        let send = queue.take_next(&access()).unwrap();
        assert!(matches!(
            send.message(),
            OutboundMessage::ResyncRequired { .. }
        ));
        assert_eq!(queue.usage().envelopes, 121);
        queue.complete_send(send.id());
        assert_eq!(queue.usage().envelopes, 121);
        drop(send);
        assert_eq!(queue.usage().envelopes, 120);
        drop(queue);
        assert_eq!(global.usage().bytes, 0);
    }
    #[test]
    fn coalescing_only_removes_unsigned_obsolete_presentation_and_purges_ineligible_content() {
        let mut queue = ConnectionEgress::new(EgressBudget::new());
        queue
            .enqueue(OutboundMessage::ContentInvalidated(session(1)), None)
            .unwrap();
        assert_eq!(
            queue.enqueue(OutboundMessage::ContentInvalidated(session(1)), None),
            Ok(EnqueueDisposition::Replaced)
        );
        assert_eq!(queue.usage().envelopes, 1);
        let mut revoked = access();
        revoked.effects_enabled = false;
        assert!(queue.take_next(&revoked).is_none());
        assert_eq!(queue.usage().bytes, 0);
    }
    #[test]
    fn foreign_reservation_and_limit_plus_one_never_create_an_egress_obligation() {
        let global = EgressBudget::new();
        let mut first = ConnectionEgress::new(global.clone());
        let mut second = ConnectionEgress::new(global.clone());
        let reservation = first.reserve_reliable(1024).unwrap();
        assert_eq!(
            second.enqueue(
                OutboundMessage::ResyncRequired {
                    epoch: ServerEpoch::new(1),
                    revision: OrganizationRevision::new(2)
                },
                Some(reservation)
            ),
            Err(EgressFailure::ReservationMismatch)
        );
        assert_eq!(global.usage().bytes, 0);
        let _reservation = first.reserve_reliable(1024).unwrap();
        for _ in 0..127 {
            let reservation = first.reserve_reliable(1024).unwrap();
            first
                .enqueue(
                    OutboundMessage::ResyncRequired {
                        epoch: ServerEpoch::new(1),
                        revision: OrganizationRevision::new(2),
                    },
                    Some(reservation),
                )
                .unwrap();
        }
        assert_eq!(
            first.reserve_reliable(1024).err(),
            Some(EgressFailure::Capacity)
        );
        assert_eq!(first.usage().envelopes, 128);
    }
    #[test]
    fn egress_exact_byte_limit_and_limit_plus_one() {
        let global = EgressBudget::new();
        let mut queue = ConnectionEgress::new(global.clone());
        let maximum = MAX_EGRESS_BYTES / SEND_RESIDENCY_MULTIPLIER;
        let reservation = queue.reserve_reliable(maximum).unwrap();
        assert_eq!(queue.usage().bytes, maximum * SEND_RESIDENCY_MULTIPLIER);
        assert_eq!(
            queue.reserve_reliable(1).err(),
            Some(EgressFailure::Capacity)
        );
        drop(reservation);
        assert_eq!(
            queue.reserve_reliable(maximum + 1).err(),
            Some(EgressFailure::Capacity)
        );
        assert_eq!(global.usage().bytes, 0);
    }
    #[test]
    fn global_egress_limit_is_enforced_across_connection_budgets() {
        let global = EgressBudget::new();
        let mut reservations = Vec::new();
        let maximum = MAX_EGRESS_BYTES / SEND_RESIDENCY_MULTIPLIER;
        for _ in 0..16 {
            let mut queue = ConnectionEgress::new(global.clone());
            reservations.push(queue.reserve_reliable(maximum).unwrap());
        }
        let mut last = ConnectionEgress::new(global.clone());
        reservations.push(last.reserve_reliable(5).unwrap());
        assert_eq!(global.usage().bytes, MAX_GLOBAL_EGRESS_BYTES - 1);
        assert_eq!(
            last.reserve_reliable(1).err(),
            Some(EgressFailure::Capacity)
        );
        drop(reservations);
        assert_eq!(global.usage().bytes, 0);
    }
    #[test]
    fn global_lifecycle_reserve_survives_normal_byte_saturation() {
        let global = EgressBudget::new();
        let mut reservations = Vec::new();
        let normal = MAX_EGRESS_BYTES - RESERVED_LIFECYCLE_BYTES;
        for _ in 0..14 {
            let queue = ConnectionEgress::new(global.clone());
            reservations.push(queue.reserve(normal / 3, false).unwrap());
        }
        let mut queue = ConnectionEgress::new(global.clone());
        let remaining = MAX_GLOBAL_EGRESS_BYTES - GLOBAL_LIFECYCLE_RESERVE - global.usage().bytes;
        reservations.push(queue.reserve(remaining / 3, false).unwrap());
        assert_eq!(queue.reserve(1, false).err(), Some(EgressFailure::Capacity));
        let reliable = queue.reserve_reliable(64 * 1024).unwrap();
        assert!(global.usage().bytes > MAX_GLOBAL_EGRESS_BYTES - GLOBAL_LIFECYCLE_RESERVE);
        drop(reliable);
        drop(reservations);
        assert_eq!(global.usage().bytes, 0);
    }
    #[test]
    fn queue_close_retains_the_charge_of_an_unfinished_native_send() {
        let global = EgressBudget::new();
        let mut queue = ConnectionEgress::new(global.clone());
        queue
            .enqueue(OutboundMessage::ContentInvalidated(session(1)), None)
            .unwrap();
        let send = queue.take_next(&access()).unwrap();
        queue.close();
        drop(queue);
        assert_ne!(global.usage().bytes, 0);
        drop(send);
        assert_eq!(global.usage().bytes, 0);
    }
    #[tokio::test(start_paused = true)]
    async fn stalled_pressure_is_connection_local_and_idle_connections_have_no_deadline() {
        let global = EgressBudget::new();
        let mut stalled = ConnectionEgress::new(global.clone());
        let mut healthy = ConnectionEgress::new(global);
        tokio::time::advance(std::time::Duration::from_secs(100)).await;
        assert!(!healthy.stalled_at(Instant::now()));
        stalled
            .enqueue(OutboundMessage::ContentInvalidated(session(1)), None)
            .unwrap();
        healthy
            .enqueue(OutboundMessage::ContentInvalidated(session(2)), None)
            .unwrap();
        // A new obligation after long idle starts its own five-second deadline.
        assert!(!healthy.stalled_at(Instant::now()));
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        healthy.progress_at(Instant::now());
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(stalled.stalled_at(Instant::now()));
        assert!(!healthy.stalled_at(Instant::now()));
        let send = healthy.take_next(&access()).unwrap();
        healthy.complete_send(send.id());
        tokio::time::advance(std::time::Duration::from_secs(100)).await;
        assert!(!healthy.stalled_at(Instant::now()));
    }
}
