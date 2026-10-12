//! Native Stop requires the provider's original durable Stopped authority.
//! Protocol fields and same-account transport eligibility supply none of it.

use crate::contracts::ServerFailure;
use spaceterm_core::{authority::ServerEpoch, identity::ServerId, native_management::*};
use std::sync::Arc;

pub trait StoppedAuthorizationPort: Send + Sync {
    /// Revalidate the retained native request, fixed action, owning account/build
    /// and durably committed Stopped intent. Native proofs remain provider-owned.
    fn verify_stopped(
        &self,
        server: ServerId,
        epoch: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure>;
}
pub struct TrustedShutdownRequest {
    pub authority: Arc<dyn StoppedAuthorizationPort>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    pub final_store_revision: spaceterm_core::authority::StoreRevision,
    pub final_organization_revision: spaceterm_core::authority::OrganizationRevision,
    pub cleanup_confirmed: bool,
    pub listeners_closed: bool,
    pub started_writes_finished: bool,
    pub recovery_required: bool,
}
