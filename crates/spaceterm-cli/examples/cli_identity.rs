//! CLI identities against a real Development server in this process: one identity's mutating
//! commands serialize across processes, distinct agents run in parallel, and the next command
//! settles every killed agent's held outcome so the desktop's edits are never refused for
//! capacity. Each CLI command runs in a child process, as `spaceterm` does.
//!
//! The parent refuses to run while a Development server runs or Development state exists, and
//! removes the state it created.
use spaceterm_cli::journal::{Editor, recovered_warning};
use spaceterm_cli::state::{CliState, RouteKey};
use spaceterm_server::{
    CliRoute, ServerConnection, ServerError,
    connection::link::{LinkEvent, ServerLink},
    hierarchy::{CreateControl, Mutation, Outcome},
    protocol::{AgentName, Operation, ReplyBody},
};
use std::error::Error;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IDENTITY: &str = "spaceterm-development";
/// How long a serializing child keeps its writer after its edit, so overlap would show.
const HOLD: Duration = Duration::from_millis(600);
/// The server keeps 32 unacknowledged outcomes for all clients together.
const ABANDONED: usize = 33;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let result = match arguments.first().map(String::as_str) {
        Some("edit") => edit(&arguments[1], &arguments[2]),
        Some("abandon") => abandon(&arguments[1]),
        None => parent(),
        Some(_) => Err("unknown role".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cli_identity: {error}");
            ExitCode::FAILURE
        }
    }
}

fn agent(name: &str) -> Result<Option<AgentName>> {
    Ok(match name {
        "base" => None,
        name => Some(name.parse()?),
    })
}

fn handle(agent: Option<&AgentName>) -> std::result::Result<ServerConnection, ServerError> {
    ServerConnection::for_cli(IDENTITY, CliRoute::Local, agent.cloned())
}

fn connect(server: &ServerConnection) -> Result<ServerLink> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match server.connect(Arc::new(|_: LinkEvent| {})) {
            Ok(link) => return Ok(link),
            Err(ServerError::NotRunning) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis())
}

/// One mutating command: creates a Workspace, then holds its writer for [`HOLD`]. Prints when
/// it held the writer, in milliseconds since the Unix epoch.
fn edit(name: &str, workspace: &str) -> Result<()> {
    let agent = agent(name)?;
    let state = CliState::open(IDENTITY)?;
    let server = handle(agent.as_ref())?;
    let link = connect(&server)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut editor = Editor::open(
        &state,
        RouteKey::Local,
        agent.as_ref(),
        &server,
        &link,
        &mut |agent| handle(agent),
        deadline,
    )
    .map_err(|failure| failure.text())?;
    let start = now_ms();
    editor
        .mutate(
            Mutation::CreateWorkspace {
                name: workspace.into(),
                directory: None,
                control: CreateControl::Creator,
            },
            deadline,
        )
        .map_err(|failure| failure.text())?;
    std::thread::sleep(HOLD);
    println!("{start} {}", now_ms());
    Ok(())
}

/// A command killed after its edit applied and before it acknowledged it: the server keeps its
/// outcome, and its journal keeps the edit pending.
fn abandon(name: &str) -> Result<()> {
    let agent = agent(name)?;
    let state = CliState::open(IDENTITY)?;
    let server = handle(agent.as_ref())?;
    let link = connect(&server)?;
    let mut writer = state
        .writer(&RouteKey::Local, agent.as_ref(), Instant::now())
        .map_err(|code| code.as_str())?;
    let journal = writer.journal(&server).map_err(|failure| failure.text())?;
    let (_, revision) = link.next_inventory(0)?;
    let mutation = Mutation::CreateWorkspace {
        name: format!("abandoned {name}"),
        directory: None,
        control: CreateControl::Unclaimed,
    };
    journal.submit(revision.next_sequence, mutation.clone())?;
    // The 33rd is refused for capacity; its journal keeps an edit the server never recorded.
    let _ = link
        .request(Operation::Mutate {
            sequence: revision.next_sequence,
            mutation,
        })?
        .wait();
    std::process::exit(0);
}

fn child(arguments: &[&str]) -> Result<std::process::Child> {
    Ok(Command::new(std::env::current_exe()?)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?)
}

