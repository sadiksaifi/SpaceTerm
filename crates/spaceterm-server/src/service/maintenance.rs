//! The exact maintenance contract one SpaceTerm Client submits to an account's service.
//!
//! A client reaching this account over its own authority, such as the SSH account route, asks for
//! the same service and update operations the local control surfaces offer. Every exchange is one
//! bounded request and one bounded reply, so a lost reply leaves the submitted operation resolvable
//! rather than repeated.
//!
//! Two rules shape this contract. An operation carries the identity the client bound before
//! dispatch, so the service recognizes a resubmission of the same original instead of performing
//! new work. A destructive request also carries the confirmation the service issued for one exact
//! observation, so a confirmation taken when the server was running never applies to work started
//! later.
//!
//! Only typed classifications and values that name a build cross this boundary. Account paths,
//! native errors and host text never do.

use serde::{Deserialize, Serialize};

pub use super::control::Action;

/// What one maintenance invocation asks of the account's service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Reads service policy, the approved build and any recorded original operation. Starts,
    /// stages, installs and stops nothing.
    Observe,
    /// Describes one destructive action and issues the confirmation for the observation it
    /// describes. Changes nothing.
    Prepare { action: Action },
    /// Describes activation of one exact staged candidate and issues its confirmation. Changes
    /// nothing. An artifact that is already the approved build needs no update.
    PrepareUpdate { candidate: String },
    /// Describes returning to the retained previous runtime of an update that never became ready.
    PrepareReturn,
    /// Performs exactly the described action once, under `operation`. Resubmitting the same
    /// `operation` returns the retained outcome rather than dispatching again.
    Apply {
        operation: [u8; 32],
        action: Action,
        confirmation: String,
    },
    /// Performs exactly the described activation once, under `operation`.
    ApplyUpdate {
        operation: [u8; 32],
        candidate: String,
        confirmation: String,
    },
    /// Performs exactly the described return once, under `operation`.
    ApplyReturn {
        operation: [u8; 32],
        confirmation: String,
    },
    /// Starts the server explicitly, which is the only operation that reverses deliberate Stop.
    Start { operation: [u8; 32] },
    /// Resolves an already-submitted original's outcome. It dispatches no destructive work,
    /// chooses no artifact and renews no approval.
    Resolve { operation: [u8; 32] },
}

/// What the service answers. Exactly one reply is written for one request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Facts(MaintenanceFacts),
    Prepared(PreparedChange),
    /// The verified artifact transferred is retained and ready to be activated.
    Staged { candidate: RemoteBuild },
    /// The exact artifact is already this account's approved build, so no update is needed.
    AlreadyApproved,
    Outcome(OperationOutcome),
    /// This service holds no record of that original operation.
    Unknown,
    Refused(MaintenanceRefusal),
}

/// A server build as a person and a confirmation name it: never a path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBuild {
    pub version: String,
    pub commit: String,
    /// The whole signed package's lowercase SHA-256 digest.
    pub digest: String,
}

/// What one observation found about the account's service, short of changing it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceFacts {
    pub server: [u8; 32],
    pub account: u32,
    pub build: String,
    pub status: super::ServiceStatus,
    pub startup: super::ServiceStartup,
    /// The durably approved build, or `None` while no service is installed.
    pub approved: Option<RemoteBuild>,
    /// The last update or return this service records, resolved or not.
    pub activation: Option<RemoteActivation>,
}

/// The original update or return this service retains, which recovery resolves.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteActivation {
    pub operation: [u8; 32],
    pub previous: RemoteBuild,
    pub candidate: RemoteBuild,
    pub stopped: bool,
    pub phase: RemoteActivationPhase,
    pub returned: bool,
}

/// How far the retained update got, as a control surface presents it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteActivationPhase {
    Activating,
    Completed { running: bool },
    Failed { can_return: bool },
    Uncertain,
}

/// A change the service described and bound to the observation it made. Only an Apply carrying
/// this confirmation performs it, once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedChange {
    /// The exact-observation confirmation. Apply recomputes it and refuses a changed observation.
    pub confirmation: String,
    pub running: bool,
    pub stopped: bool,
    pub startup: super::ServiceStartup,
    /// The approved build the change replaces, for an update or return.
    pub previous: Option<RemoteBuild>,
    /// The verified build the change installs, for an update or return.
    pub candidate: Option<RemoteBuild>,
}

/// One original operation's outcome as the service validated it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationOutcome {
    pub operation: [u8; 32],
    pub kind: OperationKind,
    pub resolution: OperationResolution,
    /// The service policy observed after the operation resolved, when it could be read.
    pub status: Option<super::ServiceStatus>,
}

/// What an original operation asked for, retained with it so a resubmission cannot change it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationKind {
    Start,
    Service { action: Action },
    Update { candidate: RemoteBuild },
    Return { previous: RemoteBuild },
}

/// Whether an original operation finished, and how. A reply arriving is not completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationResolution {
    /// The service performed it and validated the result.
    Completed,
    /// The service admitted it and it is still underway. Resolve reports it again.
    Admitted,
    /// The service refused it before any effect.
    Refused(MaintenanceRefusal),
    /// It was admitted and its result could not be validated, so it stays unconfirmed.
    Unconfirmed,
    /// Activation failed before the new runtime became ready.
    Failed { can_return: bool },
}

/// Why the service refused, as a bounded classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceRefusal {
    /// This account has no installed service.
    NotInstalled,
    /// The server is not running, and the request needed it running.
    NotRunning,
    /// The server was deliberately stopped. Only explicit Start reverses that.
    Stopped,
    /// The service's observation differs from the one the confirmation named.
    Changed,
    /// The operating system requires an authorization the service does not have.
    AuthorizationRequired,
    /// No verified retained artifact matches the request.
    CandidateUnavailable,
    /// The artifact named does not verify for this account, build or target.
    CandidateChanged,
    /// Returning to the previous runtime could not be proved safe.
    ReturnUnavailable,
    /// The account's service storage is unavailable or unsafely owned.
    StorageUnavailable,
    /// The request did not parse, or named another application identity.
    Protocol,
    /// This service does not offer the requested operation.
    Unsupported,
}
