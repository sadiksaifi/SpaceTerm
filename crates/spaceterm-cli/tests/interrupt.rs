//! Ctrl-C reaches real CLI processes before and after they dispatch an effect.
#[expect(
    dead_code,
    reason = "the test owns only a foreground Development server"
)]
#[path = "../examples/support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "this fixture uses only connection and Workspace cleanup helpers"
)]
#[path = "../examples/support/mod.rs"]
mod support;
#[path = "../examples/support/workspace.rs"]
mod workspace;

use spaceterm_cli::state::{CliState, RouteKey};
use spaceterm_server::{ServerConnection, protocol::AgentName};
use std::{
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const IDENTITY: &str = "spaceterm-development";

struct Process(Option<Child>);
impl Process {
    fn interrupt(&self) {
        let pid = self.0.as_ref().unwrap().id() as libc::pid_t;
        // SAFETY: signals only the child this test started.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
    }
    /// Waits for the process to exit, and fails the test if it outlives `limit`.
    fn wait(mut self, limit: Duration) -> (Output, Duration) {
        let started = Instant::now();
        let child = self.0.as_mut().unwrap();
        while child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < limit, "the command outlived its Ctrl-C");
            std::thread::sleep(Duration::from_millis(20));
        }
        let elapsed = started.elapsed();
        (self.0.take().unwrap().wait_with_output().unwrap(), elapsed)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Fixture {
    server: lifecycle::Server,
    link: spaceterm_server::connection::link::ServerLink,
    client_name: String,
    agent: AgentName,
}
impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm"));
        command
            .args(["--as", self.agent.as_str(), "--json"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in [
            "SPACETERM_SERVER",
            "SPACETERM_AS",
            "SPACETERM_IDENTITY",
            "SPACETERM_SERVER_ID",
            "SPACETERM_PANE",
            "SPACETERM_SESSION",
        ] {
            command.env_remove(name);
        }
        command
    }
    fn spawn(&self, args: &[&str]) -> Process {
        Process(Some(self.command(args).spawn().unwrap()))
    }
    fn run(&self, args: &[&str]) -> serde_json::Value {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn state(&self) -> CliState {
        CliState::open(IDENTITY).unwrap()
    }
    /// What this agent holds unsettled: journal edits and runs.
    fn held(&self) -> (u32, u32) {
        self.state()
            .held(&RouteKey::Local)
            .unwrap()
            .iter()
            .find(|held| {
                matches!(&held.identity,
                    spaceterm_cli::contract::RecoveredIdentity::Agent(agent) if *agent == self.agent)
            })
            .map_or((0, 0), |held| (held.operations, held.runs))
    }
    fn assert_no_run_locks(&self) {
        let root = spaceterm_server::cli_state_root(IDENTITY).unwrap();
        let locks = root
            .join("routes/local/agents")
            .join(self.agent.as_str())
            .join("runs");
        assert!(
            std::fs::read_dir(locks).unwrap().next().is_none(),
            "settled runs leave no lock files"
        );
    }
    /// Stops the server's process until the guard drops, so it answers nothing meanwhile.
    fn freeze(&self) -> Frozen {
        let pid = self.server.0.id() as libc::pid_t;
        // SAFETY: signals only the server this test started.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        Frozen(pid)
    }
}

struct Frozen(libc::pid_t);
impl Drop for Frozen {
    fn drop(&mut self) {
        // SAFETY: signals only the server this test started.
        unsafe { libc::kill(self.0, libc::SIGCONT) };
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.link.close();
        if let Ok(root) = spaceterm_server::cli_state_root(IDENTITY) {
            let _ =
                std::fs::remove_dir_all(root.join("routes/local/agents").join(self.agent.as_str()));
            let _ = std::fs::remove_dir_all(
                root.parent()
                    .unwrap()
                    .join(format!("desktop-{}", self.client_name)),
            );
        }
    }
}

fn envelope(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stdout)))
}

