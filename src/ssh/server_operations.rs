//! The desktop's submitted remote maintenance operations and what survives interruption.
//!
//! One owner holds every remote service and update operation this client submitted, together with
//! the service facts its last validated read found. The Servers surface reads a snapshot, so
//! rendering asks no computer anything and construction reaches no host.
//!
//! A submitted operation outlives the attempt that submitted it. It survives the server being
//! stopped or unreachable, the desktop quitting or being killed, and removal of its saved
//! connection: removal ends presentation and ordinary reconnect intent, while the operation's own
//! record stays until its outcome is validated and the person acknowledges it. Recovery resolves
//! that record alone. It never recreates the removed connection, writes saved connection intent,
//! or lets a connection saved again under the same destination inherit another connection's
//! pending original.
//!
//! The person never handles an operation identifier, a confirmation token or a helper lifetime.
//! They choose an action, confirm what it affects, and later see its validated result or a
//! truthful unconfirmed outcome.

use std::sync::Arc;

use spaceterm_server::service::maintenance::{
    Action, MaintenanceFacts, OperationOutcome, PreparedChange,
};

use super::server_connection::{ContinuityChange, InteractionReason, SshIntentStoreError};
use super::server_maintenance::{
    MaintenancePhase, MaintenanceTarget, SshMaintenanceFailure, SubmittedKind,
};
use crate::domain::SshDestination;

/// What the person asked for. Start ends no work, so it needs no confirmation; the others
/// describe their effects and wait for one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteMaintenanceRequest {
    Service(Action),
    /// The exact verified artifact this client holds for that machine.
    Update,
    /// A return to the previous runtime of an update that never became ready.
    Return,
}

/// Names one described change or one validated result inside the owner. It is an opaque handle,
/// so no surface composes an operation identity or a confirmation token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RemoteOperationHandle(pub(crate) u64);

/// One remote server's maintenance state, derived from what the owner already knows.
#[derive(Clone, Debug)]
pub(crate) enum RemoteOperationState {
    /// Nothing has been read for this server and nothing is submitted.
    Unread,
    /// A read, description, submission or resolution is underway. `kind` names the submitted
    /// operation it belongs to, when one is submitted.
    Working {
        phase: MaintenancePhase,
        kind: Option<SubmittedKind>,
        cancellable: bool,
    },
    /// The service facts the last validated read found.
    Read(Box<MaintenanceFacts>),
    /// A change the service described for one exact observation, awaiting confirmation. Only
    /// confirming this handle performs it, once.
    Described {
        handle: RemoteOperationHandle,
        request: RemoteMaintenanceRequest,
        change: Box<PreparedChange>,
        facts: Box<MaintenanceFacts>,
    },
    /// A submitted original whose outcome the service validated, awaiting acknowledgement.
    Resolved {
        handle: RemoteOperationHandle,
        kind: SubmittedKind,
        outcome: Box<OperationOutcome>,
    },
    /// A submitted original whose result could not be validated. It stays unconfirmed until
    /// resolving the same original settles it; nothing here repeats it.
    Unconfirmed {
        handle: RemoteOperationHandle,
        kind: SubmittedKind,
    },
    /// Continuation needs the person. Background work never asks on its own.
    NeedsPerson {
        reason: InteractionReason,
        kind: Option<SubmittedKind>,
    },
    /// The route, account, application identity or server is not the one the original bound, so
    /// continuation paused without sending anything.
    Paused {
        reason: ContinuityChange,
        kind: Option<SubmittedKind>,
    },
    /// The last attempt reached no validated result.
    Failed {
        failure: SshMaintenanceFailure,
        kind: Option<SubmittedKind>,
    },
}

/// One server the Servers surface presents maintenance for.
#[derive(Clone, Debug)]
pub(crate) struct RemoteOperationEntry {
    /// The destination the original bound, which names the machine.
    pub destination: SshDestination,
    /// Whether its saved connection was removed while this operation was unresolved. Such an
    /// entry offers only resolving and acknowledging the original, never reconnecting.
    pub removed: bool,
    pub state: RemoteOperationState,
}

