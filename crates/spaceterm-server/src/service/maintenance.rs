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

use super::{
    Run, Service, ServiceStartup, ServiceStatus,
    activation::{ActivationPhase, ActivationStatus},
    control::{self, Controller},
    runtime,
};
use crate::{ServerError, service::RetainedInstallation, storage::Directory};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::{Read, Write},
};

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
    Staged {
        candidate: RemoteBuild,
    },
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

/// The largest request this service reads. The longest one names a 64-character digest and a
/// 64-character confirmation, so a request never approaches this bound.
const MAX_REQUEST: u64 = 8 * 1024;

/// How many originals this service retains. A person maintains one server from a few surfaces at
/// a time, each original is resolved on the next Connect or Resume, and an unresolved original is
/// never evicted, so eight keeps every plausible outstanding original in a few kilobytes.
const RETAINED: usize = 8;

/// Account-authorized maintenance of one application identity's service.
///
/// One instance answers exactly one request. Every operation reuses the local service controls
/// and their confirmations, so remote and local maintenance share one policy.
pub struct Maintenance {
    service: Service,
}

impl Maintenance {
    /// Resolve the account's service for the compiled application identity.
    ///
    /// `requested` is the identity the caller named. A different identity is refused before any
    /// account path resolution or service access.
    pub fn for_identity(identity: &str, requested: &str) -> Result<Self, MaintenanceRefusal> {
        if requested != identity {
            return Err(MaintenanceRefusal::Protocol);
        }
        Ok(Self::new(Service::for_identity(identity)?))
    }

    fn new(service: Service) -> Self {
        Self { service }
    }

    /// Read one bounded request and write exactly one reply.
    pub fn exchange(
        &self,
        input: &mut impl Read,
        output: &mut impl Write,
    ) -> Result<u8, control::cli::Error> {
        let reply = match read_request(input) {
            Ok(request) => self.reply(request),
            Err(refusal) => Reply::Refused(refusal),
        };
        write_reply(&reply, output)
    }

    /// Perform one request and answer it. Every refusal is classified in one place.
    pub fn reply(&self, request: Request) -> Reply {
        match self.perform(request) {
            Ok(reply) => reply,
            Err(error) => Reply::Refused(error.into()),
        }
    }

    fn perform(&self, request: Request) -> Result<Reply, ServerError> {
        match request {
            Request::Observe => Ok(Reply::Facts(self.facts()?)),
            Request::Prepare { action } => Ok(Reply::Prepared(self.prepare(action)?)),
            Request::Apply {
                operation,
                action,
                confirmation,
            } => self.apply(
                operation,
                Applied::Service {
                    action,
                    confirmation,
                },
            ),
            Request::Start { operation } => self.apply(operation, Applied::Start),
            Request::PrepareUpdate { candidate } => self.prepare_update(&candidate),
            Request::PrepareReturn => Ok(Reply::Prepared(self.prepare_return()?)),
            Request::ApplyUpdate {
                operation,
                candidate,
                confirmation,
            } => self.apply(
                operation,
                Applied::Update {
                    candidate,
                    confirmation,
                },
            ),
            Request::ApplyReturn {
                operation,
                confirmation,
            } => self.apply(operation, Applied::Return { confirmation }),
            _ => Err(ServerError::Unsupported),
        }
    }

    /// Read service policy, the approved build and any recorded original. Starts, stages,
    /// installs and stops nothing.
    fn facts(&self) -> Result<MaintenanceFacts, ServerError> {
        let observation = self.observe()?;
        let activation = match &observation.activation {
            Some(status) => Some(self.remote_activation(status)?),
            None => None,
        };
        Ok(MaintenanceFacts {
            server: observation.server,
            account: observation.account,
            build: self.service.identity.clone(),
            status: observation.status,
            startup: observation.startup,
            approved: observation.approved,
            activation,
        })
    }

    /// Describe one destructive action and issue the confirmation for the observation it
    /// describes. The local control surface owns the same preconditions, and preparing changes
    /// nothing.
    fn prepare(&self, action: Action) -> Result<PreparedChange, ServerError> {
        let observation = self.observe()?;
        Controller::new(self.service.clone()).prepare(action)?;
        Ok(PreparedChange {
            confirmation: observation
                .confirmation(&self.service.identity, &Change::Service { action })?,
            running: observation.status.running,
            stopped: observation.status.stopped,
            startup: observation.startup,
            previous: None,
            candidate: None,
        })
    }

    /// Describe activation of one exact staged candidate and issue its confirmation. Changes
    /// nothing, and an artifact that is already the approved build needs no update.
    fn prepare_update(&self, candidate: &str) -> Result<Reply, ServerError> {
        let installation = self.retained(candidate)?;
        let observation = self.observe()?;
        let Some(approval) = self.service.prepare_activation(&installation)? else {
            return Ok(Reply::AlreadyApproved);
        };
        let previous = RemoteBuild::describe(approval.previous())?;
        let candidate = RemoteBuild::describe(approval.candidate())?;
        Ok(Reply::Prepared(PreparedChange {
            confirmation: observation.confirmation(
                &self.service.identity,
                &Change::Update {
                    candidate: &candidate,
                },
            )?,
            running: approval.running(),
            stopped: approval.stopped(),
            startup: observation.startup,
            previous: Some(previous),
            candidate: Some(candidate),
        }))
    }

    /// Describe returning to the retained previous runtime of an update that never became ready.
    fn prepare_return(&self) -> Result<PreparedChange, ServerError> {
        let observation = self.observe()?;
        let approval = self.service.prepare_return()?;
        let previous = RemoteBuild::describe(approval.previous())?;
        let candidate = RemoteBuild::describe(approval.candidate())?;
        Ok(PreparedChange {
            confirmation: observation.confirmation(
                &self.service.identity,
                &Change::Return {
                    previous: &previous,
                },
            )?,
            running: observation.status.running,
            stopped: approval.stopped(),
            startup: observation.startup,
            previous: Some(previous),
            candidate: Some(candidate),
        })
    }

