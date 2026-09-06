//! Native Adapter integration evidence.
use super::*;
use crate::platform::macos_ssh_process::MacOsSshProcessAdapter;
use crate::ssh::command::SshCommandSpec;
use crate::ssh::control_connection::SshCancellationToken;
use crate::ssh::process::SshProcessEnvironment;
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[test]
fn dropping_native_utility_future_should_cancel_and_reap_the_private_group() {
    let pid_file = PathBuf::from(format!(
        "/private/tmp/spaceterm-utility-drop-{}.pid",
        std::process::id()
    ));
    let script = format!("echo $$ > '{}'; sleep 30", pid_file.display());
    let command = Arc::new(SshCommandSpec::for_test(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), script.into()],
    ));
    let environment =
        SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
            .unwrap();
    let runner = SshRemoteUtilityProcessRunner::new(MacOsSshProcessAdapter, environment);
    let mut future = Box::pin(runner.run(
        command,
        Vec::new(),
        MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
        SshCancellationToken::default(),
    ));
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);

    assert!(matches!(
        Pin::as_mut(&mut future).poll(&mut context),
        Poll::Pending
    ));
    for _ in 0..100 {
        if pid_file.exists() {
            break;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    let process: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    drop(future);

    let terminated = (0..100).any(|_| {
        // SAFETY: signal zero checks process existence and dereferences no pointers.
        let missing = unsafe { libc::kill(process, 0) } == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if !missing {
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        missing
    });
    let _ = fs::remove_file(pid_file);
    assert!(terminated);
}

#[test]
fn completed_native_utility_should_not_cancel_the_reusable_client_token() {
    let command = Arc::new(SshCommandSpec::for_test(
        PathBuf::from("/bin/sh"),
        vec!["-s".into()],
    ));
    let environment =
        SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
            .unwrap();
    let runner = SshRemoteUtilityProcessRunner::new(MacOsSshProcessAdapter, environment);
    let cancellation = SshCancellationToken::default();

    let output = block_on_external(runner.run(
        command,
        b"printf ok\n".to_vec(),
        MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
        cancellation.clone(),
    ))
    .unwrap();

    assert!(output.exit.is_success() && output.stdout == b"ok" && !cancellation.is_cancelled());
}

#[test]
fn generated_remote_scripts_should_be_valid_posix_shell_syntax() {
    for script in [
        build_account_script(),
        build_path_script("list", "/tmp/space ' with quote").unwrap(),
        build_path_script("probe", "~/project").unwrap(),
        build_path_script("mkdir", "/tmp/-leading").unwrap(),
        build_path_script("physical", "/tmp/project").unwrap(),
    ] {
        let mut child = Command::new("/bin/sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "generated script failed syntax validation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn account_script_should_reject_a_conforming_sh_without_a_login_option() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-account-sh-reject-{}",
        std::process::id()
    ));
    let home = test_root.join("home");
    let fake_bin = test_root.join("bin");
    let fake_shell = fake_bin.join("sh");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(&fake_shell, b"#!/bin/sh\nexit 64\n").unwrap();
    fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", &fake_shell)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&build_account_script())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(
        parse_account(&output.stdout).unwrap_err(),
        RemoteUtilityError::UnsupportedLoginShell
    );
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn account_script_should_reject_relative_sh_without_executing_it() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-account-relative-sh-{}",
        std::process::id()
    ));
    let home = test_root.join("home");
    let fake_bin = test_root.join("bin");
    let fake_shell = fake_bin.join("sh");
    let execution_marker = test_root.join("executed");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(
        &fake_shell,
        format!("#!/bin/sh\ntouch '{}'\n", execution_marker.display()),
    )
    .unwrap();
    fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("SHELL", "sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&build_account_script())
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert_eq!(
        parse_account(&output.stdout).unwrap_err(),
        RemoteUtilityError::UnsupportedLoginShell
    );
    assert!(!execution_marker.exists());
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn account_script_should_record_a_supported_posix_sh_login_option() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-account-sh-accept-{}",
        std::process::id()
    ));
    let home = test_root.join("home");
    let fake_bin = test_root.join("bin");
    let fake_shell = fake_bin.join("sh");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(
        &fake_shell,
        b"#!/bin/sh\n[ \"$#\" -eq 3 ] && [ \"$1\" = -l ] && [ \"$2\" = -c ]\n",
    )
    .unwrap();
    fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", &fake_shell)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&build_account_script())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let metadata = parse_account(&output.stdout).unwrap();

    assert!(output.status.success());
    assert_eq!(
        metadata.posix_sh_login_capability(),
        PosixShLoginCapability::LoginOptionSupported
    );
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn injected_million_entry_enumerator_should_stop_at_the_examination_bound() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-list-bound-{}",
        std::process::id()
    ));
    let fake_bin = test_root.join("bin");
    let fake_find = fake_bin.join("find");
    let count_file = test_root.join("count");
    let child_file = test_root.join("ordinary-file");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(&child_file, b"not a directory").unwrap();
    fs::write(
        &fake_find,
        br#"#!/bin/sh
shift 6
index=0
while [ "$index" -lt 1000000 ]; do
    index=$((index + 1))
    printf '%s\n' "$index" > "$SPACETERM_FAKE_FIND_COUNT"
    /bin/sh -c "$3" "$4" "$5" "$6" "$SPACETERM_FAKE_CHILD"
done
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_find, fs::Permissions::from_mode(0o700)).unwrap();
    let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", "/private/tmp")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("TMPDIR", &test_root)
        .env("SPACETERM_FAKE_FIND_COUNT", &count_file)
        .env("SPACETERM_FAKE_CHILD", &child_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&script).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "bounded list script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = parse_listing(&output.stdout).unwrap();
    assert!(listing.names().is_empty());
    assert!(listing.is_truncated());
    assert_eq!(
        fs::read_to_string(&count_file).unwrap().trim(),
        (MAXIMUM_REMOTE_DIRECTORY_ENTRIES_EXAMINED + 1).to_string()
    );
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn listing_enumerator_should_be_terminated_and_waited_when_remote_shell_is_cancelled() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-list-cancel-{}",
        std::process::id()
    ));
    let fake_bin = test_root.join("bin");
    let fake_find = fake_bin.join("find");
    let pid_file = test_root.join("enumerator-pid");
    let ready_file = test_root.join("enumerator-ready");
    let stopped_file = test_root.join("enumerator-stopped");
    let _ = fs::remove_dir_all(&test_root);
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(
        &fake_find,
        br#"#!/bin/sh
printf '%s\n' "$$" > "$SPACETERM_ENUMERATOR_PID"
trap 'printf stopped > "$SPACETERM_ENUMERATOR_STOPPED"; exit 0' TERM
printf ready > "$SPACETERM_ENUMERATOR_READY"
while :; do /bin/sleep 1; done
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_find, fs::Permissions::from_mode(0o700)).unwrap();
    let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", "/private/tmp")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("TMPDIR", &test_root)
        .env("SPACETERM_ENUMERATOR_PID", &pid_file)
        .env("SPACETERM_ENUMERATOR_READY", &ready_file)
        .env("SPACETERM_ENUMERATOR_STOPPED", &stopped_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&script).unwrap();
    let readiness_deadline = Instant::now() + Duration::from_secs(10);
    while !ready_file.exists() && Instant::now() < readiness_deadline {
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    assert!(ready_file.exists(), "fake enumerator did not become ready");
    let enumerator: libc::pid_t = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // SAFETY: this signals only the child shell created by this test.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let status = child.wait().unwrap();

    assert!(!status.success());
    assert!(stopped_file.exists());
    // SAFETY: signal zero checks process existence and dereferences no pointers.
    assert_eq!(unsafe { libc::kill(enumerator, 0) }, -1);
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn generated_listing_should_preserve_argv_safe_directory_names() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-list-names-{}",
        std::process::id()
    ));
    let expected = ["Space Term", "-'quoted", ".hidden"];
    fs::create_dir_all(&test_root).unwrap();
    for name in expected {
        fs::create_dir(test_root.join(name)).unwrap();
    }
    fs::create_dir(test_root.join("line\nbreak")).unwrap();
    fs::write(test_root.join("ordinary-file"), b"ignored").unwrap();
    let script = build_path_script("list", test_root.to_str().unwrap()).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", "/private/tmp")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", "/private/tmp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&script).unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(&test_root).unwrap();

    assert!(
        output.status.success(),
        "listing script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = parse_listing(&output.stdout).unwrap();
    for name in expected {
        assert!(listing.names().iter().any(|candidate| candidate == name));
    }
    assert_eq!(listing.names().len(), expected.len());
    assert!(listing.is_truncated());
}

