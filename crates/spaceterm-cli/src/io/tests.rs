//! `mise run cli:io:test`: the terminal commands against an in-process server.
//!
//! Each test starts a real server host behind loopback connections, with the account's shell
//! and an empty home, and acts through the same functions the commands use. Run them on one
//! thread: the shell and home come from this process's environment.
use super::cursor::Cursor;
use super::read::{self, Mode, Snapshot, Source};
use super::*;
use crate::args::{Cli, Command, PaneCommand};
use crate::contract::{Format, Screen as ScreenData, ScreenSize};
use clap::Parser;
use spaceterm::runtime::{Served, SharedWorkspaceHost};
use spaceterm_server::connection::io::{Waker, serve, spawn_link};
use spaceterm_server::connection::loopback::Loopback;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::server::AdmittedConnection;
use spaceterm_server::connection::service::{PaneService, Peer, SharedPaneService};
use spaceterm_server::connection::sync::SyncWorker;
use spaceterm_server::hierarchy::{CreateControl, Mutation, Outcome};
use spaceterm_server::protocol::screen::History;
use spaceterm_server::protocol::{Operation, ReplyBody};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};

const SERVER: Id = [9; 32];

/// The shell and home every test's Terminal Sessions start with.
fn environment() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join(format!("spaceterm-cli-io-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: `cli:io:test` runs these tests on one thread, before any Session starts.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("ZDOTDIR", &home);
            std::env::set_var("SHELL", shell());
        }
    });
}

fn shell() -> String {
    std::env::var("SPACETERM_TEST_ZSH").unwrap_or_else(|_| "/bin/zsh".into())
}

struct Server {
    service: SharedPaneService,
    sync: SyncWorker,
    stopping: Arc<AtomicBool>,
    clients: u8,
}

impl Server {
    fn start() -> Self {
        environment();
        let (sync, _) = SyncWorker::spawn().unwrap();
        let host = SharedWorkspaceHost::local(sync.clone()).unwrap();
        let service: SharedPaneService = Arc::new(Mutex::new(
            Box::new(Served::new(host)) as Box<dyn PaneService>
        ));
        service.lock().unwrap().ticker(Arc::new(|| {}));
        let stopping = Arc::new(AtomicBool::new(false));
        let ticking = Arc::downgrade(&service);
        let stop = Arc::clone(&stopping);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let Some(service) = ticking.upgrade() else {
                    break;
                };
                service.lock().unwrap().tick(Instant::now());
                drop(service);
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Self {
            service,
            sync,
            stopping,
            clients: 0,
        }
    }

    /// A new connection of `client`.
    fn connect(&mut self, client: u8) -> ServerLink {
        self.clients += 1;
        let (server, peer_end) = Loopback::pair(64).unwrap();
        let (waker, wake) = Waker::new().unwrap();
        let outbox = Outbox::new(wake);
        let peer = Peer {
            client: [client; 32],
            connection: [self.clients; 32],
            pairing: None,
        };
        self.service.lock().unwrap().admitted(
            &peer,
            format!("Client {client}").as_str().into(),
            outbox.clone(),
        );
        let connection =
            AdmittedConnection::new(peer, outbox, self.sync.clone(), None, Instant::now());
        let service = Arc::clone(&self.service);
        let stopping = Arc::clone(&self.stopping);
        std::thread::spawn(move || {
            serve(Box::new(server), connection, &waker, &service, &stopping)
        });
        spawn_link(Box::new(peer_end), Arc::new(|_| {})).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.service.lock().unwrap().shutdown();
        self.stopping.store(true, Ordering::Release);
    }
}

/// A new Workspace and its first Pane.
fn workspace(link: &ServerLink, sequence: u64) -> (Id, Id) {
    let reply = request(
        link,
        Operation::Mutate {
            sequence,
            mutation: Mutation::CreateWorkspace {
                name: format!("io {sequence}"),
                directory: None,
                control: CreateControl::Creator,
            },
        },
        Deadline::after(Some(Duration::from_secs(10))),
    )
    .unwrap();
    let ReplyBody::Outcome(outcome) = reply else {
        panic!("an outcome")
    };
    let Outcome::Accepted { target } = outcome.outcome else {
        panic!("created")
    };
    let (_, revision) = link.next_inventory(0).unwrap();
    let mut seen = 0;
    let mut revision = revision;
    loop {
        if let Some(pane) = revision
            .inventory
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target)
            .and_then(|workspace| workspace.tabs.first())
            .and_then(|tab| tab.panes.first())
        {
            return (target, pane.id);
        }
        let next = link.next_inventory(seen).unwrap();
        seen = next.0;
        revision = next.1;
    }
}

