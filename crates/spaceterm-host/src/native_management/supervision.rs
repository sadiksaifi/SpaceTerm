//! System supervision of one stable launcher, independent of registration generations.
//!
//! The installation owner supplies transaction and execution custody. This module
//! owns service policy and fixed native commands, not another journal or release lock.

use std::{fmt, path::Path, process::Command};

use crate::native_management::{
    controller::ManagementPhase, facade::RuntimeAssessment, helper_lifetime::HelperDeadline,
    macos::BackgroundItemsStatus,
};
use crate::{
    installation::{QuiescenceProgress, ServerLifecyclePort},
    native_deployment::{
        contracts::{NativeFailure, NativeRequestId, PublicationDisposition},
        profile::{NativePlatform, service_label},
    },
    startup::{AccountBuildKey, DesiredServerState, InstallationId, ProtectedRegistration},
};

#[path = "supervision_commands.rs"]
pub mod commands;

/// Stable service facts. Paths come from the native installation owner, not a request.
pub struct ServiceDefinition {
    platform: NativePlatform,
    key: AccountBuildKey,
    launcher: String,
}
impl ServiceDefinition {
    /// Checks rendering bounds only. This does not authenticate or protect the launcher.
    pub fn new(
        platform: NativePlatform,
        key: AccountBuildKey,
        launcher: &Path,
    ) -> Result<Self, NativeFailure> {
        let launcher = launcher.to_str().ok_or(NativeFailure::InvalidRecord)?;
        if matches!(key.account_uid, 0 | u32::MAX)
            || !launcher.starts_with('/')
            || launcher.len() > 1024
            || !launcher
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" /._-".contains(&b))
            || launcher
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
        {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            platform,
            key,
            launcher: launcher.to_owned(),
        })
    }
    /// System service namespace; never a user/login service.
    pub fn platform(&self) -> NativePlatform {
        self.platform
    }
    /// Registered owning account and compiled build identity.
    pub fn key(&self) -> AccountBuildKey {
        self.key
    }
    /// Fixed label derived from the registered account/build, not a version or generation.
    pub fn label(&self) -> String {
        service_label(self.platform, self.key)
    }
    /// Fixed protected supervisor configuration location.
    pub fn configuration_path(&self) -> String {
        match self.platform {
            NativePlatform::Macos => format!("/Library/LaunchDaemons/{}.plist", self.label()),
            NativePlatform::Linux => format!("/etc/systemd/system/{}", self.label()),
        }
    }
    /// Stable launcher invocation. Each launch rereads protected registration and
    /// revalidates its current generation through the startup authority.
    pub fn configuration(&self) -> Vec<u8> {
        let uid = self.key.account_uid;
        let label = self.label();
        let launcher = &self.launcher;
        let identifier = label
            .rsplit_once('.')
            .map_or(label.as_str(), |(identifier, _)| identifier);
        match self.platform {
            NativePlatform::Macos => format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{launcher}</string><string>--account-uid</string><string>{uid}</string></array>
<key>UserName</key><string>root</string>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>ThrottleInterval</key><integer>20</integer>
<key>ExitTimeOut</key><integer>0</integer>
<key>AbandonProcessGroup</key><false/>
<key>WorkingDirectory</key><string>/</string>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
<key>AssociatedBundleIdentifiers</key><array><string>{identifier}</string></array>
</dict></plist>
"#).into_bytes(),
            NativePlatform::Linux => format!(r#"[Unit]
Description=SpaceTerm Server
StartLimitIntervalSec=0

[Service]
Type=exec
User=root
ExecStart="{launcher}" --account-uid {uid}
WorkingDirectory=/
Restart=on-failure
RestartSec=20s
SuccessExitStatus=0
KillMode=mixed
TimeoutStopSec=infinity
SendSIGKILL=no
StandardOutput=null
StandardError=null

[Install]
WantedBy=multi-user.target
"#).into_bytes(),
        }
    }
}
impl fmt::Debug for ServiceDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceDefinition(..)")
    }
}

