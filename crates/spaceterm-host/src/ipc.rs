//! Private same-account Unix IPC with retained endpoint identity and bounded framing.
//!
//! Servers reclaim dead owned entries under the endpoint lease. Orderly close
//! removes only the registered socket identity after closing its listener. Clients
//! never remove entries. The retained owner-only directory, native peer account and
//! pinned protocol proof protect against foreign accounts and impostor listeners.
//! A same-account replacement between the final identity check and unlinkat remains
//! possible and is outside the owning-account threat model.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as NativeMutex, OnceLock};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::peer_credentials::same_account_peer;
use crate::transport::{
    AdmissionLimits, ConnectionState, MessageLimit, MessageTransport, PeerFacts, PendingConnection,
    SendGuard, TransportError, TransportFuture,
};

/// Host socket-address bound, leaving one byte for the NUL terminator.
pub const MAX_IPC_PATH_BYTES: usize = std::mem::size_of::<libc::sockaddr_un>()
    - std::mem::offset_of!(libc::sockaddr_un, sun_path)
    - 1;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn from_status(status: &libc::stat) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            device: status.st_dev as u64,
            #[cfg(target_os = "linux")]
            device: status.st_dev,
            inode: status.st_ino,
        }
    }
}

struct Directory {
    path: PathBuf,
    file: File,
    identity: Identity,
}

impl Directory {
    fn verify(&self) -> Result<(), TransportError> {
        let current =
            fs::symlink_metadata(&self.path).map_err(|_| TransportError::UnsafeEndpoint)?;
        let held = self
            .file
            .metadata()
            .map_err(|_| TransportError::UnsafeEndpoint)?;
        if !private_directory(&current)
            || !private_directory(&held)
            || Identity::from_metadata(&current) != self.identity
            || Identity::from_metadata(&held) != self.identity
        {
            return Err(TransportError::UnsafeEndpoint);
        }
        Ok(())
    }
}

fn private_directory(metadata: &fs::Metadata) -> bool {
    // SAFETY: geteuid has no preconditions.
    metadata.is_dir()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o7777 == 0o700
}

/// Retained authority for an existing owner-only `0700` IPC directory.
///
/// Directory creation and durable discovery belong to host composition. This adapter
/// does not repair permissions or follow a symlink at the directory entry. It verifies
/// the retained descriptor against the current path before and after socket operations.
#[derive(Clone)]
pub struct PrivateIpcDirectory(Arc<Directory>);

impl std::fmt::Debug for PrivateIpcDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateIpcDirectory(..)")
    }
}

impl PrivateIpcDirectory {
    /// Opens an existing private directory. Relative paths, controls, and `..` are rejected.
    pub fn open(path: &Path) -> Result<Self, TransportError> {
        validate_absolute_path(path, MAX_IPC_PATH_BYTES - 2)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| TransportError::UnsafeEndpoint)?;
        let metadata = file
            .metadata()
            .map_err(|_| TransportError::UnsafeEndpoint)?;
        if !private_directory(&metadata) {
            return Err(TransportError::UnsafeEndpoint);
        }
        let directory = Arc::new(Directory {
            path: path.to_owned(),
            file,
            identity: Identity::from_metadata(&metadata),
        });
        directory.verify()?;
        Ok(Self(directory))
    }

    /// Resolves a short literal socket entry without creating or replacing anything.
    pub fn endpoint(&self, name: &str) -> Result<IpcEndpoint, TransportError> {
        if name.is_empty()
            || name.len() > 16
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err(TransportError::InvalidConfiguration);
        }
        self.0.verify()?;
        let path = self.0.path.join(name);
        validate_absolute_path(&path, MAX_IPC_PATH_BYTES)?;
        Ok(IpcEndpoint {
            directory: Arc::clone(&self.0),
            name: CString::new(name).map_err(|_| TransportError::InvalidConfiguration)?,
            path,
        })
    }
}

/// A route in a retained private directory. It is not a Server identity or authentication proof.
#[derive(Clone)]
pub struct IpcEndpoint {
    directory: Arc<Directory>,
    name: CString,
    path: PathBuf,
}

impl std::fmt::Debug for IpcEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("IpcEndpoint(..)")
    }
}

