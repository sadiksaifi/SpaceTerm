//! Native receiver protection and cleanup before ordinary server admission.
//!
//! The transcript binds the anonymous channel to one startup. It is not native
//! authority. The authority adapter must retain and revalidate the registration,
//! installation and current Running intent after the exact retriever is reaped.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    os::fd::{FromRawFd, OwnedFd},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::{
    native_deployment::contracts::NativeFailure,
    startup_exchange::{
        ExchangeDeadline, FrameKind, PREFIX_BYTES, StartupTranscript,
        unix::{ExactChild, LockedMemory, PipeEndpoint, ReceiverPipes, ReceiverReceipt, receive},
    },
};

pub mod guard;
pub mod retriever;

/// The receiver's bounded self-report. Native post-exec proof checks consistency;
/// this report does not independently establish the account's directory groups.
pub struct ReceiverGroups(Vec<u32>);
impl ReceiverGroups {
    /// Validates a native getgroups observation without granting release authority.
    pub fn observed(groups: Vec<u32>) -> Result<Self, NativeFailure> {
        if groups.len() > 256 || groups.contains(&0) {
            return Err(NativeFailure::PrivilegeDropUnproven);
        }
        Ok(Self(groups))
    }

    /// Reads the dedicated inherited group channel and binds every startup byte.
    /// The caller's retained native execution lease authenticates ownership of
    /// this anonymous channel before using the report as proof consistency data.
    pub fn read_report(
        channel: OwnedFd,
        transcript: &StartupTranscript,
        deadline: ExchangeDeadline,
    ) -> Result<Self, NativeFailure> {
        let mut frame = LockedMemory::<1381>::allocate()?;
        PipeEndpoint::take(channel, false)?.read_frame(&mut frame, 1380, deadline)?;
        transcript.recognize_prefix(FrameKind::Ready, &frame.bytes()[..PREFIX_BYTES])?;
        let count = u32::from_be_bytes(
            frame.bytes()[352..356]
                .try_into()
                .map_err(|_| NativeFailure::InvalidRecord)?,
        ) as usize;
        if count > 256
            || frame.bytes()[356 + count * 4..1380]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(NativeFailure::InvalidRecord);
        }
        let groups = frame.bytes()[356..356 + count * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        Self::observed(groups)
    }

    /// Full checked getgroups observation, retained for native evidence.
    pub fn groups(&self) -> &[u32] {
        &self.0
    }
}
impl std::fmt::Debug for ReceiverGroups {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReceiverGroups")
            .field("count", &self.0.len())
            .finish()
    }
}

/// Native entry protection boundary. Test peers replace only kernel observations.
/// Production composition uses `SystemReceiverProtection`, never a declaration bit.
pub trait ReceiverProtection {
    /// Requires exact startup identity and protection before readiness or secrets.
    fn prepare(&self, transcript: &StartupTranscript) -> Result<ReceiverGroups, NativeFailure>;
}

/// Receiver-side registration and execution revalidation after cleanup.
/// Implementations retain native authority; a wire match is insufficient.
pub trait ReceiverAuthority {
    /// Rechecks the exact startup and Running generation before ordinary admission.
    fn revalidate(
        &self,
        transcript: &StartupTranscript,
        groups: &ReceiverGroups,
    ) -> Result<(), NativeFailure>;
}