/// Reads the current fixed protected registration for a stable launcher invocation.
/// The result is comparison data, not admission. The launcher must pass it to
/// NativeStartupAuthority::admit, which rereads the head and revalidates account,
/// intent, journal, custody and the release fence before credentials or execution.
pub fn current_registration_generation<R: crate::startup::NativeStartupRegistration + ?Sized>(
    key: AccountBuildKey,
    registration: &R,
) -> Result<std::num::NonZeroU64, crate::startup::StartupError> {
    crate::startup::validate_requested_key(key)?;
    let current = registration.read_fixed_registration(key)?;
    if current.key != key {
        return Err(crate::startup::StartupError::IdentityMismatch);
    }
    Ok(current.generation)
}

/// Supervisor observation, never authenticated runtime readiness or endpoint absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorObservation {
    /// Configuration is not loaded by the native service manager.
    NotLoaded,
    /// A system service is loaded without a currently running process.
    Inactive,
    /// A native service-manager transition is pending.
    Transitioning,
    /// The manager reports a running process, not an authenticated server.
    Running,
    /// Background Items requires owner approval. No login-scoped fallback is permitted.
    Suppressed,
}

/// Closed native supervisor commands. No argv or executable can arrive from the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorCommand {
    /// Observe this exact system service.
    Observe,
    /// Reload systemd's stable configuration. Launchd loads it during bootstrap.
    Reload,
    /// Restore automatic startup without terminating any process.
    Enable,
    /// Suspend automatic startup without stop, bootout, or a signal.
    Disable,
    /// Load the stable launchd system LaunchDaemon, whose RunAtLoad starts its launcher.
    Bootstrap,
    /// Start only an inactive service. Never force-restart an existing process.
    Start,
    /// Remove supervision only after the lifecycle owner proves prior-instance absence.
    Retire,
}
/// Typed reply from a bounded native utility invocation.
#[derive(Debug)]
pub enum SupervisorReply {
    /// The command was accepted. This proves neither readiness nor drainage.
    Applied,
    /// Independent supervisor state.
    Observation(SupervisorObservation),
}

/// Installation-owner authority retained through publication and supervisor execution.
/// Implementations share A3's transaction/release lock and existing native journal.
/// A bounded privileged effect holds that lock; drainage never holds it while waiting.
/// Retain/recheck must also validate the journal's Stop/Restart startup barrier.
pub trait SupervisorCustody: Send + Sync {
    /// Existing installation journal transaction, never a decoded record.
    type Transaction;
    /// Noncloneable protected launcher, configuration, utility and closure authority.
    type Lease;
    /// Rereads the current protected registration and account incarnation in the transaction.
    fn registration(
        &self,
        transaction: &Self::Transaction,
    ) -> Result<ProtectedRegistration, NativeFailure>;
    /// Returns the original journal request and installation bound at admission.
    fn original(&self, transaction: &Self::Transaction) -> (NativeRequestId, InstallationId);
    /// Reads Restart progress from the existing journal under retained transaction authority.
    /// SupervisionRemoved means the original instance and its supervision are absent;
    /// resolution must reconcile replacement startup instead of retiring again.
    fn restart_phase(
        &self,
        _transaction: &Self::Transaction,
    ) -> Result<ManagementPhase, NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
    /// Durably records SupervisionRemoved before any replacement startup effect.
    /// The owner binds this to the original Restart request and approved installation,
    /// preserving Running and fencing unrelated startup/release. The existing journal's
    /// Starting state can represent this transition; no second journal is permitted.
    /// Indeterminate publication must latch recovery closed before returning.
    fn commit_restart_supervision_removed(
        &self,
        _transaction: &Self::Transaction,
        _retirement: &PriorInstanceRetirement,
    ) -> PublicationDisposition<()> {
        PublicationDisposition::NotApplied(NativeFailure::NativeUnavailable)
    }
    /// Retains protected code, dependencies, configuration, ancestors, mount evidence
    /// and publication policy excluding writable aliases. Authenticate copies before use.
    fn retain(&self, transaction: &Self::Transaction) -> Result<Self::Lease, NativeFailure>;
    /// Returns facts from that retained authority, not a pathname supplied by the caller.
    fn definition<'a>(&self, lease: &'a Self::Lease) -> &'a ServiceDefinition;
    /// Revalidates actual retained custody and the original journal transaction.
    fn recheck(
        &self,
        transaction: &Self::Transaction,
        lease: &mut Self::Lease,
    ) -> Result<(), NativeFailure>;
    /// Publishes the fixed configuration durably using A's protected publisher.
    /// An identical retained file is idempotent. A collision/substitution is refused.
    /// Indeterminate publication latches startup/release closed in the existing journal.
    fn publish_configuration(
        &self,
        transaction: &Self::Transaction,
        lease: &mut Self::Lease,
        bytes: &[u8],
    ) -> PublicationDisposition<()>;
    /// Binds the fixed system utility's retained executable to execution and keeps
    /// its namespace/closure replacement-excluded until that exact child is reaped.
    fn bind_exec(
        &self,
        lease: &mut Self::Lease,
        command: &mut Command,
    ) -> Result<(), NativeFailure>;
    /// Arms utility execution in the existing journal before spawn. Publication and
    /// replacement exclusion must survive helper exit until exact child absence is
    /// proved. A process-local descriptor or lock alone cannot implement this port.
    /// Unknown spawn/retirement outcomes leave the owner inhibited, including after
    /// the watchdog exits. Recovery must not infer absence from a reused PID.
    fn begin_utility_custody(&self, _lease: &mut Self::Lease) -> PublicationDisposition<()> {
        PublicationDisposition::NotApplied(NativeFailure::CustodyUnavailable)
    }
    /// Resolves that same utility scope after exact reap. The owner must also prove
    /// its original native child scope absent before releasing durable exclusion.
    /// Failure leaves custody/recovery inhibited, never an unlocked retry.
    fn finish_utility_custody(&self, _lease: &mut Self::Lease) -> Result<(), NativeFailure> {
        Err(NativeFailure::CleanupUnresolved)
    }
    /// Native Background Items observation for the exact protected service owner.
    /// Linux returns Enabled-equivalent None; an unavailable Mac observation is an error.
    fn background_items(
        &self,
        lease: &Self::Lease,
    ) -> Result<Option<BackgroundItemsStatus>, NativeFailure>;
}

