use super::{RegisteredServer, ServiceFailure, ServiceFuture};
use crate::startup::{AccountBuildKey, BuildIdentity};
use std::{
    ffi::{CString, OsString},
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 16 * 1024;
const CONFIGURATION_LIMIT: usize = 16 * 1024;

/// The signed installation owner supplies exact approved executable selection.
/// Implementations verify the registration's selection and storage compatibility
/// before returning a path. Native managers do not scan installations or approve code.
pub trait ServiceInstallations: Send + Sync {
    /// Verifies and resolves only this registration's approved installed server.
    fn approved_executable(
        &self,
        registration: &RegisteredServer,
    ) -> Result<PathBuf, ServiceFailure>;
    /// Removes verified installed code after the supervisor has proved quiescence.
    /// Identity, pairing, organization and account-wide linger remain untouched.
    fn uninstall(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure>;
}

#[derive(Clone)]
pub(super) struct Account {
    pub(super) key: AccountBuildKey,
    pub(super) home: PathBuf,
    pub(super) runtime: PathBuf,
}
impl Account {
    pub(super) fn current() -> Result<Self, ServiceFailure> {
        let (uid, home) = super::supervisor::current_account()?;
        let build_identity = match env!("SPACETERM_HOST_BUILD") {
            "release" => BuildIdentity::Release,
            "preflight" => BuildIdentity::Preflight,
            "development" => BuildIdentity::Development,
            _ => return Err(ServiceFailure::UnsupportedHost),
        };
        Ok(Self {
            key: AccountBuildKey {
                account_uid: uid,
                build_identity,
            },
            home,
            runtime: PathBuf::from(format!("/run/user/{uid}")),
        })
    }
    pub(super) fn check(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        if registration.key() != self.key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        registration.directory.verify()?;
        Ok(())
    }
    pub(super) fn environment(&self) -> Vec<(OsString, OsString)> {
        let runtime = format!("/run/user/{}", self.key.account_uid);
        vec![
            ("HOME".into(), self.home.as_os_str().to_owned()),
            ("XDG_RUNTIME_DIR".into(), runtime.clone().into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={runtime}/bus").into(),
            ),
        ]
    }
}

/// A closed native utility invocation. Only account-service operations construct it.
/// It has no Debug implementation because arguments can contain local paths.
pub struct ServiceCommand {
    program: &'static str,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    interactive: bool,
    timeout: Duration,
    capture_stdout: bool,
}
impl ServiceCommand {
    pub(super) fn new(
        program: &'static str,
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Self {
        Self {
            program,
            arguments: arguments.into_iter().collect(),
            environment: Vec::new(),
            interactive: false,
            timeout: Duration::from_secs(30),
            capture_stdout: true,
        }
    }
    pub(super) fn user_manager(mut self, account: &Account) -> Self {
        self.environment = account.environment();
        self
    }
    pub(super) fn manager_configuration(&mut self, root: &Path) {
        self.environment
            .push(("XDG_CONFIG_HOME".into(), root.as_os_str().to_owned()));
    }
    pub(super) fn authorization(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self.timeout = Duration::from_secs(120);
        self
    }
    pub(super) fn discard_stdout(mut self) -> Self {
        self.capture_stdout = false;
        self
    }
    /// The fixed OS utility, never SpaceTerm or downloaded code.
    pub fn program(&self) -> &str {
        self.program
    }
    /// Fixed arguments for inspection by an injected native runner.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    /// Independently constructed, nonsecret manager reachability values.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }
}

/// Bounded utility output used only for native observation decoding.
/// This value is not a diagnostic and deliberately has no Debug implementation.
pub struct ServiceCommandOutput {
    pub(super) code: i32,
    pub(super) bytes: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}
impl ServiceCommandOutput {
    pub(super) fn success(&self) -> Result<(), ServiceFailure> {
        if self.code == 0 {
            Ok(())
        } else {
            Err(ServiceFailure::SupervisorUnavailable)
        }
    }
}

/// Owns a fixed utility through completion and exact-child reap.
/// Dropping an observer must not abandon its process.
pub trait ServiceCommandRunner: Send + Sync {
    /// Executes a closed command with bounded output and typed failures only.
    fn run(&self, command: ServiceCommand) -> ServiceFuture<'_, ServiceCommandOutput>;
}

/// Native runner using the shared descriptor exclusion and process creation boundary.
#[derive(Default)]
pub struct NativeServiceCommandRunner;
impl ServiceCommandRunner for NativeServiceCommandRunner {
    fn run(&self, command: ServiceCommand) -> ServiceFuture<'_, ServiceCommandOutput> {
        Box::pin(async move {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // The worker, not the observer, owns the Child and its original deadline.
            std::thread::Builder::new()
                .name("service-utility".into())
                .spawn(move || {
                    let (result, unreaped) = execute(command);
                    let _ = sender.send(result);
                    if let Some(mut child) = unreaped {
                        let _ = child.wait();
                    }
                })
                .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
            receiver
                .await
                .map_err(|_| ServiceFailure::CleanupUnresolved)?
        })
    }
}

fn execute(
    invocation: ServiceCommand,
) -> (Result<ServiceCommandOutput, ServiceFailure>, Option<Child>) {
    let mut retained = None;
    let result = (|| {
        let deadline = Instant::now()
            .checked_add(invocation.timeout)
            .ok_or(ServiceFailure::LimitExceeded)?;
        let mut command = Command::new(invocation.program);
        command
            .args(&invocation.arguments)
            .env_clear()
            .env("LC_ALL", "C")
            .env("PATH", "/usr/bin:/bin")
            .envs(invocation.environment)
            .current_dir("/")
            .stdin(if invocation.interactive {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stdout(if invocation.capture_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped());
        let mut child = crate::process::spawn(&mut command)
            .map_err(|_| ServiceFailure::SupervisorUnavailable)?;
        let result = collect(&mut child, deadline, invocation.capture_stdout);
        match result {
            Ok((status, bytes, stderr)) => {
                use std::os::unix::process::ExitStatusExt as _;
                if matches!(invocation.program, "/usr/bin/sudo" | "/usr/bin/pkexec")
                    && status.signal() == Some(libc::SIGINT)
                {
                    return Err(ServiceFailure::AuthorizationCancelled);
                }
                Ok(ServiceCommandOutput {
                    code: status.code().ok_or(ServiceFailure::SupervisorUnavailable)?,
                    bytes,
                    stderr,
                })
            }
            Err(failure) => match retire(child) {
                Ok(()) => Err(failure),
                Err(child) => {
                    retained = Some(child);
                    Err(ServiceFailure::CleanupUnresolved)
                }
            },
        }
    })();
    (result, retained)
}

fn collect(
    child: &mut Child,
    deadline: Instant,
    capture_stdout: bool,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), ServiceFailure> {
    let mut stdout = child.stdout.take();
    if capture_stdout && stdout.is_none() {
        return Err(ServiceFailure::SupervisorUnavailable);
    }
    let mut stderr = child
        .stderr
        .take()
        .ok_or(ServiceFailure::SupervisorUnavailable)?;
    for fd in stdout
        .iter()
        .map(AsRawFd::as_raw_fd)
        .chain([stderr.as_raw_fd()])
    {
        // SAFETY: both live owned pipes remain open; flags affect only these descriptors.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
    }
    let mut bytes = Vec::new();
    let mut error_bytes = Vec::new();
    let mut count = 0usize;
    let mut eof = [!capture_stdout, false];
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Err(ServiceFailure::SupervisorUnavailable);
        }
        let streams = [
            stdout.as_mut().map(|stdout| stdout as &mut dyn Read),
            Some(&mut stderr as &mut dyn Read),
        ];
        for (index, stream) in streams.into_iter().enumerate() {
            let Some(stream) = stream else {
                continue;
            };
            if eof[index] {
                continue;
            }
            let mut buffer = [0; 2048];
            match stream.read(&mut buffer) {
                Ok(0) => eof[index] = true,
                Ok(n) => {
                    count = count.checked_add(n).ok_or(ServiceFailure::LimitExceeded)?;
                    if count > OUTPUT_LIMIT {
                        return Err(ServiceFailure::LimitExceeded);
                    }
                    if index == 0 {
                        bytes.extend_from_slice(&buffer[..n]);
                    } else {
                        error_bytes.extend_from_slice(&buffer[..n]);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(ServiceFailure::SupervisorUnavailable),
            }
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        }
        if let Some(status) = status
            && eof == [true, true]
        {
            return Ok((status, bytes, error_bytes));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn retire(mut child: Child) -> Result<(), Child> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(_) => return Err(child),
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => return Err(child),
        }
    }
}

pub(super) fn checked_executable(path: &Path) -> Result<&str, ServiceFailure> {
    super::registration::validate_path(path)?;
    path.to_str().ok_or(ServiceFailure::InvalidRecord)
}

// User service directories often already have mode 0755. They are not secret
// storage. Do not repair them to 0700 or reuse the strict key-store admission.
pub(super) struct ConfigurationFile {
    parent: File,
    name: CString,
}
impl ConfigurationFile {
    pub(super) fn open(
        home: &Path,
        components: &[&str],
        name: &str,
        create: bool,
    ) -> Result<Option<Self>, ServiceFailure> {
        super::registration::validate_path(home)?;
        let mut parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home)
            .map_err(|_| ServiceFailure::StorageUnavailable)?;
        check_directory(&parent)?;
        for part in components {
            let name = CString::new(*part).map_err(|_| ServiceFailure::InvalidRecord)?;
            // SAFETY: parent and the one-component name are live throughout openat.
            let mut fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                if !create {
                    return Ok(None);
                }
                if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(ServiceFailure::StorageUnavailable);
                }
                parent
                    .sync_all()
                    .map_err(|_| ServiceFailure::Indeterminate)?;
                fd = unsafe {
                    libc::openat(
                        parent.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
            if fd < 0 {
                return Err(ServiceFailure::UnsafeStorage);
            }
            // SAFETY: successful openat returned a fresh descriptor.
            parent = unsafe { File::from_raw_fd(fd) };
            check_directory(&parent)?;
        }
        Ok(Some(Self {
            parent,
            name: CString::new(name).map_err(|_| ServiceFailure::InvalidRecord)?,
        }))
    }
    fn existing(&self) -> Result<Option<File>, ServiceFailure> {
        // SAFETY: this fixed single-component name is resolved beneath a held directory.
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(ServiceFailure::UnsafeStorage)
            };
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let facts = file
            .metadata()
            .map_err(|_| ServiceFailure::StorageUnavailable)?;
        if !facts.is_file()
            || facts.uid() != unsafe { libc::geteuid() }
            || facts.nlink() != 1
            || facts.mode() & 0o7777 != 0o600
            || facts.len() > CONFIGURATION_LIMIT as u64
        {
            return Err(ServiceFailure::UnsafeStorage);
        }
        check_acl(&file)?;
        Ok(Some(file))
    }
    pub(super) fn publish(&self, bytes: &[u8]) -> Result<(), ServiceFailure> {
        if bytes.len() > CONFIGURATION_LIMIT {
            return Err(ServiceFailure::LimitExceeded);
        }
        check_directory(&self.parent)?;
        let original = self.existing()?;
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).map_err(|_| ServiceFailure::StorageUnavailable)?;
        let temporary = CString::new(format!(".spaceterm-{:016x}", u64::from_be_bytes(nonce)))
            .map_err(|_| ServiceFailure::InvalidRecord)?;
        let fd = unsafe {
            libc::openat(
                self.parent.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(ServiceFailure::StorageUnavailable);
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            // fchmod avoids an unusually restrictive umask producing unreadable service content.
            if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
                return Err(ServiceFailure::StorageUnavailable);
            }
            check_acl(&file)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServiceFailure::StorageUnavailable)?;
            self.check_original(original.as_ref())?;
            if unsafe {
                libc::renameat(
                    self.parent.as_raw_fd(),
                    temporary.as_ptr(),
                    self.parent.as_raw_fd(),
                    self.name.as_ptr(),
                )
            } != 0
            {
                return Err(ServiceFailure::StorageUnavailable);
            }
            self.parent
                .sync_all()
                .map_err(|_| ServiceFailure::Indeterminate)
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(self.parent.as_raw_fd(), temporary.as_ptr(), 0);
            }
        }
        result
    }
    fn check_original(&self, original: Option<&File>) -> Result<(), ServiceFailure> {
        let current = self.existing()?;
        match (original, current.as_ref()) {
            (None, None) => Ok(()),
            (Some(a), Some(b)) => {
                let a = a
                    .metadata()
                    .map_err(|_| ServiceFailure::StorageUnavailable)?;
                let b = b
                    .metadata()
                    .map_err(|_| ServiceFailure::StorageUnavailable)?;
                if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
                    Ok(())
                } else {
                    Err(ServiceFailure::RequestConflict)
                }
            }
            _ => Err(ServiceFailure::RequestConflict),
        }
    }
    pub(super) fn remove(&self) -> Result<(), ServiceFailure> {
        check_directory(&self.parent)?;
        let Some(original) = self.existing()? else {
            return Ok(());
        };
        self.check_original(Some(&original))?;
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0) } != 0 {
            return Err(ServiceFailure::StorageUnavailable);
        }
        self.parent
            .sync_all()
            .map_err(|_| ServiceFailure::Indeterminate)
    }
}
fn check_directory(file: &File) -> Result<(), ServiceFailure> {
    let facts = file
        .metadata()
        .map_err(|_| ServiceFailure::StorageUnavailable)?;
    if !facts.is_dir() || facts.uid() != unsafe { libc::geteuid() } || facts.mode() & 0o022 != 0 {
        return Err(ServiceFailure::UnsafeStorage);
    }
    check_acl(file)
}

