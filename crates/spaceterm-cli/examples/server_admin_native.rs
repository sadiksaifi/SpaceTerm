//! Native acceptance of server administration (spec section 12F) against an installed server:
//! status, Stop, Restart, Update by upload and across protocol majors, return, the service
//! operation crash windows, stale and expired tokens, a Session started after a preview, a full
//! ledger, local-only commands and pairing.
//!
//! The runner drives a `spaceterm` CLI with `--json`, the way a person or an agent does, and
//! checks each envelope and exit status. Fixture control runs on the server's own machine
//! through the `fixture` role of this executable: it injects ledger records, ends the server or
//! kills the helper inside a crash window, starts a Terminal Session and runs the pairing checks
//! on a pty. A command prefix such as `ssh penguin` reaches a remote machine.
//!
//! ```text
//! server_admin_native [--identity ID] [--server NAME] [--cli PATH]
//!     [--machine PREFIX] [--machine-cli PATH] [--machine-fixture PATH]
//!     [--network all|IP|IP:PORT|off] [--candidate ARCHIVE SIGNATURE]
//!     [--major5 ARCHIVE SIGNATURE] [--broken ARCHIVE SIGNATURE] [--restore ARCHIVE SIGNATURE]
//!     [--expiry] [--case NAME]...
//! server_admin_native fixture ID ACTION [ARGUMENT]...
//! ```
//!
//! Each option also reads `SPACETERM_ADMIN_<OPTION>` with dashes as underscores, for example
//! `SPACETERM_ADMIN_SERVER=penguin` and `SPACETERM_ADMIN_MACHINE="ssh penguin"`. Without
//! `--server`, the target is this machine's own server. Update cases run only when their
//! archives are given. `--expiry` waits out a token's ten-minute lifetime.
//!
//! A SIGKILL'd server leaves an unresolved same-boot run that refuses every start until the
//! machine restarts, so the "server ended after Admitted" window ends the server with SIGTERM.
//! Recovery treats both the same way: the record's boot is not the new server's.
#[allow(
    dead_code,
    reason = "this check only connects, attaches one Pane and runs one command"
)]
mod support;

use serde_json::Value;
use spaceterm_server::service::operations::{
    Action, Actor, Ledger, MAX_RECORDS, OperationId, Record, State,
};
use spaceterm_server::{CliRoute, ServerConnection};
use std::error::Error;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The pairing and client of every record the fixture writes, so clearing forgets only them.
const FIXTURE_PAIRING: [u8; 32] = [0xf0; 32];
/// How long the server may take to return after a restart, update or local start.
const RETURN: Duration = Duration::from_secs(180);

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("fixture") => fixture(&arguments[1..]),
        _ => Options::parse(&arguments).and_then(|options| run(&options)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("server_admin_native: {error}");
            ExitCode::FAILURE
        }
    }
}

// Options

