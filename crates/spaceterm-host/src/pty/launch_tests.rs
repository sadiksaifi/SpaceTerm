use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::native_pty::{
    NativePtyAdapterConstructionFailure, NativePtyCloseHandle, NativePtyExit, NativePtyOutput,
    NativePtyOutputSink, NativePtyOwner, NativePtySize, NativePtyStartupFailure,
};
use super::shell_resources_fixture::ShellResourcesFixture;
use super::unix_pty::test_factory;
use crate::shell_integration::{ShellEnvironment, ShellIntegrationMode, ShellIntegrationPolicy};
use crate::shell_launch::{ShellEnvironmentDenylist, ShellLaunchPlanner, ShellTerminalIdentity};

const PARENT_TEST: &str =
    "pty::launch_tests::native_launch_excludes_secrets_and_nonstandard_descriptors";
const PROBE_TEST: &str = "pty::launch_tests::native_launch_child_probe";
const ISOLATED_PARENT: &str = "SPACETERM_ISOLATED_LAUNCH_TEST";
const PROBE_MARKER: &str = "SPACETERM_NATIVE_LAUNCH_PROBE_COMPLETED";

struct OutputSink(Mutex<mpsc::Sender<NativePtyOutput>>);

impl NativePtyOutputSink for OutputSink {
    fn publish(&self, output: NativePtyOutput) -> bool {
        self.0.lock().unwrap().send(output).is_ok()
    }
}

#[test]
#[ignore = "runs only as a child of the native launch privacy test"]
fn native_launch_child_probe() {
    assert_eq!(
        std::env::var("SPACETERM_NATIVE_LAUNCH_PROBE").as_deref(),
        Ok("1")
    );
    for name in [
        "SPACETERM_TEST_SECRET",
        "SPACETERM_EXTRA_SECRET_TOKEN",
        "SPACETERM_SERVER_CREDENTIAL",
        "SPACETERM_SERVER_UNKNOWN_FUTURE_VALUE",
        "SPACETERM_STARTUP_CREDENTIAL_FD",
        "SPACETERM_STARTUP_UNKNOWN_FUTURE_VALUE",
        "TMUX",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "parent variable reached the shell: {name}"
        );
    }
    assert_eq!(
        std::env::var_os("SPACETERM_PROMPT_OWNER"),
        Some("fresh-owner".into())
    );
    assert_eq!(
        std::env::var_os("SPACETERM_ORDINARY_VALUE"),
        Some("preserved-value".into())
    );
    assert_eq!(
        std::env::var_os("TERM_PROGRAM_VERSION"),
        Some("injected-version".into())
    );
    assert_eq!(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
        std::path::PathBuf::from(std::env::var_os("SPACETERM_TEST_EXPECTED_WORKDIR").unwrap())
            .canonicalize()
            .unwrap()
    );
    for descriptor in 0..3 {
        // SAFETY: isatty only inspects the descriptor, including when it is closed.
        assert_eq!(
            unsafe { libc::isatty(descriptor) },
            1,
            "standard stream is not a PTY"
        );
    }
    for descriptor in 3..1_024 {
        // SAFETY: F_GETFD only inspects the descriptor, including when it is closed.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert_eq!(
            flags, -1,
            "nonstandard descriptor reached the shell: {descriptor}"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }
    eprintln!("{PROBE_MARKER}");
}

fn isolated_launch_command() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", PARENT_TEST, "--test-threads=1", "--nocapture"])
        .env(ISOLATED_PARENT, PARENT_TEST)
        .env("SPACETERM_TEST_SECRET", "parent-secret-fixture")
        .env("SPACETERM_EXTRA_SECRET_TOKEN", "parent-secret-fixture")
        .env("SPACETERM_SERVER_CREDENTIAL", "parent-secret-fixture")
        .env(
            "SPACETERM_SERVER_UNKNOWN_FUTURE_VALUE",
            "parent-secret-fixture",
        )
        .env("SPACETERM_STARTUP_CREDENTIAL_FD", "parent-secret-fixture")
        .env(
            "SPACETERM_STARTUP_UNKNOWN_FUTURE_VALUE",
            "parent-secret-fixture",
        )
        .env("SPACETERM_PROMPT_OWNER", "parent-owner")
        .env("SPACETERM_ORDINARY_VALUE", "preserved-value")
        .env("TMUX", "parent-terminal");
    command
}

