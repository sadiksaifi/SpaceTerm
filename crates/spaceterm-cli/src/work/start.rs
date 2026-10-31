//! Process entry for work commands over the selected account-local or paired route.
//! Account-local commands never construct a native credential store.
use super::{
    backend::Connected,
    dispatch,
    execute::{self, Environment},
    output::Output,
};
use crate::connection::{CliCredentials, route};
use crate::{
    args::{Cli, Command, WorktreeCommand},
    contract::*,
};
use spaceterm_server::protocol::AgentName;
use std::{
    io::{BufRead, IsTerminal, Read, Write},
    time::{Duration, Instant},
};

pub fn start(cli: &Cli) -> Option<Output> {
    if !execute::handles(&cli.command) {
        return None;
    }
    let credentials = CliCredentials::new(env!("SPACETERM_SERVER_IDENTITY"));
    let mut output = selected(cli, &credentials);
    credentials.report(&mut output.envelope);
    Some(output)
}

fn selected(cli: &Cli, credentials: &CliCredentials<'_>) -> Output {
    let failed = |failure| {
        Output::failed(
            cli.command.path(),
            None,
            failure,
            serde_json::json!({}),
            String::new(),
        )
    };
    let selector = match route::selector(&cli.global) {
        Ok(selector) => selector,
        Err(failure) => return failed(failure),
    };
    let local = selector == "local";
    if let Err(failure) = execute::validate(&cli.command, local) {
        return failed(failure);
    }
    let identity = env!("SPACETERM_SERVER_IDENTITY");
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
    let route::RoutedConnection {
        selected,
        connection,
        agent,
        interaction,
    } = match route::connect(identity, &cli.global, credentials, Some(deadline)) {
        Ok(routed) => routed,
        Err(error) => {
            let data = error.operational_data();
            return Output::failed(
                cli.command.path(),
                error.server.map(|server| *server),
                error.failure,
                data,
                String::new(),
            );
        }
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
    run_with_confirmation(
        deadline,
        timeout,
        |deadline, confirmation| {
            let mut connect = |agent: Option<&AgentName>| {
                selected.server_connection(
                    identity,
                    agent.cloned(),
                    credentials,
                    interaction,
                    Some(deadline),
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
                    mutating: !matches!(cli.command, Command::List(_) | Command::Show(_)),
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
                    deadline,
                    local,
                    name: &server.name,
                    route: &server.route,
                    agent: agent.is_some(),
                },
                confirmation,
                &mut *journal,
                &mut reader,
            )
        },
        |output| prompt(cli, output),
    )
}

/// Each pass owns its writer until it returns. Human input happens between passes, and the
/// confirmed pass re-reads the server facts against the displayed token with a fresh deadline.
fn run_with_confirmation(
    deadline: Instant,
    timeout: Duration,
    mut run: impl FnMut(Instant, Option<&str>) -> Output,
    mut prompt: impl FnMut(&Output) -> Result<Option<bool>, Failure>,
) -> Output {
    let mut output = run(deadline, None);
    if output.envelope.status != Status::ConfirmationRequired {
        return output;
    }
    match prompt(&output) {
        Ok(Some(true)) => {
            if let Some(token) = output.envelope.data.body["confirmation"]["token"].as_str() {
                let mut confirmed = run(Instant::now() + timeout, Some(token));
                confirmed.envelope.warnings.extend(output.envelope.warnings);
                confirmed
                    .envelope
                    .data
                    .recovered_operations
                    .extend(output.envelope.data.recovered_operations);
                return confirmed;
            }
        }
        Ok(Some(false)) => {}
        Ok(None) => return output,
        Err(failure) => {
            output.envelope.status = Status::Error;
            output.envelope.error = Some(failure.into());
        }
    }
    output.text.clear();
    output
}

fn prompt(cli: &Cli, output: &Output) -> Result<Option<bool>, Failure> {
    if cli.global.json
        || cli.global.no_input
        || !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
    {
        return Ok(None);
    }
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    stdout
        .write_all(output.text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|_| Failure::new(Code::OutputFailed))?;
    let _ = stderr.write_all(b"Continue? [y/N] ");
    let _ = stderr.flush();
    let mut answer = String::new();
    let read = std::io::stdin().lock().take(128).read_line(&mut answer);
    Ok(Some(
        read.is_ok() && matches!(answer.trim(), "y" | "Y" | "yes" | "YES"),
    ))
}

pub struct Route<'a> {
    pub identity: &'a str,
    /// When this pass gives up.
    pub deadline: Instant,
    pub local: bool,
    pub name: &'a str,
    pub route: &'a str,
    pub agent: bool,
}

