//! Process entry for work commands over the selected account-local or paired route.
//! Account-local commands never construct a native credential store.
use super::{
    backend::Connected,
    dispatch,
    execute::{self, Environment},
    output::Output,
};
use crate::connection::{NativeCredentials, route};
use crate::{
    args::{Cli, Command, WorktreeCommand},
    contract::*,
};
use spaceterm_connections::Interaction;
use spaceterm_server::protocol::AgentName;
use std::{
    io::{BufRead, IsTerminal, Read, Write},
    time::{Duration, Instant},
};

pub fn start(cli: &Cli) -> Option<Output> {
    if !execute::handles(&cli.command) {
        return None;
    }
    Some(selected(cli))
}

fn selected(cli: &Cli) -> Output {
    let failed = |failure| {
        Output::failed(
            cli.command.path(),
            None,
            failure,
            serde_json::json!({}),
            String::new(),
        )
    };
    let environment = if cli.global.server.is_none() {
        std::env::var_os("SPACETERM_SERVER")
    } else {
        None
    };
    let environment = match environment
        .as_ref()
        .map(|value| {
            value.to_str().ok_or_else(|| {
                Failure::with(
                    Code::InvalidArguments,
                    Details::Usage(UsageDetails {
                        argument: Some("--server".into()),
                        kind: UsageKind::InvalidUtf8,
                    }),
                )
            })
        })
        .transpose()
    {
        Ok(environment) => environment,
        Err(failure) => return failed(failure),
    };
    let selector = match route::selected(cli.global.server.as_deref(), environment) {
        Ok(selector) => selector,
        Err(failure) => return failed(failure),
    };
    let local = selector == "local";
    if let Err(failure) = execute::validate(&cli.command, local) {
        return failed(failure);
    }
    let agent = match cli
        .global
        .agent
        .clone()
        .map(Ok)
        .or_else(|| {
            std::env::var("SPACETERM_AS")
                .ok()
                .map(|a| a.parse::<AgentName>())
        })
        .transpose()
    {
        Ok(agent) => agent,
        Err(_) => {
            return failed(Failure::with(
                Code::InvalidArguments,
                Details::Usage(UsageDetails {
                    argument: Some("--as".into()),
                    kind: UsageKind::Other,
                }),
            ));
        }
    };
    let identity = env!("SPACETERM_SERVER_IDENTITY");
    let selected = match route::from_global(&cli.global, identity) {
        Ok(selected) => selected,
        Err(error) => {
            return Output::failed(
                cli.command.path(),
                None,
                error.failure,
                serde_json::json!({"candidates":error.data.candidates}),
                String::new(),
            );
        }
    };
    let credentials = NativeCredentials(identity);
    let interaction = if cli.global.json || cli.global.no_input || !std::io::stdin().is_terminal() {
        Interaction::Quiet
    } else {
        Interaction::Requested
    };
    let timeout = cli.global.timeout.unwrap_or_else(|| {
        if matches!(
            cli.command,
            Command::Worktree(WorktreeCommand::Create { .. })
        ) {
            Duration::from_secs(300)
        } else {
            Duration::from_secs(30)
        }
    });
    let deadline = Instant::now() + timeout;
    let connection =
        match selected.connect(identity, agent.clone(), &credentials, interaction, deadline) {
            Ok(connection) => connection,
            Err(failure) => return failed(failure),
        };
    let link = match connection.link_cancellable(&|| Instant::now() >= deadline) {
        Ok(link) => link,
        Err(e) => {
            return failed(if Instant::now() >= deadline {
                Failure::new(Code::Timeout)
            } else {
                from_server_error(e)
            });
        }
    };
    let server = match connection.admitted_server_id() {
        Ok(server) => selected.reference(&server),
        Err(error) => return failed(from_server_error(error)),
    };
    let mut connect = |agent: Option<&AgentName>| {
        selected.server_connection(
            identity,
            agent.cloned(),
            &credentials,
            interaction,
            deadline,
        )
    };
    let mut journal = match dispatch::open(
        &connection,
        &link,
        &dispatch::Configuration {
            identity,
            route: selected.key(),
            agent: agent.as_ref(),
            deadline,
            mutating: !matches!(cli.command, Command::List(_)),
        },
        &mut connect,
    ) {
        Ok(j) => j,
        Err(e) => return failed(e),
    };
    let mut reader = Connected {
        link: link.clone(),
        actor: journal.client_id(),
        deadline,
    };
    run_connected(
        cli,
        &Route {
            identity,
            local,
            name: &server.name,
            route: &server.route,
            agent: agent.is_some(),
        },
        &mut *journal,
        &mut reader,
    )
}

pub struct Route<'a> {
    pub identity: &'a str,
    pub local: bool,
    pub name: &'a str,
    pub route: &'a str,
    pub agent: bool,
}

pub fn run_connected(
    cli: &Cli,
    selected: &Route<'_>,
    journal: &mut dyn dispatch::Dispatch,
    reader: &mut dyn super::backend::Read,
) -> Output {
    let Route {
        identity,
        local,
        name: server_name,
        route,
        agent,
    } = *selected;
    let failed = |failure| {
        Output::failed(
            cli.command.path(),
            None,
            failure,
            serde_json::json!({}),
            String::new(),
        )
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return failed(Failure::new(Code::DirectoryUnavailable)),
    };
    let current_identity = std::env::var("SPACETERM_IDENTITY").ok();
    let current_server = std::env::var("SPACETERM_SERVER_ID").ok();
    let current_pane = std::env::var("SPACETERM_PANE").ok();
    let mut env = Environment {
        identity,
        local,
        cwd: &cwd,
        current_identity: current_identity.as_deref(),
        current_server: current_server.as_deref(),
        current_pane: current_pane.as_deref(),
        agent,
        now: jiff::Timestamp::now().as_second(),
        confirmation: None,
    };
    let mut output = execute::run(
        &cli.command,
        &cli.global,
        &env,
        journal,
        reader,
        server_name,
        route,
    );
    if output.envelope.status == Status::ConfirmationRequired
        && !cli.global.json
        && !cli.global.no_input
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        let token = output.envelope.data.body["confirmation"]["token"]
            .as_str()
            .map(str::to_owned);
        let mut stdout = std::io::stdout().lock();
        let mut stderr = std::io::stderr().lock();
        if stdout
            .write_all(output.text.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            return failed(Failure::new(Code::OutputFailed));
        }
        let _ = stderr.write_all(b"Continue? [y/N] ");
        let _ = stderr.flush();
        let mut answer = String::new();
        let read = std::io::stdin().lock().take(128).read_line(&mut answer);
        if read.is_ok()
            && matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
            && let Some(token) = token
        {
            env.confirmation = Some(&token);
            env.now = jiff::Timestamp::now().as_second();
            let warnings = output.envelope.warnings;
            let recovered = output.envelope.data.recovered_operations;
            output = execute::run(
                &cli.command,
                &cli.global,
                &env,
                journal,
                reader,
                server_name,
                route,
            );
            output.envelope.warnings.extend(warnings);
            output.envelope.data.recovered_operations.extend(recovered);
        } else {
            output.text.clear();
        }
    }
    output
}