struct Options {
    identity: String,
    server: String,
    cli: String,
    machine: Vec<String>,
    machine_cli: String,
    machine_fixture: String,
    network: Option<String>,
    candidate: Option<(String, String)>,
    major5: Option<(String, String)>,
    broken: Option<(String, String)>,
    restore: Option<(String, String)>,
    expiry: bool,
    cases: Vec<String>,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self> {
        let env = |name: &str| std::env::var(format!("SPACETERM_ADMIN_{name}")).ok();
        let pair = |name: &str| {
            env(name).and_then(|value| {
                let (archive, signature) = value.split_once(' ')?;
                Some((archive.to_owned(), signature.to_owned()))
            })
        };
        let identity = env("IDENTITY").unwrap_or_else(|| "spaceterm-preflight".into());
        let launcher = |identity: &str| {
            std::env::var("HOME")
                .map(|home| format!("{home}/.local/bin/{identity}"))
                .unwrap_or_else(|_| identity.to_owned())
        };
        let mut options = Self {
            server: env("SERVER").unwrap_or_else(|| "local".into()),
            cli: env("CLI").unwrap_or_else(|| launcher(&identity)),
            machine: env("MACHINE")
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            machine_cli: env("MACHINE_CLI").unwrap_or_default(),
            machine_fixture: env("MACHINE_FIXTURE").unwrap_or_default(),
            network: env("NETWORK"),
            candidate: pair("CANDIDATE"),
            major5: pair("MAJOR5"),
            broken: pair("BROKEN"),
            restore: pair("RESTORE"),
            expiry: env("EXPIRY").is_some(),
            cases: env("CASES")
                .map(|v| v.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            identity,
        };
        let mut rest = arguments.iter();
        while let Some(flag) = rest.next() {
            let mut value = || -> Result<String> {
                rest.next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value").into())
            };
            match flag.as_str() {
                "--identity" => options.identity = value()?,
                "--server" => options.server = value()?,
                "--cli" => options.cli = value()?,
                "--machine" => {
                    options.machine = value()?.split_whitespace().map(str::to_owned).collect()
                }
                "--machine-cli" => options.machine_cli = value()?,
                "--machine-fixture" => options.machine_fixture = value()?,
                "--network" => options.network = Some(value()?),
                "--candidate" => options.candidate = Some((value()?, value()?)),
                "--major5" => options.major5 = Some((value()?, value()?)),
                "--broken" => options.broken = Some((value()?, value()?)),
                "--restore" => options.restore = Some((value()?, value()?)),
                "--expiry" => options.expiry = true,
                "--case" => options.cases.push(value()?),
                other => return Err(format!("unknown option {other}").into()),
            }
        }
        // The server's machine is this one unless a prefix reaches another.
        if options.machine_cli.is_empty() {
            options.machine_cli = if options.machine.is_empty() {
                options.cli.clone()
            } else {
                launcher(&options.identity)
            };
        }
        if options.machine_fixture.is_empty() {
            if !options.machine.is_empty() {
                return Err("--machine needs --machine-fixture, this example built there".into());
            }
            options.machine_fixture = std::env::current_exe()?.display().to_string();
        }
        Ok(options)
    }

    fn wants(&self, case: &str) -> bool {
        self.cases.is_empty() || self.cases.iter().any(|v| v == case)
    }
}

// Running commands

/// One CLI command's exit status and envelope.
struct Ran {
    exit: i32,
    envelope: Value,
}

impl Ran {
    fn code(&self) -> &str {
        self.envelope["error"]["code"].as_str().unwrap_or("")
    }
    fn expect(self, exit: i32, what: &str) -> Result<Self> {
        if self.exit != exit {
            return Err(format!(
                "{what}: expected exit {exit}, got {} with {}",
                self.exit,
                self.code()
            )
            .into());
        }
        Ok(self)
    }
    fn expect_code(self, exit: i32, code: &str, what: &str) -> Result<Self> {
        let ran = self.expect(exit, what)?;
        if ran.code() != code {
            return Err(format!("{what}: expected {code}, got {}", ran.code()).into());
        }
        Ok(ran)
    }
    fn token(&self) -> Result<String> {
        self.envelope["data"]["confirmation"]["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "the preview carries no token".into())
    }
}

fn output(command: &mut Command) -> Result<(i32, Vec<u8>)> {
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    Ok((output.status.code().unwrap_or(-1), output.stdout))
}

impl Options {
    /// Runs the client CLI against the target server.
    fn cli(&self, arguments: &[&str]) -> Result<Ran> {
        let mut command = Command::new(&self.cli);
        command
            .args(["--server", &self.server])
            .args(arguments)
            .arg("--json");
        let (exit, stdout) = output(&mut command)?;
        Ok(Ran {
            exit,
            envelope: serde_json::from_slice(&stdout)
                .map_err(|_| format!("{arguments:?} printed no envelope"))?,
        })
    }

    /// A command on the server's own machine.
    fn on_machine(&self, program: &str, arguments: &[&str]) -> Command {
        match self.machine.split_first() {
            None => {
                let mut command = Command::new(program);
                command.args(arguments);
                command
            }
            Some((first, rest)) => {
                let mut command = Command::new(first);
                command.args(rest).arg(program).args(arguments);
                command
            }
        }
    }

    /// The server machine's own CLI, as the person at that machine runs it.
    fn machine_cli(&self, arguments: &[&str]) -> Result<Ran> {
        let mut arguments = arguments.to_vec();
        arguments.push("--json");
        let (exit, stdout) = output(&mut self.on_machine(&self.machine_cli, &arguments))?;
        Ok(Ran {
            exit,
            envelope: serde_json::from_slice(&stdout)
                .map_err(|_| format!("the machine's {arguments:?} printed no envelope"))?,
        })
    }

