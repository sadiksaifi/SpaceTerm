use serde::{Deserialize, Serialize};
use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    identity::ServerId,
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::SessionCleanupObservation,
};

use super::{RegisteredServer, ServiceFailure, ServiceFuture, SupervisorState};

/// Exact content-free runtime facts supplied by the owning runtime, not a PID scan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAssessment {
    /// The registered server whose work was observed.
    pub server: ServerId,
    /// The actual runtime epoch; absent only with trusted absence evidence.
    pub epoch: Option<ServerEpoch>,
    /// The runtime's comparison stamp for its whole affected work scope.
    pub generation: u64,
    /// The exact old Session generations, in strictly increasing order.
    pub sessions: Vec<SessionKey>,
    /// An opaque runtime-owned comparison value, never ordinary wire authority.
    /// The runtime never reuses it for another assessment or reservation owner.
    pub token: [u8; 32],
}
impl RuntimeAssessment {
    pub(super) fn validate(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if self.server.bytes() != registration.server_pin().identity
            || self.generation == 0
            || self.token == [0; 32]
            || self.sessions.len() > 64
            || self.epoch.is_some_and(|epoch| epoch.get() == 0)
            || self.sessions.iter().any(|session| {
                Some(session.epoch) != self.epoch
                    || session.generation.get() == 0
                    || session.pane.server() != self.server
            })
            || !self.sessions.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(ServiceFailure::InvalidRecord);
        }
        Ok(())
    }
}

/// Native manager operations selected only from the retained account registration.
/// Implementations own and reap their utilities even when an observer disappears.
/// Stop never force-kills a draining runtime or its started Git work.
pub trait ServiceManager: Send + Sync {
    /// Observes the fixed account/build service without authorizing or starting it.
    fn status<'a>(
        &'a self,
        registration: Option<&'a RegisteredServer>,
    ) -> ServiceFuture<'a, SupervisorState>;
    /// Observes only this fixed service's native process scope. Linux includes
    /// the unit cgroup and MainPID; macOS includes the LaunchAgent's PID.
    /// Runtime-owned Session custody is proved separately. Unsupported providers
    /// fail closed rather than substituting an account-wide process scan.
    fn scoped_processes<'a>(
        &'a self,
        _registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, Vec<u32>> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    /// Observes independently resolved Linux linger; macOS returns None.
    fn linger(&self) -> ServiceFuture<'_, Option<super::LingerState>>;
    /// Publishes fixed supervision content for the exact approved registration.
    /// Stopped suppression must not require an available approved executable.
    fn configure<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()>;
    /// Requests idempotent startup, never replacement of a running process.
    fn start<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()>;
    /// Requests graceful Stop after durable Stopped or a retained restart/update barrier.
    fn stop<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()>;
    /// Removes only this service's configuration and verified installed code, not data or linger.
    fn uninstall<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()>;
    /// Validates the exact approved code and its implemented storage compatibility.
    fn verify_installation(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure>;
}

/// Runtime-owned assessment, graceful retirement and trusted process-scope evidence.
/// A new empty registry or a dead main PID is not absence evidence.
pub trait ServiceProcesses: Send + Sync {
    /// Obtains an exact all-client assessment from the runtime or trusted absent scope.
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment>;
    /// Atomically revalidates and reserves admission against the original scope.
    /// The runtime owns this barrier until original quiescence or validated
    /// non-admission. It does not retire Sessions or refresh assessment expiry.
    /// A successfully aborted token cannot reserve admissions again.
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()>;
    /// Releases only the barrier owned by this exact original runtime assessment.
    /// The host calls this under its mutation lease after proving durable non-admission.
    /// It is idempotent when that barrier is absent and rejects any replacement owner.
    /// Success retires the token, so delayed callbacks cannot release a later barrier.
    fn abort_reservation<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()>;
    /// Releases only the exact original paired-origin request. The production
    /// native adapter requires signed non-admission or exact old-instance absence.
    fn abort_native_origin<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        _request: NativeManagementRequestRef,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        self.abort_reservation(registration, assessment)
    }
    /// Releases an old unaccepted reservation only after exact native-instance
    /// absence. This cannot claim retirement or grant startup authority.
    fn abort_reservation_with_absence<'a>(
        &'a self,
        _registration: &'a RegisteredServer,
        _assessment: &'a RuntimeAssessment,
        _absence: &'a super::runtime_inventory::NativeInstanceAbsence,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    /// Atomically revalidates the original assessment and closes runtime admissions.
    /// Replays resolve this request's accepted retirement, never a replacement batch.
    /// Started writes settle before exact Sessions are retired and reaped.
    fn quiesce<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>>;
    /// Proves manager scope, descendant processes and the registered endpoint are absent.
    fn verify_absent<'a>(&'a self, registration: &'a RegisteredServer) -> ServiceFuture<'a, ()>;
    /// Reconciles only the exact old-epoch Sessions and first retirement reasons
    /// recovered from the validated durable store. Default adapters preserve the
    /// original cleanup interface and must return those exact reasons.
    fn reconcile_previous_instance<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            let sessions: Vec<_> = expected.iter().map(|value| value.session()).collect();
            let observations = self
                .previous_instance_cleanup(registration, epoch, &sessions)
                .await?;
            cleanup_matches(&sessions, &observations)?;
            if !expected
                .iter()
                .zip(&observations)
                .all(|(expected, observed)| expected.first_reason() == observed.first_reason())
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            Ok(observations)
        })
    }
    /// Uses an opaque scope proof issued only after native-manager and endpoint
    /// absence have been observed. Ordinary requests cannot construct this proof.
    fn reconcile_previous_instance_with_absence<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        expected: &'a [spaceterm_core::retirement::RetiredSession],
        _absence: &'a super::runtime_inventory::NativeInstanceAbsence,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        self.reconcile_previous_instance(registration, epoch, expected)
    }
    /// Establishes exact previous-instance ownership before startup recovery.
    fn previous_instance_cleanup<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        epoch: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>>;
}

