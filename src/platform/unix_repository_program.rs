//! Runs Repository Status programs in a private process group on the POSIX hosts.
//!
//! Stdout is read without blocking on the calling thread: the pipe is nonblocking and `poll`
//! waits for data with a short timeout, so cancellation, the deadline, and the child's exit are
//! checked between reads. A background process the child leaves behind can keep the pipe open
//! after the child exits, so reading continues past the exit only until EOF or a short drain
//! deadline. The child's own output is complete by then, because everything it wrote before
//! exiting is already in the pipe.
//!
//! The exit is observed with `waitid(WNOWAIT)`, which leaves the child unreaped. The zombie keeps
//! the process group ID reserved, so the group signal that follows cannot reach a reused ID.

use std::io::{self, Read};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use crate::repository_status::{
    ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner,
};
use crate::ssh::cancellation::SshCancellationToken;

/// The longest wait for stdout before cancellation, the deadline, and exit are checked again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long stdout is still read after the child exits while another process holds the pipe.
const POST_EXIT_DRAIN: Duration = Duration::from_millis(200);
const READ_CHUNK_BYTES: usize = 64 * 1024;
/// Reads per poll, so an endless writer cannot starve the cancellation and deadline checks.
const READS_PER_POLL: usize = 16;
/// Every signal number either host defines. Undefined numbers fail harmlessly.
const HIGHEST_SIGNAL: libc::c_int = 64;

/// Spawns each program directly with `fork` and `exec`; it holds no state between runs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UnixRepositoryProgramRunner;

#[allow(dead_code, reason = "Repository Status composition selects it in a later change")]
impl UnixRepositoryProgramRunner {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl RepositoryProgramRunner for UnixRepositoryProgramRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &SshCancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        if cancellation.is_cancelled() {
            return Err(ProgramError::Cancelled);
        }
        if request
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ProgramError::TimedOut);
        }
        // A missing directory and a missing program both fail `exec` with ENOENT, so the directory
        // is checked first to keep NotFound about the program.
        if !request.directory.is_dir() {
            return Err(ProgramError::Failed);
        }
        let mut program = ProgramGroup::spawn(request)?;
        let pipe = program.child.stdout.take().ok_or(ProgramError::Failed)?;
        set_nonblocking(&pipe)?;
        let mut reader = StdoutReader {
            pipe,
            open: true,
            received: 0,
            limit: request.stdout_limit,
            buffer: vec![0; READ_CHUNK_BYTES],
        };

        loop {
            program.check_abort(request.deadline, cancellation)?;
            let wait = remaining(POLL_INTERVAL, request.deadline);
            if reader.open {
                wait_readable(&reader.pipe, wait);
                program.on_failure(reader.read_available(stdout))?;
            } else {
                std::thread::sleep(wait);
            }
            if program.has_exited()? {
                break;
            }
        }

        if !request.keep_process_group_on_exit {
            program.signal_group();
        }
        let drain_deadline = Instant::now() + POST_EXIT_DRAIN;
        let drain_deadline = request
            .deadline
            .map_or(drain_deadline, |deadline| deadline.min(drain_deadline));
        while reader.open && Instant::now() < drain_deadline {
            if cancellation.is_cancelled() {
                return Err(program.abort(ProgramError::Cancelled));
            }
            wait_readable(&reader.pipe, remaining(POLL_INTERVAL, Some(drain_deadline)));
            program.on_failure(reader.read_available(stdout))?;
        }
        program.reap()
    }
}

/// A spawned program whose group is killed and whose leader is reaped if it is dropped early.
struct ProgramGroup {
    child: Child,
    group: libc::pid_t,
    reaped: bool,
}

