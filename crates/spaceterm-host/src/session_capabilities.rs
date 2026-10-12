//! Content-free capability facts and GUI-session-scoped adapters.
//!
//! These records grant no execution, credential, filesystem, or GUI authority.
//! AppKit probes belong to the evidence fixture, not to the server. A production
//! session broker needs an independent native session observation provider.

use std::{fmt, time::Duration};

pub mod native;
pub use native::{NativeGuiSessionPeer, SystemGuiSessionObserver, SystemNativeGuiSessionPeer};

/// Content-free failures at the capability boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CapabilityFailure {
    /// An observation is malformed or inconsistent.
    #[error("invalid-record")]
    InvalidRecord,
    /// Native session observation is unavailable.
    #[error("native-unavailable")]
    NativeUnavailable,
    /// The original GUI session ended, changed, or expired.
    #[error("stale-assessment")]
    StaleAssessment,
    /// The original absolute deadline expired.
    #[error("timed-out")]
    TimedOut,
    /// A bounded input exceeds its limit.
    #[error("limit-exceeded")]
    LimitExceeded,
}

macro_rules! codes {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident = $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u32)]
        pub enum $name { $($(#[$variant_meta])* $variant = $value),+ }
        impl TryFrom<u32> for $name {
            type Error = CapabilityFailure;
            fn try_from(value: u32) -> Result<Self, Self::Error> {
                match value { $($value => Ok(Self::$variant),)+ _ => Err(CapabilityFailure::InvalidRecord) }
            }
        }
    };
}

codes! {
    /// Stable native evidence capability codes.
    Capability {
        /// Synthetic markers in protected folders.
        ProtectedFolders = 1,
        /// A synthetic marker on an explicitly selected volume.
        SelectedVolume = 2,
        /// Device activation with discarded samples.
        Microphone = 3,
        /// Capture of a synthetic window, with discarded pixels.
        ScreenRecording = 4,
        /// A synthetic companion control.
        Accessibility = 5,
        /// A synthetic clipboard round trip with safe restoration.
        Clipboard = 6,
        /// Receipt of a companion-owned URL.
        UrlOpening = 7,
        /// Receipt of a synthetic Apple Event.
        AppleEvents = 8,
        /// A scoped synthetic login-Keychain item.
        LoginKeychain = 9,
        /// A bounded SSH-agent protocol round trip, not signing authority.
        SshAgent = 10,
        /// A bounded GPG-agent protocol round trip, not signing authority.
        GpgAgent = 11,
        /// User-mediated sudo of the fixed /usr/bin/true executable.
        TouchIdSudo = 12,
        /// Normalized environment and interactive login-shell comparison.
        LoginShell = 13,
    }
}
codes! {
    /// Observed PTY parenting contexts, never merged by the matrix.
    ProbeContext {
        /// An interactive PTY whose ancestry was observed to reach the desktop.
        GuiParented = 1,
        /// An interactive PTY whose ancestry was observed to reach the server.
        DaemonParented = 2,
    }
}
codes! {
    /// Bounded probe results.
    ProbeOutcome {
        /// A synthetic effect and its required cleanup were observed.
        Passed = 1,
        /// Native permission was denied.
        Denied = 2,
        /// The user cancelled.
        Cancelled = 3,
        /// A native prerequisite is unavailable.
        Unavailable = 4,
        /// The original deadline expired.
        TimedOut = 5,
        /// No effect was attempted because a prerequisite was missing.
        Unrun = 6,
        /// The attempted effect failed.
        Failed = 7,
        /// The operation has not settled.
        Pending = 8,
        /// Its disposition is uncertain.
        Indeterminate = 9,
        /// Required cleanup could not be established.
        CleanupUnresolved = 10,
    }
}
codes! {
    /// Actual responsible-process attribution, not an ancestry assumption.
    ResponsibleProcess {
        /// The desktop client.
        Desktop = 1,
        /// The headless server.
        Server = 2,
        /// The probe executable.
        Probe = 3,
        /// The synthetic companion.
        Companion = 4,
        /// Another natively observed process.
        Other = 5,
        /// Native attribution could not be established.
        Unknown = 6,
    }
}
codes! {
    /// Preflight is recorded separately from the effect.
    PreflightClass {
        /// No preflight observation is available.
        Unknown = 0,
        /// The preflight accepted the capability.
        Allowed = 1,
        /// Native permission is denied.
        Denied = 2,
        /// No privacy decision exists.
        NotDetermined = 3,
        /// The native prerequisite is unavailable.
        Unavailable = 4,
    }
}
codes! {
    /// Content-free effect observations.
    EffectClass {
        /// No effect was observed.
        Unobserved = 0,
        /// The exact synthetic effect was observed.
        Verified = 1,
        /// Native permission denied the attempted effect.
        Denied = 2,
        /// The effect cannot be exercised here.
        Unavailable = 3,
        /// The attempted effect failed verification.
        Failed = 4,
        /// Execution completed but the claimed mechanism was not observed.
        CompletedUnattributed = 5,
    }
}
codes! {
    /// Cleanup has precedence over a successful effect.
    CleanupClass {
        /// This case created no resource requiring restoration.
        NotNeeded = 0,
        /// Required cleanup was independently checked.
        Verified = 1,
        /// Required cleanup could not be established.
        Unresolved = 2,
    }
}

/// An exact 32-byte, eight-u32 big-endian observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityObservation {
    capability: Capability,
    context: ProbeContext,
    outcome: ProbeOutcome,
    responsible: ResponsibleProcess,
    preflight: PreflightClass,
    effect: EffectClass,
    cleanup: CleanupClass,
}

