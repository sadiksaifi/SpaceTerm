//! Native owner of signed running Stage effects. SSH can carry an exact bounded
//! signed package and restore original comparisons. It cannot admit Stage.

use std::{sync::Arc, time::Duration};

use spaceterm_distribution::VerifiedAccountServerArtifact;
use spaceterm_host::{
    account_service::{
        NativeStageHandoff, NativeStageOwnerToken, NativeStageRequest, ServiceFailure,
        ServicePhase, ServiceProgress, ServiceSupervisor,
    },
    startup::AccountBuildKey,
};
use spaceterm_protocol::authentication::ServerPin;
use tokio::{sync::watch, task::JoinHandle};

pub use spaceterm_host::account_service::{NativeStageAccountReply, NativeStageAccountRequest};

/// Resolves one exact guest-native Stage request. The existing paired worker and
/// serialized actor admit Stage separately; this handler never obtains authority
/// from decoded data or invokes a service manager or a terminal reservation.
pub fn handle(
    supervisor: &ServiceSupervisor,
    request: &NativeStageAccountRequest,
    artifact: Option<&VerifiedAccountServerArtifact>,
) -> NativeStageAccountReply {
    match perform(supervisor, request, artifact) {
        Ok(reply) => match reply.validate() {
            Ok(()) => reply,
            Err(error) => NativeStageAccountReply::Refused(error),
        },
        Err(error) => NativeStageAccountReply::Refused(error),
    }
}

fn perform(
    supervisor: &ServiceSupervisor,
    request: &NativeStageAccountRequest,
    artifact: Option<&VerifiedAccountServerArtifact>,
) -> Result<NativeStageAccountReply, ServiceFailure> {
    request.validate()?;
    let (expected_key, expected_pin) = request.expected_identity();
    if supervisor.key() != expected_key {
        return Err(ServiceFailure::IdentityMismatch);
    }
    supervisor.reconcile_runtime_stage_storage(expected_key, expected_pin, request.request())?;
    let registration = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
    if registration.key() != expected_key || registration.server_pin() != expected_pin {
        return Err(ServiceFailure::IdentityMismatch);
    }
    match request {
        NativeStageAccountRequest::Capture {
            request,
            artifact_sha256,
            ..
        } => {
            let artifact = artifact.ok_or(ServiceFailure::ArtifactRejected)?;
            if artifact.artifact_sha256() != *artifact_sha256 {
                return Err(ServiceFailure::RequestConflict);
            }
            supervisor
                .capture_runtime_stage(*request, artifact)
                .map(NativeStageAccountReply::Captured)
        }
        NativeStageAccountRequest::Restore { request, .. } => {
            if artifact.is_some() {
                return Err(ServiceFailure::RequestConflict);
            }
            let original = supervisor.restore_runtime_stage(*request)?;
            let assessment = match supervisor.restore_runtime_stage_reference(original) {
                Ok(reference) => Some(reference),
                Err(ServiceFailure::Pending) => None,
                Err(error) => return Err(error),
            };
            let progress = match supervisor.inspect_install_request(*request) {
                Ok((progress, digest)) if digest == original.artifact_sha256 => Some(progress),
                Ok(_) => return Err(ServiceFailure::RequestConflict),
                Err(ServiceFailure::UnknownRequest) => None,
                Err(error) => return Err(error),
            };
            Ok(NativeStageAccountReply::Restored {
                original,
                assessment,
                progress,
            })
        }
        NativeStageAccountRequest::Resolve { request, .. } => supervisor
            .resolve_runtime_stage_exact(*request, artifact)
            .map(NativeStageAccountReply::Progress),
        NativeStageAccountRequest::Acknowledge { progress, .. } => {
            if artifact.is_some() {
                return Err(ServiceFailure::RequestConflict);
            }
            supervisor.acknowledge_runtime_stage_exact(progress)?;
            Ok(NativeStageAccountReply::Acknowledged {
                progress: progress.clone(),
            })
        }
    }
}

