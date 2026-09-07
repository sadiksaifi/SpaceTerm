//! Native Adapter integration evidence.
use super::tests::pane_command;
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_PROBE_SCRIPT: AtomicU64 = AtomicU64::new(0);

#[test]
fn pane_command_should_launch_the_remote_shell_with_only_remote_compatibility_markers() {
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
        .args(["-c", &command.argument])
        .env_clear()
        .env("SPACETERM_REMOTE_ENVIRONMENT", &environment)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_to_string(environment).unwrap(),
        "1\ntruecolor\nunset\nunset\n"
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
