//! SSH/native consumer for closed live lifecycle comparisons.
//!
//! The actual paired actor owns assessment and admission. This producer cannot
//! obtain that authority from an SSH account, public reference, or owner channel.
//! It uses the native supervisor to observe, capture, recover, and resolve the
//! exact signed original, including lease-validated non-admission cleanup.

use std::{num::NonZeroU64, sync::Arc, time::Duration};

use spaceterm_host::{
    account_service::{
        NativeLifecycleHandoff, NativeLifecycleOwnerToken, NativeLifecycleRequest,
        RegisteredServer, ServiceFailure, ServicePhase, ServiceProgress, ServiceRequestState,
        ServiceSupervisor,
        remote_wire::{
            NativeLifecycleAccountReply, NativeLifecycleAccountRequest, NativeLifecycleObservation,
            NativeLifecycleWireFailure,
        },
    },
    startup::AccountBuildKey,
};
use spaceterm_protocol::authentication::ServerPin;
use tokio::{sync::watch, task::JoinHandle};

/// Handles one validated, pathless request without owning the helper's lifetime.
/// Framed SSH helpers must use [`NativeLifecycleOriginalOwners::handle`] for
/// Captured and Restored owner-ready replies. No operation runs before checking
/// the guest's own key and retained pin.
pub async fn handle(
    supervisor: &ServiceSupervisor,
    request: &NativeLifecycleAccountRequest,
) -> NativeLifecycleAccountReply {
    match perform(supervisor, request).await {
        Ok(reply) => match reply.validate() {
            Ok(()) => reply,
            Err(error) => NativeLifecycleAccountReply::Refused(wire_failure(error)),
        },
        Err(error) => NativeLifecycleAccountReply::Refused(error),
    }
}

fn validated_registration(
    supervisor: &ServiceSupervisor,
    request: &NativeLifecycleAccountRequest,
) -> Result<RegisteredServer, ServiceFailure> {
    request.validate().map_err(wire_failure)?;
    let (expected_key, expected_pin) = request.expected_identity();
    if supervisor.key() != expected_key {
        return Err(ServiceFailure::IdentityMismatch);
    }
    let original_request = match request {
        NativeLifecycleAccountRequest::Capture { request, .. }
        | NativeLifecycleAccountRequest::Resolve { request, .. } => Some(*request),
        NativeLifecycleAccountRequest::Restore { original, .. } => Some(original.request),
        NativeLifecycleAccountRequest::Acknowledge { progress, .. } => Some(progress.request),
        NativeLifecycleAccountRequest::Observe { .. }
        | NativeLifecycleAccountRequest::Reserve { .. } => None,
    };
    if let Some(original_request) = original_request {
        // Validate exact native identity before reconciling only this original
        // storage publication. This can recover the same supervisor's latch
        // without admitting work or refreshing its capture.
        supervisor
            .reconcile_runtime_lifecycle_storage(expected_key, expected_pin, original_request)
            .map_err(|error| {
                if matches!(request, NativeLifecycleAccountRequest::Resolve { .. }) {
                    unassessed_pending(error)
                } else {
                    error
                }
            })?;
    }
    let registration = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
    if registration.key() != expected_key || registration.server_pin() != expected_pin {
        return Err(ServiceFailure::IdentityMismatch);
    }
    Ok(registration)
}

