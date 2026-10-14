//! Owned OpenSSH stream-local forwarding. A tunnel changes reachability, never protocol authority.
//!
//! Linux readiness pins the socket inode before verifying the owned process's PID.
//! macOS verifies the PID and original bound address before registering the observed
//! pathname identity. Cleanup reaps the child, then removes only that registered
//! dead socket through its retained private directory. Replacement entries remain.
//! The owning-account check/unlink race is accepted by the endpoint-reclaim owner
//! decision. macOS cannot pin Unix socket pathnames against owning-account renames.
//! A lease excludes cooperating starts, not raw native binds. Each start uses a
//! fresh name; an unverified socket or a refused connection grants no removal authority.

use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::ipc::{EndpointLease, IpcEndpoint, RegisteredSocket, validate_absolute_path};
use crate::peer_credentials::same_account_peer_fd;
use crate::transport::TransportError;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_VALUE_BYTES: usize = 16 * 1024;
const LOCALE_NAMES: [&str; 8] = [
    "LANG",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];

/// Validated literal OpenSSH destination, including configured aliases and optional account prefixes.
#[derive(Clone)]
pub struct SshDestination(String);

impl std::fmt::Debug for SshDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SshDestination(..)")
    }
}

impl SshDestination {
    /// Rejects option prefixes, shell metacharacters, empty account/host tokens, and controls.
    /// Colon and brackets admit IPv6 literals; a port belongs in the selected SSH configuration.
    pub fn new(value: &str) -> Result<Self, TransportError> {
        if value.is_empty()
            || value.len() > 255
            || value.starts_with('-')
            || value.split('@').any(str::is_empty)
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.@:%[]".contains(&byte))
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self(value.to_owned()))
    }
}

/// A remote Unix socket path that cannot inject another OpenSSH forwarding field.
#[derive(Clone)]
pub struct RemoteSocketPath(String);

impl std::fmt::Debug for RemoteSocketPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteSocketPath(..)")
    }
}

impl RemoteSocketPath {
    /// Admits a short absolute, literal path, bounded to the macOS/Linux common socket limit.
    pub fn new(value: &str) -> Result<Self, TransportError> {
        validate_forwarding_path(OsStr::new(value))?;
        Ok(Self(value.to_owned()))
    }
}

fn validate_forwarding_path(value: &OsStr) -> Result<(), TransportError> {
    validate_absolute_path(std::path::Path::new(value), 103)?;
    if value.as_bytes().len() <= 1
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(byte))
    {
        return Err(TransportError::InvalidConfiguration);
    }
    Ok(())
}

/// OpenSSH authentication policy. Neither mode enrolls a SpaceTerm device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationMode {
    /// Noninteractive key/agent authentication; no AskPass environment is supplied.
    BatchMode,
    /// A native desktop adapter supplies a live zeroized broker capability for this process only.
    AskPass,
}

/// One validated argv invocation, with no remote shell command or shell interpolation.
///
/// The selected configuration retains OpenSSH host/account trust and jump-host policy.
/// It is trusted local configuration, not a wire value. As with the existing desktop
/// control-master invocation, configured forwarding directives remain OpenSSH policy.
/// Composition must not substitute remotely supplied executable or configuration paths.
pub struct SshTunnelCommand {
    executable: PathBuf,
    arguments: Vec<OsString>,
    endpoint: IpcEndpoint,
    authentication: AuthenticationMode,
}

