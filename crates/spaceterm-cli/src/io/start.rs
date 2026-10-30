//! Process entry for `pane read`, `run`, `send` and `release` over the selected route.
//!
//! The route, its connection and the target come from the same code the work commands use: the
//! selected route, this identity's journals settled first, and one Pane resolved from one
//! inventory snapshot. `read` is passive; the others mutate.
use super::{Deadline, Done, PaneTarget, read, run, send};
use crate::args::{Cli, Command, PaneCommand};
use crate::connection::{NativeCredentials, route};
use crate::contract::*;
use crate::scope::{self, Snapshot, TargetData};
use crate::work::backend::{Connected, Read as _};
use crate::work::{output::Output, paths};
use spaceterm_server::protocol::AgentName;
use std::time::{Duration, Instant};

/// Connecting, admission and targeting give up after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn start(cli: &Cli) -> Option<Output> {
    let Command::Pane(
        command @ (PaneCommand::Run(_)
        | PaneCommand::Send(_)
        | PaneCommand::Read(_)
        | PaneCommand::Release { .. }),
    ) = &cli.command
    else {
        return None;
    };
    Some(selected(cli, command))
}

fn failed(path: &str, server: Option<ServerRef>, failure: Failure) -> Output {
    Output::failed(path, server, failure, serde_json::json!({}), String::new())
}

fn selected(cli: &Cli, command: &PaneCommand) -> Output {
    let path = cli.command.path();
    let identity = env!("SPACETERM_SERVER_IDENTITY");
    let deadline = Deadline::after(cli.global.timeout);
    let connecting = Instant::now()
        + cli
            .global
            .timeout
            .unwrap_or(CONNECT_TIMEOUT)
            .min(CONNECT_TIMEOUT);
    let route::RoutedConnection {
        selected,
        connection,
        agent,
        interaction,
    } = match route::connect(identity, &cli.global, Some(connecting)) {
        Ok(routed) => routed,
        Err(error) => {
            let data = error.operational_data();
            return Output::failed(
                path,
                error.server.map(|server| *server),
                error.failure,
                data,
                String::new(),
            );
        }
    };
    let link = match connection.link_cancellable(&|| Instant::now() >= connecting) {
        Ok(link) => link,
        Err(error) => {
            let failure = if Instant::now() >= connecting {
                Failure::new(Code::Timeout)
            } else {
                from_server_error(error)
            };
            return failed(path, None, failure);
        }
    };
    let server_id = match connection.admitted_server_id() {
        Ok(server) => server,
        Err(error) => return failed(path, None, from_server_error(error)),
    };
    let server = selected.reference(&server_id);
    let credentials = NativeCredentials(identity);
    let mut reconnect = |agent: Option<&AgentName>| {
        selected.server_connection(
            identity,
            agent.cloned(),
            &credentials,
            interaction,
            Some(connecting),
        )
    };
    let state = match crate::state::CliState::open(identity) {
        Ok(state) => state,
        Err(error) => return failed(path, Some(server), from_server_error(error)),
    };
    let recovered = match crate::journal::settle(
        &state,
        &selected.key(),
        crate::journal::Current {
            agent: agent.as_ref(),
            connection: &connection,
            link: &link,
            writer: None,
        },
        &mut reconnect,
        connecting,
    ) {
        Ok(recovered) => recovered,
        Err(failure) => return failed(path, Some(server), failure),
    };
    let actor = match connection.admitted_client_id() {
        Ok(actor) => actor,
        Err(error) => return failed(path, Some(server), from_server_error(error)),
    };
    let mut reader = Connected {
        link: link.clone(),
        actor,
        deadline: connecting,
    };
    let target = match target(cli, command, &mut reader, server_id) {
        Ok(target) => target,
        Err(output) => return *output,
    };
    let mut output = match command {
        PaneCommand::Read(args) => render(
            path,
            &server,
            read::read(&link, &target, args, deadline),
            |data| data.rows.as_ref().map(rows_text).unwrap_or_default(),
        ),
        PaneCommand::Send(args) => render(
            path,
            &server,
            send::send(&link, &target, args, &mut std::io::stdin().lock(), deadline),
            |_| String::new(),
        ),
        PaneCommand::Release { .. } => render(
            path,
            &server,
            send::release(&link, &target, deadline),
            |_| String::new(),
        ),
        PaneCommand::Run(args) => {
            let directory = match &args.directory {
                Some(directory) => {
                    match resolve_directory(&mut reader, directory, selected.key()) {
                        Ok(directory) => Some(directory),
                        Err(failure) => return failed(path, Some(server), failure),
                    }
                }
                None => None,
            };
            let mut runs = match state.runs(&selected.key(), agent.as_ref(), connecting) {
                Ok(runs) => runs,
                Err(code) => return failed(path, Some(server), Failure::new(code)),
            };
            let mut connect = || {
                selected
                    .server_connection(
                        identity,
                        agent.clone(),
                        &credentials,
                        interaction,
                        Some(Instant::now() + CONNECT_TIMEOUT),
                    )
                    .and_then(|connection| connection.link())
                    .map_err(from_server_error)
            };
            render(
                path,
                &server,
                run::run(
                    &link,
                    &mut connect,
                    &target,
                    args,
                    directory,
                    &mut std::io::stdin().lock(),
                    &mut runs,
                    deadline,
                ),
                |data| data.output.as_ref().map(rows_text).unwrap_or_default(),
            )
        }
        _ => unreachable!("start handles only terminal I/O"),
    };
    if let Some(warning) = crate::journal::recovered_warning(&recovered) {
        output.envelope.warnings.insert(0, warning);
    }
    output.envelope.data.recovered_operations = recovered;
    output.diagnostics = scope::verbose(&target.steps);
    output
}