/// Fixed fail-closed composition until the authority owner supplies retained
/// receiver-side registration and execution authority.
pub struct UnavailableReceiverAuthority;
impl ReceiverAuthority for UnavailableReceiverAuthority {
    fn revalidate(
        &self,
        _transcript: &StartupTranscript,
        _groups: &ReceiverGroups,
    ) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

/// Native post-exec receiver protection. Earliest native entry must also restore
/// descriptor inheritance flags before handing owned endpoints to this module.
pub struct SystemReceiverProtection;
impl ReceiverProtection for SystemReceiverProtection {
    fn prepare(&self, transcript: &StartupTranscript) -> Result<ReceiverGroups, NativeFailure> {
        if crate::startup_exchange::unix::process_identity(std::process::id())?
            != transcript.original_identity()
        {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: these native operations affect only this receiver process.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        #[cfg(target_os = "linux")]
        {
            // Resetting a dumpable-1 exec here cannot prove continuous custody.
            let initial = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
            if !matches!(initial, 0 | 2) {
                return Err(NativeFailure::ExecutionBindingUnproven);
            }
            if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
                || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
            {
                return Err(NativeFailure::ExecutionBindingUnproven);
            }
        }
        let mut groups = [0 as libc::gid_t; 256];
        let count = unsafe { libc::getgroups(groups.len() as i32, groups.as_mut_ptr()) };
        if count < 0 {
            return Err(NativeFailure::PrivilegeDropUnproven);
        }
        ReceiverGroups::observed(groups[..count as usize].to_vec())
    }
}

/// Owned exact-startup receiver input. All pipe endpoints have CLOEXEC restored
/// before this type can be constructed. It owns the sole retriever wait token.
pub struct NativeReceiver {
    transcript: StartupTranscript,
    pipes: ReceiverPipes,
    child: ExactChild,
    group_report: Option<PipeEndpoint>,
    deadline: ExchangeDeadline,
}
static INHERITED_CLAIMED: AtomicBool = AtomicBool::new(false);

impl NativeReceiver {
    /// Claims the fixed native channels and direct child across the launcher's
    /// same-PID exec. The native entry object must run before this bridge.
    ///
    /// # Safety
    /// Call only once at the earliest controlled entry, before any ordinary Rust
    /// descriptor owners or threads exist. Descriptors 3 through 7 (also 8 on
    /// Darwin) and the retriever's sole wait reservation must have been transferred
    /// by the trusted launcher. No descriptor or child may have another owner.
    pub unsafe fn from_inherited() -> Result<Self, NativeFailure> {
        if INHERITED_CLAIMED.swap(true, Ordering::AcqRel) {
            return Err(NativeFailure::UnsafeNativeState);
        }
        let deadline = ExchangeDeadline::start(crate::startup_exchange::MAX_EXCHANGE_WAIT)?;
        let last = if cfg!(target_os = "macos") { 8 } else { 7 };
        let mut invalid = false;
        for raw in 3..=last {
            let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
            if flags < 0
                || unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0
            {
                invalid = true;
            }
        }
        if invalid {
            for raw in 3..=last {
                unsafe { libc::close(raw) };
            }
            // Without a valid context channel, exact child cleanup cannot be
            // proven. The independent retriever guard still bounds its life.
            return Err(NativeFailure::CleanupUnresolved);
        }
        // SAFETY: the native launcher transferred these exclusive live endpoints.
        let context = unsafe { OwnedFd::from_raw_fd(3) };
        let ready = unsafe { OwnedFd::from_raw_fd(4) };
        let delivery = unsafe { OwnedFd::from_raw_fd(5) };
        let confirmation = unsafe { OwnedFd::from_raw_fd(6) };
        let acknowledgement = unsafe { OwnedFd::from_raw_fd(7) };
        #[cfg(target_os = "macos")]
        let groups = Some(unsafe { OwnedFd::from_raw_fd(8) });
        #[cfg(target_os = "linux")]
        let groups = None;
        let decoded = (|| {
            let mut frame = LockedMemory::<353>::allocate()?;
            PipeEndpoint::take(context, false)?.read_frame(&mut frame, PREFIX_BYTES, deadline)?;
            StartupTranscript::recognize_ready(&frame.bytes()[..PREFIX_BYTES])
        })();
        let transcript = decoded.map_err(|_| NativeFailure::CleanupUnresolved)?;
        // SAFETY: the trusted context identifies the child whose exclusive fork
        // reservation survived this service-tracked process's exec.
        let child = unsafe { ExactChild::inherit(transcript.retriever_identity()) }
            .map_err(|_| NativeFailure::CleanupUnresolved)?;
        let pipes = match ReceiverPipes::take(ready, delivery, confirmation, acknowledgement) {
            Ok(pipes) => pipes,
            Err(failure) => return Err(retire_failed_receiver(child, failure)),
        };
        Self::take(transcript, pipes, child, groups, deadline)
    }

