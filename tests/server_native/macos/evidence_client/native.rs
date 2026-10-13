use super::decimal;
use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    protected_directory::{NativeDirectoryProtection, RetainedProtectedDirectory},
    root_protection::NativeRootProtection,
};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[path = "peer.rs"]
mod peer;

const ROOT: &str = "/Library/Application Support/SpaceTerm Native Evidence 520";
const CERTIFICATE: &str = "9e07cd5c15c5c862a21b93ea55d4303bc19b7366";
fn io_failure(_: std::io::Error) -> NativeFailure {
    NativeFailure::IoUnavailable
}

pub(super) fn monotonic_ns() -> Result<u64, NativeFailure> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC writes one initialized timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 || now.tv_sec < 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    Ok(now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64)
}

fn bounded_status(
    mut child: std::process::Child,
    deadline: Instant,
) -> Result<std::process::ExitStatus, NativeFailure> {
    loop {
        if let Some(status) = child.try_wait().map_err(io_failure)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    child.kill().map_err(|_| NativeFailure::CleanupUnresolved)?;
    let retirement = Instant::now() + Duration::from_millis(250);
    while Instant::now() < retirement {
        if child
            .try_wait()
            .map_err(|_| NativeFailure::CleanupUnresolved)?
            .is_some()
        {
            return Err(NativeFailure::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Err(NativeFailure::CleanupUnresolved)
}

fn verify_code(path: &Path, identifier: &str) -> Result<(), NativeFailure> {
    let parent = path.parent().ok_or(NativeFailure::InvalidRecord)?;
    let protection = NativeRootProtection;
    let retained = RetainedProtectedDirectory::open(parent, &protection)
        .map_err(|_| NativeFailure::CustodyUnavailable)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_failure)?;
    let before = file.metadata().map_err(io_failure)?;
    if !before.is_file() || before.nlink() != 1 {
        return Err(NativeFailure::ArtifactRejected);
    }
    protection
        .verify_regular_file(&file)
        .map_err(|_| NativeFailure::CustodyUnavailable)?;
    let requirement =
        format!("=identifier \"{identifier}\" and certificate leaf = H\"{CERTIFICATE}\"");
    let status = Command::new("/usr/bin/codesign")
        .args([
            "--verify",
            "--strict",
            "--all-architectures",
            "-R",
            &requirement,
        ])
        .arg(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(io_failure)?;
    let status = bounded_status(status, Instant::now() + Duration::from_secs(5))?;
    retained
        .verify()
        .map_err(|_| NativeFailure::CustodyUnavailable)?;
    protection
        .verify_regular_file(&file)
        .map_err(|_| NativeFailure::CustodyUnavailable)?;
    let after = file.metadata().map_err(io_failure)?;
    let edge = fs::symlink_metadata(path).map_err(io_failure)?;
    if !status.success()
        || (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec(),
            before.size(),
        ) != (
            after.dev(),
            after.ino(),
            after.ctime(),
            after.ctime_nsec(),
            after.size(),
        )
        || (edge.dev(), edge.ino()) != (before.dev(), before.ino())
    {
        return Err(NativeFailure::ArtifactRejected);
    }
    Ok(())
}

pub(super) fn exec_probe(
    path: &str,
    arguments: &[String],
    environment: Vec<(OsString, OsString)>,
) -> Result<(), NativeFailure> {
    verify_code(
        Path::new(path),
        "io.github.sadiksaifi.spaceterm-development.server.native-evidence.probe",
    )?;
    exec_verified_probe(path, arguments, environment)
}

fn probe_descriptors(arguments: &[String]) -> Result<Vec<i32>, NativeFailure> {
    // Exec preserves the originating shell as parent and leaves mode bits unread.
    match arguments {
        [mode, context, fd] if mode == "--environment" => {
            if !(1..=2).contains(&decimal(context, 2)?) {
                return Err(NativeFailure::InvalidRecord);
            }
            Ok(vec![decimal(fd, i32::MAX as u64)? as i32])
        }
        [mode, case, context, deadline, target, rest @ ..] if mode == "--run" => {
            let case = decimal(case, 13)?;
            if case == 0
                || !(1..=2).contains(&decimal(context, 2)?)
                || (case == 13) != (rest.len() == 1)
                || (case != 13 && !rest.is_empty())
            {
                return Err(NativeFailure::InvalidRecord);
            }
            decimal(deadline, u64::MAX)?;
            let target = decimal(target, i32::MAX as u64)? as i32;
            if target < 3 {
                return Err(NativeFailure::InvalidRecord);
            }
            let mut descriptors = vec![target];
            if let Some(mode) = rest.first() {
                let mode = decimal(mode, i32::MAX as u64)? as i32;
                if mode == target {
                    return Err(NativeFailure::InvalidRecord);
                }
                descriptors.push(mode);
            }
            Ok(descriptors)
        }
        _ => Err(NativeFailure::InvalidRecord),
    }
}

fn exec_verified_probe(
    path: &str,
    arguments: &[String],
    environment: Vec<(OsString, OsString)>,
) -> Result<(), NativeFailure> {
    let descriptors = probe_descriptors(arguments)?;
    for fd in descriptors {
        // SAFETY: fcntl operates on an existing caller-owned descriptor. Only the
        // two public metadata descriptors are permitted to cross this one exec.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } != 0 {
            return Err(NativeFailure::InvalidRecord);
        }
    }
    let error = Command::new(path)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .exec();
    Err(io_failure(error))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn run_id(text: &str) -> Result<[u8; 16], NativeFailure> {
    if text.len() != 32
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(NativeFailure::InvalidRecord);
    }
    let mut result = [0; 16];
    for (byte, pair) in result.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        let text = std::str::from_utf8(pair).map_err(|_| NativeFailure::InvalidRecord)?;
        *byte = u8::from_str_radix(text, 16).map_err(|_| NativeFailure::InvalidRecord)?;
    }
    if result == [0; 16] {
        return Err(NativeFailure::InvalidRecord);
    }
    Ok(result)
}

fn read_line(
    stream: &mut UnixStream,
    deadline: Instant,
    signals: &AttemptSignals,
) -> Result<String, NativeFailure> {
    let mut line = Vec::with_capacity(256);
    while line.len() < 256 {
        signals.check(deadline)?;
        stream
            .set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1))
                    .min(Duration::from_millis(20)),
            ))
            .map_err(io_failure)?;
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(1) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            _ => return Err(NativeFailure::InvalidRecord),
        }
        if byte[0] == b'\n' {
            return String::from_utf8(line).map_err(|_| NativeFailure::InvalidRecord);
        }
        if byte[0] != b' '
            && byte[0] != b'='
            && byte[0] != b'-'
            && !byte[0].is_ascii_lowercase()
            && !byte[0].is_ascii_digit()
        {
            return Err(NativeFailure::InvalidRecord);
        }
        line.push(byte[0]);
    }
    Err(NativeFailure::LimitExceeded)
}
fn send(
    stream: &mut UnixStream,
    record: &str,
    deadline: Instant,
    signals: &AttemptSignals,
) -> Result<(), NativeFailure> {
    signals.check(deadline)?;
    if record.len() > 256 {
        return Err(NativeFailure::LimitExceeded);
    }
    stream
        .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
        .map_err(io_failure)?;
    stream.write_all(record.as_bytes()).map_err(io_failure)
}
fn accept(
    listener: &UnixListener,
    deadline: Instant,
    signals: &AttemptSignals,
) -> Result<UnixStream, NativeFailure> {
    listener.set_nonblocking(true).map_err(io_failure)?;
    loop {
        signals.check(deadline)?;
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(_) => return Err(NativeFailure::TimedOut),
        }
    }
}

