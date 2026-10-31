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
use spaceterm::runtime::{Served, WorkspaceHost};
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
        let host = WorkspaceHost::local(sync.clone()).unwrap();
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
        session: None,
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
            None,
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
    assert!(
        runs.pending().unwrap().is_empty(),
        "a settled run is retired"
    );

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
            None,
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

/// Forward pages spend their byte budget on history when the screen is omitted.
#[test]
fn forward_pages_reclaim_the_omitted_screen_budget() {
    struct LargeScreen(Rewrapping);
    impl Source for LargeScreen {
        fn snapshot(&mut self, format: Format) -> Result<Snapshot, Failure> {
            let mut snapshot = self.0.snapshot(format)?;
            snapshot.screen.rows = vec!["screen exceeds the page budget".into()];
            Ok(snapshot)
        }
        fn page(
            &mut self,
            epoch: u64,
            start: u64,
            rows: u16,
            format: Format,
        ) -> Result<Option<read::PageRows>, Failure> {
            self.0.page(epoch, start, rows, format)
        }
    }
    for (limit, budget, expected) in [(2, 3, 1), (2, 6, 2), (10, 6, 3)] {
        let mut source = LargeScreen(Rewrapping { snapshots: 1 });
        let captured = read::capture(
            &mut source,
            Mode::After {
                row: 0,
                rows: limit,
            },
            None,
            budget,
            Format::Text,
        )
        .unwrap();
        assert_eq!(captured.rows.len(), expected);
        assert_eq!(captured.after, expected as u64);
        assert!(captured.truncated);
        assert!(captured.screen.is_none());
    }
}

/// A paste followed by Enter executes the command and confirms Enter's serial.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn paste_then_enter_is_delivered() {
    let mut server = Server::start();
    let link = server.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let target = target(workspace, pane, false);
    let ready = run_at_prompt(&link, &target, "true", &mut runs());
    assert!(ready.failure.is_none(), "{:?}", ready.failure);
    let target = PaneTarget {
        running: true,
        ..target
    };
    let sent = send::send(
        &link,
        &target,
        &send_args(&["printf 'PASTE-%s\\n' EXECUTED", "--paste", "--enter"]),
        &mut std::io::empty(),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert!(sent.failure.is_none(), "{:?}", sent.failure);
    let delivery = sent.data.unwrap().input;
    assert!(delivery.delivered);
    assert_eq!(delivery.serial, 3, "focus, paste, then Enter");
    let observed = read::read(
        &link,
        &target,
        &read_args(&["--wait-for", "PASTE-EXECUTED"]),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert!(observed.failure.is_none(), "{:?}", observed.failure);
}

/// A first call waits for a slow shell startup without retrying the command.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn first_run_waits_for_slow_zsh_startup() {
    slow_startup("ZSH", "/bin/zsh", ".zshrc");
}

#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn first_run_waits_for_slow_bash_startup() {
    slow_startup("BASH", "/opt/homebrew/bin/bash", ".bash_profile");
}

#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn unsupported_shells_and_disabled_integration_fail_without_startup_wait() {
    environment();
    let original = std::env::var_os("SHELL").unwrap();
    let integration = std::env::var_os("SPACETERM_SHELL_INTEGRATION");
    let mut results = Vec::new();
    for (shell, mode) in [("/bin/dash".to_owned(), "on"), (shell(), "off")] {
        if !std::path::Path::new(&shell).is_file() {
            continue;
        }
        // SAFETY: these native tests run serially, before each server captures its launch plan.
        unsafe {
            std::env::set_var("SHELL", shell);
            std::env::set_var("SPACETERM_SHELL_INTEGRATION", mode);
        }
        let mut server = Server::start();
        let link = server.connect(1);
        let (workspace, pane) = workspace(&link, 1);
        let started = Instant::now();
        let done = run::run(
            &link,
            &mut || Ok(link.clone()),
            &target(workspace, pane, false),
            &run_args("printf SHOULD_NOT_RUN"),
            None,
            &mut std::io::empty(),
            &mut runs(),
            Deadline::after(Some(Duration::from_secs(3))),
        );
        results.push((done, started.elapsed()));
    }
    // SAFETY: each server has stopped and these tests run serially.
    unsafe {
        std::env::set_var("SHELL", original);
        match integration {
            Some(value) => std::env::set_var("SPACETERM_SHELL_INTEGRATION", value),
            None => std::env::remove_var("SPACETERM_SHELL_INTEGRATION"),
        }
    }
    for (done, elapsed) in results {
        assert_eq!(done.failure.unwrap().code, Code::UnsupportedSession);
        assert!(
            elapsed < Duration::from_secs(1),
            "unsupported Session waited {elapsed:?}"
        );
    }
}