/// What the Servers surface asks of the owner. Reading never reaches a host; every other method
/// hands work to the owner's worker and returns at once.
pub(crate) trait RemoteServerOperations: Send + Sync {
    /// Signals every state change, so a surface repaints without polling.
    fn subscribe(&self) -> async_channel::Receiver<()>;

    /// Every server with maintenance state, including one whose saved connection was removed,
    /// ordered by destination.
    fn entries(&self) -> Vec<RemoteOperationEntry>;

    /// One server's state, or [`RemoteOperationState::Unread`] when it has none.
    fn state(&self, destination: &SshDestination) -> RemoteOperationState;

    /// Reads the service facts on the bound route, with the authority the person just granted by
    /// asking. Starts, stages, installs and stops nothing.
    fn refresh(&self, destination: &SshDestination);

    /// Describes `request` without performing it, then waits for a confirmation.
    fn describe(&self, destination: &SshDestination, request: RemoteMaintenanceRequest);

    /// Performs exactly the described change this handle names, once. A changed observation
    /// refuses and asks for a new confirmation instead of acting.
    fn confirm(&self, handle: RemoteOperationHandle);

    /// Starts the server explicitly. This is the only remote operation that reverses a deliberate
    /// Stop, and it ends no work, so it needs no confirmation.
    fn start(&self, destination: &SshDestination);

    /// Abandons a described change, or stops waiting for the work underway. Cancelling does not
    /// establish that effects the service already admitted were undone.
    fn cancel(&self, destination: &SshDestination);

    /// Resolves an unresolved original the person asks about, acquiring fresh query authority on
    /// its original route. It repeats no dispatch and renews no approval.
    fn resume(&self, destination: &SshDestination);

    /// Retires a validated result the person acknowledged, and its retained record with it.
    fn acknowledge(&self, handle: RemoteOperationHandle);

    /// Continues unresolved originals quietly after relaunch, using available noninteractive
    /// authority only. Called once, after a Workspace Window exists.
    fn resume_retained(&self);
}

/// One submitted operation as it survives relaunch and saved-connection removal. It holds only
/// what resolving the same original needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RetainedRemoteOperation {
    pub target: MaintenanceTarget,
    /// The client that submitted it. A stable nonsecret identifier that grants no authority.
    pub client: [u8; 32],
    pub operation: [u8; 32],
    pub kind: SubmittedKind,
    /// The validated outcome awaiting acknowledgement, or `None` while it is unresolved.
    pub outcome: Option<RetainedOutcome>,
    /// Whether its saved connection has been removed.
    pub removed: bool,
}

/// A validated outcome as it survives relaunch, reduced to what a person must still be told.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetainedOutcome {
    Completed { running: bool },
    Refused(spaceterm_server::service::maintenance::MaintenanceRefusal),
    Failed { can_return: bool },
}

/// Private retained operation records, separate from saved connections and shipped Settings.
pub(crate) trait RemoteOperationStore: Send + Sync {
    fn load(&self) -> Result<Vec<RetainedRemoteOperation>, SshIntentStoreError>;
    fn retain(&self, operation: RetainedRemoteOperation) -> Result<(), SshIntentStoreError>;
    fn retire(&self, operation: [u8; 32]) -> Result<(), SshIntentStoreError>;
    /// The nonsecret identifier this client records on the operations it submits, created on
    /// first use and retained afterwards.
    fn client(&self) -> Result<[u8; 32], SshIntentStoreError>;
}

/// The desktop's owner, or `None` in a build or test that installs none.
pub(crate) fn installed(cx: &gpui::App) -> Option<Arc<dyn RemoteServerOperations>> {
    cx.try_global::<InstalledRemoteOperations>()
        .map(|installed| Arc::clone(&installed.0))
}

/// Makes `operations` the desktop's owner.
pub(crate) fn install(operations: Arc<dyn RemoteServerOperations>, cx: &mut gpui::App) {
    cx.set_global(InstalledRemoteOperations(operations));
}

struct InstalledRemoteOperations(Arc<dyn RemoteServerOperations>);

impl gpui::Global for InstalledRemoteOperations {}