impl IpcEndpoint {
    /// Supplies the route to native socket/SSH adapters. Do not include it in diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> Result<(), TransportError> {
        self.directory.verify()
    }

    pub(crate) fn ensure_absent(&self) -> Result<(), TransportError> {
        self.verify()?;
        match status_at(&self.directory.file, &self.name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(TransportError::UnsafeEndpoint),
        }
    }

    pub(crate) fn lease(&self) -> Result<EndpointLease, TransportError> {
        self.verify()?;
        // Closing any descriptor of a POSIX-locked inode releases this process's
        // locks. Reject local contenders before opening that inode, not afterwards.
        let local = LocalEndpointLease::acquire(self)?;
        let name = CString::new(format!(".l-{}", self.name.to_string_lossy()))
            .map_err(|_| TransportError::InvalidConfiguration)?;
        // SAFETY: the retained parent and literal name stay live. O_NOFOLLOW rejects
        // redirected entries; O_CLOEXEC excludes the descriptor after child exec.
        let descriptor = unsafe {
            libc::openat(
                self.directory.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(TransportError::UnsafeEndpoint);
        }
        // SAFETY: openat returned a new owned descriptor, transferred exactly once.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file
            .metadata()
            .map_err(|_| TransportError::UnsafeEndpoint)?;
        // SAFETY: geteuid has no preconditions.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(TransportError::UnsafeEndpoint);
        }
        let lock = libc::flock {
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        // SAFETY: fcntl reads this live lock record and descriptor, without blocking.
        // POSIX locks belong to the parent process, so an unrelated fork cannot
        // retain the lease while awaiting exec, unlike a flock/open-description lock.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) } != 0 {
            return Err(TransportError::UnsafeEndpoint);
        }
        let status =
            status_at(&self.directory.file, &name).map_err(|_| TransportError::UnsafeEndpoint)?;
        if Identity::from_status(&status) != Identity::from_metadata(&metadata) {
            return Err(TransportError::UnsafeEndpoint);
        }
        self.verify()?;
        Ok(EndpointLease {
            _file: file,
            _local: local,
        })
    }

    pub(crate) fn socket_identity(&self) -> Result<Option<Identity>, TransportError> {
        self.verify()?;
        let identity = match status_at(&self.directory.file, &self.name) {
            Ok(status) => Some(socket_identity(&status)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(TransportError::UnsafeEndpoint),
        };
        self.verify()?;
        Ok(identity)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn pin_socket(&self) -> Result<Option<PinnedSocket>, TransportError> {
        self.verify()?;
        // SAFETY: the retained directory and literal name remain live. O_PATH pins
        // the socket's filesystem object without opening it for I/O or following links.
        let descriptor = unsafe {
            libc::openat(
                self.directory.file.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(TransportError::UnsafeEndpoint)
            };
        }
        // SAFETY: openat returned a new owned descriptor, transferred exactly once.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file
            .metadata()
            .map_err(|_| TransportError::UnsafeEndpoint)?;
        // SAFETY: geteuid has no preconditions.
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(TransportError::UnsafeEndpoint);
        }
        self.verify()?;
        Ok(Some(PinnedSocket {
            identity: Identity::from_metadata(&metadata),
            file,
        }))
    }

    // Called only by the listener while its native and local endpoint leases live.
    fn reclaim_dead(&self, required: Option<Identity>) -> Result<(), TransportError> {
        let Some(identity) = self.socket_identity()? else {
            return Ok(());
        };
        if required.is_some_and(|required| required != identity) {
            return Err(TransportError::UnsafeEndpoint);
        }
        #[cfg(target_os = "linux")]
        let pin = self.pin_socket()?.ok_or(TransportError::UnsafeEndpoint)?;
        #[cfg(target_os = "linux")]
        if pin.identity != identity {
            return Err(TransportError::UnsafeEndpoint);
        }
        let probe = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
            .map_err(|_| TransportError::Unavailable)?;
        probe
            .set_nonblocking(true)
            .map_err(|_| TransportError::Unavailable)?;
        #[cfg(target_os = "linux")]
        let address = pin.address()?;
        #[cfg(target_os = "macos")]
        let address =
            socket2::SockAddr::unix(&self.path).map_err(|_| TransportError::UnsafeEndpoint)?;
        match probe.connect(&address) {
            Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {}
            _ => return Err(TransportError::UnsafeEndpoint),
        }
        self.verify()?;
        if self.socket_identity()? != Some(identity) {
            return Err(TransportError::UnsafeEndpoint);
        }
        // SAFETY: the verified retained directory and literal name remain live.
        // The accepted same-account check/unlink race is documented above.
        if unsafe { libc::unlinkat(self.directory.file.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(TransportError::Unavailable);
        }
        self.verify()?;
        self.ensure_absent()
    }

    pub(crate) fn register_socket(&self) -> Result<RegisteredSocket, TransportError> {
        #[cfg(target_os = "linux")]
        {
            self.pin_socket()?
                .ok_or(TransportError::UnsafeEndpoint)?
                .register(self)
        }
        #[cfg(target_os = "macos")]
        {
            let identity = self
                .socket_identity()?
                .ok_or(TransportError::UnsafeEndpoint)?;
            self.register_identity(identity)
        }
    }

    pub(crate) fn register_identity(
        &self,
        identity: Identity,
    ) -> Result<RegisteredSocket, TransportError> {
        if self.socket_identity()? != Some(identity) {
            return Err(TransportError::UnsafeEndpoint);
        }
        Ok(RegisteredSocket {
            endpoint: self.clone(),
            identity,
            active: true,
            #[cfg(target_os = "linux")]
            _pin: None,
        })
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct LeaseKey {
    directory: Identity,
    name: CString,
}

fn local_leases() -> &'static NativeMutex<HashSet<LeaseKey>> {
    static LEASES: OnceLock<NativeMutex<HashSet<LeaseKey>>> = OnceLock::new();
    LEASES.get_or_init(|| NativeMutex::new(HashSet::new()))
}

struct LocalEndpointLease(LeaseKey);

impl LocalEndpointLease {
    fn acquire(endpoint: &IpcEndpoint) -> Result<Self, TransportError> {
        let key = LeaseKey {
            directory: endpoint.directory.identity,
            name: endpoint.name.clone(),
        };
        if !local_leases()
            .lock()
            .map_err(|_| TransportError::Unavailable)?
            .insert(key.clone())
        {
            return Err(TransportError::UnsafeEndpoint);
        }
        Ok(Self(key))
    }
}

impl Drop for LocalEndpointLease {
    fn drop(&mut self) {
        if let Ok(mut leases) = local_leases().lock() {
            leases.remove(&self.0);
        }
    }
}

// Lock entries are deliberately retained. Unlinking one on release would let a new
// opener lock a different inode while an existing descriptor still owns the old lease.
pub(crate) struct EndpointLease {
    // Field order closes the native lock before admitting another local opener.
    _file: File,
    _local: LocalEndpointLease,
}

#[cfg(target_os = "linux")]
pub(crate) struct PinnedSocket {
    file: File,
    identity: Identity,
}

#[cfg(target_os = "linux")]
impl PinnedSocket {
    pub(crate) fn address(&self) -> Result<socket2::SockAddr, TransportError> {
        // Linux procfs magic links resolve the retained object, even after rename,
        // rather than performing a fresh lookup of the original endpoint pathname.
        socket2::SockAddr::unix(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
            .map_err(|_| TransportError::Unavailable)
    }

    pub(crate) fn register(
        self,
        endpoint: &IpcEndpoint,
    ) -> Result<RegisteredSocket, TransportError> {
        let mut registered = endpoint.register_identity(self.identity)?;
        // Retain the pinned inode through cleanup so it cannot be reused after the
        // SSH process closes its socket. Connected peer proof belongs to its caller.
        registered._pin = Some(self.file);
        Ok(registered)
    }
}

pub(crate) struct RegisteredSocket {
    endpoint: IpcEndpoint,
    identity: Identity,
    active: bool,
    #[cfg(target_os = "linux")]
    _pin: Option<File>,
}

impl RegisteredSocket {
    pub(crate) fn remove(&mut self) -> Result<(), TransportError> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.endpoint.reclaim_dead(Some(self.identity))
    }

    // Legacy SSH cleanup remains inspection-only until the tunnel owner switches
    // its fresh-name lifecycle to explicit remove after native process cleanup.
    pub(crate) fn cleanup(&mut self) -> Result<(), TransportError> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.endpoint.verify()?;
        match self.endpoint.socket_identity()? {
            Some(identity) if identity == self.identity => Ok(()),
            None => Ok(()),
            _ => Err(TransportError::UnsafeEndpoint),
        }
    }
}

impl Drop for RegisteredSocket {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn socket_identity(status: &libc::stat) -> Result<Identity, TransportError> {
    // SAFETY: geteuid has no preconditions.
    if status.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || status.st_uid != unsafe { libc::geteuid() }
        || status.st_nlink != 1
    {
        return Err(TransportError::UnsafeEndpoint);
    }
    Ok(Identity::from_status(status))
}

fn status_at(directory: &File, name: &CString) -> std::io::Result<libc::stat> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat is writable and the descriptor and NUL-terminated name remain live.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized status.
    Ok(unsafe { status.assume_init() })
}

pub(crate) fn validate_absolute_path(path: &Path, maximum: usize) -> Result<(), TransportError> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.len() > maximum
        || bytes.iter().any(u8::is_ascii_control)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(TransportError::InvalidConfiguration);
    }
    Ok(())
}