fn slow_startup(name: &str, fallback: &str, rc: &str) {
    environment();
    let original = std::env::var_os("SHELL").unwrap();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let shell = std::env::var(format!("SPACETERM_TEST_{name}")).unwrap_or_else(|_| {
        if std::path::Path::new(fallback).is_file() {
            fallback.into()
        } else {
            format!("/bin/{}", name.to_ascii_lowercase())
        }
    });
    std::fs::write(home.join(rc), "sleep 2\n").unwrap();
    // SAFETY: these native tests run serially, and this is set before starting a server.
    unsafe {
        std::env::set_var("SHELL", &shell);
    }
    let mut results = Vec::new();
    for prestarted in [false, true] {
        let mut server = Server::start();
        let link = server.connect(1);
        let (workspace, pane) = workspace(&link, 1);
        let mut target = target(workspace, pane, false);
        let started = Instant::now();
        if prestarted {
            let channel = super::channel::Channel::open(&link).unwrap();
            let claimed = channel
                .claim(
                    &target,
                    spaceterm_server::pane::AttachmentClaim::Select { explicit: true },
                    None,
                    Deadline::after(Some(Duration::from_secs(10))),
                )
                .unwrap();
            let super::channel::Claimed::Attached(lease) = claimed else {
                panic!("the Tab starts its shell");
            };
            target.running = true;
            target.session = Some(lease.session);
            channel.keep();
        }
        let done = run::run(
            &link,
            &mut || Ok(link.clone()),
            &target,
            &run_args("printf 'SLOW-%s\\n' STARTED"),
            None,
            &mut std::io::empty(),
            &mut runs(),
            Deadline::after(Some(Duration::from_secs(10))),
        );
        results.push((prestarted, done, started.elapsed()));
    }
    std::fs::remove_file(home.join(rc)).unwrap();
    // SAFETY: the native tests run serially and both servers have stopped.
    unsafe {
        std::env::set_var("SHELL", &original);
    }
    for (prestarted, done, elapsed) in results {
        assert!(
            done.failure.is_none(),
            "{name}, prestarted={prestarted}: {:?}",
            done.failure
        );
        assert_eq!(done.data.as_ref().unwrap().run.started_shell, !prestarted);
        assert!(elapsed >= Duration::from_secs(2));
        assert!(output_text(&done).contains("SLOW-STARTED"));
    }
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
    // The run receipt can precede the frame that publishes E3's new history boundary.
    let until = Instant::now() + Duration::from_secs(5);
    let after = loop {
        let observed = read::read(
            &link,
            &target,
            &read_args(&["--tail", "5"]),
            Deadline::after(Some(Duration::from_secs(5))),
        );
        assert!(observed.failure.is_none(), "{:?}", observed.failure);
        let history = observed.data.unwrap().rows.unwrap().history;
        if history.oldest == history.total || Instant::now() >= until {
            break history;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
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
        None,
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
    assert!(runs.pending().unwrap().is_empty());

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
            None,
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

/// A stale inventory must ask the caller to resolve again, without suggesting a takeover.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn list_maps_a_stale_observation_to_changed() {
    let mut server = Server::start();
    let link = server.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let target = target(workspace, pane, false);
    let done = run_at_prompt(&link, &target, "true", &mut runs());
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let (_, revision) = link.next_inventory(0).unwrap();
    let mut inventory = revision.inventory.clone();
    inventory
        .close
        .as_mut()
        .unwrap()
        .sessions
        .iter_mut()
        .find(|session| session.id == pane)
        .unwrap()
        .session = Some([99; 32]);
    let snapshot = crate::scope::Snapshot {
        server: SERVER,
        repositories: inventory.repositories.clone(),
        inventory,
    };
    let resolved = crate::scope::resolve(
        &snapshot,
        &crate::scope::ScopeArgs::default(),
        None,
        crate::scope::Want::Server,
        crate::scope::Effect::Passive,
    )
    .unwrap();
    let failure = crate::list::collect(
        &snapshot,
        &resolved,
        [1; 32],
        &link,
        Instant::now() + Duration::from_secs(5),
    )
    .err()
    .expect("a stale observation fails");
    assert_eq!(failure.code, Code::Changed);
}

/// Two live CLI calls of one identity cannot enqueue a second command behind the first.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn concurrent_runs_of_one_identity_refuse_the_second_without_input() {
    let mut server = Server::start();
    let first = server.connect(1);
    let second = server.connect(1);
    let (workspace, pane) = workspace(&first, 1);
    let target = target(workspace, pane, false);
    let done = run_at_prompt(&first, &target, "true", &mut runs());
    assert!(done.failure.is_none(), "{:?}", done.failure);
    let target = PaneTarget {
        running: true,
        ..target
    };
    let running = target.clone();
    let call = std::thread::spawn(move || {
        run_at_prompt(
            &first,
            &running,
            "printf 'RUN-%s\\n' BLOCKED; read -s cli_pending; printf 'FIRST-%s\\n' \"$cli_pending\"",
            &mut runs(),
        )
    });
    let ready = read::read(
        &second,
        &target,
        &read_args(&["--wait-for", "RUN-BLOCKED"]),
        Deadline::after(Some(Duration::from_secs(10))),
    );
    assert!(ready.failure.is_none(), "{:?}", ready.failure);
    assert!(!call.is_finished());
    let mut second_runs = runs();
    let refused = run::run(
        &second,
        &mut || Ok(second.clone()),
        &target,
        &run_args("printf 'SECOND-%s\\n' EXECUTED"),
        None,
        &mut std::io::empty(),
        &mut second_runs,
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert_eq!(refused.failure.unwrap().code, Code::NotAtPrompt);
    assert!(
        second_runs.pending().unwrap().is_empty(),
        "a refused call leaves no run journal"
    );
    let sent = send::send(
        &second,
        &target,
        &send_args(&["sentinel", "--enter"]),
        &mut std::io::empty(),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert!(sent.failure.is_none(), "{:?}", sent.failure);
    let done = call.join().unwrap();
    assert!(done.failure.is_none(), "{:?}", done.failure);
    // If the second run sent even part of its command, the silent read would consume it.
    let probe = run_at_prompt(&second, &target, "true", &mut second_runs);
    assert!(probe.failure.is_none(), "{:?}", probe.failure);
    let observed = read::read(
        &second,
        &target,
        &read_args(&["--tail", "100"]),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert!(observed.failure.is_none(), "{:?}", observed.failure);
    let output = super::start::rows_text(observed.data.unwrap().rows.as_ref().unwrap());
    assert!(output.contains("FIRST-sentinel"), "{output}");
    assert!(!output.contains("SECOND-EXECUTED"), "{output}");
}

/// A stale fence cannot take control or start a replacement Terminal Session.
#[test]
#[ignore = "starts a server and shells; run mise run cli:io:test"]
fn stale_session_fences_leave_control_and_ended_sessions_unchanged() {
    let mut server = Server::start();
    let person = server.connect(1);
    let agent = server.connect(2);
    let (workspace, pane) = workspace(&person, 1);
    let target = target(workspace, pane, false);
    let ready = run_at_prompt(&person, &target, "true", &mut runs());
    assert!(ready.failure.is_none(), "{:?}", ready.failure);
    let target = PaneTarget {
        running: true,
        session: Some([7; 32]),
        ..target
    };
    let sent = send::send(
        &agent,
        &target,
        &send_args(&["x", "--take-over", "--session", "eeeeeeee"]),
        &mut std::io::empty(),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert_eq!(sent.failure.unwrap().code, Code::SessionChanged);
    let still_owned = run_at_prompt(&person, &target, "true", &mut runs());
    assert!(still_owned.failure.is_none(), "{:?}", still_owned.failure);
    let ended = run_at_prompt(&person, &target, "exit 4", &mut runs());
    assert_eq!(ended.failure.unwrap().code, Code::SessionEnded);
    let target = PaneTarget {
        running: false,
        session: None,
        ..target
    };
    let mut args = run_args("true");
    args.session = crate::scope::IdPrefix::parse("eeeeeeee");
    let refused = run::run(
        &agent,
        &mut || Ok(agent.clone()),
        &target,
        &args,
        None,
        &mut std::io::empty(),
        &mut runs(),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert_eq!(refused.failure.unwrap().code, Code::SessionChanged);
    let observed = read::read(
        &person,
        &target,
        &read_args(&[]),
        Deadline::after(Some(Duration::from_secs(5))),
    );
    assert_eq!(observed.failure.unwrap().code, Code::SessionEnded);
}

#[test]
#[ignore = "starts real PTYs; run on one thread with cli:io:test"]
fn end_all_sessions_preview_and_confirmation_end_only_the_reviewed_sessions() {
    use crate::contract::{Mode as OutputMode, ServerRef};
    use crate::server::end_all_sessions;
    use crate::work::dispatch::Dispatch;
    use spaceterm_server::hierarchy::Inventory;

    struct Journal {
        link: ServerLink,
        sequence: u64,
    }
    impl Dispatch for Journal {
        fn server_id(&self) -> Id {
            SERVER
        }
        fn client_id(&self) -> Id {
            [1; 32]
        }
        fn resolve_leftovers(
            &mut self,
        ) -> Result<Vec<crate::contract::RecoveredOperation>, Failure> {
            Ok(Vec::new())
        }
        fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure> {
            let reply = request(
                &self.link,
                Operation::Mutate {
                    sequence: self.sequence,
                    mutation,
                },
                Deadline::after(Some(Duration::from_secs(10))),
            )?;
            self.sequence += 1;
            let ReplyBody::Outcome(result) = reply else {
                panic!("mutation outcome");
            };
            Ok(result.outcome)
        }
    }
    fn inventory(link: &ServerLink) -> Inventory {
        link.inventory().unwrap().1.inventory.clone()
    }
    let mut host = Server::start();
    let link = host.connect(1);
    let (workspace, pane) = workspace(&link, 1);
    let channel = super::channel::Channel::open(&link).unwrap();
    let claimed = channel
        .claim(
            &target(workspace, pane, false),
            spaceterm_server::pane::AttachmentClaim::Select { explicit: true },
            None,
            Deadline::after(Some(Duration::from_secs(10))),
        )
        .unwrap();
    let super::channel::Claimed::Attached(_) = claimed else {
        panic!("started Session");
    };
    let until = Instant::now() + Duration::from_secs(10);
    let initial = loop {
        let current = inventory(&link);
        if current.close.as_ref().is_some_and(|c| {
            c.sessions
                .iter()
                .any(|p| p.id == pane && p.session.is_some())
        }) {
            break current;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    };
    let server = ServerRef {
        name: "local".into(),
        route: "local".into(),
        id: hex(&SERVER),
    };
    let mut journal = Journal {
        link: link.clone(),
        sequence: 2,
    };
    let now = std::time::SystemTime::now();
    let preview =
        end_all_sessions::execute(&initial, &mut journal, server.clone(), None, now).unwrap();
    let mut stdout = Vec::new();
    assert_eq!(
        preview.write(
            OutputMode {
                json: true,
                ..OutputMode::default()
            },
            &mut stdout,
            &mut Vec::new()
        ),
        3
    );
    let preview: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    let token = preview["data"]["confirmation"]["token"].as_str().unwrap();
    let refused =
        end_all_sessions::execute(&initial, &mut journal, server.clone(), Some("wrong"), now);
    assert!(matches!(refused, Err(failure) if failure.code == Code::Changed));
    assert_eq!(
        inventory(&link).close.unwrap().sessions,
        initial.close.as_ref().unwrap().sessions
    );
    let done = end_all_sessions::execute(&initial, &mut journal, server, Some(token), now).unwrap();
    assert_eq!(done.exit(), 0);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let ended = inventory(&link);
        let close = ended.close.unwrap();
        assert_eq!(close.boot, initial.close.as_ref().unwrap().boot);
        if close.sessions.iter().all(|p| p.session.is_none()) {
            assert_eq!(
                ended.workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
                initial.workspaces.iter().map(|w| w.id).collect::<Vec<_>>()
            );
            break;
        }
        assert!(Instant::now() < until, "confirmed Sessions did not end");
        std::thread::sleep(Duration::from_millis(5));
    }
    link.close();
}