/// Runs two `edit` children at once and returns the intervals they held their writers.
fn concurrently(first: &str, second: &str) -> Result<[(u128, u128); 2]> {
    let children = [
        child(&["edit", first, &format!("identity {first} 1")])?,
        child(&["edit", second, &format!("identity {second} 2")])?,
    ];
    let mut intervals = [(0, 0); 2];
    for (interval, child) in intervals.iter_mut().zip(children) {
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err("a mutating command failed".into());
        }
        let text = String::from_utf8(output.stdout)?;
        let mut fields = text.split_whitespace().map(str::parse::<u128>);
        *interval = (fields.next().ok_or("start")??, fields.next().ok_or("end")??);
    }
    Ok(intervals)
}

fn overlap([first, second]: [(u128, u128); 2]) -> bool {
    first.0 < second.1 && second.0 < first.1
}

/// A desktop's edit, the way the desktop dispatches and acknowledges it.
fn desktop_edit(link: &ServerLink, name: &str) -> Result<Outcome> {
    let (_, revision) = link.next_inventory(0)?;
    let sequence = revision.next_sequence;
    let reply = link
        .request(Operation::Mutate {
            sequence,
            mutation: Mutation::CreateWorkspace {
                name: name.into(),
                directory: None,
                control: CreateControl::Creator,
            },
        })?
        .wait()?;
    let ReplyBody::Outcome(outcome) = reply else {
        return Err("the edit has an outcome".into());
    };
    link.acknowledge(sequence)?;
    Ok(outcome.outcome)
}

fn parent() -> Result<()> {
    let data = spaceterm_server::cli_state_root(IDENTITY)?
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("data directory")?
        .to_owned();
    let state_root = data.parent().ok_or("state root")?.to_owned();
    if state_root.exists() {
        return Err("Remove the Development state before this exercise.".into());
    }
    let base = handle(None)?;
    if base.refresh().is_ok() {
        return Err("Stop the Development server before this exercise.".into());
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exercise(&base, &data)))
        .unwrap_or_else(|_| Err("an expectation failed".into()));
    // SAFETY: raising SIGTERM in this process asks the in-process server to stop.
    unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
    std::thread::sleep(Duration::from_millis(500));
    std::fs::remove_dir_all(&state_root)?;
    result
}

fn exercise(base: &ServerConnection, data: &std::path::Path) -> Result<()> {
    base.setup()?;
    std::thread::spawn(|| {
        spaceterm_server::run_local_with_panes(
            IDENTITY,
            spaceterm_server::panes(|sync| {
                Ok(spaceterm::runtime::Served::new(
                    spaceterm::runtime::WorkspaceHost::local(sync)?,
                ))
            }),
        )
    });
    drop(connect(base)?);

    let serialized = concurrently("base", "base")?;
    assert!(
        !overlap(serialized),
        "two processes of one identity mutate one at a time: {serialized:?}"
    );
    println!("one identity serializes, and both commands succeed");
    let parallel = concurrently("agent-a", "agent-b")?;
    assert!(
        overlap(parallel),
        "distinct agents mutate in parallel: {parallel:?}"
    );
    println!("distinct agents run in parallel");

    for index in 0..ABANDONED {
        let status = child(&["abandon", &format!("killed-{index:02}")])?.wait()?;
        assert!(status.success(), "abandoned agent {index}");
    }
    let state = CliState::open(IDENTITY)?;
    let held = state.held(&RouteKey::Local)?;
    assert_eq!(held.len(), ABANDONED, "{held:?}");
    let link = connect(base)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let editor = Editor::open(
        &state,
        RouteKey::Local,
        None,
        base,
        &link,
        &mut |agent| handle(agent),
        deadline,
    )
    .map_err(|failure| failure.text())?;
    assert_eq!(editor.recovered().len(), ABANDONED);
    assert!(recovered_warning(editor.recovered()).is_some());
    drop(editor);
    assert!(state.held(&RouteKey::Local)?.is_empty());
    println!("the next command settled {ABANDONED} killed agents");
    let desktop = ServerConnection::for_development_client("cli-identity")?;
    let desktop_link = connect(&desktop)?;
    assert!(matches!(
        desktop_edit(&desktop_link, "desktop after settlement")?,
        Outcome::Accepted { .. }
    ));
    println!("the desktop is not refused for capacity");

    assert!(
        !data.join("clients/desktop").exists(),
        "the CLI never touches the desktop's client state"
    );
    println!("the CLI never touched clients/desktop");
    Ok(())
}