fn target(workspace: Id, pane: Id, running: bool) -> PaneTarget {
    PaneTarget {
        server: SERVER,
        workspace,
        pane,
        running,
        steps: Vec::new(),
    }
}

fn parse(arguments: &[&str]) -> PaneCommand {
    let mut argv = vec!["spaceterm", "pane"];
    argv.extend_from_slice(arguments);
    match Cli::try_parse_from(argv).unwrap().command {
        Command::Pane(command) => command,
        _ => unreachable!(),
    }
}

fn run_args(command: &str) -> crate::args::RunArgs {
    match parse(&["run", command]) {
        PaneCommand::Run(args) => args,
        _ => unreachable!(),
    }
}

fn read_args(arguments: &[&str]) -> crate::args::ReadArgs {
    let mut all = vec!["read"];
    all.extend_from_slice(arguments);
    match parse(&all) {
        PaneCommand::Read(args) => args,
        _ => unreachable!(),
    }
}

fn send_args(arguments: &[&str]) -> crate::args::SendArgs {
    let mut all = vec!["send"];
    all.extend_from_slice(arguments);
    match parse(&all) {
        PaneCommand::Send(args) => args,
        _ => unreachable!(),
    }
}

fn runs() -> crate::state::Runs {
    use std::sync::atomic::AtomicU32;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "st-cli-io-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let state = crate::state::CliState::at(root);
    state
        .runs(
            &crate::state::RouteKey::Local,
            None,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap()
}

/// Runs `command`, retrying while the shell has not drawn its first prompt.
fn run_at_prompt(
    link: &ServerLink,
    target: &PaneTarget,
    command: &str,
    runs: &mut crate::state::Runs,
) -> Done<crate::contract::PaneRun> {
    let started = Instant::now();
    loop {
        let link_for_reconnect = link.clone();
        let mut connect = move || Ok(link_for_reconnect.clone());
        let done = run::run(
            link,
            &mut connect,
            target,
            &run_args(command),
            &mut std::io::empty(),
            runs,
            Deadline::after(Some(Duration::from_secs(30))),
        );
        let waiting = done.failure.as_ref().is_some_and(|failure| {
            matches!(failure.code, Code::NotAtPrompt | Code::UnsupportedSession)
        });
        if !waiting || started.elapsed() > Duration::from_secs(20) {
            return done;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn output_text(done: &Done<crate::contract::PaneRun>) -> String {
    let output = done.data.as_ref().unwrap().output.as_ref().unwrap();
    output
        .rows
        .iter()
        .map(|row| row.text.clone())
        .chain(output.screen.iter().flat_map(|screen| screen.rows.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A command runs verbatim, its status is bound, a second Session's cursor is refused, and
/// scrollback pages with `--before` and `--after` lose no row under row and byte caps.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn run_read_and_page_through_scrollback() {
    let mut server = Server::start();
    let link = server.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let target = target(workspace, pane, false);
    let mut runs = runs();

    let done = run_at_prompt(
        &link,
        &target,
        "printf '%s\\n' 'Q[a  $b]'; false",
        &mut runs,
    );
    assert_eq!(
        done.failure.as_ref().map(|failure| failure.code),
        Some(Code::CommandFailed)
    );
    let data = done.data.as_ref().unwrap();
    assert_eq!(data.run.exit_status, Some(1));
    assert!(data.run.started_shell);
    assert!(
        output_text(&done).contains("Q[a  $b]"),
        "{}",
        output_text(&done)
    );
    assert!(runs.pending().is_empty(), "a settled run is retired");

    let target = PaneTarget {
        running: true,
        ..target
    };
    let done = run_at_prompt(&link, &target, "seq 1 3000", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);

    // Page forward from the oldest row in small pages; every number arrives once, in order.
    let first = read::read(
        &link,
        &target,
        &read_args(&["--tail", "1"]),
        Deadline::after(None),
    );
    let rows = first.data.unwrap().rows.unwrap();
    let mut cursor = Cursor::decode(&rows.next.before).unwrap();
    cursor.row = rows.history.oldest;
    let mut seen = Vec::new();
    loop {
        let done = read::read(
            &link,
            &target,
            &read_args(&[
                "--after",
                &cursor.encode(),
                "--rows",
                "333",
                "--max-bytes",
                "2000",
            ]),
            Deadline::after(None),
        );
        assert!(done.failure.is_none(), "{:?}", done.failure);
        let rows = done.data.unwrap().rows.unwrap();
        seen.extend(
            rows.rows
                .iter()
                .filter_map(|row| row.text.trim().parse::<u32>().ok()),
        );
        let next = Cursor::decode(&rows.next.after).unwrap();
        if !rows.truncated {
            // The last page ends with the screen, which holds the newest rows.
            seen.extend(
                rows.screen
                    .iter()
                    .flat_map(|screen| screen.rows.iter())
                    .filter_map(|row| row.trim().parse::<u32>().ok()),
            );
            break;
        }
        assert!(next.row > cursor.row, "paging advances");
        cursor = next;
    }
    let numbers: Vec<u32> = seen
        .into_iter()
        .skip_while(|number| *number != 1)
        .take(3000)
        .collect();
    assert_eq!(
        numbers,
        (1..=3000).collect::<Vec<_>>(),
        "--after loses nothing"
    );

    // Page backward from the screen; pages meet without gaps.
    let mut before = read::read(&link, &target, &read_args(&[]), Deadline::after(None))
        .data
        .unwrap()
        .rows
        .unwrap()
        .next
        .after;
    let mut backward = Vec::new();
    for _ in 0..40 {
        let done = read::read(
            &link,
            &target,
            &read_args(&["--before", &before, "--rows", "250"]),
            Deadline::after(None),
        );
        let rows = done.data.unwrap().rows.unwrap();
        if rows.rows.is_empty() {
            break;
        }
        let mut page: Vec<u64> = rows.rows.iter().map(|row| row.row).collect();
        page.extend(backward);
        backward = page;
        before = rows.next.before;
    }
    assert!(
        backward.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "--before pages without gaps"
    );

    // A cursor of another Session is refused before anything is observed.
    let mut other = Cursor::decode(&before).unwrap();
    other.session = [0; 16];
    let done = read::read(
        &link,
        &target,
        &read_args(&["--after", &other.encode()]),
        Deadline::after(None),
    );
    assert_eq!(
        done.failure.map(|failure| failure.code),
        Some(Code::CursorMismatch)
    );

    // The alternate screen hides history from cursors.
    let done = run_at_prompt(&link, &target, "printf '\\033[?1049h'", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);
    // The observer sees the alternate screen once its frame is published.
    let started = Instant::now();
    let code = loop {
        let done = read::read(
            &link,
            &target,
            &read_args(&["--before", &before]),
            Deadline::after(None),
        );
        let code = done.failure.map(|failure| failure.code);
        if code.is_some() || started.elapsed() > Duration::from_secs(5) {
            break code;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(Code::HistoryHidden));
}

/// Typing into a running Session is delivered, even input a password prompt hides; reading
/// waits for text and for silence; release gives control up.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn send_wait_and_release() {
    let mut server = Server::start();
    let link = server.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let mut runs = runs();
    let target = target(workspace, pane, false);
    let done = run_at_prompt(&link, &target, "true", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let target = PaneTarget {
        running: true,
        ..target
    };
    let typed = send::send(
        &link,
        &target,
        &send_args(&["read -s secret; echo GOT-$secret", "--enter"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert!(typed.failure.is_none(), "{:?}", typed.failure);
    // The silent read echoes nothing, so only progress confirms delivery.
    let sent = send::send(
        &link,
        &target,
        &send_args(&["hidden", "--enter"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert!(sent.failure.is_none(), "{:?}", sent.failure);
    assert!(sent.data.unwrap().input.delivered);

    let read = read::read(
        &link,
        &target,
        &read_args(&["--wait-for", "GOT-hidden"]),
        Deadline::after(Some(Duration::from_secs(20))),
    );
    assert!(read.failure.is_none(), "{:?}", read.failure);
    assert!(read.data.unwrap().wait.unwrap().matched);

    let started = Instant::now();
    let idle = read::read(
        &link,
        &target,
        &read_args(&["--wait-idle", "500ms"]),
        Deadline::after(Some(Duration::from_secs(20))),
    );
    assert!(idle.failure.is_none(), "{:?}", idle.failure);
    assert!(started.elapsed() >= Duration::from_millis(500));

    let released = send::release(&link, &target, Deadline::after(None));
    assert!(released.data.unwrap().released);
}

/// An observer reads a Pane another client controls without changing its size; another client
/// cannot type into it without `--take-over`; the shell's own exit ends a run and later input.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn observe_controlled_panes_and_end_sessions() {
    let mut server = Server::start();
    let person = server.connect(1);
    let (workspace, pane) = workspace(&person, 1);
    let mut runs = runs();
    let sized = run_args("true");
    let mut sized = sized;
    sized.size = Some(crate::args::TerminalSize {
        columns: 100,
        rows: 30,
    });
    let target = target(workspace, pane, false);
    let started = Instant::now();
    loop {
        let link = person.clone();
        let done = run::run(
            &person,
            &mut move || Ok(link.clone()),
            &target,
            &sized,
            &mut std::io::empty(),
            &mut runs,
            Deadline::after(Some(Duration::from_secs(30))),
        );
        if done.failure.is_none() || started.elapsed() > Duration::from_secs(20) {
            assert!(done.failure.is_none(), "{:?}", done.failure);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let target = PaneTarget {
        running: true,
        ..target
    };

    let agent = server.connect(2);
    let observed = read::read(&agent, &target, &read_args(&[]), Deadline::after(None));
    let screen = observed.data.unwrap().rows.unwrap().screen.unwrap();
    assert_eq!(screen.size.columns, 100, "observing keeps the size");
    let typed = send::send(
        &agent,
        &target,
        &send_args(&["x"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert_eq!(
        typed.failure.map(|failure| failure.code),
        Some(Code::Controlled)
    );

    let ended = run_at_prompt(&person, &target, "exit 4", &mut runs);
    assert_eq!(
        ended.failure.map(|failure| failure.code),
        Some(Code::SessionEnded)
    );
    let typed = send::send(
        &person,
        &target,
        &send_args(&["x"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert_eq!(
        typed.failure.map(|failure| failure.code),
        Some(Code::SessionEnded)
    );
}

/// A source whose epoch changes on the first snapshot's pages, then holds.
struct Rewrapping {
    snapshots: u32,
}

impl Source for Rewrapping {
    fn snapshot(&mut self, _: Format) -> Result<Snapshot, Failure> {
        self.snapshots += 1;
        Ok(Snapshot {
            history: History {
                epoch: u64::from(self.snapshots),
                oldest: 0,
                total: 10,
            },
            screen: ScreenData {
                rows: vec![String::new()],
                cursor: crate::contract::CursorPosition { row: 0, column: 0 },
                size: ScreenSize {
                    columns: 80,
                    rows: 1,
                },
                alternate_screen: false,
            },
        })
    }

    fn page(
        &mut self,
        epoch: u64,
        start: u64,
        rows: u16,
        _: Format,
    ) -> Result<Option<read::PageRows>, Failure> {
        if epoch == 1 {
            return Ok(None);
        }
        Ok(Some((
            start,
            (start..(start + u64::from(rows)).min(10))
                .map(|row| (row.to_string(), false))
                .collect(),
        )))
    }
}

#[test]
fn an_epoch_change_while_paging_captures_again_once() {
    let mut source = Rewrapping { snapshots: 0 };
    let captured = read::capture(&mut source, Mode::Tail(5), None, 1 << 20, Format::Text).unwrap();
    assert_eq!(source.snapshots, 2);
    assert_eq!(captured.rows.len(), 5);
    assert_eq!(captured.history.epoch, 2);

    // A source that keeps changing fails with history_reset.
    struct Unstable;
    impl Source for Unstable {
        fn snapshot(&mut self, format: Format) -> Result<Snapshot, Failure> {
            Rewrapping { snapshots: 0 }.snapshot(format)
        }
        fn page(
            &mut self,
            _: u64,
            _: u64,
            _: u16,
            _: Format,
        ) -> Result<Option<read::PageRows>, Failure> {
            Ok(None)
        }
    }
    let failure =
        read::capture(&mut Unstable, Mode::Tail(5), None, 1 << 20, Format::Text).unwrap_err();
    assert_eq!(failure.code, Code::HistoryReset);
}

#[test]
fn run_commands_are_one_line_without_control_characters() {
    let mut stdin = std::io::Cursor::new(b"echo 'from stdin'\nsecond\n".to_vec());
    assert_eq!(
        run::command_line("-", &mut stdin).unwrap(),
        "echo 'from stdin'"
    );
    for command in ["", "a\nb", "a\u{1b}b", "a\u{85}b"] {
        let failure = run::command_line(command, &mut std::io::empty()).unwrap_err();
        assert_eq!(failure.code, Code::InvalidArguments);
    }
    let long = "x".repeat(16 * 1024 + 1);
    assert!(run::command_line(&long, &mut std::io::empty()).is_err());
}

/// Clearing Scrollback keeps the epoch and moves `oldest` up to `total`.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn clearing_scrollback_keeps_the_epoch() {
    let mut server = Server::start();
    let link = server.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let mut runs = runs();
    let target = target(workspace, pane, false);
    let done = run_at_prompt(&link, &target, "seq 1 200", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let target = PaneTarget {
        running: true,
        ..target
    };
    let before = read::read(
        &link,
        &target,
        &read_args(&["--tail", "5"]),
        Deadline::after(None),
    )
    .data
    .unwrap()
    .rows
    .unwrap()
    .history;
    // `clear` with E3 erases Scrollback, as Clear Scrollback does.
    let done = run_at_prompt(&link, &target, "printf '\\033[3J'", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let after = read::read(
        &link,
        &target,
        &read_args(&["--tail", "5"]),
        Deadline::after(None),
    )
    .data
    .unwrap()
    .rows
    .unwrap()
    .history;
    assert_eq!(after.epoch, before.epoch);
    assert_eq!(after.oldest, after.total);
}

/// Typed input refuses a run; a Take Over mid-run ends it with `controlled`; a link lost
/// between admission and finish is settled by `RunStatus`, and the command runs once.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn runs_refuse_typed_input_and_survive_take_over_and_lost_links() {
    let mut server = Server::start();
    let link = server.connect(1);
    let spare = server.connect(1);
    let person = server.connect(2);
    let (workspace, pane) = workspace(&link, 1);
    let mut runs = runs();
    let target = target(workspace, pane, false);
    let done = run_at_prompt(&link, &target, "true", &mut runs);
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let target = PaneTarget {
        running: true,
        ..target
    };

    let typed = send::send(
        &link,
        &target,
        &send_args(&["echo typed"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert!(typed.failure.is_none(), "{:?}", typed.failure);
    let refused = run::run(
        &link,
        &mut || Err(Failure::new(Code::Unreachable)),
        &target,
        &run_args("true"),
        &mut std::io::empty(),
        &mut runs,
        Deadline::after(Some(Duration::from_secs(10))),
    );
    assert_eq!(
        refused.failure.map(|failure| failure.code),
        Some(Code::NotAtPrompt)
    );
    let cleared = send::send(
        &link,
        &target,
        // Clearing the line and pressing Enter draws a fresh, empty prompt.
        &send_args(&["--key", "C-u", "--enter"]),
        &mut std::io::empty(),
        Deadline::after(None),
    );
    assert!(cleared.failure.is_none(), "{:?}", cleared.failure);

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let probe = run_at_prompt(&link, &target, "true", &mut runs);
        if probe.failure.is_none() {
            break;
        }
    }
    // The link closes while the command runs; a second link of the same identity resolves it.
    let closing = link.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        closing.close();
    });
    let resolver = spare.clone();
    let done = run_at_prompt_with(
        &link,
        &mut move || Ok(resolver.clone()),
        &target,
        "sleep 2; echo ONCE-$((40 + 2))",
        &mut runs,
    );
    assert!(done.failure.is_none(), "{:?}", done.failure);
    assert_eq!(done.data.unwrap().run.exit_status, Some(0));
    let screen = read::read(
        &spare,
        &target,
        &read_args(&["--wait-for", "ONCE-42"]),
        Deadline::after(Some(Duration::from_secs(10))),
    );
    let rows = screen.data.unwrap().rows.unwrap();
    let printed = rows
        .screen
        .iter()
        .flat_map(|screen| screen.rows.iter())
        .chain(rows.rows.iter().map(|row| &row.text))
        .filter(|row| row.trim() == "ONCE-42")
        .count();
    assert_eq!(printed, 1, "the command is never sent again");
    assert!(runs.pending().is_empty());

    // Another client takes over while the command runs.
    let taker = person.clone();
    let taken = target.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let _ = send::send(
            &taker,
            &taken,
            &send_args(&["--take-over", "--key", "Enter"]),
            &mut std::io::empty(),
            Deadline::after(None),
        );
    });
    let resolver = spare.clone();
    let done = run_at_prompt_with(
        &spare,
        &mut move || Ok(resolver.clone()),
        &target,
        "sleep 3",
        &mut runs,
    );
    assert_eq!(
        done.failure.map(|failure| failure.code),
        Some(Code::Controlled)
    );
}

fn run_at_prompt_with(
    link: &ServerLink,
    connect: &mut dyn FnMut() -> Result<ServerLink, Failure>,
    target: &PaneTarget,
    command: &str,
    runs: &mut crate::state::Runs,
) -> Done<crate::contract::PaneRun> {
    let started = Instant::now();
    loop {
        let done = run::run(
            link,
            connect,
            target,
            &run_args(command),
            &mut std::io::empty(),
            runs,
            Deadline::after(Some(Duration::from_secs(30))),
        );
        let waiting = done
            .failure
            .as_ref()
            .is_some_and(|failure| failure.code == Code::NotAtPrompt);
        if !waiting || started.elapsed() > Duration::from_secs(20) {
            return done;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