    /// One fixture action on the server's machine. It prints one line of result.
    fn fixture(&self, action: &str, arguments: &[&str]) -> Result<String> {
        let mut all = vec!["fixture", self.identity.as_str(), action];
        all.extend_from_slice(arguments);
        let (exit, stdout) = output(&mut self.on_machine(&self.machine_fixture, &all))?;
        if exit != 0 {
            return Err(format!("fixture {action} failed").into());
        }
        Ok(String::from_utf8(stdout)?.trim().to_owned())
    }

    /// Waits until the target answers `server status` with a running server.
    fn await_running(&self) -> Result<()> {
        let deadline = Instant::now() + RETURN;
        loop {
            let ran = self.cli(&["server", "status"])?;
            if ran.exit == 0 && ran.envelope["data"]["running"] == true {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("the server did not return".into());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Starts the server on its machine if it is not running, and waits for it.
    fn ensure_running(&self) -> Result<()> {
        if self
            .cli(&["server", "status"])
            .is_ok_and(|ran| ran.exit == 0 && ran.envelope["data"]["running"] == true)
        {
            return Ok(());
        }
        self.machine_cli(&["server", "start"])?
            .expect(0, "local start")?;
        self.await_running()
    }
}

// Cases

fn run(options: &Options) -> Result<()> {
    type Case = fn(&Options) -> Result<String>;
    let mut cases: Vec<(&str, Case)> = vec![
        ("status", status),
        ("local-only", local_only),
        ("stale-token", stale_token),
        ("session-after-preview", session_after_preview),
        ("capacity", capacity),
        ("restart", restart),
        ("token-after-restart", token_after_restart),
        ("admitted-crash", admitted_crash),
        ("helper-killed", helper_killed),
        ("stop", stop),
        ("pairing", pairing),
        ("setup", setup),
    ];
    if options.expiry {
        cases.push(("expired-token", expired_token));
    }
    if options.candidate.is_some() {
        cases.push(("update-upload", update_upload));
    }
    if options.major5.is_some() {
        cases.push(("update-major5", update_major5));
    }
    if options.broken.is_some() {
        cases.push(("return", return_through_update));
    }
    let mut failed = 0;
    for (name, case) in cases.into_iter().filter(|(name, _)| options.wants(name)) {
        // Local-only refusals happen before any connection, so they need no running server.
        if name != "local-only" {
            options.ensure_running()?;
        }
        match case(options) {
            Ok(note) => println!("ok {name}: {note}"),
            Err(error) => {
                failed += 1;
                println!("FAILED {name}: {error}");
            }
        }
    }
    if let Some((archive, signature)) = &options.restore {
        options.ensure_running()?;
        update_with(options, archive, signature, "restore")?;
        println!("restored the original runtime");
    }
    if failed > 0 {
        return Err(format!("{failed} cases failed").into());
    }
    Ok(())
}

fn status(options: &Options) -> Result<String> {
    let ran = options.cli(&["server", "status"])?.expect(0, "status")?;
    let data = &ran.envelope["data"];
    if data["lane_major"] != 1 || data["running"] != true {
        return Err("status names no running server on lane major 1".into());
    }
    Ok(format!(
        "running {} with {} Sessions",
        data["runtime"]["version"], data["sessions"]
    ))
}

fn local_only(options: &Options) -> Result<String> {
    if options.server == "local" {
        return Ok("skipped: the target is this machine".into());
    }
    let started = Instant::now();
    for arguments in [
        &["server", "start"][..],
        &["server", "pair"],
        &["server", "setup", "--network", "off"],
        &["server", "unpair", "id:abcdef"],
    ] {
        options
            .cli(arguments)?
            .expect_code(5, "local_only", arguments[1])?;
    }
    // A refusal before any connection is immediate.
    if started.elapsed() > Duration::from_secs(5) {
        return Err("local-only refusals waited on the network".into());
    }
    Ok("start, pair, setup and unpair exit 5 without connecting".into())
}

fn stale_token(options: &Options) -> Result<String> {
    options
        .cli(&[
            "server",
            "stop",
            "--confirm",
            &format!("c1-{}", "0".repeat(64)),
        ])?
        .expect_code(4, "changed", "a forged token")?;
    let token = options
        .cli(&["server", "restart"])?
        .expect(3, "preview")?
        .token()?;
    options
        .cli(&["server", "stop", "--confirm", &token])?
        .expect_code(4, "changed", "a Restart token used for Stop")?;
    Ok("forged and foreign tokens are changed".into())
}

fn session_after_preview(options: &Options) -> Result<String> {
    let token = options
        .cli(&["server", "restart"])?
        .expect(3, "preview")?
        .token()?;
    options.fixture("start-session", &[])?;
    options
        .cli(&["server", "restart", "--confirm", &token])?
        .expect_code(4, "changed", "a Session started after the preview")?;
    Ok("a new Session changes the observation".into())
}

fn capacity(options: &Options) -> Result<String> {
    options.fixture("fill-ledger", &[])?;
    let result = (|| {
        let token = options
            .cli(&["server", "restart"])?
            .expect(3, "preview")?
            .token()?;
        options
            .cli(&["server", "restart", "--confirm", &token])?
            .expect_code(5, "capacity", "a full ledger")
    })();
    options.fixture("clear-ledger", &[])?;
    result.map(|_| format!("{MAX_RECORDS} unacknowledged records refuse Execute"))
}

fn restart(options: &Options) -> Result<String> {
    let preview = options.cli(&["server", "restart"])?.expect(3, "preview")?;
    let token = preview.token()?;
    let ran = options
        .cli(&["server", "restart", "--confirm", &token])?
        .expect(0, "restart")?;
    let data = &ran.envelope["data"];
    if data["receipt"]["state"] != "done" || data["observed"] != "restarted" {
        return Err(format!("restart settled as {}", data["receipt"]["state"]).into());
    }
    Ok("done on a new boot".into())
}

fn token_after_restart(options: &Options) -> Result<String> {
    let token = options
        .cli(&["server", "stop"])?
        .expect(3, "preview")?
        .token()?;
    let restart = options
        .cli(&["server", "restart"])?
        .expect(3, "preview")?
        .token()?;
    options
        .cli(&["server", "restart", "--confirm", &restart])?
        .expect(0, "restart")?;
    options
        .cli(&["server", "stop", "--confirm", &token])?
        .expect_code(4, "changed", "a token from before a restart")?;
    Ok("a restart forgets every token".into())
}

fn expired_token(options: &Options) -> Result<String> {
    let token = options
        .cli(&["server", "stop"])?
        .expect(3, "preview")?
        .token()?;
    std::thread::sleep(Duration::from_secs(10 * 60 + 5));
    options
        .cli(&["server", "stop", "--confirm", &token])?
        .expect_code(4, "changed", "an expired token")?;
    Ok("a token expires after ten minutes".into())
}

fn admitted_crash(options: &Options) -> Result<String> {
    let operation = OperationId::random()?.to_string();
    options.fixture("inject-admitted", &[&operation])?;
    options.fixture("end-server", &[])?;
    options
        .machine_cli(&["server", "start"])?
        .expect(0, "local start")?;
    options.await_running()?;
    let state = options.fixture("ledger-state", &[&operation])?;
    options.fixture("clear-ledger", &[])?;
    if state != "not_performed" {
        return Err(format!("the admitted record settled as {state}").into());
    }
    Ok("a server that ended after Admitted leaves not_performed".into())
}

fn helper_killed(options: &Options) -> Result<String> {
    let token = options
        .cli(&["server", "restart"])?
        .expect(3, "preview")?
        .token()?;
    let mut command = Command::new(&options.cli);
    command
        .args([
            "--server",
            &options.server,
            "server",
            "restart",
            "--confirm",
            &token,
            "--json",
        ])
        .args(["--timeout", "90s"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let child = command.spawn()?;
    let killed = options.fixture("kill-helper", &["30"])?;
    let finished = child.wait_with_output()?;
    let envelope: Value = serde_json::from_slice(&finished.stdout)?;
    let state = envelope["data"]["receipt"]["state"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let exit = finished.status.code().unwrap_or(-1);
    // Killed before its claim, the operation was never performed. Killed while running, it is
    // unresolved. Neither is reported as done.
    if !matches!(state.as_str(), "unresolved" | "not_performed") || !matches!(exit, 1 | 7) {
        return Err(format!("a killed helper settled as {state} with exit {exit}").into());
    }
    Ok(format!("killed {killed}; settled as {state}, exit {exit}"))
}

fn stop(options: &Options) -> Result<String> {
    let preview = options.cli(&["server", "stop"])?.expect(3, "preview")?;
    if preview.envelope["data"]["confirmation"]["effects"]["start_requires_machine"] != true {
        return Err("the Stop preview does not say Start needs the machine".into());
    }
    let ran = options.cli(&["server", "stop", "--confirm", &preview.token()?])?;
    let operation = ran.envelope["data"]["operation"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let local = options.server == "local";
    match (local, ran.exit, ran.envelope["data"]["observed"].as_str()) {
        (false, 7, Some("shutdown")) | (true, 0 | 7, _) => {}
        (_, exit, observed) => {
            return Err(format!("stop exited {exit}, observed {observed:?}").into());
        }
    }
    options
        .machine_cli(&["server", "start"])?
        .expect(0, "local start")?;
    options.await_running()?;
    if !local {
        let status = options.cli(&["server", "status"])?.expect(0, "status")?;
        let settled = status.envelope["data"]["recovered_operations"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["operation"] == operation.as_str()
                        && item["kind"] == "service"
                        && item["outcome"]["state"] == "applied"
                })
            });
        if !settled {
            return Err("the next lane connection did not settle the Stop".into());
        }
    }
    Ok("shutdown, then a local Start and a settled receipt".into())
}

fn update_with(options: &Options, archive: &str, signature: &str, what: &str) -> Result<Ran> {
    let preview = options
        .cli(&[
            "server",
            "update",
            "--archive",
            archive,
            "--signature",
            signature,
        ])?
        .expect(3, what)?;
    let token = preview.token()?;
    let ran = options.cli(&[
        "server",
        "update",
        "--archive",
        archive,
        "--signature",
        signature,
        "--confirm",
        &token,
    ])?;
    Ok(Ran {
        exit: ran.exit,
        envelope: serde_json::json!({ "preview": preview.envelope, "result": ran.envelope }),
    })
}

fn update_upload(options: &Options) -> Result<String> {
    let (archive, signature) = options.candidate.as_ref().ok_or("no candidate")?;
    let ran = update_with(options, archive, signature, "update preview")?;
    let result = &ran.envelope["result"];
    if ran.exit != 0 || result["data"]["receipt"]["state"] != "done" {
        return Err(format!(
            "the update settled as {}",
            result["data"]["receipt"]["state"]
        )
        .into());
    }
    options.await_running()?;
    Ok(format!(
        "updated to {}",
        result["data"]["version"]["version"]
    ))
}

fn update_major5(options: &Options) -> Result<String> {
    let (archive, signature) = options.major5.as_ref().ok_or("no major-5 candidate")?;
    options.fixture("start-session", &[])?;
    let ran = update_with(options, archive, signature, "major-5 preview")?;
    let preview = &ran.envelope["preview"];
    let warned = preview["warnings"]
        .as_array()
        .is_some_and(|w| w.iter().any(|w| w["code"] == "client_major_differs"));
    if !warned {
        return Err("the preview did not warn about the protocol major".into());
    }
    if preview["data"]["confirmation"]["effects"]["ends_sessions"].as_u64() < Some(1) {
        return Err("the preview does not end the running Session".into());
    }
    let result = &ran.envelope["result"];
    if ran.exit != 0 || result["data"]["receipt"]["state"] != "done" {
        return Err(format!(
            "the update settled as {}",
            result["data"]["receipt"]["state"]
        )
        .into());
    }
    // The lane answers the major-5 server, though work commands need a matching CLI.
    let status = options
        .cli(&["server", "status"])?
        .expect(0, "status after the update")?;
    if status.envelope["data"]["sessions"] != 0 {
        return Err("Sessions survived an update".into());
    }
    Ok("Sessions ended, the lane reconnected across majors, and the receipt is done".into())
}

fn return_through_update(options: &Options) -> Result<String> {
    let (archive, signature) = options.broken.as_ref().ok_or("no broken candidate")?;
    let ran = update_with(options, archive, signature, "broken preview")?;
    let state = ran.envelope["result"]["data"]["receipt"]["state"].clone();
    if ran.exit == 0 {
        return Err("a candidate that cannot start updated the server".into());
    }
    // On the machine, `server update` settles the failed activation and offers return.
    let preview = options
        .machine_cli(&["server", "update"])?
        .expect(3, "return preview")?;
    if preview.envelope["data"]["confirmation"]["effects"]["kind"] != "return" {
        return Err("the machine's update did not offer return".into());
    }
    options
        .machine_cli(&["server", "update", "--confirm", &preview.token()?])?
        .expect(0, "return")?;
    options
        .machine_cli(&["server", "start"])?
        .expect(0, "local start")?;
    options.await_running()?;
    Ok(format!(
        "the broken update settled as {state}, then returned"
    ))
}

fn pairing(options: &Options) -> Result<String> {
    options.fixture("pair-checks", &[&options.machine_cli])
}

fn setup(options: &Options) -> Result<String> {
    let Some(network) = &options.network else {
        return Ok("skipped: pass --network to keep the server's Network Access".into());
    };
    for (toggle, enabled) in [("off", false), ("on", true)] {
        let ran = options
            .machine_cli(&[
                "server",
                "setup",
                "--network",
                network,
                "--automatic-start",
                toggle,
            ])?
            .expect(0, "setup")?;
        if ran.envelope["data"]["automatic_start"] != enabled {
            return Err(format!("automatic start is not {toggle}").into());
        }
    }
    let again = options
        .machine_cli(&["server", "setup", "--network", network])?
        .expect(0, "setup again")?;
    let all_settled = again.envelope["data"]["steps"]
        .as_array()
        .is_some_and(|steps| steps.iter().all(|step| step["result"] != "done"));
    if !all_settled {
        return Err("a repeated setup changed something".into());
    }
    Ok("automatic start round-trips, and a repeated setup is already done".into())
}

// Fixture role, on the server's machine

fn fixture(arguments: &[String]) -> Result<()> {
    let [identity, action, rest @ ..] = arguments else {
        return Err("fixture needs an identity and an action".into());
    };
    let line = match action.as_str() {
        "fill-ledger" => fill_ledger(identity)?,
        "clear-ledger" => {
            Ledger::open(identity)?.forget_pairing(&FIXTURE_PAIRING)?;
            "cleared".into()
        }
        "inject-admitted" => inject_admitted(identity, rest.first().ok_or("operation")?)?,
        "ledger-state" => ledger_state(identity, rest.first().ok_or("operation")?)?,
        "end-server" => end_server(identity)?,
        "kill-helper" => kill_helper(identity, rest.first().map_or("30", String::as_str))?,
        "start-session" => start_session(identity)?,
        "pair-checks" => pair_checks(rest.first().ok_or("cli")?)?,
        other => return Err(format!("unknown fixture action {other}").into()),
    };
    println!("{line}");
    Ok(())
}

fn fixture_record(identity: &str, id: OperationId, state: State) -> Result<Record> {
    let server = spaceterm_server::service::Service::for_identity(identity)?
        .passive_server_id()?
        .ok_or("no server")?;
    Ok(Record {
        id,
        actor: Actor {
            pairing: Some(FIXTURE_PAIRING),
            client: FIXTURE_PAIRING,
        },
        server,
        // No server run has this boot, so recovery treats the record as its predecessor's.
        boot: [0; 32],
        action: Action::Restart,
        observation_digest: [0; 32],
        candidate: None,
        state,
        updated_at: 0,
    })
}

fn fill_ledger(identity: &str) -> Result<String> {
    let mut ledger = Ledger::open(identity)?;
    let mut written = 0;
    loop {
        let id = OperationId::random()?;
        match ledger.admit(fixture_record(identity, id, State::Admitted)?) {
            Ok(_) => {
                ledger.mark(&id, State::Unresolved)?;
                written += 1;
            }
            Err(spaceterm_server::ServerError::Capacity) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(format!("wrote {written}"))
}

fn inject_admitted(identity: &str, operation: &str) -> Result<String> {
    let id = OperationId::parse(operation).ok_or("operation")?;
    Ledger::open(identity)?.admit(fixture_record(identity, id, State::Admitted)?)?;
    Ok("admitted".into())
}

fn ledger_state(identity: &str, operation: &str) -> Result<String> {
    let id = OperationId::parse(operation).ok_or("operation")?;
    let record = Ledger::open(identity)?.resolve(&id)?.ok_or("no record")?;
    Ok(match record.state {
        State::Admitted => "admitted",
        State::Launched => "launched",
        State::Running => "running",
        State::Done => "done",
        State::Failed { .. } => "failed",
        State::NotPerformed(_) => "not_performed",
        State::Unresolved => "unresolved",
    }
    .into())
}

/// The server process's own pattern, from its unit's argv.
fn server_pattern(identity: &str) -> String {
    format!("server run --service --identity {identity}")
}

/// Ends the server cleanly with SIGTERM and waits until it has exited. Its supervisor does not
/// restart a successful exit, so a local Start brings it back.
fn end_server(identity: &str) -> Result<String> {
    let pattern = server_pattern(identity);
    Command::new("pkill")
        .args(["-TERM", "-f", &pattern])
        .status()?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while Command::new("pgrep")
        .args(["-f", &pattern])
        .stdout(Stdio::null())
        .status()?
        .success()
    {
        if Instant::now() >= deadline {
            return Err("the server did not end".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok("ended".into())
}

/// Kills the first helper of `identity` that appears within `seconds`.
fn kill_helper(identity: &str, seconds: &str) -> Result<String> {
    let pattern = format!("service-run --operation .* --identity {identity}");
    let deadline = Instant::now() + Duration::from_secs(seconds.parse()?);
    loop {
        let found = Command::new("pgrep").args(["-f", &pattern]).output()?;
        let pids = String::from_utf8(found.stdout)?;
        if let Some(pid) = pids.split_whitespace().next() {
            Command::new("kill").args(["-KILL", pid]).status()?;
            return Ok(format!("helper {pid}"));
        }
        if Instant::now() >= deadline {
            return Err("no helper appeared".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Starts a Terminal Session on the server and leaves it running.
fn start_session(identity: &str) -> Result<String> {
    let agent = "admin-native".parse()?;
    let server = ServerConnection::for_cli(identity, CliRoute::Local, Some(agent))?;
    let link = support::connect(&server)?;
    let pane = support::attach(
        &link,
        true,
        spaceterm_server::pane::PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
    )?;
    pane.command("true")?;
    Ok("started".into())
}

/// The pairing checks a person at the server's machine would see: no invitation without a
/// terminal, the advertised address in the link, and Ctrl-C withdrawing the invitation.
fn pair_checks(cli: &str) -> Result<String> {
    for arguments in [
        &["server", "pair", "--json"][..],
        &["server", "pair", "--no-input"],
    ] {
        let status = Command::new(cli)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.code() != Some(3) {
            return Err(format!("{arguments:?} did not exit 3").into());
        }
    }
    // An IP, and a DNS name with a port, as a NAT or cloud machine advertises itself.
    for address in ["203.0.113.5", "build.example.test:9300"] {
        let (link, exit, withdrawn) = pair_on_pty(cli, &["server", "pair", "--address", address])?;
        let invitation = spaceterm_server::direct::Invitation::parse(&link)
            .map_err(|_| "the first line is no Invitation Link")?;
        if !invitation.endpoint().contains(address) {
            return Err(format!("the link does not advertise {address}").into());
        }
        if exit != 130 || !withdrawn {
            return Err(format!("Ctrl-C exited {exit} and withdrew: {withdrawn}").into());
        }
    }
    Ok("no invitation without a terminal; --address advertised; Ctrl-C withdraws".into())
}

/// Runs `cli` on a new pty, reads its Invitation Link, interrupts it and reports its exit and
/// whether it said the invitation was withdrawn.
fn pair_on_pty(cli: &str, arguments: &[&str]) -> Result<(String, i32, bool)> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty fills two descriptors; both are owned below.
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0
    {
        return Err("no pty".into());
    }
    // SAFETY: openpty returned fresh descriptors that nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut child = Command::new(cli)
        .args(arguments)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .spawn()?;
    let mut reader = std::fs::File::from(master);
    let mut seen = String::new();
    let link = read_until(&mut reader, &mut seen, |text| {
        text.lines()
            .find(|line| line.contains("://"))
            .map(str::to_owned)
    })?;
    // SAFETY: the child's PID is live until it is waited for below.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    let status = child.wait()?;
    let _ = read_until(&mut reader, &mut seen, |text| {
        text.contains("withdrawn").then_some(())
    });
    Ok((
        link.trim().to_owned(),
        status.code().unwrap_or(-1),
        seen.contains("withdrawn"),
    ))
}

fn read_until<T>(
    reader: &mut std::fs::File,
    seen: &mut String,
    found: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut buffer = [0u8; 4096];
    loop {
        if let Some(value) = found(seen) {
            return Ok(value);
        }
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 200) };
        if ready > 0 {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return found(seen).ok_or_else(|| "the pty closed".into()),
                Ok(count) => seen.push_str(&String::from_utf8_lossy(&buffer[..count])),
            }
        }
        if Instant::now() >= deadline {
            return Err("timed out reading the pty".into());
        }
    }
}
