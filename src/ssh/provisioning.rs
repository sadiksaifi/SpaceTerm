//! Temporary SSH provisioning with paired QUIC admission as its completion boundary.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(all(test, feature = "native-tests"))]
use super::process::{SshProcessCounts, TrackedSshProcessAdapter};
use super::{
    askpass::{AskPassAttemptFactory, AskPassBrokerLease},
    command::{
        OpenSshExecutable, SshCapability, SshCapabilityProbe, SshCommandContext,
        ValidatedRemoteShellCommand, setup_configuration, validate_setup_configuration_path,
    },
    host_config::{HostConfigFilesystem, HostConfigFilesystemError},
    process::{
        CapturedEffectError, CapturedProcessError, CapturedProcessOutput, SetupMaster,
        SshProcessAdapter, SshProcessEnvironment, run_captured_effect, run_captured_process,
        run_captured_transfer,
    },
    startup_environment::StartupSshEnvironment,
};
use crate::{
    application_identity::ApplicationIdentity,
    cancellation::CancellationToken,
    direct_connections::{DirectConnections, DirectRouteId},
    domain::SshDestination,
    platform::app_paths::{AppPaths, RuntimeOwner},
};
#[cfg(all(test, feature = "native-tests"))]
use sha2::{Digest, Sha256};
use spaceterm_core::{
    CoreError,
    direct::{DirectError, DirectStatus, ListenerState, MachinePin, NetworkPolicy},
    pane::control::ClientName,
    provisioning::{
        AccountDiscovery, NetworkRestore, RestoredAccess, SetupCancellation,
        SetupCancellationReply, SetupRefusal, SetupReply as EnrollmentResponse,
        SetupRequest as EnrollmentRequest,
        preflight::{self, Preflight},
    },
    service::{SetupStartObservation, verify_candidate_bytes},
};

const OBSERVATION: Duration = Duration::from_secs(15);
const MAX_DISCOVERY: usize = 16 * 1024;
const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;

pub(crate) use spaceterm_core::direct::Endpoint as SetupAddress;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SetupOffer {
    Installation {
        version: String,
        platform: (String, String),
    },
    Startup,
    Network {
        address: SetupAddress,
        bind: SocketAddr,
        current: Option<SocketAddr>,
        enabled: bool,
    },
}

/// Waits for consent on the setup worker, returning false when declined or cancelled.
/// Implementations must observe cancellation while waiting for their window's response.
pub(crate) trait SetupApprovalHandler: Send + Sync {
    fn approve(&self, offer: SetupOffer, cancellation: &CancellationToken) -> bool;
}

