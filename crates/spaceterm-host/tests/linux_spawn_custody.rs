//! The reusable spawner is an unprivileged native peer, not a credential authority.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

use spaceterm_host::{
    git::{CancellationToken, ProgramError, ProgramRequest, RepositoryProgramRunner},
    native_spawn::{
        CredentialFreePtyAdapterFactory, CredentialFreeRepositoryProgramRunner,
        CredentialFreeSpawner,
    },
    pty::{NativePtyAdapterFactory, NativePtyExit, NativePtySize},
    shell_launch::{
        PreparedRemoteShellCommand, PreparedShellLaunch, RemoteShellLaunchParts, ShellLaunchFailure,
    },
    startup_exchange::unix::{ExactChild, LockedMemory, process_identity},
};

fn peer(leak_high_descriptor: bool) -> (UnixStream, Child) {
    restricted_peer(leak_high_descriptor, false)
}

fn restricted_peer(leak_high_descriptor: bool, no_new_privs: bool) -> (UnixStream, Child) {
    let (server, peer) = UnixStream::pair().unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let fd = peer.as_raw_fd();
    let unrelated = File::open("/dev/null").unwrap();
    let unrelated_fd = unrelated.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm-server-spawner"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the injected launcher performs native descriptor operations only before exec.
    // It runs as the test account, never root, and retrieves no credential.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if no_new_privs && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "macos")]
            let _ = no_new_privs;
            if leak_high_descriptor {
                if libc::dup2(unrelated_fd, 1007) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let limit = libc::rlimit {
                    rlim_cur: 64,
                    rlim_max: 64,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    drop(peer);
    (server, child)
}

fn spawner(leak_high_descriptor: bool) -> CredentialFreeSpawner {
    let (channel, child) = peer(leak_high_descriptor);
    let identity = process_identity(child.id()).unwrap();
    let mut owner = Some(child);
    let exact = ExactChild::take(&mut owner, identity).unwrap();
    assert!(owner.is_none());
    CredentialFreeSpawner::connect(channel, exact).unwrap()
}

#[path = "native_handoff/spawn_channel.rs"]
mod spawn_channel;

fn observed_spawner(
    observe: impl FnMut(u8, bool) + Send + 'static,
) -> (
    CredentialFreeSpawner,
    std::thread::JoinHandle<std::io::Result<()>>,
) {
    let (peer, child) = peer(false);
    let (channel, proxy) = UnixStream::pair().unwrap();
    let worker = std::thread::spawn(move || spawn_channel::proxy(proxy, peer, observe));
    let identity = process_identity(child.id()).unwrap();
    let exact = ExactChild::take(&mut Some(child), identity).unwrap();
    (
        CredentialFreeSpawner::connect(channel, exact).unwrap(),
        worker,
    )
}

struct DelayedPollPeer {
    spawner: CredentialFreeSpawner,
    held: std::sync::mpsc::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    proxy: std::thread::JoinHandle<std::io::Result<()>>,
}

fn delayed_poll_spawner(fragment: bool) -> DelayedPollPeer {
    let (peer, child) = peer(false);
    let (channel, proxy) = UnixStream::pair().unwrap();
    let (held, observed) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay = armed.clone();
    let worker = std::thread::spawn(move || {
        spawn_channel::delayed_poll(proxy, peer, held, released, delay, fragment)
    });
    let identity = process_identity(child.id()).unwrap();
    let exact = ExactChild::take(&mut Some(child), identity).unwrap();
    DelayedPollPeer {
        spawner: CredentialFreeSpawner::connect(channel, exact).unwrap(),
        held: observed,
        release,
        armed,
        proxy: worker,
    }
}

fn observer() -> &'static Path {
    static EXECUTABLE: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &EXECUTABLE
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let executable = directory.path().join("spawn-observer");
            let result = Command::new("cc")
                .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/native_handoff/spawn_observer.c"),
                )
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
            assert!(result.status.success(), "the native observer must compile");
            (directory, executable)
        })
        .1
}

