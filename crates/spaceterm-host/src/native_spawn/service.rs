use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use super::protocol::{self, Failure, Program, ProgramKind, Reply, Request, Result, Retirement};
use crate::{
    native_deployment::contracts::NativeFailure,
    pty::unix_pty::{ProcessIdentity, UnixPtyHost},
};

const POLL: Duration = Duration::from_millis(10);
const GRACE: Duration = Duration::from_secs(2);
const FORCE: Duration = Duration::from_secs(1);

/// Run only in the protected, exec-separated account spawner. The launcher must authenticate
/// its executable, drop all saved privileges, retain ordinary account groups, and exec it before
/// any credential retrieval. The only inherited resource is the anonymous channel at fd 3.
/// This function grants no startup or credential authority and refuses a privileged invocation.
///
/// # Safety
/// The caller must be this spawner executable's single-threaded entry, before any credential
/// access. It exclusively owns fd 3 and the process's inherited descriptors; no other Rust
/// descriptor owners or application workers may exist in this process.
pub unsafe fn run_inherited() -> std::result::Result<(), NativeFailure> {
    verify_account_process()?;
    reset_signals().map_err(|_| NativeFailure::NativeUnavailable)?;
    verify_inherited_channel().map_err(native_failure)?;
    // SAFETY: this executable owns the fixed inherited descriptor. No other Rust owner exists.
    let mut channel = UnixStream::from(unsafe { OwnedFd::from_raw_fd(3) });
    protocol::configure(&channel).map_err(native_failure)?;
    discard_inherited_descriptors().map_err(native_failure)?;
    let null = File::options()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|_| NativeFailure::NativeUnavailable)?;
    for fd in 0..3 {
        // SAFETY: the entry point exclusively owns its standard streams.
        if unsafe { libc::dup2(null.as_raw_fd(), fd) } < 0 {
            return Err(NativeFailure::NativeUnavailable);
        }
    }
    drop(null);
    let mut hello = [0u8; 16];
    hello[..4].copy_from_slice(protocol::HELLO_MAGIC);
    hello[4..8].copy_from_slice(&std::process::id().to_be_bytes());
    // SAFETY: these are read-only native account queries, checked above.
    hello[8..12].copy_from_slice(&unsafe { libc::getuid() }.to_be_bytes());
    hello[12..16].copy_from_slice(&unsafe { libc::getgid() }.to_be_bytes());
    channel
        .write_all(&hello)
        .map_err(|_| NativeFailure::NativeUnavailable)?;
    let mut service = Service {
        children: HashMap::new(),
        next_handle: 1,
    };
    let outcome = loop {
        service.advance_retirements();
        if service.children.values().any(|child| {
            matches!(
                child.retirement,
                Some(RetirementProgress::Hangup(_) | RetirementProgress::Kill { .. })
            )
        }) {
            let mut descriptor = libc::pollfd {
                fd: channel.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // Progress retirement even if its requester stops polling. Never wait out a grace
            // period inside a request: every other child uses this same anonymous channel.
            let ready = unsafe { libc::poll(&mut descriptor, 1, POLL.as_millis() as i32) };
            if ready == 0
                || (ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted)
            {
                continue;
            }
            if ready < 0 {
                break Err(NativeFailure::NativeUnavailable);
            }
        }
        let received = match protocol::receive_request(&mut channel) {
            Ok(Some(received)) => received,
            Ok(None) => break Ok(()),
            Err(failure) => break Err(native_failure(failure)),
        };
        let (payload, descriptors) = received;
        let (sequence, payload) = match protocol::untag(&payload) {
            Ok(tagged) => tagged,
            Err(failure) => break Err(native_failure(failure)),
        };
        let result =
            Request::decode(payload).and_then(|request| service.execute(request, descriptors));
        if let Err(failure) = protocol::send_reply(&mut channel, sequence, result) {
            break Err(native_failure(failure));
        }
    };
    // Channel loss cannot abandon owned children. Kill all first, then share one bounded deadline.
    service.cleanup().map_err(native_failure)?;
    outcome
}

