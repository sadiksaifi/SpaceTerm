//! Private same-account Unix IPC with retained endpoint identity and bounded framing.
//!
//! Endpoint leases are advisory. Native macOS and Linux pathname removal cannot
//! atomically require the registered inode, so this adapter preserves socket entries
//! on shutdown. The directory owner must remove stale entries with local authority
//! before reusing their names.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
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

    pub(crate) fn register_socket(&self) -> Result<RegisteredSocket, TransportError> {
        let identity = self
            .socket_identity()?
            .ok_or(TransportError::UnsafeEndpoint)?;
        self.register_identity(identity)
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
    pub(crate) fn cleanup(&mut self) -> Result<(), TransportError> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        // A pinned inode or a random quarantine name cannot protect a deletion
        // target from same-account replacement between fstatat and unlinkat.
        // Inspect for a typed replacement failure, but never rename or remove it.
        match status_at(&self.endpoint.directory.file, &self.endpoint.name) {
            Ok(status) if socket_identity(&status)? == self.identity => Ok(()),
            Ok(_) => Err(TransportError::UnsafeEndpoint),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(TransportError::Unavailable),
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
    listener: UnixListener,
    socket: RegisteredSocket,
    _lease: EndpointLease,
    limit: MessageLimit,
    admission: AdmissionLimits,
    slots: Arc<Semaphore>,
}

impl IpcListener {
    /// Binds a new socket in a retained directory. Existing entries are never unlinked.
    /// Drop closes the listener but preserves its entry; reuse requires owner cleanup.
    pub fn bind(
        endpoint: IpcEndpoint,
        limit: MessageLimit,
        admission: AdmissionLimits,
    ) -> Result<Self, TransportError> {
        let lease = endpoint.lease()?;
        endpoint.ensure_absent()?;
        let listener =
            UnixListener::bind(endpoint.path()).map_err(|_| TransportError::Unavailable)?;
        let socket = endpoint.register_socket()?;
        Ok(Self {
            listener,
            socket,
            _lease: lease,
            limit,
            admission,
            slots: Arc::new(Semaphore::new(admission.connections)),
        })
    }

    /// Accepts only native same-account peers, eligible for a separate protocol handshake.
    pub async fn accept(&self) -> Result<PendingConnection<IpcTransport>, TransportError> {
        self.socket.endpoint.verify()?;
        let permit = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| TransportError::Closed)?;
        let (stream, _) = self
            .listener
            .accept()
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
        // Keep the listener live while checking its pathname identity. The entry
        // remains because native removal cannot exclude a concurrent replacement.
        let _ = self.socket.cleanup();
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
        endpoint.verify()?;
        let metadata =
            fs::symlink_metadata(endpoint.path()).map_err(|_| TransportError::UnsafeEndpoint)?;
        if !metadata.file_type().is_socket() {
            return Err(TransportError::UnsafeEndpoint);
        }
        let stream = UnixStream::connect(endpoint.path())
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
            if let Some(mut writer) = writer {
                writer
                    .shutdown()
                    .await
                    .map_err(|_| TransportError::Unavailable)?;
            }
            Ok(())
        })
    }
}
