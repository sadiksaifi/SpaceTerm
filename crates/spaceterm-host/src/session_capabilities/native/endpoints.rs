use crate::session_capabilities::{
    CapabilityFailure, GuiSessionObserver, SessionAdapterBroker, SessionAdapterLease,
    SessionPurpose,
};
use std::{
    ffi::CStr,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
};

/// Account-directory lookup is the only location input. No environment is read.
pub trait NativeSessionLocations: Send + Sync {
    /// Observe the account database's home, not HOME or GNUPGHOME.
    fn home(&self, uid: u32) -> Result<PathBuf, CapabilityFailure>;
    /// Native account runtime root, never XDG_RUNTIME_DIR from a client.
    fn runtime(&self, uid: u32) -> Result<Option<PathBuf>, CapabilityFailure>;
}
/// Native account database and fixed runtime-root discovery.
#[derive(Clone, Copy, Default)]
pub struct SystemSessionLocations;
impl NativeSessionLocations for SystemSessionLocations {
    fn home(&self, uid: u32) -> Result<PathBuf, CapabilityFailure> {
        let mut buffer = vec![0_u8; 16384];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        if unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        } != 0
            || result.is_null()
        {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_uid != uid || entry.pw_dir.is_null() {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        let home = unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes();
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(home));
        if !path.is_absolute() || home.len() > 4096 {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        Ok(path)
    }
    fn runtime(&self, uid: u32) -> Result<Option<PathBuf>, CapabilityFailure> {
        #[cfg(target_os = "linux")]
        {
            Ok(Some(PathBuf::from(format!("/run/user/{uid}"))))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = uid;
            Ok(None)
        }
    }
}
/// Narrow native endpoints remain account/session/purpose/expiry bound throughout
/// each operation. Paths and protocol output are never diagnostic metadata.
pub struct SystemSessionConnector<L = SystemSessionLocations> {
    uid: u32,
    account: [u8; 32],
    locations: L,
}
impl SystemSessionConnector {
    /// Compose native discovery for the original account binding.
    pub fn new(account: &crate::native_deployment::account::AccountBinding) -> Self {
        Self {
            uid: account.key().account_uid,
            account: account.sha256(),
            locations: SystemSessionLocations,
        }
    }
}
impl<L: NativeSessionLocations> SystemSessionConnector<L> {
    /// Inject only account-directory observation at construction.
    pub fn with_locations(
        account: &crate::native_deployment::account::AccountBinding,
        locations: L,
    ) -> Self {
        Self {
            uid: account.key().account_uid,
            account: account.sha256(),
            locations,
        }
    }
    /// Revalidate before connecting, before the operation, and at completion.
    pub fn with_connection<O: GuiSessionObserver, R>(
        &self,
        broker: &SessionAdapterBroker<O>,
        lease: &SessionAdapterLease,
        purpose: SessionPurpose,
        now: impl Fn() -> u64,
        operation: impl FnOnce(&mut UnixStream) -> Result<R, CapabilityFailure>,
    ) -> Result<R, CapabilityFailure> {
        if lease.facts.account_binding != self.account {
            return Err(CapabilityFailure::StaleAssessment);
        }
        broker.revalidate(lease, purpose, now())?;
        let mut candidates = Vec::new();
        match purpose {
            SessionPurpose::GpgAgent => {
                if let Some(runtime) = self.locations.runtime(self.uid)? {
                    candidates.push(runtime.join("gnupg/S.gpg-agent"));
                }
                candidates.push(self.locations.home(self.uid)?.join(".gnupg/S.gpg-agent"));
            }
            SessionPurpose::UserBus | SessionPurpose::GraphicalPortal => {
                let runtime = self
                    .locations
                    .runtime(self.uid)?
                    .ok_or(CapabilityFailure::NativeUnavailable)?;
                candidates.push(runtime.join("bus"));
            }
            _ => return Err(CapabilityFailure::NativeUnavailable),
        }
        let mut connected = None;
        for path in candidates {
            if !private_socket(&path, self.uid) {
                continue;
            }
            if let Ok(stream) = connect(&path, lease.expires_ns.saturating_sub(now()))
                && same_account(&stream, self.uid)
            {
                connected = Some(stream);
                break;
            }
        }
        let mut stream = connected.ok_or(CapabilityFailure::NativeUnavailable)?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .map_err(|_| CapabilityFailure::NativeUnavailable)?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(1)))
            .map_err(|_| CapabilityFailure::NativeUnavailable)?;
        broker.revalidate(lease, purpose, now())?;
        let result = operation(&mut stream);
        broker.revalidate(lease, purpose, now())?;
        result
    }
}
fn private_socket(path: &Path, uid: u32) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(directory) = std::fs::symlink_metadata(parent) else {
        return false;
    };
    let Ok(socket) = std::fs::symlink_metadata(path) else {
        return false;
    };
    directory.is_dir()
        && directory.uid() == uid
        && directory.mode() & 0o077 == 0
        && socket.file_type().is_socket()
        && socket.uid() == uid
}
fn same_account(stream: &UnixStream, uid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut peer_uid = u32::MAX;
        let mut peer_gid = u32::MAX;
        unsafe {
            libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) == 0
                && peer_uid == uid
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: u32::MAX,
            gid: u32::MAX,
        };
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut size,
            ) == 0
                && size as usize == std::mem::size_of::<libc::ucred>()
                && credentials.uid == uid
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (stream, uid);
        false
    }
}

fn connect(path: &Path, remaining_ns: u64) -> Result<UnixStream, CapabilityFailure> {
    use socket2::{Domain, SockAddr, Socket, Type};
    if remaining_ns == 0 {
        return Err(CapabilityFailure::StaleAssessment);
    }
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    socket
        .set_nonblocking(true)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    let address = SockAddr::unix(path).map_err(|_| CapabilityFailure::NativeUnavailable)?;
    if let Err(error) = socket.connect(&address) {
        if ![Some(libc::EINPROGRESS), Some(libc::EAGAIN)].contains(&error.raw_os_error()) {
            return Err(CapabilityFailure::NativeUnavailable);
        }
        let mut descriptor = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let milliseconds = remaining_ns.div_ceil(1_000_000).min(1000) as i32;
        if unsafe { libc::poll(&mut descriptor, 1, milliseconds) } != 1
            || descriptor.revents & libc::POLLOUT == 0
            || socket
                .take_error()
                .map_err(|_| CapabilityFailure::NativeUnavailable)?
                .is_some()
        {
            return Err(CapabilityFailure::NativeUnavailable);
        }
    }
    socket
        .set_nonblocking(false)
        .map_err(|_| CapabilityFailure::NativeUnavailable)?;
    Ok(socket.into())
}