impl CapabilityObservation {
    /// Recognize bounded codes only. Text, extensions, and unknown codes reject.
    pub fn decode(bytes: &[u8]) -> Result<Self, CapabilityFailure> {
        let bytes: &[u8; 32] = bytes
            .try_into()
            .map_err(|_| CapabilityFailure::InvalidRecord)?;
        let mut fields = [0; 8];
        for (field, chunk) in fields.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *field = u32::from_be_bytes(*chunk);
        }
        if fields[0] != 1 {
            return Err(CapabilityFailure::InvalidRecord);
        }
        let observation = Self {
            capability: fields[1].try_into()?,
            context: fields[2].try_into()?,
            outcome: fields[3].try_into()?,
            responsible: fields[4].try_into()?,
            preflight: fields[5].try_into()?,
            effect: fields[6].try_into()?,
            cleanup: fields[7].try_into()?,
        };
        if (observation.outcome == ProbeOutcome::Passed
            && (observation.preflight != PreflightClass::Allowed
                || observation.effect != EffectClass::Verified))
            || ((observation.cleanup == CleanupClass::Unresolved)
                != (observation.outcome == ProbeOutcome::CleanupUnresolved))
            || (observation.outcome == ProbeOutcome::Unrun
                && observation.effect != EffectClass::Unobserved)
        {
            return Err(CapabilityFailure::InvalidRecord);
        }
        Ok(observation)
    }

    /// Encode only the validated fixed record, never native output.
    pub fn encode(self) -> [u8; 32] {
        let fields = [
            1,
            self.capability as u32,
            self.context as u32,
            self.outcome as u32,
            self.responsible as u32,
            self.preflight as u32,
            self.effect as u32,
            self.cleanup as u32,
        ];
        let mut bytes = [0; 32];
        for (destination, field) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            destination.copy_from_slice(&field.to_be_bytes());
        }
        bytes
    }

    /// The tested capability.
    pub fn capability(self) -> Capability {
        self.capability
    }
    /// The observed parenting context.
    pub fn context(self) -> ProbeContext {
        self.context
    }
    /// The settled outcome.
    pub fn outcome(self) -> ProbeOutcome {
        self.outcome
    }
    /// Native process attribution, or Unknown.
    pub fn responsible_process(self) -> ResponsibleProcess {
        self.responsible
    }
    /// The independent preflight classification.
    pub fn preflight(self) -> PreflightClass {
        self.preflight
    }
    /// The observed synthetic effect.
    pub fn effect(self) -> EffectClass {
        self.effect
    }
    /// Required cleanup disposition.
    pub fn cleanup(self) -> CleanupClass {
        self.cleanup
    }
}

/// One run's separately retained GUI and daemon observations.
#[derive(Default)]
pub struct CapabilityMatrix {
    observations: [[Option<CapabilityObservation>; 13]; 2],
}

impl CapabilityMatrix {
    /// Retain one observation per capability and context. Duplicates reject.
    pub fn insert(&mut self, observation: CapabilityObservation) -> Result<(), CapabilityFailure> {
        let entry = &mut self.observations[observation.context as usize - 1]
            [observation.capability as usize - 1];
        if entry.is_some() {
            return Err(CapabilityFailure::InvalidRecord);
        }
        *entry = Some(observation);
        Ok(())
    }

    /// Read one context without falling back to another context.
    pub fn get(
        &self,
        capability: Capability,
        context: ProbeContext,
    ) -> Option<CapabilityObservation> {
        self.observations[context as usize - 1][capability as usize - 1]
    }

    /// Count actual records, including denied and unrun records.
    pub fn recorded_count(&self) -> usize {
        self.observations
            .iter()
            .flatten()
            .filter(|value| value.is_some())
            .count()
    }
}

/// One absolute monotonic deadline. Partial output cannot extend it.
#[derive(Clone, Copy, Debug)]
pub struct ProbeDeadline {
    deadline_ns: u64,
}

