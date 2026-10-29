//! `pane run` and observation through the protocol, on real headless PTYs. Runs use each installed
//! shell with SpaceTerm shell integration; shells that are not installed are skipped.
use super::dispatch::Served;
use super::tests::{create_shared, shared_inventory, size};
use super::*;
use spaceterm_server::attachments::Attachment;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::service::{Dispatch, PaneService as _, Peer};
use spaceterm_server::protocol::{
    self, Channel, ClaimReply, InputEvent, Operation, PaneEvent, ReplyBody, RequestId, RunDoubt,
    RunId, RunReceipt, RunRefusal, RunReply, RunState, ServerMessage, frame::Assembler,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const AGENT: Peer = Peer {
    client: [1; 32],
    connection: [1; 32],
    pairing: None,
};
const PERSON: Peer = Peer {
    client: [2; 32],
    connection: [2; 32],
    pairing: None,
};
const CHANNEL: Channel = Channel(1);
/// Generous, because a shell's first prompt waits for its startup files.
const DEADLINE: Duration = Duration::from_secs(20);

/// Starts every shell with an empty home, so only SpaceTerm's integration and the system's
/// startup files run.
struct EmptyHome {
    pty: Arc<dyn NativePtyAdapterFactory>,
    home: String,
}

impl NativePtyAdapterFactory for EmptyHome {
    fn create(
        &self,
        launch: crate::platform::shell_launch::PreparedShellLaunch,
        size: crate::platform::native_pty::NativePtySize,
    ) -> Result<
        crate::platform::native_pty::NativePtyAdapterParts,
        crate::platform::native_pty::NativePtyAdapterConstructionFailure,
    > {
        self.pty
            .create(launch.with_environment_value("HOME", &self.home), size)
    }
}

/// A temporary home directory, removed on drop.
struct Home(std::path::PathBuf);

impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spaceterm-run-home-{}-{:x}",
            std::process::id(),
            u64::from_ne_bytes(nonce().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn installed(shell: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let found = std::env::split_paths(&path)
        .map(|directory| directory.join(shell))
        .find(|candidate| candidate.is_file());
    if found.is_none() {
        eprintln!("{shell} is not installed; skipping its run checks");
    }
    found
}

fn integrated_host(shell: &std::path::Path, home: &Home) -> DevelopmentPaneHost {
    let filesystem = local_filesystem();
    let directory = filesystem.validate_directory(&home.0).unwrap();
    let factory = NativeTerminalSessionFactory::new(
        Arc::new(EmptyHome {
            pty: native_pty_factory(),
            home: home.0.to_str().unwrap().into(),
        }),
        ShellLaunchPlanner::new(
            shell.into(),
            crate::platform::launch_host::resource_root(),
            ShellIntegrationMode::Automatic,
            ShellEnvironment::default(),
            ShellIntegrationPolicy {
                supported: true,
                path_list_separator: ':',
                fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
            },
            &[],
        ),
        filesystem,
        LocalMachine::default(),
    );
    DevelopmentPaneHost::new(factory, directory).unwrap()
}

/// One server, one Pane, and the clients that use it.
struct Bench {
    served: Served<SharedWorkspaceHost>,
    workspace: [u8; 32],
    pane: [u8; 32],
    outbox: Outbox,
    assembler: Assembler,
    rows: Vec<String>,
    receipts: Vec<RunReceipt>,
    serial: u32,
    _home: Home,
}

impl Bench {
    fn new(shell: &std::path::Path) -> Self {
        let home = Home::new();
        let mut shared = SharedWorkspaceHost::new(integrated_host(shell, &home));
        let workspace = create_shared(&mut shared, 1);
        let pane = shared_inventory(&mut shared)
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.tabs.first())
            .and_then(|tab| tab.panes.first())
            .unwrap()
            .id;
        let mut served = Served::new(shared);
        served.ticker(Arc::new(|| {}));
        let outbox = Outbox::new(Arc::new(|| {}));
        served.admitted(&AGENT, "Agent".into(), outbox.clone());
        let mut bench = Self {
            served,
            workspace,
            pane,
            outbox,
            assembler: Assembler::default(),
            rows: Vec::new(),
            receipts: Vec::new(),
            serial: 0,
            _home: home,
        };
        let claim = bench.reply(
            &AGENT,
            bench.claim(CHANNEL, AttachmentClaim::Select { explicit: true }),
        );
        assert!(matches!(
            claim,
            Ok(ReplyBody::Claim(ClaimReply::Attached { .. }))
        ));
        bench.outbox.view(CHANNEL, true);
        bench.served.view(
            &AGENT,
            protocol::View {
                channel: CHANNEL,
                visible: true,
                max_hz: 60,
            },
        );
        bench.input(vec![InputEvent::Focus(FocusTarget::Terminal)]);
        bench
    }

    fn claim(&self, channel: Channel, claim: AttachmentClaim) -> Operation {
        Operation::Claim(protocol::Claim {
            channel,
            attachment: Attachment {
                workspace: self.workspace,
                generation: 1,
            },
            pane: self.pane,
            claim,
            // An agent keeps the running size; a fresh Session starts at the default.
            size: None,
            style: None,
        })
    }

    fn reply(&mut self, peer: &Peer, operation: Operation) -> Result<ReplyBody, Refusal> {
        match self.served.request(peer, RequestId(1), operation) {
            Dispatch::Reply(result) => *result,
            _ => panic!("the operation replies at once"),
        }
    }

    fn input(&mut self, events: Vec<InputEvent>) {
        self.serial += 1;
        self.served.input(
            &AGENT,
            protocol::Input {
                channel: CHANNEL,
                serial: self.serial,
                generation: 1,
                events,
            },
        );
    }

    fn session(&self) -> [u8; 32] {
        self.served.host().running_lease(self.pane).unwrap().session
    }

    fn run(&mut self, run: u8, command: &str) -> RunReply {
        let session = self.session();
        match self.reply(
            &AGENT,
            Operation::Run {
                channel: CHANNEL,
                run: RunId([run; 16]),
                command: command.into(),
                session,
            },
        ) {
            Ok(ReplyBody::Run(reply)) => reply,
            other => panic!("unexpected run reply {other:?}"),
        }
    }

    /// Runs `command` once the owning shell's first prompt arrived.
    fn run_at_prompt(&mut self, run: u8, command: &str) -> RunReply {
        self.run_within(run, command, DEADLINE)
    }

    fn run_within(&mut self, run: u8, command: &str, wait: Duration) -> RunReply {
        let deadline = Instant::now() + wait;
        loop {
            match self.run(run, command) {
                RunReply::Refused(RunRefusal::NoIntegration | RunRefusal::NotAtPrompt)
                    if Instant::now() < deadline =>
                {
                    self.pump();
                    std::thread::sleep(Duration::from_millis(20));
                }
                reply => return reply,
            }
        }
    }

    /// Publishes and reads everything the channel received, as the connection's I/O thread and
    /// its client would.
    fn pump(&mut self) {
        if self.outbox.take_publication() {
            self.served.publish(&AGENT);
        }
        self.served.tick(Instant::now());
        while let Some((frame, _)) = self.outbox.next_frame() {
            let frame = protocol::RawFrame::parse(&frame).unwrap();
            match self.assembler.server(frame).unwrap() {
                Some(ServerMessage::Frame(frame)) => {
                    if let Some(geometry) = frame.update.geometry
                        && self.rows.len() != usize::from(geometry.rows)
                    {
                        self.rows = vec![String::new(); usize::from(geometry.rows)];
                    }
                    if let Some(shift) = frame.update.shift {
                        shift.apply(&mut self.rows, String::new).unwrap();
                    }
                    for row in &frame.update.rows {
                        self.rows[usize::from(row.index)] =
                            row.runs.iter().map(|run| run.text.as_str()).collect();
                    }
                    self.outbox.acknowledge(frame.channel, frame.frame).unwrap();
                }
                Some(ServerMessage::Pane(message)) => {
                    if let PaneEvent::RunFinished(receipt) = message.event {
                        self.receipts.push(*receipt);
                    }
                }
                _ => {}
            }
        }
    }

    fn await_receipt(&mut self) -> RunReceipt {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.pump();
            if let Some(receipt) = self.receipts.pop() {
                return receipt;
            }
            assert!(
                Instant::now() < deadline,
                "no receipt; screen: {:?}",
                self.rows
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn await_row(&mut self, text: &str) {
        let deadline = Instant::now() + DEADLINE;
        while !self.rows.iter().any(|row| row.contains(text)) {
            assert!(
                Instant::now() < deadline,
                "expected {text:?}; screen: {:?}",
                self.rows
            );
            self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = self.served.shutdown();
    }
}

/// Whether this Bash enables bracketed paste by default: 5.1 or later.
fn bash_has_bracketed_paste(shell: &std::path::Path) -> bool {
    let output = std::process::Command::new(shell)
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .is_ok_and(|version| version >= 501)
}

fn text(text: &str) -> InputEvent {
    let InteractiveInput::Key(key) = super::tests::text_key(text, false) else {
        unreachable!()
    };
    InputEvent::Key(key)
}

/// Every installed integrated shell runs a quoted command verbatim and binds its exit status,
/// and refuses a second run until it settles, typed input, and a running command.
#[test]
fn integrated_shells_run_one_command_and_bind_its_status() {
    for name in ["zsh", "bash", "fish", "elvish", "nu"] {
        let Some(shell) = installed(name) else {
            continue;
        };
        let mut bench = Bench::new(&shell);
        if name == "bash" && !bash_has_bracketed_paste(&shell) {
            // Readline enables bracketed paste by default only from Bash 5.1, and Bash before
            // 4.4 marks no command start.
            let reply = bench.run_within(1, "true", Duration::from_secs(5));
            assert!(
                matches!(
                    reply,
                    RunReply::Refused(RunRefusal::NoBracketedPaste | RunRefusal::NoIntegration)
                ),
                "an old bash is unsupported, not {reply:?}"
            );
            continue;
        }
        // Single quotes, double spaces and a dollar sign arrive exactly as given.
        let command = match name {
            "nu" => "print 'RUN-[a  $b]'; exit 3",
            "elvish" => "echo 'RUN-[a  $b]'; fail x",
            _ => "printf '%s\\n' 'RUN-[a  $b]'; false",
        };
        let command = command.replace("; exit 3", "; ^sh -c 'exit 3'");
        match bench.run_at_prompt(1, &command) {
            RunReply::Admitted { .. } => {}
            reply => panic!(
                "{name}: unexpected admission {reply:?}; screen: {:?}",
                bench.rows
            ),
        }
        assert_eq!(
            bench.run(2, "true"),
            RunReply::Refused(RunRefusal::RunPending),
            "{name}: a second run waits"
        );
        let receipt = bench.await_receipt();
        bench.await_row("RUN-[a  $b]");
        match receipt.state {
            RunState::Finished {
                exit_status: Some(status),
                ..
            } => assert_ne!(status, 0, "{name}: the failure is bound"),
            state => panic!("{name}: unexpected receipt {state:?}"),
        }
        assert!(receipt.end.is_some());
        assert!(matches!(
            bench.reply(&AGENT, Operation::RunStatus { run: RunId([1; 16]) }),
            Ok(ReplyBody::Run(RunReply::Receipt(status))) if status == receipt
        ));
        assert!(
            bench
                .reply(
                    &AGENT,
                    Operation::RunAcknowledge {
                        run: RunId([1; 16])
                    }
                )
                .is_ok()
        );
        assert!(matches!(
            bench.reply(
                &AGENT,
                Operation::RunStatus {
                    run: RunId([1; 16])
                }
            ),
            Err(Refusal::Missing)
        ));

        // Typed input makes the line nonempty.
        bench.input(vec![text("echo typed")]);
        bench.await_row("echo typed");
        assert_eq!(
            bench.run(3, "true"),
            RunReply::Refused(RunRefusal::InputPending),
            "{name}: typed input is never run over"
        );
    }
}

/// A shell without SpaceTerm's prompt markers is never run in.
#[test]
fn shells_without_prompt_markers_are_unsupported() {
    for name in ["sh", "dash"] {
        let Some(shell) = installed(name) else {
            continue;
        };
        let mut bench = Bench::new(&shell);
        bench.input(vec![text("printf 'READY\\n'\r")]);
        bench.await_row("READY");
        assert_eq!(
            bench.run(1, "true"),
            RunReply::Refused(RunRefusal::NoIntegration),
            "{name}"
        );
    }
}

/// A silent read is a running command, a shell that exits ends its run, and a take over leaves
/// the run following its command.
#[test]
fn zsh_runs_settle_when_the_session_ends_or_control_moves() {
    let Some(shell) = installed("zsh") else {
        return;
    };
    let mut bench = Bench::new(&shell);
    assert!(matches!(
        bench.run_at_prompt(1, "true"),
        RunReply::Admitted { .. }
    ));
    bench.await_receipt();
    bench.input(vec![text("read -s secret\r")]);
    let deadline = Instant::now() + DEADLINE;
    loop {
        match bench.run(2, "true") {
            RunReply::Refused(RunRefusal::NotAtPrompt) => break,
            RunReply::Refused(RunRefusal::InputPending) if Instant::now() < deadline => {
                bench.pump();
                std::thread::sleep(Duration::from_millis(10));
            }
            reply => panic!("a silent read refuses runs, not {reply:?}"),
        }
    }
    bench.input(vec![text("x\r")]);
    assert!(matches!(
        bench.run_at_prompt(3, "sleep 30"),
        RunReply::Admitted { .. }
    ));
    let outbox = Outbox::new(Arc::new(|| {}));
    bench.served.admitted(&PERSON, "Person".into(), outbox);
    let session = bench.session();
    let takeover = bench.claim(
        Channel(1),
        AttachmentClaim::TakeOver {
            target: pane::PaneTarget {
                server: bench
                    .served
                    .host()
                    .running_lease(bench.pane)
                    .unwrap()
                    .server,
                session,
            },
        },
    );
    assert!(matches!(
        bench.reply(&PERSON, takeover),
        Ok(ReplyBody::Claim(ClaimReply::Attached { .. }))
    ));
    assert_eq!(
        bench.served.host().session_size(bench.pane),
        Some(super::dispatch::DEFAULT_SIZE),
        "a claim without a size keeps the running size"
    );
    bench.pump();
    let Ok(ReplyBody::Run(RunReply::Receipt(receipt))) = bench.reply(
        &AGENT,
        Operation::RunStatus {
            run: RunId([3; 16]),
        },
    ) else {
        panic!("the run stays resolvable")
    };
    assert_eq!(receipt.state, RunState::TakenOver);
}

/// The shell's own `exit` ends its Session, and the run settles as ended.
#[test]
fn a_run_that_exits_its_shell_settles_as_session_ended() {
    let Some(shell) = installed("zsh") else {
        return;
    };
    let mut bench = Bench::new(&shell);
    assert!(matches!(
        bench.run_at_prompt(1, "exit 4"),
        RunReply::Admitted { .. }
    ));
    let deadline = Instant::now() + DEADLINE;
    let receipt = loop {
        bench.pump();
        if let Ok(ReplyBody::Run(RunReply::Receipt(receipt))) = bench.reply(
            &AGENT,
            Operation::RunStatus {
                run: RunId([1; 16]),
            },
        ) && receipt.state != RunState::Admitted
            && receipt.state != RunState::Running
        {
            break receipt;
        }
        assert!(Instant::now() < deadline, "the run never settled");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        matches!(
            receipt.state,
            RunState::SessionEnded | RunState::Unconfirmed(RunDoubt::NoExitStatus)
        ),
        "unexpected {receipt:?}"
    );
}

/// One connection, and what it received on each channel.
struct Client {
    peer: Peer,
    outbox: Outbox,
    assembler: Assembler,
    rows: BTreeMap<Channel, Vec<String>>,
    events: Vec<(Channel, PaneEvent)>,
    pages: usize,
}

impl Client {
    fn admit(served: &mut Served<SharedWorkspaceHost>, peer: Peer, name: &str) -> Self {
        let outbox = Outbox::new(Arc::new(|| {}));
        served.admitted(&peer, name.into(), outbox.clone());
        Self {
            peer,
            outbox,
            assembler: Assembler::default(),
            rows: BTreeMap::new(),
            events: Vec::new(),
            pages: 0,
        }
    }

    fn reply(
        &self,
        served: &mut Served<SharedWorkspaceHost>,
        operation: Operation,
    ) -> Result<ReplyBody, Refusal> {
        match served.request(&self.peer, RequestId(1), operation) {
            Dispatch::Reply(result) => *result,
            _ => panic!("the operation replies at once"),
        }
    }

    fn show(&self, served: &mut Served<SharedWorkspaceHost>, channel: Channel, visible: bool) {
        self.outbox.view(channel, visible);
        served.view(
            &self.peer,
            protocol::View {
                channel,
                visible,
                max_hz: 60,
            },
        );
    }

    fn pump(&mut self, served: &mut Served<SharedWorkspaceHost>) {
        if self.outbox.take_publication() {
            served.publish(&self.peer);
        }
        while let Some((frame, _)) = self.outbox.next_frame() {
            let frame = protocol::RawFrame::parse(&frame).unwrap();
            match self.assembler.server(frame).unwrap() {
                Some(ServerMessage::Frame(frame)) => {
                    let rows = self.rows.entry(frame.channel).or_default();
                    if let Some(geometry) = frame.update.geometry
                        && rows.len() != usize::from(geometry.rows)
                    {
                        *rows = vec![String::new(); usize::from(geometry.rows)];
                    }
                    if let Some(shift) = frame.update.shift {
                        shift.apply(rows, String::new).unwrap();
                    }
                    for row in &frame.update.rows {
                        rows[usize::from(row.index)] =
                            row.runs.iter().map(|run| run.text.as_str()).collect();
                    }
                    self.outbox.acknowledge(frame.channel, frame.frame).unwrap();
                }
                Some(ServerMessage::Pane(message)) => {
                    self.events.push((message.channel, message.event));
                }
                Some(ServerMessage::Reply(reply)) => match reply.result {
                    Ok(ReplyBody::Page(_)) => self.pages += 1,
                    other => panic!("unexpected reply {other:?}"),
                },
                _ => {}
            }
        }
    }

    fn shows(&self, channel: Channel, text: &str) -> bool {
        self.rows
            .get(&channel)
            .is_some_and(|rows| rows.iter().any(|row| row.contains(text)))
    }
}

fn pump_until(
    served: &mut Served<SharedWorkspaceHost>,
    clients: &mut [&mut Client],
    done: impl Fn(&[&mut Client]) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    while !done(clients) {
        assert!(
            Instant::now() < deadline,
            "the condition never held; screens: {:?}; events: {:?}",
            clients
                .iter()
                .map(|client| &client.rows)
                .collect::<Vec<_>>(),
            clients
                .iter()
                .map(|client| &client.events)
                .collect::<Vec<_>>()
        );
        served.tick(Instant::now());
        for client in clients.iter_mut() {
            client.pump(served);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The Pane's creator controls it; another client observes it.
const CONTROLLER: Peer = Peer {
    client: [1; 32],
    connection: [3; 32],
    pairing: None,
};
const OBSERVER: Peer = Peer {
    client: [2; 32],
    connection: [4; 32],
    pairing: None,
};

/// An observer sees a Pane another client controls, even while that client hides it, and never
/// changes its size, control or effects. Its channel refuses input and every terminal command,
/// reads Scrollback, and ends with Release or with the Session.
#[test]
fn observing_a_controlled_pane_changes_nothing_about_it() {
    let mut shared = SharedWorkspaceHost::new(super::tests::host());
    let workspace = create_shared(&mut shared, 1);
    let pane = shared_inventory(&mut shared)
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .and_then(|workspace| workspace.tabs.first())
        .and_then(|tab| tab.panes.first())
        .unwrap()
        .id;
    let mut served = Served::new(shared);
    served.ticker(Arc::new(|| {}));
    let mut person = Client::admit(&mut served, CONTROLLER, "Person");
    let mut agent = Client::admit(&mut served, OBSERVER, "Agent");
    let observed_before = agent.reply(
        &mut served,
        Operation::Observe {
            channel: Channel(1),
            pane,
            session: None,
        },
    );
    assert!(matches!(
        observed_before,
        Ok(ReplyBody::Observe(protocol::ObserveReply::NoSession {
            state: protocol::SessionState::NotStarted
        }))
    ));

    let claimed = person.reply(
        &mut served,
        Operation::Claim(protocol::Claim {
            channel: CHANNEL,
            attachment: Attachment {
                workspace,
                generation: 1,
            },
            pane,
            claim: AttachmentClaim::Select { explicit: true },
            size: Some(size()),
            style: None,
        }),
    );
    let Ok(ReplyBody::Claim(ClaimReply::Attached { lease })) = claimed else {
        panic!("the person claims the Pane")
    };
    person.show(&mut served, CHANNEL, true);
    served.input(
        &CONTROLLER,
        protocol::Input {
            channel: CHANNEL,
            serial: 1,
            generation: 1,
            events: vec![InputEvent::Focus(FocusTarget::Terminal)],
        },
    );

    let observed = agent.reply(
        &mut served,
        Operation::Observe {
            channel: Channel(2),
            pane,
            session: Some(lease.session),
        },
    );
    assert!(matches!(
        observed,
        Ok(ReplyBody::Observe(protocol::ObserveReply::Observing { session, .. }))
            if session == lease.session
    ));
    agent.show(&mut served, Channel(2), true);
    // The person hides the Pane; the visible observer still receives its frames.
    person.show(&mut served, CHANNEL, false);
    served.input(
        &CONTROLLER,
        protocol::Input {
            channel: CHANNEL,
            serial: 2,
            generation: 1,
            events: vec![text("printf 'OBS-%s\\a\\n' seen\r")],
        },
    );
    pump_until(&mut served, &mut [&mut person, &mut agent], |clients| {
        clients[1].shows(Channel(2), "OBS-seen")
    });

    // Every terminal command on the observer channel is refused; input is discarded.
    for operation in [
        Operation::ClearScrollback {
            channel: Channel(2),
        },
        Operation::Close {
            channel: Channel(2),
        },
        Operation::EndAccessibility {
            channel: Channel(2),
        },
    ] {
        assert!(matches!(
            agent.reply(&mut served, operation),
            Err(Refusal::Unsupported)
        ));
    }
    served.input(
        &OBSERVER,
        protocol::Input {
            channel: Channel(2),
            serial: 1,
            generation: 1,
            events: vec![InputEvent::Resize(PaneSize {
                columns: 40,
                ..size()
            })],
        },
    );
    agent.pump(&mut served);
    assert!(
        agent
            .events
            .iter()
            .any(|(channel, event)| *channel == Channel(2)
                && matches!(
                    event,
                    PaneEvent::Input {
                        outcome: protocol::InputOutcome::Discarded(Refusal::Unsupported),
                        ..
                    }
                ))
    );
    assert!(
        !agent
            .events
            .iter()
            .any(|(_, event)| matches!(event, PaneEvent::Effect(_))),
        "an observer receives no effects"
    );
    assert_eq!(
        served.host().running_lease(pane),
        Some(lease),
        "control and Session are unchanged"
    );
    assert_eq!(served.host().session_size(pane), Some(size()));

    // Scrollback pages answer on the observer channel.
    agent.outbox.begin(RequestId(2)).unwrap();
    assert!(matches!(
        served.request(
            &OBSERVER,
            RequestId(2),
            Operation::ScrollbackPage {
                channel: Channel(2),
                epoch: 1,
                start: 0,
                rows: 10,
            },
        ),
        Dispatch::Deferred
    ));
    pump_until(&mut served, &mut [&mut agent], |clients| {
        clients[0].pages == 1
    });

    // Release ends only the observation.
    assert!(matches!(
        agent.reply(
            &mut served,
            Operation::Release {
                channel: Channel(2)
            }
        ),
        Ok(ReplyBody::Done)
    ));
    assert_eq!(served.host().running_lease(pane), Some(lease));
    served.shutdown().unwrap();
}
