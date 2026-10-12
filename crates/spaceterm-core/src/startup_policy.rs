//! Explicit-use comparisons shared by the server and runtime admission.
//! Measurements complete a retained intent; they cannot create one.

use crate::authority::{ConnectionBinding, ContextGeneration, ServerEpoch};
use crate::control::{MeasuredStartupMeasurement, StartupIntent, StartupSource};
use crate::geometry::{GridLimits, MeasuredGrid};
use crate::identity::TabId;
use crate::organization::{ControlRecord, PaneRuntimeRecord, SessionPhase};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPolicyError {
    AdmissionsClosed,
    ScopeMismatch,
    StaleAuthority,
    AlreadyStarted,
    InvalidGeometry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StartupContext {
    pub epoch: ServerEpoch,
    pub runtime: PaneRuntimeRecord,
    pub tab: TabId,
    pub connection: ConnectionBinding,
    pub context: ContextGeneration,
    pub admissions_open: bool,
}

pub fn validate_explicit_use(
    intent: StartupIntent,
    current: StartupContext,
) -> Result<(), StartupPolicyError> {
    if !current.admissions_open {
        return Err(StartupPolicyError::AdmissionsClosed);
    }
    let fence = intent.fence();
    let session = fence.session();
    if session.pane != current.runtime.pane
        || current.tab.server() != session.pane.server()
        || intent.id().stream().client() != current.connection.client
        || matches!(intent.source(), StartupSource::TabOpening { tab } if tab != current.tab)
    {
        return Err(StartupPolicyError::ScopeMismatch);
    }
    if current.epoch.get() == 0
        || session.epoch != current.epoch
        || session.generation != current.runtime.session_generation
        || fence.connection() != current.connection
        || current.connection.generation.get() == 0
        || fence.context() != current.context
        || current.context.get() == 0
        || fence.content().is_some()
        || fence.grant().generation().get() == 0
        || current.runtime.control_generation != fence.grant().generation()
        || current.runtime.control
            != (ControlRecord::Held {
                client: fence.grant().client(),
                generation: fence.grant().generation(),
            })
    {
        return Err(StartupPolicyError::StaleAuthority);
    }
    if matches!(
        current.runtime.phase,
        SessionPhase::Starting { .. } | SessionPhase::Running { .. }
    ) {
        return Err(StartupPolicyError::AlreadyStarted);
    }
    Ok(())
}

pub fn validate_measurement(
    intent: StartupIntent,
    measurement: MeasuredStartupMeasurement,
    current: StartupContext,
    limits: GridLimits,
) -> Result<MeasuredGrid, StartupPolicyError> {
    validate_explicit_use(intent, current)?;
    if measurement.intent != intent.id() || !measurement.fence.matches(&intent.fence()) {
        return Err(StartupPolicyError::StaleAuthority);
    }
    measurement
        .grid
        .validate(limits)
        .map_err(|_| StartupPolicyError::InvalidGeometry)
}