/// Resolves the command's Pane from one inventory snapshot.
fn target(
    cli: &Cli,
    command: &PaneCommand,
    reader: &mut Connected,
    server: [u8; 32],
) -> Result<PaneTarget, Box<Output>> {
    let path = cli.command.path();
    let fail = |failure| Box::new(failed(path, None, failure));
    let (scope_args, effect) = match command {
        PaneCommand::Read(args) => (args.scope.args(), scope::Effect::Passive),
        PaneCommand::Run(args) => (args.scope.args(), scope::Effect::Mutating),
        PaneCommand::Send(args) => (args.scope.args(), scope::Effect::Mutating),
        PaneCommand::Release { scope } => (scope.args(), scope::Effect::Mutating),
        _ => unreachable!("start handles only terminal I/O"),
    };
    let inventory = reader.inventory().map_err(fail)?;
    let snapshot = Snapshot {
        server,
        repositories: inventory.repositories.clone(),
        inventory,
    };
    let current = scope::current_from_context(
        &snapshot,
        env!("SPACETERM_SERVER_IDENTITY"),
        std::env::var("SPACETERM_IDENTITY").ok().as_deref(),
        std::env::var("SPACETERM_SERVER_ID").ok().as_deref(),
        std::env::var("SPACETERM_PANE").ok().as_deref(),
    );
    let home = reader.home().map_err(fail)?;
    let cwd =
        std::env::current_dir().map_err(|_| fail(Failure::new(Code::DirectoryUnavailable)))?;
    let local = route::selector(&cli.global).is_ok_and(|selector| selector == "local");
    let scope_args = paths::Paths {
        local,
        cwd: &cwd,
        home: &home,
    }
    .scope(&scope_args)
    .map_err(|code| fail(Failure::new(code)))?;
    let resolved = scope::resolve(
        &snapshot,
        &scope_args,
        current.as_ref(),
        scope::Want::Pane,
        effect,
    )
    .map_err(|error| {
        let (failure, data) = error.into_parts();
        let text = scope::data_text(&data);
        let data = match data {
            TargetData::None => serde_json::json!({}),
            TargetData::Candidates(data) => serde_json::to_value(data).unwrap_or_default(),
            TargetData::Mismatch(data) => serde_json::to_value(data).unwrap_or_default(),
        };
        Box::new(Output::failed(path, None, failure, data, text))
    })?;
    let pane = resolved
        .pane
        .ok_or_else(|| fail(Failure::new(Code::NotFound)))?;
    let placement = snapshot
        .inventory
        .placement(pane)
        .ok_or_else(|| fail(Failure::new(Code::NotFound)))?;
    let session = snapshot.inventory.close.as_ref().and_then(|close| {
        close
            .sessions
            .iter()
            .find(|session| session.id == pane)
            .and_then(|session| session.session)
    });
    Ok(PaneTarget {
        server,
        workspace: placement.workspace,
        pane,
        running: session.is_some(),
        session,
        steps: scope::target_steps(&snapshot, &resolved),
    })
}

/// `--directory`, by the route's path rules: a remote route accepts only absolute paths and
/// `~/`.
fn resolve_directory(
    reader: &mut Connected,
    directory: &std::path::Path,
    route: crate::state::RouteKey,
) -> Result<std::path::PathBuf, Failure> {
    let home = reader.home()?;
    let cwd = std::env::current_dir().map_err(|_| Failure::new(Code::DirectoryUnavailable))?;
    paths::Paths {
        local: route == crate::state::RouteKey::Local,
        cwd: &cwd,
        home: &home,
    }
    .resolve(directory)
    .map_err(|_| {
        Failure::with(
            Code::InvalidArguments,
            Details::Usage(UsageDetails {
                argument: Some("--directory".into()),
                kind: UsageKind::InvalidValue,
            }),
        )
    })
}

/// The envelope of a terminal command, with its text.
fn render<D: serde::Serialize>(
    path: &str,
    server: &ServerRef,
    done: Done<D>,
    text: impl Fn(&D) -> String,
) -> Output {
    let Done {
        data,
        warnings,
        failure,
    } = done;
    let rendered = data.as_ref().map(&text).unwrap_or_default();
    let mut output = match failure {
        None => Output::ok(
            path,
            server.clone(),
            data.expect("a successful command has data"),
            rendered,
            &[],
        ),
        Some(failure) => Output::failed(
            path,
            Some(server.clone()),
            failure,
            data.map_or_else(
                || serde_json::json!({}),
                |data| serde_json::to_value(data).unwrap_or_default(),
            ),
            rendered,
        ),
    };
    output.envelope.warnings.extend(warnings);
    output
}

/// Rows as text: soft-wrapped rows join, then the screen without its trailing blank rows.
pub fn rows_text(rows: &Rows) -> String {
    let mut text = String::new();
    for row in &rows.rows {
        text.push_str(&row.text);
        if !row.wrapped {
            text.push('\n');
        }
    }
    if let Some(screen) = &rows.screen {
        let end = screen
            .rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .map_or(0, |last| last + 1);
        for row in &screen.rows[..end] {
            text.push_str(row);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
#[path = "golden_tests.rs"]
mod golden_tests;