    /// Resolve one retained installation by its whole-package digest, exactly as the local
    /// service-update control does. No caller-supplied path reaches this account's storage.
    fn retained(&self, digest: &str) -> Result<RetainedInstallation, ServerError> {
        if !runtime::digest_valid(digest) {
            return Err(ServerError::RuntimeUnavailable);
        }
        let directory = self.service.paths.data.join("installations").join(digest);
        // A digest the client names may not be retained here. Verification still owns every
        // safety decision about the entry; this only reports an absent candidate truthfully.
        if !directory.is_dir() {
            return Err(ServerError::RuntimeUnavailable);
        }
        let (version, commit) = runtime::describe(&directory, &self.service.identity)?;
        Ok(RetainedInstallation {
            directory,
            version,
            commit,
        })
    }

    /// Perform one original exactly once under the submitted operation identity.
    ///
    /// Admission is durable before dispatch, so an interrupted original is resolvable instead of
    /// repeatable, and resubmitting the same identity returns the retained outcome.
    fn apply(&self, operation: [u8; 32], applied: Applied) -> Result<Reply, ServerError> {
        if operation == [0; 32] {
            return Err(ServerError::Protocol);
        }
        let data = Directory::open(&self.service.paths.data, false)?;
        // This fence serializes maintenance originals. The service's own operation lock belongs
        // to each dispatch, so exactly-once admission needs its own lock at the same discipline.
        let _fence = data.lock("maintenance.lock", false)?;
        data.verify_path(&self.service.paths.data)?;
        let mut journal = Journal::read(&data)?;
        if let Some(original) = journal.find(operation) {
            return Ok(Reply::Outcome(original.outcome()));
        }
        let original = match self.admit(applied)? {
            Admission::AlreadyApproved => return Ok(Reply::AlreadyApproved),
            Admission::Refused {
                kind,
                bound,
                refusal,
            } => {
                let original = Original::refused(operation, kind, bound, refusal);
                journal.retain(&data, original.clone())?;
                return Ok(Reply::Outcome(original.outcome()));
            }
            Admission::Ready {
                kind,
                bound,
                confirmation,
                dispatch,
            } => {
                let original = Original {
                    operation,
                    kind,
                    confirmation,
                    bound,
                    activation: None,
                    resolution: OperationResolution::Admitted,
                    status: None,
                };
                journal.retain(&data, original.clone())?;
                let (resolution, activation, status) = self.dispatch(dispatch);
                Original {
                    resolution,
                    activation,
                    status,
                    ..original
                }
            }
        };
        journal.retain(&data, original.clone())?;
        Ok(Reply::Outcome(original.outcome()))
    }

    /// Validate one original against a fresh observation. No branch here dispatches anything.
    fn admit(&self, applied: Applied) -> Result<Admission, ServerError> {
        let controller = Controller::new(self.service.clone());
        match applied {
            // Start carries no confirmation: it ends no work, and it is the only operation that
            // reverses a deliberate Stop.
            Applied::Start => Ok(Admission::Ready {
                kind: OperationKind::Start,
                bound: self.observe()?.status,
                confirmation: None,
                dispatch: Dispatch::Start,
            }),
            Applied::Service {
                action,
                confirmation,
            } => {
                let observation = self.observe()?;
                let kind = OperationKind::Service { action };
                let expected = observation
                    .confirmation(&self.service.identity, &Change::Service { action })?;
                if expected != confirmation {
                    return Ok(Admission::Refused {
                        kind,
                        bound: observation.status,
                        refusal: MaintenanceRefusal::Changed,
                    });
                }
                match controller.prepare(action) {
                    Ok(prepared) => Ok(Admission::Ready {
                        kind,
                        bound: observation.status,
                        confirmation: Some(expected),
                        dispatch: Dispatch::Service(prepared),
                    }),
                    Err(error) => Ok(Admission::Refused {
                        kind,
                        bound: observation.status,
                        refusal: error.into(),
                    }),
                }
            }
            Applied::Update {
                candidate,
                confirmation,
            } => {
                let installation = self.retained(&candidate)?;
                let candidate = RemoteBuild::describe(&installation)?;
                let kind = OperationKind::Update {
                    candidate: candidate.clone(),
                };
                let observation = self.observe()?;
                let expected = observation.confirmation(
                    &self.service.identity,
                    &Change::Update {
                        candidate: &candidate,
                    },
                )?;
                if expected != confirmation {
                    return Ok(Admission::Refused {
                        kind,
                        bound: observation.status,
                        refusal: MaintenanceRefusal::Changed,
                    });
                }
                // A candidate that cannot be approved, including an incompatible one, neither
                // stages nor activates anything.
                match self.service.prepare_activation(&installation) {
                    Ok(None) => Ok(Admission::AlreadyApproved),
                    Ok(Some(approval)) => Ok(Admission::Ready {
                        kind,
                        bound: observation.status,
                        confirmation: Some(expected),
                        dispatch: Dispatch::Update(approval),
                    }),
                    Err(error) => Ok(Admission::Refused {
                        kind,
                        bound: observation.status,
                        refusal: error.into(),
                    }),
                }
            }
            Applied::Return { confirmation } => {
                let observation = self.observe()?;
                let approval = self.service.prepare_return()?;
                let previous = RemoteBuild::describe(approval.previous())?;
                let kind = OperationKind::Return {
                    previous: previous.clone(),
                };
                let expected = observation.confirmation(
                    &self.service.identity,
                    &Change::Return {
                        previous: &previous,
                    },
                )?;
                if expected != confirmation {
                    return Ok(Admission::Refused {
                        kind,
                        bound: observation.status,
                        refusal: MaintenanceRefusal::Changed,
                    });
                }
                Ok(Admission::Ready {
                    kind,
                    bound: observation.status,
                    confirmation: Some(expected),
                    dispatch: Dispatch::Return(approval),
                })
            }
        }
    }