pub(crate) struct SetupRequest {
    pub destination: SshDestination,
    pub address: Option<SetupAddress>,
    pub name: ClientName,
    pub prompts: Arc<dyn AskPassAttemptFactory>,
    pub approvals: Arc<dyn SetupApprovalHandler>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupPhase {
    Authenticating,
    Discovering,
    Installing,
    Starting,
    Enrolling,
    Connecting,
}
pub(crate) struct SetupProgress(Arc<dyn Fn(SetupPhase) + Send + Sync>);
impl SetupProgress {
    pub(crate) fn new(callback: impl Fn(SetupPhase) + Send + Sync + 'static) -> Self {
        Self(Arc::new(callback))
    }
    fn report(&self, phase: SetupPhase) {
        (self.0)(phase);
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupOperation {
    Install,
    Start,
    Enrollment,
    Save,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IncompatibilityKind {
    Protocol,
    Platform,
    Installation,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefusalKind {
    InstallRefused,
    MissingArtifact,
    InvalidArtifact,
    MachineChanged,
    InvalidEndpoint,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnavailableKind {
    SshFailed,
    HostUnreachable,
    ServiceUnavailable,
    EndpointUnreachable,
    /// Another program on the machine holds the UDP port Network Access asked for. Nothing
    /// changed there.
    PortInUse,
    /// SpaceTerm Core is off in Login Items on that Mac, so its service started nothing. The
    /// person turns it on there.
    LoginItemOff,
    CredentialUnavailable,
    Storage,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SetupFailure {
    Cancelled,
    /// Add Machine refuses this computer. Each refusal reads as one sentence.
    Unsupported(spaceterm_core::provisioning::SetupRefusal),
    /// This computer couldn't reach SpaceTerm Core at `address` over UDP after setup paired it.
    /// `network` is what setup left Network Access as. `blocked_port` is the UDP port when the
    /// evidence points at a firewall: SSH reached `address` itself, with no proxy, and SpaceTerm
    /// Core reported listening on that port.
    Unreachable {
        address: SetupAddress,
        network: NetworkAfter,
        blocked_port: Option<u16>,
    },
    /// Setup stopped after it changed Network Access, because the person cancelled or the
    /// pairing step failed, or it stopped while Network Access couldn't go back as it was. Setup
    /// revoked the pairing, and `network` is what it left Network Access as.
    Stopped {
        network: NetworkAfter,
    },
    Incompatible(IncompatibilityKind),
    Refused(RefusalKind),
    Unavailable(UnavailableKind),
    Unconfirmed(SetupOperation),
}
/// What setup left Network Access as when this computer couldn't reach SpaceTerm Core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NetworkAfter {
    /// Setup didn't change Network Access.
    Unchanged,
    /// Setup changed Network Access and revoked its pairing. SpaceTerm Core then did this with
    /// the Network Access setup changed.
    Restored(RestoredAccess),
    /// Setup changed Network Access, and SpaceTerm Core didn't confirm the cancellation.
    Unconfirmed,
}
pub(crate) trait SshSetupBackend: Send + Sync {
    fn setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure>;
}

pub(crate) struct NativeSshSetupConfig<A: SshProcessAdapter> {
    pub paths: Arc<AppPaths>,
    pub filesystem: Arc<dyn HostConfigFilesystem>,
    pub bootstrap: Arc<dyn SshCoreBootstrap>,
    pub home: PathBuf,
    pub startup: StartupSshEnvironment,
    pub executable: OpenSshExecutable,
    pub adapter: A,
    pub artifacts: CoreArtifactCatalog,
}

pub(crate) struct NativeSshSetup<A: SshProcessAdapter> {
    #[cfg(all(test, feature = "native-tests"))]
    config: NativeSshSetupConfig<TrackedSshProcessAdapter<A>>,
    #[cfg(not(all(test, feature = "native-tests")))]
    config: NativeSshSetupConfig<A>,
    direct: DirectConnections,
}

impl<A: SshProcessAdapter> NativeSshSetup<A> {
    pub(crate) fn new(config: NativeSshSetupConfig<A>, direct: DirectConnections) -> Self {
        let NativeSshSetupConfig {
            paths,
            filesystem,
            bootstrap,
            home,
            startup,
            executable,
            adapter,
            artifacts,
        } = config;
        #[cfg(all(test, feature = "native-tests"))]
        let adapter = TrackedSshProcessAdapter::new(adapter);
        Self {
            config: NativeSshSetupConfig {
                paths,
                filesystem,
                bootstrap,
                home,
                startup,
                executable,
                adapter,
                artifacts,
            },
            direct,
        }
    }
    #[cfg(all(test, feature = "native-tests"))]
    pub(crate) fn process_counts(&self) -> SshProcessCounts {
        self.config.adapter.counts()
    }
}

impl<A: SshProcessAdapter> SshSetupBackend for NativeSshSetup<A> {
    fn setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        self.run_setup(request, cancellation, progress)
    }
}

impl<A: SshProcessAdapter> NativeSshSetup<A> {
    fn run_setup(
        &self,
        request: SetupRequest,
        cancellation: &CancellationToken,
        progress: &SetupProgress,
    ) -> Result<DirectRouteId, SetupFailure> {
        check_cancelled(cancellation)?;
        validate_setup_configuration_path(self.config.executable.as_path())
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let configuration_owner = self
            .config
            .paths
            .create_runtime_owner("s")
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let configuration = configuration_owner
            .create_private_file(
                "config",
                &setup_configuration(&self.config.home)
                    .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?,
            )
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        validate_setup_configuration_path(&configuration)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let socket = configuration_owner
            .socket_path("m")
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        validate_setup_configuration_path(&socket)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        progress.report(SetupPhase::Authenticating);
        let probe = SshCapabilityProbe::from_startup(
            self.config.executable.clone(),
            self.config.home.clone(),
            &self.config.startup,
            self.config.adapter.clone(),
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let capability = probe.probe(cancellation);
        check_cancelled(cancellation)?;
        if let SshCapability::Unavailable(reason) = capability {
            let _classification = reason;
            return Err(SetupFailure::Unavailable(UnavailableKind::SshFailed));
        }
        let attempt = request
            .prompts
            .start_attempt(&self.config.paths)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let _authentication = SetupAuthentication(attempt.lease.clone());
        let authentication_cancel =
            CancellationToken::observing(attempt.observation.cancellation_flag());
        let ssh_cancellation = CancellationToken::linked(cancellation, &authentication_cancel);
        let environment = match SshProcessEnvironment::new(
            self.config.home.clone(),
            attempt.lease.clone(),
            &self.config.startup,
        ) {
            Ok(environment) => environment,
            Err(_) => {
                attempt.lease.finish();
                return Err(SetupFailure::Unavailable(UnavailableKind::SshFailed));
            }
        };
        let resolve = SshCommandContext::resolve_setup(
            self.config.executable.clone(),
            &configuration,
            &request.destination,
        )
        .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
        let resolved = run_captured_process(
            &self.config.adapter,
            &resolve,
            &environment,
            vec![],
            256 * 1024,
            &ssh_cancellation,
            Instant::now() + OBSERVATION,
        )
        .map_err(map_process_error)?;
        let address = match request.address.clone() {
            Some(address) => address,
            None => resolved_setup_address(&resolved)?,
        };
        // SSH dials the address setup advertises only when the person entered none and the
        // configuration names no proxy. Only then can a UDP failure there be blamed on a firewall.
        let ssh_dials_address =
            request.address.is_none() && !resolved_through_proxy(&resolved.stdout);
        let master_command = SshCommandContext::setup_master(
            self.config.executable.clone(),
            &configuration,
            &socket,
            &request.destination,
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let master = SetupMaster::start(
            &self.config.adapter,
            &master_command,
            &environment,
            &ssh_cancellation,
        );
        // Authentication can fail after OpenSSH publishes its socket but before the ready
        // preamble arrives. Capture that socket after the process has been reaped too.
        let registered_socket = configuration_owner.register_socket("m");
        let master = master.map_err(map_process_error)?;
        registered_socket.map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        let session = SetupSession {
            config: &self.config,
            destination: &request.destination,
            environment,
            configuration,
            socket,
            master,
            _configuration_owner: configuration_owner,
            completed_effect: std::cell::Cell::new(None),
        };
        progress.report(SetupPhase::Discovering);
        // Every refusal but a blocked UDP port comes before anything is uploaded or changed.
        let preflight = session.preflight(&ssh_cancellation)?;
        if let Some(refusal) = preflight.refusal() {
            return Err(SetupFailure::Unsupported(refusal));
        }
        // A machine that needs linger keeps SpaceTerm Core running after logout only with it.
        if preflight.needs_linger() {
            session.authorize_linger(&ssh_cancellation)?;
        }
        let mut discovered = session.discover(&ssh_cancellation)?;
        let discovery = loop {
            match discovered {
                Discovery::Existing(discovery) => break discovery,
                Discovery::NeedsInstallation {
                    account,
                    target,
                    retained,
                } => {
                    let artifact = self
                        .config
                        .artifacts
                        .candidate(self.config.filesystem.as_ref(), &target)?;
                    if retained.as_ref().is_some_and(|previous| {
                        previous.version != artifact.version || previous.commit != artifact.commit
                    }) {
                        return Err(SetupFailure::Incompatible(
                            IncompatibilityKind::Installation,
                        ));
                    }
                    session.approve(
                        request.approvals.as_ref(),
                        SetupOffer::Installation {
                            version: artifact.version.clone(),
                            platform: artifact.platform.clone(),
                        },
                        &ssh_cancellation,
                    )?;
                    let current = self
                        .config
                        .artifacts
                        .candidate(self.config.filesystem.as_ref(), &target)?;
                    discovered = session.discover(&ssh_cancellation)?;
                    if current.digest != artifact.digest
                        || !matches!(&discovered,
                    Discovery::NeedsInstallation { account: current_account, target: current_target, retained: current_retained }
                        if *current_account == account && current_target == &target && current_retained == &retained)
                    {
                        continue;
                    }
                    progress.report(SetupPhase::Installing);
                    session.install(&artifact, &ssh_cancellation)?;
                    match session.discover(&ssh_cancellation) {
                        Ok(Discovery::Existing(discovery))
                            if discovery.installed
                                && discovery.account == account
                                && discovery.target == target
                                && discovery.core.is_some()
                                && discovery.version == artifact.version
                                && discovery.commit == artifact.commit
                                && retained.as_ref().is_none_or(|previous| {
                                    discovery.core == previous.core
                                        && discovery.stopped == previous.stopped
                                        && discovery.version == previous.version
                                        && discovery.commit == previous.commit
                                }) =>
                        {
                            break discovery;
                        }
                        _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Install)),
                    }
                }
            }
        };
        let mut discovery = discovery;
        while !discovery.running {
            let observation = startup_observation(&discovery)?;
            session.approve(
                request.approvals.as_ref(),
                SetupOffer::Startup,
                &ssh_cancellation,
            )?;
            let current = match session.discover(&ssh_cancellation)? {
                Discovery::Existing(current) => current,
                Discovery::NeedsInstallation { .. } => {
                    return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
                }
            };
            if current.start_observation.as_ref() != Some(&observation) {
                discovery = current;
                continue;
            }
            progress.report(SetupPhase::Starting);
            let input = serde_json::to_vec(&observation)
                .map_err(|_| SetupFailure::Refused(RefusalKind::MachineChanged))?;
            session.effect(
                "setup-start",
                input,
                &ssh_cancellation,
                SetupOperation::Start,
            )?;
            match session.discover(&ssh_cancellation) {
                Ok(Discovery::Existing(current))
                    if current.running
                        && current.account == discovery.account
                        && current.core == discovery.core
                        && current.start_observation.as_ref().is_some_and(|current| {
                            current.account == observation.account
                                && current.build == observation.build
                                && current.core == observation.core
                                && current.installation == observation.installation
                                && current.version == observation.version
                                && current.commit == observation.commit
                                && !current.stopped
                        }) =>
                {
                    discovery = current;
                }
                _ => return Err(SetupFailure::Unconfirmed(SetupOperation::Start)),
            }
        }
        let (status, bind) = loop {
            let status = session.direct_status(&ssh_cancellation)?;
            if status.pin.build != identity() || Some(status.pin.core) != discovery.core {
                return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
            }
            let bind_ip = status
                .policy
                .as_ref()
                .map(|policy| policy.bind.ip())
                .filter(|configured| {
                    // An explicit reachable address may forward to another server interface.
                    // The default hostname follows the authenticated SSH destination instead.
                    request.address.is_some()
                        || configured.is_unspecified()
                        || Some(*configured) == discovery.ssh_local_address
                })
                .or_else(|| {
                    let requested = request
                        .address
                        .as_ref()
                        .filter(|_| status.policy.is_none())?;
                    let ip = requested.host().parse::<std::net::IpAddr>().ok()?;
                    let observed = status.network_addresses.as_ref().ok()?;
                    observed.available.contains(&ip).then_some(ip)
                })
                .or(discovery.ssh_local_address)
                .ok_or(SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
            let bind = request
                .address
                .as_ref()
                .and_then(|_| status.policy.as_ref().map(|policy| policy.bind))
                .unwrap_or_else(|| SocketAddr::new(bind_ip, address.port()));
            NetworkPolicy::new(bind)
                .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
            if !status.enabled || status.policy.as_ref().map(|policy| policy.bind) != Some(bind) {
                session.approve(
                    request.approvals.as_ref(),
                    SetupOffer::Network {
                        address: address.clone(),
                        bind,
                        current: status.policy.as_ref().map(|policy| policy.bind),
                        enabled: status.enabled,
                    },
                    &ssh_cancellation,
                )?;
                let current = session.direct_status(&ssh_cancellation)?;
                if current.pin != status.pin
                    || current.policy != status.policy
                    || current.enabled != status.enabled
                {
                    if current.pin != status.pin {
                        return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
                    }
                    continue;
                }
            }
            break (status, bind);
        };
        session.check_cancelled(&ssh_cancellation)?;
        let endpoint = address.url();
        let prepared = self
            .direct
            .prepare_setup(
                endpoint.clone(),
                status.pin.clone(),
                status.setup_generation,
            )
            .map_err(|error| match error {
                DirectError::StorageUnavailable => {
                    SetupFailure::Unavailable(UnavailableKind::Storage)
                }
                error => map_direct_error(error),
            })?;
        let input = serde_json::to_vec(&EnrollmentRequest {
            operation: prepared.operation(),
            public_key: prepared.public_key(),
            name: request.name,
            pin: status.pin.clone(),
            expected_policy: status.policy.clone(),
            expected_enabled: status.enabled,
            endpoint: endpoint.clone(),
            generation: status.setup_generation,
        })
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
        progress.report(SetupPhase::Enrolling);
        let prepared_reply = session.effect(
            &format!("setup-direct-prepare {}", bind),
            input.clone(),
            &ssh_cancellation,
            SetupOperation::Enrollment,
        );
        let prepared_reply = prepared_reply.and_then(|output| {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Prepared {
                prepared: bool,
            }
            let reply: Prepared = serde_json::from_slice(&output.stdout)
                .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Enrollment))?;
            if !reply.prepared {
                return Err(SetupFailure::Unconfirmed(SetupOperation::Enrollment));
            }
            Ok(())
        });
        if let Err(error) = prepared_reply {
            let confirmed = session
                .cancel_enrollment(prepared.operation(), &status.pin, None)
                .is_some();
            drop(session);
            if confirmed {
                let _ = self.direct.confirm_setup_cancelled(&prepared);
            } else {
                let _ = self.direct.cancel_setup(&prepared);
            }
            return Err(error);
        }
        let enrolled = session
            .effect(
                &format!("setup-direct {}", bind),
                input,
                &ssh_cancellation,
                SetupOperation::Enrollment,
            )
            .and_then(|output| {
                let response: EnrollmentResponse = serde_json::from_slice(&output.stdout)
                    .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Enrollment))?;
                if response.schema != 1
                    || response.endpoint != endpoint
                    || response.paired.pin != status.pin
                {
                    return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
                }
                Ok(response)
            });
        let cancelled = cancellation.is_cancelled();
        // When setup changed Network Access, SSH stays open through the QUIC check, so a blocked
        // UDP port can put Network Access back. Otherwise the broker and socket close first.
        let changed_network =
            !status.enabled || status.policy.as_ref().map(|policy| policy.bind) != Some(bind);
        let installed = NetworkPolicy::new(bind)
            .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
        // A failed or cancelled pairing step revokes the pairing, and puts back Network Access
        // that setup changed in the same step.
        let cleanup = (enrolled.is_err() || cancelled).then(|| {
            session.cancel_enrollment(
                prepared.operation(),
                &status.pin,
                changed_network.then(|| NetworkRestore::new(installed.clone(), &status)),
            )
        });
        let session = (changed_network && enrolled.is_ok() && !cancelled).then_some(session);
        let enrolled = match (enrolled, cleanup) {
            (Ok(enrolled), None) => enrolled,
            (enrolled, cleanup) => {
                let cleanup = cleanup.flatten();
                if cleanup.is_some() {
                    let _ = self.direct.confirm_setup_cancelled(&prepared);
                } else {
                    let _ = self.direct.cancel_setup(&prepared);
                }
                return Err(end_enrollment(
                    enrolled.err(),
                    cancelled,
                    cleanup,
                    changed_network,
                ));
            }
        };
        progress.report(SetupPhase::Connecting);
        let completed = self.direct.complete_setup(
            &prepared,
            enrolled.paired,
            enrolled.machine_name,
            cancellation,
        );
        let failure = match completed {
            Ok(route) => return Ok(route),
            Err(error) => map_direct_error(error),
        };
        let held = session.as_ref().map(|session| Held {
            session,
            operation: prepared.operation(),
            pin: &status.pin,
        });
        let ended = end_admission(
            held.as_ref().map(|held| held as &dyn HeldSetup),
            cancellation.is_cancelled(),
            failure,
            Admission {
                address,
                installed,
                before: &status,
                ssh_dials_address,
            },
        );
        match ended.confirmed {
            Some(true) => {
                let _ = self.direct.confirm_setup_cancelled(&prepared);
            }
            Some(false) => {
                let _ = self.direct.cancel_setup(&prepared);
            }
            None => {}
        }
        Err(ended.failure)
    }
}

/// How setup ends when the pairing step failed with `error` or the person cancelled it, after
/// setup asked SpaceTerm Core over SSH to revoke the pairing. `cleanup` is `Some` once SpaceTerm
/// Core confirmed that, holding what it did with Network Access when `restoring` asked it to put
/// back what setup changed. A cancellation that leaves nothing changed closes the sheet.
fn end_enrollment(
    error: Option<SetupFailure>,
    cancelled: bool,
    cleanup: Option<Option<RestoredAccess>>,
    restoring: bool,
) -> SetupFailure {
    match cleanup {
        Some(Some(RestoredAccess::Off | RestoredAccess::On)) if restoring && cancelled => {
            SetupFailure::Cancelled
        }
        Some(Some(restored)) if restoring => SetupFailure::Stopped {
            network: NetworkAfter::Restored(restored),
        },
        _ if restoring => SetupFailure::Stopped {
            network: NetworkAfter::Unconfirmed,
        },
        Some(_) if cancelled => SetupFailure::Cancelled,
        _ => error.unwrap_or(SetupFailure::Unconfirmed(SetupOperation::Enrollment)),
    }
}

/// The SSH session setup still holds after QUIC admission. It exists only when setup changed
/// Network Access, so it can revoke the pairing and put Network Access back.
trait HeldSetup {
    /// SpaceTerm Core's listener now, or `None` when it didn't answer.
    fn listener(&self) -> Option<ListenerState>;
    /// Revokes setup's pairing and puts Network Access back as `restore` says, in one step.
    /// `Some` once SpaceTerm Core confirmed it, holding what it did with Network Access.
    fn cancel(&self, restore: NetworkRestore) -> Option<RestoredAccess>;
}

struct Held<'s, 'a, A: SshProcessAdapter> {
    session: &'s SetupSession<'a, A>,
    operation: [u8; 32],
    pin: &'s MachinePin,
}
impl<A: SshProcessAdapter> HeldSetup for Held<'_, '_, A> {
    fn listener(&self) -> Option<ListenerState> {
        self.session
            .direct_status(&CancellationToken::default())
            .ok()
            .map(|status| status.listener)
    }
    fn cancel(&self, restore: NetworkRestore) -> Option<RestoredAccess> {
        self.session
            .cancel_enrollment(self.operation, self.pin, Some(restore))
            .flatten()
    }
}

/// What setup knows when QUIC admission ends without a route: the address this computer dialed,
/// the policy setup turned on, Network Access before setup, and whether SSH dialed that address
/// itself.
struct Admission<'a> {
    address: SetupAddress,
    installed: NetworkPolicy,
    before: &'a DirectStatus,
    ssh_dials_address: bool,
}

/// How setup ends after QUIC admission returned no route. `confirmed` says whether SpaceTerm
/// Core confirmed revoking setup's pairing over SSH, or `None` when setup didn't ask it.
#[derive(Debug, Eq, PartialEq)]
struct Ended {
    failure: SetupFailure,
    confirmed: Option<bool>,
}

/// Whether SpaceTerm Core listening at `listener` takes what this computer sends to `bind`.
fn listens(listener: &ListenerState, bind: SocketAddr) -> bool {
    matches!(listener, ListenerState::Listening { addr }
        if addr.port() == bind.port() && (addr.ip().is_unspecified() || *addr == bind))
}

/// Ends setup after QUIC admission failed with `failure` or the person cancelled it. With a held
/// session, setup revokes its pairing and puts Network Access back in one step, whether no QUIC
/// packet arrived or the person cancelled. A firewall is named only when SSH dialed the address
/// itself and SpaceTerm Core listened on the port this computer dialed.
fn end_admission(
    held: Option<&dyn HeldSetup>,
    cancelled: bool,
    failure: SetupFailure,
    admission: Admission<'_>,
) -> Ended {
    let bind = admission.installed.bind;
    let restore = NetworkRestore::new(admission.installed.clone(), admission.before);
    if cancelled {
        // Network Access was already on, so nothing changed there. Admission already asked the
        // device registry to revoke the pairing, and the sheet closes as for a restored one.
        let Some(held) = held else {
            return Ended {
                failure: SetupFailure::Cancelled,
                confirmed: Some(false),
            };
        };
        // A cancellation that put Network Access back changed nothing, so the sheet closes.
        return match held.cancel(restore) {
            Some(RestoredAccess::Off | RestoredAccess::On) => Ended {
                failure: SetupFailure::Cancelled,
                confirmed: Some(true),
            },
            Some(restored) => Ended {
                failure: SetupFailure::Stopped {
                    network: NetworkAfter::Restored(restored),
                },
                confirmed: Some(true),
            },
            None => Ended {
                failure: SetupFailure::Stopped {
                    network: NetworkAfter::Unconfirmed,
                },
                confirmed: Some(false),
            },
        };
    }
    if failure != SetupFailure::Unavailable(UnavailableKind::EndpointUnreachable) {
        return Ended {
            failure,
            confirmed: None,
        };
    }
    let blocked_port =
        |listening: bool| (admission.ssh_dials_address && listening).then_some(bind.port());
    // No QUIC packet reached SpaceTerm Core. Network Access that was already on stays, and the
    // listener read before setup is the evidence.
    let Some(held) = held else {
        return Ended {
            failure: SetupFailure::Unreachable {
                address: admission.address,
                network: NetworkAfter::Unchanged,
                blocked_port: blocked_port(listens(&admission.before.listener, bind)),
            },
            confirmed: None,
        };
    };
    // Read before the restore changes it.
    let listening = held
        .listener()
        .is_some_and(|listener| listens(&listener, bind));
    let (network, confirmed) = match held.cancel(restore) {
        Some(restored) => (NetworkAfter::Restored(restored), true),
        None => (NetworkAfter::Unconfirmed, false),
    };
    Ended {
        failure: SetupFailure::Unreachable {
            address: admission.address,
            network,
            blocked_port: blocked_port(listening),
        },
        confirmed: Some(confirmed),
    }
}

struct SetupAuthentication(AskPassBrokerLease);
impl Drop for SetupAuthentication {
    fn drop(&mut self) {
        self.0.finish();
    }
}

struct SetupSession<'a, A: SshProcessAdapter> {
    config: &'a NativeSshSetupConfig<A>,
    destination: &'a SshDestination,
    environment: SshProcessEnvironment,
    configuration: PathBuf,
    socket: PathBuf,
    master: SetupMaster<A>,
    _configuration_owner: RuntimeOwner,
    completed_effect: std::cell::Cell<Option<SetupOperation>>,
}
impl<A: SshProcessAdapter> Drop for SetupSession<'_, A> {
    fn drop(&mut self) {
        self.master.close();
        self.environment.finish_authentication();
    }
}
impl<A: SshProcessAdapter> SetupSession<'_, A> {
    fn approve(
        &self,
        approvals: &dyn SetupApprovalHandler,
        offer: SetupOffer,
        cancellation: &CancellationToken,
    ) -> Result<(), SetupFailure> {
        self.check_cancelled(cancellation)?;
        if !approvals.approve(offer, cancellation) {
            return Err(self.process_failure(CapturedProcessError::Cancelled));
        }
        self.check_cancelled(cancellation)
    }
    fn direct_status(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<DirectStatus, SetupFailure> {
        serde_json::from_slice(&self.read("setup-direct-status", cancellation)?.stdout)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))
    }
    fn process_failure(&self, error: CapturedProcessError) -> SetupFailure {
        if matches!(error, CapturedProcessError::Cancelled)
            && let Some(operation) = self.completed_effect.get()
        {
            return SetupFailure::Unconfirmed(operation);
        }
        map_process_error(error)
    }
    fn check_cancelled(&self, cancellation: &CancellationToken) -> Result<(), SetupFailure> {
        if cancellation.is_cancelled() {
            return Err(self.process_failure(CapturedProcessError::Cancelled));
        }
        Ok(())
    }
    fn command(&self, script: &str) -> Result<super::command::SshCommandSpec, SetupFailure> {
        let command = ValidatedRemoteShellCommand::core_script(script)
            .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))?;
        SshCommandContext::setup_channel(
            self.config.executable.clone(),
            &self.configuration,
            &self.socket,
            self.destination,
            command,
        )
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::SshFailed))
    }
    fn read(
        &self,
        action: &str,
        cancellation: &CancellationToken,
    ) -> Result<CapturedProcessOutput, SetupFailure> {
        let command = self.command(&self.config.bootstrap.locator(
            identity(),
            action,
            action == "ssh-discover",
        ))?;
        let output = run_captured_process(
            &self.config.adapter,
            &command,
            &self.environment,
            Vec::new(),
            MAX_DISCOVERY,
            cancellation,
            Instant::now() + OBSERVATION,
        )
        .map_err(|error| self.process_failure(error))?;
        if !output.exit.is_success() {
            return Err(SetupFailure::Unavailable(
                UnavailableKind::ServiceUnavailable,
            ));
        }
        Ok(output)
    }
    /// Revokes this setup's pairing, and restores Network Access in the same step when
    /// `restore` says how it was. `Some` once SpaceTerm Core confirmed the cancellation, holding
    /// what it did with Network Access when `restore` asked it to.
    fn cancel_enrollment(
        &self,
        operation: [u8; 32],
        pin: &MachinePin,
        restore: Option<NetworkRestore>,
    ) -> Option<Option<RestoredAccess>> {
        let Ok(command) = ValidatedRemoteShellCommand::core_script(&self.config.bootstrap.locator(
            identity(),
            "setup-direct-cancel",
            false,
        )) else {
            return None;
        };
        let Ok(command) = SshCommandContext::setup_channel(
            self.config.executable.clone(),
            &self.configuration,
            &self.socket,
            self.destination,
            command,
        ) else {
            return None;
        };
        let asked = restore.is_some();
        let Ok(input) = serde_json::to_vec(&SetupCancellation {
            operation,
            pin: pin.clone(),
            restore,
        }) else {
            return None;
        };
        let result = run_captured_effect(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            &CancellationToken::default(),
            Instant::now() + OBSERVATION,
        );
        let output = result.ok().filter(|output| output.exit.is_success())?;
        serde_json::from_slice::<SetupCancellationReply>(&output.stdout)
            .ok()
            .filter(|reply| {
                reply.cancelled
                    && reply.operation == operation
                    && reply.pin == *pin
                    && reply.restored.is_some() == asked
            })
            .map(|reply| reply.restored)
    }
    fn discover(&self, cancellation: &CancellationToken) -> Result<Discovery, SetupFailure> {
        parse_discovery(&self.read("ssh-discover", cancellation)?.stdout)
    }
    /// Reads what the computer is, changing nothing.
    fn preflight(&self, cancellation: &CancellationToken) -> Result<Preflight, SetupFailure> {
        let command = self.command(preflight::SCRIPT)?;
        let output = run_captured_process(
            &self.config.adapter,
            &command,
            &self.environment,
            Vec::new(),
            preflight::MAX_REPLY,
            cancellation,
            Instant::now() + OBSERVATION,
        )
        .map_err(|error| self.process_failure(error))?;
        if !output.exit.is_success() {
            return Err(SetupFailure::Unavailable(UnavailableKind::SshFailed));
        }
        std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|text| Preflight::parse(text).ok())
            .ok_or(SetupFailure::Unavailable(UnavailableKind::SshFailed))
    }
    /// Enables linger before anything is uploaded: first without a password, then with one the
    /// remote authorization prompt asks for. Declining refuses setup.
    fn authorize_linger(&self, cancellation: &CancellationToken) -> Result<(), SetupFailure> {
        let declined = SetupFailure::Unsupported(SetupRefusal::LingerDeclined);
        let run = |script: &str, input: Vec<u8>| -> Result<bool, SetupFailure> {
            let output = run_captured_transfer(
                &self.config.adapter,
                &self.command(script)?,
                &self.environment,
                input,
                4096,
                cancellation,
            )
            .map_err(|error| self.process_failure(error))?;
            match output.exit.code() {
                Some(0) => Ok(true),
                Some(preflight::LINGER_REFUSED) => Ok(false),
                _ => Err(SetupFailure::Unavailable(UnavailableKind::SshFailed)),
            }
        };
        self.check_cancelled(cancellation)?;
        if run(preflight::LINGER, Vec::new())? {
            return Ok(());
        }
        let prompt = self
            .environment
            .remote_authorization_command()
            .map_err(|_| declined.clone())?;
        let answer = run_captured_transfer(
            &self.config.adapter,
            &prompt,
            &self.environment,
            Vec::new(),
            4096,
            cancellation,
        )
        .map_err(|error| self.process_failure(error))?;
        let mut secret = zeroize::Zeroizing::new(answer.stdout);
        if !answer.exit.is_success() {
            return Err(declined);
        }
        if secret.last() == Some(&b'\n') {
            secret.pop();
        }
        if secret.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
            return Err(declined);
        }
        let mut input = zeroize::Zeroizing::new(Vec::with_capacity(secret.len() + 1));
        input.extend_from_slice(&secret);
        input.push(b'\n');
        if run(preflight::LINGER_WITH_PASSWORD, std::mem::take(&mut *input))? {
            Ok(())
        } else {
            Err(declined)
        }
    }
    fn effect(
        &self,
        action: &str,
        input: Vec<u8>,
        cancellation: &CancellationToken,
        operation: SetupOperation,
    ) -> Result<CapturedProcessOutput, SetupFailure> {
        let command = self.command(&self.config.bootstrap.locator(identity(), action, false))?;
        let output = run_captured_effect(
            &self.config.adapter,
            &command,
            &self.environment,
            input,
            MAX_DISCOVERY,
            cancellation,
            Instant::now() + OBSERVATION,
        )
        .map_err(|error| match error {
            CapturedEffectError::NotAdmitted(error) => self.process_failure(error),
            CapturedEffectError::Unconfirmed => SetupFailure::Unconfirmed(operation),
        })?;
        if !output.exit.is_success() {
            return Err(step_failure(
                output.exit.code(),
                SetupFailure::Unconfirmed(operation),
            ));
        }
        self.completed_effect.set(Some(operation));
        Ok(output)
    }
    fn install(
        &self,
        artifact: &VerifiedArtifact,
        cancellation: &CancellationToken,
    ) -> Result<(), SetupFailure> {
        let command = self.command(&self.config.bootstrap.install(
            identity(),
            artifact.bytes.len(),
            &artifact.signature,
            &artifact.digest,
        ))?;
        self.check_cancelled(cancellation)?;
        let output = run_captured_transfer(
            &self.config.adapter,
            &command,
            &self.environment,
            artifact.bytes.clone(),
            MAX_DISCOVERY,
            cancellation,
        )
        .map_err(|_| SetupFailure::Unconfirmed(SetupOperation::Install))?;
        if !output.exit.is_success() {
            return Err(step_failure(
                output.exit.code(),
                SetupFailure::Refused(RefusalKind::InstallRefused),
            ));
        }
        self.completed_effect.set(Some(SetupOperation::Install));
        Ok(())
    }
}

