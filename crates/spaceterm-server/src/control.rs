//! Control orchestration uses the core's admission policy and current reducer
//! records. Disconnect has no control mutation and cannot release a grant.

use crate::api::VerifiedPrincipal;
use crate::contracts::{ServerFailure, ServerFailureClass};
use crate::failure::{control_failure, failure};
use spaceterm_core::{
    control::*,
    control_policy::{self, ControlTransition},
    operations::*,
    organization::*,
};

pub struct ControlRegistry;
impl ControlRegistry {
    pub fn prepare(
        principal: VerifiedPrincipal,
        snapshot: &OrganizationSnapshot,
        command: &Command,
    ) -> Result<ControlTransition, ServerFailure> {
        let pane = match command {
            Command::CompareAcquire(v) => v.pane,
            Command::TakeOver(v) => v.pane,
            Command::ReleaseControl(v) => v.grant.pane(),
            _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
        };
        let current = snapshot
            .runtime()
            .iter()
            .find(|v| v.pane == pane)
            .copied()
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let epoch = snapshot.epoch();
        let client = principal.client();
        let transition = match command {
            Command::CompareAcquire(request) => {
                control_policy::compare_acquire(epoch, client, current, *request)
            }
            Command::TakeOver(request) => {
                control_policy::take_over(epoch, client, current, *request)
            }
            Command::ReleaseControl(request) => {
                control_policy::release_exact(epoch, client, current, *request)
            }
            _ => return Err(failure(ServerFailureClass::ScopeMismatch)),
        };
        transition.map_err(control_failure)
    }
    pub fn compare_acquire(
        principal: VerifiedPrincipal,
        snapshot: &OrganizationSnapshot,
        request: CompareAcquireRequest,
    ) -> Result<ControlTransition, ServerFailure> {
        Self::prepare(principal, snapshot, &Command::CompareAcquire(request))
    }
    pub fn take_over(
        principal: VerifiedPrincipal,
        snapshot: &OrganizationSnapshot,
        request: TakeOverRequest,
    ) -> Result<ControlTransition, ServerFailure> {
        Self::prepare(principal, snapshot, &Command::TakeOver(request))
    }
    pub fn release_exact(
        principal: VerifiedPrincipal,
        snapshot: &OrganizationSnapshot,
        request: ReleaseControlRequest,
    ) -> Result<ControlTransition, ServerFailure> {
        Self::prepare(principal, snapshot, &Command::ReleaseControl(request))
    }
}
