//! Pure pairing proposals. Only the serialized actor can turn these into effects.

use super::{PairingOutcome, PairingWrites};
use crate::authority::StoreRevision;
use crate::identity::{ClientId, IdentityHighWater, OperationStreamId};
use crate::operation_ledger_contracts::StreamRecord;

/// The actor merges these requirements with ledger completion and exact reducer
/// control release in one transaction. Stream registration must pass the ledger's
/// dormant-capacity admission before signing or publishing a binding.
#[derive(Clone)]
pub struct PairingTransition {
    pub expected_store: StoreRevision,
    pub high_water: IdentityHighWater,
    pub writes: PairingWrites,
    pub register_streams: Vec<StreamRecord>,
    pub retire_ordinary_streams: Vec<OperationStreamId>,
    pub release_clients: Vec<ClientId>,
    pub outcome: PairingOutcome,
}

/// Actor-owned housekeeping, not a fabricated identified operation. Commit the
/// terminal ticket phases and retire their Enrollment-purpose headers together.
/// Retired headers and any admitted/unacknowledged result remain retained. The
/// ledger alone decides reservation accounting; invalidation never evicts proof.
#[derive(Clone)]
pub struct PairingTicketInvalidations {
    pub expected_store: StoreRevision,
    pub writes: PairingWrites,
    pub retire_streams: Vec<OperationStreamId>,
}