/// What a setup step's nonzero exit `code` says. A status the CLI reserves names its one cause;
/// any other status is `otherwise`.
fn step_failure(code: Option<i32>, otherwise: SetupFailure) -> SetupFailure {
    use spaceterm_core::provisioning::{LOGIN_ITEM_OFF, PORT_IN_USE};
    match code.and_then(|code| u8::try_from(code).ok()) {
        Some(PORT_IN_USE) => SetupFailure::Unavailable(UnavailableKind::PortInUse),
        Some(LOGIN_ITEM_OFF) => SetupFailure::Unavailable(UnavailableKind::LoginItemOff),
        _ => otherwise,
    }
}

fn startup_observation(
    discovery: &AccountDiscovery,
) -> Result<SetupStartObservation, SetupFailure> {
    let observation = discovery
        .start_observation
        .as_ref()
        .ok_or(SetupFailure::Refused(RefusalKind::MachineChanged))?;
    if observation.account != discovery.account
        || observation.build != discovery.build
        || Some(observation.core) != discovery.core
        || observation.stopped != discovery.stopped
    {
        return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
    }
    Ok(observation.clone())
}

fn resolved_setup_address(output: &CapturedProcessOutput) -> Result<SetupAddress, SetupFailure> {
    if !output.exit.is_success() {
        return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint));
    }
    let output = std::str::from_utf8(&output.stdout)
        .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))?;
    let mut hostname = None;
    let mut port = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "hostname" if hostname.is_none() => hostname = Some(value),
            "port" if port.is_none() => port = value.parse::<u16>().ok().filter(|port| *port != 0),
            "hostname" | "port" => return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint)),
            _ => (),
        }
    }
    // SSH's port selects its authentication transport, never the QUIC listener.
    if port.is_none() {
        return Err(SetupFailure::Refused(RefusalKind::InvalidEndpoint));
    }
    SetupAddress::parse(
        hostname.ok_or(SetupFailure::Refused(RefusalKind::InvalidEndpoint))?,
        own_port(),
    )
    .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidEndpoint))
}