    /// Takes receiver ownership from the native entry bridge. The optional Darwin
    /// report channel is created by the launcher, never opened through a pathname.
    pub fn take(
        transcript: StartupTranscript,
        pipes: ReceiverPipes,
        child: ExactChild,
        group_report: Option<OwnedFd>,
        deadline: ExchangeDeadline,
    ) -> Result<Self, NativeFailure> {
        let report = group_report
            .map(|fd| PipeEndpoint::take(fd, true))
            .transpose();
        match report {
            Ok(group_report) => Ok(Self {
                transcript,
                pipes,
                child,
                group_report,
                deadline,
            }),
            Err(failure) => {
                drop(pipes);
                Err(retire_failed_receiver(child, failure))
            }
        }
    }

    /// Protects memory, completes STN2, closes all channels, retires the exact
    /// retriever and revalidates native authority. No ordinary effects run here.
    pub fn complete(
        self,
        protection: &dyn ReceiverProtection,
        authority: &dyn ReceiverAuthority,
    ) -> Result<AdmittedNativeStartup, NativeFailure> {
        let Self {
            transcript,
            pipes,
            child,
            mut group_report,
            deadline,
        } = self;
        let protected = (|| {
            deadline.remaining()?;
            #[cfg(target_os = "macos")]
            if group_report.is_none() {
                return Err(NativeFailure::ExecutionBindingUnproven);
            }
            let groups = protection.prepare(&transcript)?;
            if let Some(report) = group_report.as_mut() {
                let mut frame = LockedMemory::<1380>::allocate()?;
                let length = PREFIX_BYTES + 4 + groups.groups().len() * 4;
                frame.bytes_mut()[..PREFIX_BYTES]
                    .copy_from_slice(&transcript.prefix(FrameKind::Ready));
                frame.bytes_mut()[PREFIX_BYTES..PREFIX_BYTES + 4]
                    .copy_from_slice(&(groups.groups().len() as u32).to_be_bytes());
                for (chunk, group) in frame.bytes_mut()[PREFIX_BYTES + 4..length]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(groups.groups())
                {
                    chunk.copy_from_slice(&group.to_be_bytes());
                }
                report.write_frame(frame.bytes(), deadline)?;
            }
            Ok(groups)
        })();
        drop(group_report);
        let groups = match protected {
            Ok(groups) => groups,
            Err(failure) => {
                drop(pipes);
                return Err(retire_failed_receiver(child, failure));
            }
        };
        let receipt = receive(&transcript, pipes, child, deadline)?;
        deadline.remaining()?;
        authority.revalidate(&transcript, &groups)?;
        deadline.remaining()?;
        Ok(AdmittedNativeStartup {
            receipt,
            groups,
            transcript,
        })
    }
}

fn retire_failed_receiver(mut child: ExactChild, failure: NativeFailure) -> NativeFailure {
    let Ok(deadline) = ExchangeDeadline::start(Duration::from_millis(250)) else {
        return NativeFailure::CleanupUnresolved;
    };
    // Setup failures close the same guarded exchange as receive. Exact child
    // exit alone cannot override an unresolved tool or guard retirement.
    match child.reap_retriever(deadline) {
        Ok(_) => failure,
        Err(_) => NativeFailure::CleanupUnresolved,
    }
}

/// Receipt available only after native revalidation and proven exchange cleanup.
/// Holds the secret in its locked mapping for the server's credential consumer.
pub struct AdmittedNativeStartup {
    receipt: ReceiverReceipt,
    groups: ReceiverGroups,
    transcript: StartupTranscript,
}
impl AdmittedNativeStartup {
    /// Borrows the credential without copying it into pageable storage.
    pub fn secret(&self) -> &[u8; 32] {
        self.receipt.secret()
    }

    /// Exact admitted startup comparison transcript for runtime and evidence binding.
    pub fn transcript(&self) -> &StartupTranscript {
        &self.transcript
    }

    /// Checked complete receiver getgroups report for the evidence consumer.
    pub fn receiver_groups(&self) -> &ReceiverGroups {
        &self.groups
    }
}