impl SshTunnelCommand {
    /// Constructs a dedicated, foreground, non-multiplexed stream-local forwarding process.
    pub fn new(
        executable: PathBuf,
        managed_config: PathBuf,
        destination: SshDestination,
        endpoint: IpcEndpoint,
        remote: RemoteSocketPath,
        authentication: AuthenticationMode,
    ) -> Result<Self, TransportError> {
        validate_absolute_path(&executable, 4096)?;
        validate_absolute_path(&managed_config, 4096)?;
        validate_forwarding_path(endpoint.path().as_os_str())?;
        endpoint.verify()?;
        let mut arguments = vec![OsString::from("-F"), managed_config.into_os_string()];
        for option in [
            "ControlMaster=no",
            "ControlPath=none",
            "ControlPersist=no",
            "IgnoreUnknown=ForkAfterAuthentication",
            "ForkAfterAuthentication=no",
            "ExitOnForwardFailure=yes",
            "StreamLocalBindUnlink=no",
            "StreamLocalBindMask=0177",
            "PermitLocalCommand=no",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ForwardX11Trusted=no",
            "RequestTTY=no",
            "RemoteCommand=none",
            "SessionType=none",
            "LogLevel=ERROR",
        ] {
            arguments.extend([OsString::from("-o"), OsString::from(option)]);
        }
        arguments.extend([
            OsString::from("-o"),
            OsString::from(match authentication {
                AuthenticationMode::BatchMode => "BatchMode=yes",
                AuthenticationMode::AskPass => "BatchMode=no",
            }),
        ]);
        arguments.extend([
            OsString::from("-N"),
            OsString::from("-T"),
            OsString::from("-L"),
        ]);
        let mut forward = endpoint.path().as_os_str().to_owned();
        forward.push(":");
        forward.push(remote.0);
        arguments.extend([forward, OsString::from("--"), OsString::from(destination.0)]);
        Ok(Self {
            executable,
            arguments,
            endpoint,
            authentication,
        })
    }

    /// Exposes validated argv for the native process boundary. Values must not enter diagnostics.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

/// Captured, allowlisted SSH environment. No ambient process variables survive a spawn.
pub struct SshEnvironment {
    home: PathBuf,
    entries: Vec<(OsString, OsString)>,
}

impl SshEnvironment {
    /// Supplies an absolute HOME and explicit PATH, locale, and optional SSH_AUTH_SOCK entries.
    ///
    /// Loader variables, AskPass state, duplicate keys, and all other names are rejected.
    /// Capture these values once at desktop startup rather than from a terminal or wire request.
    pub fn new(home: PathBuf, entries: Vec<(OsString, OsString)>) -> Result<Self, TransportError> {
        validate_absolute_path(&home, 4096)?;
        if entries.len() > 10 || !entries.iter().any(|(name, _)| name == "PATH") {
            return Err(TransportError::InvalidConfiguration);
        }
        for (index, (name, value)) in entries.iter().enumerate() {
            let name = name.to_str().ok_or(TransportError::InvalidConfiguration)?;
            if !matches!(name, "PATH" | "SSH_AUTH_SOCK") && !LOCALE_NAMES.contains(&name) {
                return Err(TransportError::InvalidConfiguration);
            }
            if entries[..index].iter().any(|(earlier, _)| earlier == name)
                || value.is_empty()
                || value.as_bytes().len() > MAX_VALUE_BYTES
                || value.as_bytes().iter().any(u8::is_ascii_control)
            {
                return Err(TransportError::InvalidConfiguration);
            }
            if name == "SSH_AUTH_SOCK" {
                validate_absolute_path(std::path::Path::new(value), 4096)?;
            }
        }
        Ok(Self { home, entries })
    }
}

/// Narrow injection boundary for the existing native zeroized AskPass broker.
///
/// The adapter stays alive until process cleanup finishes. It owns prompt presentation,
/// broker IPC, cancellation, and secret replies. This crate never implements a fallback
/// credential provider and never reads or stores authentication responses.
pub trait TunnelAskPass: Send + Sync + 'static {
    /// Issues one noncloneable spawn-only capability for a live native broker.
    fn handoff(&self) -> Result<AskPassHandoff, TransportError>;

    /// Fences presentation and replies against revocable foreground authority.
    /// Adapters without this support fail closed rather than checking only at spawn.
    fn handoff_authorized(
        &self,
        _authority: AskPassAuthorityCheck,
    ) -> Result<AskPassHandoff, TransportError> {
        Err(TransportError::PeerAccountRejected)
    }
}

/// Content-free validation of the exact foreground credential generation.
/// The native adapter checks it before presentation, while waiting, and before replying.
pub type AskPassAuthorityCheck = Arc<dyn Fn() -> bool + Send + Sync>;

struct AskPassAttemptLifetime(Option<Box<dyn FnOnce() + Send>>);

impl Drop for AskPassAttemptLifetime {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel();
        }
    }
}