async fn perform(
    supervisor: &ServiceSupervisor,
    request: &NativeLifecycleAccountRequest,
) -> Result<NativeLifecycleAccountReply, ServiceFailure> {
    let registration = validated_registration(supervisor, request)?;
    let (expected_key, expected_pin) = request.expected_identity();
    match request {
        NativeLifecycleAccountRequest::Observe { .. } => {
            let status = supervisor.status().await?;
            // Status and generation must come from the same registration. A
            // replacement during the manager observation is not a fresh capture.
            let current = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
            if current.key() != expected_key
                || current.server_pin() != expected_pin
                || current.generation() != registration.generation()
                || status.key != expected_key
                || status.server.map(|server| server.bytes()) != Some(expected_pin.identity)
            {
                return Err(ServiceFailure::IdentityMismatch);
            }
            let registration_generation =
                NonZeroU64::new(current.generation()).ok_or(ServiceFailure::InvalidRecord)?;
            Ok(NativeLifecycleAccountReply::Observed(
                NativeLifecycleObservation {
                    status,
                    registration_generation,
                },
            ))
        }
        NativeLifecycleAccountRequest::Reserve { .. } => Ok(NativeLifecycleAccountReply::Reserved(
            supervisor.allocate_request()?,
        )),
        NativeLifecycleAccountRequest::Capture {
            request, action, ..
        } => Ok(NativeLifecycleAccountReply::Captured(
            supervisor.capture_runtime_request(*request, *action)?,
        )),
        NativeLifecycleAccountRequest::Restore {
            original,
            assessment,
            ..
        } => {
            let restored = supervisor.restore_runtime_request(original.request)?;
            if restored != *original {
                return Err(ServiceFailure::RequestConflict);
            }
            let retained = match supervisor.restore_runtime_lifecycle_reference(restored) {
                Ok(reference) => Some(reference),
                Err(ServiceFailure::RequestConflict) => None,
                Err(error) => return Err(error),
            };
            if assessment
                .as_ref()
                .is_some_and(|expected| retained.as_ref() != Some(expected))
            {
                return Err(ServiceFailure::RequestConflict);
            }
            Ok(NativeLifecycleAccountReply::Restored {
                original: restored,
                assessment: retained,
            })
        }
        NativeLifecycleAccountRequest::Resolve { request, .. } => {
            // A signed original is required even for read-only Reserved progress.
            // This excludes owner, Start, Stage, install, and unsigned native work.
            // Uninstall still requires this same actual paired admission.
            Ok(NativeLifecycleAccountReply::Progress(
                supervisor
                    .resolve_runtime_lifecycle(expected_key, expected_pin, *request)
                    .await
                    .map_err(unassessed_pending)?,
            ))
        }
        NativeLifecycleAccountRequest::Acknowledge { progress, .. } => {
            supervisor.acknowledge_runtime_lifecycle_exact(progress)?;
            Ok(NativeLifecycleAccountReply::Acknowledged {
                progress: progress.clone(),
            })
        }
    }
}

fn unassessed_pending(error: ServiceFailure) -> ServiceFailure {
    match error {
        ServiceFailure::RequestConflict => ServiceFailure::Pending,
        other => other,
    }
}

fn wire_failure(error: NativeLifecycleWireFailure) -> ServiceFailure {
    match error {
        NativeLifecycleWireFailure::LimitExceeded => ServiceFailure::LimitExceeded,
        NativeLifecycleWireFailure::InvalidRecord => ServiceFailure::InvalidRecord,
    }
}

/// Maximum simultaneous original workers owned by one account helper.
pub const NATIVE_LIFECYCLE_OWNER_LIMIT: usize = 32;
const OWNER_READY_BUDGET: Duration = Duration::from_secs(5);
const OWNER_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Owns original lifecycle workers outside the running account server.
///
/// Captured and Restored replies are released only after a worker has verified
/// the exact native handoff. The helper must protect its process from SSH HUP
/// before calling `handle`, then call `finish` after EOF or any reply error and
/// before shutting down its Tokio runtime or deleting its staged executable.
/// Dropping this value is not a substitute for joining the workers.
pub struct NativeLifecycleOriginalOwners {
    supervisor: Arc<ServiceSupervisor>,
    originals: Vec<OriginalOwner>,
    failure: Option<ServiceFailure>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct OriginalScope {
    key: AccountBuildKey,
    pin: ServerPin,
    original: NativeLifecycleRequest,
}

struct OriginalOwner {
    scope: OriginalScope,
    ready: watch::Receiver<bool>,
    terminal: watch::Sender<Option<ServiceProgress>>,
    task: JoinHandle<Result<(), ServiceFailure>>,
}

impl NativeLifecycleOriginalOwners {
    /// Creates a bounded, ephemeral owner set over the actual native provider.
    pub fn new(supervisor: Arc<ServiceSupervisor>) -> Self {
        Self {
            supervisor,
            originals: Vec::new(),
            failure: None,
        }
    }

