#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use spaceterm_host::ipc::PrivateIpcDirectory;
use spaceterm_host::ssh_tunnel::{
    AuthenticationMode, RemoteSocketPath, SshDestination, SshEnvironment, SshTunnel,
    SshTunnelCommand,
};
use spaceterm_host::transport::TransportError;

fn fixture() -> (tempfile::TempDir, PrivateIpcDirectory) {
    let temporary = tempfile::Builder::new()
        .prefix("st-ssh-")
        .tempdir_in("/private/tmp")
        .or_else(|_| {
            tempfile::Builder::new()
                .prefix("st-ssh-")
                .tempdir_in("/tmp")
        })
        .unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let directory = PrivateIpcDirectory::open(temporary.path()).unwrap();
    (temporary, directory)
}

fn environment(home: &std::path::Path) -> SshEnvironment {
    SshEnvironment::new(
        home.to_owned(),
        vec![("PATH".into(), "/usr/bin:/bin".into())],
    )
    .unwrap()
}

fn command(
    executable: std::path::PathBuf,
    endpoint: spaceterm_host::ipc::IpcEndpoint,
    destination: &str,
) -> SshTunnelCommand {
    SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new(destination).unwrap(),
        endpoint,
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap()
}

// Behavior tests advance Tokio time only after native fixture setup. A separate
// wall-clock watchdog still bounds hangs without charging that setup to the deadline.
async fn fixture_watchdog<F: std::future::Future>(future: F) -> Result<F::Output, ()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Ok(result),
            () = tokio::task::yield_now() => {
                if std::time::Instant::now() >= deadline {
                    return Err(());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

async fn wait_for_file(path: &std::path::Path) {
    fixture_watchdog(async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture marker watchdog expired");
}

async fn start_tunnel(
    command: SshTunnelCommand,
    environment: SshEnvironment,
    askpass: Option<std::sync::Arc<dyn spaceterm_host::ssh_tunnel::TunnelAskPass>>,
    deadline: Duration,
) -> Result<SshTunnel, TransportError> {
    static INITIALIZERS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    let initializers = INITIALIZERS.get_or_init(|| tokio::sync::Semaphore::new(3));
    // Queue before polling start, so contention cannot consume its behavior deadline.
    let _permit = fixture_watchdog(initializers.acquire())
        .await
        .unwrap()
        .unwrap();
    // Keep the paused runtime runnable while native launch and setup happen. Tokio's
    // idle auto-advance would otherwise expire the tunnel before Python can run.
    fixture_watchdog(SshTunnel::start(command, environment, askpass, deadline))
        .await
        .expect("fake-SSH launch watchdog expired")
}

#[tokio::test(start_paused = true)]
async fn fake_ssh_setup_does_not_consume_the_tunnel_start_deadline() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\npublish_marker('initializing', str(os.getpid()))\nwhile not os.path.exists('finish-setup'): time.sleep(0.001)\ntime.sleep(0.05)\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\ntime.sleep(60)",
    );
    let start = tokio::spawn(start_tunnel(
        command(executable, directory.endpoint("t").unwrap(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(1),
    ));
    wait_for_file(&temporary.path().join("initializing")).await;
    let pid: libc::pid_t = fs::read_to_string(temporary.path().join("initializing"))
        .unwrap()
        .parse()
        .unwrap();
    fs::write(temporary.path().join("finish-setup"), []).unwrap();
    let result = start.await.unwrap();
    let error = result.as_ref().err().copied();
    if let Ok(tunnel) = result {
        tunnel.cancel().await.unwrap();
    }
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(error, None, "fixture setup consumed the behavior deadline");
}

#[tokio::test(start_paused = true)]
async fn fake_ssh_initialization_is_bounded_before_the_behavior_deadline_starts() {
    let mut fixtures = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..8 {
        let (temporary, directory) = fixture();
        let executable = fake_ssh(
            temporary.path(),
            "import socket, sys, time\npublish_marker('initializing', str(os.getpid()))\nwhile not os.path.exists('finish-setup'): time.sleep(0.001)\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\ntime.sleep(60)",
        );
        starts.push(tokio::spawn(start_tunnel(
            command(executable, directory.endpoint("t").unwrap(), "host"),
            environment(temporary.path()),
            None,
            Duration::from_secs(1),
        )));
        fixtures.push(temporary);
    }
    fixture_watchdog(async {
        while !fixtures
            .iter()
            .any(|fixture| fixture.path().join("initializing").exists())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let observe_until = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < observe_until {
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
    let initializing = fixtures
        .iter()
        .filter(|fixture| fixture.path().join("initializing").exists())
        .count();
    for fixture in &fixtures {
        fs::write(fixture.path().join("finish-setup"), []).unwrap();
    }
    for start in starts {
        let tunnel = fixture_watchdog(start).await.unwrap().unwrap().unwrap();
        fixture_watchdog(tunnel.cancel()).await.unwrap().unwrap();
    }
    for fixture in &fixtures {
        let pid: libc::pid_t = fs::read_to_string(fixture.path().join("initializing"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    assert!(
        initializing <= 3,
        "unbounded fake-SSH initialization: {initializing}"
    );
}

const NEVER_BINDING_SSH: &str =
    "import os, time\npublish_marker('started', str(os.getpid()))\ntime.sleep(60)";

#[cfg(target_os = "linux")]
#[path = "support/cleanup_race.rs"]
mod cleanup_race;

#[cfg(target_os = "linux")]
#[tokio::test(start_paused = true)]
async fn cancellation_preserves_a_foreign_socket_substituted_after_the_native_inode_check() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;

    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let tunnel = start_tunnel(
        command(executable, endpoint.clone(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let pid = tunnel.process_id().unwrap() as libc::pid_t;
    let foreign_path = temporary.path().join("foreign");
    let foreign = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
    foreign
        .bind(&socket2::SockAddr::unix(&foreign_path).unwrap())
        .unwrap();
    let bound = fs::symlink_metadata(&foreign_path).unwrap();
    let race = cleanup_race::CleanupRace::arm(endpoint.path(), &foreign_path);
    assert_eq!(
        tunnel.cancel().await.err(),
        Some(TransportError::UnsafeEndpoint)
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    race.restore_preserved_entry();
    let retained = fs::symlink_metadata(endpoint.path())
        .expect("owned peer proof cannot authorize removing a replaced deletion target");
    assert_eq!((retained.dev(), retained.ino()), (bound.dev(), bound.ino()));
    foreign.listen(8).unwrap();
    let mut client = std::os::unix::net::UnixStream::connect(endpoint.path()).unwrap();
    client.write_all(b"retained").unwrap();
    let (accepted, _) = foreign.accept().unwrap();
    let mut accepted = std::os::unix::net::UnixStream::from(accepted);
    accepted
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut message = [0; 8];
    accepted.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"retained");
}

#[tokio::test(start_paused = true)]
async fn fake_ssh_publishes_complete_pid_markers_before_they_become_visible() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        &format!(
            r#"import builtins, os, time
original_open = builtins.open
def gated_open(name, *args, **kwargs):
    file = original_open(name, *args, **kwargs)
    if name in ('started', 'started.pending'):
        os.close(os.open('writing-pid', os.O_CREAT | os.O_WRONLY, 0o600))
        while not os.path.exists('finish-pid'): time.sleep(0.001)
    return file
builtins.open = gated_open
{NEVER_BINDING_SSH}"#
        ),
    );
    let start = tokio::spawn(start_tunnel(
        command(executable, directory.endpoint("t").unwrap(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(10),
    ));
    wait_for_file(&temporary.path().join("writing-pid")).await;
    let exposed_incomplete_marker = temporary.path().join("started").exists();
    fs::write(temporary.path().join("finish-pid"), []).unwrap();
    let pid: libc::pid_t = fixture_watchdog(async {
        loop {
            if let Ok(pid) = fs::read_to_string(temporary.path().join("started"))
                .unwrap_or_default()
                .parse()
            {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    start.abort();
    assert!(start.await.err().unwrap().is_cancelled());
    fixture_watchdog(async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !exposed_incomplete_marker,
        "the awaited PID marker was visible before its contents were written"
    );
}

fn fake_ssh(home: &std::path::Path, body: &str) -> std::path::PathBuf {
    let executable = home.join("ssh");
    fs::write(
        &executable,
        format!(
            "#!/usr/bin/python3\nimport os\ndef publish_marker(name, value):\n    with open(name + '.pending', 'w') as file: file.write(value)\n    os.replace(name + '.pending', name)\n{body}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

#[test]
fn tunnel_argv_is_one_validated_forward_without_a_shell_command() {
    let (_temporary, directory) = fixture();
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        "/usr/bin/ssh".into(),
        "/dev/null".into(),
        SshDestination::new("user@host").unwrap(),
        endpoint.clone(),
        RemoteSocketPath::new("/run/user/501/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let arguments = command.arguments();
    let forward = format!("{}:/run/user/501/spaceterm/s", endpoint.path().display());
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == [OsString::from("-L"), OsString::from(&forward)])
    );
    for option in [
        "BatchMode=yes",
        "ExitOnForwardFailure=yes",
        "StreamLocalBindUnlink=no",
        "ControlMaster=no",
        "ControlPath=none",
        "ControlPersist=no",
        "ForkAfterAuthentication=no",
        "PermitLocalCommand=no",
        "ForwardAgent=no",
        "ForwardX11=no",
        "RemoteCommand=none",
    ] {
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == [OsString::from("-o"), OsString::from(option)])
        );
    }
    assert_eq!(
        &arguments[arguments.len() - 2..],
        &[OsString::from("--"), OsString::from("user@host")]
    );
    assert!(
        !arguments
            .iter()
            .any(|argument| argument == "ClearAllForwardings=yes")
    );
}

#[test]
fn tunnel_rejects_option_shell_and_forwarding_delimiter_injection() {
    for destination in [
        "",
        "-oProxyCommand=x",
        "host;touch",
        "user@host\n",
        "host$(id)",
        "host path",
    ] {
        assert_eq!(
            SshDestination::new(destination).err(),
            Some(TransportError::InvalidConfiguration)
        );
    }
    for socket in [
        "relative",
        "/tmp/a:b",
        "/tmp/../s",
        "/tmp/s\n",
        "/tmp/[s]",
        "/tmp/a\\b",
    ] {
        assert_eq!(
            RemoteSocketPath::new(socket).err(),
            Some(TransportError::InvalidConfiguration)
        );
    }
    assert_eq!(
        SshEnvironment::new(
            "/tmp".into(),
            vec![("LD_PRELOAD".into(), "/untrusted".into())]
        )
        .err(),
        Some(TransportError::InvalidConfiguration)
    );
}

#[tokio::test(start_paused = true)]
async fn tunnel_drop_reaps_its_process_and_removes_its_registered_endpoint() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\nassert 'LD_PRELOAD' not in __import__('os').environ\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint.clone(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let tunnel = start_tunnel(
        command,
        environment(temporary.path()),
        None,
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    let pid = tunnel.process_id().unwrap();
    let completion = tunnel.completion();
    drop(tunnel);
    fixture_watchdog(completion.wait()).await.unwrap().unwrap();
    assert!(
        !endpoint.path().exists(),
        "registered forwarding socket leaked after cleanup"
    );
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test(start_paused = true)]
async fn ssh_exit_before_listener_readiness_preserves_its_unverified_socket() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\nsys.exit(19)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint.clone(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    assert_eq!(
        start_tunnel(
            command,
            environment(temporary.path()),
            None,
            Duration::from_secs(3),
        )
        .await
        .err(),
        Some(TransportError::TunnelExited)
    );
    assert!(endpoint.path().exists());
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_tunnel_does_not_unlink_a_replacement_socket() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint.clone(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let tunnel = start_tunnel(
        command,
        environment(temporary.path()),
        None,
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    fs::rename(endpoint.path(), endpoint.path().with_file_name("old")).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
    let cancelled = tunnel.cancel().await;
    assert_eq!(cancelled.err(), Some(TransportError::UnsafeEndpoint));
    assert!(endpoint.path().exists());
    drop(replacement);
}

#[tokio::test(start_paused = true)]
async fn a_tunnel_start_deadline_terminates_and_reaps_a_process_that_never_binds() {
    let (temporary, directory) = fixture();
    let executable = temporary.path().join("ssh");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s' \"$$\" >pid.pending\nmv pid.pending pid\nexec /bin/sleep 60\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint,
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let start = tokio::spawn(start_tunnel(
        command,
        environment(temporary.path()),
        None,
        Duration::from_secs(2),
    ));
    wait_for_file(&temporary.path().join("pid")).await;
    let pid: libc::pid_t = fs::read_to_string(temporary.path().join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        start.await.unwrap().err(),
        Some(TransportError::HandshakeExpired)
    );
    fixture_watchdog(async {
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test(start_paused = true)]
async fn askpass_is_injected_only_for_its_mode_and_the_broker_lease_lasts_through_cleanup() {
    use spaceterm_host::ssh_tunnel::{AskPassHandoff, TunnelAskPass};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct AskPassFixture {
        released: Arc<AtomicBool>,
    }
    impl TunnelAskPass for AskPassFixture {
        fn handoff(&self) -> Result<AskPassHandoff, TransportError> {
            AskPassHandoff::new(
                "/usr/bin/true".into(),
                "00000123:/tmp/broker".into(),
                zeroize::Zeroizing::new(vec![b'a'; 64]),
            )
        }
    }
    impl Drop for AskPassFixture {
        fn drop(&mut self) {
            self.released.store(true, Ordering::Release);
        }
    }
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import os, socket, sys, time\nassert os.environ['SPACETERM_SSH_ASKPASS_MODE'] == 'broker-v1'\nassert os.environ['SPACETERM_SSH_ASKPASS_CAPABILITY'] == 'a' * 64\nassert os.environ['SSH_ASKPASS_REQUIRE'] == 'force'\nassert os.environ['HOME'] == os.getcwd()\nassert os.environ['PATH'] == '/usr/bin:/bin'\nassert 'LD_PRELOAD' not in os.environ\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint,
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::AskPass,
    )
    .unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let provider: Arc<dyn TunnelAskPass> = Arc::new(AskPassFixture {
        released: Arc::clone(&released),
    });
    let tunnel = start_tunnel(
        command,
        environment(temporary.path()),
        Some(provider),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert!(!released.load(Ordering::Acquire));
    tunnel.cancel().await.unwrap();
    assert!(released.load(Ordering::Acquire));
}

struct AttemptAskPassFixture {
    cancellations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl spaceterm_host::ssh_tunnel::TunnelAskPass for AttemptAskPassFixture {
    fn handoff(&self) -> Result<spaceterm_host::ssh_tunnel::AskPassHandoff, TransportError> {
        let cancellations = self.cancellations.clone();
        Ok(spaceterm_host::ssh_tunnel::AskPassHandoff::new(
            "/usr/bin/true".into(),
            "00000123:/tmp/broker".into(),
            zeroize::Zeroizing::new(vec![b'a'; 64]),
        )?
        .with_attempt_lifetime(move || {
            cancellations.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn askpass_attempt_lifetime_ends_when_forwarding_is_ready() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let cancellations = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn spaceterm_host::ssh_tunnel::TunnelAskPass> =
        Arc::new(AttemptAskPassFixture {
            cancellations: Arc::clone(&cancellations),
        });
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        directory.endpoint("t").unwrap(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::AskPass,
    )
    .unwrap();
    let tunnel = start_tunnel(
        command,
        environment(temporary.path()),
        Some(provider.clone()),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    // The application still owns its provider and the tunnel remains alive.
    // Neither may extend interactive authority beyond this authentication attempt.
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    tunnel.cancel().await.unwrap();
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    drop(provider);
}

#[tokio::test(start_paused = true)]
async fn askpass_attempt_lifetime_ends_on_dial_timeout_while_provider_is_shared() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (temporary, directory) = fixture();
    let executable = temporary.path().join("pending-ssh");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s' \"$$\" >pid.pending\nmv pid.pending pid\nexec /bin/sleep 60\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let cancellations = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn spaceterm_host::ssh_tunnel::TunnelAskPass> =
        Arc::new(AttemptAskPassFixture {
            cancellations: Arc::clone(&cancellations),
        });
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        directory.endpoint("t").unwrap(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::AskPass,
    )
    .unwrap();
    let start = tokio::spawn(start_tunnel(
        command,
        environment(temporary.path()),
        Some(provider.clone()),
        Duration::from_secs(2),
    ));
    wait_for_file(&temporary.path().join("pid")).await;
    assert_eq!(cancellations.load(Ordering::Acquire), 0);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        start.await.unwrap().err(),
        Some(TransportError::HandshakeExpired)
    );
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    drop(provider);
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn askpass_mode_has_no_batch_or_mock_credential_fallback() {
    let (temporary, directory) = fixture();
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        "/usr/bin/ssh".into(),
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint,
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::AskPass,
    )
    .unwrap();
    assert_eq!(
        start_tunnel(
            command,
            environment(temporary.path()),
            None,
            Duration::from_secs(1)
        )
        .await
        .err(),
        Some(TransportError::InvalidConfiguration)
    );
}

#[test]
fn tunnel_cleanup_survives_shutdown_of_the_async_executor() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint.clone(),
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    let tunnel = runtime
        .block_on(start_tunnel(
            command,
            environment(temporary.path()),
            None,
            Duration::from_secs(3),
        ))
        .unwrap();
    let completion = tunnel.completion();
    drop(tunnel);
    drop(runtime);
    let observer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap();
    observer.block_on(async { fixture_watchdog(completion.wait()).await.unwrap().unwrap() });
    assert!(
        !endpoint.path().exists(),
        "registered forwarding socket leaked after cleanup"
    );
}

#[tokio::test(start_paused = true)]
async fn overlapping_starts_cannot_adopt_or_remove_the_winning_forwarding_socket() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import os, socket, sys, time\nrole = sys.argv[-1]\npublish_marker(role + '-started', str(os.getpid()))\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\nif role == 'winner':\n    while not os.path.exists('bind-winner'): time.sleep(0.001)\n    s = socket.socket(socket.AF_UNIX)\n    s.bind(p)\n    s.listen(8)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let winner = tokio::spawn(start_tunnel(
        command(executable.clone(), endpoint.clone(), "winner"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    ));
    wait_for_file(&temporary.path().join("winner-started")).await;
    // A separate directory open must share the lease, not just clones of one Arc.
    let second_directory = PrivateIpcDirectory::open(temporary.path()).unwrap();
    let loser = tokio::spawn(start_tunnel(
        command(executable, second_directory.endpoint("t").unwrap(), "loser"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    ));
    fixture_watchdog(async {
        while !loser.is_finished() && !temporary.path().join("loser-started").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fs::write(temporary.path().join("bind-winner"), []).unwrap();
    let winner = winner.await.unwrap().unwrap();
    let loser = loser.await.unwrap();
    let rejected = loser.is_err();
    if let Ok(loser) = loser {
        loser.cancel().await.unwrap();
    }
    let winner_retained = endpoint.path().exists();
    let no_losing_process = !temporary.path().join("loser-started").exists();
    winner.cancel().await.unwrap();
    assert!(
        rejected,
        "the losing start reported readiness from another process"
    );
    assert!(
        winner_retained,
        "the losing start removed the winner's socket"
    );
    assert!(
        no_losing_process,
        "the endpoint must be reserved before spawning"
    );
}

#[tokio::test(start_paused = true)]
async fn startup_rejects_a_foreign_same_account_listener_without_unlinking_it() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(temporary.path(), NEVER_BINDING_SSH);
    let endpoint = directory.endpoint("t").unwrap();
    let start = tokio::spawn(start_tunnel(
        command(executable, endpoint.clone(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    ));
    wait_for_file(&temporary.path().join("started")).await;
    let foreign = std::os::unix::net::UnixListener::bind(endpoint.path()).unwrap();
    let result = start.await.unwrap();
    let rejected = result.as_ref().err().copied();
    if let Ok(tunnel) = result {
        tunnel.cancel().await.unwrap();
    }
    assert_eq!(rejected, Some(TransportError::UnsafeEndpoint));
    assert!(
        endpoint.path().exists(),
        "a foreign listener is not ours to clean"
    );
    drop(foreign);
}

#[tokio::test(start_paused = true)]
async fn deadline_preserves_a_foreign_socket_bound_without_listening() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;

    let (temporary, directory) = fixture();
    let executable = fake_ssh(temporary.path(), NEVER_BINDING_SSH);
    let endpoint = directory.endpoint("t").unwrap();
    let start = tokio::spawn(start_tunnel(
        command(executable, endpoint.clone(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    ));
    wait_for_file(&temporary.path().join("started")).await;
    let pid: libc::pid_t = fs::read_to_string(temporary.path().join("started"))
        .unwrap()
        .parse()
        .unwrap();
    // This actor bypasses the advisory endpoint lease with a native bind. Keep its
    // socket live but refusing connections until the owned SSH process is reaped.
    let foreign = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
    foreign
        .bind(&socket2::SockAddr::unix(endpoint.path()).unwrap())
        .unwrap();
    let bound = fs::symlink_metadata(endpoint.path()).unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(
        start.await.unwrap().err(),
        Some(TransportError::HandshakeExpired)
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let retained = fs::symlink_metadata(endpoint.path())
        .expect("an unverified refusing socket is not ours to remove");
    assert_eq!((retained.dev(), retained.ino()), (bound.dev(), bound.ino()));
    foreign.listen(8).unwrap();
    foreign
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut client = std::os::unix::net::UnixStream::connect(endpoint.path()).unwrap();
    client.write_all(b"retained").unwrap();
    let (accepted, _) = foreign.accept().unwrap();
    let mut accepted = std::os::unix::net::UnixStream::from(accepted);
    accepted
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut message = [0; 8];
    accepted.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"retained");
}

#[tokio::test(start_paused = true)]
async fn deadline_preserves_an_unverified_owned_socket_and_rejects_retry() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import os, socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\npublish_marker('bound', str(os.getpid()))\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let start = tokio::spawn(start_tunnel(
        command(executable.clone(), endpoint.clone(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    ));
    wait_for_file(&temporary.path().join("bound")).await;
    let pid: libc::pid_t = fs::read_to_string(temporary.path().join("bound"))
        .unwrap()
        .parse()
        .unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    let result = start.await.unwrap();
    let error = result.as_ref().err().copied();
    if let Ok(tunnel) = result {
        tunnel.cancel().await.unwrap();
    }
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "the deadline returned before process cleanup"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(error, Some(TransportError::HandshakeExpired));
    assert!(
        endpoint.path().exists(),
        "a refusing socket has no portable owned-process provenance"
    );
    assert_eq!(
        start_tunnel(
            command(executable, endpoint, "retry"),
            environment(temporary.path()),
            None,
            Duration::from_secs(5),
        )
        .await
        .err(),
        Some(TransportError::UnsafeEndpoint)
    );
}

mod socket_provenance {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct ConnectGate {
        endpoint: PathBuf,
        owned: PathBuf,
        foreign: PathBuf,
        identity: (u64, u64),
        swapped: Arc<AtomicBool>,
    }

    static CONNECT_GATE: Mutex<Option<ConnectGate>> = Mutex::new(None);

    // Substitute the owned listener only for the native connect, restoring the
    // foreign inode before the supervisor's peer check and registration. This gate
    // belongs to the test executable, not to the production transport adapter.
    #[unsafe(export_name = "connect")]
    unsafe extern "C" fn connect_socket(
        socket: libc::c_int,
        address: *const libc::sockaddr,
        length: libc::socklen_t,
    ) -> libc::c_int {
        // SAFETY: callers of native connect supply a readable address of this length.
        let path = unsafe {
            if !address.is_null()
                && length as usize > std::mem::offset_of!(libc::sockaddr_un, sun_path)
                && (*address).sa_family as libc::c_int == libc::AF_UNIX
            {
                let bytes = std::slice::from_raw_parts(
                    address
                        .cast::<u8>()
                        .add(std::mem::offset_of!(libc::sockaddr_un, sun_path)),
                    (length as usize - std::mem::offset_of!(libc::sockaddr_un, sun_path)).min(104),
                );
                Some(Path::new(std::ffi::OsStr::from_bytes(
                    bytes.split(|byte| *byte == 0).next().unwrap_or_default(),
                )))
            } else {
                None
            }
        };
        let gate = CONNECT_GATE.lock().ok().and_then(|mut gate| {
            let matches = gate.as_ref().is_some_and(|gate| {
                path.is_some_and(|path| {
                    path == gate.endpoint
                        || (path.starts_with("/proc/self/fd")
                            && fs::metadata(path).is_ok_and(|metadata| {
                                (metadata.dev(), metadata.ino()) == gate.identity
                            }))
                })
            });
            if matches { gate.take() } else { None }
        });
        if let Some(gate) = &gate {
            fs::rename(&gate.endpoint, &gate.foreign).unwrap();
            fs::rename(&gate.owned, &gate.endpoint).unwrap();
        }
        // SAFETY: the original native arguments remain live and unchanged.
        let result = unsafe { native_connect(socket, address, length) };
        if let Some(gate) = gate {
            // SAFETY: the native thread-local errno pointer remains live.
            let error = unsafe { *errno() };
            fs::rename(&gate.endpoint, &gate.owned).unwrap();
            fs::rename(&gate.foreign, &gate.endpoint).unwrap();
            gate.swapped.store(true, Ordering::Release);
            // SAFETY: preserve the native connect result across test-only renames.
            unsafe { *errno() = error };
        }
        result
    }

    #[cfg(target_os = "linux")]
    unsafe fn native_connect(
        socket: libc::c_int,
        address: *const libc::sockaddr,
        length: libc::socklen_t,
    ) -> libc::c_int {
        // SAFETY: forward the original connect arguments to the actual syscall.
        unsafe { libc::syscall(libc::SYS_connect, socket, address, length) as libc::c_int }
    }

    #[cfg(target_os = "macos")]
    unsafe fn native_connect(
        socket: libc::c_int,
        address: *const libc::sockaddr,
        length: libc::socklen_t,
    ) -> libc::c_int {
        type Connect = unsafe extern "C" fn(
            libc::c_int,
            *const libc::sockaddr,
            libc::socklen_t,
        ) -> libc::c_int;
        static CONNECT: std::sync::OnceLock<Connect> = std::sync::OnceLock::new();
        let connect = CONNECT.get_or_init(|| {
            // SAFETY: RTLD_NEXT resolves the system connect, not this test definition.
            let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"connect".as_ptr()) };
            assert!(!symbol.is_null());
            // SAFETY: the native symbol has connect's declared function signature.
            unsafe { std::mem::transmute::<*mut libc::c_void, Connect>(symbol) }
        });
        // SAFETY: forward the original connect arguments to the native function.
        unsafe { connect(socket, address, length) }
    }

    #[cfg(target_os = "linux")]
    unsafe fn errno() -> *mut libc::c_int {
        // SAFETY: libc returns the current thread's errno pointer.
        unsafe { libc::__errno_location() }
    }

    #[cfg(target_os = "macos")]
    unsafe fn errno() -> *mut libc::c_int {
        // SAFETY: libc returns the current thread's errno pointer.
        unsafe { libc::__error() }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_preserves_a_foreign_inode_substituted_around_the_owned_peer_probe() {
        let (temporary, directory) = fixture();
        let executable = fake_ssh(
            temporary.path(),
            "import socket, time\ns = socket.socket(socket.AF_UNIX)\ns.bind('owned')\ns.listen(8)\npublish_marker('owned-bound', str(os.getpid()))\ntime.sleep(60)",
        );
        let endpoint = directory.endpoint("t").unwrap();
        let start = tokio::spawn(start_tunnel(
            command(executable, endpoint.clone(), "host"),
            environment(temporary.path()),
            None,
            Duration::from_secs(5),
        ));
        wait_for_file(&temporary.path().join("owned-bound")).await;
        let pid: libc::pid_t = fs::read_to_string(temporary.path().join("owned-bound"))
            .unwrap()
            .parse()
            .unwrap();
        let foreign =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        foreign
            .bind(&socket2::SockAddr::unix(endpoint.path()).unwrap())
            .unwrap();
        let bound = fs::symlink_metadata(endpoint.path()).unwrap();
        let swapped = Arc::new(AtomicBool::new(false));
        *CONNECT_GATE.lock().unwrap() = Some(ConnectGate {
            endpoint: endpoint.path().to_owned(),
            owned: temporary.path().join("owned"),
            foreign: temporary.path().join("foreign"),
            identity: (bound.dev(), bound.ino()),
            swapped: Arc::clone(&swapped),
        });
        // Let the gate exercise the owned-peer probe before advancing expiration.
        fixture_watchdog(async {
            while !swapped.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        match start.await.unwrap() {
            Ok(tunnel) => {
                tunnel.cancel().await.unwrap();
            }
            Err(error) => {
                #[cfg(target_os = "linux")]
                assert_eq!(error, TransportError::HandshakeExpired);
                #[cfg(target_os = "macos")]
                assert_eq!(error, TransportError::UnsafeEndpoint);
            }
        }
        CONNECT_GATE.lock().unwrap().take();
        assert!(
            swapped.load(Ordering::Acquire),
            "the native connect gate did not run"
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let retained = fs::symlink_metadata(endpoint.path())
            .expect("a peer PID from a different socket cannot authorize removing this inode");
        assert_eq!((retained.dev(), retained.ino()), (bound.dev(), bound.ino()));
        foreign.listen(8).unwrap();
        let mut client = std::os::unix::net::UnixStream::connect(endpoint.path()).unwrap();
        client.write_all(b"retained").unwrap();
        let (accepted, _) = foreign.accept().unwrap();
        let mut accepted = std::os::unix::net::UnixStream::from(accepted);
        accepted
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut message = [0; 8];
        accepted.read_exact(&mut message).unwrap();
        assert_eq!(&message, b"retained");
    }
}

#[cfg(target_os = "linux")]
mod cancellation_binding {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct BindGate {
        pid: libc::pid_t,
        release: PathBuf,
        bound: PathBuf,
    }

    static BIND_GATE: Mutex<Option<BindGate>> = Mutex::new(None);

    // The test executable alone interposes this native boundary. Delay one owned
    // group's stop, after the supervisor's last readiness inspection, until its real
    // fake-SSH process has bound. All other signals use the actual syscall.
    // No production process adapter or scheduling hook is added for this regression.
    #[unsafe(export_name = "kill")]
    extern "C" fn signal_process(pid: libc::pid_t, signal: libc::c_int) -> libc::c_int {
        let gate = BIND_GATE.lock().ok().and_then(|mut gate| {
            if gate
                .as_ref()
                .is_some_and(|gate| pid == -gate.pid && signal == libc::SIGSTOP)
            {
                gate.take()
            } else {
                None
            }
        });
        if let Some(gate) = gate {
            let _ = fs::write(gate.release, []);
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !gate.bound.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // SAFETY: kill takes scalar process/signal arguments, without pointer preconditions.
        unsafe { libc::syscall(libc::SYS_kill, pid, signal) as libc::c_int }
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_removes_a_registered_socket_bound_during_the_final_ownership_probe() {
        let (temporary, directory) = fixture();
        let executable = fake_ssh(
            temporary.path(),
            "import os, socket, sys, time\npublish_marker('started', str(os.getpid()))\nif sys.argv[-1] != 'retry':\n    while not os.path.exists('bind-during-cancel'): time.sleep(0.001)\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\npublish_marker('bound-during-cancel', 'bound')\ntime.sleep(60)",
        );
        let endpoint = directory.endpoint("t").unwrap();
        let start = tokio::spawn(start_tunnel(
            command(executable.clone(), endpoint.clone(), "host"),
            environment(temporary.path()),
            None,
            Duration::from_secs(5),
        ));
        wait_for_file(&temporary.path().join("started")).await;
        let pid: libc::pid_t = fs::read_to_string(temporary.path().join("started"))
            .unwrap()
            .parse()
            .unwrap();
        *BIND_GATE.lock().unwrap() = Some(BindGate {
            pid,
            release: temporary.path().join("bind-during-cancel"),
            bound: temporary.path().join("bound-during-cancel"),
        });
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            start.await.unwrap().err(),
            Some(TransportError::HandshakeExpired)
        );
        assert!(
            temporary.path().join("bound-during-cancel").exists(),
            "the native signal boundary must coordinate the late bind"
        );
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(
            !endpoint.path().exists(),
            "the stopped child and pinned inode prove ownership before reaping"
        );
        let retry = start_tunnel(
            command(executable, endpoint, "retry"),
            environment(temporary.path()),
            None,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        retry.cancel().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn endpoint_lease_excludes_another_process_before_the_owner_has_bound() {
    const FIXTURE: &str = "SPACETERM_TRANSPORT_LEASE_TEST";
    if let Some(home) = std::env::var_os(FIXTURE) {
        // The parent invokes this exact test in another process with its private
        // fixture route. This branch is test composition, never production startup.
        let home = std::path::PathBuf::from(home);
        let directory = PrivateIpcDirectory::open(&home).unwrap();
        let result = start_tunnel(
            command(home.join("ssh"), directory.endpoint("t").unwrap(), "loser"),
            environment(&home),
            None,
            Duration::from_secs(5),
        )
        .await;
        let error = result.as_ref().err().copied();
        if let Ok(tunnel) = result {
            tunnel.cancel().await.unwrap();
        }
        assert_eq!(error, Some(TransportError::UnsafeEndpoint));
        return;
    }
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import os, socket, sys, time\nif sys.argv[-1] == 'winner':\n    publish_marker('winner-started', 'started')\n    while not os.path.exists('bind-winner'): time.sleep(0.001)\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\ntime.sleep(60)",
    );
    let owner = tokio::spawn(start_tunnel(
        command(executable, directory.endpoint("t").unwrap(), "winner"),
        environment(temporary.path()),
        None,
        Duration::from_secs(10),
    ));
    wait_for_file(&temporary.path().join("winner-started")).await;
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "endpoint_lease_excludes_another_process_before_the_owner_has_bound",
        ])
        .env_clear()
        .env(FIXTURE, temporary.path())
        .status()
        .unwrap();
    fs::write(temporary.path().join("bind-winner"), []).unwrap();
    owner.await.unwrap().unwrap().cancel().await.unwrap();
    assert!(
        status.success(),
        "the native lease did not exclude another process"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unrelated_fork_cannot_retain_a_finished_tunnels_endpoint_lease() {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, sys, time\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(8)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let tunnel = start_tunnel(
        command(executable.clone(), endpoint.clone(), "host"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let (mut parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let fork = std::thread::spawn(move || {
        let mut native = std::process::Command::new("/usr/bin/true");
        native.env_clear();
        // SAFETY: the child uses only async-signal-safe read/write on an inherited
        // socket. It signals fork completion, then waits before exec can close leases.
        unsafe {
            native.pre_exec(move || {
                let mut byte = 1_u8;
                if libc::write(child.as_raw_fd(), (&raw const byte).cast(), 1) != 1
                    || libc::read(child.as_raw_fd(), (&raw mut byte).cast(), 1) != 1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        native.spawn().unwrap().wait().unwrap()
    });
    parent.read_exact(&mut [0]).unwrap();
    tunnel.cancel().await.unwrap();
    assert!(
        !endpoint.path().exists(),
        "awaited cleanup removes the registered socket before releasing its lease"
    );
    let retry = start_tunnel(
        command(executable, endpoint, "retry"),
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    )
    .await;
    // Always release/reap the unrelated child before asserting the regression result.
    parent.write_all(&[1]).unwrap();
    assert!(fork.join().unwrap().success());
    retry
        .expect("a forked child's pre-exec descriptor must not retain our lease")
        .cancel()
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_terminates_the_entire_private_process_group() {
    let (temporary, directory) = fixture();
    let executable = fake_ssh(
        temporary.path(),
        "import socket, subprocess, sys, time\nchild = subprocess.Popen(['/bin/sleep', '60'])\npublish_marker('child-pid', str(child.pid))\np = sys.argv[sys.argv.index('-L') + 1].split(':')[0]\ns = socket.socket(socket.AF_UNIX)\ns.bind(p)\ns.listen(1)\ntime.sleep(60)",
    );
    let endpoint = directory.endpoint("t").unwrap();
    let command = SshTunnelCommand::new(
        executable,
        "/dev/null".into(),
        SshDestination::new("host").unwrap(),
        endpoint,
        RemoteSocketPath::new("/run/spaceterm/s").unwrap(),
        AuthenticationMode::BatchMode,
    )
    .unwrap();
    let tunnel = start_tunnel(
        command,
        environment(temporary.path()),
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let child: libc::pid_t = fs::read_to_string(temporary.path().join("child-pid"))
        .unwrap()
        .parse()
        .unwrap();
    tunnel.cancel().await.unwrap();
    fixture_watchdog(async {
        loop {
            // A descendant is reaped by its host parent, not by SpaceTerm. A zombie
            // has terminated and cannot retain sockets or continue executing.
            if unsafe { libc::kill(child, 0) } != 0 {
                break;
            }
            #[cfg(target_os = "linux")]
            if fs::read_to_string(format!("/proc/{child}/stat"))
                .ok()
                .and_then(|status| {
                    status
                        .rsplit_once(')')
                        .map(|(_, fields)| fields.trim_start().starts_with('Z'))
                })
                .unwrap_or(false)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