    /// Perform exactly one admitted original and validate what it achieved. A reply is not
    /// completion: an unvalidated result stays unconfirmed.
    fn dispatch(
        &self,
        dispatch: Dispatch,
    ) -> (OperationResolution, Option<[u8; 32]>, Option<ServiceStatus>) {
        let controller = Controller::new(self.service.clone());
        match dispatch {
            Dispatch::Start => match controller.start() {
                // Only an observed running server completes an explicit Start.
                Ok(status) if status.running => {
                    (OperationResolution::Completed, None, Some(status))
                }
                Ok(status) => (OperationResolution::Unconfirmed, None, Some(status)),
                Err(error) => (resolution_after(error), None, self.policy()),
            },
            Dispatch::Service(prepared) => match controller.confirm(prepared) {
                Ok(status) => (OperationResolution::Completed, None, Some(status)),
                Err(error) => (resolution_after(error), None, self.policy()),
            },
            Dispatch::Update(approval) => self.activated(self.service.activate(approval)),
            Dispatch::Return(approval) => self.activated(self.service.return_to_previous(approval)),
        }
    }

    /// Report what the service retains for an activation rather than what its reply claimed.
    fn activated(
        &self,
        result: Result<ActivationStatus, ServerError>,
    ) -> (OperationResolution, Option<[u8; 32]>, Option<ServiceStatus>) {
        match result {
            Ok(status) => (
                resolution_of(status.phase),
                Some(status.operation),
                self.policy(),
            ),
            Err(error) => match resolution_after(error) {
                // Refused before the service admitted any activation.
                refused @ OperationResolution::Refused(_) => (refused, None, self.policy()),
                unresolved => match self.service.activation_status() {
                    Ok(Some(status)) => (
                        resolution_of(status.phase),
                        Some(status.operation),
                        self.policy(),
                    ),
                    _ => (unresolved, None, self.policy()),
                },
            },
        }
    }

    /// The service policy after an original resolved, when it can still be read.
    fn policy(&self) -> Option<ServiceStatus> {
        self.observe().ok().map(|observation| observation.status)
    }

    /// Observe this account's service once, without dispatching or recovering anything.
    fn observe(&self) -> Result<Observation, ServerError> {
        let data = Directory::open(&self.service.paths.data, false)?;
        let _operation = data.lock("service.lock", false)?;
        data.verify_path(&self.service.paths.data)?;
        let server = data.read()?.server;
        let state = Service::state(&data)?;
        let (status, approved, activation) = match &state {
            Some(state) => (
                self.service.snapshot(state)?,
                match state.installed {
                    true => Some(RemoteBuild::describe(
                        &self.service.describe_approved(state)?,
                    )?),
                    false => None,
                },
                self.service.activation_status_locked(&data)?,
            ),
            None => (ServiceStatus::default(), None, None),
        };
        Ok(Observation {
            server,
            // SAFETY: geteuid has no preconditions.
            account: unsafe { libc::geteuid() },
            boot: super::boot()?,
            status,
            startup: self.service.startup(),
            approved,
            run: state.and_then(|state| state.run),
            activation,
        })
    }

    fn remote_activation(
        &self,
        status: &ActivationStatus,
    ) -> Result<RemoteActivation, ServerError> {
        Ok(RemoteActivation {
            operation: status.operation,
            previous: RemoteBuild::describe(&status.previous)?,
            candidate: RemoteBuild::describe(&status.candidate)?,
            stopped: status.stopped,
            phase: match status.phase {
                ActivationPhase::Activating => RemoteActivationPhase::Activating,
                ActivationPhase::Completed { running } => {
                    RemoteActivationPhase::Completed { running }
                }
                ActivationPhase::Failed { can_return } => {
                    RemoteActivationPhase::Failed { can_return }
                }
                ActivationPhase::Uncertain => RemoteActivationPhase::Uncertain,
            },
            returned: status.returned,
        })
    }
}

/// One exact observation of the account's service. A confirmation binds this, so a confirmation
/// taken while the server was running never applies to work started afterwards.
struct Observation {
    server: [u8; 32],
    account: u32,
    boot: String,
    status: ServiceStatus,
    startup: ServiceStartup,
    approved: Option<RemoteBuild>,
    /// The server's current process run, which changes whenever the server runs again.
    run: Option<Run>,
    activation: Option<ActivationStatus>,
}

/// What one submitted original asks this service to perform once.
enum Applied {
    Start,
    Service {
        action: Action,
        confirmation: String,
    },
    Update {
        candidate: String,
        confirmation: String,
    },
    Return {
        confirmation: String,
    },
}

/// One validated original, ready for its single dispatch.
enum Dispatch {
    Start,
    Service(control::Confirmation),
    Update(super::activation::ActivationApproval),
    Return(super::activation::ReturnApproval),
}

/// What admission decided before anything was performed.
enum Admission {
    Ready {
        kind: OperationKind,
        bound: ServiceStatus,
        confirmation: Option<String>,
        dispatch: Dispatch,
    },
    /// Refused before any effect, with the original known well enough to retain its refusal.
    Refused {
        kind: OperationKind,
        bound: ServiceStatus,
        refusal: MaintenanceRefusal,
    },
    /// The exact artifact is already approved, so the request names no change.
    AlreadyApproved,
}

/// Only the guards that precede any durable change refuse an admitted original. Every other
/// failure leaves its result unvalidated rather than claiming that nothing happened.
fn resolution_after(error: ServerError) -> OperationResolution {
    match error {
        ServerError::NotInstalled
        | ServerError::Stopped
        | ServerError::Changed
        | ServerError::AuthenticationRefused => OperationResolution::Refused(error.into()),
        _ => OperationResolution::Unconfirmed,
    }
}

/// How far a retained activation got, as one original's outcome.
fn resolution_of(phase: ActivationPhase) -> OperationResolution {
    match phase {
        ActivationPhase::Activating => OperationResolution::Admitted,
        ActivationPhase::Completed { .. } => OperationResolution::Completed,
        ActivationPhase::Failed { can_return } => OperationResolution::Failed { can_return },
        ActivationPhase::Uncertain => OperationResolution::Unconfirmed,
    }
}

