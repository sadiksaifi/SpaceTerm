//! Bounded orchestration over the installation owner's single protected journal.

use std::sync::{Mutex, TryLockError};
use std::time::Duration;

use super::helper_lifetime::{HelperDeadline, HelperLifetimeGuard};

use super::request::{ManagementRequest, OperationTag};
use crate::native_deployment::contracts::{NativeFailure, PublicationDisposition};
use crate::startup::DesiredServerState;

/// Maximum native helper work window. A drain is polled, never waited out here.
pub const HELPER_LIFETIME: Duration = Duration::from_secs(2);
/// A confirmation's original assessment window. OS prompts do not extend it.
pub const ASSESSMENT_LIFETIME: Duration = Duration::from_secs(30);

/// Fixed native authorization route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationRoute {
    /// Operation-specific GUI authorization.
    Gui,
    /// Sudo's controlling-terminal authentication.
    CliInteractive,
    /// Sudo with no prompt.
    CliNonInteractive,
}

/// Bounded lifecycle phase stored by the shared native journal adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementPhase {
    /// Native request durably admitted.
    Admitted,
    /// Stopped intent and advanced generations durably committed.
    IntentCommitted,
    /// Automatic restart suspended without killing the tracked process.
    StartupSuspended,
    /// Unprivileged drainage requested.
    Quiescing,
    /// Previous runtime, retriever, Sessions, and endpoint absence verified.
    Quiesced,
    /// Supervision removed after verified absence.
    SupervisionRemoved,
    /// Retained outcome durably completed.
    Completed,
}

/// Content-free retained request state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressState {
    /// The caller may resolve the original request later.
    Pending,
    /// The durable outcome and all required cleanup are proven.
    Complete,
    /// Durability was uncertain. The native owner must latch startup/mutation closed.
    Indeterminate,
    /// Exact previous-instance retirement remains unproven.
    CleanupUnresolved,
}

/// Bounded content-free progress. Request bytes are public identity, not authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementProgress {
    /// Original native request identity.
    pub request: [u8; 16],
    /// Last validated native journal phase.
    pub phase: ManagementPhase,
    /// Known outcome class.
    pub state: ProgressState,
}

/// The common native publication result, applied to the Stopped head commit.
pub type IntentDisposition = PublicationDisposition<()>;

/// Actual old-instance cleanup, not emptiness of a new process registry.
pub enum Quiescence {
    /// Already-Started Git writes are still settling.
    Pending,
    /// All previous process/Session/endpoint absence has been proved.
    Complete,
    /// Old-instance cleanup cannot be proved.
    CleanupUnresolved,
}

/// Admission to the installation owner's one journal.
pub enum JournalAdmission<T> {
    /// Retained noncloneable journal authority.
    Transaction(T),
    /// Exact same request already has a retained outcome.
    Retained(ManagementProgress),
}

