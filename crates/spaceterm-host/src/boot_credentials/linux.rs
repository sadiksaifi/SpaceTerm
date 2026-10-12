//! Linux credential output framing. Production tool execution remains gated by native custody.

use super::{
    BootCredentialAdapter, CredentialPolicyFacts, CredentialReleasePermit, LockedSecret32,
};
use crate::native_deployment::{contracts::NativeFailure, profile::BootCredentialBinding};
use std::{
    fs::File,
    os::fd::{AsRawFd, OwnedFd},
    time::Instant,
};

/// Borrowed from a genuine native release lease. Its issuer must retain the
/// authenticated downstream-patched tool and complete protected closure, the
/// immutable fixed ciphertext, and admitted host-key/TPM storage. Protection
/// must exclude account writes and replacement races throughout descriptor-exec.
pub(crate) struct RetainedCredentialExecution<'a> {
    pub(crate) tool: &'a File,
    pub(crate) ciphertext: &'a File,
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
#[path = "../../tests/native_handoff/credential_release_peer.rs"]
pub mod test_support;

/// Fixed packaged systemd-creds adapter. No tool, ciphertext, environment or account selector input.
#[derive(Default)]
pub struct SystemdBootCredentials;
impl BootCredentialAdapter for SystemdBootCredentials {
    fn inspect_policy(
        &self,
        binding: &BootCredentialBinding,
    ) -> Result<CredentialPolicyFacts, NativeFailure> {
        binding.linux_mode()?;
        // Do not open a host key, initialize one, or execute a tool without an independently
        // admitted closure and account-incarnation provider. Neither exists in the base.
        Err(NativeFailure::CustodyUnavailable)
    }
    fn retrieve_for_startup(
        &self,
        permit: &CredentialReleasePermit,
        destination: &mut LockedSecret32,
    ) -> Result<(), NativeFailure> {
        destination.wipe();
        crate::linux_launcher::inspect_linux_credential_policy_bound(permit)?;
        crate::linux_launcher::credential_tool::retrieve(
            permit,
            permit.linux_execution()?,
            destination,
        )
    }
}

/// Builds the only permitted upstream credential name from nonsecret binding facts.
pub fn credential_name(binding: &BootCredentialBinding) -> Result<String, NativeFailure> {
    binding.linux_mode()?;
    Ok(format!(
        "spaceterm.boot.{}",
        crate::native_deployment::bindings::hex(&binding.sha256()?)
    ))
}

/// Recognizes exactly 32 bytes followed by EOF on an owned anonymous tool-output pipe.
/// This does not execute a tool, admit custody, prove exit/reaping, or authorize delivery.
/// A single absolute deadline bounds fragmentation; partial output never resets it.
pub fn recognize_credential_tool_output(
    output: &OwnedFd,
    deadline: Instant,
) -> Result<LockedSecret32, NativeFailure> {
    let deadline = deadline.min(Instant::now() + std::time::Duration::from_secs(2));
    let fd = output.as_raw_fd();
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes only the provided native stat buffer.
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } != 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    // SAFETY: successful fstat initialized metadata.
    if unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(NativeFailure::UnsafeNativeState);
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs writes only the provided native filesystem observation.
    if unsafe { libc::fstatfs(fd, filesystem.as_mut_ptr()) } != 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    // SAFETY: successful fstatfs initialized filesystem. Linux anonymous pipes use pipefs.
    if unsafe { filesystem.assume_init() }.f_type != 0x5049_5045 {
        return Err(NativeFailure::UnsafeNativeState);
    }
    // SAFETY: F_GETFD observes only the owned descriptor's inheritance flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    if flags & libc::FD_CLOEXEC == 0 {
        return Err(NativeFailure::UnsafeNativeState);
    }
    // OwnedFd does not exclude duplicated readers. Readiness can disappear before read,
    // so the native output endpoint must already be nonblocking under retained ownership.
    // SAFETY: F_GETFL observes only the owned descriptor's file status flags.
    let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if status < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    if status & libc::O_NONBLOCK == 0 || status & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(NativeFailure::UnsafeNativeState);
    }
    let secret = LockedSecret32::new()?;
    let mut count = 0;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(NativeFailure::TimedOut)?;
        if remaining.is_zero() {
            return Err(NativeFailure::TimedOut);
        }
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll receives one initialized owned-descriptor observation.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(NativeFailure::IoUnavailable);
        }
        if ready == 0 {
            continue;
        }
        if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        let mut probe = 0u8;
        let (pointer, length) = if count < 32 {
            // SAFETY: remaining buffer is within this uniquely owned locked mapping.
            (unsafe { secret.allocation.as_ptr().add(count) }, 32 - count)
        } else {
            (&mut probe as *mut u8, 1)
        };
        // SAFETY: native read is bounded to the owned secret region or one-byte probe.
        let read = unsafe { libc::read(fd, pointer.cast(), length) };
        if count == 32 {
            // Wipe the oversize probe on every path, including a native failure.
            unsafe {
                std::ptr::write_volatile(&mut probe, 0);
            }
        }
        if read < 0 {
            match std::io::Error::last_os_error().kind() {
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock => continue,
                _ => return Err(NativeFailure::IoUnavailable),
            }
        }
        if Instant::now() >= deadline {
            return Err(NativeFailure::TimedOut);
        }
        if read == 0 {
            return if count == 32 {
                Ok(secret)
            } else {
                Err(NativeFailure::CredentialUnavailable)
            };
        }
        if count == 32 {
            return Err(NativeFailure::CredentialPolicyRejected);
        }
        count += read as usize;
    }
}