/// Whether `ssh -G` output names a proxy, so SSH reaches the destination through another host.
/// Output that isn't text counts as proxied, so it never blames a firewall.
fn resolved_through_proxy(stdout: &[u8]) -> bool {
    std::str::from_utf8(stdout).map_or(true, |output| {
        output.lines().any(|line| {
            line.split_once(' ').is_some_and(|(key, value)| {
                matches!(key, "proxyjump" | "proxycommand") && !value.eq_ignore_ascii_case("none")
            })
        })
    })
}

/// The UDP port this desktop's build identity uses unless an address names one.
pub(crate) fn own_port() -> u16 {
    spaceterm_core::direct::default_port(ApplicationIdentity::current().directory_name())
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), SetupFailure> {
    if cancellation.is_cancelled() {
        Err(SetupFailure::Cancelled)
    } else {
        Ok(())
    }
}
fn map_process_error(error: CapturedProcessError) -> SetupFailure {
    match error {
        CapturedProcessError::Cancelled => SetupFailure::Cancelled,
        _ => SetupFailure::Unavailable(UnavailableKind::HostUnreachable),
    }
}
fn map_direct_error(error: DirectError) -> SetupFailure {
    match error {
        DirectError::CredentialUnavailable | DirectError::InteractionRequired => {
            SetupFailure::Unavailable(UnavailableKind::CredentialUnavailable)
        }
        DirectError::StorageUnavailable => SetupFailure::Unconfirmed(SetupOperation::Save),
        DirectError::PortInUse => SetupFailure::Unavailable(UnavailableKind::PortInUse),
        DirectError::PolicyChanged => SetupFailure::Unconfirmed(SetupOperation::Enrollment),
        DirectError::IncompatibleProtocol => {
            SetupFailure::Incompatible(IncompatibilityKind::Protocol)
        }
        DirectError::PinMismatch
        | DirectError::WrongMachine
        | DirectError::AuthenticationRefused
        | DirectError::Substituted
        | DirectError::Revoked => SetupFailure::Refused(RefusalKind::MachineChanged),
        _ => SetupFailure::Unavailable(UnavailableKind::EndpointUnreachable),
    }
}
fn map_core_error(error: CoreError) -> SetupFailure {
    match error {
        CoreError::IncompatibleProtocol(_) => {
            SetupFailure::Incompatible(IncompatibilityKind::Protocol)
        }
        CoreError::AuthorizationRequired => SetupFailure::Unsupported(SetupRefusal::LingerDeclined),
        _ => SetupFailure::Refused(RefusalKind::InvalidArtifact),
    }
}