/// Spawn-only AskPass helper route and zeroized broker capability, without Debug or Clone.
pub struct AskPassHandoff {
    helper: PathBuf,
    endpoint: OsString,
    capability: Zeroizing<Vec<u8>>,
    attempt: Option<AskPassAttemptLifetime>,
}

impl AskPassHandoff {
    /// Validates the existing broker-v1 endpoint and its 64-byte hexadecimal capability.
    /// No authentication response crosses this handoff.
    pub fn new(
        helper: PathBuf,
        endpoint: OsString,
        capability: Zeroizing<Vec<u8>>,
    ) -> Result<Self, TransportError> {
        validate_absolute_path(&helper, 4096)?;
        let bytes = endpoint.as_bytes();
        if bytes.len() < 10
            || bytes.len() > 256
            || bytes[8] != b':'
            || !bytes[..8].iter().all(u8::is_ascii_hexdigit)
            || bytes[9] != b'/'
            || bytes.iter().any(u8::is_ascii_control)
            || capability.len() != 64
            || !capability.iter().all(u8::is_ascii_hexdigit)
        {
            return Err(TransportError::InvalidConfiguration);
        }
        Ok(Self {
            helper,
            endpoint,
            capability,
            attempt: None,
        })
    }

    /// Retains exactly one broker until authentication succeeds or this start ends.
    /// Cancellation runs on spawn failure, deadline, dropped start, and process exit.
    pub fn with_attempt_lifetime(mut self, cancel: impl FnOnce() + Send + 'static) -> Self {
        self.attempt = Some(AskPassAttemptLifetime(Some(Box::new(cancel))));
        self
    }
}

/// Bounded process-exit facts. Native stderr, command arguments, and paths are never retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TunnelExit {
    /// Whether the owned OpenSSH leader exited successfully.
    pub success: bool,
    /// Native numeric exit code, absent when terminated by a signal.
    pub code: Option<i32>,
}

impl From<ExitStatus> for TunnelExit {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
        }
    }
}

#[derive(Clone, Copy)]
enum ProcessState {
    Starting,
    Ready(u32),
    Finished(Option<u32>, Result<TunnelExit, TransportError>),
}

/// Awaitable cleanup observation that remains usable after the tunnel owner is dropped.
#[derive(Clone)]
pub struct TunnelCompletion(watch::Receiver<ProcessState>);

impl TunnelCompletion {
    /// Resolves after group termination, leader reaping and registered-socket removal.
    /// An unverified or replacement socket is preserved.
    pub async fn wait(&self) -> Result<TunnelExit, TransportError> {
        let mut state = self.0.clone();
        loop {
            if let ProcessState::Finished(_, result) = *state.borrow_and_update() {
                return result;
            }
            state
                .changed()
                .await
                .map_err(|_| TransportError::Unavailable)?;
        }
    }
}

/// Singular tunnel owner. Drop enqueues cleanup on a dedicated native supervisor thread.
///
/// Cleanup never depends on a running async executor: cancelling a start future, dropping
/// an owner, or shutting down tokio still terminates the private process group and reaps
/// its leader. Descendants are terminated but are reaped by their owning host parent.
pub struct SshTunnel {
    cancel: mpsc::Sender<()>,
    state: watch::Receiver<ProcessState>,
    endpoint: IpcEndpoint,
}

impl SshTunnel {
    /// Starts OpenSSH and waits for a native connection to its owned peer under a deadline.
    /// No real host contact occurs in this crate's tests; production uses the injected executable.
    pub async fn start(
        command: SshTunnelCommand,
        environment: SshEnvironment,
        askpass: Option<Arc<dyn TunnelAskPass>>,
        deadline: Duration,
    ) -> Result<Self, TransportError> {
        if deadline.is_zero()
            || deadline > Duration::from_secs(60)
            || (command.authentication == AuthenticationMode::AskPass) != askpass.is_some()
        {
            return Err(TransportError::InvalidConfiguration);
        }
        let lease = command.endpoint.lease()?;
        command.endpoint.ensure_absent()?;
        let endpoint = command.endpoint.clone();
        let (cancel, cancellation) = mpsc::channel();
        let (updates, state) = watch::channel(ProcessState::Starting);
        std::thread::Builder::new()
            .name("spaceterm-ssh-tunnel".into())
            .spawn(move || {
                supervise(command, environment, askpass, lease, cancellation, updates);
            })
            .map_err(|_| TransportError::Unavailable)?;
        let tunnel = Self {
            cancel,
            state,
            endpoint,
        };
        match tokio::time::timeout(deadline, tunnel.wait_ready()).await {
            Ok(result) => result?,
            Err(_) => {
                // Do not report an expired start while its child or endpoint can still
                // overlap a retry. Dropped futures retain the lease in the supervisor.
                let _ = tunnel.cancel().await;
                return Err(TransportError::HandshakeExpired);
            }
        }
        Ok(tunnel)
    }