/// Maximum original Stage workers retained by one account helper.
pub const NATIVE_STAGE_OWNER_LIMIT: usize = 32;
const OWNER_READY_BUDGET: Duration = Duration::from_secs(5);
const OWNER_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Owns exact original Stage work and its verified archive outside the server.
///
/// A Captured reply is released only after the worker retains the exact archive
/// and verifies the native handoff. The helper must protect itself from SSH HUP
/// before handling frames and call `finish` after EOF or reply failure, before
/// shutting down its runtime or removing its executable. Dropping this value
/// does not join the workers. No owner task can admit a Reserved original.
pub struct NativeStageOriginalOwners {
    supervisor: Arc<ServiceSupervisor>,
    originals: Vec<OriginalOwner>,
    failure: Option<ServiceFailure>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct OriginalScope {
    key: AccountBuildKey,
    pin: ServerPin,
    original: NativeStageRequest,
}

struct OriginalOwner {
    scope: OriginalScope,
    ready: watch::Receiver<bool>,
    archive: watch::Sender<Option<Arc<VerifiedAccountServerArtifact>>>,
    terminal: watch::Sender<Option<ServiceProgress>>,
    task: JoinHandle<Result<(), ServiceFailure>>,
}

impl NativeStageOriginalOwners {
    /// Creates a bounded, ephemeral owner set over the actual native provider.
    pub fn new(supervisor: Arc<ServiceSupervisor>) -> Self {
        Self {
            supervisor,
            originals: Vec::new(),
            failure: None,
        }
    }

    /// Handles one closed request while retaining its exact original archive.
    /// Capture requires input. Resolve may explicitly re-supply the same verified
    /// archive after recreation. Missing Accepted input remains Pending; this
    /// handler never creates another capture, proof, expiry or admission.
    pub async fn handle(
        &mut self,
        request: &NativeStageAccountRequest,
        artifact: Option<Arc<VerifiedAccountServerArtifact>>,
    ) -> NativeStageAccountReply {
        self.reap_finished().await;
        if let Err(error) = request.validate() {
            return NativeStageAccountReply::Refused(error);
        }
        let (key, pin) = request.expected_identity();
        if self.supervisor.key() != key {
            return NativeStageAccountReply::Refused(ServiceFailure::IdentityMismatch);
        }
        if matches!(
            request,
            NativeStageAccountRequest::Capture { .. }
                | NativeStageAccountRequest::Restore { .. }
                | NativeStageAccountRequest::Resolve { .. }
        ) && !self
            .originals
            .iter()
            .any(|owner| owner.scope.original.request == request.request())
            && self.originals.len() >= NATIVE_STAGE_OWNER_LIMIT
        {
            return NativeStageAccountReply::Refused(ServiceFailure::LimitExceeded);
        }
        // Register the owner and transfer its archive before any copy future.
        // Cancelling a Resolve frame cannot abandon the original admitted copy.
        if let NativeStageAccountRequest::Resolve { request, .. } = request {
            let original = match self
                .supervisor
                .reconcile_runtime_stage_storage(key, pin, *request)
                .and_then(|()| self.supervisor.restore_runtime_stage(*request))
            {
                Ok(original) => original,
                Err(error) => return NativeStageAccountReply::Refused(error),
            };
            if let Err(error) = self
                .arm(OriginalScope { key, pin, original }, artifact.clone())
                .await
            {
                return NativeStageAccountReply::Refused(error);
            }
        }
        let reply = if matches!(request, NativeStageAccountRequest::Resolve { .. }) {
            let supervisor = self.supervisor.clone();
            let request = request.clone();
            let input = artifact.clone();
            match tokio::task::spawn_blocking(move || {
                handle(&supervisor, &request, input.as_deref())
            })
            .await
            {
                Ok(reply) => reply,
                Err(_) => NativeStageAccountReply::Refused(ServiceFailure::SupervisorUnavailable),
            }
        } else {
            // Capture's synchronous publication and owner registration have no
            // cancellation point between them. Input stays owned throughout.
            handle(&self.supervisor, request, artifact.as_deref())
        };
        match &reply {
            NativeStageAccountReply::Captured(original)
            | NativeStageAccountReply::Restored { original, .. } => {
                if let Err(error) = self
                    .arm(
                        OriginalScope {
                            key,
                            pin,
                            original: *original,
                        },
                        artifact,
                    )
                    .await
                {
                    return NativeStageAccountReply::Refused(error);
                }
            }
            NativeStageAccountReply::Progress(progress)
            | NativeStageAccountReply::Acknowledged { progress }
                if progress.phase == ServicePhase::Completed =>
            {
                // These receipts were verified by the native handler. Preserve
                // exact settlement if ACK precedes the worker's next poll.
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

    /// Joins all originals after transport closure, retaining join failures.
    /// Native expiry settles never-admitted work. Accepted work waits for exact
    /// resolution, including explicit original input re-supply by another helper.
    pub async fn finish(mut self) -> Result<(), ServiceFailure> {
        for owner in self.originals {
            let error = match owner.task.await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some(ServiceFailure::SupervisorUnavailable),
            };
            if self.failure.is_none() {
                self.failure = error;
            }
        }
        self.failure.map_or(Ok(()), Err)
    }

    async fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.originals.len() {
            if self.originals[index].task.is_finished() {
                let owner = self.originals.swap_remove(index);
                let error = match owner.task.await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(_) => Some(ServiceFailure::SupervisorUnavailable),
                };
                if self.failure.is_none() {
                    self.failure = error;
                }
            } else {
                index += 1;
            }
        }
    }