struct Launch(RemoteShellLaunchParts);
impl PreparedRemoteShellCommand for Launch {
    fn into_remote_shell_parts(self) -> Result<RemoteShellLaunchParts, ShellLaunchFailure> {
        Ok(self.0)
    }
}
fn launch(directory: &Path, arguments: &[&str]) -> PreparedShellLaunch {
    PreparedShellLaunch::remote(
        directory,
        Launch(RemoteShellLaunchParts {
            executable: observer().to_owned(),
            arguments: arguments.iter().map(OsString::from).collect(),
            working_directory: directory.to_owned(),
            environment: vec![],
            terminal_name: "xterm-256color",
        }),
    )
    .unwrap()
}
fn request(directory: &Path, arguments: &[&str]) -> ProgramRequest {
    ProgramRequest {
        executable: observer().to_owned(),
        arguments: arguments.iter().map(OsString::from).collect(),
        directory: directory.to_owned(),
        environment: vec![],
        stdout_limit: Some(4096),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        keep_process_group_on_exit: false,
    }
}
fn facts(output: &[u8]) -> BTreeMap<&str, u64> {
    std::str::from_utf8(output)
        .unwrap()
        .split_ascii_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(name, value)| (name, value.parse().unwrap()))
        .collect()
}

#[test]
fn pre_secret_spawner_serves_one_anonymous_channel() {
    let (mut server, mut child) = peer(false);
    let mut hello = [0u8; 16];
    let result = server.read_exact(&mut hello);
    drop(server);
    let status = child.wait().unwrap();
    assert!(
        result.is_ok(),
        "the spawner must send its bounded startup identity"
    );
    assert_eq!(&hello[..4], b"STS2");
    assert_eq!(
        u32::from_be_bytes(hello[4..8].try_into().unwrap()),
        child.id()
    );
    assert!(status.success());
}

#[test]
fn repeated_pty_and_tool_launches_share_the_pre_secret_process() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(true);
    let pid = spawner.process_id();
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    // The peer already execed before these random synthetic secret bytes exist. Its later
    // children are never forked from this process, regardless of CLOEXEC or WIPEONFORK.
    let mut secret = LockedMemory::<32>::allocate().unwrap();
    getrandom::fill(secret.bytes_mut()).unwrap();
    for _ in 0..3 {
        let mut parts = factory
            .create(
                launch(directory.path(), &["7"]),
                NativePtySize {
                    rows: 31,
                    columns: 97,
                    ..NativePtySize::default()
                },
            )
            .unwrap();
        let mut output = Vec::new();
        parts
            .adapter
            .take_reader()
            .unwrap()
            .read_to_end(&mut output)
            .unwrap();
        let observation = facts(&output);
        assert_eq!(
            (
                observation["parent"],
                observation["open"],
                observation["inode"],
                observation["rows"],
                observation["cols"]
            ),
            (
                u64::from(pid),
                0,
                directory.path().metadata().unwrap().ino(),
                31,
                97
            )
        );
        assert_eq!(
            parts.adapter.wait_for_exit(Duration::from_secs(2)).unwrap(),
            NativePtyExit::ExitCode(7)
        );
        let mut output = Vec::new();
        let exit = runner
            .run(
                &request(directory.path(), &["9"]),
                &mut |chunk| output.extend_from_slice(chunk),
                &CancellationToken::default(),
            )
            .unwrap();
        let observation = facts(&output);
        assert_eq!(
            (
                observation["parent"],
                observation["open"],
                observation["inode"],
                exit.code
            ),
            (
                u64::from(pid),
                0,
                directory.path().metadata().unwrap().ino(),
                Some(9)
            )
        );
    }
    spawner.shutdown().unwrap();
    assert!(process_identity(pid).is_err());
}

#[test]
fn terminal_programs_keep_account_groups_and_password_gated_sudo_prerequisites() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let mut parts = factory
        .create(
            launch(directory.path(), &[]),
            NativePtySize {
                rows: 24,
                columns: 80,
                ..NativePtySize::default()
            },
        )
        .unwrap();
    let mut output = Vec::new();
    parts
        .adapter
        .take_reader()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    parts.adapter.wait_for_exit(Duration::from_secs(2)).unwrap();
    let observation = facts(&output);
    assert_eq!(
        (
            observation["uid"],
            observation["gid"],
            observation["nnp"],
            observation["seccomp"]
        ),
        (
            u64::from(unsafe { libc::getuid() }),
            u64::from(unsafe { libc::getgid() }),
            0,
            0
        )
    );
    let text = std::str::from_utf8(&output).unwrap();
    let reported: Vec<u32> = text
        .lines()
        .find(|line| line.starts_with("groups"))
        .unwrap()
        .split_ascii_whitespace()
        .skip(1)
        .map(|group| group.parse().unwrap())
        .collect();
    let mut expected = vec![0u32; 256];
    let count = unsafe { libc::getgroups(expected.len() as i32, expected.as_mut_ptr()) };
    assert!(count >= 0);
    expected.truncate(count as usize);
    assert_eq!(reported, expected);
    // This test never invokes sudo. Actual password and Touch ID authorization remain owner-run.
    spawner.shutdown().unwrap();
}