struct Attempt {
    target: &'static str,
    nonce: String,
    armed: bool,
}
impl Attempt {
    fn consume(&mut self, record: &str) -> Result<bool, NativeFailure> {
        if record == format!("armed target={} nonce={}", self.target, self.nonce) && !self.armed {
            self.armed = true;
            return Ok(false);
        }
        if record.starts_with("refused ") {
            return Err(NativeFailure::OutcomePending);
        }
        if self.armed
            && record
                == format!(
                    "receipt target={} status=accepted nonce={}",
                    self.target, self.nonce
                )
        {
            return Ok(true);
        }
        Err(NativeFailure::InvalidRecord)
    }
}

struct OwnedEntry {
    path: PathBuf,
    device: u64,
    inode: u64,
    directory: bool,
    removed: bool,
}
impl OwnedEntry {
    fn retain(path: PathBuf, directory: bool) -> Result<Self, NativeFailure> {
        let metadata = fs::symlink_metadata(&path).map_err(io_failure)?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            directory,
            removed: false,
        })
    }
    fn remove(mut self) -> Result<(), NativeFailure> {
        self.remove_entry()
    }
    fn remove_entry(&mut self) -> Result<(), NativeFailure> {
        if self.removed {
            return Ok(());
        }
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| NativeFailure::CleanupUnresolved)?;
        if (metadata.dev(), metadata.ino()) != (self.device, self.inode)
            || metadata.file_type().is_symlink()
        {
            return Err(NativeFailure::CleanupUnresolved);
        }
        if self.directory {
            fs::remove_dir(&self.path)
        } else {
            fs::remove_file(&self.path)
        }
        .map_err(|_| NativeFailure::CleanupUnresolved)?;
        self.removed = true;
        Ok(())
    }
}
impl Drop for OwnedEntry {
    fn drop(&mut self) {
        // A panic still cannot delete a replacement. Normal exits propagate
        // cleanup uncertainty explicitly through targets().
        let _ = self.remove_entry();
    }
}