/// Cross-process assessment time tied to the current OS boot, not observer receipt time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTime {
    /// A content-free digest distinguishing OS boots.
    pub boot: [u8; 32],
    /// Milliseconds from the OS monotonic origin.
    pub millis: u64,
}

impl ServiceTime {
    /// Reads the shared native boot identity and cross-process monotonic origin.
    pub fn current() -> Result<Self, ServiceFailure> {
        NativeClock.now()
    }
}

/// Injected only at composition; test clocks never enter production constructors.
pub trait ServiceClock: Send + Sync {
    /// Reads the actual monotonic time and boot identity.
    fn now(&self) -> Result<ServiceTime, ServiceFailure>;
}

pub(super) struct NativeClock;
impl ServiceClock for NativeClock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        use sha2::{Digest as _, Sha256};
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime fills a live timespec without changing native state.
        #[cfg(target_os = "linux")]
        let clock = libc::CLOCK_BOOTTIME;
        #[cfg(target_os = "macos")]
        let clock = libc::CLOCK_MONOTONIC;
        if unsafe { libc::clock_gettime(clock, time.as_mut_ptr()) } != 0 {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        // SAFETY: successful clock_gettime initialized this value.
        let time = unsafe { time.assume_init() };
        let millis = u64::try_from(time.tv_sec)
            .ok()
            .and_then(|seconds| {
                seconds
                    .checked_mul(1000)?
                    .checked_add(u64::try_from(time.tv_nsec).ok()? / 1_000_000)
            })
            .ok_or(ServiceFailure::SupervisorUnavailable)?;
        #[cfg(target_os = "linux")]
        let boot = {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::fs::File::open("/proc/sys/kernel/random/boot_id")
                .and_then(|file| file.take(65).read_to_end(&mut bytes))
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            if bytes.len() != 37 {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            Sha256::digest(&bytes).into()
        };
        #[cfg(target_os = "macos")]
        let boot = {
            let mut value = std::mem::MaybeUninit::<libc::timeval>::uninit();
            let mut size = std::mem::size_of::<libc::timeval>();
            // SAFETY: fixed read-only sysctl writes at most the supplied timeval size.
            let result = unsafe {
                libc::sysctlbyname(
                    c"kern.boottime".as_ptr(),
                    value.as_mut_ptr().cast(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if result != 0 || size != std::mem::size_of::<libc::timeval>() {
                return Err(ServiceFailure::SupervisorUnavailable);
            }
            // SAFETY: successful sysctl initialized both timeval fields.
            let value = unsafe { value.assume_init() };
            let mut hash = Sha256::new();
            hash.update(value.tv_sec.to_be_bytes());
            hash.update(value.tv_usec.to_be_bytes());
            hash.finalize().into()
        };
        Ok(ServiceTime { boot, millis })
    }
}

pub(super) fn validate_previous_scope(
    server: ServerId,
    epoch: ServerEpoch,
    sessions: &[SessionKey],
) -> Result<(), ServiceFailure> {
    // Epoch zero represents a newly installed store before its first runtime.
    if (epoch.get() == 0 && !sessions.is_empty())
        || sessions.len() > 64
        || !sessions.windows(2).all(|pair| pair[0] < pair[1])
        || sessions.iter().any(|session| {
            session.pane.server() != server
                || session.epoch != epoch
                || session.generation.get() == 0
        })
    {
        return Err(ServiceFailure::InvalidRecord);
    }
    Ok(())
}

pub(super) fn cleanup_matches(
    sessions: &[SessionKey],
    observations: &[SessionCleanupObservation],
) -> Result<(), ServiceFailure> {
    use spaceterm_core::retirement::CleanupResult;
    if sessions.len() != observations.len()
        || !sessions
            .iter()
            .zip(observations)
            .all(|(session, observation)| {
                *session == observation.session()
                    && matches!(observation.result(), CleanupResult::Confirmed(_))
            })
    {
        return Err(ServiceFailure::CleanupUnresolved);
    }
    Ok(())
}