    async fn wait_ready(&self) -> Result<(), TransportError> {
        let mut state = self.state.clone();
        loop {
            match *state.borrow_and_update() {
                ProcessState::Ready(_) => return Ok(()),
                ProcessState::Finished(_, Err(error)) => return Err(error),
                ProcessState::Finished(_, Ok(_)) => return Err(TransportError::TunnelExited),
                ProcessState::Starting => {}
            }
            state
                .changed()
                .await
                .map_err(|_| TransportError::Unavailable)?;
        }
    }

    /// Reports the owned process ID, when spawned. Do not use it as authentication authority.
    pub fn process_id(&self) -> Option<u32> {
        match *self.state.borrow() {
            ProcessState::Ready(pid) => Some(pid),
            ProcessState::Finished(pid, _) => pid,
            ProcessState::Starting => None,
        }
    }

    /// Supplies the local route for an ordinary IPC and Paired Device handshake.
    pub fn endpoint(&self) -> &IpcEndpoint {
        &self.endpoint
    }

    /// Observes final native cleanup independently of this owner's lifetime.
    pub fn completion(&self) -> TunnelCompletion {
        TunnelCompletion(self.state.clone())
    }

    /// Cancels and waits for native termination, reaping and registered-socket removal.
    /// Unverified and replacement entries remain untouched.
    pub async fn cancel(&self) -> Result<TunnelExit, TransportError> {
        let _ = self.cancel.send(());
        self.completion().wait().await
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
    }
}

struct OwnedChild {
    child: Option<Child>,
    pid: u32,
    askpass_attempt: Option<AskPassAttemptLifetime>,
}

impl OwnedChild {
    fn status(&self, events: libc::c_int) -> Result<Option<libc::siginfo_t>, TransportError> {
        let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: status is writable. WNOWAIT retains the child's process identity,
        // including a zombie leader, until group signaling and the final owned wait.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                status.as_mut_ptr(),
                events | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(TransportError::Unavailable)
            };
        }
        // SAFETY: waitid initialized status; si_pid reads its child-status field.
        let status = unsafe { status.assume_init() };
        Ok((unsafe { status.si_pid() } != 0).then_some(status))
    }

    fn exited(&self) -> Result<bool, TransportError> {
        self.status(libc::WEXITED).map(|status| status.is_some())
    }

    fn stop(&self) -> Result<bool, TransportError> {
        // SAFETY: the private group ID still belongs to the unreaped owned child.
        if unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGSTOP) } != 0 {
            // macOS denies signaling a zombie group. Observe the still-owned child
            // before classifying that result as failure; it may have just exited.
            return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                || self.exited()?
            {
                Ok(false)
            } else {
                Err(TransportError::Unavailable)
            };
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(status) = self.status(libc::WSTOPPED | libc::WEXITED)? {
                return Ok(status.si_code == libc::CLD_STOPPED);
            }
            if std::time::Instant::now() >= deadline {
                return Err(TransportError::Unavailable);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn terminate_and_reap(&mut self) -> Result<TunnelExit, TransportError> {
        let Some(child) = self.child.as_mut() else {
            return Err(TransportError::Unavailable);
        };
        // SAFETY: the private group ID is the still-unreaped owned child's ID. No PID
        // lookup or signal can target a replacement process after reaping.
        let result = unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            let _ = child.kill();
        }
        let status = child.wait().map_err(|_| TransportError::Unavailable)?;
        self.child.take();
        Ok(status.into())
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.terminate_and_reap();
        }
    }
}

