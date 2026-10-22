//! Existing foreground Development server acceptance. Never starts or installs a server.
#[cfg(feature = "client")]
#[path = "support/lifecycle.rs"]
#[expect(dead_code, reason = "only the common Pane size is needed")]
mod lifecycle;
#[cfg(feature = "client")]
#[path = "support/presentation.rs"]
mod presentation;
#[cfg(feature = "client")]
#[path = "support/repository.rs"]
#[expect(dead_code, reason = "only shared inventory operations are needed")]
mod repository;
#[cfg(feature = "client")]
#[expect(
    dead_code,
    reason = "shared fixture modules also serve other native exercises"
)]
mod support;
#[cfg(feature = "client")]
#[path = "support/terminal_semantics.rs"]
#[expect(dead_code, reason = "only bounded failure diagnostics are needed")]
mod terminal_semantics;
#[cfg(feature = "client")]
#[path = "support/worktree.rs"]
#[expect(dead_code, reason = "only journaled hierarchy operations are needed")]
mod worktree_fixture;

#[cfg(feature = "client")]
mod exercise {
    use super::{
        lifecycle, presentation, repository, support, terminal_semantics::*, worktree_fixture,
    };
    use spaceterm_server::{
        LocalServer, ServerError,
        account_bridge::{AccountBridge, AccountBridgeIdentity, AccountConnector},
        hierarchy::{CloseTarget, Id, Mutation, Outcome},
        pane::{Lease, PaneConnection, Reply, Request},
    };
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        sync::Arc,
    };

    const IDENTITY: &str = "spaceterm-development";
    type Result<T> = std::result::Result<T, Failure>;

    struct Options {
        server: Id,
        boot: Option<Id>,
        ssh: Option<(String, String, u32)>,
    }
    fn id(text: &str) -> Result<Id> {
        if text.len() != 64 || !text.is_ascii() {
            return Err("identity must contain 64 hexadecimal digits".into());
        }
        let mut id = [0; 32];
        for (index, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(|_| Failure::from("identity must contain 64 hexadecimal digits"))?;
        }
        if id == [0; 32] {
            return Err("zero identity refused".into());
        }
        Ok(id)
    }
    impl Options {
        fn parse(args: &[String]) -> Result<Option<Self>> {
            let mut run = false;
            let (mut server, mut boot, mut ssh, mut binary, mut account) =
                (None, None, None, None, None);
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                let mut next = || {
                    args.next()
                        .map(String::as_str)
                        .ok_or_else(|| Failure::from("argument value missing"))
                };
                match arg.as_str() {
                    "--run" => run = true,
                    "--help" => return Ok(None),
                    "--expect-server" => server = Some(id(next()?)?),
                    "--expect-boot" => boot = Some(id(next()?)?),
                    "--ssh" => ssh = Some(next()?.to_owned()),
                    "--remote-server" => binary = Some(next()?.to_owned()),
                    "--account" => {
                        account = Some(
                            next()?
                                .parse::<u32>()
                                .map_err(|_| Failure::from("invalid account"))?,
                        )
                    }
                    _ => return Err("unknown argument".into()),
                }
            }
            if !run {
                return Ok(None);
            }
            let ssh = match (ssh, binary, account) {
                (None, None, None) => None,
                (Some(destination), Some(binary), Some(account))
                    if !destination.is_empty()
                        && !destination.starts_with('-')
                        && destination.len() <= 256
                        && destination.chars().all(|c| c.is_ascii_alphanumeric() || "_.:@%[]-".contains(c))
                        && binary.starts_with('/')
                        && binary.len() <= 4096
                        && !binary.chars().any(char::is_control)
                        && account != 0 => Some((destination, binary, account)),
                _ => return Err("SSH requires a destination, absolute remote server binary and non-root account".into()),
            };
            Ok(Some(Self {
                server: server.ok_or("--expect-server is required")?,
                boot,
                ssh,
            }))
        }
    }

    struct Ssh {
        destination: String,
        binary: String,
        expected: AccountBridgeIdentity,
    }
    impl AccountConnector for Ssh {
        fn connect(&self) -> std::result::Result<AccountBridge, ServerError> {
            let remote = format!(
                "exec '{}' ssh-connect {IDENTITY}",
                self.binary.replace('\'', "'\\''")
            );
            let mut child = Command::new("ssh")
                .args([
                    "-T",
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "StrictHostKeyChecking=yes",
                    "-o",
                    "UpdateHostKeys=no",
                    "-o",
                    "AddKeysToAgent=no",
                    "-o",
                    "ConnectTimeout=10",
                    "-o",
                    "ServerAliveInterval=5",
                    "-o",
                    "ServerAliveCountMax=2",
                    "-o",
                    "ControlMaster=no",
                    "-o",
                    "ControlPath=none",
                    "-o",
                    "LogLevel=ERROR",
                ])
                .arg(&self.destination)
                .arg(remote)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|_| ServerError::NotRunning)?;
            let read = child.stdout.take().ok_or(ServerError::Protocol)?;
            let write = child.stdin.take().ok_or(ServerError::Protocol)?;
            let cleanup = Box::new(move || {
                // SAFETY: this child is the leader of the process group created above.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
            });
            AccountBridge::from_streams(Box::new(read), Box::new(write), cleanup, &self.expected)
        }
    }

    struct Scope<'a> {
        server: &'a LocalServer,
        boot: Id,
        workspace: Option<(Id, Option<Id>)>,
    }
    impl Scope<'_> {
        fn cleanup(&mut self) -> Result<()> {
            let Some((workspace, pane)) = self.workspace else {
                return Ok(());
            };
            let mut connection = stage("cleanup-connect", self.server.connect_pane())?;
            let inventory = stage("cleanup-inventory", repository::inventory(&mut connection))?.0;
            if inventory.close.as_ref().map(|close| close.boot) != Some(self.boot) {
                return Err("cleanup refused replacement server boot".into());
            }
            if let Some(owned) = inventory
                .workspaces
                .iter()
                .find(|work| work.id == workspace)
            {
                if owned.tabs.len() != 1
                    || owned.tabs[0].panes.len() != 1
                    || pane.is_some_and(|pane| owned.tabs[0].root_pane != pane)
                {
                    return Err("cleanup preserved changed test Workspace membership".into());
                }
                let target = CloseTarget::Workspace { workspace };
                let confirmation = inventory
                    .close_confirmation(target)
                    .ok_or("cleanup confirmation missing")?;
                accepted(
                    self.server,
                    &mut connection,
                    Mutation::Close {
                        target,
                        confirmation,
                    },
                )?;
            }
            self.workspace = None;
            Ok(())
        }
    }
    impl Drop for Scope<'_> {
        fn drop(&mut self) {
            if let Err(failure) = self.cleanup() {
                eprintln!("presentation cleanup: {failure}");
            }
        }
    }
    fn accepted(
        server: &LocalServer,
        connection: &mut PaneConnection,
        mutation: Mutation,
    ) -> Result<Id> {
        let outcome = stage(
            "presentation-mutation",
            worktree_fixture::journaled_mutation(server, connection, mutation),
        )?;
        match outcome.outcome {
            Outcome::Accepted { target } => Ok(target),
            _ => Err("test Workspace mutation refused".into()),
        }
    }
    fn select(connection: &mut PaneConnection, pane: Id) -> Result<Lease> {
        match stage(
            "presentation-select",
            connection.exchange(&Request::SelectPane {
                pane,
                explicit: true,
                size: lifecycle::size(),
                style: None,
            }),
        )? {
            Reply::Attached { lease } => Ok(lease),
            _ => Err("test Pane did not attach".into()),
        }
    }
    pub fn run() -> Result<()> {
        let Some(options) = Options::parse(&std::env::args().skip(1).collect::<Vec<_>>())? else {
            println!("Dry run: no storage, server or SSH access performed.");
            println!("Use --run --expect-server HEX [--expect-boot HEX] for IPC.");
            println!(
                "Add --ssh DEST --remote-server /absolute/candidate/spaceterm-server --account UID for the SSH account bridge."
            );
            println!(
                "Requires an existing foreground Development server and python3 on its host. Creates and closes one test Workspace; never starts, installs or resets a server."
            );
            return Ok(());
        };
        let nonce = stage(
            "client-name",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
        )?
        .as_nanos();
        let name = format!("p612-{nonce:x}");
        if options.ssh.is_none()
            && stage(
                "expected-local-server",
                spaceterm_server::account_bridge::discover(IDENTITY),
            )?
            .server
                != Some(options.server)
        {
            return Err("expected server identity changed".into());
        }
        let (server, observer) = if let Some((destination, binary, account)) = options.ssh {
            let expected = AccountBridgeIdentity {
                account,
                build: IDENTITY.into(),
                server: options.server,
            };
            let connector = Arc::new(Ssh {
                destination,
                binary,
                expected: expected.clone(),
            });
            // SAFETY: both handles are constructed before this fixture starts threads or SSH.
            unsafe {
                std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", &name);
            }
            let server = stage(
                "ssh-client",
                LocalServer::remote(
                    IDENTITY,
                    options.server,
                    expected.clone(),
                    connector.clone(),
                ),
            )?;
            // SAFETY: construction above does not start workers; each handle captures its name.
            unsafe {
                std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", format!("{name}-observer"));
            }
            let observer = stage(
                "ssh-observer",
                LocalServer::remote(IDENTITY, options.server, expected, connector),
            )?;
            (server, observer)
        } else {
            (
                stage("ipc-client", LocalServer::for_development_client(&name, 1))?,
                stage(
                    "ipc-observer",
                    LocalServer::for_development_client(&format!("{name}-observer"), 1),
                )?,
            )
        };
        // Client credentials are created by admission, without service registration or setup/reset.
        let mut connection = stage("presentation-connect", server.connect_pane())?;
        let mut observer = stage("observer-connect", observer.connect_pane())?;
        let inventory = stage(
            "presentation-inventory",
            repository::inventory(&mut connection),
        )?
        .0;
        let boot = inventory.close.as_ref().ok_or("server boot missing")?.boot;
        if options.boot.is_some_and(|expected| expected != boot) {
            return Err("expected server boot changed".into());
        }
        let workspace = accepted(
            &server,
            &mut connection,
            Mutation::CreateWorkspace {
                name: "Presentation acceptance 612".into(),
                directory: None,
            },
        )?;
        let mut scope = Scope {
            server: &server,
            boot,
            workspace: Some((workspace, None)),
        };
        let current = stage("fixture-inventory", repository::inventory(&mut connection))?.0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("test Workspace missing")?
            .root_pane;
        scope.workspace = Some((workspace, Some(pane)));
        let lease = select(&mut connection, pane)?;
        let witness = presentation::begin(&mut connection, lease, support::command)?;
        presentation::observe_hidden_inventory(&mut observer, pane)?;
        drop(connection);
        let mut connection = stage("presentation-reconnect", server.connect_pane())?;
        let resumed = select(&mut connection, pane)?;
        presentation::observe_hidden_inventory(&mut observer, pane)?;
        presentation::finish(&mut connection, resumed, lease, witness)?;
        scope.cleanup()?;
        println!(
            "PASS existing-server presentation acceptance and independent inventory observer; test Workspace removed"
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn parse(args: &[&str]) -> Result<Option<Options>> {
            Options::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
        }
        #[test]
        fn mutation_requires_explicit_run_and_exact_server_identity() {
            assert!(parse(&[]).unwrap().is_none());
            assert!(parse(&["--run"]).is_err());
            assert!(parse(&["--run", "--expect-server", &"00".repeat(32)]).is_err());
            assert!(
                parse(&["--run", "--expect-server", &"12".repeat(32)])
                    .unwrap()
                    .is_some()
            );
        }
        #[test]
        fn ssh_scope_refuses_partial_routes_and_option_injection() {
            let server = "12".repeat(32);
            for route in [
                vec!["--ssh", "penguin"],
                vec![
                    "--ssh",
                    "-ProxyCommand=bad",
                    "--remote-server",
                    "/candidate/server",
                    "--account",
                    "1000",
                ],
                vec![
                    "--ssh",
                    "penguin",
                    "--remote-server",
                    "relative/server",
                    "--account",
                    "1000",
                ],
            ] {
                let mut args = vec!["--run", "--expect-server", &server];
                args.extend(route);
                assert!(parse(&args).is_err());
            }
        }
    }
}

#[cfg(feature = "client")]
fn main() -> std::process::ExitCode {
    match exercise::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("terminal_presentation: {failure}");
            std::process::ExitCode::FAILURE
        }
    }
}
#[cfg(not(feature = "client"))]
fn main() {
    eprintln!("terminal_presentation requires the client feature");
    std::process::exit(2);
}