impl ProgramGroup {
    fn spawn(request: &ProgramRequest) -> Result<Self, ProgramError> {
        let mut command = Command::new(&request.executable);
        command
            .args(&request.arguments)
            .current_dir(&request.directory)
            .env_clear()
            .envs(request.environment.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: the callback runs after fork and performs only async-signal-safe signal
        // operations before exec. It does not access shared application state.
        unsafe {
            command.pre_exec(reset_child_signals);
        }
        let mut child = command.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => ProgramError::NotFound,
            _ => ProgramError::Failed,
        })?;
        match libc::pid_t::try_from(child.id()) {
            Ok(group) => Ok(Self {
                child,
                group,
                reaped: false,
            }),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ProgramError::Failed)
            }
        }
    }

    fn check_abort(
        &mut self,
        deadline: Option<Instant>,
        cancellation: &SshCancellationToken,
    ) -> Result<(), ProgramError> {
        if cancellation.is_cancelled() {
            return Err(self.abort(ProgramError::Cancelled));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(self.abort(ProgramError::TimedOut));
        }
        Ok(())
    }

    fn on_failure(&mut self, result: Result<(), ProgramError>) -> Result<(), ProgramError> {
        result.map_err(|error| self.abort(error))
    }

    /// Ends the whole group, reaps the leader, and returns `error`.
    fn abort(&mut self, error: ProgramError) -> ProgramError {
        self.signal_group();
        let _ = self.child.wait();
        self.reaped = true;
        error
    }

    fn signal_group(&self) {
        // SAFETY: the group belongs to the unreaped child launched above with a private group, so
        // its ID cannot have been reused. An empty group reports ESRCH, which needs no handling.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }

    /// Reports whether the leader exited, leaving it unreaped.
    fn has_exited(&mut self) -> Result<bool, ProgramError> {
        let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: the information pointer is valid for writes, and WNOWAIT leaves the child for
        // the later reap.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.group as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(self.abort(ProgramError::Failed));
        }
        // SAFETY: the structure was zeroed and waitid succeeded; WNOHANG leaves the process ID zero
        // when the child has not changed state.
        Ok(unsafe { information.assume_init().si_pid() } != 0)
    }

    fn reap(&mut self) -> Result<ProgramExit, ProgramError> {
        let status = self.child.wait();
        self.reaped = true;
        status
            .map(|status| ProgramExit {
                code: status.code(),
            })
            .map_err(|_| ProgramError::Failed)
    }
}

impl Drop for ProgramGroup {
    fn drop(&mut self) {
        if !self.reaped {
            self.signal_group();
            let _ = self.child.wait();
        }
    }
}

struct StdoutReader {
    pipe: ChildStdout,
    open: bool,
    received: usize,
    limit: Option<usize>,
    buffer: Vec<u8>,
}

impl StdoutReader {
    /// Delivers what the pipe holds now, up to a bounded number of reads.
    fn read_available(&mut self, stdout: &mut dyn FnMut(&[u8])) -> Result<(), ProgramError> {
        for _ in 0..READS_PER_POLL {
            match self.pipe.read(&mut self.buffer) {
                Ok(0) => {
                    self.open = false;
                    return Ok(());
                }
                Ok(count) => {
                    self.received = self.received.saturating_add(count);
                    if self.limit.is_some_and(|limit| self.received > limit) {
                        return Err(ProgramError::OutputTooLarge);
                    }
                    stdout(&self.buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ProgramError::Failed),
            }
        }
        Ok(())
    }
}

fn remaining(interval: Duration, deadline: Option<Instant>) -> Duration {
    deadline.map_or(interval, |deadline| {
        interval.min(deadline.saturating_duration_since(Instant::now()))
    })
}

fn set_nonblocking(pipe: &ChildStdout) -> Result<(), ProgramError> {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: the descriptor is the live stdout pipe owned by `pipe`.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    // SAFETY: as above; only the status flags change.
    if flags == -1 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(ProgramError::Failed);
    }
    Ok(())
}

/// Waits until the pipe is readable or closed, or `timeout` passes. Errors fall through to the read.
fn wait_readable(pipe: &ChildStdout, timeout: Duration) {
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let milliseconds = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: the descriptor array has one valid entry for the live pipe.
    unsafe {
        libc::poll(&mut descriptor, 1, milliseconds);
    }
}

