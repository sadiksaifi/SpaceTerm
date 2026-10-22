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

use std::time::Duration;

use spaceterm_server::account_bridge::AccountDiscovery;
use spaceterm_server::service::activation::is_released_downgrade;
use spaceterm_server::service::maintenance::{
    Action, MaintenanceFacts, MaintenanceRefusal, OperationOutcome, PreparedChange, Reply, Request,
};

use super::cancellation::SshCancellationToken;
use super::process::{CapturedProcessOutput, SshProcessAdapter};
use super::server_connection::{
    ConnectMode, ContinuityChange, IncompatibilityKind, InteractionReason, RefusalKind,
    SshConnectFailure, SshServerContinuity, UnavailableKind,
};
use super::server_process::{
    NativeRoute, NativeSshConnectBackend, RouteClaim, SshDispatchFailure, VerifiedArtifact,
};
use crate::domain::SshDestination;

/// The remote action every maintenance exchange runs on the approved installation.
///
/// One exchange is one bounded request and one bounded reply. The request travels as one
/// serialized [`Request`] on standard input, so nothing it asks for becomes a shell argument,
/// and the service writes one serialized [`Reply`] on standard output. The staging transfer
/// answers the same way.
const MAINTAIN: &str = "ssh-maintain";
/// One bounded read or description, which changes nothing on the other computer.
const READ: Duration = Duration::from_secs(20);
/// One bounded dispatch. The service bounds its own lock and supervisor observation again, so a
/// request that outlives this is admitted and unconfirmed rather than repeated.
const DISPATCH: Duration = Duration::from_secs(120);

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
    Update {
        candidate: String,
    },
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
    /// This client could not retain the record that keeps the operation resolvable, so it never
    /// submitted it. Nothing reached the other computer.
    NotRetained,
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

/// Whether admitting this request may change the account. A lost reply to one of these stays
/// unconfirmed, because only the service can say what its original did.
const fn dispatches(request: &Request) -> bool {
    matches!(
        request,
        Request::Apply { .. }
            | Request::ApplyUpdate { .. }
            | Request::ApplyReturn { .. }
            | Request::Start { .. }
    )
}

/// Reads the one bounded reply an exchange wrote. The locator's own refusals happen before it
/// runs the approved installation, so they classify truthfully even for a dispatched request.
fn reply(output: &CapturedProcessOutput, dispatched: bool) -> Result<Reply, SshMaintenanceFailure> {
    if let Ok(reply) = serde_json::from_slice::<Reply>(&output.stdout) {
        return Ok(reply);
    }
    Err(match output.exit.code() {
        Some(72) => SshMaintenanceFailure::Incompatible(IncompatibilityKind::Platform),
        Some(73) => SshMaintenanceFailure::Refused(MaintenanceRefusal::NotInstalled),
        Some(74) => SshMaintenanceFailure::Refused(MaintenanceRefusal::StorageUnavailable),
        _ if dispatched => SshMaintenanceFailure::Unconfirmed,
        _ => SshMaintenanceFailure::Unavailable(UnavailableKind::ServiceUnavailable),
    })
}

/// A reply that does not answer what was asked, so the exchange reached no validated result.
fn unexpected(reply: Reply) -> SshMaintenanceFailure {
    match reply {
        Reply::Refused { refusal } => SshMaintenanceFailure::Refused(refusal),
        Reply::Unknown => SshMaintenanceFailure::Forgotten,
        _ => SshMaintenanceFailure::Unavailable(UnavailableKind::ServiceUnavailable),
    }
}

fn prepared(reply: Reply) -> Result<PreparedChange, SshMaintenanceFailure> {
    match reply {
        Reply::Prepared(change) => Ok(change),
        reply => Err(unexpected(reply)),
    }
}

/// One original's outcome. A dispatched request whose outcome cannot be matched to the original
/// stays unconfirmed, because repeating it could end work the original never admitted.
fn outcome(
    reply: Reply,
    original: [u8; 32],
    dispatched: bool,
) -> Result<OperationOutcome, SshMaintenanceFailure> {
    match reply {
        Reply::Outcome(outcome) if outcome.operation == original => Ok(outcome),
        // A refusal the service decided before any effect arrives as that original's outcome.
        // A bare refusal to a dispatched request means something failed around the dispatch
        // itself, so this client cannot tell whether the operation ran and must not say that
        // nothing changed.
        Reply::Refused { refusal } if !dispatched => Err(SshMaintenanceFailure::Refused(refusal)),
        Reply::Unknown if !dispatched => Err(SshMaintenanceFailure::Forgotten),
        _ if dispatched => Err(SshMaintenanceFailure::Unconfirmed),
        reply => Err(unexpected(reply)),
    }
}

/// Whether the artifact this client holds may replace the account's approved build. An update
/// never moves a server back to an older release, so a downgrade transfers nothing.
fn replaceable(candidate: &str, approved: &str) -> Result<(), SshMaintenanceFailure> {
    if is_released_downgrade(candidate, approved) {
        return Err(SshMaintenanceFailure::Refused(
            MaintenanceRefusal::CandidateUnavailable,
        ));
    }
    Ok(())
}