    async fn arm(
        &mut self,
        scope: OriginalScope,
        artifact: Option<Arc<VerifiedAccountServerArtifact>>,
    ) -> Result<(), ServiceFailure> {
        if let Some(artifact) = &artifact {
            // The existing native capture checks compiled trust, exact SHA and
            // version. Repeating it preserves the original nonce and expiry.
            if self
                .supervisor
                .capture_runtime_stage(scope.original.request, artifact)?
                != scope.original
            {
                return Err(ServiceFailure::RequestConflict);
            }
        }
        let index = match self
            .originals
            .iter()
            .position(|owner| owner.scope.original.request == scope.original.request)
        {
            Some(index) if self.originals[index].scope == scope => {
                if artifact.is_some() {
                    self.originals[index].archive.send_replace(artifact);
                }
                index
            }
            Some(_) => return Err(ServiceFailure::RequestConflict),
            None => {
                if self.originals.len() >= NATIVE_STAGE_OWNER_LIMIT {
                    return Err(ServiceFailure::LimitExceeded);
                }
                let token = self.supervisor.native_stage_owner_token(
                    scope.key,
                    scope.pin,
                    scope.original,
                )?;
                let (ready_tx, ready) = watch::channel(false);
                let (archive, archive_rx) = watch::channel(artifact);
                let (terminal, terminal_rx) = watch::channel(None);
                let task = tokio::spawn(own_original(
                    self.supervisor.clone(),
                    scope,
                    token,
                    ready_tx,
                    archive_rx,
                    terminal_rx,
                ));
                self.originals.push(OriginalOwner {
                    scope,
                    ready,
                    archive,
                    terminal,
                    task,
                });
                self.originals.len() - 1
            }
        };
        let mut ready = self.originals[index].ready.clone();
        // A reply timeout does not cancel the already registered original.
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

async fn own_original(
    supervisor: Arc<ServiceSupervisor>,
    scope: OriginalScope,
    token: NativeStageOwnerToken,
    ready: watch::Sender<bool>,
    mut archive: watch::Receiver<Option<Arc<VerifiedAccountServerArtifact>>>,
    mut terminal: watch::Receiver<Option<ServiceProgress>>,
) -> Result<(), ServiceFailure> {
    let mut archive_closed = false;
    let mut terminal_closed = false;
    loop {
        if terminal.borrow_and_update().is_some() {
            return Ok(());
        }
        // Neither storage errors, an invalid clock nor a retired highwater
        // mark proves settlement. Preserve the original and retry exactly.
        if let Ok(disposition) = supervisor.poll_owned_runtime_stage_handoff(&token) {
            ready.send_replace(true);
            match disposition {
                NativeStageHandoff::Awaiting { .. } => {}
                NativeStageHandoff::ExpiredNonAdmission { original, .. }
                    if original == scope.original =>
                {
                    return Ok(());
                }
                NativeStageHandoff::RetiredOriginal { original } if original == scope.original => {
                    // Drain only. Do not imply an exact outcome or ACK match.
                    return Err(ServiceFailure::RequestRetired);
                }
                NativeStageHandoff::ResolveOriginal { progress } => {
                    if exact_terminal(scope, &progress) {
                        return Ok(());
                    }
                    let input = archive.borrow_and_update().clone();
                    if let Some(input) = input {
                        let provider = supervisor.clone();
                        // The blocking copy owns both archive and native
                        // provider until it returns, even if its frame dies.
                        if let Ok(Ok(progress)) = tokio::task::spawn_blocking(move || {
                            provider
                                .resolve_runtime_stage_exact(scope.original.request, Some(&input))
                        })
                        .await
                            && exact_terminal(scope, &progress)
                        {
                            return Ok(());
                        }
                    }
                }
                NativeStageHandoff::ExpiredNonAdmission { .. }
                | NativeStageHandoff::RetiredOriginal { .. } => {}
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(OWNER_POLL_INTERVAL) => {},
            result = archive.changed(), if !archive_closed => {
                archive_closed = result.is_err();
            },
            result = terminal.changed(), if !terminal_closed => {
                terminal_closed = result.is_err();
            },
        }
    }
}

fn exact_terminal(scope: OriginalScope, progress: &ServiceProgress) -> bool {
    progress.request == scope.original.request
        && progress.phase == ServicePhase::Completed
        && NativeStageAccountReply::Progress(progress.clone())
            .validate()
            .is_ok()
}