/// One original operation as this service retains it: what it asked for, the observation it
/// bound, how far it got and its validated outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Original {
    operation: [u8; 32],
    kind: OperationKind,
    /// The confirmation the client submitted, which named one exact observation.
    confirmation: Option<String>,
    /// The service policy this original bound.
    bound: ServiceStatus,
    /// The service activation this original admitted, once the service generated it.
    activation: Option<[u8; 32]>,
    resolution: OperationResolution,
    /// The service policy observed after the original resolved, when it could be read.
    status: Option<ServiceStatus>,
}
impl Original {
    fn refused(
        operation: [u8; 32],
        kind: OperationKind,
        bound: ServiceStatus,
        refusal: MaintenanceRefusal,
    ) -> Self {
        Self {
            operation,
            kind,
            confirmation: None,
            bound,
            activation: None,
            resolution: OperationResolution::Refused(refusal),
            status: None,
        }
    }
    fn outcome(&self) -> OperationOutcome {
        OperationOutcome {
            operation: self.operation,
            kind: self.kind.clone(),
            resolution: self.resolution,
            status: self.status,
        }
    }
    /// Only an unresolved original still needs resolving, so only it may never be evicted.
    fn resolved(&self) -> bool {
        !matches!(
            self.resolution,
            OperationResolution::Admitted | OperationResolution::Unconfirmed
        )
    }
}

/// The account's retained originals, in a private file beside its service policy.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    originals: Vec<Original>,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            schema: 1,
            originals: Vec::new(),
        }
    }
}
impl Journal {
    /// Read the retained originals. Unreadable or unexpected contents are refused, never
    /// replaced, because an original this service cannot read may still be unresolved.
    fn read(data: &Directory) -> Result<Self, ServerError> {
        let journal: Self = data.read_json("maintenance.json")?.unwrap_or_default();
        let readable = journal.schema == 1
            && journal.originals.len() <= RETAINED
            && journal
                .originals
                .iter()
                .enumerate()
                .all(|(index, original)| {
                    original.operation != [0; 32]
                        && !journal.originals[..index]
                            .iter()
                            .any(|earlier| earlier.operation == original.operation)
                });
        if !readable {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(journal)
    }
    fn find(&self, operation: [u8; 32]) -> Option<&Original> {
        self.originals
            .iter()
            .find(|original| original.operation == operation)
    }
    /// Publish one original durably. The caller holds the maintenance fence.
    fn retain(&mut self, data: &Directory, original: Original) -> Result<(), ServerError> {
        match self
            .originals
            .iter()
            .position(|retained| retained.operation == original.operation)
        {
            Some(index) => self.originals[index] = original,
            None => {
                while self.originals.len() >= RETAINED {
                    let Some(index) = self.originals.iter().position(Original::resolved) else {
                        // Every retained original still needs resolving, so none may be evicted.
                        return Err(ServerError::StorageUnavailable);
                    };
                    self.originals.remove(index);
                }
                self.originals.push(original);
            }
        }
        data.write_json(c"maintenance.json", self)
    }
}

impl Observation {
    /// The nonsecret exact-observation confirmation for one change.
    ///
    /// It binds the application identity, server identity, account, boot, approved build and
    /// installed, enabled, running and stopped policy, together with the server's process run.
    /// Apply recomputes it, so a confirmation taken while the server was running cannot authorize
    /// stopping work that started afterwards.
    fn confirmation(&self, identity: &str, change: &Change<'_>) -> Result<String, ServerError> {
        super::activation::confirmation_token(&Binding {
            identity,
            server: self.server,
            account: self.account,
            boot: &self.boot,
            status: self.status,
            startup: self.startup,
            approved: self.approved.as_ref(),
            run: self.run.as_ref(),
            change,
        })
    }
}

#[derive(Serialize)]
struct Binding<'a> {
    identity: &'a str,
    server: [u8; 32],
    account: u32,
    boot: &'a str,
    status: ServiceStatus,
    startup: ServiceStartup,
    approved: Option<&'a RemoteBuild>,
    run: Option<&'a Run>,
    change: &'a Change<'a>,
}

/// What one confirmation authorizes. A confirmation for one change never authorizes another.
#[derive(Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
enum Change<'a> {
    Service { action: Action },
    Update { candidate: &'a RemoteBuild },
    Return { previous: &'a RemoteBuild },
}

impl RemoteBuild {
    /// Name a verified retained build without exposing its path.
    ///
    /// Only bounded nonsecret values cross the maintenance boundary, so metadata outside the
    /// signed build document's shape is refused rather than forwarded.
    pub fn describe(installation: &RetainedInstallation) -> Result<Self, ServerError> {
        let digest = installation
            .directory
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| runtime::digest_valid(name))
            .ok_or(ServerError::RuntimeUnavailable)?;
        if installation.version.len() > 64
            || !installation
                .version
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || v == b'.' || v == b'-')
            || installation.commit.len() != 40
            || !installation
                .commit
                .bytes()
                .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
        {
            return Err(ServerError::RuntimeUnavailable);
        }
        Ok(Self {
            version: installation.version.clone(),
            commit: installation.commit.clone(),
            digest: digest.into(),
        })
    }
}