pub(super) fn input(arguments: &[String]) -> Result<(), NativeFailure> {
    let [mode, run, case, output] = arguments else {
        return Err(NativeFailure::InvalidRecord);
    };
    let case = decimal(case, 13)?;
    if mode != "input"
        || !matches!(case, 1 | 2 | 3 | 6 | 9 | 10 | 11 | 12 | 13)
        || !Path::new(output).is_absolute()
    {
        return Err(NativeFailure::InvalidRecord);
    }
    let mut bytes = [0; 4144];
    bytes[..4].copy_from_slice(&2_u32.to_ne_bytes());
    bytes[16..32].copy_from_slice(&run_id(run)?);
    getrandom::fill(&mut bytes[32..48]).map_err(|_| NativeFailure::NativeUnavailable)?;
    if bytes[32..48] == [0; 16] {
        return Err(NativeFailure::NativeUnavailable);
    }
    if case == 2 {
        // The owner supplies the explicitly selected volume through a private
        // descriptor, never in arguments or evidence. Empty input means unrun.
        let mut volume = Vec::with_capacity(4096);
        std::io::stdin()
            .take(4096)
            .read_to_end(&mut volume)
            .map_err(io_failure)?;
        if volume.len() > 4095 || volume.contains(&0) || std::str::from_utf8(&volume).is_err() {
            return Err(NativeFailure::InvalidRecord);
        }
        bytes[48..48 + volume.len()].copy_from_slice(&volume);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(output)
        .map_err(|_| NativeFailure::UnsafeNativeState)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_failure)?;
    Ok(())
}

struct AttemptSignals {
    blocked: libc::sigset_t,
    original: libc::sigset_t,
}
impl AttemptSignals {
    fn block() -> Result<Self, NativeFailure> {
        // SAFETY: these initialized sets belong to this one connector thread.
        unsafe {
            let mut blocked = std::mem::zeroed();
            let mut original = std::mem::zeroed();
            if libc::sigemptyset(&mut blocked) != 0 {
                return Err(NativeFailure::NativeUnavailable);
            }
            for signal in [libc::SIGINT, libc::SIGHUP, libc::SIGTERM] {
                if libc::sigaddset(&mut blocked, signal) != 0 {
                    return Err(NativeFailure::NativeUnavailable);
                }
            }
            if libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut original) != 0 {
                return Err(NativeFailure::NativeUnavailable);
            }
            Ok(Self { blocked, original })
        }
    }
    fn cancellation(&self) -> Result<(), NativeFailure> {
        // The connector is single-threaded. All its termination signals remain
        // blocked until owned entries have been removed, including on refusal.
        // SAFETY: pending and received are sized native signal outputs.
        unsafe {
            let mut pending = std::mem::zeroed();
            if libc::sigpending(&mut pending) != 0 {
                return Err(NativeFailure::NativeUnavailable);
            }
            if [libc::SIGINT, libc::SIGHUP, libc::SIGTERM]
                .into_iter()
                .any(|signal| libc::sigismember(&pending, signal) == 1)
            {
                let mut received = 0;
                if libc::sigwait(&self.blocked, &mut received) != 0 {
                    return Err(NativeFailure::NativeUnavailable);
                }
                return Err(NativeFailure::AuthorizationCancelled);
            }
        }
        Ok(())
    }
    fn check(&self, deadline: Instant) -> Result<(), NativeFailure> {
        self.cancellation()?;
        if Instant::now() >= deadline {
            Err(NativeFailure::TimedOut)
        } else {
            Ok(())
        }
    }
}
impl Drop for AttemptSignals {
    fn drop(&mut self) {
        // SAFETY: restore only this thread's prior mask after target cleanup.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.original, std::ptr::null_mut());
        }
    }
}

