use super::*;
use crate::cancellation::CancellationToken;

#[test]
fn native_pipes_deliver_eof_to_captured_setup_commands() {
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
        &CancellationToken::default(),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap();
    assert!(output.exit.is_success());
    assert_eq!(output.stdout, b"bounded transfer");
}

#[test]
fn setup_command_cancellation_kills_owned_descendants_and_unblocks_captured_pipes() {
    use std::os::unix::fs::DirBuilderExt;

    let root = crate::platform::unix_adapter_tests::short_temporary_root()
        .join(format!("st-setup-cancel-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let witness = root.join("processes");
    let command = SshCommandSpec::for_test(
        "/bin/sh".into(),
        vec![
            "-c".into(),
            "/bin/sleep 5 & printf '%s %s\\n' $$ $! > \"$1\"; wait".into(),
            "setup-cancellation-fixture".into(),
            witness.clone().into_os_string(),
        ],
    );
    let cancellation = CancellationToken::default();
    let worker_cancellation = cancellation.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let environment =
            SshProcessEnvironment::new_without_authentication(std::env::temp_dir(), None).unwrap();
        let result = run_captured_process(
            &crate::platform::unix_ssh_process::UnixSshProcessAdapter,
            &command,
            &environment,
            Vec::new(),
            64,
            &worker_cancellation,
            Instant::now() + Duration::from_secs(3),
        );
        sender.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let processes = loop {
        if let Ok(line) = std::fs::read_to_string(&witness)
            && line.ends_with('\n')
        {
            break line
                .split_whitespace()
                .map(|pid| pid.parse::<libc::pid_t>().unwrap())
                .collect::<Vec<_>>();
        }
        assert!(Instant::now() < deadline, "owned descendant did not start");
        std::thread::sleep(Duration::from_millis(5));
    };
    let [command, descendant] = processes.as_slice() else {
        panic!("fixture must publish its command and descendant");
    };
    // SAFETY: getpgid only observes the live command and descendant created by this fixture.
    let command_group = unsafe { libc::getpgid(*command) };
    let descendant_group = unsafe { libc::getpgid(*descendant) };
    let caller_group = unsafe { libc::getpgrp() };
    cancellation.cancel();
    let result = receiver.recv_timeout(Duration::from_secs(2));
    worker.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(matches!(
        result.unwrap(),
        Err(CapturedProcessError::Cancelled)
    ));
    assert!(command_group > 0 && command_group != caller_group);
    assert_eq!(descendant_group, command_group);
}
