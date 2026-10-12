//! Pure control admission. The actor supplies authenticated identity and the
//! reducer's current record. A transition is a proposal, never a live grant.

use crate::authority::{ControlGeneration, ControlGrant, GrantedControl, ServerEpoch};
use crate::control::{CompareAcquireRequest, ReleaseControlRequest, TakeOverRequest};
use crate::identity::ClientId;
use crate::organization::{ControlRecord, PaneRuntimeRecord};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlPolicyError {
    ScopeMismatch,
    StaleEpoch,
    StaleComparison,
    NotOwner,
    Exhausted,
    InvalidState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlTransition {
    epoch: ServerEpoch,
    expected: PaneRuntimeRecord,
    next: PaneRuntimeRecord,
}
impl ControlTransition {
    pub const fn epoch(self) -> ServerEpoch {
        self.epoch
    }
    pub const fn expected(self) -> PaneRuntimeRecord {
        self.expected
    }
    pub const fn next(self) -> PaneRuntimeRecord {
        self.next
    }
    pub fn granted(self) -> Option<GrantedControl> {
        match self.next.control {
            ControlRecord::Unclaimed => None,
            ControlRecord::Held { client, generation } => {
                ControlGrant::new(self.next.pane, client, generation)
                    .and_then(|grant| GrantedControl::new(self.epoch, grant))
                    .ok()
            }
        }
    }
}

pub fn compare_acquire(
    epoch: ServerEpoch,
    client: ClientId,
    current: PaneRuntimeRecord,
    request: CompareAcquireRequest,
) -> Result<ControlTransition, ControlPolicyError> {
    validate(
        epoch,
        request.epoch,
        client,
        request.client,
        current,
        request.pane,
    )?;
    if current.control != ControlRecord::Unclaimed
        || current.control_generation != request.expected_generation
    {
        return Err(ControlPolicyError::StaleComparison);
    }
    hold(epoch, client, current)
}

pub fn take_over(
    epoch: ServerEpoch,
    client: ClientId,
    current: PaneRuntimeRecord,
    request: TakeOverRequest,
) -> Result<ControlTransition, ControlPolicyError> {
    validate(
        epoch,
        request.epoch,
        client,
        request.client,
        current,
        request.pane,
    )?;
    hold(epoch, client, current)
}

pub fn release_exact(
    epoch: ServerEpoch,
    client: ClientId,
    current: PaneRuntimeRecord,
    request: ReleaseControlRequest,
) -> Result<ControlTransition, ControlPolicyError> {
    validate(
        epoch,
        request.epoch,
        client,
        request.grant.client(),
        current,
        request.grant.pane(),
    )?;
    if current.control
        != (ControlRecord::Held {
            client,
            generation: request.grant.generation(),
        })
    {
        return Err(ControlPolicyError::StaleComparison);
    }
    let next = PaneRuntimeRecord {
        control: ControlRecord::Unclaimed,
        control_generation: successor(current.control_generation)?,
        ..current
    };
    Ok(ControlTransition {
        epoch,
        expected: current,
        next,
    })
}

fn validate(
    epoch: ServerEpoch,
    requested_epoch: ServerEpoch,
    client: ClientId,
    claimed: ClientId,
    current: PaneRuntimeRecord,
    pane: crate::identity::PaneId,
) -> Result<(), ControlPolicyError> {
    if client != claimed || pane != current.pane || pane.server() != client.server() {
        return Err(ControlPolicyError::ScopeMismatch);
    }
    if epoch.get() == 0 || requested_epoch != epoch {
        return Err(ControlPolicyError::StaleEpoch);
    }
    if matches!(current.control, ControlRecord::Held { client, generation } if client.server() != pane.server() || generation.get() == 0 || generation != current.control_generation)
    {
        return Err(ControlPolicyError::InvalidState);
    }
    Ok(())
}

fn hold(
    epoch: ServerEpoch,
    client: ClientId,
    current: PaneRuntimeRecord,
) -> Result<ControlTransition, ControlPolicyError> {
    let generation = successor(current.control_generation)?;
    let next = PaneRuntimeRecord {
        control: ControlRecord::Held { client, generation },
        control_generation: generation,
        ..current
    };
    Ok(ControlTransition {
        epoch,
        expected: current,
        next,
    })
}

fn successor(current: ControlGeneration) -> Result<ControlGeneration, ControlPolicyError> {
    current
        .checked_successor()
        .ok_or(ControlPolicyError::Exhausted)
}