trait TargetPlatform {
    type Peer;
    fn verify(&self, app: &Path) -> Result<(), NativeFailure>;
    fn launch(
        &self,
        app: &Path,
        run: &str,
        socket: &Path,
        deadline: Instant,
    ) -> Result<(), NativeFailure>;
    fn authenticate(&self, channel: &UnixStream, pid: i32) -> Result<Self::Peer, NativeFailure>;
    fn recheck(&self, peer: &Self::Peer, channel: &UnixStream) -> Result<(), NativeFailure>;
}
struct NativeTargets;
impl TargetPlatform for NativeTargets {
    type Peer = peer::Peer;
    fn verify(&self, app: &Path) -> Result<(), NativeFailure> {
        verify_code(
            &app.join("Contents/MacOS/spaceterm-evidence-companion"),
            "io.github.sadiksaifi.spaceterm-development.server.native-evidence.companion",
        )
    }
    fn launch(
        &self,
        app: &Path,
        run: &str,
        socket: &Path,
        deadline: Instant,
    ) -> Result<(), NativeFailure> {
        let mut command = Command::new("/usr/bin/open");
        command
            .args(["-n", "-a"])
            .arg(app)
            .args(["--args", "--run-id", run, "--controller-socket"])
            .arg(socket)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: the child resets its own inherited mask with async-signal-safe
        // calls. The connector alone owns cancellation and scoped cleanup.
        unsafe {
            command.pre_exec(|| {
                let mut empty = std::mem::zeroed();
                if libc::sigemptyset(&mut empty) != 0
                    || libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let status = bounded_status(command.spawn().map_err(io_failure)?, deadline)?;
        if status.success() {
            Ok(())
        } else {
            Err(NativeFailure::NativeUnavailable)
        }
    }
    fn authenticate(&self, channel: &UnixStream, pid: i32) -> Result<Self::Peer, NativeFailure> {
        peer::Peer::authenticate(channel, pid)
    }
    fn recheck(&self, peer: &Self::Peer, channel: &UnixStream) -> Result<(), NativeFailure> {
        peer.recheck(channel)
    }
}

pub(super) fn targets(arguments: &[String]) -> Result<(), NativeFailure> {
    targets_with_platform(arguments, &NativeTargets, Duration::from_secs(60))
}

fn targets_with_platform(
    arguments: &[String],
    platform: &impl TargetPlatform,
    wait: Duration,
) -> Result<(), NativeFailure> {
    let signals = AttemptSignals::block()?;
    let mut entries = Vec::new();
    let result = target_attempt(arguments, &mut entries, platform, &signals, wait);
    let mut cleanup = Ok(());
    for entry in entries.into_iter().rev() {
        if entry.remove().is_err() {
            cleanup = Err(NativeFailure::CleanupUnresolved);
        }
    }
    // A native refusal can return before the next wait. Consume pending
    // cancellation before restoring the mask; cleanup uncertainty still wins.
    let cancellation = signals.cancellation();
    cleanup?;
    cancellation?;
    result
}

fn target_attempt(
    arguments: &[String],
    entries: &mut Vec<OwnedEntry>,
    platform: &impl TargetPlatform,
    signals: &AttemptSignals,
    wait: Duration,
) -> Result<(), NativeFailure> {
    let [mode, run, case, output] = arguments else {
        return Err(NativeFailure::InvalidRecord);
    };
    if mode != "targets" {
        return Err(NativeFailure::InvalidRecord);
    }
    let run_bytes = run_id(run)?;
    let case_number = decimal(case, 8)? as u32;
    let target = match case_number {
        4 => "capture",
        5 => "accessibility",
        7 => "url",
        8 => "apple-event",
        _ => return Err(NativeFailure::InvalidRecord),
    };
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| NativeFailure::NativeUnavailable)?;
    if nonce == [0; 16] || !Path::new(output).is_absolute() {
        return Err(NativeFailure::InvalidRecord);
    }
    let nonce_text = hex(&nonce);
    let directory = PathBuf::from(format!("/private/tmp/st520-{run}-{nonce_text}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| NativeFailure::UnsafeNativeState)?;
    entries.push(OwnedEntry::retain(directory.clone(), true)?);
    let socket = directory.join("companion.sock");
    let listener = UnixListener::bind(&socket).map_err(io_failure)?;
    entries.push(OwnedEntry::retain(socket.clone(), false)?);
    // Reserve every account-owned output before any LaunchServices or target effect.
    let receipt_path = PathBuf::from(format!(
        "/private/tmp/spaceterm-native-520-{run}.{case_number}.receipt.sock"
    ));
    let receipt = if matches!(case_number, 5 | 7) {
        Some(UnixListener::bind(&receipt_path).map_err(|_| NativeFailure::UnsafeNativeState)?)
    } else {
        None
    };
    // The probe's existing receipt pathname is fixed by its target ABI. Keep it
    // exclusive and identity-bound; a concurrent same-case attempt refuses.
    if receipt.is_some() {
        entries.push(OwnedEntry::retain(receipt_path, false)?);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(output)
        .map_err(|_| NativeFailure::UnsafeNativeState)?;
    let app = Path::new(ROOT).join("SpaceTerm Evidence Companion.app");
    platform.verify(&app)?;
    let deadline = Instant::now() + wait;
    signals.check(deadline)?;
    platform.launch(&app, run, &socket, deadline)?;
    signals.check(deadline)?;
    let mut channel = accept(&listener, deadline, signals)?;
    let hello = read_line(&mut channel, deadline, signals)?;
    let pid = hello
        .strip_prefix(&format!("hello schema=1 run={run} pid="))
        .ok_or(NativeFailure::InvalidRecord)?;
    let pid = decimal(pid, i32::MAX as u64)? as i32;
    let peer = platform.authenticate(&channel, pid)?;
    send(
        &mut channel,
        &format!("welcome schema=1 run={run}\n"),
        deadline,
        signals,
    )?;
    send(&mut channel, "run state=active\n", deadline, signals)?;
    let window = read_line(&mut channel, deadline, signals)?
        .strip_prefix("targets capture-window=")
        .ok_or(NativeFailure::InvalidRecord)
        .and_then(|number| decimal(number, u32::MAX as u64))? as u32;
    send(
        &mut channel,
        &format!("expect target={target} nonce={nonce_text} deadline-ms=60000\n"),
        deadline,
        signals,
    )?;
    let mut attempt = Attempt {
        target,
        nonce: nonce_text,
        armed: false,
    };
    attempt.consume(&read_line(&mut channel, deadline, signals)?)?;
    if !attempt.armed {
        return Err(NativeFailure::InvalidRecord);
    }
    platform.recheck(&peer, &channel)?;
    let mut bytes = [0; 4144];
    bytes[..4].copy_from_slice(&2_u32.to_ne_bytes());
    bytes[4..8].copy_from_slice(&(pid as u32).to_ne_bytes());
    bytes[8..12].copy_from_slice(&window.to_ne_bytes());
    bytes[12..16].copy_from_slice(&std::process::id().to_ne_bytes());
    bytes[16..32].copy_from_slice(&run_bytes);
    bytes[32..48].copy_from_slice(&nonce);
    output
        .write_all(&bytes)
        .and_then(|()| output.sync_all())
        .map_err(io_failure)?;
    println!("phase=targets classification=armed capability={case_number}");
    // Capture draws when armed and has no receipt. A successful open/AX call
    // cannot satisfy a URL/AX receipt.
    let effect = if case_number == 4 {
        while Instant::now() < deadline {
            signals.check(deadline)?;
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    } else {
        let mut request_peer = if let Some(listener) = &receipt {
            Some(accept(listener, deadline, signals)?)
        } else {
            None
        };
        let mut request = [0; 45];
        if let Some(stream) = &mut request_peer {
            let mut received = Vec::with_capacity(45);
            while received.len() < 45 {
                signals.check(deadline)?;
                stream
                    .set_read_timeout(Some(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(20)),
                    ))
                    .map_err(io_failure)?;
                let mut byte = [0];
                match stream.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => received.push(byte[0]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => return Err(NativeFailure::IoUnavailable),
                }
            }
            let mut expected = [0; 44];
            expected[..4].copy_from_slice(b"SCTR");
            expected[7] = 1;
            expected[11] = case_number as u8;
            expected[12..28].copy_from_slice(&run_bytes);
            expected[28..44].copy_from_slice(&nonce);
            if received != expected {
                return Err(NativeFailure::InvalidRecord);
            }
            request[..44].copy_from_slice(&expected);
        }
        if !attempt.consume(&read_line(&mut channel, deadline, signals)?)? {
            return Err(NativeFailure::OutcomePending);
        }
        platform.recheck(&peer, &channel)?;
        if let Some(stream) = &mut request_peer {
            stream
                .set_write_timeout(Some(deadline.saturating_duration_since(Instant::now())))
                .map_err(io_failure)?;
            stream.write_all(&request[..44]).map_err(io_failure)?;
            stream.shutdown(Shutdown::Write).map_err(io_failure)?;
            platform.recheck(&peer, &channel)?;
        }
        Ok(())
    };
    // This stops only the existing target run. It does not claim the companion
    // process exited or authorize removal of the protected product installation.
    let _ = send(
        &mut channel,
        "run state=stopped\n",
        Instant::now() + Duration::from_secs(1),
        signals,
    );
    drop(channel);
    drop(listener);
    drop(receipt);
    effect
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_target_inputs_are_exclusive_and_cannot_arm_gui_effects() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("input");
        let arguments = [
            "input".to_owned(),
            "1".repeat(32),
            "12".to_owned(),
            path.to_str().unwrap().to_owned(),
        ];
        input(&arguments).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 4144);
        assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), 2);
        assert_eq!(&bytes[4..16], &[0; 12]);
        assert_eq!(input(&arguments), Err(NativeFailure::UnsafeNativeState));
        let mut gui = arguments;
        gui[2] = "5".to_owned();
        assert_eq!(input(&gui), Err(NativeFailure::InvalidRecord));
    }

    #[test]
    fn receipt_requires_the_matching_armed_target_and_nonce() {
        let mut attempt = Attempt {
            target: "url",
            nonce: "1".repeat(32),
            armed: false,
        };
        assert_eq!(
            attempt.consume(&format!(
                "receipt target=url status=accepted nonce={}",
                "1".repeat(32)
            )),
            Err(NativeFailure::InvalidRecord)
        );
        attempt
            .consume(&format!("armed target=url nonce={}", "1".repeat(32)))
            .unwrap();
        assert_eq!(
            attempt.consume(&format!(
                "receipt target=url status=accepted nonce={}",
                "2".repeat(32)
            )),
            Err(NativeFailure::InvalidRecord)
        );
        assert!(
            attempt
                .consume(&format!(
                    "receipt target=url status=accepted nonce={}",
                    "1".repeat(32)
                ))
                .unwrap()
        );
    }
    #[test]
    fn refusal_never_starts_a_target_probe() {
        let mut attempt = Attempt {
            target: "accessibility",
            nonce: "1".repeat(32),
            armed: false,
        };
        assert_eq!(
            attempt.consume(
                "refused target=accessibility nonce=11111111111111111111111111111111 reason=full"
            ),
            Err(NativeFailure::OutcomePending)
        );
        assert!(!attempt.armed);
    }
    #[test]
    fn socket_cleanup_preserves_a_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("socket");
        let old = UnixListener::bind(&path).unwrap();
        let entry = OwnedEntry::retain(path.clone(), false).unwrap();
        fs::remove_file(&path).unwrap();
        let _replacement = UnixListener::bind(&path).unwrap();
        assert_eq!(entry.remove(), Err(NativeFailure::CleanupUnresolved));
        drop(old);
        assert!(path.exists());
    }
}