#[test]
fn pty_signal_exit_and_idempotent_bounded_retirement_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let mut parts = factory
        .create(
            launch(directory.path(), &["signal"]),
            NativePtySize {
                rows: 24,
                columns: 80,
                ..NativePtySize::default()
            },
        )
        .unwrap();
    let mut output = Vec::new();
    parts
        .adapter
        .take_reader()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert_eq!(
        parts.adapter.wait_for_exit(Duration::from_secs(2)).unwrap(),
        NativePtyExit::Signal(format!("SIG{}", libc::SIGTERM))
    );
    for (arguments, expected) in [
        (&["linger"][..], NativePtyExit::GracefulShutdown),
        (&["linger", "ignore-hup"][..], NativePtyExit::ForcedShutdown),
    ] {
        let mut parts = factory
            .create(
                launch(directory.path(), arguments),
                NativePtySize {
                    rows: 24,
                    columns: 80,
                    ..NativePtySize::default()
                },
            )
            .unwrap();
        let mut reader = parts.adapter.take_reader().unwrap();
        let mut first = [0u8; 1];
        reader.read_exact(&mut first).unwrap();
        let started = Instant::now();
        parts.termination.request_termination().unwrap();
        parts.termination.request_termination().unwrap();
        assert_eq!(
            parts.adapter.wait_for_exit(Duration::from_secs(1)).unwrap(),
            expected
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    spawner.shutdown().unwrap();
}

fn ready_pty(
    factory: &CredentialFreePtyAdapterFactory,
    directory: &Path,
    arguments: &[&str],
) -> (
    spaceterm_host::pty::NativePtyAdapterParts,
    Box<dyn Read + Send>,
) {
    let mut parts = factory
        .create(
            launch(directory, arguments),
            NativePtySize {
                rows: 24,
                columns: 80,
                ..NativePtySize::default()
            },
        )
        .unwrap();
    let mut reader = parts.adapter.take_reader().unwrap();
    let mut lines = 0;
    while lines < 2 {
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        lines += usize::from(byte[0] == b'\n');
    }
    (parts, reader)
}

#[test]
fn retiring_one_pty_does_not_block_another_timed_wait() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let (mut retiring, mut reader) =
        ready_pty(&factory, directory.path(), &["linger", "report-hup"]);
    let (mut unrelated, _reader) = ready_pty(&factory, directory.path(), &["linger"]);
    let termination = retiring.termination.clone();
    let worker = std::thread::spawn(move || termination.request_termination());
    let mut hangup = [0];
    reader.read_exact(&mut hangup).unwrap();
    assert_eq!(hangup, *b"H"); // The service is now retiring the HUP-ignoring child.
    let started = Instant::now();
    let result = unrelated.adapter.wait_for_exit(Duration::from_millis(20));
    let elapsed = started.elapsed();
    worker.join().unwrap().unwrap();
    assert_eq!(
        retiring
            .adapter
            .wait_for_exit(Duration::from_secs(1))
            .unwrap(),
        NativePtyExit::ForcedShutdown
    );
    unrelated.termination.request_termination().unwrap();
    spawner.shutdown().unwrap();
    assert_eq!(
        result.unwrap_err(),
        spaceterm_host::pty::NativePtyWaitFailure::new(std::io::ErrorKind::TimedOut)
    );
    assert!(
        elapsed < Duration::from_millis(300),
        "timed wait took {elapsed:?}"
    );
}

#[test]
fn retiring_one_pty_does_not_defer_tool_deadlines_or_cancellation() {
    for cancelled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let spawner = spawner(false);
        let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        let (retiring, mut reader) =
            ready_pty(&factory, directory.path(), &["linger", "report-hup"]);
        let cancellation = CancellationToken::default();
        let mut request = request(directory.path(), &["delayed-exit"]);
        request.deadline = Some(Instant::now() + Duration::from_millis(150));
        let mut worker = None;
        let mut started = None;
        let result = runner.run(
            &request,
            &mut |_| {
                if worker.is_none() {
                    let termination = retiring.termination.clone();
                    worker = Some(std::thread::spawn(move || {
                        termination.request_termination()
                    }));
                    let mut hangup = [0];
                    reader.read_exact(&mut hangup).unwrap();
                    assert_eq!(hangup, *b"H");
                    started = Some(Instant::now());
                    if cancelled {
                        cancellation.cancel();
                    }
                }
            },
            &cancellation,
        );
        let elapsed = started.unwrap().elapsed();
        worker.unwrap().join().unwrap().unwrap();
        spawner.shutdown().unwrap();
        assert_eq!(
            result,
            Err(if cancelled {
                ProgramError::Cancelled
            } else {
                ProgramError::TimedOut
            })
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "tool abort took {elapsed:?}"
        );
    }
}