/// A private listener with a bounded number of pending application handshakes.
pub struct IpcListener {
    listener: Option<UnixListener>,
    socket: RegisteredSocket,
    _lease: EndpointLease,
    limit: MessageLimit,
    admission: AdmissionLimits,
    slots: Arc<Semaphore>,
}

impl IpcListener {
    /// Reclaims a dead owned entry under the lease, then binds a new private socket.
    pub fn bind(
        endpoint: IpcEndpoint,
        limit: MessageLimit,
        admission: AdmissionLimits,
    ) -> Result<Self, TransportError> {
        let _guard = crate::process::descriptor_guard().map_err(|_| TransportError::Unavailable)?;
        let lease = endpoint.lease()?;
        endpoint.reclaim_dead(None)?;
        endpoint.ensure_absent()?;
        let listener =
            UnixListener::bind(endpoint.path()).map_err(|_| TransportError::Unavailable)?;
        let socket = endpoint.register_socket()?;
        Ok(Self {
            listener: Some(listener),
            socket,
            _lease: lease,
            limit,
            admission,
            slots: Arc::new(Semaphore::new(admission.connections)),
        })
    }

    /// Closes the listener before identity-checked removal. A replacement is preserved.
    pub fn close(mut self) -> Result<(), TransportError> {
        let _guard = crate::process::descriptor_guard().map_err(|_| TransportError::Unavailable)?;
        self.close_native_listener();
        self.slots.close();
        self.socket.remove()
    }