#[cfg(target_os = "linux")]
fn check_acl(_: &File) -> Result<(), ServiceFailure> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_acl(file: &File) -> Result<(), ServiceFailure> {
    use std::ffi::c_void;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut c_void;
        fn acl_get_entry(
            acl: *mut c_void,
            entry_id: libc::c_int,
            entry: *mut *mut c_void,
        ) -> libc::c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut libc::c_int) -> libc::c_int;
        fn acl_free(acl: *mut c_void) -> libc::c_int;
    }
    // Deny-only ACLs are normal on macOS home directories. They grant no access.
    // Refuse all allow entries rather than interpret identity or permission masks.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if acl.is_null() {
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(ServiceFailure::StorageUnavailable)
        };
    }
    let result = (|| {
        let mut entry = std::ptr::null_mut();
        let mut selector = 0;
        loop {
            let status = unsafe { acl_get_entry(acl, selector, &mut entry) };
            if status != 0 {
                return if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                    Ok(())
                } else {
                    Err(ServiceFailure::StorageUnavailable)
                };
            }
            let mut tag = 0;
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
                return Err(ServiceFailure::StorageUnavailable);
            }
            if tag != 2 {
                return Err(ServiceFailure::UnsafeStorage);
            }
            selector = -1;
        }
    })();
    if unsafe { acl_free(acl) } != 0 {
        return Err(ServiceFailure::StorageUnavailable);
    }
    result
}