/// External service-manager boundary. Tests inject it only in test composition.
pub trait SupervisorCommands<C: SupervisorCustody>: Send + Sync {
    /// Executes one fixed command with bounded output and the original absolute deadline.
    fn call(
        &self,
        custody: &C,
        lease: &mut C::Lease,
        command: SupervisorCommand,
        deadline: HelperDeadline,
    ) -> Result<SupervisorReply, NativeFailure>;
}

/// Lifecycle owner's proof for one original request. It is not reconstructed from a PID,
/// endpoint pathname, empty registry, or supervisor observation.
pub struct PriorInstanceRetirement {
    request: NativeRequestId,
}
impl fmt::Debug for PriorInstanceRetirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PriorInstanceRetirement(..)")
    }
}

/// Runs the real lifecycle port. Started-write waits remain unprivileged, outside the helper.
pub struct SupervisedLifecycle<L> {
    lifecycle: L,
}
impl<L: ServerLifecyclePort<RuntimeAssessment>> SupervisedLifecycle<L> {
    /// Injects the runtime owner. Its Complete result must cover the exact assessed instance,
    /// retriever, Sessions and endpoint, after listener closure, read cancellation and write drainage.
    pub fn new(lifecycle: L) -> Self {
        Self { lifecycle }
    }
    /// Revalidates the original destructive assessment before quiescence admission.
    pub async fn begin(
        &self,
        request: NativeRequestId,
        assessment: &RuntimeAssessment,
    ) -> Result<QuiescenceProgress, NativeFailure> {
        self.lifecycle.revalidate(assessment).await?;
        self.lifecycle.begin_quiescence(request, assessment).await
    }
    /// Resolves the same admitted operation. Pending or unresolved cleanup cannot authorize bootout.
    pub async fn retirement(
        &self,
        request: NativeRequestId,
    ) -> Result<Option<PriorInstanceRetirement>, NativeFailure> {
        match self.lifecycle.resolve_quiescence(request).await? {
            QuiescenceProgress::Complete => Ok(Some(PriorInstanceRetirement { request })),
            QuiescenceProgress::Pending => Ok(None),
            QuiescenceProgress::CleanupUnresolved => Err(NativeFailure::CleanupUnresolved),
        }
    }
}

