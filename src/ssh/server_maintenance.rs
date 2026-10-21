//! Explicit native service and update operations on a selected SSH account server.
//!
//! The person selects one saved SSH server and asks for Start, Stop, Restart, Uninstall or an
//! exact server update. Each request travels the original OpenSSH account route that admitted the
//! connection and runs as that account, with no SpaceTerm paired-device credential and no helper
//! the person installs or manages.
//!
//! Every exchange validates the original route, account, application identity and server identity
//! before it asks for anything, so a destination that now reaches another computer, account or
//! server pauses instead of acting. A destructive request carries the confirmation the service
//! issued for one exact observation: a Stop confirmed while the server was running cannot stop
//! work started afterwards, and an update confirmed for one build cannot replace a later approved
//! one.
//!
//! Resolving an operation already submitted is continuation of that submitted intent. Fresh query
//! authority may read and acknowledge its outcome. It never repeats a destructive dispatch, renews
//! its approval, selects another artifact or starts a stopped server.

use spaceterm_server::service::maintenance::{
    Action, MaintenanceFacts, MaintenanceRefusal, OperationOutcome, PreparedChange,
};

use super::cancellation::SshCancellationToken;
use super::server_connection::{
    ContinuityChange, IncompatibilityKind, InteractionReason, SshServerContinuity, UnavailableKind,
};
use crate::domain::SshDestination;

/// The server one maintenance operation names, bound before dispatch and never widened.
///
/// `destination` dials the route and names the machine in the interface. `continuity` is the
/// authority: every exchange refuses unless the route, account, application identity and server
/// identity it observes are the ones the original bound, so a current alias of the same name
/// cannot redirect the operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaintenanceTarget {
    pub destination: SshDestination,
    pub continuity: SshServerContinuity,
}

/// How much authority one exchange may acquire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaintenanceAuthority {
    /// Background continuation. It uses noninteractive authority that is already available and
    /// presents no authentication, host-approval or credential-provider request. BatchMode alone
    /// does not prove an agent or provider stays silent, so an attempt that needs any of them
    /// stops and waits for the person.
    Quiet,
    /// The person asked here, so a fresh SSH query route may be authenticated in front of them.
    Requested,
}

/// What a submitted operation asked for. It is retained with the operation, so resolving it can
/// never change what was asked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SubmittedKind {
    /// The only operation that reverses a deliberate Stop.
    Start,
    Service(Action),
    /// Activation of one exact verified artifact, named by its whole-package digest.
    Update { candidate: String },
    /// A return to the retained previous runtime of an update that never became ready.
    Return,
}

/// One bound submission: the operation identity the service recognizes, what it asks for, and the
/// exact-observation confirmation that authorizes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationSubmission {
    pub operation: [u8; 32],
    pub kind: SubmittedKind,
    /// The confirmation the service issued for the observation the person reviewed. `None` only
    /// for Start, which ends no work.
    pub confirmation: Option<String>,
}

/// Truthful progress through one maintenance exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaintenancePhase {
    /// Signing in on the original route.
    Authenticating,
    /// Checking that the account, application identity and server are the ones bound.
    Validating,
    /// Transferring the verified server artifact.
    Transferring,
    /// Waiting for the service to admit the operation.
    Dispatching,
    /// Reading the submitted operation's validated outcome.
    Resolving,
}

pub(crate) type MaintenanceProgress<'a> = dyn Fn(MaintenancePhase) + Send + Sync + 'a;

/// Why one maintenance exchange could not produce a validated result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SshMaintenanceFailure {
    Cancelled,
    /// Authority is needed that background continuation never asks for.
    NeedsPerson(InteractionReason),
    /// The route, account, application identity or server is not the one bound.
    Changed(ContinuityChange),
    /// The service refused, before any effect.
    Refused(MaintenanceRefusal),
    Incompatible(IncompatibilityKind),
    Unavailable(UnavailableKind),
    /// The operation was admitted and its result could not be validated. It stays unconfirmed,
    /// and only resolving the same original can settle it.
    Unconfirmed,
    /// This service holds no record of the submitted original.
    Forgotten,
}

/// The remote maintenance exchanges, run on a worker and never on a render thread.
///
/// Construction performs no SSH work. Describing a change performs no destructive work, and
/// nothing here starts a server except [`SubmittedKind::Start`].
pub(crate) trait SshMaintenanceBackend: Send + Sync {
    /// Reads the service's policy, approved build and any recorded original operation on the
    /// bound route. Starts, stages, installs and stops nothing.
    fn observe(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<MaintenanceFacts, SshMaintenanceFailure>;

    /// Describes one destructive action and returns the service's confirmation for the exact
    /// observation it describes. Changes nothing.
    fn prepare(
        &self,
        target: &MaintenanceTarget,
        action: Action,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<PreparedChange, SshMaintenanceFailure>;

    /// Verifies the exact artifact this client holds for the target's platform, transfers it to
    /// the bound account, and describes its activation. `Ok(None)` when that artifact is already
    /// the approved build, so no update is needed. Live Terminal Sessions keep using the approved
    /// running code until an activation the person confirms.
    fn prepare_update(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<Option<PreparedChange>, SshMaintenanceFailure>;

    /// Describes a return to the previous runtime of an update that never became ready.
    fn prepare_return(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<PreparedChange, SshMaintenanceFailure>;

    /// Submits exactly one bound operation and reads its validated outcome. A transport reply is
    /// not completion: the outcome says whether the service validated the result.
    fn apply(
        &self,
        target: &MaintenanceTarget,
        submission: &OperationSubmission,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<OperationOutcome, SshMaintenanceFailure>;

    /// Resolves an already-submitted original's outcome. It dispatches no destructive work,
    /// renews no approval, selects no artifact and starts no stopped server.
    fn resolve(
        &self,
        target: &MaintenanceTarget,
        operation: [u8; 32],
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<OperationOutcome, SshMaintenanceFailure>;

    /// Releases the bounded route and owned processes this backend holds for `destination`.
    /// Pending originals are unaffected.
    fn release(&self, destination: &SshDestination);
}