impl ProbeDeadline {
    /// Start an ordinary five-second probe.
    pub fn ordinary(now_ns: u64) -> Result<Self, CapabilityFailure> {
        Self::new(now_ns, 5_000_000_000)
    }
    /// Start an explicitly user-mediated sixty-second probe.
    pub fn user_mediated(now_ns: u64) -> Result<Self, CapabilityFailure> {
        Self::new(now_ns, 60_000_000_000)
    }
    fn new(now_ns: u64, budget_ns: u64) -> Result<Self, CapabilityFailure> {
        Ok(Self {
            deadline_ns: now_ns
                .checked_add(budget_ns)
                .ok_or(CapabilityFailure::LimitExceeded)?,
        })
    }
    /// Remaining time in the same monotonic clock domain.
    pub fn remaining(self, now_ns: u64) -> Result<Duration, CapabilityFailure> {
        let remaining = self
            .deadline_ns
            .checked_sub(now_ns)
            .filter(|value| *value != 0)
            .ok_or(CapabilityFailure::TimedOut)?;
        Ok(Duration::from_nanos(remaining))
    }
}

/// A narrow purpose that cannot select commands or import GUI environment values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPurpose {
    /// Reach the current account's SSH agent.
    SshAgent,
    /// Reach the current account's GPG agent.
    GpgAgent,
    /// Perform a synthetic clipboard operation.
    Clipboard,
    /// Open the fixed synthetic companion URL.
    CompanionUrl,
    /// Reach the current graphical session's account-scoped user D-Bus.
    UserBus,
    /// Reach graphical-session tools through the account's desktop portal bus.
    GraphicalPortal,
}

/// Native session comparison facts. Constructing facts creates no adapter lease.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct GuiSessionFacts {
    account_binding: [u8; 32],
    incarnation: [u8; 16],
}

impl GuiSessionFacts {
    /// Check nonzero account and current GUI-session incarnation bindings.
    pub fn new(
        account_binding: [u8; 32],
        incarnation: [u8; 16],
    ) -> Result<Self, CapabilityFailure> {
        if account_binding == [0; 32] || incarnation == [0; 16] {
            return Err(CapabilityFailure::InvalidRecord);
        }
        Ok(Self {
            account_binding,
            incarnation,
        })
    }
}
impl fmt::Debug for GuiSessionFacts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GuiSessionFacts([redacted])")
    }
}

/// Native observations must distinguish logout and replacement GUI sessions.
/// Environment variables, account names, and a UID alone are not that proof.
pub trait GuiSessionObserver: Send + Sync {
    /// Observe the current account binding and current GUI-session incarnation.
    fn observe(&self) -> Result<Option<GuiSessionFacts>, CapabilityFailure>;
}

/// A noncloneable, expiring narrow session lease, issued by its own broker.
pub struct SessionAdapterLease {
    facts: GuiSessionFacts,
    purpose: SessionPurpose,
    expires_ns: u64,
    broker: std::sync::Arc<()>,
}
impl fmt::Debug for SessionAdapterLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionAdapterLease([redacted])")
    }
}

/// Bind and recheck narrow adapters against the same native observation provider.
pub struct SessionAdapterBroker<T> {
    observer: T,
    identity: std::sync::Arc<()>,
}
impl<T: GuiSessionObserver> SessionAdapterBroker<T> {
    /// Compose with a native observer. Test observers belong to test composition.
    pub fn new(observer: T) -> Self {
        Self {
            observer,
            identity: std::sync::Arc::new(()),
        }
    }
    /// Issue a lease for at most thirty seconds, without importing GUI environment.
    pub fn bind(
        &self,
        purpose: SessionPurpose,
        now_ns: u64,
        lifetime: Duration,
    ) -> Result<SessionAdapterLease, CapabilityFailure> {
        if lifetime.is_zero() || lifetime > Duration::from_secs(30) {
            return Err(CapabilityFailure::LimitExceeded);
        }
        let expires_ns = now_ns
            .checked_add(lifetime.as_nanos() as u64)
            .ok_or(CapabilityFailure::LimitExceeded)?;
        let facts = self
            .observer
            .observe()?
            .ok_or(CapabilityFailure::NativeUnavailable)?;
        Ok(SessionAdapterLease {
            facts,
            purpose,
            expires_ns,
            broker: self.identity.clone(),
        })
    }
    /// Revalidate immediately before each effect and completion.
    /// A lease from another broker, another purpose, or an ended session rejects.
    pub fn revalidate(
        &self,
        lease: &SessionAdapterLease,
        purpose: SessionPurpose,
        now_ns: u64,
    ) -> Result<(), CapabilityFailure> {
        if !std::sync::Arc::ptr_eq(&self.identity, &lease.broker)
            || purpose != lease.purpose
            || now_ns >= lease.expires_ns
            || self.observer.observe()? != Some(lease.facts)
        {
            return Err(CapabilityFailure::StaleAssessment);
        }
        Ok(())
    }
}