/// Every route, discovery and artifact failure as one maintenance classification.
fn failure(error: SshConnectFailure) -> SshMaintenanceFailure {
    match error {
        SshConnectFailure::Cancelled => SshMaintenanceFailure::Cancelled,
        SshConnectFailure::NeedsPerson(reason) => SshMaintenanceFailure::NeedsPerson(reason),
        SshConnectFailure::Changed(change) => SshMaintenanceFailure::Changed(change),
        SshConnectFailure::Stopped(_) => {
            SshMaintenanceFailure::Refused(MaintenanceRefusal::Stopped)
        }
        SshConnectFailure::Incompatible(kind) => SshMaintenanceFailure::Incompatible(kind),
        SshConnectFailure::Refused(RefusalKind::AuthorizationRefused) => {
            SshMaintenanceFailure::Refused(MaintenanceRefusal::AuthorizationRequired)
        }
        SshConnectFailure::Refused(RefusalKind::InvalidArtifact) => {
            SshMaintenanceFailure::Refused(MaintenanceRefusal::CandidateChanged)
        }
        SshConnectFailure::Unavailable(UnavailableKind::CandidateUnavailable) => {
            SshMaintenanceFailure::Refused(MaintenanceRefusal::CandidateUnavailable)
        }
        SshConnectFailure::Unavailable(kind) => SshMaintenanceFailure::Unavailable(kind),
        SshConnectFailure::Unconfirmed(_) => SshMaintenanceFailure::Unconfirmed,
        SshConnectFailure::Refused(RefusalKind::InstallRefused)
        | SshConnectFailure::InstallationOffered(_) => {
            SshMaintenanceFailure::Unavailable(UnavailableKind::ServiceUnavailable)
        }
    }
}