fn until(mut ready: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// Run in a separate process because the handler exits without unwinding blocked output.
#[test]
#[ignore = "child process for stalled_output_is_interruptible"]
fn blocked_output_child() {
    use spaceterm_cli::{contract, interrupt};
    use std::io::Write;
    let Ok(kind) = std::env::var("SPACETERM_TEST_BLOCKED_OUTPUT") else {
        return;
    };
    let mode = contract::Mode {
        json: true,
        ..Default::default()
    };
    interrupt::install("pane.read", mode).unwrap();
    let data = serde_json::json!({"rows": ["x".repeat(8 * 1024 * 1024)]});
    let envelope = if kind == "confirmed" {
        contract::Envelope::ok("pane.read", None, data)
    } else {
        contract::Envelope::error(
            "pane.read",
            None,
            contract::Failure::new(contract::Code::RunUnconfirmed),
            data,
        )
    };
    let finishing = kind != "emergency";
    let interrupted = if finishing {
        interrupt::finish(envelope.exit())
    } else {
        false
    };
    std::io::stderr().write_all(b"ready\n").unwrap();
    if finishing {
        let exit = contract::render(
            &envelope,
            "",
            "",
            mode,
            interrupted,
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
        );
        panic!("output unexpectedly completed with exit {exit}");
    } else {
        std::io::stdout()
            .write_all(&vec![b'x'; 1024 * 1024])
            .unwrap();
    }
}

#[test]
#[ignore = "process interrupt checks; run mise run cli:io:test"]
fn stalled_output_is_interruptible() {
    use std::io::BufRead;
    use std::os::fd::{AsRawFd, FromRawFd};
    for (kind, expected) in [
        ("emergency", 130),
        ("final", 7),
        ("second", 7),
        ("confirmed", 130),
    ] {
        let mut descriptors = [-1; 2];
        // SAFETY: pipe fills two writable descriptor slots, owned below after success.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: pipe returned two fresh descriptors, each transferred exactly once.
        let (_reader, writer) = unsafe {
            (
                std::fs::File::from_raw_fd(descriptors[0]),
                std::fs::File::from_raw_fd(descriptors[1]),
            )
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "blocked_output_child", "--ignored"])
            .env("SPACETERM_TEST_BLOCKED_OUTPUT", kind)
            .stdout(writer.try_clone().unwrap())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stderr.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        let process = Process(Some(child));
        // Leave the reader open without draining it until the process exits.
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: writer retains the inherited descriptor after the child's stdio initialization.
        let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        process.interrupt();
        if kind == "second" || kind == "confirmed" {
            std::thread::sleep(Duration::from_millis(100));
            process.interrupt();
        }
        let limit = if kind == "final" {
            Duration::from_secs(7)
        } else {
            Duration::from_secs(2)
        };
        let (output, _) = process.wait(limit);
        assert_eq!(output.status.code(), Some(expected), "{kind}");
        // SAFETY: the parent retains the same open file description the child inherited.
        assert_eq!(
            unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) },
            flags,
            "{kind}: inherited output flags changed"
        );
    }
}

// The child plays a command that resolves until exactly its settlement instant, as a lane
// flow, an offline activation or a local Stop does, and only then renders its result.
#[test]
#[ignore = "child process for a_result_rendered_at_the_settlement_instant_is_written_whole"]
fn late_result_child() {
    use spaceterm_cli::{contract, interrupt};
    use std::io::Write;
    if std::env::var_os("SPACETERM_TEST_LATE_RESULT").is_none() {
        return;
    }
    let mode = contract::Mode {
        json: true,
        ..Default::default()
    };
    interrupt::install("server.restart", mode).unwrap();
    interrupt::dispatch().unwrap();
    std::io::stderr().write_all(b"ready\n").unwrap();
    while !interrupt::interrupted() {
        std::thread::sleep(Duration::from_millis(1));
    }
    // The signal records its own arrival, so the settlement instant is known at once.
    let settle_by = interrupt::settle_by().expect("an interrupt reports its settlement instant");
    assert!(settle_by <= Instant::now() + interrupt::GRACE);
    std::thread::sleep(settle_by.saturating_duration_since(Instant::now()));
    let envelope = contract::Envelope::ok(
        "server.restart",
        None,
        serde_json::json!({ "operation": "0f".repeat(16) }),
    );
    let interrupted = interrupt::finish(envelope.exit());
    // A reader that drains slowly, past the interrupt thread's next check.
    std::thread::sleep(interrupt::TICK * 3);
    std::process::exit(contract::render(
        &envelope,
        "",
        "",
        mode,
        interrupted,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ) as i32);
}