    fn close_native_listener(&mut self) {
        if let Some(listener) = self.listener.take() {
            // Linux shuts down pre-exec duplicates too. macOS does not shut down
            // listening sockets; the descriptor guard waits for concurrent exec instead.
            #[cfg(target_os = "linux")]
            let _ = socket2::SockRef::from(&listener).shutdown(std::net::Shutdown::Both);
            drop(listener);
        }
    }

    /// Accepts only native same-account peers, eligible for a separate protocol handshake.
    pub async fn accept(&self) -> Result<PendingConnection<IpcTransport>, TransportError> {
        self.socket.endpoint.verify()?;
        let permit = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| TransportError::Closed)?;
        let (stream, _) = crate::process::descriptor_io(
            self.listener
                .as_ref()
                .ok_or(TransportError::Closed)?
                .accept(),
        )
        .await
        .map_err(|_| TransportError::Unavailable)?;
        let facts = same_account_peer(&stream)?;
        self.socket.endpoint.verify()?;
        Ok(PendingConnection::new(
            IpcTransport::new(stream, self.limit, facts)?,
            permit,
            Instant::now() + self.admission.deadline,
        ))
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        let Ok(_guard) = crate::process::descriptor_guard() else {
            return;
        };
        self.close_native_listener();
        let _ = self.socket.remove();
    }
}

struct Reader {
    stream: OwnedReadHalf,
    prefix: [u8; 4],
    prefix_read: usize,
    body: Vec<u8>,
    body_read: usize,
}

impl Reader {
    async fn message(&mut self, limit: MessageLimit) -> Result<Option<Vec<u8>>, TransportError> {
        while self.prefix_read < 4 {
            let read = self
                .stream
                .read(&mut self.prefix[self.prefix_read..])
                .await
                .map_err(|_| TransportError::Unavailable)?;
            if read == 0 {
                return if self.prefix_read == 0 {
                    Ok(None)
                } else {
                    Err(TransportError::InvalidFrame)
                };
            }
            self.prefix_read += read;
        }
        if self.body.is_empty() {
            let length = u32::from_be_bytes(self.prefix) as usize;
            limit.check(length)?;
            self.body
                .try_reserve_exact(length)
                .map_err(|_| TransportError::Unavailable)?;
            self.body.resize(length, 0);
        }
        while self.body_read < self.body.len() {
            let read = self
                .stream
                .read(&mut self.body[self.body_read..])
                .await
                .map_err(|_| TransportError::Unavailable)?;
            if read == 0 {
                return Err(TransportError::InvalidFrame);
            }
            self.body_read += read;
        }
        self.prefix_read = 0;
        self.body_read = 0;
        Ok(Some(std::mem::take(&mut self.body)))
    }
}