    /// Handles a closed request and arms its exact original before a ready reply.
    /// No admission, actor principal or replacement original is created here.
    pub async fn handle(
        &mut self,
        request: &NativeLifecycleAccountRequest,
    ) -> NativeLifecycleAccountReply {
        self.reap_finished().await;
        if let Err(error) = validated_registration(&self.supervisor, request) {
            return NativeLifecycleAccountReply::Refused(error);
        }
        let (key, pin) = request.expected_identity();
        let original_request = match request {
            NativeLifecycleAccountRequest::Capture { request, .. }
            | NativeLifecycleAccountRequest::Resolve { request, .. } => Some(*request),
            NativeLifecycleAccountRequest::Restore { original, .. } => Some(original.request),
            _ => None,
        };
        if original_request.is_some_and(|request| {
            !self
                .originals
                .iter()
                .any(|owner| owner.scope.original.request == request)
        }) && self.originals.len() >= NATIVE_LIFECYCLE_OWNER_LIMIT
        {
            return NativeLifecycleAccountReply::Refused(ServiceFailure::LimitExceeded);
        }
        // Resolve can await manager effects. Arm before entering that future so
        // cancellation of the frame handler cannot abandon an admitted original.
        if let NativeLifecycleAccountRequest::Resolve { request, .. } = request {
            let original = match self.supervisor.restore_runtime_request(*request) {
                Ok(original) => original,
                Err(error) => {
                    return NativeLifecycleAccountReply::Refused(unassessed_pending(error));
                }
            };
            if let Err(error) = self.arm(OriginalScope { key, pin, original }).await {
                return NativeLifecycleAccountReply::Refused(error);
            }
        }
        let reply = handle(&self.supervisor, request).await;
        match &reply {
            NativeLifecycleAccountReply::Captured(original)
            | NativeLifecycleAccountReply::Restored { original, .. } => {
                if let Err(error) = self
                    .arm(OriginalScope {
                        key,
                        pin,
                        original: *original,
                    })
                    .await
                {
                    return NativeLifecycleAccountReply::Refused(error);
                }
            }
            NativeLifecycleAccountReply::Progress(progress)
            | NativeLifecycleAccountReply::Acknowledged { progress }
                if progress.phase == ServicePhase::Completed =>
            {
                // Both replies came from exact native verification above. Keep
                // that proof if ACK wins the race with the worker's next poll.
                for owner in &self.originals {
                    if owner.scope.key == key
                        && owner.scope.pin == pin
                        && owner.scope.original.request == progress.request
                    {
                        owner.terminal.send_replace(Some(progress.clone()));
                    }
                }
            }
            _ => {}
        }
        reply
    }

    /// Joins every original after transport closure, retaining errors until all join.
    /// Native expiry, not a wall timeout, settles an unaccepted original.
    pub async fn finish(mut self) -> Result<(), ServiceFailure> {
        for owner in self.originals {
            if let Err(error) = joined_owner(owner.task.await)
                && self.failure.is_none()
            {
                self.failure = Some(error);
            }
        }
        self.failure.map_or(Ok(()), Err)
    }

    async fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.originals.len() {
            if self.originals[index].task.is_finished() {
                let owner = self.originals.swap_remove(index);
                if let Err(error) = joined_owner(owner.task.await)
                    && self.failure.is_none()
                {
                    self.failure = Some(error);
                }
            } else {
                index += 1;
            }
        }
    }