#[test]
fn oversized_and_invalid_tool_payloads_reject_without_losing_the_spawner() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut oversized = request(directory.path(), &[]);
    oversized
        .environment
        .push(("ORDINARY_TEST_VALUE".into(), "x".repeat(256 * 1024).into()));
    let mut calls = 0;
    assert_eq!(
        runner.run(
            &oversized,
            &mut |_| calls += 1,
            &CancellationToken::default()
        ),
        Err(ProgramError::Failed)
    );
    let mut invalid = request(directory.path(), &[]);
    invalid.arguments.push(OsString::from("invalid\0argument"));
    assert_eq!(
        runner.run(&invalid, &mut |_| calls += 1, &CancellationToken::default()),
        Err(ProgramError::Failed)
    );
    assert_eq!(calls, 0);
    assert!(
        runner
            .run(
                &request(directory.path(), &[]),
                &mut |_| {},
                &CancellationToken::default()
            )
            .unwrap()
            .success()
    );
    spawner.shutdown().unwrap();
}

#[test]
fn tool_deadlines_and_output_limits_reap_the_owned_program() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut timed = request(directory.path(), &["linger"]);
    timed.deadline = Some(Instant::now() + Duration::from_secs(1));
    let mut output = Vec::new();
    assert_eq!(
        runner.run(
            &timed,
            &mut |chunk| output.extend_from_slice(chunk),
            &CancellationToken::default()
        ),
        Err(ProgramError::TimedOut)
    );
    let pid = facts(&output)["pid"] as u32;
    assert!(process_identity(pid).is_err());
    let mut limited = request(directory.path(), &[]);
    limited.stdout_limit = Some(1);
    assert_eq!(
        runner.run(
            &limited,
            &mut |_| panic!("excess output must not reach the consumer"),
            &CancellationToken::default()
        ),
        Err(ProgramError::OutputTooLarge)
    );
    spawner.shutdown().unwrap();
}

#[test]
fn cancellation_during_post_exit_drain_keeps_group_authority_until_reaping() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut request = request(directory.path(), &["background"]);
    request.keep_process_group_on_exit = true;
    let cancellation = CancellationToken::default();
    let mut output = Vec::new();
    let result = runner.run(
        &request,
        &mut |chunk| {
            output.extend_from_slice(chunk);
            cancellation.cancel();
        },
        &cancellation,
    );
    assert_eq!(result, Err(ProgramError::Cancelled));
    let pid = facts(&output)["pid"] as u32;
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_identity(pid).is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Reparented zombies may remain briefly on Linux; the native host excludes exited members.
    #[cfg(target_os = "linux")]
    {
        use spaceterm_host::pty::unix_pty::UnixPtyHost;
        assert!(
            spaceterm_host::pty::linux_pty_host::LinuxPtyHost
                .observe_process(pid as i32)
                .is_none()
        );
    }
    #[cfg(target_os = "macos")]
    assert!(process_identity(pid).is_err());
    spawner.shutdown().unwrap();
}