/// Gives the program default signal dispositions and an empty signal mask.
///
/// `exec` already resets caught signals, but ignored signals and the mask are inherited from the
/// spawning thread.
fn reset_child_signals() -> io::Result<()> {
    for signal in 1..=HIGHEST_SIGNAL {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        // SAFETY: an all-zero sigaction is a valid default action with an empty mask.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_DFL;
        // SAFETY: the action is initialized; the previous action is not needed. Signal numbers
        // the host does not define fail with EINVAL, which is ignored.
        unsafe {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes the provided signal set.
    if unsafe { libc::sigemptyset(empty.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sigemptyset initialized the signal set after succeeding above.
    let empty = unsafe { empty.assume_init() };
    // SAFETY: empty is initialized and the previous mask is not needed in the child process.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, feature = "native-tests"))]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::platform::unix_adapter_tests::short_temporary_root;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = short_temporary_root().join(format!(
                "spaceterm-program-{name}-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request(executable: &str, arguments: &[&str]) -> ProgramRequest {
        ProgramRequest {
            executable: PathBuf::from(executable),
            arguments: arguments.iter().map(OsString::from).collect(),
            directory: short_temporary_root().to_path_buf(),
            environment: vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))],
            stdout_limit: None,
            deadline: Some(Instant::now() + Duration::from_secs(20)),
            keep_process_group_on_exit: false,
        }
    }

    fn shell(script: &str) -> ProgramRequest {
        request("/bin/sh", &["-c", script])
    }

    fn run(request: &ProgramRequest) -> (Result<ProgramExit, ProgramError>, Vec<u8>, usize) {
        let mut output = Vec::new();
        let mut chunks = 0;
        let result = UnixRepositoryProgramRunner::new().run(
            request,
            &mut |chunk| {
                chunks += 1;
                output.extend_from_slice(chunk);
            },
            &SshCancellationToken::default(),
        );
        (result, output, chunks)
    }

    fn published_process(path: &Path) -> libc::pid_t {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(process) = std::fs::read_to_string(path)
                .ok()
                .and_then(|value| value.trim().parse().ok())
            {
                return process;
            }
            assert!(Instant::now() < deadline, "the fixture did not publish its process");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn is_running(process: libc::pid_t) -> bool {
        // SAFETY: signal zero checks process existence and dereferences no pointers.
        unsafe { libc::kill(process, 0) == 0 }
    }

    fn wait_for_missing_process(process: libc::pid_t) -> bool {
        (0..200).any(|_| {
            let missing = !is_running(process);
            if !missing {
                std::thread::sleep(Duration::from_millis(10));
            }
            missing
        })
    }

    #[test]
    fn unix_repository_program_streams_large_output_in_chunks() {
        let (result, output, chunks) = run(&shell("head -c 1000000 /dev/zero; exit 3"));

        assert_eq!(result, Ok(ProgramExit { code: Some(3) }));
        assert_eq!(output.len(), 1_000_000);
        assert!(chunks > 1);
    }

    #[test]
    fn unix_repository_program_stops_at_the_output_limit_and_ends_the_group() {
        let fixture = Fixture::new("limit");
        let pid_file = fixture.0.join("pid");
        let mut request = shell(&format!(
            "sleep 30 >/dev/null & echo $! > '{}'; while :; do echo output; done",
            pid_file.display()
        ));
        request.stdout_limit = Some(4096);
        let started = Instant::now();

        let (result, output, _) = run(&request);

        assert_eq!(result, Err(ProgramError::OutputTooLarge));
        assert!(output.len() <= 4096);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(wait_for_missing_process(published_process(&pid_file)));
    }

    #[test]
    fn unix_repository_program_cancellation_ends_the_group() {
        let fixture = Fixture::new("cancel");
        let pid_file = fixture.0.join("pid");
        let request = shell(&format!(
            "sleep 30 & echo $! > '{}'; wait",
            pid_file.display()
        ));
        let cancellation = SshCancellationToken::default();
        let canceller = {
            let cancellation = cancellation.clone();
            let pid_file = pid_file.clone();
            std::thread::spawn(move || {
                published_process(&pid_file);
                cancellation.cancel();
            })
        };
        let started = Instant::now();

        let result = UnixRepositoryProgramRunner::new().run(&request, &mut |_| {}, &cancellation);
        canceller.join().unwrap();

        assert_eq!(result, Err(ProgramError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(wait_for_missing_process(published_process(&pid_file)));
    }

    #[test]
    fn unix_repository_program_deadline_ends_the_group() {
        let fixture = Fixture::new("deadline");
        let pid_file = fixture.0.join("pid");
        let mut request = shell(&format!(
            "sleep 30 & echo $! > '{}'; wait",
            pid_file.display()
        ));
        request.deadline = Some(Instant::now() + Duration::from_millis(500));
        let started = Instant::now();

        let (result, _, _) = run(&request);

        assert_eq!(result, Err(ProgramError::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(wait_for_missing_process(published_process(&pid_file)));
    }

    #[test]
    fn unix_repository_program_refuses_an_already_cancelled_or_expired_run() {
        let runner = UnixRepositoryProgramRunner::new();
        let mut expired = shell("exit 0");
        expired.deadline = Some(Instant::now());

        assert_eq!(
            runner.run(
                &shell("exit 0"),
                &mut |_| {},
                &SshCancellationToken::cancelled()
            ),
            Err(ProgramError::Cancelled)
        );
        assert_eq!(run(&expired).0, Err(ProgramError::TimedOut));
    }

    #[test]
    fn unix_repository_program_ends_the_group_after_a_normal_exit_by_default() {
        let fixture = Fixture::new("exit-kill");
        let pid_file = fixture.0.join("pid");
        // The background process holds stdout open, as a careless daemon would.
        let request = shell(&format!(
            "sleep 30 & echo $! > '{}'; echo done",
            pid_file.display()
        ));
        let started = Instant::now();

        let (result, output, _) = run(&request);

        assert_eq!(result, Ok(ProgramExit { code: Some(0) }));
        assert_eq!(output, b"done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(wait_for_missing_process(published_process(&pid_file)));
    }

    #[test]
    fn unix_repository_program_keeps_the_group_after_a_normal_exit_when_asked() {
        for (name, background) in [
            ("keep-detached", "sleep 30 >/dev/null"),
            ("keep-holding-stdout", "sleep 30"),
        ] {
            let fixture = Fixture::new(name);
            let pid_file = fixture.0.join("pid");
            let mut request = shell(&format!(
                "{background} & echo $! > '{}'; echo done",
                pid_file.display()
            ));
            request.keep_process_group_on_exit = true;
            let started = Instant::now();

            let (result, output, _) = run(&request);
            let survivor = published_process(&pid_file);
            let survived = is_running(survivor);
            // SAFETY: the fixture's own background process is ended after the observation.
            unsafe {
                libc::kill(survivor, libc::SIGKILL);
            }

            assert_eq!(result, Ok(ProgramExit { code: Some(0) }), "{name}");
            assert_eq!(output, b"done\n", "{name}");
            assert!(started.elapsed() < Duration::from_secs(5), "{name}");
            assert!(survived, "{name}");
        }
    }

    #[test]
    fn unix_repository_program_clears_the_inherited_environment() {
        let mut request = request("/usr/bin/env", &[]);
        request.environment = vec![(OsString::from("ONLY"), OsString::from("value"))];

        let (result, output, _) = run(&request);

        assert_eq!(result, Ok(ProgramExit { code: Some(0) }));
        assert_eq!(output, b"ONLY=value\n");
    }

    #[test]
    fn unix_repository_program_runs_in_the_requested_directory() {
        let fixture = Fixture::new("directory");
        let mut request = shell("pwd -P");
        request.directory = fixture.0.clone();

        let (result, output, _) = run(&request);

        assert_eq!(result, Ok(ProgramExit { code: Some(0) }));
        assert_eq!(output, format!("{}\n", fixture.0.display()).as_bytes());
    }

    #[test]
    fn unix_repository_program_maps_missing_and_unexecutable_programs_to_not_found() {
        let fixture = Fixture::new("missing");
        let unexecutable = fixture.0.join("program");
        std::fs::write(&unexecutable, "#!/bin/sh\nexit 0\n").unwrap();
        let mut missing_directory = shell("exit 0");
        missing_directory.directory = fixture.0.join("missing");

        assert_eq!(
            run(&request(&fixture.0.join("absent").to_string_lossy(), &[])).0,
            Err(ProgramError::NotFound)
        );
        assert_eq!(
            run(&request(&unexecutable.to_string_lossy(), &[])).0,
            Err(ProgramError::NotFound)
        );
        assert_eq!(run(&missing_directory).0, Err(ProgramError::Failed));
    }

    struct SignalMaskRestore(libc::sigset_t);

    impl Drop for SignalMaskRestore {
        fn drop(&mut self) {
            // SAFETY: the saved signal set was initialized by pthread_sigmask for this thread.
            let result =
                unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
            assert_eq!(result, 0, "the test thread signal mask should be restored");
        }
    }

    fn block_sigterm() -> SignalMaskRestore {
        let mut blocked = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigemptyset initializes the provided signal set.
        assert_eq!(unsafe { libc::sigemptyset(blocked.as_mut_ptr()) }, 0);
        // SAFETY: sigemptyset initialized the signal set above.
        let mut blocked = unsafe { blocked.assume_init() };
        // SAFETY: blocked is initialized and SIGTERM is a valid signal number.
        assert_eq!(unsafe { libc::sigaddset(&mut blocked, libc::SIGTERM) }, 0);
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: both signal-set pointers are valid for the duration of this call.
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, previous.as_mut_ptr()) },
            0
        );
        // SAFETY: pthread_sigmask initialized previous after succeeding above.
        SignalMaskRestore(unsafe { previous.assume_init() })
    }

    #[test]
    fn unix_repository_program_does_not_inherit_the_calling_thread_signal_mask() {
        let restore = block_sigterm();
        // With SIGTERM still blocked the shell would survive its own signal and exit 0.
        let (result, _, _) = run(&shell("kill -TERM $$; exit 0"));
        drop(restore);

        assert_eq!(result, Ok(ProgramExit { code: None }));
    }

    /// Git for the end-to-end test, from the usual install locations.
    fn installed_git() -> Option<PathBuf> {
        [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/Library/Developer/CommandLineTools/usr/bin/git",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
            #[cfg(target_os = "linux")]
            "/usr/bin/git",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    }

    fn fixture_git(git: &Path, home: &Path, directory: &Path, arguments: &[&str]) {
        let status = Command::new(git)
            .args(arguments)
            .current_dir(directory)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "fixture git {arguments:?} failed");
    }

    fn executable_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn unix_repository_program_runs_hardened_git_status_without_hooks_or_index_writes() {
        let Some(git) = installed_git() else {
            eprintln!("skipping: git is not installed");
            return;
        };
        let fixture = Fixture::new("git");
        let home = fixture.0.join("home");
        let repository = fixture.0.join("repository");
        let ran = fixture.0.join("ran");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::create_dir_all(&ran).unwrap();
        fixture_git(&git, &home, &repository, &["-c", "init.defaultBranch=main", "init", "-q"]);
        std::fs::write(repository.join("tracked.txt"), "first\n").unwrap();
        fixture_git(&git, &home, &repository, &["add", "tracked.txt"]);
        fixture_git(
            &git,
            &home,
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        );
        let hooks = repository.join(".git/hooks");
        for hook in ["post-checkout", "pre-commit", "post-index-change"] {
            executable_script(
                &hooks.join(hook),
                &format!("touch '{}'", ran.join(hook).display()),
            );
        }
        let fsmonitor = fixture.0.join("fsmonitor-hook");
        executable_script(
            &fsmonitor,
            &format!("touch '{}'", ran.join("fsmonitor").display()),
        );
        fixture_git(
            &git,
            &home,
            &repository,
            &["config", "core.fsmonitor", &fsmonitor.to_string_lossy()],
        );
        fixture_git(
            &git,
            &home,
            &repository,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        // A newer file with the same size makes the index stat information stale, which a
        // status without --no-optional-locks would refresh and write.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(repository.join("tracked.txt"), "other\n").unwrap();
        std::fs::write(repository.join("untracked.txt"), "new\n").unwrap();
        let index_before = std::fs::read(repository.join(".git/index")).unwrap();

        let mut arguments = vec!["--no-optional-locks", "--no-pager"];
        for setting in [
            "core.fsmonitor=false",
            "core.hooksPath=/dev/null",
            "color.ui=false",
            "core.quotePath=false",
            "status.relativePaths=false",
            "advice.statusHints=false",
        ] {
            arguments.extend(["-c", setting]);
        }
        let repository_argument = repository.to_string_lossy().into_owned();
        arguments.extend(["-C", &repository_argument]);
        arguments.extend(["status", "--porcelain=v2", "--branch", "-z"]);
        let git_directory = git.parent().unwrap().as_os_str().to_owned();
        let mut path = git_directory;
        path.push(":/usr/bin:/bin");
        let mut request = request(&git.to_string_lossy(), &arguments);
        request.directory = repository.clone();
        request.environment = [
            ("HOME", home.as_os_str().to_owned()),
            ("PATH", path),
            ("LC_ALL", "C".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_OPTIONAL_LOCKS", "0".into()),
            ("GIT_PAGER", "cat".into()),
            ("PAGER", "cat".into()),
            ("GIT_NO_LAZY_FETCH", "1".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), value))
        .collect();

        let (result, output, _) = run(&request);

        assert_eq!(result, Ok(ProgramExit { code: Some(0) }));
        let records: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
        assert!(records.contains(&b"# branch.head main".as_slice()));
        assert!(
            records
                .iter()
                .any(|record| record.starts_with(b"1 .M ") && record.ends_with(b" tracked.txt"))
        );
        assert!(records.contains(&b"? untracked.txt".as_slice()));
        assert_eq!(
            std::fs::read_dir(&ran).unwrap().count(),
            0,
            "a hook or hook-program fsmonitor ran"
        );
        assert!(!repository.join(".git/index.lock").exists());
        assert_eq!(
            std::fs::read(repository.join(".git/index")).unwrap(),
            index_before
        );

        // The same status without the hardening runs the hook program, so the fixture is armed.
        let mut unhardened = request.clone();
        unhardened.arguments = ["-C", &repository_argument, "status", "--porcelain=v2"]
            .into_iter()
            .map(OsString::from)
            .collect();
        assert_eq!(run(&unhardened).0, Ok(ProgramExit { code: Some(0) }));
        assert!(ran.join("fsmonitor").exists());
    }
}
