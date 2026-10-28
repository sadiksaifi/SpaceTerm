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
        ServerConnection,
        connection::link::ServerLink,
        hierarchy::{CloseTarget, Id, Mutation, Outcome},
    };
    const IDENTITY: &str = "spaceterm-development";
    type Result<T> = std::result::Result<T, Failure>;

    struct Options {
        server: Id,
        boot: Option<Id>,
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
            let (mut server, mut boot) = (None, None);
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
                    _ => return Err("unknown argument".into()),
                }
            }
            if !run {
                return Ok(None);
            }
            Ok(Some(Self {
                server: server.ok_or("--expect-server is required")?,
                boot,
            }))
        }
    }

    struct Scope<'a> {
        server: &'a ServerConnection,
        boot: Id,
        workspace: Option<(Id, Option<Id>)>,
    }
    impl Scope<'_> {
        fn cleanup(&mut self) -> Result<()> {
            let Some((workspace, pane)) = self.workspace else {
                return Ok(());
            };
            let connection = stage("cleanup-connect", support::connect(self.server))?;
            let inventory = stage("cleanup-inventory", repository::inventory(&connection))?.0;
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
                    &connection,
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
        server: &ServerConnection,
        connection: &ServerLink,
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
    fn select(connection: &ServerLink, pane: Id) -> Result<support::Pane> {
        stage(
            "presentation-select",
            support::select(connection, pane, true, lifecycle::size()),
        )?
        .attached()
        .ok_or_else(|| "test Pane did not attach".into())
    }
    pub fn run() -> Result<()> {
        let Some(options) = Options::parse(&std::env::args().skip(1).collect::<Vec<_>>())? else {
            println!("Dry run: no storage or server access performed.");
            println!("Use --run --expect-server HEX [--expect-boot HEX] for IPC.");
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
        if stage(
            "expected-local-server",
            spaceterm_server::provisioning::discover(IDENTITY),
        )?
        .server
            != Some(options.server)
        {
            return Err("expected server identity changed".into());
        }
        let server = stage(
            "ipc-client",
            ServerConnection::for_development_client(&name, 3),
        )?;
        let observer = stage(
            "ipc-observer",
            ServerConnection::for_development_client(&format!("{name}-observer"), 3),
        )?;
        // Client credentials are created by admission, without service registration or setup/reset.
        let connection = stage("presentation-connect", support::connect(&server))?;
        let observer = stage("observer-connect", support::connect(&observer))?;
        let inventory = stage("presentation-inventory", repository::inventory(&connection))?.0;
        let boot = inventory.close.as_ref().ok_or("server boot missing")?.boot;
        if options.boot.is_some_and(|expected| expected != boot) {
            return Err("expected server boot changed".into());
        }
        let workspace = accepted(
            &server,
            &connection,
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
        let current = stage("fixture-inventory", repository::inventory(&connection))?.0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("test Workspace missing")?
            .root_pane;
        scope.workspace = Some((workspace, Some(pane)));
        let started = select(&connection, pane)?;
        let witness = presentation::begin(&started)?;
        presentation::observe_hidden_inventory(&observer, pane)?;
        let lease = started.lease();
        connection.close();
        drop(started);
        let connection = stage("presentation-reconnect", support::connect(&server))?;
        let resumed = select(&connection, pane)?;
        presentation::observe_hidden_inventory(&observer, pane)?;
        presentation::finish(&resumed, lease, witness)?;
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
