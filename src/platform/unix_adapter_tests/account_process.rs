use super::*;
#[test]
fn pollable_native_pipes_deliver_eof_to_captured_commands() {
    let adapter = crate::platform::unix_ssh_process::UnixSshProcessAdapter;
    let environment =
        SshProcessEnvironment::new_without_authentication(std::env::temp_dir(), None).unwrap();
    let command = SshCommandSpec::for_test("/bin/cat".into(), Vec::new());
    let output = run_captured_process(
        &adapter,
        &command,
        &environment,
        b"bounded transfer".to_vec(),
        64,
        &SshCancellationToken::default(),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    assert!(output.exit.is_success());
    assert_eq!(output.stdout, b"bounded transfer");
}

#[test]
fn account_bridge_cancellation_kills_owned_processes_and_unblocks_pipes() {
    let adapter = crate::platform::unix_ssh_process::UnixSshProcessAdapter;
    let environment =
        SshProcessEnvironment::new_without_authentication(std::env::temp_dir(), None).unwrap();
    let command = SshCommandSpec::for_test(
        "/bin/sh".into(),
        vec!["-c".into(), "/bin/sleep 30 & wait".into()],
    );
    let cancellation = SshCancellationToken::default();
    let (mut read, write, cleanup) =
        spawn_account_bridge(adapter, &command, &environment, cancellation.clone()).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = read.read_to_end(&mut bytes);
        sender.send(result).unwrap();
    });
    cancellation.cancel();
    assert!(
        receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_ok()
    );
    cleanup();
    drop(write);
    reader.join().unwrap();
}