#[cfg(test)]
mod regressions {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn verified_exec_preserves_target_and_mode_descriptors_and_pid() {
        for case in [5, 13] {
            let temporary = tempfile::tempdir().unwrap();
            let script = temporary.path().join("probe");
            fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$$\"\n/bin/cat <&3\nif [ \"$2\" = 13 ]; then /bin/cat <&4; fi\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            let targets = temporary.path().join("targets");
            let modes = temporary.path().join("modes");
            fs::write(&targets, b"target\n").unwrap();
            fs::write(&modes, b"11\n").unwrap();
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "client::native::regressions::exec_child",
                    "--nocapture",
                ])
                .env_clear()
                .env("E4_EXEC_SCRIPT", &script)
                .env("E4_EXEC_TARGETS", &targets)
                .env("E4_EXEC_MODES", &modes)
                .env("E4_EXEC_CASE", case.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let pid = child.id();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains(&format!("{pid}\ntarget\n")), "{text}");
            assert_eq!(text.contains("11\n"), case == 13, "{text}");
        }
    }

    #[test]
    fn exec_child() {
        let Some(script) = std::env::var_os("E4_EXEC_SCRIPT") else {
            assert_eq!(probe_descriptors(&[]), Err(NativeFailure::InvalidRecord));
            return;
        };
        let target = fs::File::open(std::env::var_os("E4_EXEC_TARGETS").unwrap()).unwrap();
        let mode = fs::File::open(std::env::var_os("E4_EXEC_MODES").unwrap()).unwrap();
        // SAFETY: the child owns these descriptors; duplicate them onto the two
        // fixed test slots before exercising the real exec operation.
        unsafe {
            assert!(libc::dup2(target.as_raw_fd(), 3) >= 0);
            assert!(libc::dup2(mode.as_raw_fd(), 4) >= 0);
            assert_eq!(libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC), 0);
            assert_eq!(libc::fcntl(4, libc::F_SETFD, libc::FD_CLOEXEC), 0);
        }
        std::mem::forget(target);
        std::mem::forget(mode);
        let case = std::env::var("E4_EXEC_CASE").unwrap();
        let mut arguments = vec![
            "--run".into(),
            case.clone(),
            "1".into(),
            "10".into(),
            "3".into(),
        ];
        if case == "13" {
            arguments.push("4".into());
        }
        exec_verified_probe(script.to_str().unwrap(), &arguments, Vec::new()).unwrap();
    }

    #[test]
    fn malformed_verified_probe_arguments_reject_without_panicking() {
        for arguments in [
            vec![],
            vec!["--run".into()],
            vec!["--environment".into(), "1".into()],
            vec![
                "--run".into(),
                "5".into(),
                "1".into(),
                "10".into(),
                "bad".into(),
            ],
        ] {
            assert_eq!(
                probe_descriptors(&arguments),
                Err(NativeFailure::InvalidRecord)
            );
        }
    }

    #[test]
    fn refusal_and_unsolicited_requests_clean_attempt_entries_before_retry() {
        for record in [
            "refused target=url reason=full",
            "request action=probe capability=7",
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let directory = temporary.path().join("attempt");
            let socket = directory.join("socket");
            let result = (|| {
                fs::create_dir(&directory).unwrap();
                let _directory = OwnedEntry::retain(directory.clone(), true)?;
                let _listener = UnixListener::bind(&socket).unwrap();
                let _socket = OwnedEntry::retain(socket.clone(), false)?;
                let mut attempt = Attempt {
                    target: "url",
                    nonce: "1".repeat(32),
                    armed: false,
                };
                attempt.consume(record)
            })();
            assert!(result.is_err());
            assert!(!directory.exists());
            fs::create_dir(&directory).unwrap();
            let _directory = OwnedEntry::retain(directory.clone(), true).unwrap();
            let _listener = UnixListener::bind(&socket).unwrap();
            let _socket = OwnedEntry::retain(socket, false).unwrap();
            let mut attempt = Attempt {
                target: "url",
                nonce: "2".repeat(32),
                armed: false,
            };
            attempt
                .consume(&format!("armed target=url nonce={}", "2".repeat(32)))
                .unwrap();
            assert!(attempt.armed);
        }
    }

    #[test]
    fn clipboard_target_refuses_before_launch_or_creating_entries() {
        assert_eq!(
            targets(&[
                "targets".into(),
                "1".repeat(32),
                "6".into(),
                "/unused".into()
            ]),
            Err(NativeFailure::InvalidRecord)
        );
    }
}