/// A full-duplex bounded length-framed byte connection with native same-account peer facts.
pub struct IpcTransport {
    reader: Mutex<Option<Reader>>,
    writer: Mutex<Option<OwnedWriteHalf>>,
    state: ConnectionState,
    facts: PeerFacts,
    limit: MessageLimit,
}

impl IpcTransport {
    fn new(
        stream: UnixStream,
        limit: MessageLimit,
        facts: PeerFacts,
    ) -> Result<Self, TransportError> {
        let socket = socket2::SockRef::from(&stream)
            .try_clone()
            .map_err(|_| TransportError::Unavailable)?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: Mutex::new(Some(Reader {
                stream: reader,
                prefix: [0; 4],
                prefix_read: 0,
                body: Vec::new(),
                body_read: 0,
            })),
            writer: Mutex::new(Some(writer)),
            state: ConnectionState::with_socket(socket),
            facts,
            limit,
        })
    }

    /// Connects a private route and checks the native peer account. Protocol proofs remain required.
    pub async fn connect(
        endpoint: &IpcEndpoint,
        limit: MessageLimit,
    ) -> Result<Self, TransportError> {
        endpoint
            .socket_identity()?
            .ok_or(TransportError::Unavailable)?;
        let stream = crate::process::descriptor_io(UnixStream::connect(endpoint.path()))
            .await
            .map_err(|_| TransportError::Unavailable)?;
        let facts = same_account_peer(&stream)?;
        endpoint.verify()?;
        Self::new(stream, limit, facts)
    }
}

impl MessageTransport for IpcTransport {
    fn peer_facts(&self) -> PeerFacts {
        self.facts
    }

    fn send<'a>(&'a self, message: &'a [u8]) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            self.limit.check(message.len())?;
            let mut writer = self
                .state
                .run(async { Ok(self.writer.lock().await) })
                .await?;
            let writer = writer.as_mut().ok_or(TransportError::Closed)?;
            let mut guard = SendGuard::new(&self.state);
            let result = self
                .state
                .run(async {
                    writer
                        .write_all(&(message.len() as u32).to_be_bytes())
                        .await
                        .map_err(|_| TransportError::Unavailable)?;
                    writer
                        .write_all(message)
                        .await
                        .map_err(|_| TransportError::Unavailable)?;
                    Ok(())
                })
                .await;
            guard.complete();
            if let Err(error) = result {
                self.state.abort(error);
            }
            result
        })
    }

    fn recv(&self) -> TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            if let Some(error) = self.state.error() {
                return if error == TransportError::Closed {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            let result = self
                .state
                .run(async {
                    self.reader
                        .lock()
                        .await
                        .as_mut()
                        .ok_or(TransportError::Closed)?
                        .message(self.limit)
                        .await
                })
                .await;
            match &result {
                Ok(None) => self.state.close(TransportError::Closed),
                Err(TransportError::Closed) => {}
                Err(error) => self.state.abort(*error),
                _ => {}
            }
            if result == Err(TransportError::Closed) {
                Ok(None)
            } else {
                result
            }
        })
    }

    fn close(&self) -> TransportFuture<'_, ()> {
        Box::pin(async move {
            self.state.close(TransportError::Closed);
            let writer = self.writer.lock().await.take();
            self.reader.lock().await.take();
            self.state.release_socket();
            if let Some(mut writer) = writer
                && let Err(error) = writer.shutdown().await
                && !matches!(
                    error.kind(),
                    std::io::ErrorKind::NotConnected | std::io::ErrorKind::BrokenPipe
                )
            {
                return Err(TransportError::Unavailable);
            }
            Ok(())
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn linux_socket_pin_cannot_register_a_replaced_inode() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateIpcDirectory::open(temporary.path()).unwrap();
        let endpoint = directory.endpoint("s").unwrap();
        let old = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
        let pin = endpoint.pin_socket().unwrap().unwrap();
        fs::rename(endpoint.path(), temporary.path().join("old")).unwrap();
        let replacement = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
        let identity = endpoint.socket_identity().unwrap();
        assert!(matches!(
            pin.register(&endpoint),
            Err(TransportError::UnsafeEndpoint)
        ));
        assert!(endpoint.socket_identity().unwrap() == identity);
        drop(old);
        drop(replacement);
    }
}
