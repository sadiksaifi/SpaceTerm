//! Content-free observations. JSONL is a report, never installation or cleanup authority.

use crate::{
    native_deployment::{
        bindings::hex,
        contracts::{NativeFailure, NativeRequestId},
    },
    session_capabilities::{
        Capability, CapabilityObservation, CleanupClass, EffectClass, LocalEnvironmentComparison,
        LocaleDifference, PathDifference, PreflightClass, ProbeContext, ProbeOutcome,
        ResponsibleProcess,
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Write, sync::Mutex};

const RECORD_BYTES: usize = 8192;
const RUN_RECORDS: u32 = 4096;
const RUN_BYTES: usize = 32 * 1024 * 1024;

/// The exact candidate and run observed by every producer in one output stream.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct EvidenceIdentity {
    run: NativeRequestId,
    revision: [u8; 20],
    dirty: bool,
    artifact_sha256: [u8; 32],
}
impl EvidenceIdentity {
    /// Checks public comparison facts, not candidate authentication.
    pub fn new(
        run: [u8; 16],
        revision: [u8; 20],
        dirty: bool,
        artifact_sha256: [u8; 32],
    ) -> Result<Self, NativeFailure> {
        if revision == [0; 20] || artifact_sha256 == [0; 32] {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self {
            run: NativeRequestId::new(run)?,
            revision,
            dirty,
            artifact_sha256,
        })
    }
}

/// Bounded host version observations, not inferred host support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeHostObservation {
    /// Apple Silicon macOS and its kernel.
    Macos {
        #[doc = "os version"]
        os_version: [u16; 3],
        #[doc = "kernel version"]
        kernel_version: [u16; 3],
    },
    /// x86_64 Linux, glibc and systemd.
    Linux {
        #[doc = "os version"]
        os_version: [u16; 3],
        #[doc = "kernel version"]
        kernel_version: [u16; 3],
        #[doc = "libc version"]
        libc_version: [u16; 3],
        #[doc = "supervisor version"]
        supervisor_version: [u16; 3],
    },
}
impl NativeHostObservation {
    /// Records versions observed from this host.
    pub fn macos(os_version: [u16; 3], kernel_version: [u16; 3]) -> Self {
        Self::Macos {
            os_version,
            kernel_version,
        }
    }
    fn supported(self) -> bool {
        match self {
            Self::Macos { os_version, .. } => os_version[0] >= 26,
            Self::Linux {
                kernel_version,
                libc_version,
                supervisor_version,
                ..
            } => {
                kernel_version >= [5, 15, 0]
                    && libc_version >= [2, 35, 0]
                    && supervisor_version[0] >= 249
            }
        }
    }
    fn value(self) -> Value {
        match self {
            Self::Macos {
                os_version,
                kernel_version,
            } => json!({
                "platform": "macos", "architecture": "arm64", "os_version": os_version,
                "kernel_version": kernel_version, "libc_version": null,
                "supervisor": "launchd", "supervisor_version": null,
            }),
            Self::Linux {
                os_version,
                kernel_version,
                libc_version,
                supervisor_version,
            } => json!({
                "platform": "linux", "architecture": "x86_64", "os_version": os_version,
                "kernel_version": kernel_version, "libc_version": libc_version,
                "supervisor": "systemd", "supervisor_version": supervisor_version,
            }),
        }
    }
}

macro_rules! names {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
        pub enum $name { $(#[doc = $text] $variant),+ }
        impl $name {
            /// Returns the closed JSONL identifier.
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $text),+ } }
        }
    };
}
names! {
    /// Actual parenting or isolated host context.
    EvidenceContext {
        GuiParented => "gui-parented", DaemonParented => "daemon-parented", Headless => "headless",
        DisposableMacos => "disposable-macos", DisposableJammy => "disposable-jammy"
    }
}
impl From<ProbeContext> for EvidenceContext {
    fn from(value: ProbeContext) -> Self {
        match value {
            ProbeContext::GuiParented => Self::GuiParented,
            ProbeContext::DaemonParented => Self::DaemonParented,
        }
    }
}
names! {
    /// Closed scenario outcomes. Missing observations are not success.
    EvidenceResult {
        Passed => "passed", Denied => "denied", Cancelled => "cancelled", Unavailable => "unavailable",
        TimedOut => "timed-out", Unrun => "unrun", Failed => "failed", Pending => "pending",
        Indeterminate => "indeterminate", CleanupUnresolved => "cleanup-unresolved"
    }
}
/// A typed outcome whose diagnostics cannot contain native text.
#[derive(Clone, Copy, Debug)]
pub struct EvidenceOutcome {
    result: EvidenceResult,
    failure: Option<NativeFailure>,
}
impl EvidenceOutcome {
    /// Validates the relationship between the result and typed failure.
    pub fn new(
        result: EvidenceResult,
        failure: Option<NativeFailure>,
    ) -> Result<Self, NativeFailure> {
        if (result == EvidenceResult::Passed && failure.is_some())
            || match result {
                EvidenceResult::TimedOut => failure != Some(NativeFailure::TimedOut),
                EvidenceResult::Indeterminate => failure != Some(NativeFailure::Indeterminate),
                EvidenceResult::CleanupUnresolved => {
                    failure != Some(NativeFailure::CleanupUnresolved)
                }
                EvidenceResult::Denied | EvidenceResult::Cancelled => failure.is_none(),
                _ => false,
            }
        {
            return Err(NativeFailure::InvalidRecord);
        }
        Ok(Self { result, failure })
    }
    /// A complete successful observation.
    pub fn passed() -> Self {
        Self {
            result: EvidenceResult::Passed,
            failure: None,
        }
    }
    /// Incomplete work retained for explicit resolution.
    pub fn pending(failure: NativeFailure) -> Self {
        Self {
            result: EvidenceResult::Pending,
            failure: Some(failure),
        }
    }
}