#[test]
#[ignore = "process interrupt checks; run mise run cli:io:test"]
fn a_result_rendered_at_the_settlement_instant_is_written_whole() {
    use std::io::BufRead;
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "late_result_child", "--ignored", "--nocapture"])
        .env("SPACETERM_TEST_LATE_RESULT", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = Process(Some(child));
    let mut stderr = std::io::BufReader::new(process.0.as_mut().unwrap().stderr.take().unwrap());
    let mut ready = String::new();
    stderr.read_line(&mut ready).unwrap();
    assert_eq!(ready, "ready\n");
    process.interrupt();
    // The pipe is drained while the child runs, so only the output bound could cut it short.
    let (output, elapsed) = process.wait(Duration::from_secs(8));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(130), "{stdout}");
    assert!(elapsed >= Duration::from_millis(4900), "{elapsed:?}");
    let envelope: serde_json::Value = stdout
        .lines()
        .find(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap_or_else(|| panic!("no envelope in {stdout}"));
    assert_eq!(envelope["error"]["code"], "interrupted");
    assert_eq!(envelope["data"]["operation"], "0f".repeat(16));
}

// Each child installs the real signal handler and runs the send handler. The transport holds
// the paste outcome until SIGINT arrives, then delivers it inside the settlement grace.
#[test]
#[ignore = "child process for the paste interrupt checks"]
fn paste_wait_child() {
    use clap::Parser;
    use spaceterm_cli::{args, contract, interrupt, io};
    use spaceterm_server::connection::link::ServerLink;
    use spaceterm_server::pane::{Lease, PasteConfirmation, PasteOutcome};
    use spaceterm_server::protocol::{
        ClaimReply, ClientMessage, InputEvent, InputOutcome, Operation, PaneEvent, PaneMessage,
        RawFrame, Reply, ReplyBody, ServerMessage, frame,
    };
    use std::io::Write;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    let Ok(kind) = std::env::var("SPACETERM_TEST_PASTE_WAIT") else {
        return;
    };
    let confirmation = kind == "confirmation";
    let mode = contract::Mode {
        json: true,
        ..Default::default()
    };
    interrupt::install("pane.send", mode).unwrap();
    let (wake, ready) = mpsc::channel();
    let notify = wake.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let link = ServerLink::new(
        Arc::new(|_| {}),
        Arc::new(move || {
            let _ = wake.send(());
        }),
        Instant::now(),
    );
    let peer = link.clone();
    let stopped = Arc::clone(&finished);
    let server = std::thread::spawn(move || {
        let send = |message| {
            for bytes in frame::encode_server(&message).unwrap().frames {
                peer.receive(RawFrame::parse(&bytes).unwrap(), Instant::now())
                    .unwrap();
            }
        };
        let mut assembler = frame::Assembler::default();
        let mut keys = 0;
        let mut resolutions = Vec::new();
        loop {
            ready.recv_timeout(Duration::from_secs(10)).unwrap();
            while let Some((bytes, _)) = peer.next_frame() {
                let Some(message) = assembler.client(RawFrame::parse(&bytes).unwrap()).unwrap()
                else {
                    continue;
                };
                match message {
                    ClientMessage::Request(request) => {
                        assert!(matches!(request.operation, Operation::Claim(_)));
                        send(ServerMessage::Reply(Box::new(Reply {
                            id: request.id,
                            result: Ok(ReplyBody::Claim(ClaimReply::Attached {
                                lease: Lease {
                                    server: [1; 32],
                                    session: [2; 32],
                                    control: [3; 32],
                                    connection: [4; 32],
                                },
                            })),
                        })));
                    }
                    ClientMessage::Input(input) => {
                        let event = |event| {
                            send(ServerMessage::Pane(PaneMessage {
                                channel: input.channel,
                                event,
                            }))
                        };
                        for input_event in input.events {
                            let outcome = match input_event {
                                InputEvent::Paste { last: true, .. } => {
                                    std::io::stderr().write_all(b"ready\n").unwrap();
                                    until(
                                        interrupt::interrupted,
                                        "SIGINT never reached the paste wait",
                                    );
                                    Some(if confirmation {
                                        PasteOutcome::ConfirmationRequired(PasteConfirmation {
                                            id: 7,
                                            byte_len: 8,
                                            line_count: 2,
                                            multiline: true,
                                            control_bytes: false,
                                            closing_fence: false,
                                        })
                                    } else {
                                        PasteOutcome::Written
                                    })
                                }
                                InputEvent::ResolvePaste { id, confirm } => {
                                    assert_eq!(id, 7);
                                    resolutions.push(confirm);
                                    Some(if confirm {
                                        PasteOutcome::Written
                                    } else {
                                        PasteOutcome::Cancelled
                                    })
                                }
                                InputEvent::Key(_) => {
                                    keys += 1;
                                    None
                                }
                                InputEvent::Focus(_) => None,
                                _ => panic!("unexpected input"),
                            };
                            if let Some(outcome) = outcome {
                                event(PaneEvent::Input {
                                    serial: input.serial,
                                    outcome: InputOutcome::Paste(outcome),
                                });
                            }
                        }
                        event(PaneEvent::Progress {
                            input: input.serial,
                            output_bytes: 0,
                        });
                    }
                    ClientMessage::View(_) => {}
                    _ => panic!("unexpected client message"),
                }
            }
            if stopped.load(Ordering::Acquire) {
                return (keys, resolutions);
            }
        }
    });
    let args::Command::Pane(args::PaneCommand::Send(args)) = args::Cli::parse_from([
        "spaceterm",
        "pane",
        "send",
        "echo one\necho two",
        "--paste",
        "--enter",
        "--allow-unsafe-paste",
    ])
    .command
    else {
        unreachable!();
    };
    let target = io::PaneTarget {
        server: [1; 32],
        workspace: [5; 32],
        pane: [6; 32],
        running: true,
        session: Some([2; 32]),
        steps: Vec::new(),
    };
    let done = io::send::send(
        &link,
        &target,
        &args,
        &mut std::io::empty(),
        io::Deadline::after(Some(Duration::from_secs(10))),
    );
    finished.store(true, Ordering::Release);
    // Wake the transport so it drains every message queued before send returned.
    notify.send(()).unwrap();
    let (keys, resolutions) = server.join().unwrap();
    link.close();
    assert_eq!(keys, 0, "SIGINT must prevent Enter");
    assert_eq!(
        resolutions,
        if confirmation {
            vec![false]
        } else {
            Vec::new()
        },
        "an interrupted pending paste must be cancelled"
    );
    let data = done.data.unwrap();
    assert_eq!(data.input.serial, if confirmation { 3 } else { 2 });
    assert_eq!(data.input.delivered, !confirmation);
    let envelope = match done.failure {
        Some(failure) => contract::Envelope::error("pane.send", None, failure, data),
        None => contract::Envelope::ok("pane.send", None, data),
    };
    let interrupted = interrupt::finish(envelope.exit());
    std::process::exit(contract::render(
        &envelope,
        "",
        "",
        mode,
        interrupted,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    ) as i32);
}