    async fn arm(&mut self, scope: OriginalScope) -> Result<(), ServiceFailure> {
        let index = match self
            .originals
            .iter()
            .position(|owner| owner.scope.original.request == scope.original.request)
        {
            Some(index) if self.originals[index].scope == scope => index,
            Some(_) => return Err(ServiceFailure::RequestConflict),
            None => {
                if self.originals.len() >= NATIVE_LIFECYCLE_OWNER_LIMIT {
                    return Err(ServiceFailure::LimitExceeded);
                }
                let token = self.supervisor.native_lifecycle_owner_token(
                    scope.key,
                    scope.pin,
                    scope.original,
                )?;
                let (ready_tx, ready) = watch::channel(false);
                let (terminal, terminal_rx) = watch::channel(None);
                let supervisor = self.supervisor.clone();
                let task = tokio::spawn(own_original(
                    supervisor,
                    scope,
                    token,
                    ready_tx,
                    terminal_rx,
                ));
                self.originals.push(OriginalOwner {
                    scope,
                    ready,
                    terminal,
                    task,
                });
                self.originals.len() - 1
            }
        };
        let mut ready = self.originals[index].ready.clone();
        // A bounded readiness failure does not cancel the registered worker.
        // finish still joins it if the reply cannot be delivered.
        tokio::time::timeout(OWNER_READY_BUDGET, async move {
            while !*ready.borrow_and_update() {
                ready
                    .changed()
                    .await
                    .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| ServiceFailure::Pending)?
    }
}

fn joined_owner(
    result: Result<Result<(), ServiceFailure>, tokio::task::JoinError>,
) -> Result<(), ServiceFailure> {
    result.map_err(|_| ServiceFailure::SupervisorUnavailable)?
}

async fn own_original(
    supervisor: Arc<ServiceSupervisor>,
    scope: OriginalScope,
    token: NativeLifecycleOwnerToken,
    ready: watch::Sender<bool>,
    mut terminal: watch::Receiver<Option<ServiceProgress>>,
) -> Result<(), ServiceFailure> {
    loop {
        if terminal.borrow_and_update().is_some() {
            return Ok(());
        }
        if let Ok(disposition) = supervisor.poll_owned_runtime_lifecycle_handoff(&token) {
            ready.send_replace(true);
            match disposition {
                NativeLifecycleHandoff::Awaiting { .. } => {}
                NativeLifecycleHandoff::ExpiredNonAdmission { original, .. }
                    if original == scope.original =>
                {
                    return Ok(());
                }
                NativeLifecycleHandoff::RetiredOriginal { original }
                    if original == scope.original =>
                {
                    // Native proof permits drain only. Do not fabricate an
                    // outcome, successful ACK or client-journal retirement.
                    return Err(ServiceFailure::RequestRetired);
                }
                NativeLifecycleHandoff::ResolveOriginal { progress } => {
                    if exact_terminal(scope, &progress) {
                        return Ok(());
                    }
                    // Reserved+reserving permits exact original abort cleanup.
                    // Plain Reserved never reaches this branch before expiry.
                    if let Ok(progress) = supervisor
                        .resolve_runtime_lifecycle(scope.key, scope.pin, scope.original.request)
                        .await
                        && exact_terminal(scope, &progress)
                    {
                        return Ok(());
                    }
                }
                NativeLifecycleHandoff::ExpiredNonAdmission { .. }
                | NativeLifecycleHandoff::RetiredOriginal { .. } => {}
            }
        }
        // Storage, clock, retirement and cleanup errors are not proof of
        // non-admission or completion. Preserve ownership and retry exactly.
        tokio::select! {
            _ = tokio::time::sleep(OWNER_POLL_INTERVAL) => {},
            result = terminal.changed() => {
                if result.is_err() {
                    // Dropping the owner set cannot cancel accepted work.
                    tokio::time::sleep(OWNER_POLL_INTERVAL).await;
                }
            },
        }
    }
}

fn exact_terminal(scope: OriginalScope, progress: &ServiceProgress) -> bool {
    progress.request == scope.original.request
        && progress.phase == ServicePhase::Completed
        && matches!(
            progress.state,
            ServiceRequestState::Completed(_) | ServiceRequestState::NotApplied(_)
        )
        && NativeLifecycleAccountReply::Progress(progress.clone())
            .validate()
            .is_ok()
}
