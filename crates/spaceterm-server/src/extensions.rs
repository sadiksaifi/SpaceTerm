//! Extensions observe committed facts and request further transitions through the
//! actor. They receive no store handle, allocator or publication authority.

use crate::{api::ServerHandle, contracts::ServerFailure};
use spaceterm_core::{
    identity::{ClientId, PairingId},
    observations::ConfirmedDirectoryObservation,
    operation_ledger_contracts::{CanonicalOperationRequest, StreamRecord},
    pairing::{MAX_CLIENTS_PER_PAIRING, PairingClientStreams, PairingOutcome, PairingWrites},
    storage::CommitReceipt,
};

pub trait ExtensionObserverPort: Send + Sync {
    /// Called after durable admission. Scheduling must be bounded and nonblocking.
    /// The extension must commit its Prepared/Started prerequisites through handle
    /// before using external authority. Connection loss does not own this work.
    fn admitted(
        &self,
        request: CanonicalOperationRequest,
        receipt: CommitReceipt,
        handle: ServerHandle,
    ) -> Result<(), ServerFailure>;
    /// Install the committed pairing lookup projection. This is bounded and
    /// nonblocking; it never acquires a second store authority.
    fn pairing_committed(
        &self,
        writes: &PairingWrites,
        receipt: CommitReceipt,
    ) -> Result<(), ServerFailure>;
    /// Read-side notification after the actor's one restoration-directory commit.
    fn confirmed_directory_committed(
        &self,
        observation: &ConfirmedDirectoryObservation,
        receipt: CommitReceipt,
    );
}

/// Pairing policy requests allocation; only the reducer chooses fresh identities.
/// Restricted enrollment/native-owner admission uses its separate purpose-bound
/// provider and cannot borrow this ordinary identified-operation entry point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingAllocationRequest {
    None,
    AdditionalClient(PairingId),
}

pub struct PairingCompletionRequest {
    pub allocation: PairingAllocationRequest,
    pub revoked_clients: Vec<ClientId>,
    pub preparation: Box<dyn PairingCompletionPort>,
}
impl PairingCompletionRequest {
    pub fn validate_bounds(&self) -> Result<(), ServerFailure> {
        if self.revoked_clients.len() > MAX_CLIENTS_PER_PAIRING
            || self
                .revoked_clients
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(crate::failure::failure(
                crate::contracts::ServerFailureClass::Capacity,
            ));
        }
        Ok(())
    }
}

/// A one-use, synchronous policy proposal executed inside the actor's exclusive
/// preparation window. Signing uses injected credential handles; no I/O or wait
/// belongs here. Failure discards only this operation's staged candidate.
pub trait PairingCompletionPort: Send {
    fn prepare(
        &mut self,
        context: PairingCompletionContext<'_>,
    ) -> Result<PreparedPairingCompletion, ServerFailure>;
}
pub struct PairingCompletionContext<'a> {
    pub request: &'a CanonicalOperationRequest,
    pub cursor: crate::adapters::core::CommitCursor,
    pub allocated_client: Option<PairingClientStreams>,
}

/// Bounded core-owned facts. The actor checks matching allocation and stream
/// purpose, then the ledger reserves capacity and completes in one projection.
/// The complete validator verifies receipts and cross-domain policy before SQL.
pub struct PreparedPairingCompletion {
    pub writes: PairingWrites,
    pub streams: Vec<StreamRecord>,
    pub outcome: PairingOutcome,
}