fn verify_inherited_channel() -> Result<()> {
    if unsafe { libc::fcntl(3, libc::F_GETFD) } < 0 {
        return Err(Failure::Unavailable);
    }
    let mut socket_type = 0i32;
    let mut size = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            3,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut socket_type as *mut i32).cast(),
            &mut size,
        )
    } != 0
        || size as usize != std::mem::size_of_val(&socket_type)
        || socket_type != libc::SOCK_STREAM
    {
        return Err(Failure::Invalid);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    if unsafe { libc::getsockname(3, address.as_mut_ptr().cast(), &mut length) } != 0 {
        return Err(Failure::Invalid);
    }
    let address = unsafe { address.assume_init() };
    if address.sun_family as i32 != libc::AF_UNIX || address.sun_path.iter().any(|byte| *byte != 0)
    {
        return Err(Failure::Invalid);
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    if unsafe { libc::getpeername(3, address.as_mut_ptr().cast(), &mut length) } != 0 {
        return Err(Failure::Invalid);
    }
    let address = unsafe { address.assume_init() };
    if address.sun_family as i32 != libc::AF_UNIX || address.sun_path.iter().any(|byte| *byte != 0)
    {
        return Err(Failure::Invalid);
    }
    Ok(())
}

fn native_failure(failure: Failure) -> NativeFailure {
    match failure {
        Failure::CleanupUnresolved => NativeFailure::CleanupUnresolved,
        Failure::Limit => NativeFailure::LimitExceeded,
        Failure::Invalid => NativeFailure::InvalidRecord,
        _ => NativeFailure::NativeUnavailable,
    }
}

fn verify_account_process() -> std::result::Result<(), NativeFailure> {
    // SAFETY: read-only queries. The protocol has no credential-changing operation.
    let (uid, euid, gid, egid) = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    // Exec sets saved IDs from the effective IDs. This protected entry has performed no
    // credential change since exec; Darwin's equality checks therefore also exclude saved root.
    if uid == 0 || gid == 0 || uid != euid || gid != egid {
        return Err(NativeFailure::ExecutionBindingUnproven);
    }
    #[cfg(target_os = "linux")]
    {
        let mut users = [0; 3];
        let mut groups = [0; 3];
        // Saved IDs must not permit a return to root. Neither the secret-server guard nor
        // NoNewPrivs may cross this exec: set-ID sudo remains available to ordinary programs.
        if unsafe { libc::getresuid(&mut users[0], &mut users[1], &mut users[2]) } != 0
            || unsafe { libc::getresgid(&mut groups[0], &mut groups[1], &mut groups[2]) } != 0
            || users != [uid; 3]
            || groups != [gid; 3]
            || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 0
            || unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } != 0
        {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        let status = std::fs::read_to_string("/proc/self/status")
            .map_err(|_| NativeFailure::ExecutionBindingUnproven)?;
        for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
            let value = status
                .lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|value| u64::from_str_radix(value.trim(), 16).ok());
            if value != Some(0) {
                return Err(NativeFailure::ExecutionBindingUnproven);
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn discard_inherited_descriptors() -> Result<()> {
    // A syscall over the descriptor table also closes descriptors above a lowered RLIMIT_NOFILE.
    // SAFETY: only this new exec-separated process's descriptor table is modified.
    if unsafe { libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32) } != 0 {
        return Err(Failure::Unavailable);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn descriptor_table() -> Result<Vec<i32>> {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Entry {
        fd: i32,
        kind: u32,
    }
    let size = unsafe { libc::proc_pidinfo(libc::getpid(), 1, 0, std::ptr::null_mut(), 0) };
    let entry_size = std::mem::size_of::<Entry>();
    if size <= 0 || size as usize > 1024 * 1024 || !(size as usize).is_multiple_of(entry_size) {
        return Err(Failure::Unavailable);
    }
    // Command creates its own CLOEXEC exec-error pipe after this snapshot. All account
    // descriptors already exist here; no other thread can add descriptors in this spawner.
    let mut entries = vec![Entry { fd: -1, kind: 0 }; size as usize / entry_size + 16];
    let bytes = (entries.len() * entry_size) as i32;
    let actual =
        unsafe { libc::proc_pidinfo(libc::getpid(), 1, 0, entries.as_mut_ptr().cast(), bytes) };
    if actual <= 0 || actual >= bytes || !(actual as usize).is_multiple_of(entry_size) {
        return Err(Failure::Unavailable);
    }
    entries.truncate(actual as usize / entry_size);
    Ok(entries.into_iter().map(|entry| entry.fd).collect())
}

#[cfg(target_os = "macos")]
fn discard_inherited_descriptors() -> Result<()> {
    for fd in descriptor_table()? {
        if fd > 3 && unsafe { libc::close(fd) } != 0 {
            return Err(Failure::Unavailable);
        }
    }
    Ok(())
}

struct Service {
    children: HashMap<u64, AccountChild>,
    next_handle: u64,
}
impl Service {
    fn execute(&mut self, request: Request, descriptors: Vec<OwnedFd>) -> Result<Reply> {
        match request {
            Request::Spawn(program) => {
                if self.children.len() >= protocol::MAX_CHILDREN {
                    return Err(Failure::Limit);
                }
                let next = self.next_handle.checked_add(1).ok_or(Failure::Limit)?;
                let child = AccountChild::spawn(program, descriptors)?;
                let reply = Reply::Spawned {
                    handle: self.next_handle,
                    pid: child.child.id(),
                };
                self.children.insert(self.next_handle, child);
                self.next_handle = next;
                Ok(reply)
            }
            Request::Poll(handle) => {
                if !descriptors.is_empty() {
                    return Err(Failure::Invalid);
                }
                self.children
                    .get(&handle)
                    .ok_or(Failure::Invalid)?
                    .exited()
                    .map(Reply::Poll)
            }
            Request::KillGroup(handle) => {
                if !descriptors.is_empty() {
                    return Err(Failure::Invalid);
                }
                let child = self.children.get(&handle).ok_or(Failure::Invalid)?;
                child.signal(libc::SIGKILL)?;
                Ok(Reply::Poll(child.exited()?))
            }
            Request::ReleaseGroup(handle) => {
                if !descriptors.is_empty() {
                    return Err(Failure::Invalid);
                }
                let child = self.children.get_mut(&handle).ok_or(Failure::Invalid)?;
                if !matches!(child.retirement, Some(RetirementProgress::KeepGroup)) {
                    return Err(Failure::Invalid);
                }
                // Only the client's accepted completion releases daemon-group ownership. Until
                // this request, WNOWAIT reserves the exact leader and permits a real abort kill.
                let reply = child.reap(false)?;
                self.children.remove(&handle);
                Ok(reply)
            }
            Request::Retire(handle, mode) => {
                if !descriptors.is_empty() {
                    return Err(Failure::Invalid);
                }
                let child = self.children.get_mut(&handle).ok_or(Failure::Invalid)?;
                child.start_retirement(mode)?;
                child.advance_retirement();
                match child.retirement {
                    Some(RetirementProgress::Complete(result)) => {
                        // Keep the exact leader reservation until retirement is complete, then
                        // retain its reply until the owning client consumes it.
                        if result.is_ok() {
                            self.children.remove(&handle);
                        }
                        result
                    }
                    Some(RetirementProgress::KeepGroup) => Ok(Reply::GroupRetained),
                    _ => Ok(Reply::Retiring),
                }
            }
        }
    }

    fn advance_retirements(&mut self) {
        for child in self.children.values_mut() {
            child.advance_retirement();
        }
    }

    fn cleanup(&mut self) -> Result<()> {
        // Completed leaders are already reaped; signalling their old PID would be unsafe.
        self.children.retain(|_, child| {
            !matches!(child.retirement, Some(RetirementProgress::Complete(Ok(_))))
        });
        let mut failed = false;
        for child in self.children.values() {
            failed |= child.signal(libc::SIGKILL).is_err();
        }
        let deadline = Instant::now() + FORCE;
        while !self.children.is_empty() && Instant::now() < deadline {
            self.children.retain(|_, child| match child.exited() {
                Ok(true) => match child.members_alive() {
                    Ok(false) => {
                        failed |= child.child.wait().is_err();
                        false
                    }
                    Ok(true) => true,
                    Err(_) => {
                        failed = true;
                        true
                    }
                },
                Ok(false) => true,
                Err(_) => {
                    failed = true;
                    true
                }
            });
            if !self.children.is_empty() {
                std::thread::sleep(POLL);
            }
        }
        if failed || !self.children.is_empty() {
            Err(Failure::CleanupUnresolved)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum RetirementProgress {
    Hangup(Instant),
    Kill { deadline: Instant, forced: bool },
    Complete(Result<Reply>),
    KeepGroup,
}

struct AccountChild {
    child: Child,
    kind: ProgramKind,
    pid: i32,
    retirement: Option<RetirementProgress>,
}
impl AccountChild {
    fn spawn(program: Program, mut descriptors: Vec<OwnedFd>) -> Result<Self> {
        if descriptors.len() != 2 {
            return Err(Failure::Invalid);
        }
        let stream = descriptors.pop().ok_or(Failure::Invalid)?;
        let directory = descriptors.pop().ok_or(Failure::Invalid)?;
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0
            || unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR
        {
            return Err(Failure::Invalid);
        }
        let mut command = Command::new(&program.executable);
        command.args(&program.arguments).env_clear().envs(
            program
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
        match program.kind {
            ProgramKind::Pty => {
                if unsafe { libc::isatty(stream.as_raw_fd()) } != 1 {
                    return Err(Failure::Invalid);
                }
                command
                    .stdin(Stdio::from(
                        stream.try_clone().map_err(|_| Failure::Unavailable)?,
                    ))
                    .stdout(Stdio::from(
                        stream.try_clone().map_err(|_| Failure::Unavailable)?,
                    ))
                    .stderr(Stdio::from(stream));
            }
            ProgramKind::Tool => {
                let mut stat = MaybeUninit::<libc::stat>::uninit();
                let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
                if flags < 0
                    || flags & libc::O_ACCMODE != libc::O_WRONLY
                    || unsafe { libc::fstat(stream.as_raw_fd(), stat.as_mut_ptr()) } != 0
                    || unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO
                {
                    return Err(Failure::Invalid);
                }
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(stream))
                    .stderr(Stdio::null())
                    .process_group(0);
            }
        }
        #[cfg(target_os = "macos")]
        let descriptors = descriptor_table().map_err(|_| Failure::Unavailable)?;
        let kind = program.kind;
        // SAFETY: the spawner is single-threaded and credential-free. This callback only uses
        // native operations and owned, preallocated data, then Command execs the account program.
        unsafe {
            command.pre_exec(move || {
                reset_signals()?;
                if libc::fchdir(directory.as_raw_fd()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if kind == ProgramKind::Pty
                    && (libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0)
                {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) != 0 {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(target_os = "macos")]
                for fd in &descriptors {
                    if *fd > 2 && libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = crate::process::spawn(&mut command).map_err(|error| match error {
            crate::process::SpawnError::NotFound | crate::process::SpawnError::PermissionDenied => {
                Failure::NotFound
            }
            crate::process::SpawnError::Unavailable => Failure::Unavailable,
        })?;
        let pid = match i32::try_from(child.id()) {
            Ok(pid) if pid > 0 => pid,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Unavailable);
            }
        };
        Ok(Self {
            child,
            kind,
            pid,
            retirement: None,
        })
    }

    fn exited(&self) -> Result<bool> {
        if matches!(self.retirement, Some(RetirementProgress::Complete(Ok(_)))) {
            return Ok(true);
        }
        let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
        // WNOWAIT reserves this PID, hence its private process group and session, until retirement.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as _,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(Failure::CleanupUnresolved);
        }
        Ok(unsafe { information.assume_init().si_pid() } != 0)
    }

    fn signal(&self, signal: i32) -> Result<()> {
        if matches!(self.retirement, Some(RetirementProgress::Complete(Ok(_)))) {
            return Ok(());
        }
        if self.kind == ProgramKind::Tool {
            // Darwin rejects a signal to a zombie-only group with PermissionDenied. Keep the
            // leader reservation, but signal only if the leader or a group member is live.
            if !self.exited()? || self.members_alive()? {
                return signal_group(self.pid, signal);
            }
            return Ok(());
        }
        let host = native_host();
        let mut groups = BTreeMap::<i32, Vec<ProcessIdentity>>::new();
        for process in host.process_ids().map_err(|_| Failure::CleanupUnresolved)? {
            if let Some(observation) =
                live_process(process).filter(|observation| observation.session == self.pid)
            {
                groups
                    .entry(observation.process_group)
                    .or_default()
                    .push(observation.identity);
            }
        }
        // The unreaped session leader keeps the SID reserved. Recheck member identities before
        // signalling job-control groups. Always signal the reserved leader group last.
        let leader_members = groups.remove(&self.pid).is_some();
        for (group, members) in groups {
            if members.iter().any(|member| {
                live_process(member.process).is_some_and(|observed| {
                    observed.identity == *member
                        && observed.session == self.pid
                        && observed.process_group == group
                })
            }) {
                signal_group(group, signal)?;
            }
        }
        if !self.exited()? || leader_members {
            signal_group(self.pid, signal)
        } else {
            Ok(())
        }
    }

    fn members_alive(&self) -> Result<bool> {
        let host = native_host();
        Ok(host
            .process_ids()
            .map_err(|_| Failure::CleanupUnresolved)?
            .into_iter()
            .any(|pid| {
                live_process(pid).is_some_and(|process| match self.kind {
                    ProgramKind::Tool => process.process_group == self.pid,
                    ProgramKind::Pty => process.session == self.pid,
                })
            }))
    }

    fn start_retirement(&mut self, mode: Retirement) -> Result<()> {
        if let Some(progress) = self.retirement {
            if matches!(mode, Retirement::Kill)
                && matches!(
                    progress,
                    RetirementProgress::Hangup(_) | RetirementProgress::KeepGroup
                )
            {
                self.signal(libc::SIGKILL)?;
                self.retirement = Some(RetirementProgress::Kill {
                    deadline: Instant::now() + FORCE,
                    forced: true,
                });
            }
            return Ok(());
        }
        if matches!(mode, Retirement::KeepGroup) {
            if self.kind != ProgramKind::Tool || !self.exited()? {
                return Err(Failure::Invalid);
            }
            self.retirement = Some(RetirementProgress::KeepGroup);
        } else if matches!(mode, Retirement::Hangup) {
            self.signal(libc::SIGHUP)?;
            self.retirement = Some(RetirementProgress::Hangup(Instant::now() + GRACE));
        } else {
            // The unreaped leader reserves its group/session while descendants are signalled.
            self.signal(libc::SIGKILL)?;
            self.retirement = Some(RetirementProgress::Kill {
                deadline: Instant::now() + FORCE,
                forced: false,
            });
        }
        Ok(())
    }

    fn advance_retirement(&mut self) {
        let Some(progress) = self.retirement else {
            return;
        };
        let (deadline, forced) = match progress {
            RetirementProgress::Hangup(deadline) => (deadline, false),
            RetirementProgress::Kill { deadline, forced } => (deadline, forced),
            RetirementProgress::Complete(_) | RetirementProgress::KeepGroup => return,
        };
        let result = (|| {
            if self.exited()? && !self.members_alive()? {
                return Ok(Some(self.reap(forced)?));
            }
            if Instant::now() >= deadline {
                if matches!(progress, RetirementProgress::Hangup(_)) {
                    self.signal(libc::SIGKILL)?;
                    self.retirement = Some(RetirementProgress::Kill {
                        deadline: Instant::now() + FORCE,
                        forced: true,
                    });
                } else {
                    return Err(Failure::CleanupUnresolved);
                }
            }
            Ok(None)
        })();
        match result {
            Ok(Some(reply)) => self.retirement = Some(RetirementProgress::Complete(Ok(reply))),
            Err(failure) => self.retirement = Some(RetirementProgress::Complete(Err(failure))),
            Ok(None) => {}
        }
    }

    fn reap(&mut self, forced: bool) -> Result<Reply> {
        let status = self.child.wait().map_err(|_| Failure::CleanupUnresolved)?;
        Ok(Reply::Retired {
            status: status.into_raw(),
            forced,
        })
    }
}

fn signal_group(group: i32, signal: i32) -> Result<()> {
    if unsafe { libc::kill(-group, signal) } == 0
        || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Ok(());
    }
    // The last live member can exit between observation and signalling. Darwin then reports
    // PermissionDenied for the zombie-only group. Confirm absence of live members instead of
    // either treating that error as retirement or failing a completed ordinary exit.
    let host = native_host();
    if host
        .process_ids()
        .map_err(|_| Failure::CleanupUnresolved)?
        .into_iter()
        .any(|pid| live_process(pid).is_some_and(|process| process.process_group == group))
    {
        Err(Failure::CleanupUnresolved)
    } else {
        Ok(())
    }
}

fn reset_signals() -> io::Result<()> {
    for signal in 1..=64 {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_DFL;
        // Both hosts have unused signal numbers; EINVAL does not indicate inherited state.
        unsafe {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } != 0
        || unsafe { libc::sigprocmask(libc::SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut()) }
            != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn native_host() -> impl UnixPtyHost {
    #[cfg(target_os = "linux")]
    {
        crate::pty::linux_pty_host::LinuxPtyHost
    }
    #[cfg(target_os = "macos")]
    {
        crate::pty::macos_pty_host::MacosPtyHost
    }
}

fn live_process(pid: i32) -> Option<crate::pty::unix_pty::ProcessObservation> {
    let observation = native_host().observe_process(pid)?;
    #[cfg(target_os = "macos")]
    {
        // The ordinary Darwin host observation retains zombies for identity checks. Shutdown
        // counts only live members, and verifies that this separate status query is for that
        // same start identity before classifying it.
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let expected = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        if unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                expected,
            )
        } != expected
        {
            return None;
        }
        let info = unsafe { info.assume_init() };
        if info.pbi_pid != pid as u32
            || info.pbi_start_tvsec != observation.identity.start.coarse
            || info.pbi_start_tvusec != observation.identity.start.fine
            || info.pbi_status == 5
        {
            return None;
        }
    }
    Some(observation)
}