pub(crate) trait SshCoreBootstrap: Send + Sync {
    fn locator(&self, identity: &str, action: &str, missing: bool) -> String;
    fn install(
        &self,
        identity: &str,
        archive_length: usize,
        signature: &[u8],
        digest: &str,
    ) -> String;
}

/// Signed server packages captured by host composition.
#[derive(Clone)]
pub(crate) struct CoreArtifactCatalog {
    root: Option<PathBuf>,
}
impl CoreArtifactCatalog {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }
    fn candidate(
        &self,
        filesystem: &dyn HostConfigFilesystem,
        target: &str,
    ) -> Result<VerifiedArtifact, SetupFailure> {
        let platform = platform(target)?;
        let directory = self
            .root
            .as_ref()
            .ok_or(SetupFailure::Refused(RefusalKind::MissingArtifact))?
            .join(target);
        let archive = directory.join("artifact.tar.gz");
        let signature_path = directory.join("artifact.sig");
        let bytes = read_bounded(
            filesystem,
            &archive,
            MAX_ARTIFACT,
            RefusalKind::MissingArtifact,
        )?;
        // A present archive without its authentication material is an unverified package.
        let signature = read_bounded(
            filesystem,
            &signature_path,
            64,
            RefusalKind::InvalidArtifact,
        )?;
        let digest_bytes = read_bounded(
            filesystem,
            &directory.join("artifact.sha256"),
            65,
            RefusalKind::InvalidArtifact,
        )?;
        let digest = std::str::from_utf8(&digest_bytes)
            .map_err(|_| SetupFailure::Refused(RefusalKind::InvalidArtifact))?
            .trim()
            .to_owned();
        let metadata = verify_candidate_bytes(
            identity(),
            &bytes,
            &signature,
            &digest,
            target,
            spaceterm_core::protocol::MAJOR,
        )
        .map_err(map_core_error)?;
        Ok(VerifiedArtifact {
            bytes,
            signature,
            digest,
            version: metadata.version,
            commit: metadata.commit,
            platform,
        })
    }
}
fn read_bounded(
    filesystem: &dyn HostConfigFilesystem,
    path: &std::path::Path,
    limit: u64,
    missing: RefusalKind,
) -> Result<Vec<u8>, SetupFailure> {
    let bytes = filesystem
        .read_artifact_file_limited(path, limit as usize)
        .map_err(|error| {
            SetupFailure::Refused(match error {
                HostConfigFilesystemError::Missing => missing,
                HostConfigFilesystemError::Unavailable => RefusalKind::InvalidArtifact,
            })
        })?;
    if bytes.len() as u64 > limit {
        return Err(SetupFailure::Refused(RefusalKind::InvalidArtifact));
    }
    Ok(bytes)
}
pub(super) struct VerifiedArtifact {
    bytes: Vec<u8>,
    signature: Vec<u8>,
    digest: String,
    version: String,
    commit: String,
    platform: (String, String),
}
fn identity() -> &'static str {
    ApplicationIdentity::current().directory_name()
}
#[cfg(all(test, feature = "native-tests"))]
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn platform(target: &str) -> Result<(String, String), SetupFailure> {
    match target {
        "aarch64-apple-darwin" => Ok(("macOS".into(), "arm64".into())),
        "x86_64-unknown-linux-gnu" => Ok(("linux".into(), "x86_64".into())),
        _ => Err(SetupFailure::Incompatible(IncompatibilityKind::Platform)),
    }
}
enum Discovery {
    Existing(Box<AccountDiscovery>),
    NeedsInstallation {
        account: u32,
        target: String,
        retained: Option<Box<AccountDiscovery>>,
    },
}
fn parse_discovery(bytes: &[u8]) -> Result<Discovery, SetupFailure> {
    if bytes.len() > MAX_DISCOVERY {
        return Err(SetupFailure::Unavailable(
            UnavailableKind::ServiceUnavailable,
        ));
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && let Some(missing) = text.strip_prefix("SPACETERM_MISSING ")
    {
        let parts: Vec<_> = missing.split_whitespace().collect();
        if let [account, target] = parts.as_slice() {
            let account = account
                .parse::<u32>()
                .ok()
                .filter(|account| *account != 0)
                .ok_or(SetupFailure::Refused(RefusalKind::InstallRefused))?;
            platform(target)?;
            return Ok(Discovery::NeedsInstallation {
                account,
                target: (*target).to_owned(),
                retained: None,
            });
        }
    }
    let discovery: AccountDiscovery = serde_json::from_slice(bytes)
        .map_err(|_| SetupFailure::Unavailable(UnavailableKind::ServiceUnavailable))?;
    validate_discovery(&discovery)?;
    if !discovery.installed {
        return Ok(Discovery::NeedsInstallation {
            account: discovery.account,
            target: discovery.target.clone(),
            retained: Some(Box::new(discovery)),
        });
    }
    Ok(Discovery::Existing(Box::new(discovery)))
}
fn validate_discovery(discovery: &AccountDiscovery) -> Result<(), SetupFailure> {
    if discovery.schema != 1 || discovery.account == 0 || discovery.build != identity() {
        return Err(SetupFailure::Refused(RefusalKind::MachineChanged));
    }
    platform(&discovery.target)?;
    if !discovery
        .protocol_majors
        .contains(&spaceterm_core::protocol::MAJOR)
    {
        return Err(SetupFailure::Incompatible(IncompatibilityKind::Protocol));
    }
    Ok(())
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
#[path = "../platform/unix_adapter_tests/core_setup.rs"]
mod unix_adapter_tests;

#[cfg(test)]
mod proxy_tests {
    use super::resolved_through_proxy;

    /// Only a configuration without a proxy lets setup blame a firewall for blocked UDP.
    #[test]
    fn a_proxy_in_the_resolved_configuration_is_detected() {
        assert!(!resolved_through_proxy(b"hostname penguin\nport 22\n"));
        assert!(!resolved_through_proxy(
            b"hostname penguin\nport 22\nproxycommand none\n"
        ));
        assert!(resolved_through_proxy(
            b"hostname penguin\nport 22\nproxyjump bastion\n"
        ));
        assert!(resolved_through_proxy(
            b"hostname penguin\nproxycommand ssh -W %h:%p bastion\n"
        ));
        assert!(resolved_through_proxy(&[0xff, 0xfe]));
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::cell::RefCell;

    const BIND: &str = "10.1.2.3:9231";

    /// A held SSH session whose SpaceTerm Core listens at `listener` and answers a cancellation
    /// with `restored`.
    struct Session {
        listener: Option<ListenerState>,
        restored: Option<RestoredAccess>,
        cancels: RefCell<Vec<NetworkRestore>>,
    }
    impl Session {
        fn new(listening: bool, restored: Option<RestoredAccess>) -> Self {
            Self {
                listener: Some(listener(listening)),
                restored,
                cancels: RefCell::default(),
            }
        }
    }
    impl HeldSetup for Session {
        fn listener(&self) -> Option<ListenerState> {
            self.listener.clone()
        }
        fn cancel(&self, restore: NetworkRestore) -> Option<RestoredAccess> {
            self.cancels.borrow_mut().push(restore);
            self.restored
        }
    }

    fn listener(listening: bool) -> ListenerState {
        if listening {
            ListenerState::Listening {
                addr: BIND.parse().unwrap(),
            }
        } else {
            ListenerState::Off
        }
    }

    fn before(listening: bool) -> DirectStatus {
        DirectStatus {
            setup_generation: 0,
            pin: MachinePin {
                build: "spaceterm".into(),
                core: [1; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            policy: listening.then(|| NetworkPolicy::new(BIND.parse().unwrap()).unwrap()),
            enabled: listening,
            machine_name: "penguin".into(),
            listener: listener(listening),
            network_addresses: Err(DirectError::Transport),
        }
    }

    fn end(
        held: Option<&dyn HeldSetup>,
        cancelled: bool,
        before: &DirectStatus,
        ssh_dials_address: bool,
    ) -> Ended {
        end_admission(
            held,
            cancelled,
            SetupFailure::Unavailable(UnavailableKind::EndpointUnreachable),
            Admission {
                address: SetupAddress::parse("10.1.2.3", 9231).unwrap(),
                installed: NetworkPolicy::new(BIND.parse().unwrap()).unwrap(),
                before,
                ssh_dials_address,
            },
        )
    }

    fn unreachable(network: NetworkAfter, blocked_port: Option<u16>) -> SetupFailure {
        SetupFailure::Unreachable {
            address: SetupAddress::parse("10.1.2.3", 9231).unwrap(),
            network,
            blocked_port,
        }
    }

    /// Network Access that was already on at the dialed address, which SSH reached directly,
    /// points at a firewall too, from the listener read before setup.
    #[test]
    fn a_firewall_is_named_when_network_access_was_already_on() {
        assert_eq!(
            end(None, false, &before(true), true),
            Ended {
                failure: unreachable(NetworkAfter::Unchanged, Some(9231)),
                confirmed: None,
            }
        );
        assert_eq!(
            end(None, false, &before(true), false).failure,
            unreachable(NetworkAfter::Unchanged, None),
            "SSH through a proxy or an entered address blames no firewall"
        );
    }

    /// When setup changed Network Access, a firewall is named only once SpaceTerm Core says it
    /// listens there, and the cancellation puts Network Access back.
    #[test]
    fn a_held_session_restores_and_names_the_firewall_only_while_listening() {
        let session = Session::new(true, Some(RestoredAccess::Off));
        assert_eq!(
            end(Some(&session), false, &before(false), true),
            Ended {
                failure: unreachable(NetworkAfter::Restored(RestoredAccess::Off), Some(9231)),
                confirmed: Some(true),
            }
        );
        let session = Session::new(false, None);
        assert_eq!(
            end(Some(&session), false, &before(false), true),
            Ended {
                failure: unreachable(NetworkAfter::Unconfirmed, None),
                confirmed: Some(false),
            }
        );
    }

    /// A failed or cancelled pairing step that changed Network Access says what it left Network
    /// Access as, and a cancellation that put it back closes the sheet.
    #[test]
    fn a_stopped_pairing_step_reports_network_access() {
        let failed = || Some(SetupFailure::Unconfirmed(SetupOperation::Enrollment));
        assert_eq!(
            end_enrollment(None, true, Some(Some(RestoredAccess::Off)), true),
            SetupFailure::Cancelled
        );
        assert_eq!(
            end_enrollment(failed(), false, Some(Some(RestoredAccess::Off)), true),
            SetupFailure::Stopped {
                network: NetworkAfter::Restored(RestoredAccess::Off),
            }
        );
        assert_eq!(
            end_enrollment(failed(), false, None, true),
            SetupFailure::Stopped {
                network: NetworkAfter::Unconfirmed,
            }
        );
        assert_eq!(
            end_enrollment(None, true, Some(None), false),
            SetupFailure::Cancelled,
            "a confirmed cancellation that changed no Network Access closes the sheet"
        );
        assert_eq!(
            end_enrollment(failed(), false, Some(None), false),
            failed().unwrap()
        );
    }

    /// Cancel while connecting with Network Access already on changed nothing there, so the
    /// sheet closes as it does after a restore.
    #[test]
    fn cancel_while_connecting_with_network_access_already_on_closes() {
        assert_eq!(
            end(None, true, &before(true), true),
            Ended {
                failure: SetupFailure::Cancelled,
                confirmed: Some(false),
            }
        );
    }

    /// Cancel while connecting revokes the pairing and puts Network Access back like the other
    /// paths. When that worked the sheet closes, and otherwise it says what stayed.
    #[test]
    fn cancel_while_connecting_revokes_and_restores() {
        let session = Session::new(true, Some(RestoredAccess::Off));
        assert_eq!(
            end(Some(&session), true, &before(false), true),
            Ended {
                failure: SetupFailure::Cancelled,
                confirmed: Some(true),
            }
        );
        assert_eq!(
            session.cancels.borrow().as_slice(),
            [NetworkRestore::new(
                NetworkPolicy::new(BIND.parse().unwrap()).unwrap(),
                &before(false)
            )]
        );
        let session = Session::new(true, Some(RestoredAccess::Failed));
        assert_eq!(
            end(Some(&session), true, &before(false), true),
            Ended {
                failure: SetupFailure::Stopped {
                    network: NetworkAfter::Restored(RestoredAccess::Failed),
                },
                confirmed: Some(true),
            }
        );
        let session = Session::new(true, None);
        assert_eq!(
            end(Some(&session), true, &before(false), true),
            Ended {
                failure: SetupFailure::Stopped {
                    network: NetworkAfter::Unconfirmed,
                },
                confirmed: Some(false),
            }
        );
    }
}

#[cfg(test)]
mod login_item_tests {
    use super::*;

    /// SSH setup's install or start on a Mac whose SpaceTerm Core is off in Login Items says so
    /// and what the person turns on, instead of an unconfirmed start or a refused installation.
    #[test]
    fn a_core_off_in_login_items_says_so_for_install_and_start() {
        let off = Some(i32::from(spaceterm_core::provisioning::LOGIN_ITEM_OFF));
        let start = step_failure(off, SetupFailure::Unconfirmed(SetupOperation::Start));
        let install = step_failure(off, SetupFailure::Refused(RefusalKind::InstallRefused));
        assert_eq!(start, install);
        assert_eq!(
            crate::ui::ssh_setup::problem(&start, "penguin").explanation,
            "SpaceTerm Core is off in Login Items on penguin. Turn it on there, then try again."
        );
        assert_eq!(
            step_failure(Some(1), SetupFailure::Unconfirmed(SetupOperation::Start)),
            SetupFailure::Unconfirmed(SetupOperation::Start),
            "any other status keeps its meaning"
        );
    }
}