#[cfg(test)]
mod target_exits {
    use super::*;
    use std::sync::Mutex;

    struct FakePlatform {
        record: &'static str,
        signal: Option<i32>,
        socket: Mutex<Option<PathBuf>>,
    }
    impl TargetPlatform for FakePlatform {
        type Peer = ();
        fn verify(&self, _app: &Path) -> Result<(), NativeFailure> {
            Ok(())
        }
        fn launch(
            &self,
            _app: &Path,
            run: &str,
            socket: &Path,
            _deadline: Instant,
        ) -> Result<(), NativeFailure> {
            *self.socket.lock().unwrap() = Some(socket.to_owned());
            if let Some(signal) = self.signal {
                // SAFETY: only this test thread receives the blocked signal.
                assert_eq!(unsafe { libc::raise(signal) }, 0);
                return if self.record == "refusal-signal" {
                    Err(NativeFailure::NativeUnavailable)
                } else {
                    Ok(())
                };
            }
            if self.record == "timeout" {
                return Ok(());
            }
            let socket = socket.to_owned();
            let run = run.to_owned();
            let record = self.record;
            std::thread::spawn(move || {
                let mut channel = UnixStream::connect(socket).unwrap();
                channel
                    .write_all(format!("hello schema=1 run={run} pid=1\n").as_bytes())
                    .unwrap();
                let mut reader = std::io::BufReader::new(channel.try_clone().unwrap());
                use std::io::BufRead;
                let mut line = String::new();
                for _ in 0..2 {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                }
                channel.write_all(b"targets capture-window=1\n").unwrap();
                line.clear();
                reader.read_line(&mut line).unwrap();
                if record == "refusal" {
                    channel
                        .write_all(b"refused target=accessibility reason=full\n")
                        .unwrap();
                } else {
                    channel
                        .write_all(
                            line.replace("expect ", "armed ")
                                .split(" deadline-ms=")
                                .next()
                                .unwrap()
                                .as_bytes(),
                        )
                        .unwrap();
                    channel.write_all(b"\n").unwrap();
                }
                line.clear();
                let _ = reader.read_line(&mut line);
            });
            Ok(())
        }
        fn authenticate(
            &self,
            _channel: &UnixStream,
            _pid: i32,
        ) -> Result<Self::Peer, NativeFailure> {
            Ok(())
        }
        fn recheck(&self, _peer: &Self::Peer, _channel: &UnixStream) -> Result<(), NativeFailure> {
            Ok(())
        }
    }