macro_rules! flags {
    ($(#[$meta:meta])* $name:ident { $($field:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, serde::Serialize)]
        pub struct $name { $(#[doc = stringify!($field)] pub $field: bool),+ }
        impl $name { fn complete(self) -> bool { true $(&& self.$field)+ } }
    };
}
flags! { /// Exact credential retrieval and retirement observations.
    CredentialObservation { policy_accepted, interaction_disabled, retrieved, secret_wiped, release_rechecked, retriever_absent }
}
flags! { /// Actual denied release and exact retriever retirement.
    CredentialDenialObservation { attempt_observed, release_denied, secret_wiped, retriever_absent }
}
flags! { /// Independent privilege, execution, descriptor and retirement observations.
    PrivilegeObservation {
        real_ids_verified, effective_ids_verified, saved_ids_verified, supplementary_groups_verified,
        execution_bound, hardened_runtime_verified, descriptor_scope_verified, secret_channels_closed,
        custody_descriptors_absent, retriever_absent, root_process_absent, root_listener_absent
    }
}
flags! { /// Authenticated startup and previous-instance retirement.
    StartupObservation { readiness_authenticated, original_process_retired, endpoint_bound }
}
flags! { /// Retained server identity and state after an actual signed update.
    UpdateObservation { fresh_version_authenticated, approval_durable, candidate_running, identity_retained,
        pairings_retained, organization_retained, roots_retained, remote_access_retained,
        previous_process_absent, sessions_retired, started_writes_settled }
}
flags! { /// Actual protected-copy authentication before privileged execution.
    BootstrapObservation { candidate_manifest_verified, candidate_signatures_verified, protected_admin_digest_verified,
        protected_admin_signature_verified, protected_admin_policy_verified, verified_before_root_exec,
        receipt_durable_before_effects, root_execution_observed, fresh_copy, collision_check_complete }
}
flags! { /// Exact retired scope and endpoint absence, independently of output delivery.
    CleanupObservation { receipt_retained, scope_verified, process_absent, retriever_absent, sessions_absent, endpoint_absent }
}
names! { /// The negative control actually attempted.
    CredentialDenialCase { UnprivilegedCopy => "unprivileged-copy-denied", UnsignedCopy => "unsigned-copy-denied",
        WrongIdentifier => "wrong-identifier-denied", OtherAccount => "other-account-denied",
        Stopped => "stopped-retrieval-denied", StaleGeneration => "stale-generation-denied" }
}
names! { /// The state actually read from protected registration.
    ObservedDesiredState { Running => "running", Stopped => "stopped", Unknown => "unknown" }
}
names! { /// The installation selection actually observed.
    ObservedSelection { Approved => "approved", Staged => "staged", Unknown => "unknown" }
}
names! { /// The startup cause actually observed.
    ObservedTrigger { None => "none", Crash => "crash", Start => "start", Restart => "restart" }
}
names! { /// The observed startup trigger, not an assumed service status.
    StartupCase { ApprovedSelection => "startup-never-selects-staged-installation", Crash => "crash-restart",
        Start => "explicit-start", Restart => "approved-restart" }
}
names! { /// A single fixed authorization outcome.
    AuthorizationCase { Denied => "authorization-denied", Cancelled => "authorization-cancelled",
        Unavailable => "authorization-unavailable", Granted => "authorization-granted" }
}
names! { /// Native management operations allowed in an authorization report.
    EvidenceOperation { Install => "install", Stage => "stage", Activate => "activate", Start => "start", Stop => "stop",
        Restart => "restart", Uninstall => "uninstall", Repair => "repair", Collect => "collect",
        RemoteAccess => "remote-access", OwnerEnrollment => "owner-enrollment" }
}
names! { /// Observed Stop phase order.
    StopPhase { StoppedDurable => "stopped-durable", AdmissionsClosed => "admissions-closed", WritesSettled => "writes-settled",
        SessionsRetired => "sessions-retired", ProcessAbsent => "process-absent", EndpointAbsent => "endpoint-absent",
        SupervisorRemoved => "supervisor-removed" }
}
const STOP_PHASES: [StopPhase; 7] = [
    StopPhase::StoppedDurable,
    StopPhase::AdmissionsClosed,
    StopPhase::WritesSettled,
    StopPhase::SessionsRetired,
    StopPhase::ProcessAbsent,
    StopPhase::EndpointAbsent,
    StopPhase::SupervisorRemoved,
];

/// Closed scenario observations. No field accepts paths, secrets, native errors or environment values.
pub enum NativeScenario {
    /// Stopped was read before any credential or installation retention call.
    Stopped {
        #[doc = "Protected registration state."]
        desired_state: ObservedDesiredState,
        #[doc = "credential calls"]
        credential_calls: u16,
        #[doc = "installation retention calls"]
        installation_retention_calls: u16,
        #[doc = "process absent"]
        process_absent: bool,
        #[doc = "endpoint absent"]
        endpoint_absent: bool,
    },
    /// Approved-only startup observed for the named trigger.
    Startup {
        #[doc = "case"]
        case: StartupCase,
        #[doc = "Protected registration state."]
        desired_state: ObservedDesiredState,
        #[doc = "Actual selected installation."]
        selection: ObservedSelection,
        #[doc = "Actual startup trigger."]
        trigger: ObservedTrigger,
        #[doc = "observation"]
        observation: StartupObservation,
    },
    /// Actual exact-startup credential retrieval.
    Credential(CredentialObservation),
    /// A denied credential control with no released secret.
    CredentialDenial {
        #[doc = "case"]
        case: CredentialDenialCase,
        #[doc = "observation"]
        observation: CredentialDenialObservation,
        #[doc = "retrieved"]
        retrieved: bool,
    },
    /// macOS privilege drop. The complete authenticated group list is retained as evidence.
    Privilege {
        #[doc = "observation"]
        observation: PrivilegeObservation,
        #[doc = "primary gid"]
        primary_gid: u32,
        #[doc = "supplementary groups"]
        supplementary_groups: Vec<u32>,
    },
    /// Started writes were counted and phases independently observed.
    Drain {
        #[doc = "started writes"]
        started_writes: u16,
        #[doc = "settled writes"]
        settled_writes: u16,
        #[doc = "phase order"]
        phase_order: Vec<StopPhase>,
        #[doc = "retriever absent"]
        retriever_absent: bool,
    },
    /// An actual signed update and preserved state.
    Update(UpdateObservation),
    /// An OS authorization attempt and helper retirement.
    Authorization {
        #[doc = "case"]
        case: AuthorizationCase,
        #[doc = "operation"]
        operation: EvidenceOperation,
        #[doc = "attempt observed"]
        attempt_observed: bool,
        #[doc = "granted"]
        granted: bool,
        #[doc = "caller bound"]
        caller_bound: bool,
        #[doc = "helper absent"]
        helper_absent: bool,
    },
    /// Protected bootstrap, not candidate preflight.
    Bootstrap(BootstrapObservation),
    /// Cleanup observations, not permission to remove resources.
    Cleanup {
        #[doc = "observation"]
        observation: CleanupObservation,
        #[doc = "remaining objects"]
        remaining_objects: u16,
    },
    /// The actual bounded probe record, with preflight separate from effects.
    Capability(CapabilityObservation),
    /// A local comparison to a public baseline, never comparison with a private peer.
    Environment {
        #[doc = "comparison"]
        comparison: LocalEnvironmentComparison,
        #[doc = "login"]
        login: bool,
        #[doc = "interactive"]
        interactive: bool,
        #[doc = "startup order verified"]
        startup_order_verified: bool,
    },
}

/// A checked report. Its contents do not grant native authority.
pub struct NativeEvidenceRecord {
    identity: EvidenceIdentity,
    host: NativeHostObservation,
    context: EvidenceContext,
    outcome: EvidenceOutcome,
    scenario: NativeScenario,
}
impl std::fmt::Debug for NativeEvidenceRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeEvidenceRecord(..)")
    }
}
impl NativeEvidenceRecord {
    /// Checks actual scenario observations and refuses unsupported success claims.
    pub fn observed(
        identity: EvidenceIdentity,
        host: NativeHostObservation,
        context: EvidenceContext,
        scenario: NativeScenario,
        outcome: EvidenceOutcome,
    ) -> Result<Self, NativeFailure> {
        let record = Self {
            identity,
            host,
            context,
            scenario,
            outcome,
        };
        record.validate()?;
        Ok(record)
    }
    /// Converts an existing fixed native probe observation without inventing effects.
    pub fn capability(
        identity: EvidenceIdentity,
        host: NativeHostObservation,
        observation: CapabilityObservation,
    ) -> Result<Self, NativeFailure> {
        let result = probe_result(observation.outcome());
        let failure = match result {
            EvidenceResult::Passed => None,
            EvidenceResult::Denied => Some(NativeFailure::AuthorizationDenied),
            EvidenceResult::Cancelled => Some(NativeFailure::AuthorizationCancelled),
            EvidenceResult::Unavailable | EvidenceResult::Unrun => {
                Some(NativeFailure::NativeUnavailable)
            }
            EvidenceResult::TimedOut => Some(NativeFailure::TimedOut),
            EvidenceResult::Pending => Some(NativeFailure::OutcomePending),
            EvidenceResult::Indeterminate => Some(NativeFailure::Indeterminate),
            EvidenceResult::CleanupUnresolved => Some(NativeFailure::CleanupUnresolved),
            EvidenceResult::Failed => Some(NativeFailure::UnsafeNativeState),
        };
        Self::observed(
            identity,
            host,
            observation.context().into(),
            NativeScenario::Capability(observation),
            EvidenceOutcome::new(result, failure)?,
        )
    }
    /// Retains the exact local reference and parenting context from schema 2.
    pub fn environment(
        identity: EvidenceIdentity,
        host: NativeHostObservation,
        comparison: LocalEnvironmentComparison,
        login: bool,
        interactive: bool,
        startup_order_verified: bool,
        outcome: EvidenceOutcome,
    ) -> Result<Self, NativeFailure> {
        Self::observed(
            identity,
            host,
            comparison.context().into(),
            NativeScenario::Environment {
                comparison,
                login,
                interactive,
                startup_order_verified,
            },
            outcome,
        )
    }
    fn validate(&self) -> Result<(), NativeFailure> {
        let result = self.outcome.result;
        if result == EvidenceResult::Passed && !self.host.supported() {
            return Err(NativeFailure::UnsupportedHost);
        }
        if matches!(
            (self.context, self.host),
            (
                EvidenceContext::DisposableMacos,
                NativeHostObservation::Linux { .. }
            ) | (
                EvidenceContext::DisposableJammy,
                NativeHostObservation::Macos { .. }
            )
        ) {
            return Err(NativeFailure::InvalidRecord);
        }
        if matches!(result, EvidenceResult::Denied | EvidenceResult::Cancelled)
            && !matches!(
                self.scenario,
                NativeScenario::CredentialDenial { .. }
                    | NativeScenario::Authorization { .. }
                    | NativeScenario::Capability(_)
            )
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let passed = result == EvidenceResult::Passed;
        let valid = match &self.scenario {
            NativeScenario::Stopped {
                desired_state,
                credential_calls,
                installation_retention_calls,
                process_absent,
                endpoint_absent,
            } => {
                *credential_calls <= 4096
                    && *installation_retention_calls <= 4096
                    && (!passed
                        || (*desired_state == ObservedDesiredState::Stopped
                            && *credential_calls == 0
                            && *installation_retention_calls == 0
                            && *process_absent
                            && *endpoint_absent))
            }
            NativeScenario::Startup {
                case,
                desired_state,
                selection,
                trigger,
                observation,
            } => {
                let expected = match case {
                    StartupCase::ApprovedSelection => ObservedTrigger::None,
                    StartupCase::Crash => ObservedTrigger::Crash,
                    StartupCase::Start => ObservedTrigger::Start,
                    StartupCase::Restart => ObservedTrigger::Restart,
                };
                !passed
                    || (*desired_state == ObservedDesiredState::Running
                        && *selection == ObservedSelection::Approved
                        && *trigger == expected
                        && observation.complete())
            }
            NativeScenario::Credential(observation) => {
                (!passed || observation.complete())
                    && (!matches!(result, EvidenceResult::Unavailable | EvidenceResult::Unrun)
                        || !observation.retrieved)
            }
            NativeScenario::CredentialDenial {
                observation,
                retrieved,
                ..
            } => {
                !passed
                    && (!matches!(
                        result,
                        EvidenceResult::Denied
                            | EvidenceResult::Cancelled
                            | EvidenceResult::Unavailable
                            | EvidenceResult::Unrun
                    ) || !retrieved)
                    && (result != EvidenceResult::Denied || observation.complete())
            }
            NativeScenario::Privilege {
                observation,
                primary_gid,
                supplementary_groups,
            } => {
                supplementary_groups.len() <= 256
                    && *primary_gid != u32::MAX
                    && !supplementary_groups.contains(&u32::MAX)
                    && (!passed
                        || (matches!(self.host, NativeHostObservation::Macos { .. })
                            && observation.complete()
                            && *primary_gid != 0
                            && !supplementary_groups.contains(&0)
                            && supplementary_groups.first() == Some(primary_gid)))
            }
            NativeScenario::Drain {
                started_writes,
                settled_writes,
                phase_order,
                retriever_absent,
            } => {
                *started_writes <= 4096
                    && *settled_writes <= 4096
                    && phase_order.len() <= 7
                    && phase_order.iter().collect::<BTreeSet<_>>().len() == phase_order.len()
                    && (!passed
                        || (*started_writes > 0
                            && started_writes == settled_writes
                            && phase_order == &STOP_PHASES
                            && *retriever_absent))
            }
            NativeScenario::Update(observation) => !passed || observation.complete(),
            NativeScenario::Bootstrap(observation) => !passed || observation.complete(),
            NativeScenario::Cleanup {
                observation,
                remaining_objects,
            } => {
                *remaining_objects <= 4096
                    && (!passed || (observation.complete() && *remaining_objects == 0))
            }
            NativeScenario::Authorization {
                case,
                attempt_observed,
                granted,
                caller_bound,
                helper_absent,
                ..
            } => {
                let expected = match case {
                    AuthorizationCase::Denied => (
                        EvidenceResult::Denied,
                        Some(NativeFailure::AuthorizationDenied),
                    ),
                    AuthorizationCase::Cancelled => (
                        EvidenceResult::Cancelled,
                        Some(NativeFailure::AuthorizationCancelled),
                    ),
                    AuthorizationCase::Unavailable => (
                        EvidenceResult::Unavailable,
                        Some(NativeFailure::AuthorizationUnavailable),
                    ),
                    AuthorizationCase::Granted => (EvidenceResult::Passed, None),
                };
                !matches!(
                    result,
                    EvidenceResult::Passed
                        | EvidenceResult::Denied
                        | EvidenceResult::Cancelled
                        | EvidenceResult::Unavailable
                ) || ((result, self.outcome.failure) == expected
                    && *attempt_observed
                    && *helper_absent
                    && *granted == passed
                    && (!passed || *caller_bound))
            }
            NativeScenario::Capability(observation) => {
                self.context == EvidenceContext::from(observation.context())
                    && result == probe_result(observation.outcome())
                    && (!matches!(
                        result,
                        EvidenceResult::Passed | EvidenceResult::Denied | EvidenceResult::Cancelled
                    ) || observation.responsible_process() != ResponsibleProcess::Unknown)
                    && (result != EvidenceResult::Denied
                        || observation.effect() == EffectClass::Denied)
                    && (!passed
                        || (observation.effect() == EffectClass::Verified
                            && observation.preflight() == PreflightClass::Allowed))
                    && (!matches!(
                        result,
                        EvidenceResult::Denied
                            | EvidenceResult::Cancelled
                            | EvidenceResult::Unavailable
                            | EvidenceResult::Unrun
                    ) || observation.effect() != EffectClass::Verified)
            }
            NativeScenario::Environment {
                comparison,
                login,
                interactive,
                startup_order_verified,
            } => {
                self.context == EvidenceContext::from(comparison.context())
                    && (!passed || (*login && *interactive && *startup_order_verified))
            }
        };
        if valid {
            Ok(())
        } else {
            Err(NativeFailure::InvalidRecord)
        }
    }
    fn fields(&self) -> (&'static str, &'static str, Value) {
        match &self.scenario {
            NativeScenario::Stopped {
                desired_state,
                credential_calls,
                installation_retention_calls,
                process_absent,
                endpoint_absent,
            } => (
                "native-installation",
                "stopped-before-credential-and-retention",
                json!({"kind": "lifecycle", "desired_state": desired_state.as_str(), "credential_calls": credential_calls,
                    "installation_retention_calls": installation_retention_calls, "process_absent": process_absent, "endpoint_absent": endpoint_absent}),
            ),
            NativeScenario::Startup {
                case,
                desired_state,
                selection,
                trigger,
                observation,
            } => {
                let mut value = json!(observation);
                value["kind"] = json!("startup");
                value["desired_state"] = json!(desired_state.as_str());
                value["selection"] = json!(selection.as_str());
                value["trigger"] = json!(trigger.as_str());
                (
                    if *case == StartupCase::ApprovedSelection {
                        "native-installation"
                    } else {
                        "macos-management"
                    },
                    case.as_str(),
                    value,
                )
            }
            NativeScenario::Credential(o) => (
                "macos-keychain",
                "exact-startup-retrieval",
                json!({"kind": "credential", "policy": if o.policy_accepted {"accepted"} else {"unchecked"},
                "interaction": if o.interaction_disabled {"disabled"} else {"unchecked"}, "retrieved": o.retrieved, "secret_wiped": o.secret_wiped, "release_rechecked": o.release_rechecked, "retriever_absent": o.retriever_absent}),
            ),
            NativeScenario::CredentialDenial {
                case,
                observation,
                retrieved,
            } => {
                let mut value = json!(observation);
                value["kind"] = json!("credential-denial");
                value["reason"] = json!(case.as_str());
                value["retrieved"] = json!(retrieved);
                ("macos-keychain", case.as_str(), value)
            }
            NativeScenario::Privilege {
                observation,
                primary_gid,
                supplementary_groups,
            } => {
                let mut value = json!(observation);
                value["kind"] = json!("privilege");
                value["primary_gid"] = json!(primary_gid);
                value["supplementary_groups"] = json!(supplementary_groups);
                ("macos-launcher", "privilege-and-inheritance-proof", value)
            }
            NativeScenario::Drain {
                started_writes,
                settled_writes,
                phase_order,
                retriever_absent,
            } => (
                "macos-management",
                "stop-drains-started-git-write-before-bootout",
                json!({"kind": "drain", "desired_state": "stopped", "started_writes": started_writes, "settled_writes": settled_writes,
                    "phase_order": phase_order.iter().map(|phase| phase.as_str()).collect::<Vec<_>>(), "retriever_absent": retriever_absent}),
            ),
            NativeScenario::Update(o) => {
                let mut value = json!(o);
                value["kind"] = json!("update");
                ("macos-management", "fresh-signed-update", value)
            }
            NativeScenario::Authorization {
                case,
                operation,
                attempt_observed,
                granted,
                caller_bound,
                helper_absent,
            } => (
                "macos-management",
                case.as_str(),
                json!({"kind": "authorization", "operation": operation.as_str(), "attempt_observed": attempt_observed, "granted": granted, "caller_bound": caller_bound, "helper_absent": helper_absent}),
            ),
            NativeScenario::Bootstrap(o) => {
                let mut value = json!(o);
                value["kind"] = json!("bootstrap");
                (
                    "macos-evidence",
                    "bootstrap-authenticates-protected-admin-before-root-exec",
                    value,
                )
            }
            NativeScenario::Cleanup {
                observation,
                remaining_objects,
            } => {
                let mut value = json!(observation);
                value["kind"] = json!("cleanup");
                value["remaining_objects"] = json!(remaining_objects);
                ("macos-evidence", "receipt-cleanup", value)
            }
            NativeScenario::Capability(o) => (
                "macos-capabilities",
                capability_name(o.capability()),
                json!({"kind": "capability", "capability": o.capability() as u32,
                "preflight": match o.preflight() { PreflightClass::Allowed => "passed", PreflightClass::Denied => "denied", PreflightClass::Unavailable => "unavailable", PreflightClass::Unknown | PreflightClass::NotDetermined => "unrun" },
                "effect": match o.effect() { EffectClass::Verified => "passed", EffectClass::Denied => "denied", EffectClass::Unavailable => "unavailable", EffectClass::Failed => "failed", EffectClass::Unobserved => "unrun", EffectClass::CompletedUnattributed => "indeterminate" },
                "responsible_process": match o.responsible_process() { ResponsibleProcess::Desktop => "desktop", ResponsibleProcess::Server => "server", ResponsibleProcess::Probe => "probe", ResponsibleProcess::Companion => "companion", ResponsibleProcess::Other => "other", ResponsibleProcess::Unknown => "unknown" },
                "cleanup": match o.cleanup() { CleanupClass::NotNeeded => "not-needed", CleanupClass::Verified => "complete", CleanupClass::Unresolved => "unresolved" }}),
            ),
            NativeScenario::Environment {
                comparison,
                login,
                interactive,
                startup_order_verified,
            } => (
                "macos-capabilities",
                "normalized-shell-comparison",
                json!({"kind": "environment-comparison", "reference": "sanitized-launch-v1", "path_difference": match comparison.path() { PathDifference::Equal => "equal", PathDifference::PresenceChanged => "presence-changed", PathDifference::ComponentsChanged => "components-changed", PathDifference::OrderChanged => "order-changed" },
                    "locale_difference": match comparison.locale() { LocaleDifference::Equal => "equal", LocaleDifference::PresenceChanged => "presence-changed", LocaleDifference::ValuesChanged => "values-changed" }, "login": login, "interactive": interactive, "startup_order_verified": startup_order_verified}),
            ),
        }
    }
    /// Encodes a bounded report. The stream owns sequence assignment.
    pub fn encode_jsonl(&self, sequence: u32) -> Result<Vec<u8>, NativeFailure> {
        if !(1..=RUN_RECORDS).contains(&sequence) {
            return Err(NativeFailure::LimitExceeded);
        }
        self.validate()?;
        let (job, case, facts) = self.fields();
        let mut value = json!({"schema": 1, "scope": "native-deployment", "run_id": hex(self.identity.run.as_bytes()), "sequence": sequence,
            "job": job, "case": case, "host": self.host.value(), "revision": hex(&self.identity.revision), "dirty": self.identity.dirty,
            "artifact_sha256": hex(&self.identity.artifact_sha256), "context": self.context.as_str(), "proof": "native",
            "result": self.outcome.result.as_str(), "failure": self.outcome.failure.map(failure_name), "facts": facts});
        value.sort_all_objects();
        let mut bytes = serde_json::to_vec(&value).map_err(|_| NativeFailure::InvalidRecord)?;
        bytes.push(b'\n');
        if bytes.len() > RECORD_BYTES {
            return Err(NativeFailure::LimitExceeded);
        }
        Ok(bytes)
    }
}

fn probe_result(outcome: ProbeOutcome) -> EvidenceResult {
    match outcome {
        ProbeOutcome::Passed => EvidenceResult::Passed,
        ProbeOutcome::Denied => EvidenceResult::Denied,
        ProbeOutcome::Cancelled => EvidenceResult::Cancelled,
        ProbeOutcome::Unavailable => EvidenceResult::Unavailable,
        ProbeOutcome::TimedOut => EvidenceResult::TimedOut,
        ProbeOutcome::Unrun => EvidenceResult::Unrun,
        ProbeOutcome::Failed => EvidenceResult::Failed,
        ProbeOutcome::Pending => EvidenceResult::Pending,
        ProbeOutcome::Indeterminate => EvidenceResult::Indeterminate,
        ProbeOutcome::CleanupUnresolved => EvidenceResult::CleanupUnresolved,
    }
}
fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::ProtectedFolders => "protected-folders",
        Capability::SelectedVolume => "selected-volume",
        Capability::Microphone => "microphone",
        Capability::ScreenRecording => "screen-recording",
        Capability::Accessibility => "accessibility",
        Capability::Clipboard => "clipboard-tools",
        Capability::UrlOpening => "url-opening",
        Capability::AppleEvents => "apple-events",
        Capability::LoginKeychain => "login-keychain",
        Capability::SshAgent => "ssh-agent",
        Capability::GpgAgent => "gpg-agent",
        Capability::TouchIdSudo => "touch-id-sudo",
        Capability::LoginShell => "shell-environment",
    }
}