#[test]
fn ambiguous_mkdir_failure_should_not_claim_permission_denied() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-mkdir-failed-{}",
        std::process::id()
    ));
    let fake_bin = test_root.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let fake_mkdir = fake_bin.join("mkdir");
    fs::write(&fake_mkdir, b"#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&fake_mkdir, fs::Permissions::from_mode(0o700)).unwrap();
    let target = test_root.join("missing/child");
    let script = build_path_script("mkdir", target.to_str().unwrap()).unwrap();
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", "/private/tmp")
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&script).unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(test_root).unwrap();

    assert_eq!(
        parse_empty_success(&output.stdout, "mkdir").unwrap_err(),
        RemoteUtilityError::RemoteFailed
    );
}

#[test]
fn generated_probe_should_report_an_inaccessible_ancestor_without_claiming_missing() {
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-probe-status-{}",
        std::process::id()
    ));
    let private = test_root.join("private");
    let ordinary_file = test_root.join("ordinary-file");
    fs::create_dir_all(&private).unwrap();
    fs::write(&ordinary_file, b"not a directory").unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();

    let run_probe = |path: &std::path::Path| {
        let script = build_path_script("probe", path.to_str().unwrap()).unwrap();
        let mut child = Command::new("/bin/sh")
            .env_clear()
            .env("HOME", "/private/tmp")
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&script).unwrap();
        child.wait_with_output().unwrap()
    };

    let inaccessible = run_probe(&private.join("child"));
    let not_directory = run_probe(&ordinary_file.join("child"));
    let missing = run_probe(&test_root.join("missing/child"));
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&test_root).unwrap();

    assert_eq!(
        parse_probe(&inaccessible.stdout).unwrap_err(),
        RemoteUtilityError::PermissionDenied
    );
    assert_eq!(
        parse_probe(&not_directory.stdout).unwrap_err(),
        RemoteUtilityError::NotDirectory
    );
    assert_eq!(
        parse_probe(&missing.stdout).unwrap(),
        RemoteDirectoryProbe::Missing
    );
}

#[test]
fn native_utility_should_force_cleanup_at_its_wall_clock_deadline() {
    let command = Arc::new(SshCommandSpec::for_test(
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), "sleep 30".into()],
    ));
    let environment =
        SshProcessEnvironment::new_without_authentication(PathBuf::from("/private/tmp"), None)
            .unwrap();
    let runner = SshRemoteUtilityProcessRunner::with_timeout(
        MacOsSshProcessAdapter,
        environment,
        Duration::from_millis(20),
    );

    let error = block_on_external(runner.run(
        command,
        Vec::new(),
        MAXIMUM_REMOTE_UTILITY_OUTPUT_BYTES,
        SshCancellationToken::default(),
    ))
    .unwrap_err();

    assert!(matches!(error, RemoteUtilityRunError::TimedOut));
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

struct ThreadWake(std::thread::Thread);

impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on_external<T>(future: impl Future<Output = T>) -> T {
    let mut future = Box::pin(future);
    let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match Pin::as_mut(&mut future).poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}