    #[test]
    fn cancellation_during_launch_refusal_is_consumed_before_restoring_the_mask() {
        // Keep the fixture's signal blocked even on the broken implementation,
        // so the red assertion cannot terminate another parallel test.
        let outer = AttemptSignals::block().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let run = hex(&random);
        let receipt = PathBuf::from(format!(
            "/private/tmp/spaceterm-native-520-{run}.5.receipt.sock"
        ));
        let platform = FakePlatform {
            record: "refusal-signal",
            signal: Some(libc::SIGTERM),
            socket: Mutex::new(None),
        };
        let arguments = [
            "targets".into(),
            run,
            "5".into(),
            temporary.path().join("targets").to_str().unwrap().into(),
        ];
        let result = targets_with_platform(&arguments, &platform, Duration::from_millis(150));
        let _ = outer.check(Instant::now() + Duration::from_secs(1));
        assert_eq!(result, Err(NativeFailure::AuthorizationCancelled));
        assert!(!receipt.exists());
        assert!(
            !platform
                .socket
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .parent()
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn target_refusal_timeout_and_signals_remove_the_actual_attempt_and_receipt() {
        for (record, signal, expected) in [
            ("refusal", None, NativeFailure::OutcomePending),
            ("timeout", None, NativeFailure::TimedOut),
            (
                "signal",
                Some(libc::SIGINT),
                NativeFailure::AuthorizationCancelled,
            ),
            (
                "signal",
                Some(libc::SIGHUP),
                NativeFailure::AuthorizationCancelled,
            ),
            (
                "signal",
                Some(libc::SIGTERM),
                NativeFailure::AuthorizationCancelled,
            ),
            ("armed", None, NativeFailure::TimedOut),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let mut random = [0; 16];
            getrandom::fill(&mut random).unwrap();
            let run = hex(&random);
            let receipt = PathBuf::from(format!(
                "/private/tmp/spaceterm-native-520-{run}.5.receipt.sock"
            ));
            let platform = FakePlatform {
                record,
                signal,
                socket: Mutex::new(None),
            };
            let arguments = [
                "targets".into(),
                run,
                "5".into(),
                temporary.path().join("targets").to_str().unwrap().into(),
            ];
            assert_eq!(
                targets_with_platform(&arguments, &platform, Duration::from_millis(150)),
                Err(expected)
            );
            assert!(!receipt.exists());
            let socket = platform.socket.lock().unwrap().clone().unwrap();
            assert!(!socket.exists());
            assert!(!socket.parent().unwrap().exists());
        }
    }
}

#[cfg(test)]
mod clipboard_input_regression {
    use super::*;
    #[test]
    fn clipboard_input_never_arms_a_gui_target() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("clipboard-input");
        input(&[
            "input".into(),
            "1".repeat(32),
            "6".into(),
            path.to_str().unwrap().into(),
        ])
        .unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[4..16], &[0; 12]);
    }
}