fn spawn(
    command: &SshTunnelCommand,
    environment: &SshEnvironment,
    askpass: Option<&dyn TunnelAskPass>,
) -> Result<OwnedChild, TransportError> {
    let mut handoff = askpass.map(TunnelAskPass::handoff).transpose()?;
    let mut native = Command::new(&command.executable);
    native
        .args(&command.arguments)
        .env_clear()
        .current_dir(&environment.home)
        .env("HOME", &environment.home)
        .envs(environment.entries.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(handoff) = &handoff {
        native
            .env("SSH_ASKPASS", &handoff.helper)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "spaceterm-askpass")
            .env("SPACETERM_SSH_ASKPASS_MODE", "broker-v1")
            .env("SPACETERM_SSH_ASKPASS_SOCKET", &handoff.endpoint)
            .env(
                "SPACETERM_SSH_ASKPASS_CAPABILITY",
                OsStr::from_bytes(&handoff.capability),
            );
    }
    // SAFETY: this callback runs after fork and uses only async-signal-safe operations
    // on a stack signal set. It does not touch shared application state.
    unsafe {
        native.pre_exec(clear_child_signal_mask);
    }
    let child = crate::process::spawn(&mut native).map_err(|_| TransportError::Unavailable)?;
    let pid = child.id();
    // Discard the spawn request immediately; the explicit capability owner is zeroized.
    drop(native);
    let askpass_attempt = handoff.as_mut().and_then(|handoff| handoff.attempt.take());
    drop(handoff);
    Ok(OwnedChild {
        child: Some(child),
        pid,
        askpass_attempt,
    })
}

fn clear_child_signal_mask() -> std::io::Result<()> {
    let mut empty = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes this stack set.
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the set is initialized and sigprocmask reads only this live stack value.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct ForwardingSocket(RegisteredSocket);

impl ForwardingSocket {
    fn cleanup(&mut self) -> Result<(), TransportError> {
        self.0.remove()
    }
}

fn inspect_forwarding_socket(
    endpoint: &IpcEndpoint,
    pid: u32,
) -> Result<Option<ForwardingSocket>, TransportError> {
    #[cfg(target_os = "linux")]
    let Some(pinned) = endpoint.pin_socket()? else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    let Some(identity) = endpoint.socket_identity()? else {
        return Ok(None);
    };
    let probe = {
        let _guard = crate::process::descriptor_guard().map_err(|_| TransportError::Unavailable)?;
        socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
            .map_err(|_| TransportError::Unavailable)?
    };
    probe
        .set_nonblocking(true)
        .map_err(|_| TransportError::Unavailable)?;
    #[cfg(target_os = "linux")]
    let address = pinned.address()?;
    #[cfg(target_os = "macos")]
    let address = socket2::SockAddr::unix(endpoint.path())
        .map_err(|_| TransportError::InvalidConfiguration)?;
    if let Err(error) = probe.connect(&address) {
        if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ) || error.raw_os_error() == Some(libc::EAGAIN)
        {
            return Ok(None);
        }
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(TransportError::UnsafeEndpoint);
        }
        let mut descriptor = libc::pollfd {
            fd: probe.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: poll reads and writes this live descriptor; a zero timeout never blocks.
        if unsafe { libc::poll(&mut descriptor, 1, 0) } <= 0 {
            return Ok(None);
        }
        if probe
            .take_error()
            .map_err(|_| TransportError::UnsafeEndpoint)?
            .is_some()
        {
            return Ok(None);
        }
    }
    // Path ownership alone is insufficient. A nonblocking, empty local connection
    // proves the listener's native process identity before publishing readiness.
    let facts = same_account_peer_fd(probe.as_fd()).map_err(|_| TransportError::UnsafeEndpoint)?;
    if facts.local_pid != Some(pid) {
        return Err(TransportError::UnsafeEndpoint);
    }
    #[cfg(target_os = "macos")]
    if !matches_bound_address(
        &probe
            .peer_addr()
            .map_err(|_| TransportError::UnsafeEndpoint)?,
        endpoint.path(),
    ) {
        // A listener renamed from another address cannot prove this forwarding entry.
        return Err(TransportError::UnsafeEndpoint);
    }
    #[cfg(target_os = "linux")]
    {
        pinned
            .register(endpoint)
            .map(|registered| Some(ForwardingSocket(registered)))
    }
    #[cfg(target_os = "macos")]
    {
        // Preserve the pre-connect identity through PID verification. Registration
        // checks it again; cleanup removes only this dead socket after child reaping.
        endpoint
            .register_identity(identity)
            .map(|registered| Some(ForwardingSocket(registered)))
    }
}