/// Native service-manager orchestration. Every call is bound to the owner's live transaction.
pub struct NativeSupervisor<C, E> {
    custody: C,
    commands: E,
}
impl<C: SupervisorCustody, E: SupervisorCommands<C>> NativeSupervisor<C, E> {
    /// Injects installation authority and the native service-manager command adapter.
    pub fn new(custody: C, commands: E) -> Self {
        Self { custody, commands }
    }
    /// Restores Running supervision for the journal's approved installation only.
    /// Repeating an interrupted restore reloads the same stable configuration.
    pub fn restore_approved(
        &self,
        transaction: &C::Transaction,
        deadline: HelperDeadline,
    ) -> Result<SupervisorObservation, NativeFailure> {
        deadline.check()?;
        let registration = self.custody.registration(transaction)?;
        if registration.desired_state != DesiredServerState::Running {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if registration.approved_installation != self.custody.original(transaction).1 {
            return Err(NativeFailure::GenerationMismatch);
        }
        let mut lease = self.custody.retain(transaction)?;
        self.custody.recheck(transaction, &mut lease)?;
        let definition = self.custody.definition(&lease);
        if definition.key() != registration.key {
            return Err(NativeFailure::IdentityMismatch);
        }
        let platform = definition.platform();
        let bytes = definition.configuration();
        self.check_background(&lease)?;
        match self
            .custody
            .publish_configuration(transaction, &mut lease, &bytes)
        {
            PublicationDisposition::Durable(()) => {}
            PublicationDisposition::NotApplied(failure) => return Err(failure),
            PublicationDisposition::Indeterminate => return Err(NativeFailure::Indeterminate),
        }
        self.custody.recheck(transaction, &mut lease)?;
        if platform == NativePlatform::Linux {
            self.apply(transaction, &mut lease, SupervisorCommand::Reload, deadline)?;
        }
        self.apply(transaction, &mut lease, SupervisorCommand::Enable, deadline)?;
        let before = self.observe_retained(transaction, &mut lease, deadline)?;
        let command = match (platform, before) {
            (_, SupervisorObservation::Running | SupervisorObservation::Transitioning) => {
                return Ok(before);
            }
            (_, SupervisorObservation::Suppressed) => {
                return Err(NativeFailure::BackgroundItemsSuppressed);
            }
            (NativePlatform::Macos, SupervisorObservation::NotLoaded) => {
                SupervisorCommand::Bootstrap
            }
            (_, SupervisorObservation::NotLoaded | SupervisorObservation::Inactive) => {
                SupervisorCommand::Start
            }
        };
        self.apply(transaction, &mut lease, command, deadline)?;
        // Supervisor acceptance is deliberately separate from authenticated runtime readiness.
        self.observe_retained(transaction, &mut lease, deadline)
    }
    /// Suspends automatic startup after a durable Stopped head or Restart barrier.
    /// This never signals or retires the running instance.
    pub fn suspend(
        &self,
        transaction: &C::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        let mut lease = self.custody.retain(transaction)?;
        self.apply(
            transaction,
            &mut lease,
            SupervisorCommand::Disable,
            deadline,
        )
    }
    /// Removes system supervision only after exact prior-instance retirement.
    pub fn retire(
        &self,
        transaction: &C::Transaction,
        retirement: &PriorInstanceRetirement,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.check_retirement(transaction, retirement)?;
        let mut lease = self.custody.retain(transaction)?;
        match self.observe_service_retained(transaction, &mut lease, deadline)? {
            SupervisorObservation::NotLoaded => return Ok(()),
            SupervisorObservation::Inactive => {}
            SupervisorObservation::Transitioning => return Err(NativeFailure::OutcomePending),
            SupervisorObservation::Running => return Err(NativeFailure::CleanupUnresolved),
            SupervisorObservation::Suppressed => {
                return Err(NativeFailure::BackgroundItemsSuppressed);
            }
        }
        self.apply(transaction, &mut lease, SupervisorCommand::Retire, deadline)?;
        match (
            self.custody.definition(&lease).platform(),
            self.observe_service_retained(transaction, &mut lease, deadline)?,
        ) {
            (_, SupervisorObservation::NotLoaded)
            | (NativePlatform::Linux, SupervisorObservation::Inactive) => Ok(()),
            (_, SupervisorObservation::Transitioning)
            | (NativePlatform::Macos, SupervisorObservation::Inactive) => {
                Err(NativeFailure::OutcomePending)
            }
            _ => Err(NativeFailure::CleanupUnresolved),
        }
    }
    /// One journaled Restart, never an unrelated native Stop/Start pair or staged activation.
    pub fn restart_approved(
        &self,
        transaction: &C::Transaction,
        retirement: &PriorInstanceRetirement,
        deadline: HelperDeadline,
    ) -> Result<SupervisorObservation, NativeFailure> {
        deadline.check()?;
        self.check_retirement(transaction, retirement)?;
        let registration = self.custody.registration(transaction)?;
        if registration.desired_state != DesiredServerState::Running {
            return Err(NativeFailure::UnsafeNativeState);
        }
        if registration.approved_installation != self.custody.original(transaction).1 {
            return Err(NativeFailure::GenerationMismatch);
        }
        match self.custody.restart_phase(transaction)? {
            ManagementPhase::Quiesced => {
                self.retire(transaction, retirement, deadline)?;
                deadline.check()?;
                match self
                    .custody
                    .commit_restart_supervision_removed(transaction, retirement)
                {
                    PublicationDisposition::Durable(()) => {}
                    PublicationDisposition::NotApplied(failure) => return Err(failure),
                    PublicationDisposition::Indeterminate => {
                        return Err(NativeFailure::Indeterminate);
                    }
                }
                if self.custody.restart_phase(transaction)? != ManagementPhase::SupervisionRemoved {
                    return Err(NativeFailure::UnsafeNativeState);
                }
            }
            ManagementPhase::SupervisionRemoved => {}
            _ => return Err(NativeFailure::UnsafeNativeState),
        }
        // A failed utility reply may conceal an already-running replacement. The
        // durable journal transition forbids treating it as the original instance.
        // This observation is not authenticated readiness or a completed Restart.
        self.restore_approved(transaction, deadline)
    }
    /// Read-only native supervisor observation, kept separate from registered intent/readiness.
    pub fn observe(
        &self,
        transaction: &C::Transaction,
        deadline: HelperDeadline,
    ) -> Result<SupervisorObservation, NativeFailure> {
        let mut lease = self.custody.retain(transaction)?;
        self.observe_retained(transaction, &mut lease, deadline)
    }
    fn check_retirement(
        &self,
        transaction: &C::Transaction,
        retirement: &PriorInstanceRetirement,
    ) -> Result<(), NativeFailure> {
        if self.custody.original(transaction).0 != retirement.request {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn apply(
        &self,
        transaction: &C::Transaction,
        lease: &mut C::Lease,
        command: SupervisorCommand,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        deadline.check()?;
        self.custody.recheck(transaction, lease)?;
        let reply = self
            .commands
            .call(&self.custody, lease, command, deadline)?;
        self.custody.recheck(transaction, lease)?;
        deadline.check()?;
        match reply {
            SupervisorReply::Applied => Ok(()),
            _ => Err(NativeFailure::InvalidRecord),
        }
    }
    fn check_background(&self, lease: &C::Lease) -> Result<(), NativeFailure> {
        match (
            self.custody.definition(lease).platform(),
            self.custody.background_items(lease)?,
        ) {
            (NativePlatform::Linux, None) => Ok(()),
            (NativePlatform::Macos, Some(status)) => status.supervision(),
            _ => Err(NativeFailure::NativeUnavailable),
        }
    }
    fn observe_retained(
        &self,
        transaction: &C::Transaction,
        lease: &mut C::Lease,
        deadline: HelperDeadline,
    ) -> Result<SupervisorObservation, NativeFailure> {
        deadline.check()?;
        self.custody.recheck(transaction, lease)?;
        match self.check_background(lease) {
            Err(NativeFailure::BackgroundItemsSuppressed) => {
                return Ok(SupervisorObservation::Suppressed);
            }
            Err(failure) => return Err(failure),
            Ok(()) => {}
        }
        self.observe_service_retained(transaction, lease, deadline)
    }
    fn observe_service_retained(
        &self,
        transaction: &C::Transaction,
        lease: &mut C::Lease,
        deadline: HelperDeadline,
    ) -> Result<SupervisorObservation, NativeFailure> {
        deadline.check()?;
        self.custody.recheck(transaction, lease)?;
        let reply =
            self.commands
                .call(&self.custody, lease, SupervisorCommand::Observe, deadline)?;
        self.custody.recheck(transaction, lease)?;
        deadline.check()?;
        match reply {
            SupervisorReply::Observation(state) => Ok(state),
            _ => Err(NativeFailure::InvalidRecord),
        }
    }
}