impl<A: SshProcessAdapter> NativeSshConnectBackend<A> {
    /// Acquires the original route and proves the account, application identity and server are
    /// the ones the original bound. Nothing is asked of the service until all of them match, so a
    /// destination that now reaches another computer, account or server sends nothing.
    ///
    /// The route comes back held for as long as the caller keeps it. Disconnect or Remove chosen
    /// on this server meanwhile therefore waits for the operation to finish rather than cutting
    /// its reply and leaving a dispatch that may already have taken effect unconfirmed.
    fn bound(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<(RouteClaim<A>, AccountDiscovery), SshMaintenanceFailure> {
        progress(MaintenancePhase::Authenticating);
        let route = self
            .maintenance_route(
                &target.destination,
                match authority {
                    // Background continuation reuses an authenticated route or a proven quiet
                    // one. It never starts an askpass attempt or any other prompt.
                    MaintenanceAuthority::Quiet => ConnectMode::QuietContinuation,
                    MaintenanceAuthority::Requested => ConnectMode::Requested,
                },
                cancellation,
                &|_| {},
            )
            .map_err(failure)?;
        // The route its owner has already begun ending carries nothing, and the next attempt
        // acquires one of its own.
        let route = route.claim().ok_or(SshMaintenanceFailure::Cancelled)?;
        if route.digest() != target.continuity.route {
            return Err(SshMaintenanceFailure::Changed(
                ContinuityChange::HostChanged,
            ));
        }
        progress(MaintenancePhase::Validating);
        let discovery = route.account_discovery(cancellation).map_err(failure)?;
        target
            .continuity
            .verify(&SshServerContinuity {
                route: route.digest(),
                account: discovery.account,
                build: discovery.build.clone(),
                server: discovery.server.unwrap_or([0; 32]),
            })
            .map_err(failure)?;
        Ok((route, discovery))
    }

    /// One bounded request and one bounded reply on an already bound route.
    fn exchange(
        &self,
        route: &NativeRoute<A>,
        request: &Request,
        cancellation: &SshCancellationToken,
    ) -> Result<Reply, SshMaintenanceFailure> {
        // The committed contract is the only encoding of a request. Serializing it keeps one
        // source of truth, and the request never becomes a shell argument.
        let asked = serde_json::to_vec(request)
            .map_err(|_| SshMaintenanceFailure::Refused(MaintenanceRefusal::Protocol))?;
        let dispatched = dispatches(request);
        let output = if dispatched {
            route
                .server_effect(MAINTAIN, asked, DISPATCH, cancellation)
                .map_err(|error| match error {
                    SshDispatchFailure::NotDispatched(error) => failure(error),
                    SshDispatchFailure::Unconfirmed => SshMaintenanceFailure::Unconfirmed,
                })?
        } else {
            route
                .server_read(MAINTAIN, asked, READ, cancellation)
                .map_err(failure)?
        };
        reply(&output, dispatched)
    }

    /// Transfers the exact artifact this client holds and returns the build the account retained.
    fn staged(
        &self,
        route: &NativeRoute<A>,
        artifact: &VerifiedArtifact,
        cancellation: &SshCancellationToken,
    ) -> Result<Option<String>, SshMaintenanceFailure> {
        let output = route.stage(artifact, cancellation).map_err(failure)?;
        match reply(&output, false)? {
            // Staging installs, activates and starts nothing, so a lost reply leaves the
            // approved runtime and its Terminal Sessions exactly as they were.
            Reply::Staged { candidate } if candidate.digest == artifact.digest() => {
                Ok(Some(candidate.digest))
            }
            Reply::Staged { .. } => Err(SshMaintenanceFailure::Refused(
                MaintenanceRefusal::CandidateChanged,
            )),
            Reply::AlreadyApproved => Ok(None),
            reply => Err(unexpected(reply)),
        }
    }
}

impl<A: SshProcessAdapter> SshMaintenanceBackend for NativeSshConnectBackend<A> {
    fn observe(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<MaintenanceFacts, SshMaintenanceFailure> {
        let (route, _) = self.bound(target, authority, cancellation, progress)?;
        match self.exchange(&route, &Request::Observe, cancellation)? {
            Reply::Facts(facts) => Ok(facts),
            reply => Err(unexpected(reply)),
        }
    }

    fn prepare(
        &self,
        target: &MaintenanceTarget,
        action: Action,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<PreparedChange, SshMaintenanceFailure> {
        let (route, _) = self.bound(target, authority, cancellation, progress)?;
        prepared(self.exchange(&route, &Request::Prepare { action }, cancellation)?)
    }

    fn prepare_update(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<Option<PreparedChange>, SshMaintenanceFailure> {
        let (route, discovery) = self.bound(target, authority, cancellation, progress)?;
        let artifact = route.candidate(&discovery.target).map_err(failure)?;
        replaceable(artifact.version(), &discovery.version)?;
        progress(MaintenancePhase::Transferring);
        let Some(candidate) = self.staged(&route, &artifact, cancellation)? else {
            return Ok(None);
        };
        match self.exchange(&route, &Request::PrepareUpdate { candidate }, cancellation)? {
            Reply::Prepared(change) => Ok(Some(change)),
            Reply::AlreadyApproved => Ok(None),
            reply => Err(unexpected(reply)),
        }
    }

    fn prepare_return(
        &self,
        target: &MaintenanceTarget,
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<PreparedChange, SshMaintenanceFailure> {
        let (route, _) = self.bound(target, authority, cancellation, progress)?;
        prepared(self.exchange(&route, &Request::PrepareReturn, cancellation)?)
    }

    fn apply(
        &self,
        target: &MaintenanceTarget,
        submission: &OperationSubmission,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<OperationOutcome, SshMaintenanceFailure> {
        let operation = submission.operation;
        let request = match (&submission.kind, &submission.confirmation) {
            (SubmittedKind::Start, None) => Request::Start { operation },
            (SubmittedKind::Service(action), Some(confirmation)) => Request::Apply {
                operation,
                action: *action,
                confirmation: confirmation.clone(),
            },
            (SubmittedKind::Update { candidate }, Some(confirmation)) => Request::ApplyUpdate {
                operation,
                candidate: candidate.clone(),
                confirmation: confirmation.clone(),
            },
            (SubmittedKind::Return, Some(confirmation)) => Request::ApplyReturn {
                operation,
                confirmation: confirmation.clone(),
            },
            // A destructive submission without its exact-observation confirmation, or a Start
            // carrying one, is not a submission this contract admits.
            _ => {
                return Err(SshMaintenanceFailure::Refused(MaintenanceRefusal::Protocol));
            }
        };
        // The person confirmed here, so this exchange may authenticate in front of them.
        let (route, _) = self.bound(
            target,
            MaintenanceAuthority::Requested,
            cancellation,
            progress,
        )?;
        progress(MaintenancePhase::Dispatching);
        outcome(
            self.exchange(&route, &request, cancellation)?,
            operation,
            true,
        )
    }

    fn resolve(
        &self,
        target: &MaintenanceTarget,
        operation: [u8; 32],
        authority: MaintenanceAuthority,
        cancellation: &SshCancellationToken,
        progress: &MaintenanceProgress<'_>,
    ) -> Result<OperationOutcome, SshMaintenanceFailure> {
        let (route, _) = self.bound(target, authority, cancellation, progress)?;
        progress(MaintenancePhase::Resolving);
        outcome(
            self.exchange(&route, &Request::Resolve { operation }, cancellation)?,
            operation,
            false,
        )
    }

    fn release(&self, destination: &SshDestination) {
        // Route ownership is shared. One route cache per backend is keyed by destination, so a
        // maintenance exchange reuses the route Connect already authenticated rather than
        // opening a second one. Releasing therefore ends only a route no presented connection
        // depends on: the connection registry keeps ending its own routes through Disconnect,
        // and Connect behavior is unchanged. Each exchange's own processes are bounded and
        // already reaped, and submitted originals live in their retained records either way.
        self.release_owned_route(destination);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sha2::{Digest, Sha256};
    use spaceterm_server::service::maintenance::{OperationKind, OperationResolution, RemoteBuild};
    use spaceterm_server::service::{ServiceStartup, ServiceStatus};

    use super::*;
    use crate::ssh::server_process::SshServerArtifacts;
    use crate::ssh::server_process::testing::{FakeAccount, backend, discovery};

    /// The effective OpenSSH configuration whose digest is the route the original bound.
    const CONFIGURATION: &str = "hostname example.test\nuser tester\nport 22\n";
    const SERVER: [u8; 32] = [9; 32];
    const OPERATION: [u8; 32] = [4; 32];

    fn build() -> String {
        crate::application_identity::ApplicationIdentity::current()
            .directory_name()
            .to_owned()
    }
    fn bound() -> SshServerContinuity {
        SshServerContinuity {
            route: Sha256::digest(CONFIGURATION.as_bytes()).into(),
            account: 501,
            build: build(),
            server: SERVER,
        }
    }
    fn target(continuity: SshServerContinuity) -> MaintenanceTarget {
        MaintenanceTarget {
            destination: SshDestination::new("maintenance-host".into()).unwrap(),
            continuity,
        }
    }
    fn facts() -> MaintenanceFacts {
        MaintenanceFacts {
            server: SERVER,
            account: 501,
            build: build(),
            status: ServiceStatus {
                installed: true,
                enabled: true,
                running: true,
                stopped: false,
            },
            startup: ServiceStartup::AtBoot,
            approved: Some(RemoteBuild {
                version: "0.1.0".into(),
                commit: "a".repeat(40),
                digest: "1".repeat(64),
            }),
            activation: None,
        }
    }
    fn completed() -> OperationOutcome {
        OperationOutcome {
            operation: OPERATION,
            kind: OperationKind::Service {
                action: Action::Stop,
            },
            resolution: OperationResolution::Completed,
            status: None,
        }
    }
    fn change() -> PreparedChange {
        PreparedChange {
            confirmation: "2".repeat(64),
            running: true,
            stopped: false,
            startup: ServiceStartup::AtBoot,
            previous: None,
            candidate: None,
        }
    }
    fn encoded(reply: &Reply) -> Vec<u8> {
        serde_json::to_vec(reply).unwrap()
    }
    /// An account that reports `discovery` and answers every exchange with `reply`.
    fn account(discovery: AccountDiscovery, reply: Reply) -> Arc<FakeAccount> {
        let discovered = serde_json::to_vec(&discovery).unwrap();
        let reply = encoded(&reply);
        answering(move |command, _| {
            if command.contains("ssh-discover") {
                (discovered.clone(), 0)
            } else {
                (reply.clone(), 0)
            }
        })
    }
    fn answering(
        answer: impl Fn(&str, &[u8]) -> (Vec<u8>, i32) + Send + Sync + 'static,
    ) -> Arc<FakeAccount> {
        FakeAccount::new(CONFIGURATION, answer)
    }
    /// Every maintenance request this account received, decoded from what the client sent.
    fn asked(account: &FakeAccount) -> Vec<Request> {
        account
            .sent("ssh-maintain")
            .iter()
            .map(|sent| serde_json::from_slice(sent).expect("one serialized request"))
            .collect()
    }
    fn artifacts() -> SshServerArtifacts {
        SshServerArtifacts::new(None)
    }
    fn silent(_: MaintenancePhase) {}

    /// Every exchange proves the route, account, application identity and server before it asks
    /// for anything, so a destination that now reaches another one sends nothing.
    #[gpui::test]
    fn a_changed_route_account_build_or_server_refuses_before_anything_is_sent(
        cx: &mut gpui::TestAppContext,
    ) {
        let changed = [
            (
                SshServerContinuity {
                    route: [1; 32],
                    ..bound()
                },
                discovery(SERVER),
                ContinuityChange::HostChanged,
            ),
            (
                bound(),
                AccountDiscovery {
                    account: 502,
                    ..discovery(SERVER)
                },
                ContinuityChange::AccountChanged,
            ),
            (
                bound(),
                AccountDiscovery {
                    build: "spaceterm-other".into(),
                    ..discovery(SERVER)
                },
                ContinuityChange::BuildChanged,
            ),
            (
                bound(),
                AccountDiscovery {
                    server: Some([8; 32]),
                    ..discovery(SERVER)
                },
                ContinuityChange::ServerChanged,
            ),
        ];
        for (continuity, discovered, expected) in changed {
            let account = account(discovered, Reply::Facts(facts()));
            let (backend, _) =
                backend(cx.background_executor.clone(), account.clone(), artifacts());
            let failure = backend
                .observe(
                    &target(continuity),
                    MaintenanceAuthority::Requested,
                    &SshCancellationToken::default(),
                    &silent,
                )
                .err();
            assert!(
                matches!(failure, Some(SshMaintenanceFailure::Changed(change)) if change == expected),
                "{expected:?} was not refused"
            );
            assert_eq!(
                account.ran("ssh-maintain"),
                0,
                "a changed original must send no maintenance request"
            );
        }
    }

    /// Background continuation uses authority that is already available. It never starts an
    /// askpass attempt, so a route it cannot reuse quietly waits for the person instead.
    #[gpui::test]
    fn quiet_authority_never_starts_an_authentication_prompt(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Facts(facts()));
        let (backend, prompts) =
            backend(cx.background_executor.clone(), account.clone(), artifacts());
        let observe = |authority| {
            backend.observe(
                &target(bound()),
                authority,
                &SshCancellationToken::default(),
                &silent,
            )
        };

        let quiet = observe(MaintenanceAuthority::Quiet);
        assert!(matches!(
            quiet,
            Err(SshMaintenanceFailure::NeedsPerson(
                InteractionReason::FirstSignIn
            ))
        ));
        assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(account.commands().is_empty(), "nothing reached the account");

        assert!(observe(MaintenanceAuthority::Requested).is_ok());
        assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            observe(MaintenanceAuthority::Quiet).is_ok(),
            "continuation must reuse the authenticated route"
        );
        assert_eq!(
            prompts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "reusing an authenticated route asks for no authority"
        );
    }

    /// A dispatched operation whose reply is lost stays unconfirmed. Only resolving the same
    /// original can settle it, so nothing here dispatches a second time.
    #[gpui::test]
    fn a_lost_reply_after_dispatch_is_unconfirmed_and_never_repeated(
        cx: &mut gpui::TestAppContext,
    ) {
        let discovered = serde_json::to_vec(&discovery(SERVER)).unwrap();
        let account = answering(move |command, _| {
            if command.contains("ssh-discover") {
                (discovered.clone(), 0)
            } else {
                // The service admitted the operation and answered nothing readable.
                (Vec::new(), 1)
            }
        });
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());
        let confirmation = "2".repeat(64);
        let submission = OperationSubmission {
            operation: OPERATION,
            kind: SubmittedKind::Service(Action::Stop),
            confirmation: Some(confirmation.clone()),
        };

        let outcome = backend.apply(
            &target(bound()),
            &submission,
            &SshCancellationToken::default(),
            &silent,
        );

        assert!(matches!(outcome, Err(SshMaintenanceFailure::Unconfirmed)));
        assert_eq!(
            account.ran("ssh-maintain"),
            1,
            "an admitted operation is never dispatched again"
        );
        let asked: Vec<Request> = account
            .sent("ssh-maintain")
            .iter()
            .map(|sent| serde_json::from_slice(sent).expect("one serialized request"))
            .collect();
        assert_eq!(
            asked,
            vec![Request::Apply {
                operation: OPERATION,
                action: Action::Stop,
                confirmation,
            }],
            "the dispatch carried exactly the original it bound"
        );
    }

    /// The service answers a bare refusal to a dispatched request only when something failed
    /// around the dispatch itself, such as retaining the result. It never proves the operation
    /// did not run, so it stays unconfirmed rather than claiming nothing changed.
    #[gpui::test]
    fn a_bare_refusal_to_a_dispatched_request_is_unconfirmed(cx: &mut gpui::TestAppContext) {
        let account = account(
            discovery(SERVER),
            Reply::Refused {
                refusal: MaintenanceRefusal::StorageUnavailable,
            },
        );
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());

        let outcome = backend.apply(
            &target(bound()),
            &OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Service(Action::Stop),
                confirmation: Some("2".repeat(64)),
            },
            &SshCancellationToken::default(),
            &silent,
        );

        assert!(
            matches!(outcome, Err(SshMaintenanceFailure::Unconfirmed)),
            "a dispatched request must not report that nothing changed: {outcome:?}"
        );
        // The same refusal to a read is what the service decided, so it reads as a refusal.
        let read = SshMaintenanceBackend::observe(
            &backend,
            &target(bound()),
            MaintenanceAuthority::Requested,
            &SshCancellationToken::default(),
            &silent,
        );
        assert!(matches!(
            read,
            Err(SshMaintenanceFailure::Refused(
                MaintenanceRefusal::StorageUnavailable
            ))
        ));
    }

    /// A destructive submission without the confirmation the service issued for one exact
    /// observation is not a submission this contract admits, so nothing is dispatched.
    #[gpui::test]
    fn an_unconfirmed_destructive_submission_dispatches_nothing(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Outcome(completed()));
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());
        for submission in [
            OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Service(Action::Uninstall),
                confirmation: None,
            },
            OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Update {
                    candidate: "3".repeat(64),
                },
                confirmation: None,
            },
            OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Return,
                confirmation: None,
            },
            // Start ends no work and carries no confirmation, so one it did not ask for is wrong.
            OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Start,
                confirmation: Some("2".repeat(64)),
            },
        ] {
            let outcome = backend.apply(
                &target(bound()),
                &submission,
                &SshCancellationToken::default(),
                &silent,
            );
            assert!(matches!(
                outcome,
                Err(SshMaintenanceFailure::Refused(MaintenanceRefusal::Protocol))
            ));
        }
        assert!(account.commands().is_empty(), "nothing reached the account");
    }

    /// Resolving an original reads its outcome. It dispatches nothing, chooses no artifact and
    /// renews no approval, so the only request it sends names the original it resolves.
    #[gpui::test]
    fn resolving_an_original_sends_no_destructive_request(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Outcome(completed()));
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());

        let outcome = backend.resolve(
            &target(bound()),
            OPERATION,
            MaintenanceAuthority::Requested,
            &SshCancellationToken::default(),
            &silent,
        );

        assert!(matches!(
            outcome,
            Ok(OperationOutcome {
                resolution: OperationResolution::Completed,
                ..
            })
        ));
        assert_eq!(
            asked(&account),
            vec![Request::Resolve {
                operation: OPERATION
            }],
            "resolving asked only about that original"
        );
        for effect in ["ssh-stage", "ssh-install", "ssh-prepare"] {
            assert_eq!(account.ran(effect), 0, "resolving asked for {effect}");
        }
    }

    /// A service that holds no record of the original says so, rather than letting a fresh query
    /// invent a result.
    #[gpui::test]
    fn an_unknown_original_is_forgotten(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Unknown);
        let (backend, _) = backend(cx.background_executor.clone(), account, artifacts());

        assert!(matches!(
            backend.resolve(
                &target(bound()),
                OPERATION,
                MaintenanceAuthority::Requested,
                &SshCancellationToken::default(),
                &silent,
            ),
            Err(SshMaintenanceFailure::Forgotten)
        ));
    }

    /// Describing a change returns the service's confirmation for the exact observation it
    /// describes, and changes nothing.
    #[gpui::test]
    fn describing_a_change_performs_nothing(cx: &mut gpui::TestAppContext) {
        for (described, expected) in [
            (None, Request::PrepareReturn),
            (
                Some(Action::Stop),
                Request::Prepare {
                    action: Action::Stop,
                },
            ),
            (
                Some(Action::Restart),
                Request::Prepare {
                    action: Action::Restart,
                },
            ),
            (
                Some(Action::Uninstall),
                Request::Prepare {
                    action: Action::Uninstall,
                },
            ),
        ] {
            let account = account(discovery(SERVER), Reply::Prepared(change()));
            let (backend, _) =
                backend(cx.background_executor.clone(), account.clone(), artifacts());
            let cancellation = SshCancellationToken::default();
            let prepared = match described {
                Some(action) => backend.prepare(
                    &target(bound()),
                    action,
                    MaintenanceAuthority::Requested,
                    &cancellation,
                    &silent,
                ),
                None => backend.prepare_return(
                    &target(bound()),
                    MaintenanceAuthority::Requested,
                    &cancellation,
                    &silent,
                ),
            };
            assert_eq!(
                prepared.ok().map(|change| change.confirmation),
                Some("2".repeat(64))
            );
            assert_eq!(asked(&account), vec![expected]);
        }
    }

    /// Staging transfers only an artifact this client verified for the account's own platform.
    #[gpui::test]
    fn staging_refuses_an_unverified_or_foreign_artifact(cx: &mut gpui::TestAppContext) {
        let unavailable = (
            SshServerArtifacts::new(None),
            discovery(SERVER),
            SshMaintenanceFailure::Refused(MaintenanceRefusal::CandidateUnavailable),
        );
        let unverified = (
            // This client holds a directory for the account's target whose artifact cannot be
            // read and verified.
            SshServerArtifacts::new(Some("/fixture/artifacts".into())),
            discovery(SERVER),
            SshMaintenanceFailure::Refused(MaintenanceRefusal::CandidateChanged),
        );
        let foreign = (
            SshServerArtifacts::new(Some("/fixture/artifacts".into())),
            AccountDiscovery {
                target: "powerpc64-unknown-linux-gnu".into(),
                ..discovery(SERVER)
            },
            SshMaintenanceFailure::Incompatible(IncompatibilityKind::Platform),
        );
        for (artifacts, discovered, expected) in [unavailable, unverified, foreign] {
            let account = account(discovered, Reply::Prepared(change()));
            let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts);

            let prepared = backend.prepare_update(
                &target(bound()),
                MaintenanceAuthority::Requested,
                &SshCancellationToken::default(),
                &silent,
            );

            assert!(
                matches!(prepared, Err(failure) if failure == expected),
                "{expected:?} was not refused"
            );
            assert_eq!(account.ran("ssh-stage"), 0, "nothing was transferred");
            assert_eq!(account.ran("ssh-maintain"), 0, "no update was described");
        }
    }

    /// An update never moves a server back to an older release, so a downgrade is refused before
    /// anything is transferred.
    #[test]
    fn a_released_downgrade_is_refused_before_transfer() {
        assert!(matches!(
            replaceable("0.6.7", "0.6.8"),
            Err(SshMaintenanceFailure::Refused(
                MaintenanceRefusal::CandidateUnavailable
            ))
        ));
        for (candidate, approved) in [
            ("0.6.9", "0.6.8"),
            ("0.6.8", "0.6.8"),
            ("0.7.0-dev", "0.6.8"),
            ("0.6.8", "0.7.0-dev"),
        ] {
            assert!(
                replaceable(candidate, approved).is_ok(),
                "{candidate} was refused against {approved}"
            );
        }
    }

    /// Reading a server's maintenance state starts, stages and installs nothing, so remote Check
    /// Status keeps its behavior and a deliberately stopped server stays stopped.
    #[gpui::test]
    fn observing_a_stopped_server_reads_it_without_starting_it(cx: &mut gpui::TestAppContext) {
        let stopped = AccountDiscovery {
            running: false,
            stopped: true,
            ..discovery(SERVER)
        };
        let account = account(stopped, Reply::Facts(facts()));
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());

        let observed = backend.observe(
            &target(bound()),
            MaintenanceAuthority::Requested,
            &SshCancellationToken::default(),
            &silent,
        );

        assert_eq!(observed.ok().map(|facts| facts.server), Some(SERVER));
        for effect in ["ssh-prepare", "ssh-stage", "ssh-install"] {
            assert_eq!(account.ran(effect), 0, "observing asked for {effect}");
        }
        assert_eq!(asked(&account), vec![Request::Observe]);
    }

    /// Releasing ends only a route maintenance alone holds, together with its processes. The next
    /// background exchange finds no route to reuse and waits for the person.
    #[gpui::test]
    fn releasing_ends_the_route_maintenance_alone_holds(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Facts(facts()));
        let (backend, prompts) =
            backend(cx.background_executor.clone(), account.clone(), artifacts());
        let observe = |authority| {
            backend.observe(
                &target(bound()),
                authority,
                &SshCancellationToken::default(),
                &silent,
            )
        };
        assert!(observe(MaintenanceAuthority::Requested).is_ok());

        backend.release(&target(bound()).destination);

        assert!(matches!(
            observe(MaintenanceAuthority::Quiet),
            Err(SshMaintenanceFailure::NeedsPerson(_))
        ));
        assert!(observe(MaintenanceAuthority::Requested).is_ok());
        assert_eq!(
            prompts.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a released route is acquired again only when the person asks"
        );
        // Releasing a destination with no route of its own leaves nothing to end.
        backend.release(&target(bound()).destination);
        backend.release(&SshDestination::new("other-host".into()).unwrap());
    }

    /// Connect presents its server only at the end, so a route it is still establishing is not
    /// a route maintenance owns. Releasing must leave it alone.
    #[gpui::test]
    fn releasing_leaves_a_route_connection_work_still_holds(cx: &mut gpui::TestAppContext) {
        use crate::ssh::server_connection::SshConnectBackend;
        use std::sync::{Mutex, mpsc::channel};

        let (entered, reached) = channel();
        let (release, proceed) = channel();
        let discovered = serde_json::to_vec(&discovery(SERVER)).unwrap();
        let proceed = Mutex::new(proceed);
        let holding = std::sync::atomic::AtomicBool::new(true);
        let account = answering(move |command, _| {
            // The first discovery stays inside the route until the release has happened.
            if command.contains("ssh-discover")
                && holding.swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                entered.send(()).ok();
                proceed.lock().unwrap().recv().ok();
            }
            (discovered.clone(), 0)
        });
        let (backend, _) = backend(cx.background_executor.clone(), account, artifacts());
        let backend = Arc::new(backend);
        let destination = target(bound()).destination;

        let connecting = {
            let backend = Arc::clone(&backend);
            let destination = destination.clone();
            std::thread::spawn(move || {
                SshConnectBackend::observe(
                    backend.as_ref(),
                    &destination,
                    None,
                    &SshCancellationToken::default(),
                    &|_| {},
                )
                .is_ok()
            })
        };
        reached.recv().expect("the connection reached its route");

        SshMaintenanceBackend::release(backend.as_ref(), &destination);

        release.send(()).expect("the connection continues");
        assert!(
            connecting.join().expect("the connection finished"),
            "releasing ended a route the connection was still establishing"
        );
    }

    /// A maintenance exchange holds the route it runs on. Disconnect or Remove chosen on that
    /// server while the operation is in flight therefore waits for the reply instead of cutting
    /// it, so a dispatch that already took effect does not become an Unconfirmed Outcome the
    /// person has to resolve from the row.
    #[gpui::test]
    fn a_dispatch_in_flight_keeps_its_reply_through_disconnect(cx: &mut gpui::TestAppContext) {
        use crate::ssh::server_connection::SshConnectBackend;
        use std::sync::{Mutex, mpsc::channel};

        let (entered, reached) = channel();
        let (release, proceed) = channel();
        let discovered = serde_json::to_vec(&discovery(SERVER)).unwrap();
        let answered = encoded(&Reply::Outcome(completed()));
        let proceed = Mutex::new(proceed);
        let account = answering(move |command, _| {
            if command.contains("ssh-discover") {
                return (discovered.clone(), 0);
            }
            // The service has admitted the dispatch and its reply is still in flight.
            entered.send(()).ok();
            proceed.lock().unwrap().recv().ok();
            (answered.clone(), 0)
        });
        let (backend, _) = backend(cx.background_executor.clone(), account, artifacts());
        let backend = Arc::new(backend);
        let destination = target(bound()).destination;
        // The route the connection registry authenticated, which the exchange then runs on.
        let connected = SshCancellationToken::default();
        assert!(
            SshConnectBackend::observe(backend.as_ref(), &destination, None, &connected, &|_| {})
                .is_ok(),
            "the registry admitted its connection"
        );

        let dispatching = {
            let backend = Arc::clone(&backend);
            std::thread::spawn(move || {
                backend.apply(
                    &target(bound()),
                    &OperationSubmission {
                        operation: OPERATION,
                        kind: SubmittedKind::Service(Action::Stop),
                        confirmation: Some("2".repeat(64)),
                    },
                    &SshCancellationToken::default(),
                    &silent,
                )
            })
        };
        reached.recv().expect("the dispatch reached the service");

        // The person chooses Disconnect and then Remove on this server in the Servers section.
        connected.cancel();
        SshConnectBackend::forget(backend.as_ref(), &destination);
        // The route's owner polls that cancellation every 20ms, so this is well past the point
        // where it would otherwise have ended the route under the exchange.
        std::thread::sleep(Duration::from_millis(250));

        release.send(()).expect("the service answers");
        let outcome = dispatching.join().expect("the exchange finished");
        assert!(
            matches!(
                outcome,
                Ok(OperationOutcome {
                    resolution: OperationResolution::Completed,
                    ..
                })
            ),
            "Disconnect cut a dispatch in flight and left it unconfirmed: {outcome:?}"
        );
    }

    /// The route an exchange holds is released when that exchange ends, on every exit path. A
    /// route nothing releases would stay up with the processes it owns and leave the person
    /// unable to disconnect from that server at all.
    #[gpui::test]
    fn a_route_an_exchange_holds_is_released_once_it_ends(cx: &mut gpui::TestAppContext) {
        use std::sync::{Mutex, mpsc::channel};

        let (entered, reached) = channel();
        let (release, proceed) = channel();
        let discovered = serde_json::to_vec(&discovery(SERVER)).unwrap();
        let answered = encoded(&Reply::Outcome(completed()));
        let proceed = Mutex::new(proceed);
        let account = answering(move |command, _| {
            if command.contains("ssh-discover") {
                return (discovered.clone(), 0);
            }
            entered.send(()).ok();
            proceed.lock().unwrap().recv().ok();
            (answered.clone(), 0)
        });
        let (backend, _) = backend(cx.background_executor.clone(), account, artifacts());
        let backend = Arc::new(backend);
        let destination = target(bound()).destination;

        let dispatching = {
            let backend = Arc::clone(&backend);
            std::thread::spawn(move || {
                backend.apply(
                    &target(bound()),
                    &OperationSubmission {
                        operation: OPERATION,
                        kind: SubmittedKind::Service(Action::Stop),
                        confirmation: Some("2".repeat(64)),
                    },
                    &SshCancellationToken::default(),
                    &silent,
                )
            })
        };
        reached.recv().expect("the dispatch reached the service");

        // Releasing while the exchange holds the route leaves it where it is.
        SshMaintenanceBackend::release(backend.as_ref(), &destination);
        release.send(()).expect("the service answers");
        assert!(
            dispatching.join().expect("the exchange finished").is_ok(),
            "releasing ended a route an exchange was still running on"
        );

        // The exchange has let go, so the route is released like any other.
        SshMaintenanceBackend::release(backend.as_ref(), &destination);
        assert!(
            matches!(
                SshMaintenanceBackend::observe(
                    backend.as_ref(),
                    &target(bound()),
                    MaintenanceAuthority::Quiet,
                    &SshCancellationToken::default(),
                    &silent,
                ),
                Err(SshMaintenanceFailure::NeedsPerson(_))
            ),
            "a route the exchange had finished with was never released"
        );
    }

    /// Start is the only remote operation that reverses a deliberate Stop. It ends no work, so it
    /// carries its bound operation identity and no confirmation.
    #[gpui::test]
    fn starting_a_stopped_server_carries_its_original_and_no_confirmation(
        cx: &mut gpui::TestAppContext,
    ) {
        let stopped = AccountDiscovery {
            running: false,
            stopped: true,
            ..discovery(SERVER)
        };
        let started = OperationOutcome {
            kind: OperationKind::Start,
            ..completed()
        };
        let account = account(stopped, Reply::Outcome(started));
        let (backend, _) = backend(cx.background_executor.clone(), account.clone(), artifacts());

        let outcome = backend.apply(
            &target(bound()),
            &OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Start,
                confirmation: None,
            },
            &SshCancellationToken::default(),
            &silent,
        );

        assert!(matches!(
            outcome,
            Ok(OperationOutcome {
                kind: OperationKind::Start,
                resolution: OperationResolution::Completed,
                ..
            })
        ));
        assert_eq!(
            asked(&account),
            vec![Request::Start {
                operation: OPERATION
            }],
            "Start carried its original and no confirmation"
        );
    }

    /// Cancellation before dispatch sends nothing at all, so there is no admitted effect to
    /// explain.
    #[gpui::test]
    fn cancellation_before_dispatch_sends_nothing(cx: &mut gpui::TestAppContext) {
        let account = account(discovery(SERVER), Reply::Outcome(completed()));
        let (backend, prompts) =
            backend(cx.background_executor.clone(), account.clone(), artifacts());

        let outcome = backend.apply(
            &target(bound()),
            &OperationSubmission {
                operation: OPERATION,
                kind: SubmittedKind::Service(Action::Restart),
                confirmation: Some("2".repeat(64)),
            },
            &SshCancellationToken::cancelled(),
            &silent,
        );

        assert!(matches!(outcome, Err(SshMaintenanceFailure::Cancelled)));
        assert!(account.commands().is_empty());
        assert_eq!(prompts.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