/// Fail-closed placeholder until platform session-incarnation proof is integrated.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableGuiSessionObserver;
impl GuiSessionObserver for UnavailableGuiSessionObserver {
    fn observe(&self) -> Result<Option<GuiSessionFacts>, CapabilityFailure> {
        Err(CapabilityFailure::NativeUnavailable)
    }
}

/// Observed ordering of controlled login and interactive shell startup files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellStartupPhase {
    /// Shell-wide startup precedes both login and interactive configuration.
    Environment,
    /// Login profile precedes interactive configuration.
    LoginProfile,
    /// Interactive configuration runs before login completion.
    InteractiveRc,
    /// Login completion follows interactive configuration.
    LoginCompletion,
}

/// Controlled startup-order and environment counts, not an equality observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedShellObservation {
    phases: [ShellStartupPhase; 4],
    path_components: u16,
    duplicate_path_components: u16,
    locale_present: bool,
}
impl NormalizedShellObservation {
    /// Recognize the exact controlled interactive-login startup order.
    pub fn new(
        phases: [ShellStartupPhase; 4],
        path_components: u16,
        duplicate_path_components: u16,
        locale_present: bool,
    ) -> Result<Self, CapabilityFailure> {
        if phases
            != [
                ShellStartupPhase::Environment,
                ShellStartupPhase::LoginProfile,
                ShellStartupPhase::InteractiveRc,
                ShellStartupPhase::LoginCompletion,
            ]
            || path_components > 256
            || duplicate_path_components > path_components
        {
            return Err(CapabilityFailure::InvalidRecord);
        }
        Ok(Self {
            phases,
            path_components,
            duplicate_path_components,
            locale_present,
        })
    }
    /// Compare counts only. Equal counts never establish equal PATH or locale values.
    pub fn compare_counts(&self, other: &Self) -> NormalizedShellDifference {
        NormalizedShellDifference {
            path_component_count_changed: self.path_components != other.path_components,
            duplicate_component_count_changed: self.duplicate_path_components
                != other.duplicate_path_components,
            locale_presence_changed: self.locale_present != other.locale_present,
            startup_order_changed: self.phases != other.phases,
        }
    }
}

/// Closed environment-comparison facts for evidence records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizedShellDifference {
    /// PATH component counts differ; no components are disclosed.
    pub path_component_count_changed: bool,
    /// Duplicate component counts differ.
    pub duplicate_component_count_changed: bool,
    /// Locale availability differs; no locale value is disclosed.
    pub locale_presence_changed: bool,
    /// Controlled startup ordering differs.
    pub startup_order_changed: bool,
}

codes! {
    /// Difference observed by internally comparing retained PATH values.
    PathDifference {
        /// Exact values, including component order and duplicates, match.
        Equal = 1,
        /// One shell has no PATH value.
        PresenceChanged = 2,
        /// Component contents or multiplicities differ.
        ComponentsChanged = 3,
        /// The same component multiset has a different order.
        OrderChanged = 4
    }
}
codes! {
    /// Difference observed across the fixed locale-variable allowlist.
    LocaleDifference {
        /// Every variable's presence and value match.
        Equal = 1,
        /// At least one variable's presence differs.
        PresenceChanged = 2,
        /// Presence matches but at least one value differs.
        ValuesChanged = 3
    }
}

/// Content-free exact environment comparison from the evidence-only native adapter.
///
/// The adapter compares private values before encoding these classifications.
/// Counts, presence alone, and controlled startup files cannot construct equality.
/// This is an observation record, not GUI-session authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizedEnvironmentDifference {
    path: PathDifference,
    locale: LocaleDifference,
}
impl NormalizedEnvironmentDifference {
    /// Recognize the exact schema-1, GUI-versus-daemon, twenty-byte record.
    pub fn decode(bytes: &[u8]) -> Result<Self, CapabilityFailure> {
        let bytes: &[u8; 20] = bytes
            .try_into()
            .map_err(|_| CapabilityFailure::InvalidRecord)?;
        let mut fields = [0; 5];
        for (field, chunk) in fields.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *field = u32::from_be_bytes(*chunk);
        }
        if fields[0] != 1
            || fields[1] != ProbeContext::GuiParented as u32
            || fields[2] != ProbeContext::DaemonParented as u32
        {
            return Err(CapabilityFailure::InvalidRecord);
        }
        Ok(Self {
            path: PathDifference::try_from(fields[3])?,
            locale: LocaleDifference::try_from(fields[4])?,
        })
    }
    /// The comparison of PATH contents and ordering, without values or hashes.
    pub fn path(self) -> PathDifference {
        self.path
    }
    /// The comparison of locale values, without values or hashes.
    pub fn locale(self) -> LocaleDifference {
        self.locale
    }
}