#[test]
fn channel_rejects_a_privileged_execution_selector() {
    let (mut channel, mut child) = peer(false);
    let mut hello = [0u8; 16];
    channel.read_exact(&mut hello).unwrap();
    // There is no root/UID operation in this account-only protocol. Unknown selectors are
    // rejected without execution, not interpreted as a new privilege or executable field.
    channel
        .write_all(&[b'S', 0, 0, 0, 9, 255, 0, 0, 0, 0, 0, 0, 0, 1])
        .unwrap();
    let mut response = [0u8; 14];
    channel.read_exact(&mut response).unwrap();
    assert_eq!(response, [0, 0, 0, 10, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
    drop(channel);
    assert!(child.wait().unwrap().success());
}

#[test]
fn native_arguments_and_environment_survive_without_unicode_conversion() {
    use std::os::unix::ffi::OsStringExt;
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut request = request(directory.path(), &[]);
    request.arguments.push(OsString::from_vec(vec![0xff, 0xfe]));
    request.environment.push((
        "ORDINARY_NATIVE_BYTES".into(),
        OsString::from_vec(vec![0xfd]),
    ));
    assert!(
        runner
            .run(&request, &mut |_| {}, &CancellationToken::default())
            .unwrap()
            .success()
    );
    spawner.shutdown().unwrap();
}

#[test]
fn normal_tool_exit_can_keep_its_daemon_group() {
    use spaceterm_host::pty::unix_pty::UnixPtyHost;
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut request = request(directory.path(), &["background-short"]);
    request.keep_process_group_on_exit = true;
    let mut output = Vec::new();
    let result = runner.run(
        &request,
        &mut |chunk| output.extend_from_slice(chunk),
        &CancellationToken::default(),
    );
    assert!(result.unwrap().success());
    let pid = facts(&output)["pid"] as i32;
    #[cfg(target_os = "linux")]
    assert!(
        spaceterm_host::pty::linux_pty_host::LinuxPtyHost
            .observe_process(pid)
            .is_some()
    );
    #[cfg(target_os = "macos")]
    assert!(
        spaceterm_host::pty::macos_pty_host::MacosPtyHost
            .observe_process(pid)
            .is_some()
    );
    // This finite test daemon exits on its own. Keeping it is the existing Git fsmonitor policy.
    std::thread::sleep(Duration::from_secs(1));
    spawner.shutdown().unwrap();
}

#[test]
fn disconnect_retires_account_programs_and_reaps_the_exact_spawner() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let peer_pid = spawner.process_id();
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut output = Vec::new();
    let result = runner.run(
        &request(directory.path(), &["linger"]),
        &mut |chunk| {
            output.extend_from_slice(chunk);
            spawner.shutdown().unwrap();
        },
        &CancellationToken::default(),
    );
    assert_eq!(result, Err(ProgramError::Failed));
    assert!(process_identity(facts(&output)["pid"] as u32).is_err());
    assert!(process_identity(peer_pid).is_err());
    spawner.shutdown().unwrap();
}

#[test]
fn waiting_for_one_program_does_not_block_other_launches() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let cancellation = CancellationToken::default();
    let worker_cancel = cancellation.clone();
    let worker_runner = runner.clone();
    let worker_request = request(directory.path(), &["linger"]);
    let (ready, observed) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut reported = false;
        worker_runner.run(
            &worker_request,
            &mut |_| {
                if !reported {
                    ready.send(()).unwrap();
                    reported = true;
                }
            },
            &worker_cancel,
        )
    });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let result = runner.run(
        &request(directory.path(), &["7"]),
        &mut |_| {},
        &CancellationToken::default(),
    );
    cancellation.cancel();
    assert_eq!(worker.join().unwrap(), Err(ProgramError::Cancelled));
    assert_eq!(result.unwrap().code, Some(7));
    spawner.shutdown().unwrap();
}