/// The one place a service failure becomes a bounded maintenance classification. No native error
/// text, path or host value crosses this boundary.
impl From<ServerError> for MaintenanceRefusal {
    fn from(error: ServerError) -> Self {
        match error {
            ServerError::NotSetUp | ServerError::NotInstalled => Self::NotInstalled,
            ServerError::NotRunning => Self::NotRunning,
            ServerError::Stopped => Self::Stopped,
            // An unresolved original and unfinished cleanup both mean the service is no longer in
            // the state the request named, so a fresh observation is required.
            ServerError::Changed
            | ServerError::ActivationUnresolved
            | ServerError::CleanupUnresolved => Self::Changed,
            ServerError::ReturnUnavailable => Self::ReturnUnavailable,
            // The account's service manager needs an operating-system session or authorization
            // this service does not have.
            ServerError::AuthorizationRequired | ServerError::ServiceUnavailable => {
                Self::AuthorizationRequired
            }
            ServerError::CandidateChanged => Self::CandidateChanged,
            // Only a verified compatible retained artifact is a candidate for this account.
            ServerError::RuntimeUnavailable
            | ServerError::IncompatibleProtocol
            | ServerError::MissingRequiredCapability => Self::CandidateUnavailable,
            ServerError::StorageUnavailable
            | ServerError::UnsafeStorage
            | ServerError::IncompatibleState => Self::StorageUnavailable,
            ServerError::Protocol | ServerError::AuthenticationRefused => Self::Protocol,
            // No maintenance request produces the remaining conditions, and none of them names an
            // operation this service can offer.
            _ => Self::Unsupported,
        }
    }
}

