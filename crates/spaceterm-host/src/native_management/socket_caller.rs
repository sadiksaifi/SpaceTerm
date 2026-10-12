//! Kernel-authenticated broker stdin. The challenge excludes queued messages from retired senders.

use super::caller::NativeCallerAuthentication;
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{controller::AuthorizationRoute, helper_lifetime::HelperDeadline};
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::Arc,
};

/// Retained actual stream writer. This is caller proof, never a management grant.
#[derive(Clone)]
pub struct BrokerCaller(Arc<Sender>);
impl std::fmt::Debug for BrokerCaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrokerCaller(..)")
    }
}
/// OS principal provider for the fixed sudo and pkexec stdin routes.
pub struct BrokerCallerPeer {
    caller: BrokerCaller,
}
impl BrokerCallerPeer {
    pub(crate) fn new(sender: Sender) -> Self {
        Self {
            caller: BrokerCaller(Arc::new(sender)),
        }
    }
    /// Original OS observation for protected assessment enrollment. It grants no right.
    pub fn caller(&self) -> &BrokerCaller {
        &self.caller
    }
    /// Exact native writer PID. A PID alone cannot construct this retained caller.
    pub fn observed_pid(&self) -> u32 {
        self.caller.0.pid()
    }
}
impl NativeCallerAuthentication for BrokerCallerPeer {
    type Caller = BrokerCaller;
    fn bind(
        &self,
        _: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<BrokerCaller, NativeFailure> {
        deadline.check()?;
        // SAFETY: native UID observations take no pointers. Only the privileged fixed helper binds a route.
        if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        self.recheck(&self.caller, deadline)?;
        Ok(self.caller.clone())
    }
    fn uid(&self, caller: &BrokerCaller) -> Result<u32, NativeFailure> {
        Ok(caller.0.uid())
    }
    fn recheck(
        &self,
        caller: &BrokerCaller,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        deadline.check()?;
        caller.0.recheck()?;
        deadline.check()
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct Sender {
    credentials: libc::ucred,
    process: OwnedFd,
}
#[cfg(target_os = "linux")]
impl Sender {
    pub(crate) fn capture(credentials: libc::ucred, _: &OwnedFd) -> Result<Self, NativeFailure> {
        if credentials.pid <= 0 || matches!(credentials.uid, 0 | u32::MAX) {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        // SAFETY: pidfd_open takes scalar arguments and returns a new owned descriptor. It never signals the process.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, credentials.pid, 0) };
        if fd < 0 {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        // SAFETY: the successful syscall transferred one descriptor to this owner.
        let sender = Self {
            credentials,
            process: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        };
        sender.recheck()?;
        Ok(sender)
    }
    pub(crate) fn matches(&self, credentials: &libc::ucred) -> bool {
        self.credentials.pid == credentials.pid
            && self.credentials.uid == credentials.uid
            && self.credentials.gid == credentials.gid
    }
    fn pid(&self) -> u32 {
        self.credentials.pid as u32
    }
    pub(crate) fn uid(&self) -> u32 {
        self.credentials.uid
    }
    pub(crate) fn recheck(&self) -> Result<(), NativeFailure> {
        let mut status = libc::pollfd {
            fd: self.process.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll inspects the retained original pidfd with no blocking or pointer escape.
        if unsafe { libc::poll(&mut status, 1, 0) } != 0 {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct Sender {
    audit: [u32; 8],
    start: (u64, u64),
    channel: OwnedFd,
}
#[cfg(target_os = "macos")]
impl Sender {
    pub(crate) fn capture(_: (), channel: &OwnedFd) -> Result<Self, NativeFailure> {
        let audit = audit_token(channel)?;
        if matches!(audit[1], 0 | u32::MAX)
            || audit[5] == 0
            || audit[5] > i32::MAX as u32
            || audit[7] == 0
        {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        let start = process_start(audit[5], audit[1])?;
        // SAFETY: duplicating an owned descriptor creates a new CLOEXEC owner without changing the stream.
        let fd = unsafe { libc::fcntl(channel.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(NativeFailure::IoUnavailable);
        }
        // SAFETY: fd is a newly allocated descriptor owned by this sender.
        let sender = Self {
            audit,
            start,
            channel: unsafe { OwnedFd::from_raw_fd(fd) },
        };
        sender.recheck()?;
        Ok(sender)
    }
    pub(crate) fn matches(&self, _: &()) -> bool {
        true
    }
    fn pid(&self) -> u32 {
        self.audit[5]
    }
    pub(crate) fn uid(&self) -> u32 {
        self.audit[1]
    }
    pub(crate) fn recheck(&self) -> Result<(), NativeFailure> {
        if audit_token(&self.channel).map_err(|_| NativeFailure::ProcessIdentityMismatch)?
            != self.audit
            || process_start(self.pid(), self.uid())? != self.start
        {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        Ok(())
    }
}
#[cfg(target_os = "macos")]
fn audit_token(channel: &OwnedFd) -> Result<[u32; 8], NativeFailure> {
    let mut token = [0u32; 8];
    let mut length = std::mem::size_of_val(&token) as libc::socklen_t;
    // SAFETY: getsockopt initializes only the exact live audit-token buffer and length.
    if unsafe {
        libc::getsockopt(
            channel.as_raw_fd(),
            0,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of_val(&token)
    {
        return Err(NativeFailure::AuthorizationUnavailable);
    }
    Ok(token)
}
#[cfg(target_os = "macos")]
fn process_start(pid: u32, uid: u32) -> Result<(u64, u64), NativeFailure> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: proc_pidinfo initializes the bounded bsdinfo buffer only on its exact successful length.
    let length = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if length as usize != std::mem::size_of::<libc::proc_bsdinfo>() {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    // SAFETY: the exact successful return initialized the entire structure.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_uid != uid || info.pbi_status == libc::SZOMB {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    Ok((info.pbi_start_tvsec, info.pbi_start_tvusec))
}
