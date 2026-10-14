//! Exercises the production fixed-command and framing adapter with a local SSH process boundary.

use super::*;

fn configuration() -> Configuration {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("ssh-boundary");
    // No remote command is executed. This executable models only multiplex ownership
    // and the raw-archive/framed-channel byte boundary.
    std::fs::write(
        &executable,
        br##"#!/bin/sh
set -efu
master=no
check=no
socket=
previous=
for argument in "$@"; do
    if [ "$previous" = -S ]; then socket=$argument; fi
    case "$argument" in -M) master=yes ;; check) check=yes ;; esac
    previous=$argument
done
if [ "$master" = yes ]; then
    : > "$socket"
    exec /bin/sleep 60
fi
if [ "$check" = yes ]; then exit 0; fi
case "$*" in
    *bootstrap-account*)
        /bin/dd bs=1 count=2 of=/dev/null 2>/dev/null
        printf '\000\000\000\002{}'
        exec /bin/cat
        ;;
    *) exit 1 ;;
esac
"##,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    Configuration {
        executable,
        home: directory.path().to_owned(),
        environment: Vec::new(),
        file,
        directory,
        askpass: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_retains_master_and_frames_only_fixed_account_operations() {
    let master = Arc::new(Master::start(configuration()).unwrap());
    let command = master.command();
    let arguments = command
        .get_args()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert!(arguments.contains(&"ControlMaster=no"));
    assert!(arguments.contains(&"BatchMode=yes"));
    assert!(arguments.contains(&"ProxyCommand=/usr/bin/false"));
    assert!(arguments.contains(&"ClearAllForwardings=yes"));
    assert!(arguments.contains(&"PermitLocalCommand=no"));
    assert!(arguments.contains(&"RemoteCommand=none"));
    let staged = Arc::new(StagedArtifact {
        bytes: b"AB".to_vec(),
        entry: "./bin/spaceterm-server",
        digest: [1; 32],
    });
    let channel = master.stage(staged, 1000).await.unwrap();
    assert_eq!(channel.recv().await.unwrap().unwrap(), b"{}");
    let request = serde_json::to_vec(&AccountRequest::Inspect).unwrap();
    channel.send(&request).await.unwrap();
    assert_eq!(channel.recv().await.unwrap().unwrap(), request);
    assert_eq!(
        channel.send(&vec![0; FRAME_LIMIT + 1]).await,
        Err(TransportError::MessageTooLarge)
    );
    channel.close().await.unwrap();
    assert!(master.revalidate().is_ok());
    // Reaping the retained owner must prevent a subsequent command from borrowing
    // a replacement OpenSSH connection, even if its local socket name remains.
    master.child.lock().unwrap().retire();
    let until = Instant::now() + Duration::from_secs(2);
    while master.revalidate().is_ok() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(matches!(
        master.revalidate(),
        Err(BootstrapFailure {
            class: BootstrapFailureClass::Pairing(PairingFailure::RouteChanged),
            ..
        })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ssh_rejects_oversized_remote_frame_before_allocation() {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf '\\377\\377\\377\\377'"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let channel = Channel::spawn(&mut command, 1000).unwrap();
    assert_eq!(channel.recv().await, Err(TransportError::Unavailable));
}