#[test]
fn queued_tool_abort_does_not_spawn_or_wait_for_an_unrelated_reply() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    for cancelled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (held, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let spawns = Arc::new(AtomicUsize::new(0));
        let observed_spawns = spawns.clone();
        let mut first = true;
        let (spawner, proxy) = observed_spawner(move |operation, reply| {
            if operation == 1 && !reply {
                observed_spawns.fetch_add(1, Ordering::Relaxed);
            }
            if operation == 1 && reply && first {
                first = false;
                held.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        let active_runner = runner.clone();
        let active_request = request(directory.path(), &["7"]);
        let active = std::thread::spawn(move || {
            active_runner.run(&active_request, &mut |_| {}, &CancellationToken::default())
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut queued = request(directory.path(), &[]);
        queued.deadline = Some(Instant::now() + Duration::from_millis(100));
        let cancellation = CancellationToken::default();
        let worker_cancel = cancellation.clone();
        let cancel = std::thread::spawn(move || {
            if cancelled {
                std::thread::sleep(Duration::from_millis(40));
                worker_cancel.cancel();
            }
        });
        let started = Instant::now();
        let result = runner.run(
            &queued,
            &mut |_| panic!("queued abort must have no output"),
            &cancellation,
        );
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        cancel.join().unwrap();
        assert_eq!(active.join().unwrap().unwrap().code, Some(7));
        assert!(
            runner
                .run(
                    &request(directory.path(), &[]),
                    &mut |_| {},
                    &CancellationToken::default()
                )
                .unwrap()
                .success()
        );
        spawner.shutdown().unwrap();
        proxy.join().unwrap().unwrap();
        assert_eq!(
            result,
            Err(if cancelled {
                ProgramError::Cancelled
            } else {
                ProgramError::TimedOut
            })
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "admission abort took {elapsed:?}"
        );
        assert_eq!(spawns.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn admitted_poll_reply_does_not_extend_a_short_pty_wait() {
    for fragment in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let DelayedPollPeer {
            spawner,
            held,
            release,
            armed,
            proxy,
        } = delayed_poll_spawner(fragment);
        let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
        let (mut parts, _reader) = ready_pty(&factory, directory.path(), &["linger"]);
        armed.store(true, std::sync::atomic::Ordering::Release);
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let started = Instant::now();
            let exit = parts.adapter.wait_for_exit(Duration::from_millis(20));
            done.send((exit, started.elapsed())).unwrap();
            parts
        });
        held.recv_timeout(Duration::from_secs(5)).unwrap();
        let prompt = result.recv_timeout(Duration::from_millis(300));
        release.send(()).unwrap();
        let parts = worker.join().unwrap();
        parts.termination.request_termination().unwrap();
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        assert_eq!(
            runner
                .run(
                    &request(directory.path(), &["7"]),
                    &mut |_| {},
                    &CancellationToken::default()
                )
                .unwrap()
                .code,
            Some(7)
        );
        spawner.shutdown().unwrap();
        proxy.join().unwrap().unwrap();
        let (exit, elapsed) =
            prompt.expect("the timed wait must finish while its admitted Poll reply is held");
        assert_eq!(
            exit.unwrap_err(),
            spaceterm_host::pty::NativePtyWaitFailure::new(std::io::ErrorKind::TimedOut)
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "timed wait took {elapsed:?}"
        );
    }
}

#[test]
fn admitted_poll_reply_does_not_defer_tool_abort_or_group_retirement() {
    for cancelled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let DelayedPollPeer {
            spawner,
            held,
            release,
            armed,
            proxy,
        } = delayed_poll_spawner(false);
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        let worker_runner = runner.clone();
        let cancellation = CancellationToken::default();
        let worker_cancel = cancellation.clone();
        let mut tool = request(directory.path(), &["linger"]);
        let deadline = Instant::now() + Duration::from_secs(2);
        tool.deadline = Some(deadline);
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut output = Vec::new();
            let exit = worker_runner.run(
                &tool,
                &mut |chunk| {
                    output.extend_from_slice(chunk);
                    if output.iter().filter(|byte| **byte == b'\n').count() >= 2 {
                        armed.store(true, std::sync::atomic::Ordering::Release);
                    }
                },
                &worker_cancel,
            );
            done.send((exit, output)).unwrap();
        });
        held.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        if cancelled {
            cancellation.cancel();
        }
        let bound = if cancelled {
            Duration::from_millis(400)
        } else {
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(400)
        };
        let prompt = result.recv_timeout(bound);
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        worker.join().unwrap();
        let observed = prompt
            .as_ref()
            .ok()
            .map(|(_, output)| facts(output)["pid"] as u32);
        let reaped = observed.is_some_and(|pid| process_identity(pid).is_err());
        assert_eq!(
            runner
                .run(
                    &request(directory.path(), &["7"]),
                    &mut |_| {},
                    &CancellationToken::default()
                )
                .unwrap()
                .code,
            Some(7)
        );
        spawner.shutdown().unwrap();
        proxy.join().unwrap().unwrap();
        let (exit, _) = prompt
            .expect("tool abort and retirement must finish while its admitted Poll reply is held");
        assert_eq!(
            exit,
            Err(if cancelled {
                ProgramError::Cancelled
            } else {
                ProgramError::TimedOut
            })
        );
        assert!(
            reaped,
            "the tool leader must be reaped before returning an abort"
        );
        assert!(elapsed < bound, "tool abort took {elapsed:?}");
    }
}

#[test]
fn kept_group_reply_keeps_abort_authority_until_completion_is_accepted() {
    for cancelled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (held, observed) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let mut first = true;
        let (spawner, proxy) = observed_spawner(move |operation, reply| {
            if operation == 3 && reply && first {
                first = false;
                held.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        let worker_runner = runner.clone();
        let cancellation = CancellationToken::default();
        let worker_cancel = cancellation.clone();
        let mut tool = request(directory.path(), &["background"]);
        tool.keep_process_group_on_exit = true;
        let deadline = Instant::now() + Duration::from_secs(2);
        tool.deadline = Some(deadline);
        let worker = std::thread::spawn(move || {
            let mut output = Vec::new();
            let result = worker_runner.run(
                &tool,
                &mut |chunk| output.extend_from_slice(chunk),
                &worker_cancel,
            );
            (result, output)
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        if cancelled {
            cancellation.cancel();
        } else {
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
            );
        }
        release.send(()).unwrap();
        let (result, output) = worker.join().unwrap();
        let observations = facts(&output);
        let pid = observations["pid"] as u32;
        let leader = observations["leader"] as u32;
        let reaped = process_identity(leader).is_err();
        // The red run must not leak its deliberately persistent ordinary background program.
        // Signal that exact test process only, never the already-reaped leader's numeric group.
        let survivor = process_identity(pid).ok();
        #[cfg(target_os = "linux")]
        let alive = {
            use spaceterm_host::pty::unix_pty::UnixPtyHost;
            spaceterm_host::pty::linux_pty_host::LinuxPtyHost
                .observe_process(pid as i32)
                .is_some()
        };
        #[cfg(target_os = "macos")]
        let alive = survivor.is_some();
        if let Some(identity) = survivor
            && alive
            && process_identity(pid).ok() == Some(identity)
        {
            assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
        }
        assert_eq!(
            runner
                .run(
                    &request(directory.path(), &["7"]),
                    &mut |_| {},
                    &CancellationToken::default()
                )
                .unwrap()
                .code,
            Some(7)
        );
        spawner.shutdown().unwrap();
        proxy.join().unwrap().unwrap();
        assert_eq!(
            result,
            Err(if cancelled {
                ProgramError::Cancelled
            } else {
                ProgramError::TimedOut
            })
        );
        assert!(reaped, "the exact tool leader must be reaped");
        assert!(
            !alive,
            "an aborted kept-group completion left a background member alive"
        );
    }
}

#[test]
fn accepted_kept_group_completion_is_not_reclassified_by_a_late_abort() {
    for cancelled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let (held, observed) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (spawner, proxy) = observed_spawner(move |operation, reply| {
            if operation == 5 && reply {
                held.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
        let worker_runner = runner.clone();
        let cancellation = CancellationToken::default();
        let worker_cancel = cancellation.clone();
        let mut tool = request(directory.path(), &["background-short"]);
        tool.keep_process_group_on_exit = true;
        let deadline = Instant::now() + Duration::from_secs(2);
        tool.deadline = Some(deadline);
        let worker = std::thread::spawn(move || {
            let mut output = Vec::new();
            let result = worker_runner.run(
                &tool,
                &mut |chunk| output.extend_from_slice(chunk),
                &worker_cancel,
            );
            (result, output)
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        if cancelled {
            cancellation.cancel();
        } else {
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10),
            );
        }
        release.send(()).unwrap();
        let (result, output) = worker.join().unwrap();
        assert_eq!(result.unwrap().code, Some(0));
        assert!(process_identity(facts(&output)["leader"] as u32).is_err());
        assert_eq!(
            runner
                .run(
                    &request(directory.path(), &["7"]),
                    &mut |_| {},
                    &CancellationToken::default()
                )
                .unwrap()
                .code,
            Some(7)
        );
        // The ordinary daemon is finite and already outside spawner ownership after acceptance.
        std::thread::sleep(Duration::from_secs(1));
        spawner.shutdown().unwrap();
        proxy.join().unwrap().unwrap();
    }
}

#[test]
fn tool_stdout_eof_bounds_status_queries_and_keeps_cancellation_and_other_launches_live() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    let directory = tempfile::tempdir().unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    let observed_polls = polls.clone();
    let (spawner, proxy) = observed_spawner(move |operation, reply| {
        if operation == 2 && !reply {
            observed_polls.fetch_add(1, Ordering::Relaxed);
        }
    });
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let cancellation = CancellationToken::default();
    let worker_cancel = cancellation.clone();
    let worker_runner = runner.clone();
    let worker_request = request(directory.path(), &["close-stdout"]);
    let (ready, observed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut reported = false;
        worker_runner.run(
            &worker_request,
            &mut |_| {
                if !reported {
                    ready.send(()).unwrap();
                    reported = true;
                }
            },
            &worker_cancel,
        )
    });
    if observed.recv_timeout(Duration::from_secs(5)).is_err() {
        let result = worker.join().unwrap();
        let transport = proxy.join().unwrap().map_err(|error| error.kind());
        panic!("EOF fixture failed before output: {result:?}, transport {transport:?}");
    }
    std::thread::sleep(Duration::from_millis(250));
    let queries = polls.load(Ordering::Relaxed);
    let unrelated = runner.run(
        &request(directory.path(), &["7"]),
        &mut |_| {},
        &CancellationToken::default(),
    );
    let started = Instant::now();
    cancellation.cancel();
    let result = worker.join().unwrap();
    let elapsed = started.elapsed();
    spawner.shutdown().unwrap();
    proxy.join().unwrap().unwrap();
    assert!(
        queries <= 40,
        "EOF produced {queries} status queries in 250 ms"
    );
    assert_eq!(unrelated.unwrap().code, Some(7));
    assert_eq!(result, Err(ProgramError::Cancelled));
    assert!(
        elapsed < Duration::from_millis(300),
        "cancellation took {elapsed:?}"
    );
}

#[test]
fn missing_inherited_channel_returns_a_typed_failure_without_output() {
    let output = Command::new(env!("CARGO_BIN_EXE_spaceterm-server-spawner"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(spaceterm_host::native_deployment::contracts::NativeFailure::NativeUnavailable as i32)
    );
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_no_new_privs_refuses_before_spawner_admission() {
    let (mut channel, mut child) = restricted_peer(false, true);
    let mut hello = [0u8; 16];
    assert!(channel.read_exact(&mut hello).is_err());
    assert_eq!(
        child.wait().unwrap().code(),
        Some(
            spaceterm_host::native_deployment::contracts::NativeFailure::ExecutionBindingUnproven
                as i32
        )
    );
}

#[test]
fn pty_input_resize_and_hidden_input_use_the_existing_adapter_port() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let launch = PreparedShellLaunch::remote(
        directory.path(),
        Launch(RemoteShellLaunchParts {
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "stty -echo; printf R; IFS= read -r answer; test \"$answer\" = ordinary-test"
                    .into(),
            ],
            working_directory: directory.path().to_owned(),
            environment: vec![],
            terminal_name: "xterm-256color",
        }),
    )
    .unwrap();
    let mut parts = factory
        .create(
            launch,
            NativePtySize {
                rows: 24,
                columns: 80,
                ..NativePtySize::default()
            },
        )
        .unwrap();
    let mut ready = [0u8; 1];
    parts
        .adapter
        .take_reader()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(ready, *b"R");
    assert!(parts.adapter.hidden_input().unwrap());
    parts
        .adapter
        .resize(NativePtySize {
            rows: 40,
            columns: 100,
            pixel_width: 1000,
            pixel_height: 800,
        })
        .unwrap();
    parts.adapter.write_all(b"ordinary-test\n").unwrap();
    assert_eq!(
        parts.adapter.wait_for_exit(Duration::from_secs(2)).unwrap(),
        NativePtyExit::Success
    );
    spawner.shutdown().unwrap();
}

#[test]
fn pty_retirement_covers_separate_job_control_groups_in_its_session() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let factory = CredentialFreePtyAdapterFactory::new(spawner.clone());
    let mut parts = factory
        .create(
            launch(directory.path(), &["job-control"]),
            NativePtySize {
                rows: 24,
                columns: 80,
                ..NativePtySize::default()
            },
        )
        .unwrap();
    let mut reader = parts.adapter.take_reader().unwrap();
    let mut output = Vec::new();
    while output.iter().filter(|byte| **byte == b'\n').count() < 3 {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).unwrap();
        output.push(byte[0]);
        assert!(output.len() < 4096);
    }
    let observed = facts(&output);
    parts.termination.request_termination().unwrap();
    assert_eq!(
        parts.adapter.wait_for_exit(Duration::from_secs(2)).unwrap(),
        NativePtyExit::GracefulShutdown
    );
    assert!(process_identity(observed["pid"] as u32).is_err());
    let job = observed["job"] as u32;
    let deadline = Instant::now() + Duration::from_secs(2);
    while process_identity(job).is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(target_os = "linux")]
    {
        use spaceterm_host::pty::unix_pty::UnixPtyHost;
        assert!(
            spaceterm_host::pty::linux_pty_host::LinuxPtyHost
                .observe_process(job as i32)
                .is_none()
        );
    }
    #[cfg(target_os = "macos")]
    assert!(process_identity(job).is_err());
    spawner.shutdown().unwrap();
}

#[test]
fn tool_lookup_distinguishes_missing_executable_from_missing_directory() {
    let directory = tempfile::tempdir().unwrap();
    let spawner = spawner(false);
    let runner = CredentialFreeRepositoryProgramRunner::new(spawner.clone());
    let mut request = request(directory.path(), &[]);
    request.executable = directory.path().join("absent-program");
    assert_eq!(
        runner.run(&request, &mut |_| {}, &CancellationToken::default()),
        Err(ProgramError::NotFound)
    );
    request.directory = directory.path().join("absent-directory");
    assert_eq!(
        runner.run(&request, &mut |_| {}, &CancellationToken::default()),
        Err(ProgramError::Failed)
    );
    spawner.shutdown().unwrap();
}