/// Execute one maintenance exchange for arguments excluding the executable name.
///
/// `identity` is the caller's compiled application identity, which the arguments must name
/// exactly. Validation precedes account-path resolution and service access. Exactly one bounded
/// reply is written, including for every refusal, so a caller never has to interpret an exit code.
pub fn run(
    identity: &str,
    args: &[OsString],
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<u8, control::cli::Error> {
    let [command, requested] = args else {
        return write_reply(&Reply::Refused(MaintenanceRefusal::Protocol), output);
    };
    if command != "ssh-maintain" {
        return write_reply(&Reply::Refused(MaintenanceRefusal::Protocol), output);
    }
    let requested = requested.to_str().unwrap_or_default();
    match Maintenance::for_identity(identity, requested) {
        Ok(maintenance) => maintenance.exchange(input, output),
        Err(refusal) => write_reply(&Reply::Refused(refusal), output),
    }
}

fn write_reply(reply: &Reply, output: &mut impl Write) -> Result<u8, control::cli::Error> {
    let mut bytes = serde_json::to_vec(reply).map_err(|_| control::cli::Error::Output)?;
    bytes.push(b'\n');
    output
        .write_all(&bytes)
        .and_then(|()| output.flush())
        .map_err(|_| control::cli::Error::Output)?;
    Ok(0)
}

fn read_request(input: &mut impl Read) -> Result<Request, MaintenanceRefusal> {
    let mut bytes = Vec::new();
    input
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MaintenanceRefusal::Protocol)?;
    if bytes.len() as u64 > MAX_REQUEST {
        return Err(MaintenanceRefusal::Protocol);
    }
    serde_json::from_slice(&bytes).map_err(|_| MaintenanceRefusal::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{
        State, Supervisor,
        runtime::test_support::{COMMIT, VERSION, target, with_artifact},
    };
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    const IDENTITY: &str = "spaceterm-development";

    /// A service manager that counts every dispatch, so a test can prove that a refused request
    /// performed nothing and that an admitted original was performed once.
    struct ServiceManager {
        data: PathBuf,
        approved: Mutex<PathBuf>,
        running: AtomicBool,
        stops: AtomicUsize,
        starts: AtomicUsize,
        uninstalls: AtomicUsize,
        fail_replace: AtomicBool,
    }
    impl Supervisor for ServiceManager {
        fn startup(&self) -> ServiceStartup {
            ServiceStartup::AtLogin
        }
        fn check(&self, runtime: &Path, _: bool) -> Result<(), ServerError> {
            if *self.approved.lock().unwrap() == runtime {
                Ok(())
            } else {
                Err(ServerError::CleanupUnresolved)
            }
        }
        fn install(&self, _: &Path) -> Result<(), ServerError> {
            panic!("maintenance never installs a fresh service")
        }
        fn prepare_start(&self) -> Result<(), ServerError> {
            Ok(())
        }
        fn start(&self) -> Result<(), ServerError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Err(ServerError::StartupFailed)
        }
        fn disable(&self) -> Result<(), ServerError> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            // A real server process clears its own run marker as it exits.
            let data = Directory::open(&self.data, false)?;
            let _intent = data.lock("service-intent.lock", false)?;
            if let Some(mut state) = Service::state(&data)? {
                state.run = None;
                data.write_json(c"service.json", &state)?;
            }
            Ok(())
        }
        fn uninstall(&self, _: &Path) -> Result<(), ServerError> {
            self.uninstalls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn running(&self) -> Result<bool, ServerError> {
            Ok(self.running.load(Ordering::SeqCst))
        }
        fn replace(&self, previous: &Path, candidate: &Path) -> Result<(), ServerError> {
            let mut approved = self.approved.lock().unwrap();
            assert!(approved.as_path() == previous || approved.as_path() == candidate);
            *approved = candidate.to_owned();
            if self.fail_replace.load(Ordering::SeqCst) {
                Err(ServerError::ServiceUnavailable)
            } else {
                Ok(())
            }
        }
    }

    struct Harness {
        maintenance: Maintenance,
        service: Service,
        previous: RetainedInstallation,
        candidate: RetainedInstallation,
        manager: Arc<ServiceManager>,
    }
    impl Harness {
        fn data(&self) -> Directory {
            Directory::open(&self.service.paths.data, false).unwrap()
        }
        fn state(&self) -> State {
            Service::state(&self.data()).unwrap().unwrap()
        }
        fn write_state(&self, state: &State) {
            self.data().write_json(c"service.json", state).unwrap();
        }
        /// Exchange one request over the same bounded bytes the SSH route carries.
        fn exchange(&self, request: &Request) -> Reply {
            let (reply, text) = self.wire(&serde_json::to_vec(request).unwrap());
            assert!(!text.contains('/'), "a reply named a path: {text}");
            reply
        }
        fn wire(&self, request: &[u8]) -> (Reply, String) {
            let mut output = Vec::new();
            assert_eq!(
                self.maintenance.exchange(&mut &request[..], &mut output),
                Ok(0)
            );
            let text = String::from_utf8(output).unwrap();
            (serde_json::from_str(&text).unwrap(), text)
        }
        fn digest(installation: &RetainedInstallation) -> String {
            RemoteBuild::describe(installation).unwrap().digest
        }
    }

    /// An installed service whose approved runtime is `previous` and whose staged different
    /// artifact is `candidate`.
    fn with_service(running: bool, stopped: bool, exercise: impl FnOnce(&Harness)) {
        use ring::signature::KeyPair;
        let fixture = crate::service::tests::Fixture::new();
        with_artifact(
            &fixture.root.join("source"),
            &fixture.paths.identity,
            |archive, signature| {
                let manager = Arc::new(ServiceManager {
                    data: fixture.paths.data.clone(),
                    approved: Mutex::new(PathBuf::new()),
                    running: AtomicBool::new(running),
                    stops: AtomicUsize::new(0),
                    starts: AtomicUsize::new(0),
                    uninstalls: AtomicUsize::new(0),
                    fail_replace: AtomicBool::new(false),
                });
                let service = Service::new(
                    fixture.paths.clone(),
                    fixture.paths.identity.clone(),
                    manager.clone(),
                );
                let retain = |service: &Service| {
                    service
                        .provision(archive, signature, target(), COMMIT, VERSION)
                        .unwrap()
                };
                let previous = retain(&service);
                let mut bytes = std::fs::read(archive).unwrap();
                bytes[4] = 1; // A signed gzip timestamp changes the archive, not its files.
                std::fs::write(archive, &bytes).unwrap();
                let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
                std::fs::write(signature, key.sign(&bytes).as_ref()).unwrap();
                let candidate = retain(&service);
                assert_ne!(previous.directory, candidate.directory);
                *manager.approved.lock().unwrap() = previous.directory.clone();
                let harness = Harness {
                    maintenance: Maintenance::new(service.clone()),
                    service,
                    previous,
                    candidate,
                    manager,
                };
                let mut state = harness.state();
                state.installation = Harness::digest(&harness.previous);
                state.stopped = stopped;
                state.run = running.then(|| Run {
                    boot: crate::service::boot().unwrap(),
                    token: [1; 32],
                });
                harness.write_state(&state);
                exercise(&harness);
            },
        );
    }

    #[test]
    fn a_request_naming_another_application_identity_is_refused_before_service_access() {
        with_service(false, false, |harness| {
            for arguments in [
                vec!["ssh-maintain", "spaceterm"],
                vec!["ssh-maintain", "private-foreign-identity"],
                vec!["ssh-maintain"],
                vec!["ssh-maintain", IDENTITY, "--confirm"],
                vec!["ssh-stage", IDENTITY],
                vec!["service-stop", IDENTITY],
            ] {
                let arguments: Vec<OsString> = arguments.into_iter().map(OsString::from).collect();
                let mut output = Vec::new();
                let request = serde_json::to_vec(&Request::Observe).unwrap();
                assert_eq!(
                    run(IDENTITY, &arguments, &mut &request[..], &mut output),
                    Ok(0)
                );
                assert_eq!(
                    serde_json::from_slice::<Reply>(&output).unwrap(),
                    Reply::Refused(MaintenanceRefusal::Protocol)
                );
            }
            // Nothing above resolved this account's service, so its policy is untouched.
            assert!(!harness.state().stopped);
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn an_unreadable_or_oversized_request_is_refused_without_an_operation() {
        with_service(false, false, |harness| {
            for request in [
                Vec::new(),
                b"{".to_vec(),
                br#"{"request":"private_unknown"}"#.to_vec(),
                vec![b' '; MAX_REQUEST as usize + 1],
            ] {
                assert_eq!(
                    harness.wire(&request).0,
                    Reply::Refused(MaintenanceRefusal::Protocol)
                );
            }
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn observation_names_the_approved_build_without_a_path_or_native_error_text() {
        with_service(true, false, |harness| {
            let Reply::Facts(facts) = harness.exchange(&Request::Observe) else {
                panic!("observation unavailable");
            };
            assert_eq!(facts.build, IDENTITY);
            assert_eq!(
                facts.approved,
                Some(RemoteBuild::describe(&harness.previous).unwrap())
            );
            assert_eq!(facts.activation, None);
            assert!(facts.status.installed && facts.status.running && !facts.status.stopped);
            assert_ne!(facts.server, [0; 32]);
            // A refusal carries its classification only, never the service's own error text.
            std::fs::remove_dir_all(&harness.previous.directory).unwrap();
            let (reply, text) = harness.wire(&serde_json::to_vec(&Request::Observe).unwrap());
            assert_eq!(
                reply,
                Reply::Refused(MaintenanceRefusal::StorageUnavailable)
            );
            assert!(!text.contains('/'));
            assert!(!text.contains(&ServerError::StorageUnavailable.to_string()));
        });
    }

    #[test]
    fn a_prepared_confirmation_binds_the_run_it_observed_and_changes_nothing() {
        with_service(true, false, |harness| {
            let request = Request::Prepare {
                action: Action::Stop,
            };
            let Reply::Prepared(prepared) = harness.exchange(&request) else {
                panic!("preparation unavailable");
            };
            assert!(prepared.running && !prepared.stopped);
            assert_eq!(prepared.startup, ServiceStartup::AtLogin);
            assert_eq!((prepared.previous, prepared.candidate), (None, None));
            assert_eq!(prepared.confirmation.len(), 64);
            // Preparing reads policy only.
            assert!(!harness.state().stopped);
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
            // The same observation repeats its confirmation; another action never shares it.
            let Reply::Prepared(again) = harness.exchange(&request) else {
                panic!("preparation unavailable");
            };
            assert_eq!(again.confirmation, prepared.confirmation);
            let Reply::Prepared(restart) = harness.exchange(&Request::Prepare {
                action: Action::Restart,
            }) else {
                panic!("preparation unavailable");
            };
            assert_ne!(restart.confirmation, prepared.confirmation);
            // A new process run is a different observation.
            let mut state = harness.state();
            state.run = Some(Run {
                boot: crate::service::boot().unwrap(),
                token: [2; 32],
            });
            harness.write_state(&state);
            let Reply::Prepared(after) = harness.exchange(&request) else {
                panic!("preparation unavailable");
            };
            assert_ne!(after.confirmation, prepared.confirmation);
        });
    }

    #[test]
    fn preparation_refuses_an_action_the_local_control_surface_refuses() {
        with_service(false, true, |harness| {
            for action in [Action::Stop, Action::Restart] {
                assert_eq!(
                    harness.exchange(&Request::Prepare { action }),
                    Reply::Refused(MaintenanceRefusal::Stopped)
                );
            }
            let mut state = harness.state();
            state.installed = false;
            harness.write_state(&state);
            assert_eq!(
                harness.exchange(&Request::Prepare {
                    action: Action::Uninstall
                }),
                Reply::Refused(MaintenanceRefusal::NotInstalled)
            );
        });
    }

    #[test]
    fn one_admitted_original_is_performed_once_and_keeps_its_retained_outcome() {
        with_service(true, false, |harness| {
            let operation = [9; 32];
            let Reply::Prepared(prepared) = harness.exchange(&Request::Prepare {
                action: Action::Stop,
            }) else {
                panic!("preparation unavailable");
            };
            let apply = Request::Apply {
                operation,
                action: Action::Stop,
                confirmation: prepared.confirmation,
            };
            let Reply::Outcome(outcome) = harness.exchange(&apply) else {
                panic!("the original was not performed");
            };
            assert_eq!(outcome.operation, operation);
            assert_eq!(
                outcome.kind,
                OperationKind::Service {
                    action: Action::Stop
                }
            );
            assert_eq!(outcome.resolution, OperationResolution::Completed);
            let status = outcome.status.expect("resolved policy unavailable");
            assert!(status.stopped && !status.running && !status.enabled);
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 1);
            // The retained originals stay private to the owning account.
            let retained = harness.service.paths.data.join("maintenance.json");
            let mode = std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&retained).unwrap().permissions(),
            );
            assert_eq!(mode & 0o777, 0o600);
            // A resubmission returns the retained outcome and dispatches nothing, including when
            // it names a different action under the same original identity.
            for request in [
                apply,
                Request::Apply {
                    operation,
                    action: Action::Uninstall,
                    confirmation: "a".repeat(64),
                },
                Request::Start { operation },
            ] {
                assert_eq!(harness.exchange(&request), Reply::Outcome(outcome.clone()));
            }
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 1);
            assert_eq!(harness.manager.uninstalls.load(Ordering::SeqCst), 0);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn a_confirmation_bound_while_running_cannot_stop_work_started_afterwards() {
        with_service(true, false, |harness| {
            let Reply::Prepared(prepared) = harness.exchange(&Request::Prepare {
                action: Action::Stop,
            }) else {
                panic!("preparation unavailable");
            };
            // The server ran again, so the observation the confirmation named is gone.
            let mut state = harness.state();
            state.run = Some(Run {
                boot: crate::service::boot().unwrap(),
                token: [3; 32],
            });
            harness.write_state(&state);
            let apply = Request::Apply {
                operation: [4; 32],
                action: Action::Stop,
                confirmation: prepared.confirmation,
            };
            let refused = Reply::Outcome(OperationOutcome {
                operation: [4; 32],
                kind: OperationKind::Service {
                    action: Action::Stop,
                },
                resolution: OperationResolution::Refused(MaintenanceRefusal::Changed),
                status: None,
            });
            assert_eq!(harness.exchange(&apply), refused);
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
            assert!(!harness.state().stopped);
            // The refused original keeps its outcome, so only a fresh original can stop it.
            assert_eq!(harness.exchange(&apply), refused);
            let Reply::Prepared(fresh) = harness.exchange(&Request::Prepare {
                action: Action::Stop,
            }) else {
                panic!("preparation unavailable");
            };
            let Reply::Outcome(outcome) = harness.exchange(&Request::Apply {
                operation: [5; 32],
                action: Action::Stop,
                confirmation: fresh.confirmation,
            }) else {
                panic!("the original was not performed");
            };
            assert_eq!(outcome.resolution, OperationResolution::Completed);
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn an_explicit_start_needs_no_confirmation_and_is_dispatched_once() {
        with_service(false, true, |harness| {
            let request = Request::Start { operation: [6; 32] };
            let Reply::Outcome(outcome) = harness.exchange(&request) else {
                panic!("the original was not performed");
            };
            // This service manager never reaches readiness, so the result stays unconfirmed.
            assert_eq!(outcome.kind, OperationKind::Start);
            assert_eq!(outcome.resolution, OperationResolution::Unconfirmed);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 1);
            assert_eq!(harness.exchange(&request), Reply::Outcome(outcome));
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn an_unresolved_original_is_never_evicted_for_a_newer_one() {
        with_service(false, true, |harness| {
            for index in 0..RETAINED {
                let operation = [index as u8 + 1; 32];
                let Reply::Outcome(outcome) = harness.exchange(&Request::Start { operation })
                else {
                    panic!("the original was not performed");
                };
                assert_eq!(outcome.resolution, OperationResolution::Unconfirmed);
            }
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), RETAINED);
            assert_eq!(
                harness.exchange(&Request::Start {
                    operation: [200; 32]
                }),
                Reply::Refused(MaintenanceRefusal::StorageUnavailable)
            );
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), RETAINED);
            for index in 0..RETAINED {
                let operation = [index as u8 + 1; 32];
                assert!(matches!(
                    harness.exchange(&Request::Start { operation }),
                    Reply::Outcome(outcome) if outcome.operation == operation
                ));
            }
        });
    }

    #[test]
    fn a_staged_candidate_leaves_the_approved_runtime_policy_and_stop_unchanged() {
        with_service(false, true, |harness| {
            let policy = std::fs::read(harness.service.paths.data.join("service.json")).unwrap();
            let identity = std::fs::read(harness.service.paths.data.join("identity.json")).unwrap();
            let Reply::Facts(facts) = harness.exchange(&Request::Observe) else {
                panic!("observation unavailable");
            };
            assert_eq!(
                facts.approved,
                Some(RemoteBuild::describe(&harness.previous).unwrap())
            );
            assert!(facts.status.installed && facts.status.stopped && !facts.status.running);
            // The verified different artifact is retained and describable without activating it.
            let Reply::Prepared(prepared) = harness.exchange(&Request::PrepareUpdate {
                candidate: Harness::digest(&harness.candidate),
            }) else {
                panic!("the staged candidate was not described");
            };
            assert_eq!(
                (prepared.previous, prepared.candidate),
                (
                    Some(RemoteBuild::describe(&harness.previous).unwrap()),
                    Some(RemoteBuild::describe(&harness.candidate).unwrap())
                )
            );
            assert!(prepared.stopped && !prepared.running);
            assert_eq!(
                *harness.manager.approved.lock().unwrap(),
                harness.previous.directory
            );
            assert_eq!(
                policy,
                std::fs::read(harness.service.paths.data.join("service.json")).unwrap()
            );
            assert_eq!(
                identity,
                std::fs::read(harness.service.paths.data.join("identity.json")).unwrap()
            );
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 0);
            // Only a verified retained artifact is a candidate.
            for candidate in ["b".repeat(64), "not-a-digest".into()] {
                assert_eq!(
                    harness.exchange(&Request::PrepareUpdate { candidate }),
                    Reply::Refused(MaintenanceRefusal::CandidateUnavailable)
                );
            }
        });
    }

    #[test]
    fn updating_a_stopped_server_leaves_it_stopped() {
        with_service(false, true, |harness| {
            let candidate = Harness::digest(&harness.candidate);
            let Reply::Prepared(prepared) = harness.exchange(&Request::PrepareUpdate {
                candidate: candidate.clone(),
            }) else {
                panic!("the staged candidate was not described");
            };
            // A confirmation naming another observation is refused without dispatch.
            assert_eq!(
                harness.exchange(&Request::ApplyUpdate {
                    operation: [7; 32],
                    candidate: candidate.clone(),
                    confirmation: "a".repeat(64),
                }),
                Reply::Outcome(OperationOutcome {
                    operation: [7; 32],
                    kind: OperationKind::Update {
                        candidate: RemoteBuild::describe(&harness.candidate).unwrap()
                    },
                    resolution: OperationResolution::Refused(MaintenanceRefusal::Changed),
                    status: None,
                })
            );
            assert_eq!(
                *harness.manager.approved.lock().unwrap(),
                harness.previous.directory
            );
            let Reply::Outcome(outcome) = harness.exchange(&Request::ApplyUpdate {
                operation: [8; 32],
                candidate: candidate.clone(),
                confirmation: prepared.confirmation,
            }) else {
                panic!("the update was not performed");
            };
            assert_eq!(outcome.resolution, OperationResolution::Completed);
            let status = outcome.status.expect("resolved policy unavailable");
            assert!(status.installed && status.stopped && !status.running);
            assert_eq!(
                *harness.manager.approved.lock().unwrap(),
                harness.candidate.directory
            );
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 0);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 0);
            let Reply::Facts(facts) = harness.exchange(&Request::Observe) else {
                panic!("observation unavailable");
            };
            assert_eq!(
                facts.approved,
                Some(RemoteBuild::describe(&harness.candidate).unwrap())
            );
            assert!(facts.status.stopped && !facts.status.running);
            // The exact artifact is now this account's approved build.
            assert_eq!(
                harness.exchange(&Request::PrepareUpdate { candidate }),
                Reply::AlreadyApproved
            );
        });
    }

    #[test]
    fn a_failed_update_offers_a_separately_confirmed_return_without_repeating_dispatch() {
        with_service(true, false, |harness| {
            let Reply::Prepared(prepared) = harness.exchange(&Request::PrepareUpdate {
                candidate: Harness::digest(&harness.candidate),
            }) else {
                panic!("the staged candidate was not described");
            };
            assert!(prepared.running && !prepared.stopped);
            let Reply::Outcome(outcome) = harness.exchange(&Request::ApplyUpdate {
                operation: [10; 32],
                candidate: Harness::digest(&harness.candidate),
                confirmation: prepared.confirmation,
            }) else {
                panic!("the update was not performed");
            };
            // This service manager never reaches readiness, so the candidate failed.
            assert_eq!(
                outcome.resolution,
                OperationResolution::Failed { can_return: true }
            );
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 1);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 1);
            let Reply::Prepared(returning) = harness.exchange(&Request::PrepareReturn) else {
                panic!("the return was not described");
            };
            assert_eq!(
                (returning.previous, returning.candidate),
                (
                    Some(RemoteBuild::describe(&harness.previous).unwrap()),
                    Some(RemoteBuild::describe(&harness.candidate).unwrap())
                )
            );
            let Reply::Outcome(returned) = harness.exchange(&Request::ApplyReturn {
                operation: [11; 32],
                confirmation: returning.confirmation,
            }) else {
                panic!("the return was not performed");
            };
            assert_eq!(returned.resolution, OperationResolution::Completed);
            assert_eq!(
                returned.kind,
                OperationKind::Return {
                    previous: RemoteBuild::describe(&harness.previous).unwrap()
                }
            );
            // The previous runtime stays stopped, and neither Stop nor Start was repeated.
            let status = returned.status.expect("resolved policy unavailable");
            assert!(status.stopped && !status.running);
            assert_eq!(
                *harness.manager.approved.lock().unwrap(),
                harness.previous.directory
            );
            assert_eq!(harness.manager.stops.load(Ordering::SeqCst), 1);
            assert_eq!(harness.manager.starts.load(Ordering::SeqCst), 1);
        });
    }
}