/// Bounded JSONL emission. Resource identities stay in protected inventory.
pub trait NativeEvidenceEmitter: Send + Sync {
    /// Emits actual observations. Output failure must not bypass independent cleanup.
    fn emit(&self, record: &NativeEvidenceRecord) -> Result<(), NativeFailure>;
}
/// Counts validated delivered records, not native gate completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmissionSummary {
    #[doc = "Delivered record count."]
    pub records: u32,
    #[doc = "Delivered byte count."]
    pub bytes: usize,
}
struct OutputState<W> {
    writer: W,
    identity: Option<(EvidenceIdentity, NativeHostObservation)>,
    seen: BTreeSet<(&'static str, &'static str, EvidenceContext)>,
    summary: EmissionSummary,
    failure: Option<NativeFailure>,
    finished: bool,
}
/// One bounded stream. Any invalid record, lost write or flush latches failure.
pub struct JsonlEvidenceEmitter<W> {
    state: Mutex<OutputState<W>>,
}
impl<W: Write + Send> JsonlEvidenceEmitter<W> {
    /// The caller supplies an exclusive output. It is not protected cleanup storage.
    pub fn new(writer: W) -> Self {
        Self {
            state: Mutex::new(OutputState {
                writer,
                identity: None,
                seen: BTreeSet::new(),
                summary: EmissionSummary {
                    records: 0,
                    bytes: 0,
                },
                failure: None,
                finished: false,
            }),
        }
    }
    /// Flushes the stream. The collector independently checks required native cases.
    pub fn finish(&self) -> Result<EmissionSummary, NativeFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        if state.summary.records == 0 {
            state.failure = Some(NativeFailure::InvalidRecord);
            return Err(NativeFailure::InvalidRecord);
        }
        if !state.finished && state.writer.flush().is_err() {
            state.failure = Some(NativeFailure::IoUnavailable);
            return Err(NativeFailure::IoUnavailable);
        }
        state.finished = true;
        Ok(state.summary)
    }
}
impl<W: Write + Send> NativeEvidenceEmitter for JsonlEvidenceEmitter<W> {
    fn emit(&self, record: &NativeEvidenceRecord) -> Result<(), NativeFailure> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| NativeFailure::IoUnavailable)?;
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        let result = (|| {
            if state.finished {
                return Err(NativeFailure::IoUnavailable);
            }
            let identity = (record.identity, record.host);
            if state.identity.is_some_and(|old| old != identity) {
                return Err(NativeFailure::IdentityMismatch);
            }
            let (job, case, _) = record.fields();
            let key = (job, case, record.context);
            if state.seen.contains(&key) {
                return Err(NativeFailure::InvalidRecord);
            }
            let bytes = record.encode_jsonl(state.summary.records + 1)?;
            if state.summary.bytes + bytes.len() > RUN_BYTES {
                return Err(NativeFailure::LimitExceeded);
            }
            state
                .writer
                .write_all(&bytes)
                .map_err(|_| NativeFailure::IoUnavailable)?;
            state.identity = Some(identity);
            state.seen.insert(key);
            state.summary.records += 1;
            state.summary.bytes += bytes.len();
            Ok(())
        })();
        if let Err(failure) = result {
            state.failure = Some(failure);
        }
        result
    }
}
/// Explicitly unavailable composition, never a successful output sink.
#[derive(Default)]
pub struct UnavailableNativeEvidenceEmitter;
impl NativeEvidenceEmitter for UnavailableNativeEvidenceEmitter {
    fn emit(&self, _record: &NativeEvidenceRecord) -> Result<(), NativeFailure> {
        Err(NativeFailure::NativeUnavailable)
    }
}

