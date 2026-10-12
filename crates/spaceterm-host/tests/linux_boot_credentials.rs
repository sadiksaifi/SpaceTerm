#![cfg(target_os = "linux")]

use spaceterm_host::{
    boot_credentials::{
        BootCredentialAdapter,
        linux::{SystemdBootCredentials, credential_name, recognize_credential_tool_output},
    },
    native_deployment::{
        account::{NativeAccountProvider, SystemAccountProvider},
        contracts::NativeFailure,
        profile::{BootCredentialBinding, LinuxCredentialMode},
    },
    startup::{AccountBuildKey, BuildIdentity},
};
use std::{
    io::Write,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};

fn pipe() -> (OwnedFd, std::fs::File) {
    let mut fd = [-1; 2];
    // SAFETY: pipe2 initializes two owned descriptors on success.
    assert_eq!(unsafe { libc::pipe2(fd.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: each descriptor is taken exactly once.
    let (read, write) = unsafe {
        (
            OwnedFd::from_raw_fd(fd[0]),
            std::fs::File::from_raw_fd(fd[1]),
        )
    };
    // Only the output reader is nonblocking; the controlled writer keeps ordinary pipe semantics.
    // SAFETY: this fixture owns the newly created endpoint and retains its flags.
    let flags = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    (read, write)
}
fn binding(mode: LinuxCredentialMode) -> BootCredentialBinding {
    BootCredentialBinding::linux(mode, [1; 32], [2; 32], [3; 16]).unwrap()
}

#[test]
fn systemd_creds_rejects_short_extra_and_fragmented_secret_output() {
    for count in [0, 1, 31, 33] {
        let (read, mut write) = pipe();
        write.write_all(&vec![17; count]).unwrap();
        drop(write);
        let outcome =
            recognize_credential_tool_output(&read, Instant::now() + Duration::from_secs(1));
        assert_eq!(
            outcome.err(),
            Some(if count < 32 {
                NativeFailure::CredentialUnavailable
            } else {
                NativeFailure::CredentialPolicyRejected
            })
        );
    }
    let (read, mut write) = pipe();
    let thread = std::thread::spawn(move || {
        write.write_all(&[17; 16]).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        let _ = write.write_all(&[17; 16]);
    });
    let started = Instant::now();
    assert_eq!(
        recognize_credential_tool_output(&read, started + Duration::from_millis(40)).err(),
        Some(NativeFailure::TimedOut)
    );
    drop(read);
    thread.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
}
#[test]
fn systemd_creds_accepts_exact_secret_only_after_eof() {
    let (read, mut write) = pipe();
    write.write_all(&[17; 32]).unwrap();
    drop(write);
    let secret =
        recognize_credential_tool_output(&read, Instant::now() + Duration::from_secs(1)).unwrap();
    assert_eq!(format!("{secret:?}"), "LockedSecret32(..)");
    let (read, mut write) = pipe();
    write.write_all(&[17; 32]).unwrap();
    assert_eq!(
        recognize_credential_tool_output(&read, Instant::now() + Duration::from_millis(20)).err(),
        Some(NativeFailure::TimedOut)
    );
    drop(write);
}
#[test]
fn systemd_creds_missing_host_key_never_initializes_on_decrypt() {
    // The production edge cannot reach any host-key or tool call before custody exists.
    // This is an unavailable-path regression, not native Host/TPM evidence.
    for mode in [LinuxCredentialMode::Host, LinuxCredentialMode::HostAndTpm2] {
        assert_eq!(
            SystemdBootCredentials.inspect_policy(&binding(mode)),
            Err(NativeFailure::CustodyUnavailable)
        );
    }
}
#[test]
fn linux_missing_issuer_and_account_incarnation_fail_before_authority_use() {
    let key = AccountBuildKey {
        account_uid: 1000,
        build_identity: BuildIdentity::Development,
    };
    assert_eq!(
        SystemAccountProvider.observe(key),
        Err(NativeFailure::AccountIdentityUnavailable)
    );
    assert_eq!(
        SystemdBootCredentials.inspect_policy(&binding(LinuxCredentialMode::Host)),
        Err(NativeFailure::CustodyUnavailable)
    );
}
#[test]
fn credential_name_is_only_fixed_domain_and_full_binding_digest() {
    let name = credential_name(&binding(LinuxCredentialMode::Host)).unwrap();
    let suffix = name.strip_prefix("spaceterm.boot.").unwrap();
    assert_eq!(suffix.len(), 64);
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_ne!(
        name,
        credential_name(&binding(LinuxCredentialMode::HostAndTpm2)).unwrap()
    );
}

#[test]
fn credential_output_rejects_descriptor_that_can_cross_an_unrelated_exec() {
    use std::os::fd::AsRawFd;
    let (read, mut write) = pipe();
    write.write_all(&[17; 32]).unwrap();
    drop(write);
    // SAFETY: this fixture owns the descriptor and intentionally weakens inheritance.
    assert_eq!(
        unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    assert_eq!(
        recognize_credential_tool_output(&read, Instant::now() + Duration::from_secs(1)).err(),
        Some(NativeFailure::UnsafeNativeState)
    );
}

#[test]
fn credential_output_rejects_blocking_pipe_even_with_an_owned_descriptor() {
    use std::os::fd::AsRawFd;
    let (read, mut write) = pipe();
    // A duplicate owns another reader of the same pipe. OwnedFd is not exclusive read authority.
    let duplicate = read.try_clone().unwrap();
    // SAFETY: the fixture owns both readers and deliberately selects blocking status.
    let flags = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) },
        0
    );
    write.write_all(&[17; 32]).unwrap();
    let mut drained = [0u8; 32];
    // SAFETY: exactly the written synthetic bytes are available for the duplicate to consume.
    assert_eq!(
        unsafe { libc::read(duplicate.as_raw_fd(), drained.as_mut_ptr().cast(), 32) },
        32
    );
    let started = Instant::now();
    assert_eq!(
        recognize_credential_tool_output(&read, started + Duration::from_millis(40)).err(),
        Some(NativeFailure::UnsafeNativeState)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(write);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn credential_output_deadline_survives_a_duplicate_reader_at_read_entry() {
    use std::{
        os::unix::process::CommandExt,
        process::{Child, Command, Stdio},
    };
    const CHILD_FLAG: &str = "SPACETERM_TEST_CREDENTIAL_READ_RACE";
    if std::env::var_os(CHILD_FLAG).is_some() {
        // SAFETY: the isolated test subprocess receives only this owned pipe as stdin.
        let read = unsafe { OwnedFd::from_raw_fd(libc::STDIN_FILENO) };
        // Exec preserved this fixture endpoint. Restore inheritance protection before recognition.
        assert_eq!(
            unsafe { libc::fcntl(read.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
        let started = Instant::now();
        assert_eq!(
            recognize_credential_tool_output(&read, started + Duration::from_millis(500)).err(),
            Some(NativeFailure::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        return;
    }
    struct Tracee {
        child: Child,
        worker: Option<libc::pid_t>,
    }
    impl Drop for Tracee {
        fn drop(&mut self) {
            let _ = self.child.kill();
            if let Some(tid) = self.worker {
                // SAFETY: reap only the exact traced fixture worker after terminating its process.
                let mut status = 0;
                unsafe {
                    libc::waitpid(tid, &mut status, libc::__WALL);
                }
            }
            let _ = self.child.wait();
        }
    }
    let (read, mut write) = pipe();
    let duplicate = read.try_clone().unwrap();
    write.write_all(&[17; 32]).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "credential_output_deadline_survives_a_duplicate_reader_at_read_entry",
            "--test-threads=1",
        ])
        .env(CHILD_FLAG, "1")
        .stdin(Stdio::from(read))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: before exec this callback makes only the async-signal-safe trace syscall.
    // It applies to this disposable test child, never to a production launcher or credential tool.
    unsafe {
        command.pre_exec(|| {
            if libc::ptrace(
                libc::PTRACE_TRACEME,
                0,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            ) < 0
            {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = Tracee {
        child: command.spawn().unwrap(),
        worker: None,
    };
    let pid = child.child.id() as libc::pid_t;
    let limit = Instant::now() + Duration::from_secs(5);
    let wait_stop = |tid: libc::pid_t| {
        loop {
            assert!(
                Instant::now() < limit,
                "trace fixture exceeded its lifetime"
            );
            let mut status = 0;
            // SAFETY: waitpid observes only the exact fixture child without blocking indefinitely.
            let result = unsafe { libc::waitpid(tid, &mut status, libc::WNOHANG | libc::__WALL) };
            assert!(result >= 0);
            if result == tid {
                assert!(
                    libc::WIFSTOPPED(status),
                    "tracee exited before the selected read"
                );
                return status;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    wait_stop(pid);
    // SAFETY: the traced child is stopped; these requests inspect/resume only that exact process.
    assert_eq!(
        unsafe {
            libc::ptrace(
                libc::PTRACE_SETOPTIONS,
                pid,
                0,
                libc::PTRACE_O_TRACESYSGOOD | libc::PTRACE_O_TRACECLONE,
            )
        },
        0
    );
    let mut tid = pid;
    loop {
        assert_eq!(unsafe { libc::ptrace(libc::PTRACE_SYSCALL, tid, 0, 0) }, 0);
        let status = wait_stop(tid);
        if (status >> 16) as u32 == libc::PTRACE_EVENT_CLONE as u32 {
            let mut worker = 0 as libc::c_ulong;
            assert_eq!(
                unsafe { libc::ptrace(libc::PTRACE_GETEVENTMSG, tid, 0, &mut worker) },
                0
            );
            assert!(worker > 0 && worker <= i32::MAX as libc::c_ulong);
            // libtest executes the one selected fixture on its worker thread. Trace that
            // exact worker; leave the main thread free to wait for the fixture's result.
            assert_eq!(unsafe { libc::ptrace(libc::PTRACE_CONT, tid, 0, 0) }, 0);
            tid = worker as libc::pid_t;
            child.worker = Some(tid);
            wait_stop(tid);
            continue;
        }
        let mut registers = std::mem::MaybeUninit::<libc::user_regs_struct>::uninit();
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_GETREGS, tid, 0, registers.as_mut_ptr()) },
            0
        );
        // SAFETY: successful GETREGS initialized the register observation.
        let registers = unsafe { registers.assume_init() };
        // x86_64 marks syscall-entry stops with -ENOSYS in the result register.
        // The initial post-exec trap can be followed by an exec exit, not an entry.
        if registers.rax == (-(libc::ENOSYS as i64)) as u64
            && registers.orig_rax == libc::SYS_read as u64
            && registers.rdi == 0
        {
            // The recognizer has polled successfully and is stopped at read's syscall entry.
            // Drain its available bytes through a duplicate before the actual read executes.
            let mut marker = [0u8; 32];
            assert_eq!(
                unsafe { libc::read(duplicate.as_raw_fd(), marker.as_mut_ptr().cast(), 32) },
                32
            );
            assert_eq!(unsafe { libc::ptrace(libc::PTRACE_CONT, tid, 0, 0) }, 0);
            break;
        }
    }
    loop {
        if let Some(worker) = child.worker {
            let mut status = 0;
            // SAFETY: this is the exact owned trace worker, whose exit must be reaped
            // before the kernel can release its process leader to Child::try_wait.
            let result =
                unsafe { libc::waitpid(worker, &mut status, libc::WNOHANG | libc::__WALL) };
            assert!(result >= 0);
            if result == worker {
                assert!(libc::WIFEXITED(status) || libc::WIFSIGNALED(status));
                child.worker = None;
            }
        }
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "nonblocking stale-readiness fixture failed"
            );
            break;
        }
        assert!(
            Instant::now() < limit,
            "credential read blocked past its absolute deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // Keep the writer alive until timeout. EOF must not supply a false successful resolution.
    drop(write);
}
