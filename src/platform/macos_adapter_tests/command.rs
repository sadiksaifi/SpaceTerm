//! Native Adapter integration evidence.
use super::tests::pane_command;
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_PROBE_SCRIPT: AtomicU64 = AtomicU64::new(0);

#[test]
fn pane_command_should_preserve_shell_umask_and_set_integration_markers() {
    let sequence = NEXT_PROBE_SCRIPT.fetch_add(1, Ordering::Relaxed);
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-remote-shell-environment-{}-{sequence}",
        std::process::id()
    ));
    let workspace = test_root.join("workspace");
    let fake_shell = test_root.join("zsh");
    let environment = test_root.join("environment");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        &fake_shell,
        br#"#!/bin/sh
printf '%s\n%s\n%s\n%s\n' \
    "${SPACETERM-unset}" \
    "${COLORTERM-unset}" \
    "${TERMINFO-unset}" \
    "${SPACETERM_SHELL_INTEGRATION_VERSION-unset}" \
    > "$SPACETERM_REMOTE_ENVIRONMENT"
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
    let command = pane_command(workspace.to_str().unwrap(), fake_shell.to_str().unwrap()).unwrap();

    let status = Command::new("/bin/sh")
        .args(["-c", &format!("umask 027; {}", command.argument)])
        .env_clear()
        .env("SPACETERM_REMOTE_ENVIRONMENT", &environment)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(&environment).unwrap(),
        "1\ntruecolor\nunset\n1\n"
    );
    assert_eq!(
        fs::metadata(environment).unwrap().permissions().mode() & 0o777,
        0o640
    );
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn posix_sh_pane_command_should_use_the_verified_login_option() {
    let sequence = NEXT_PROBE_SCRIPT.fetch_add(1, Ordering::Relaxed);
    let test_root = PathBuf::from(format!(
        "/private/tmp/spaceterm-posix-sh-{}-{sequence}",
        std::process::id()
    ));
    let workspace = test_root.join("workspace with spaces");
    let fake_bin = test_root.join("bin");
    let fake_shell = fake_bin.join("sh");
    let first_argument = test_root.join("first-argument");
    let working_directory = test_root.join("working-directory");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();
    fs::write(
        &fake_shell,
        br#"#!/bin/sh
if [ "$#" -ne 1 ] || [ "$1" != -l ]; then
    exit 64
fi
printf '%s\n' "$1" > "$SPACETERM_FIRST_ARGUMENT"
pwd -P > "$SPACETERM_WORKING_DIRECTORY"
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_shell, fs::Permissions::from_mode(0o700)).unwrap();
    let command = pane_command(workspace.to_str().unwrap(), fake_shell.to_str().unwrap()).unwrap();

    let status = Command::new("/bin/sh")
        .args(["-c", &command.argument])
        .env_clear()
        .env("SPACETERM_FIRST_ARGUMENT", &first_argument)
        .env("SPACETERM_WORKING_DIRECTORY", &working_directory)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(fs::read_to_string(first_argument).unwrap(), "-l\n");
    assert_eq!(
        fs::read_to_string(working_directory).unwrap(),
        format!("{}\n", workspace.display())
    );
    fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn child_proxy_denial_never_searches_path_and_closes_the_real_ssh_transport() {
    let sequence = NEXT_PROBE_SCRIPT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "spaceterm-proxy-denial-{}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("executed");
    for name in ["exit", "false", "exec"] {
        let command = root.join(name);
        fs::write(
            &command,
            format!("#!/bin/sh\nprintf compromised > '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(command, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let context = SshCommandContext::new(
        OpenSshExecutable::new("/usr/bin/ssh".into()).unwrap(),
        root.join("config"),
        SshDestination::new("unreachable.invalid".into()).unwrap(),
        root.join("missing.sock"),
    )
    .unwrap();
    let option = context
        .child_arguments()
        .into_iter()
        .find(|argument| argument.to_string_lossy().starts_with("ProxyCommand="))
        .unwrap();
    let output = Command::new("/usr/bin/ssh")
        .env_clear()
        .env("PATH", &root)
        .args([
            "-F",
            "/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=1",
            "-o",
        ])
        .arg(option)
        .arg("unreachable.invalid")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        !marker.exists(),
        "proxy denial executed a PATH-supplied command"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Connection closed"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_bash_should_report_navigation_and_remove_temporary_resources() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let initial = fixture.path().join("it's $(touch injected); initial");
    let next = fixture.path().join("next");
    std::fs::create_dir(&initial).unwrap();
    std::fs::create_dir(&next).unwrap();
    let command = pane_command(initial.to_str().unwrap(), "/bin/bash").unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", &command.argument])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fixture.path())
        .env("TMPDIR", fixture.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let commands = format!(
        "cd {}\nexit\n",
        quote_for_posix_shell(next.to_str().unwrap())
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(commands.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let output = String::from_utf8_lossy(&output.stdout);
    for path in [&initial, &next] {
        assert!(
            output.contains(&format!("\x1b]7;file://localhost{}\x07", path.display())),
            "missing directory report: {output:?}, stderr: {stderr:?}"
        );
    }
    assert!(!fixture.path().join("injected").exists());
    assert!(!std::fs::read_dir(fixture.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("spaceterm-shell.")
    }));
}

#[test]
fn remote_bash_should_preserve_login_profile_home_history_and_logout() {
    use std::io::Write;
    use std::process::Stdio;
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    fs::write(
        fixture.path().join(".bash_profile"),
        "printf 'PROFILE\\n'\nshopt -q login_shell && printf 'LOGIN-PROFILE\\n'\n",
    )
    .unwrap();
    fs::write(fixture.path().join(".bash_logout"), "printf 'LOGOUT\\n'\n").unwrap();
    let command = pane_command("~/", "/bin/bash").unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", &command.argument])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fixture.path())
        .env("TMPDIR", fixture.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"shopt -q login_shell && printf 'LOGIN-SESSION\\n'\nprintf 'ACCOUNT-HOME:%s\\nHISTORY:%s\\n' \"$HOME\" \"$HISTFILE\"\nexit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().filter(|line| *line == "PROFILE").count(), 1);
    assert!(stdout.contains("LOGIN-PROFILE\n"));
    assert!(stdout.contains("LOGIN-SESSION\n"));
    assert!(stdout.contains(&format!("ACCOUNT-HOME:{}\n", fixture.path().display())));
    assert!(stdout.contains(&format!(
        "HISTORY:{}/.bash_history\n",
        fixture.path().display()
    )));
    assert!(stdout.contains("LOGOUT\n"));
}

#[test]
fn remote_elvish_should_load_user_rc_and_report_directory_changes() {
    // Elvish is an optional native-test dependency, supplied explicitly to avoid PATH discovery.
    let Some(elvish) = std::env::var_os("SPACETERM_TEST_ELVISH") else {
        return;
    };
    assert_remote_shell_accepts_pty_input(&elvish, true);
}

#[test]
fn remote_bash_should_accept_delayed_pty_input() {
    assert_remote_shell_accepts_pty_input(OsStr::new("/bin/bash"), false);
}

#[test]
fn remote_posix_sh_should_preserve_user_env_and_report_pty_navigation() {
    assert_remote_shell_accepts_pty_input(OsStr::new("/bin/sh"), false);
}

fn assert_remote_shell_accepts_pty_input(shell: &OsStr, expect_user_rc: bool) {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let fixture = crate::terminal::testing::ShellResourcesFixture::new();
    let config = fixture.path().join("config");
    let next = fixture.path().join("next");
    fs::create_dir_all(config.join("elvish")).unwrap();
    fs::create_dir(&next).unwrap();
    fs::write(config.join("elvish/rc.elv"), "print USER-RC\n").unwrap();
    let posix_sh = Path::new(shell).file_name() == Some(OsStr::new("sh"));
    if posix_sh {
        fs::write(
            fixture.path().join(".profile"),
            "printf 'LOGIN-PROFILE\\n'\nexport ENV=\"$HOME/.shrc\"\n",
        )
        .unwrap();
        fs::write(fixture.path().join(".shrc"), "printf 'USER-ENV\\n'\n[ \"$ENV\" = \"$HOME/.shrc\" ] && printf 'RESTORED-ENV\\n'\nPS1='custom> '\n").unwrap();
        fs::write(
            fixture.path().join("original-env"),
            "printf 'ORIGINAL-ENV\\n'\n",
        )
        .unwrap();
    }
    let initial = fs::canonicalize(fixture.path()).unwrap();
    let next = fs::canonicalize(next).unwrap();
    let command = pane_command(initial.to_str().unwrap(), shell.to_str().unwrap()).unwrap();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut launch = portable_pty::CommandBuilder::new("/bin/sh");
    launch.args(["-c", &command.argument]);
    launch.env_clear();
    launch.env("PATH", "/usr/bin:/bin");
    launch.env("HOME", fixture.path());
    launch.env("TMPDIR", fixture.path());
    launch.env("XDG_CONFIG_HOME", &config);
    launch.env("TERM", "xterm-256color");
    if posix_sh {
        launch.env("ENV", fixture.path().join("original-env"));
    }
    let mut child = pair.slave.spawn_command(launch).unwrap();
    drop(pair.slave);
    let mut writer = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 || sender.send(bytes[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output = Vec::new();
    for (path, input) in [
        (
            initial.as_path(),
            format!("cd {}\n", quote_for_posix_shell(next.to_str().unwrap())),
        ),
        (next.as_path(), "exit\n".to_owned()),
    ] {
        let report = format!("\x1b]7;file://localhost{}\x07", path.display());
        while !String::from_utf8_lossy(&output).contains(&report) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(bytes) => output.extend(bytes),
                Err(_) => {
                    let _ = child.kill();
                    panic!("the remote shell did not report its current directory");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(child.try_wait().unwrap().is_none());
        writer.write_all(input.as_bytes()).unwrap();
    }
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the remote shell did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(writer);
    drop(pair.master);
    reader.join().unwrap();
    for bytes in receiver.try_iter() {
        output.extend(bytes);
    }
    let output = String::from_utf8_lossy(&output);
    assert_eq!(
        output.matches("USER-RC").count(),
        usize::from(expect_user_rc)
    );
    assert!(!output.contains("Exception:"));
    if posix_sh {
        assert_eq!(output.matches("LOGIN-PROFILE").count(), 1);
        assert_eq!(output.matches("USER-ENV").count(), 1);
        assert!(output.contains("RESTORED-ENV"));
        assert!(!output.contains("ORIGINAL-ENV"));
    }
    assert!(!fs::read_dir(fixture.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("spaceterm-shell.")
    }));
}