pub(crate) fn failure_name(failure: NativeFailure) -> &'static str {
    use NativeFailure::*;
    match failure {
        UnsupportedHost => "unsupported-host",
        NativeUnavailable => "native-unavailable",
        CustodyUnavailable => "custody-unavailable",
        AccountIdentityUnavailable => "account-identity-unavailable",
        RegistrationMissing => "registration-missing",
        StorageLocked => "storage-locked",
        StorageUnavailable => "storage-unavailable",
        UnsafeNativeState => "unsafe-native-state",
        InvalidRecord => "invalid-record",
        UnsupportedSchema => "unsupported-schema",
        LegacyRegistrationRequiresMigration => "legacy-registration-requires-migration",
        IdentityMismatch => "identity-mismatch",
        GenerationMismatch => "generation-mismatch",
        StaleAssessment => "stale-assessment",
        ConfirmationRequired => "confirmation-required",
        AuthorizationDenied => "authorization-denied",
        AuthorizationCancelled => "authorization-cancelled",
        AuthorizationUnavailable => "authorization-unavailable",
        InstallationUnavailable => "installation-unavailable",
        ArtifactRejected => "artifact-rejected",
        CredentialUnavailable => "credential-unavailable",
        CredentialPolicyRejected => "credential-policy-rejected",
        InteractionRequired => "interaction-required",
        SupervisorUnavailable => "supervisor-unavailable",
        BackgroundItemsSuppressed => "background-items-suppressed",
        ProcessIdentityMismatch => "process-identity-mismatch",
        PrivilegeDropUnproven => "privilege-drop-unproven",
        ExecutionBindingUnproven => "execution-binding-unproven",
        TimedOut => "timed-out",
        LimitExceeded => "limit-exceeded",
        OutcomePending => "outcome-pending",
        Indeterminate => "indeterminate",
        CleanupUnresolved => "cleanup-unresolved",
        IoUnavailable => "io-unavailable",
        LinuxPasswordlessSudo => "linux-passwordless-sudo",
        LinuxDockerMembership => "linux-docker-membership",
        LinuxLxdMembership => "linux-lxd-membership",
        LinuxContainerBrokerAccess => "linux-container-broker-access",
        LinuxRawDiskAccess => "linux-raw-disk-access",
        LinuxDelegatedDacAuthority => "linux-delegated-dac-authority",
        LinuxDelegatedTracing => "linux-delegated-tracing",
        LinuxDelegatedProfiling => "linux-delegated-profiling",
        LinuxSystemWideProfiling => "linux-system-wide-profiling",
        LinuxUnsupportedAuthorizationPolicy => "linux-unsupported-authorization-policy",
        LinuxUnsupportedPrivilegedBroker => "linux-unsupported-privileged-broker",
        LinuxPolicyCheckUnavailable => "linux-policy-check-unavailable",
        LinuxPriorBypassHistoryUnproven => "linux-prior-bypass-history-unproven",
        LinuxPublicationUnavailable => "linux-publication-unavailable",
        LinuxMountWitnessUnavailable => "linux-mount-witness-unavailable",
        LinuxUnsafeDumpableTransition => "linux-unsafe-dumpable-transition",
        BootstrapTrustUnavailable => "bootstrap-trust-unavailable",
    }
}