fn interrupt_paste_wait(kind: &str) {
    use std::io::{BufRead, Read};
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "paste_wait_child", "--ignored", "--nocapture"])
        .env("SPACETERM_TEST_PASTE_WAIT", kind)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut process = Process(Some(child));
    let mut ready = String::new();
    let mut stderr = std::io::BufReader::new(process.0.as_mut().unwrap().stderr.take().unwrap());
    stderr.read_line(&mut ready).unwrap();
    assert_eq!(ready, "ready\n");
    process.interrupt();
    let (output, _) = process.wait(Duration::from_secs(7));
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}\n{errors}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "process interrupt checks; run mise run cli:io:test"]
fn ctrl_c_during_paste_delivery_never_sends_enter() {
    interrupt_paste_wait("written");
}

#[test]
#[ignore = "process interrupt checks; run mise run cli:io:test"]
fn ctrl_c_during_paste_confirmation_cancels_without_enter() {
    interrupt_paste_wait("confirmation");
}

#[test]
#[ignore = "owns a foreground Development server; run mise run cli:io:test"]
fn ctrl_c_during_first_prompt_wait_never_sends_the_command() {
    let home = std::env::temp_dir().join(format!(
        "spaceterm-interrupt-startup-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let home = Home(home);
    let release = home.0.join("release");
    std::fs::write(
        home.0.join(".zshrc"),
        format!(
            "printf 'STARTUP-%s\\n' WAIT\nwhile [[ ! -f '{}' ]]; do sleep 0.02; done\n",
            release.display()
        ),
    )
    .unwrap();
    let client_name = format!("cli-startup-interrupt-{}", std::process::id());
    let client = ServerConnection::for_development_client(&client_name).unwrap();
    lifecycle::require_idle(&client).unwrap();
    client.setup().unwrap();
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_spaceterm"));
    let server = lifecycle::Server(
        Command::new(executable)
            .args(["server", "run", "--identity", IDENTITY])
            .env(
                "SHELL",
                std::env::var("SPACETERM_TEST_ZSH").unwrap_or_else(|_| "/bin/zsh".into()),
            )
            .env("ZDOTDIR", &home.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let link = lifecycle::connect(&client).unwrap();
    let fixture = Fixture {
        server,
        link,
        client_name,
        agent: format!("startup-interrupt-{}", std::process::id())
            .parse()
            .unwrap(),
    };
    let created = fixture.run(&["workspace", "create", "Interrupted startup"]);
    let workspace = created["data"]["workspace"]["id"].as_str().unwrap();
    let id = std::array::from_fn(|i| u8::from_str_radix(&workspace[i * 2..i * 2 + 2], 16).unwrap());
    let cleanup = workspace::Workspace::track(&client, &fixture.link, id).unwrap();
    let pane = format!(
        "id:{}",
        cleanup
            .pane
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let waiting = fixture.spawn(&["pane", "run", "printf SHOULD_NOT_RUN", "--pane", &pane]);
    until(
        || fixture.held().1 > 0,
        "the run never reached its prompt wait",
    );
    fixture.run(&[
        "pane",
        "read",
        "--pane",
        &pane,
        "--wait-for",
        "STARTUP-WAIT",
        "--timeout",
        "5s",
    ]);
    waiting.interrupt();
    // Let startup finish inside the old retry window. An interrupted CLI must not admit input.
    std::fs::write(&release, "").unwrap();
    let (output, _) = waiting.wait(Duration::from_secs(7));
    assert_eq!(output.status.code(), Some(130), "{}", envelope(&output));
    assert_eq!(fixture.held(), (0, 0), "the undispatched run was forgotten");
    fixture.assert_no_run_locks();
    let observed = fixture.run(&[
        "pane",
        "read",
        "--pane",
        &pane,
        "--wait-idle",
        "300ms",
        "--timeout",
        "5s",
    ]);
    assert!(
        !observed["data"].to_string().contains("SHOULD_NOT_RUN"),
        "no command input was written"
    );
}

#[test]
#[ignore = "owns a foreground Development server; run mise run cli:io:test"]
fn ctrl_c_before_dispatch_exits_130_and_after_dispatch_resolves_first() {
    let client_name = format!("cli-interrupt-{}", std::process::id());
    let client = ServerConnection::for_development_client(&client_name).unwrap();
    lifecycle::require_idle(&client).unwrap();
    client.setup().unwrap();
    let server = lifecycle::Server::start().unwrap();
    let link = lifecycle::connect(&client).unwrap();
    let fixture = Fixture {
        server,
        link,
        client_name,
        agent: format!("interrupt-{}", std::process::id()).parse().unwrap(),
    };
    let created = fixture.run(&["workspace", "create", "Ctrl-C"]);
    let workspace = created["data"]["workspace"]["id"].as_str().unwrap();
    let id = std::array::from_fn(|i| u8::from_str_radix(&workspace[i * 2..i * 2 + 2], 16).unwrap());
    let cleanup = workspace::Workspace::track(&client, &fixture.link, id).unwrap();
    let workspace = format!("id:{workspace}");
    let pane = format!(
        "id:{}",
        cleanup
            .pane
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let state = fixture.state();

    // A work command waiting for its identity's writer lock has dispatched nothing.
    let writer = state
        .writer(
            &RouteKey::Local,
            Some(&fixture.agent),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let waiting = fixture.spawn(&["workspace", "rename", "Before", "--workspace", &workspace]);
    std::thread::sleep(Duration::from_millis(1500));
    waiting.interrupt();
    let (output, elapsed) = waiting.wait(Duration::from_secs(2));
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(envelope(&output)["error"]["code"], "interrupted");
    assert!(elapsed < Duration::from_secs(1));
    drop(writer);
    assert_eq!(fixture.held(), (0, 0), "nothing was dispatched");

    // A work command whose edit is on its way resolves it for at most 5 s, then reports that
    // the outcome could not be confirmed.
    let writer = state
        .writer(
            &RouteKey::Local,
            Some(&fixture.agent),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let waiting = fixture.spawn(&["workspace", "rename", "After", "--workspace", &workspace]);
    std::thread::sleep(Duration::from_millis(1500));
    let frozen = fixture.freeze();
    drop(writer);
    until(|| fixture.held().0 > 0, "the edit was never journaled");
    waiting.interrupt();
    let (output, elapsed) = waiting.wait(Duration::from_secs(10));
    drop(frozen);
    assert_eq!(output.status.code(), Some(7), "{}", envelope(&output));
    assert_eq!(envelope(&output)["error"]["code"], "outcome_unconfirmed");
    assert!(elapsed < Duration::from_secs(7));
    // The next command settles the edit it left behind.
    let listed = fixture.run(&["list"]);
    assert_eq!(
        listed["data"]["recovered_operations"][0]["outcome"]["state"], "applied",
        "{listed}"
    );

    // Wait until the shell accepts runs.
    let ready = Instant::now() + Duration::from_secs(10);
    loop {
        let output = fixture
            .command(&["pane", "run", "true", "--pane", &pane, "--timeout", "5s"])
            .output()
            .unwrap();
        if output.status.success() {
            break;
        }
        let refused = envelope(&output);
        assert!(
            Instant::now() < ready
                && matches!(
                    refused["error"]["code"].as_str(),
                    Some("not_at_prompt" | "unsupported_session")
                ),
            "{refused}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // `pane run -` still reading its command from stdin has dispatched nothing.
    fixture.assert_no_run_locks();
    let mut reading = fixture.command(&["pane", "run", "-", "--pane", &pane]);
    reading.stdin(Stdio::piped());
    let mut reading = Process(Some(reading.spawn().unwrap()));
    let _stdin = reading.0.as_mut().unwrap().stdin.take();
    std::thread::sleep(Duration::from_millis(1500));
    reading.interrupt();
    let (output, _) = reading.wait(Duration::from_secs(2));
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(envelope(&output)["error"]["code"], "interrupted");
    assert_eq!(fixture.held(), (0, 0), "no run was admitted");

    // A dispatched run still running reports its confirmed state with exit 130.
    let running = fixture.spawn(&[
        "pane",
        "run",
        "printf 'INTERRUPT-%s\\n' RUNNING; sleep 8",
        "--pane",
        &pane,
    ]);
    fixture.run(&[
        "pane",
        "read",
        "--pane",
        &pane,
        "--wait-for",
        "INTERRUPT-RUNNING",
        "--timeout",
        "5s",
    ]);
    running.interrupt();
    let (output, elapsed) = running.wait(Duration::from_secs(8));
    let interrupted = envelope(&output);
    assert_eq!(output.status.code(), Some(130), "{interrupted}");
    assert_eq!(interrupted["error"]["code"], "interrupted");
    assert_eq!(interrupted["data"]["run"]["state"], "running");
    assert!(elapsed < Duration::from_secs(6));

    // A lane operation still settling earlier operations has dispatched nothing.
    let pending = state
        .identity_dir(&RouteKey::Local, Some(&fixture.agent))
        .unwrap()
        .lock("service-operations.lock", false)
        .unwrap();
    let stopping = fixture.spawn(&["server", "stop", "--confirm", "c1-00"]);
    std::thread::sleep(Duration::from_millis(1500));
    stopping.interrupt();
    let (output, _) = stopping.wait(Duration::from_secs(2));
    assert_eq!(output.status.code(), Some(130), "{}", envelope(&output));
    assert_eq!(envelope(&output)["error"]["code"], "interrupted");
    drop(pending);
    let pending: Option<Vec<String>> = state
        .identity_dir(&RouteKey::Local, Some(&fixture.agent))
        .unwrap()
        .read_json("service-operations.json")
        .unwrap();
    assert!(
        pending.unwrap_or_default().is_empty(),
        "no Execute was sent"
    );
}

/// A fake desktop on this account's Development Desktop Channel endpoint. It holds the endpoint
/// lock, so a running SpaceTerm Development desktop must be quit first.
struct FakeDesktop {
    listener: std::os::unix::net::UnixListener,
    socket: std::path::PathBuf,
    _lock: spaceterm_server::Lock,
}
impl FakeDesktop {
    fn listen() -> Self {
        let directory = spaceterm::desktop_channel::endpoint_directory(IDENTITY).unwrap();
        let retained = spaceterm_server::Directory::open(&directory, true).unwrap();
        let lock = retained
            .try_lock("lock")
            .unwrap()
            .expect("quit SpaceTerm Development first: its desktop holds the Desktop Channel");
        retained.remove(c"d").unwrap();
        let socket = directory.join("d");
        Self {
            listener: std::os::unix::net::UnixListener::bind(&socket).unwrap(),
            socket,
            _lock: lock,
        }
    }
}
impl Drop for FakeDesktop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// `open` inside a Session, with a Session context the fake desktop never checks.
fn open_in_session() -> Process {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm"));
    command
        .args([
            "--json",
            "open",
            "/private/tmp/spaceterm-interrupt-fixture.txt",
        ])
        .env_remove("SPACETERM_SERVER")
        .env("SPACETERM_IDENTITY", IDENTITY)
        .env("SPACETERM_SERVER_ID", "01".repeat(32))
        .env("SPACETERM_PANE", "02".repeat(32))
        .env("SPACETERM_SESSION", "03".repeat(32))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Process(Some(command.spawn().unwrap()))
}

#[test]
#[ignore = "owns the Development Desktop Channel endpoint; run mise run cli:io:test"]
fn an_interrupt_after_a_desktop_request_settles_its_answer_within_the_grace() {
    use std::io::{Read, Write};
    let desktop = FakeDesktop::listen();
    for answer_after in [Some(Duration::from_millis(500)), None] {
        let process = open_in_session();
        let (mut stream, _) = desktop.listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request).contains("\"open\""));
        // The request reached the desktop, so Ctrl-C now must wait for its answer.
        process.interrupt();
        if let Some(after) = answer_after {
            std::thread::sleep(after);
            stream
                .write_all(
                    br#"{"schema":"spaceterm.desktop.v1","identity":"spaceterm-development","body":"done"}"#,
                )
                .unwrap();
            drop(stream);
            let (output, _) = process.wait(Duration::from_secs(3));
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            // The confirmed outcome stays, reported as interrupted.
            assert_eq!(result["error"]["code"], "interrupted", "{result}");
            assert_eq!(
                result["data"]["path"],
                "/private/tmp/spaceterm-interrupt-fixture.txt"
            );
            assert_eq!(output.status.code(), Some(130));
        } else {
            let (output, elapsed) = process.wait(Duration::from_secs(8));
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["error"]["code"], "outcome_unconfirmed", "{result}");
            assert_eq!(output.status.code(), Some(7));
            assert!(
                elapsed >= Duration::from_secs(4),
                "it gave up before the grace ended"
            );
            drop(stream);
        }
    }
}