pub fn run_connected(
    cli: &Cli,
    selected: &Route<'_>,
    confirmation: Option<&str>,
    journal: &mut dyn dispatch::Dispatch,
    reader: &mut dyn super::backend::Read,
) -> Output {
    let Route {
        identity,
        deadline,
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
    let env = Environment {
        identity,
        local,
        cwd: &cwd,
        current_identity: current_identity.as_deref(),
        current_server: current_server.as_deref(),
        current_pane: current_pane.as_deref(),
        agent,
        now: jiff::Timestamp::now().as_second(),
        confirmation,
        desktop: &crate::desktop::LocalDesktop { identity, deadline },
    };
    execute::run(
        &cli.command,
        &cli.global,
        &env,
        journal,
        reader,
        server_name,
        route,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        scope::fixtures::{fixture, id},
        state::{CliState, RouteKey, WriterGuard},
        work::backend,
    };
    use clap::Parser;
    use spaceterm_server::hierarchy::{
        CloseInventory, ConfirmedPane, Id, Inventory, Mutation, Outcome,
    };
    use std::{cell::RefCell, collections::VecDeque, path::PathBuf, rc::Rc};

    struct Reader(VecDeque<Inventory>);
    impl backend::Read for Reader {
        fn inventory(&mut self) -> Result<Inventory, Failure> {
            Ok(self.0.pop_front().expect("unexpected inventory read"))
        }
        fn home(&mut self) -> Result<PathBuf, Failure> {
            Ok("/server/home".into())
        }
        fn worktree(
            &mut self,
            _: spaceterm_server::worktree::Read,
        ) -> Result<spaceterm_server::worktree::ReadReply, Failure> {
            panic!("workspace kill reads no Worktrees")
        }
        fn panes(
            &mut self,
            _: &crate::scope::Snapshot,
            _: &crate::scope::Resolved,
        ) -> Result<Vec<crate::list::PaneFacts>, Failure> {
            panic!("workspace kill uses the inventory's close facts")
        }
    }

    struct Writer<'a> {
        state: &'a CliState,
        deadline: Instant,
        guard: Option<WriterGuard>,
        sent: Rc<RefCell<Vec<Mutation>>>,
    }
    impl dispatch::Dispatch for Writer<'_> {
        fn server_id(&self) -> Id {
            id(9)
        }
        fn client_id(&self) -> Id {
            id(5)
        }
        fn resolve_leftovers(&mut self) -> Result<Vec<RecoveredOperation>, Failure> {
            Ok(Vec::new())
        }
        fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure> {
            self.guard = Some(
                self.state
                    .writer(&RouteKey::Local, None, self.deadline)
                    .map_err(Failure::new)?,
            );
            assert!(self.guard.is_some());
            assert!(
                Instant::now() < self.deadline,
                "dispatch has a fresh deadline"
            );
            self.sent.borrow_mut().push(mutation);
            Ok(Outcome::Accepted { target: id(1) })
        }
    }

    #[test]
    fn confirmation_releases_the_writer_and_refreshes_deadline_and_facts() {
        for changed in [false, true] {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "spaceterm-confirm-{}-{}",
                std::process::id(),
                if changed { "changed" } else { "same" }
            ));
            let state = CliState::at(root.clone());
            let mut initial = fixture().inventory;
            initial.close = Some(CloseInventory {
                boot: id(9),
                sessions: initial
                    .workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .flat_map(|t| &t.panes)
                    .map(|p| ConfirmedPane {
                        id: p.id,
                        session: Some(p.id),
                    })
                    .collect(),
                controls: Vec::new(),
            });
            let mut refreshed = initial.clone();
            if changed {
                refreshed.close.as_mut().unwrap().sessions[0].session = Some(id(99));
            }
            let mut reader = Reader(VecDeque::from([initial, refreshed.clone(), refreshed]));
            let sent = Rc::new(RefCell::new(Vec::new()));
            let cli = Cli::try_parse_from(["spaceterm", "workspace", "kill", "--workspace", "api"])
                .unwrap();
            let timeout = Duration::from_secs(1);
            let output = run_with_confirmation(
                Instant::now() + timeout,
                timeout,
                |deadline, confirmation| {
                    execute::run(
                        &cli.command,
                        &cli.global,
                        &Environment {
                            identity: "test",
                            local: true,
                            cwd: std::path::Path::new("/client"),
                            current_identity: None,
                            current_server: None,
                            current_pane: None,
                            agent: false,
                            now: 100,
                            confirmation,
                            desktop: &crate::desktop::NoDesktop,
                        },
                        &mut Writer {
                            state: &state,
                            deadline,
                            guard: None,
                            sent: sent.clone(),
                        },
                        &mut reader,
                        "fixture",
                        "local",
                    )
                },
                |_| {
                    let guard = state
                        .try_writer(&RouteKey::Local, None)
                        .unwrap()
                        .expect("prompt holds no writer lock");
                    drop(guard);
                    std::thread::sleep(timeout + Duration::from_millis(5));
                    Ok(Some(true))
                },
            );
            if changed {
                assert_eq!(output.envelope.error.unwrap().code, Code::Changed);
                assert!(sent.borrow().is_empty());
            } else {
                assert_eq!(output.envelope.exit(), 0);
                assert_eq!(sent.borrow().len(), 1);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
