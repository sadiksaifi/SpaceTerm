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

#[test]
#[ignore = "owns a foreground Development server; run mise run cli:io:test"]
fn ctrl_c_before_dispatch_exits_130_and_after_dispatch_resolves_first() {
    let client_name = format!("cli-interrupt-{}", std::process::id());
    let client =
        ServerConnection::for_development_client(&client_name, spaceterm_server::protocol::MAJOR)
            .unwrap();
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
    let running = fixture.spawn(&["pane", "run", "sleep 8", "--pane", &pane]);
    until(|| fixture.held().1 > 0, "the run was never recorded");
    std::thread::sleep(Duration::from_millis(500));
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
