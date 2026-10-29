//! Real CLI processes share one agent identity while terminal I/O remains in progress.
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

use spaceterm_server::{ServerConnection, protocol::AgentName};
use std::{
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Process(Option<Child>);
impl Process {
    fn wait(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
    fn running(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_none()
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
    _server: lifecycle::Server,
    link: spaceterm_server::connection::link::ServerLink,
    client_name: String,
    agent: AgentName,
}
impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        self.command_as(self.agent.as_str(), args)
    }
    fn command_as(&self, agent: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm"));
        command
            .args(["--as", agent, "--json"])
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
    /// The run process may not have claimed its fresh Pane when the observer connects.
    fn wait_for(&self, pane: &str, text: &str) {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let output = self
                .command(&[
                    "pane",
                    "read",
                    "--pane",
                    pane,
                    "--wait-for",
                    text,
                    "--timeout",
                    "5s",
                ])
                .output()
                .unwrap();
            let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if output.status.success() {
                return;
            }
            assert!(
                Instant::now() < until && envelope["error"]["code"] == "session_not_started",
                "{envelope}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.link.close();
        if let Ok(root) = spaceterm_server::cli_state_root("spaceterm-development") {
            for agent in [
                self.agent.as_str().to_owned(),
                format!("{}-person", self.agent.as_str()),
            ] {
                let _ = std::fs::remove_dir_all(root.join("routes/local/agents").join(agent));
            }
            let _ = std::fs::remove_dir_all(
                root.parent()
                    .unwrap()
                    .join(format!("desktop-{}", self.client_name)),
            );
        }
    }
}

#[test]
#[ignore = "owns a foreground Development server; run mise run cli:io:test"]
fn waiting_reads_and_runs_allow_same_identity_tab_creation() {
    let client_name = format!("cli-concurrency-{}", std::process::id());
    let client =
        ServerConnection::for_development_client(&client_name, spaceterm_server::protocol::MAJOR)
            .unwrap();
    lifecycle::require_idle(&client).unwrap();
    client.setup().unwrap();
    let server = lifecycle::Server::start().unwrap();
    let link = lifecycle::connect(&client).unwrap();
    let fixture = Fixture {
        _server: server,
        link,
        client_name,
        agent: format!("io-concurrency-{}", std::process::id())
            .parse()
            .unwrap(),
    };
    let created = fixture.run(&["workspace", "create", "I/O concurrency"]);
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
    let second_tab = fixture.run(&["tab", "create", "--workspace", &workspace]);
    let second_pane = format!("id:{}", second_tab["data"]["pane"]["id"].as_str().unwrap());
    fixture.run(&[
        "pane",
        "run",
        "echo hello",
        "--pane",
        &second_pane,
        "--timeout",
        "10s",
    ]);
    let mut first_run = Process(Some(
        fixture
            .command(&[
                "pane",
                "run",
                "printf 'PARALLEL-%s\\n' READY; sleep 4",
                "--pane",
                &pane,
                "--timeout",
                "10s",
            ])
            .spawn()
            .unwrap(),
    ));
    fixture.wait_for(&pane, "PARALLEL-READY");
    assert!(first_run.running());
    let same_pane = fixture
        .command(&[
            "pane",
            "run",
            "printf SHOULD_NOT_RUN",
            "--pane",
            &pane,
            "--timeout",
            "1s",
        ])
        .output()
        .unwrap();
    let refused: serde_json::Value = serde_json::from_slice(&same_pane.stdout).unwrap();
    assert_eq!(refused["error"]["code"], "not_at_prompt", "{refused}");
    let parallel = fixture.run(&[
        "pane",
        "run",
        "printf 'OTHER-%s\\n' FINISHED",
        "--pane",
        &second_pane,
        "--timeout",
        "1s",
    ]);
    assert_eq!(parallel["data"]["run"]["state"], "finished");
    assert!(
        first_run.running(),
        "the other Pane finishes while the first still runs"
    );
    let output = first_run.wait();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    // A fresh Tab avoids racing the previous run's finish against its next prompt.
    let taken_tab = fixture.run(&["tab", "create", "--workspace", &workspace]);
    let taken_pane = format!("id:{}", taken_tab["data"]["pane"]["id"].as_str().unwrap());
    // Recovery keeps a taken-over receipt until the command has actually finished.
    let taken_run = Process(Some(
        fixture
            .command(&[
                "pane",
                "run",
                "printf 'TAKEOVER-%s\\n' READY; sleep 3; printf 'TAKEOVER-%s\\n' FINISHED",
                "--pane",
                &taken_pane,
                "--timeout",
                "10s",
            ])
            .spawn()
            .unwrap(),
    ));
    fixture.wait_for(&taken_pane, "TAKEOVER-READY");
    let person = format!("{}-person", fixture.agent.as_str());
    let takeover = fixture
        .command_as(
            &person,
            &[
                "pane",
                "send",
                "--pane",
                &taken_pane,
                "--take-over",
                "--key",
                "Enter",
                "--timeout",
                "2s",
            ],
        )
        .output()
        .unwrap();
    assert!(
        takeover.status.success(),
        "{}",
        String::from_utf8_lossy(&takeover.stdout)
    );
    let lost = taken_run.wait();
    let lost: serde_json::Value = serde_json::from_slice(&lost.stdout).unwrap();
    assert_eq!(lost["error"]["code"], "controlled", "{lost}");
    let run = lost["data"]["run"]["id"].as_str().unwrap();
    let recovering = fixture.run(&["pane", "read", "--pane", &pane]);
    assert!(
        recovering["data"]["recovered_operations"]
            .as_array()
            .is_none_or(|items| items.iter().all(|item| item["operation"] != run))
    );
    let waited = fixture.run(&[
        "pane",
        "read",
        "--pane",
        &taken_pane,
        "--wait-for",
        "TAKEOVER-FINISHED",
        "--timeout",
        "5s",
    ]);
    let idle = fixture.run(&[
        "pane",
        "read",
        "--pane",
        &taken_pane,
        "--wait-idle",
        "100ms",
        "--timeout",
        "5s",
    ]);
    let settled = fixture.run(&["pane", "read", "--pane", &pane]);
    assert!(
        [waited, idle, settled]
            .iter()
            .flat_map(|result| result["data"]["recovered_operations"]
                .as_array()
                .into_iter()
                .flatten())
            .any(|item| item["operation"] == run && item["outcome"]["state"] == "applied")
    );
    for args in [
        vec![
            "pane",
            "read",
            "--pane",
            &pane,
            "--wait-idle",
            "4s",
            "--timeout",
            "10s",
        ],
        vec![
            "pane",
            "run",
            "sleep 4",
            "--pane",
            &pane,
            "--timeout",
            "10s",
        ],
    ] {
        let mut waiting = Process(Some(fixture.command(&args).spawn().unwrap()));
        std::thread::sleep(Duration::from_millis(500));
        assert!(waiting.running(), "terminal I/O must still be in progress");
        let created = fixture.run(&[
            "tab",
            "create",
            "--workspace",
            &workspace,
            "--timeout",
            "1s",
        ]);
        assert!(created["data"]["tab"]["id"].is_string());
        assert!(
            waiting.running(),
            "the Tab is created before terminal I/O ends"
        );
        let output = waiting.wait();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
