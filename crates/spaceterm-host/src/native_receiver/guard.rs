//! Independent native retriever lifetime. A blocked credential call cannot
//! outlive its guard, and an account receiver need not signal a root child.

use std::{io, os::fd::AsRawFd, sync::OnceLock, time::Duration};

use crate::{
    native_deployment::contracts::NativeFailure,
    startup_exchange::{
        ExchangeDeadline, MAX_EXCHANGE_WAIT,
        unix::{ExactChild, anonymous_pipe, process_identity},
    },
};

static ACTIVE_GUARD: OnceLock<(crate::startup::ProcessIdentity, ExchangeDeadline)> =
    OnceLock::new();

#[cfg(target_os = "linux")]
pub(crate) fn credential_deadline() -> Result<ExchangeDeadline, NativeFailure> {
    let (identity, deadline) = ACTIVE_GUARD
        .get()
        .ok_or(NativeFailure::CustodyUnavailable)?;
    deadline.remaining()?;
    if process_identity(identity.pid)? != *identity
        || unsafe { libc::getpgid(identity.pid as i32) } != unsafe { libc::getpid() }
    {
        return Err(NativeFailure::CleanupUnresolved);
    }
    Ok(*deadline)
}

/// Executes only in the one-shot retriever process. Successful retirement of the
/// independent guard precedes a zero exit. Failure leaves the guard to kill the
/// entire retriever process group, including any blocked credential tool.
///
/// # Safety
/// The caller must be the single-threaded newly forked retriever, before secret
/// access, with no other children or process-group members. The operation must
/// own and retire its tool children before returning success. This function
/// consumes the process and never returns to the launcher's ordinary code.
pub unsafe fn run_guarded_retriever(
    operation: impl FnOnce(ExchangeDeadline) -> Result<(), NativeFailure>,
) -> ! {
    let outcome = (|| {
        // SAFETY: this is the exclusive newly forked retriever process.
        if unsafe { libc::setpgid(0, 0) } != 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        #[cfg(target_os = "linux")]
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
            || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
        {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        let deadline = ExchangeDeadline::start(MAX_EXCHANGE_WAIT)?;
        let (read, write) = anonymous_pipe()?;
        #[cfg(target_os = "macos")]
        let descriptors = inherited_descriptors()?;
        let group = unsafe { libc::getpid() };
        let guard_pid = unsafe { libc::fork() };
        if guard_pid < 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
        if guard_pid == 0 {
            // No allocator, credentials, inherited authority or ordinary work is
            // reachable in the guard. Close every descriptor except its channel.
            let raw = read.as_raw_fd();
            if unsafe { libc::dup2(raw, 3) } < 0 {
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                    libc::_exit(33)
                }
            }
            #[cfg(target_os = "macos")]
            for fd in &descriptors {
                if *fd != 3 {
                    unsafe { libc::close(*fd) };
                }
            }
            #[cfg(target_os = "linux")]
            if unsafe { libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32) } != 0 {
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                    libc::_exit(33)
                }
            }
            for fd in 0..3 {
                unsafe { libc::close(fd) };
            }
            guard_loop(3, group, deadline);
        }
        // Once the guard exists, every failure leaves its group retirement
        // unproven. Preserve that uncertainty in the process exit classification,
        // including errors observing the guard or finishing a credential call.
        (|| {
            drop(read);
            let identity = process_identity(guard_pid as u32)?;
            // SAFETY: fork created this sole directly owned guard child; no other waiter exists.
            let mut guard = unsafe { ExactChild::inherit(identity) }?;
            if ACTIVE_GUARD.set((identity, deadline)).is_err() {
                return Err(NativeFailure::UnsafeNativeState);
            }
            if operation(deadline).is_err() {
                // EOF requests group kill. It does not prove scope retirement.
                drop(write);
                return Err(NativeFailure::CleanupUnresolved);
            }
            let command = b"R";
            deadline.remaining()?;
            if unsafe { libc::write(write.as_raw_fd(), command.as_ptr().cast(), 1) } != 1 {
                return Err(NativeFailure::CleanupUnresolved);
            }
            drop(write);
            match guard.reap(ExchangeDeadline::start(Duration::from_millis(100))?) {
                Ok(true) => Ok(()),
                _ => Err(NativeFailure::CleanupUnresolved),
            }
        })()
        .map_err(|_| NativeFailure::CleanupUnresolved)
    })();
    // No unwinding, stdout, logging or return to privileged launcher code.
    unsafe { libc::_exit(outcome.err().map_or(0, |failure| failure as i32)) }
}

fn guard_loop(fd: i32, group: i32, deadline: ExchangeDeadline) -> ! {
    while let Ok(remaining) = deadline.remaining() {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let milliseconds = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if ready < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        if ready <= 0 || poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }
        let mut command = [0u8; 2];
        let count = unsafe { libc::read(fd, command.as_mut_ptr().cast(), command.len()) };
        if count == 1 && command[0] == b'R' && deadline.remaining().is_ok() {
            unsafe { libc::_exit(0) }
        }
        break;
    }
    // The guard is itself still a member, reserving this PGID until group kill.
    // A malicious final-account process cannot join this root-owned group.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
        libc::_exit(33)
    }
}

#[cfg(target_os = "macos")]
fn inherited_descriptors() -> Result<Vec<i32>, NativeFailure> {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Descriptor {
        fd: i32,
        kind: u32,
    }
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            size: i32,
        ) -> i32;
    }
    let size = unsafe { proc_pidinfo(libc::getpid(), 1, 0, std::ptr::null_mut(), 0) };
    let entry = std::mem::size_of::<Descriptor>();
    if size <= 0 || size as usize > 131_072 || !(size as usize).is_multiple_of(entry) {
        return Err(NativeFailure::LimitExceeded);
    }
    let mut entries = vec![Descriptor { fd: -1, kind: 0 }; size as usize / entry];
    let actual = unsafe { proc_pidinfo(libc::getpid(), 1, 0, entries.as_mut_ptr().cast(), size) };
    if actual <= 0 || actual > size || !(actual as usize).is_multiple_of(entry) {
        return Err(NativeFailure::UnsafeNativeState);
    }
    entries.truncate(actual as usize / entry);
    if entries.iter().any(|entry| entry.fd < 0) {
        return Err(NativeFailure::UnsafeNativeState);
    }
    Ok(entries.into_iter().map(|entry| entry.fd).collect())
}
