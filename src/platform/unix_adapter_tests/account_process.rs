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
        vec![
            "-c".into(),
            "/bin/sleep 30 & printf '%s %s\\n' $$ $!; wait".into(),
        ],
    );
    let cancellation = SshCancellationToken::default();
    let (read, write, cleanup) =
        spawn_account_bridge(adapter, &command, &environment, cancellation.clone()).unwrap();
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut read = std::io::BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).unwrap();
        let processes: Vec<libc::pid_t> = line
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        ready_sender.send(processes).unwrap();
        let mut bytes = Vec::new();
        let result = read.read_to_end(&mut bytes);
        sender.send(result).unwrap();
    });
    // Prove that the descendant exists in the owned group before cancelling. Immediate
    // cancellation commonly kills the shell before its background process has been forked.
    let processes = ready_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let [command, descendant] = processes.as_slice() else {
        panic!("fixture must publish its command and descendant");
    };
    // SAFETY: getpgid only observes the live command and descendant created by this fixture.
    let command_group = unsafe { libc::getpgid(*command) };
    let descendant_group = unsafe { libc::getpgid(*descendant) };
    let caller_group = unsafe { libc::getpgrp() };
    cancellation.cancel();
    let result = receiver.recv_timeout(Duration::from_secs(2));
    cleanup();
    drop(write);
    assert!(result.unwrap().is_ok());
    reader.join().unwrap();
    assert!(command_group > 0 && command_group != caller_group);
    assert_eq!(descendant_group, command_group);
}