fn assert_isolated_launch(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "isolated native launch test failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(PROBE_MARKER));
}

fn launch_with_descriptor_fault(mode: &str) {
    let resources = ShellResourcesFixture::new();
    let source = resources.path().join("descriptor-faults.c");
    let library = resources.path().join("descriptor-faults.so");
    std::fs::write(&source, include_str!("descriptor_faults.c")).unwrap();
    let mut compiler = Command::new("cc");
    #[cfg(target_os = "macos")]
    compiler.arg("-dynamiclib");
    #[cfg(target_os = "linux")]
    compiler.args(["-shared", "-fPIC"]);
    let output = compiler
        .args(["-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut command = isolated_launch_command();
    #[cfg(target_os = "macos")]
    command.env("DYLD_INSERT_LIBRARIES", &library);
    #[cfg(target_os = "linux")]
    command.env("LD_PRELOAD", &library);
    command.env("SPACETERM_TEST_DESCRIPTOR_FAULT", mode);
    assert_isolated_launch(&mut command);
}

#[cfg(target_os = "linux")]
fn reject_descriptor_cleanup_syscall() {
    // Install only in the isolated test process. The real kernel denies close_range in that
    // process and its PTY child; production code has no injected cleanup policy or test seam.
    let mut filter = [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as _,
            jt: 0,
            jf: 0,
            k: std::mem::offset_of!(libc::seccomp_data, nr) as _,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as _,
            jt: 0,
            jf: 1,
            k: libc::SYS_close_range as _,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as _,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as _,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filter.len() as _,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: these prctl operations only narrow this process's authority. The kernel copies the
    // complete BPF program from its live, correctly aligned storage before returning.
    unsafe {
        assert_eq!(
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0),
            0
        );
        assert_eq!(
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                &program
            ),
            0
        );
    }
}

#[test]
fn native_launch_preserves_home_fallback_for_a_missing_working_directory() {
    assert_isolated_launch(isolated_launch_command().env("SPACETERM_TEST_MISSING_WORKDIR", "1"));
}

#[test]
fn native_launch_excludes_descriptors_when_enumeration_fails() {
    launch_with_descriptor_fault("enumeration");
}

#[test]
fn native_launch_rejects_descriptor_cleanup_failure() {
    launch_with_descriptor_fault("cleanup");
}

#[test]
fn native_launch_excludes_secrets_and_nonstandard_descriptors() {
    // Environment mutation belongs to this fresh test process, not the parallel test runner.
    if std::env::var(ISOLATED_PARENT).as_deref() != Ok(PARENT_TEST) {
        assert_isolated_launch(&mut isolated_launch_command());
        return;
    }
    let fault = std::env::var("SPACETERM_TEST_DESCRIPTOR_FAULT").unwrap_or_default();
    if fault == "enumeration" {
        // Verify injection in the process that forks the shell, not just in the test driver.
        let failure = std::fs::read_dir("/dev/fd").unwrap_err();
        assert_eq!(failure.raw_os_error(), Some(libc::EACCES));
    }

    assert_eq!(
        std::env::var_os("SPACETERM_SERVER_CREDENTIAL"),
        Some("parent-secret-fixture".into())
    );
    let resources = ShellResourcesFixture::new();
    let executable = resources.path().join("launch-probe-shell");
    let started = resources.path().join("shell-started");
    std::fs::write(&executable, format!(
        "#!/bin/sh\nprintf started > \"$SPACETERM_TEST_STARTED_PATH\"\nexec \"$SPACETERM_TEST_EXECUTABLE\" --exact {PROBE_TEST} --ignored --test-threads=1 --nocapture\n"
    )).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Deliberately retain file, socket, and pipe descriptors without FD_CLOEXEC.
    // Native launch must close them independently of the source descriptor's flags.
    let file = std::fs::File::open(&executable).unwrap();
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut pipe = [-1; 2];
    // SAFETY: pipe points to two writable descriptor slots.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // SAFETY: successful pipe returned two distinct descriptors owned only by this test.
    let pipe = unsafe { [OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])] };
    let mut inherited = Vec::new();
    for descriptor in [file.as_raw_fd(), socket.as_raw_fd(), pipe[0].as_raw_fd()] {
        // SAFETY: descriptor is live, and F_DUPFD returns a new owned descriptor without CLOEXEC.
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD, 200) };
        assert!(duplicate >= 200);
        // SAFETY: each successful duplication has exactly one owner.
        inherited.push(unsafe { OwnedFd::from_raw_fd(duplicate) });
        // SAFETY: duplicate remains owned by inherited during this read-only query.
        assert_eq!(
            unsafe { libc::fcntl(duplicate, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    let missing_directory = std::env::var_os("SPACETERM_TEST_MISSING_WORKDIR").is_some();
    let working_directory = if missing_directory {
        let directory = resources.path().join("removed-directory");
        std::fs::create_dir(&directory).unwrap();
        directory
    } else {
        resources.path().to_owned()
    };
    let denylist = ShellEnvironmentDenylist::default()
        .with_name("SPACETERM_TEST_SECRET")
        .with_prefix("SPACETERM_EXTRA_SECRET_");
    let launch = ShellLaunchPlanner::new(
        executable,
        resources.path().to_owned(),
        ShellIntegrationMode::Disabled,
        ShellEnvironment::default(),
        ShellIntegrationPolicy::fixture(),
        ShellTerminalIdentity {
            fallback_term: "xterm-256color",
            colorterm: "truecolor",
            program_name: "test-terminal",
            program_version: "injected-version",
            terminfo: None,
        },
    )
    .with_environment_denylist(denylist)
    .local(&working_directory)
    .unwrap()
    .with_environment_value("HOME", resources.path().to_str().unwrap())
    .with_environment_value(
        "SPACETERM_TEST_EXPECTED_WORKDIR",
        resources.path().to_str().unwrap(),
    )
    .with_environment_value("SPACETERM_NATIVE_LAUNCH_PROBE", "1")
    .with_environment_value(
        "SPACETERM_TEST_EXECUTABLE",
        std::env::current_exe().unwrap().to_str().unwrap(),
    )
    .with_environment_value("SPACETERM_PROMPT_OWNER", "fresh-owner")
    .with_environment_value("SPACETERM_TEST_STARTED_PATH", started.to_str().unwrap());
    if missing_directory {
        // The planner validated this directory. Preserve the existing spawn-time HOME fallback
        // when a directory disappears after planning but before native process creation.
        std::fs::remove_dir(&working_directory).unwrap();
    }
    #[cfg(target_os = "linux")]
    if fault == "cleanup" {
        reject_descriptor_cleanup_syscall();
    }
    let (output, events) = mpsc::channel();
    let result = NativePtyOwner::start(
        &test_factory(),
        launch,
        NativePtySize {
            rows: 24,
            columns: 80,
            ..NativePtySize::default()
        },
        Arc::new(OutputSink(Mutex::new(output))),
        &NativePtyCloseHandle::default(),
    );
    if fault == "cleanup" {
        assert!(matches!(
            result,
            Err(NativePtyStartupFailure::Adapter(
                NativePtyAdapterConstructionFailure::ProcessStartFailed
            ))
        ));
        assert!(!started.exists(), "shell executed before cleanup failed");
        for descriptor in inherited {
            // SAFETY: descriptor remains owned during this read-only query.
            assert!(unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) } >= 0);
        }
        eprintln!("{PROBE_MARKER}");
        return;
    }
    let mut owner = result.unwrap();
    let exit = owner.wait_for_exit(Duration::from_secs(10)).unwrap();
    let mut bytes = Vec::new();
    loop {
        match events.recv_timeout(Duration::from_secs(10)).unwrap() {
            NativePtyOutput::Bytes(chunk) => bytes.extend(chunk),
            NativePtyOutput::Stopped(error) => {
                assert!(error.is_none());
                break;
            }
        }
    }
    drop(owner);
    assert_eq!(
        exit,
        NativePtyExit::Success,
        "launch probe failed:\n{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(
        bytes
            .windows(PROBE_MARKER.len())
            .any(|window| window == PROBE_MARKER.as_bytes())
    );
    // The parent retained its descriptors; child cleanup must not close the parent's resources.
    for descriptor in inherited {
        // SAFETY: descriptor remains owned during this read-only query.
        assert!(unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) } >= 0);
    }
    eprintln!("{PROBE_MARKER}");
}