pub(super) fn runner() -> Arc<dyn ServiceCommandRunner> {
    Arc::new(NativeServiceCommandRunner)
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    //! Injected native boundaries. No fixture executes a service-manager utility.
    #![allow(missing_docs)]
    use super::*;
    use crate::account_service::{self as service, LingerState, SupervisorState};
    use std::{collections::VecDeque, sync::Mutex};

    pub struct CommandReply {
        code: i32,
        bytes: Vec<u8>,
        stderr: Vec<u8>,
    }
    impl CommandReply {
        pub fn success(bytes: &[u8]) -> Self {
            Self::exit(0, bytes)
        }
        pub fn exit(code: i32, bytes: &[u8]) -> Self {
            Self::exit_with_stderr(code, bytes, b"")
        }
        pub fn exit_with_stderr(code: i32, bytes: &[u8], stderr: &[u8]) -> Self {
            Self {
                code,
                bytes: bytes.to_vec(),
                stderr: stderr.to_vec(),
            }
        }
    }
    pub struct RecordingRunner {
        replies: Mutex<VecDeque<CommandReply>>,
        calls: Mutex<Vec<ServiceCommand>>,
    }
    impl RecordingRunner {
        pub fn new(replies: impl IntoIterator<Item = CommandReply>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
        pub fn arguments(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| {
                    call.arguments
                        .iter()
                        .map(|arg| arg.to_str().unwrap().to_owned())
                        .collect()
                })
                .collect()
        }
        pub fn programs(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| call.program.into())
                .collect()
        }
        pub fn environments(&self) -> Vec<Vec<(String, String)>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| {
                    call.environment
                        .iter()
                        .map(|(k, v)| (k.to_str().unwrap().into(), v.to_str().unwrap().into()))
                        .collect()
                })
                .collect()
        }
    }
    impl ServiceCommandRunner for RecordingRunner {
        fn run(&self, command: ServiceCommand) -> ServiceFuture<'_, ServiceCommandOutput> {
            self.calls.lock().unwrap().push(command);
            Box::pin(async move {
                let reply = self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or(ServiceFailure::SupervisorUnavailable)?;
                if reply.bytes.len().saturating_add(reply.stderr.len()) > OUTPUT_LIMIT {
                    return Err(ServiceFailure::LimitExceeded);
                }
                Ok(ServiceCommandOutput {
                    code: reply.code,
                    bytes: reply.bytes,
                    stderr: reply.stderr,
                })
            })
        }
    }
    pub fn launch_agent(
        identity: BuildIdentity,
        desired: crate::startup::DesiredServerState,
        executable: &str,
    ) -> Result<String, ServiceFailure> {
        service::launchd::render(identity, desired, Path::new(executable))
    }
    pub fn user_unit(
        identity: BuildIdentity,
        desired: crate::startup::DesiredServerState,
        executable: &str,
    ) -> Result<String, ServiceFailure> {
        service::systemd::render(identity, desired, Path::new(executable))
    }
    struct UnavailableInstallation;
    impl ServiceInstallations for UnavailableInstallation {
        fn approved_executable(&self, _: &RegisteredServer) -> Result<PathBuf, ServiceFailure> {
            Err(ServiceFailure::ArtifactRejected)
        }
        fn uninstall(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
            Err(ServiceFailure::ArtifactRejected)
        }
    }
    fn account(uid: u32, identity: BuildIdentity, home: PathBuf) -> Account {
        Account {
            key: AccountBuildKey {
                account_uid: uid,
                build_identity: identity,
            },
            home,
            runtime: PathBuf::from(format!("/run/user/{uid}")),
        }
    }
    pub async fn launch_start(
        runner: Arc<dyn ServiceCommandRunner>,
        uid: u32,
        identity: BuildIdentity,
        path: &str,
    ) -> Result<(), ServiceFailure> {
        let path = Path::new(path);
        let home = path.ancestors().nth(3).unwrap_or(Path::new("/")).to_owned();
        service::LaunchdServiceManager {
            account: account(uid, identity, home),
            runner,
            installations: Arc::new(UnavailableInstallation),
        }
        .start_service()
        .await
    }
    pub async fn user_commands(
        runner: Arc<dyn ServiceCommandRunner>,
        uid: u32,
        identity: BuildIdentity,
    ) -> Result<(), ServiceFailure> {
        use service::systemd::Operation::*;
        let manager = service::SystemdServiceManager {
            account: account(uid, identity, PathBuf::from("/home")),
            runner,
            installations: Arc::new(UnavailableInstallation),
            configuration_root: std::sync::OnceLock::new(),
        };
        for operation in [Reload, Enable, Disable, Start, Stop, Show, UnitPath] {
            manager.call(operation).await?.success()?;
        }
        Ok(())
    }
    pub fn systemd_state(code: i32, bytes: &[u8]) -> Result<SupervisorState, ServiceFailure> {
        service::systemd::state(code, bytes)
    }
    pub fn launchd_state(bytes: &[u8]) -> Result<SupervisorState, ServiceFailure> {
        service::launchd::state(bytes)
    }
    pub async fn linger(
        runner: Arc<dyn ServiceCommandRunner>,
        uid: u32,
    ) -> Result<LingerState, ServiceFailure> {
        service::linger::observe(runner.as_ref(), uid).await
    }
    pub fn linger_request(uid: u32) -> service::LingerRequest {
        service::LingerRequest { uid }
    }
    pub fn sudo_authorization(
        runner: Arc<dyn ServiceCommandRunner>,
        mode: service::SudoMode,
    ) -> service::SudoLingerAuthorization {
        service::SudoLingerAuthorization {
            runner,
            mode,
            uid: Some(1000),
        }
    }
    pub fn pkexec_authorization(
        runner: Arc<dyn ServiceCommandRunner>,
    ) -> service::PkexecLingerAuthorization {
        service::PkexecLingerAuthorization {
            runner,
            uid: Some(1000),
        }
    }
    pub fn current_uid() -> u32 {
        unsafe { libc::geteuid() }
    }
    pub struct NativeManagers {
        pub launchd: service::LaunchdServiceManager,
        pub systemd: service::SystemdServiceManager,
        registration: RegisteredServer,
    }
    impl NativeManagers {
        pub fn registration(&self) -> &RegisteredServer {
            &self.registration
        }
    }
    pub fn stopped_managers(
        root: &Path,
        runner: Arc<dyn ServiceCommandRunner>,
    ) -> Result<NativeManagers, ServiceFailure> {
        managers(
            root,
            runner,
            Arc::new(UnavailableInstallation),
            crate::startup::DesiredServerState::Stopped,
        )
    }
    pub fn managers(
        root: &Path,
        runner: Arc<dyn ServiceCommandRunner>,
        installations: Arc<dyn ServiceInstallations>,
        desired: crate::startup::DesiredServerState,
    ) -> Result<NativeManagers, ServiceFailure> {
        let root = root
            .canonicalize()
            .map_err(|_| ServiceFailure::StorageUnavailable)?;
        let fixture =
            service::test_support::AccountServiceFixture::new(&root, BuildIdentity::Development)?;
        let registration = fixture.initialize(desired)?;
        let mut account = account(current_uid(), BuildIdentity::Development, root.to_owned());
        account.runtime = root.join("manager-runtime");
        crate::private_filesystem::Directory::create(&account.runtime)?;
        Ok(NativeManagers {
            launchd: service::LaunchdServiceManager {
                account: account.clone(),
                runner: runner.clone(),
                installations: installations.clone(),
            },
            systemd: service::SystemdServiceManager {
                account,
                runner,
                installations,
                configuration_root: std::sync::OnceLock::new(),
            },
            registration,
        })
    }
    pub async fn fixture_command(
        script: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, ServiceFailure> {
        let mut command = ServiceCommand::new("/bin/sh", ["-c".into(), script.into()]);
        command.timeout = timeout;
        NativeServiceCommandRunner
            .run(command)
            .await
            .map(|output| output.bytes)
    }
    pub fn publish_configuration(root: &Path, contents: &[u8]) -> Result<(), ServiceFailure> {
        ConfigurationFile::open(root, &["services"], "fixture.service", true)?
            .ok_or(ServiceFailure::Missing)?
            .publish(contents)
    }
    pub fn remove_configuration(root: &Path) -> Result<(), ServiceFailure> {
        if let Some(file) = ConfigurationFile::open(root, &["services"], "fixture.service", false)?
        {
            file.remove()?;
        }
        Ok(())
    }
}