/// Native boundaries selected only by the trusted composition root.
///
/// The production authorization type must be common AuthorizedNativeRequest.
/// Facts, SUDO_UID, PKEXEC_UID, request fields, and argv cannot implement authorize.
/// Every method is bounded by the original helper deadline. Drain work and waits
/// belong to unprivileged lifecycle orchestration. The native journal owner
/// serializes all installations, starts, stops, activations, repairs, and releases.
pub trait ManagementBackend: Send + Sync {
    /// Opaque OS-authorized request, never serialized or caller-constructed.
    type Authorization;
    /// Retained authority from the shared native installation journal.
    type Transaction;
    /// Independently bind actual OS caller, account incarnation, build, and right.
    fn authorize(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Authorization, NativeFailure>;
    /// Find only an already-durably-admitted identical request. Verify all contents
    /// and caller scope; None permits fresh assessment, not implicit admission.
    /// Existing operations resolve without extending or reusing their assessment.
    fn lookup_existing(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure>;
    /// Reconstruct confirmation, account, current generations, and assessment.
    /// The assessment expires after 30 seconds from its original creation.
    fn revalidate(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure>;
    /// Durably admit or return the exact retained outcome under native serialization.
    /// Different contents under the same request identity must fail.
    fn admit(
        &self,
        request: &ManagementRequest,
        authority: Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure>;
    /// Read/resume only this durably admitted request. Never start a new mutation
    /// using expired confirmation or authorization. Authenticate the resolving caller.
    fn resolve(
        &self,
        request: [u8; 16],
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure>;
    /// Validated immutable comparison facts retained with the transaction.
    fn request<'a>(&self, transaction: &'a Self::Transaction) -> &'a ManagementRequest;
    /// Last durably validated phase; uncertainty must not be reported as a phase advance.
    fn phase(&self, transaction: &Self::Transaction) -> ManagementPhase;
    /// Current registered desired state, revalidated inside native serialization.
    fn desired_state(
        &self,
        transaction: &Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<DesiredServerState, NativeFailure>;
    /// Fence startup/release and durably publish Stopped, advancing both generations.
    /// This is the first Stop effect and remains fenced after any failure.
    fn commit_stopped(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> IntentDisposition;
    /// Suspend automatic startup without sending a termination signal or bootout.
    fn suspend_automatic_start(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Close admissions/listeners, cancel reads, and arrange unprivileged write
    /// drainage and exact Session retirement. Persist prerequisites before effects.
    fn begin_quiescence(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure>;
    /// Poll the same original reservations and instance scope without waiting.
    fn poll_quiescence(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure>;
    /// Bootout/disable only after exact previous-instance and endpoint absence.
    fn retire_supervision(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Durably retain the completed Stop outcome, after supervisor removal.
    fn finish_stop(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Restore Running and supervision for approved code only. Never install or
    /// select a staged installation. Return authenticated readiness separately.
    fn start_approved(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure>;
    /// One Running-intent journaled restart after quiescence, same approved code.
    fn restart_approved(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure>;
    /// Delegate installation/pairing effects to their existing owners, using this
    /// transaction. This must not create another activation/native request journal.
    fn apply_owned_operation(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure>;
}

/// One-request manager orchestration, not a client facade or authority provider.
/// The supplied guard retires this process at its original absolute deadline.
pub struct ManagementController<B> {
    backend: B,
    serial: Mutex<()>,
    lifetime: HelperLifetimeGuard,
}

impl<B: ManagementBackend> ManagementController<B> {
    /// Inject native boundaries and the process-entry lifetime guard. Installing
    /// the guard before transport reads prevents a new budget after request parsing.
    pub fn new(backend: B, lifetime: HelperLifetimeGuard) -> Self {
        Self {
            backend,
            serial: Mutex::new(()),
            lifetime,
        }
    }

    /// OS-authorize, revalidate, and perform bounded work. Dropping a client wait
    /// cannot cancel an admitted transaction; resolve its original request instead.
    pub fn execute(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
    ) -> Result<ManagementProgress, NativeFailure> {
        let deadline = self.lifetime.deadline();
        // Interactive OS authentication happens in unprivileged orchestration.
        // This port verifies already-obtained authority without prompting as root.
        let authority = self.backend.authorize(request, route, deadline)?;
        deadline.check()?;
        let _serial = self.serial.try_lock().map_err(lock_failure)?;
        if let Some(existing) = self
            .backend
            .lookup_existing(request, &authority, deadline)?
        {
            return self.advance(existing, *request.request_bytes(), deadline);
        }
        let assessment_age = self.backend.revalidate(request, &authority, deadline)?;
        if assessment_age >= ASSESSMENT_LIFETIME {
            return Err(NativeFailure::StaleAssessment);
        }
        deadline.check()?;
        let admission = self.backend.admit(request, authority, deadline)?;
        self.advance(admission, *request.request_bytes(), deadline)
    }

    /// Resolve one existing native request without creating another mutation.
    pub fn resolve(&self, request: [u8; 16]) -> Result<ManagementProgress, NativeFailure> {
        if request == [0; 16] {
            return Err(NativeFailure::InvalidRecord);
        }
        let deadline = self.lifetime.deadline();
        let _serial = self.serial.try_lock().map_err(lock_failure)?;
        let admission = self.backend.resolve(request, deadline)?;
        self.advance(admission, request, deadline)
    }

    fn advance(
        &self,
        admission: JournalAdmission<B::Transaction>,
        request: [u8; 16],
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        deadline.check()?;
        let mut transaction = match admission {
            JournalAdmission::Retained(progress) => return bind_progress(progress, request),
            JournalAdmission::Transaction(transaction) => transaction,
        };
        if self.backend.request(&transaction).request_bytes() != &request {
            return Err(NativeFailure::IdentityMismatch);
        }
        let operation = self.backend.request(&transaction).operation();
        // Six Stop transitions plus one final outcome. No loop can keep a helper resident.
        for _ in 0..8 {
            deadline.check()?;
            let phase = self.backend.phase(&transaction);
            let state = match operation {
                OperationTag::Stop => self.stop_step(&mut transaction, phase, deadline)?,
                OperationTag::Restart => self.restart_step(&mut transaction, phase, deadline)?,
                OperationTag::Start => {
                    let progress = self.backend.start_approved(&mut transaction, deadline)?;
                    deadline.check()?;
                    return bind_progress(progress, request);
                }
                _ => {
                    let progress = self
                        .backend
                        .apply_owned_operation(&mut transaction, deadline)?;
                    deadline.check()?;
                    return bind_progress(progress, request);
                }
            };
            deadline.check()?;
            if let Some(state) = state {
                return Ok(ManagementProgress {
                    request,
                    phase: self.backend.phase(&transaction),
                    state,
                });
            }
            if self.backend.phase(&transaction) == phase {
                return Err(NativeFailure::UnsafeNativeState);
            }
        }
        Err(NativeFailure::LimitExceeded)
    }

    fn stop_step(
        &self,
        transaction: &mut B::Transaction,
        phase: ManagementPhase,
        deadline: HelperDeadline,
    ) -> Result<Option<ProgressState>, NativeFailure> {
        match phase {
            ManagementPhase::Admitted => match self.backend.commit_stopped(transaction, deadline) {
                IntentDisposition::Durable(()) => {
                    self.require_phase(transaction, ManagementPhase::IntentCommitted)?;
                    Ok(None)
                }
                IntentDisposition::NotApplied(failure) => Err(failure),
                IntentDisposition::Indeterminate => Ok(Some(ProgressState::Indeterminate)),
            },
            ManagementPhase::IntentCommitted => {
                self.backend
                    .suspend_automatic_start(transaction, deadline)?;
                self.require_phase(transaction, ManagementPhase::StartupSuspended)?;
                Ok(None)
            }
            ManagementPhase::StartupSuspended => self.quiesce(transaction, true, deadline),
            ManagementPhase::Quiescing => self.quiesce(transaction, false, deadline),
            ManagementPhase::Quiesced => {
                self.backend.retire_supervision(transaction, deadline)?;
                self.require_phase(transaction, ManagementPhase::SupervisionRemoved)?;
                Ok(None)
            }
            ManagementPhase::SupervisionRemoved => {
                self.backend.finish_stop(transaction, deadline)?;
                self.require_phase(transaction, ManagementPhase::Completed)?;
                Ok(None)
            }
            ManagementPhase::Completed => Ok(Some(ProgressState::Complete)),
        }
    }

    fn require_phase(
        &self,
        transaction: &B::Transaction,
        expected: ManagementPhase,
    ) -> Result<(), NativeFailure> {
        if self.backend.phase(transaction) != expected {
            return Err(NativeFailure::UnsafeNativeState);
        }
        Ok(())
    }

    fn quiesce(
        &self,
        transaction: &mut B::Transaction,
        begin: bool,
        deadline: HelperDeadline,
    ) -> Result<Option<ProgressState>, NativeFailure> {
        let progress = if begin {
            self.backend.begin_quiescence(transaction, deadline)?
        } else {
            self.backend.poll_quiescence(transaction, deadline)?
        };
        let expected = match progress {
            Quiescence::Complete => ManagementPhase::Quiesced,
            Quiescence::Pending | Quiescence::CleanupUnresolved => ManagementPhase::Quiescing,
        };
        self.require_phase(transaction, expected)?;
        Ok(quiescence_state(progress))
    }

    fn restart_step(
        &self,
        transaction: &mut B::Transaction,
        phase: ManagementPhase,
        deadline: HelperDeadline,
    ) -> Result<Option<ProgressState>, NativeFailure> {
        if self.backend.desired_state(transaction, deadline)? != DesiredServerState::Running {
            return Err(NativeFailure::UnsafeNativeState);
        }
        match phase {
            ManagementPhase::Admitted => self.quiesce(transaction, true, deadline),
            ManagementPhase::Quiescing => self.quiesce(transaction, false, deadline),
            ManagementPhase::Quiesced => {
                let request = *self.backend.request(transaction).request_bytes();
                let progress = bind_progress(
                    self.backend.restart_approved(transaction, deadline)?,
                    request,
                )?;
                Ok(Some(progress.state))
            }
            ManagementPhase::Completed => Ok(Some(ProgressState::Complete)),
            _ => Err(NativeFailure::UnsafeNativeState),
        }
    }
}

fn quiescence_state(progress: Quiescence) -> Option<ProgressState> {
    match progress {
        Quiescence::Pending => Some(ProgressState::Pending),
        Quiescence::Complete => None,
        Quiescence::CleanupUnresolved => Some(ProgressState::CleanupUnresolved),
    }
}
fn bind_progress(
    progress: ManagementProgress,
    request: [u8; 16],
) -> Result<ManagementProgress, NativeFailure> {
    if progress.request != request {
        return Err(NativeFailure::IdentityMismatch);
    }
    Ok(progress)
}
fn lock_failure<T>(failure: TryLockError<T>) -> NativeFailure {
    match failure {
        TryLockError::Poisoned(_) => NativeFailure::UnsafeNativeState,
        TryLockError::WouldBlock => NativeFailure::OutcomePending,
    }
}