#[cfg(target_os = "macos")]
fn matches_bound_address(address: &socket2::SockAddr, path: &std::path::Path) -> bool {
    let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    let length = address.len() as usize;
    if address.family() != libc::AF_UNIX as libc::sa_family_t
        || length <= offset
        || length > std::mem::size_of::<libc::sockaddr_un>()
    {
        return false;
    }
    // Darwin getpeername can omit the NUL or return OpenSSH's full zero-padded
    // sockaddr. socket2::as_pathname assumes a terminator and drops the last byte
    // in the first case, so inspect the bounded name and validate all padding.
    // SAFETY: SockAddr initializes length bytes; the checked offset and length
    // keep this slice inside its live sockaddr storage.
    let bytes = unsafe {
        std::slice::from_raw_parts(address.as_ptr().cast::<u8>().add(offset), length - offset)
    };
    let name_length = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes[name_length..].iter().all(|byte| *byte == 0)
        && &bytes[..name_length] == path.as_os_str().as_bytes()
}

fn supervise(
    command: SshTunnelCommand,
    environment: SshEnvironment,
    askpass: Option<Arc<dyn TunnelAskPass>>,
    lease: EndpointLease,
    cancellation: mpsc::Receiver<()>,
    updates: watch::Sender<ProcessState>,
) {
    if !matches!(cancellation.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        drop(askpass);
        drop(lease);
        updates.send_replace(ProcessState::Finished(None, Err(TransportError::Closed)));
        return;
    }
    let mut child = match spawn(&command, &environment, askpass.as_deref()) {
        Ok(child) => child,
        Err(error) => {
            drop(askpass);
            drop(lease);
            updates.send_replace(ProcessState::Finished(None, Err(error)));
            return;
        }
    };
    let pid = child.pid;
    let mut socket: Option<ForwardingSocket> = None;
    let failure = loop {
        let cancelled = match cancellation.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        };
        let mut became_ready = false;
        if socket.is_none() {
            match inspect_forwarding_socket(&command.endpoint, pid) {
                Ok(Some(registered)) => {
                    socket = Some(registered);
                    became_ready = true;
                }
                Ok(None) => {}
                Err(error) => break Some(error),
            }
        }
        match child.exited() {
            Ok(true) => break None,
            Ok(false) => {}
            Err(error) => break Some(error),
        }
        if cancelled {
            break None;
        }
        if became_ready {
            // OpenSSH has authenticated. Its foreground prompt authority ends here,
            // not when the long-lived forwarding process eventually exits.
            drop(child.askpass_attempt.take());
            updates.send_replace(ProcessState::Ready(pid));
        }
    };
    // Dismiss unanswered prompts before waiting for native process cleanup.
    drop(child.askpass_attempt.take());
    // Stop before the final ownership probe so a cancellation-time listener remains
    // live long enough to prove its PID. The lease excludes cooperating starts, not
    // foreign native binds. Never infer socket ownership from post-reap refusal.
    let capture = if socket.is_none() && failure.is_none() {
        child.stop().and_then(|stopped| {
            if stopped {
                inspect_forwarding_socket(&command.endpoint, pid)
                    .map(|registered| socket = registered)
            } else {
                Ok(())
            }
        })
    } else {
        Ok(())
    };
    let result = child.terminate_and_reap();
    let cleanup = capture.and_then(|()| socket.as_mut().map_or(Ok(()), ForwardingSocket::cleanup));
    // Release the native broker lease before publishing cleanup completion.
    drop(askpass);
    drop(lease);
    updates.send_replace(ProcessState::Finished(
        Some(pid),
        failure
            .map_or(result, Err)
            .and_then(|exit| cleanup.map(|()| exit)),
    ));
}
