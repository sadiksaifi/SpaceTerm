//! Native evidence for remote maintenance originals, through the same bounded request and reply
//! the SSH account route carries. Run only against disposable Development account state.

#[cfg(feature = "client")]
#[expect(
    dead_code,
    reason = "Maintenance evidence uses shared hierarchy instead of legacy Attach."
)]
mod support;

#[cfg(feature = "client")]
mod exercise {
    use super::support;
    use spaceterm_server::{
        LocalServer, ServerError,
        hierarchy::{Id, Inventory, Mutation, Outcome},
        pane::*,
        service::{
            RetainedInstallation, Service,
            maintenance::{
                self, Action, MaintenanceFacts, MaintenanceRefusal, OperationOutcome,
                OperationResolution, PreparedChange, RemoteBuild,
            },
        },
    };
    use std::{
        error::Error,
        ffi::OsString,
        path::Path,
        time::{Duration, Instant},
    };

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    const IDENTITY: &str = "spaceterm-development";
    const SIZE: PaneSize = PaneSize {
        columns: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 18,
    };

    /// Carry one request over the same bounded bytes the SSH route carries.
    fn exchange(request: &maintenance::Request) -> Result<maintenance::Reply> {
        let arguments = ["ssh-maintain", IDENTITY].map(OsString::from);
        let bytes = serde_json::to_vec(request)?;
        let mut output = Vec::new();
        maintenance::run(IDENTITY, &arguments, &mut bytes.as_slice(), &mut output)?;
        let text = String::from_utf8(output)?;
        if text.contains('/') {
            return Err("a reply named a path".into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    fn facts(reply: maintenance::Reply) -> Result<MaintenanceFacts> {
        match reply {
            maintenance::Reply::Facts(facts) => Ok(facts),
            _ => Err("observation unavailable".into()),
        }
    }

    fn prepared(reply: maintenance::Reply) -> Result<PreparedChange> {
        match reply {
            maintenance::Reply::Prepared(prepared) => Ok(prepared),
            _ => Err("the change was not described".into()),
        }
    }

    fn outcome(reply: maintenance::Reply) -> Result<OperationOutcome> {
        match reply {
            maintenance::Reply::Outcome(outcome) => Ok(outcome),
            _ => Err("the original has no outcome".into()),
        }
    }

    /// One submitted original's identity. A real client binds a fresh random identity instead.
    fn operation(tag: u8) -> [u8; 32] {
        [tag; 32]
    }

    fn digest(installation: &RetainedInstallation) -> Result<String> {
        Ok(RemoteBuild::describe(installation)?.digest)
    }

    fn retain(service: &Service, args: &[String]) -> Result<RetainedInstallation> {
        Ok(service.provision_candidate(
            Path::new(&args[0]),
            Path::new(&args[1]),
            &args[2],
            1,
            &["workspace_policy"],
        )?)
    }

    fn connect(client: &LocalServer) -> Result<PaneConnection> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match client.connect_pane() {
                Ok(connection) => return Ok(connection),
                Err(ServerError::NotRunning | ServerError::NotSetUp)
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn inventory(connection: &mut PaneConnection) -> Result<Inventory> {
        match connection.exchange(&Request::Inventory)? {
            Reply::Inventory { inventory, .. } => Ok(inventory),
            _ => Err("inventory unavailable".into()),
        }
    }

    fn create_workspace(connection: &mut PaneConnection, name: &str) -> Result<Id> {
        let Reply::Inventory { next_sequence, .. } = connection.exchange(&Request::Inventory)?
        else {
            return Err("inventory unavailable".into());
        };
        let Reply::Outcome(outcome) = connection.exchange(&Request::Mutate {
            sequence: next_sequence,
            mutation: Mutation::CreateWorkspace {
                name: name.into(),
                directory: None,
            },
        })?
        else {
            return Err("creation unavailable".into());
        };
        let Outcome::Accepted { target } = outcome.outcome else {
            return Err("creation refused".into());
        };
        connection.exchange(&Request::Acknowledge {
            sequence: next_sequence,
        })?;
        Ok(target)
    }

    pub fn run() -> Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 7 || args[6] != "--confirm-disposable-development" {
            return Err("provide two signed archive/signature/SHA256 triples and \
                        --confirm-disposable-development"
                .into());
        }
        let service = Service::for_identity(IDENTITY)?;
        if service.status()?.installed {
            return Err("uninstall the disposable Development service before this check".into());
        }
        let maintenance = maintenance::Maintenance::for_identity(IDENTITY, IDENTITY)
            .map_err(|refusal| format!("maintenance unavailable: {refusal:?}"))?;
        if maintenance
            .unresolved()
            .map_err(|refusal| format!("retained originals unreadable: {refusal:?}"))?
        {
            return Err("resolve this account's pending original before this check".into());
        }
        let previous = retain(&service, &args[..3])?;
        if service.install(&previous.directory)?.stopped {
            service.start()?;
        }
        let client = LocalServer::for_development_client("maintenance-smoke", 1)?;
        client.setup()?;
        let mut pane = connect(&client)?;
        let target = create_workspace(&mut pane, "Remote maintenance check")?;
        let before = inventory(&mut pane)?;
        let workspace = before
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target)
            .ok_or("workspace missing")?;
        let root = workspace.tabs[0].root_pane;
        let Reply::Attached { lease } = pane.exchange(&Request::SelectPane {
            pane: root,
            explicit: true,
            size: SIZE,
            style: None,
        })?
        else {
            return Err("selection refused".into());
        };
        support::command(
            &mut pane,
            lease,
            "stty -echo; printf '\\033[2J\\033[HMAINTENANCE-BEGIN\\n'; sleep 3; printf \
             '\\nSTAGING-CONTINUED\\n'; sleep 120",
        )?;
        support::await_output(&mut pane, lease, "MAINTENANCE-BEGIN")?;

        let observed = facts(exchange(&maintenance::Request::Observe)?)?;
        if observed.approved != Some(RemoteBuild::describe(&previous)?) || !observed.status.running
        {
            return Err("observation did not name the running approved build".into());
        }
        let candidate = retain(&service, &args[3..6])?;
        if previous.directory == candidate.directory {
            return Err("use different signed archive bytes".into());
        }
        if facts(exchange(&maintenance::Request::Observe)?)? != observed {
            return Err("staging changed the approved runtime or service policy".into());
        }
        support::await_output(&mut pane, lease, "STAGING-CONTINUED")?;
        println!(
            "Staging retained a verified candidate while the approved runtime, service policy and \
             the running Session continued."
        );

        let stale = prepared(exchange(&maintenance::Request::Prepare {
            action: Action::Stop,
        })?)?;
        if !stale.running {
            return Err("the running server was described as idle".into());
        }
        // The server runs again, so the observation that confirmation named is gone.
        service.restart()?;
        drop(pane);
        let refused = outcome(exchange(&maintenance::Request::Apply {
            operation: operation(1),
            action: Action::Stop,
            confirmation: stale.confirmation,
        })?)?;
        if refused.resolution != OperationResolution::Refused(MaintenanceRefusal::Changed) {
            return Err("a confirmation bound to an earlier run was accepted".into());
        }
        if !service.status()?.running {
            return Err("a refused original stopped the server".into());
        }

        let confirmation = prepared(exchange(&maintenance::Request::Prepare {
            action: Action::Stop,
        })?)?
        .confirmation;
        let stopped = outcome(exchange(&maintenance::Request::Apply {
            operation: operation(2),
            action: Action::Stop,
            confirmation,
        })?)?;
        if stopped.resolution != OperationResolution::Completed {
            return Err("the confirmed Stop did not complete".into());
        }
        let resubmitted = outcome(exchange(&maintenance::Request::Apply {
            operation: operation(2),
            action: Action::Uninstall,
            confirmation: "a".repeat(64),
        })?)?;
        if resubmitted != stopped {
            return Err("a resubmitted original did not keep its outcome".into());
        }
        if outcome(exchange(&maintenance::Request::Resolve {
            operation: operation(2),
        })?)?
            != stopped
        {
            return Err("resolution changed a validated outcome".into());
        }
        if exchange(&maintenance::Request::Resolve {
            operation: operation(9),
        })? != maintenance::Reply::Unknown
        {
            return Err("an unsubmitted original was reported".into());
        }
        let status = service.status()?;
        if !status.stopped || status.running || status.enabled {
            return Err("the confirmed Stop did not suspend automatic startup".into());
        }
        println!(
            "One confirmed original stopped the server once, and its resubmission and resolution \
             kept that outcome without dispatching again."
        );

        let update = prepared(exchange(&maintenance::Request::PrepareUpdate {
            candidate: digest(&candidate)?,
        })?)?;
        if !update.stopped || update.running {
            return Err("the stopped server was described as running".into());
        }
        let updated = outcome(exchange(&maintenance::Request::ApplyUpdate {
            operation: operation(3),
            candidate: digest(&candidate)?,
            confirmation: update.confirmation,
        })?)?;
        if updated.resolution != OperationResolution::Completed {
            return Err("the confirmed update did not complete".into());
        }
        let status = service.status()?;
        if !status.stopped || status.running {
            return Err("updating a stopped server started it".into());
        }
        let after = facts(exchange(&maintenance::Request::Observe)?)?;
        if after.approved != Some(RemoteBuild::describe(&candidate)?) {
            return Err("the exact candidate did not become the approved build".into());
        }
        if after
            .activation
            .is_none_or(|activation| activation.operation != operation(3))
        {
            return Err("the retained activation did not name its submitted original".into());
        }
        if outcome(exchange(&maintenance::Request::Resolve {
            operation: operation(3),
        })?)?
            != updated
        {
            return Err("resolution changed the update's validated outcome".into());
        }
        if exchange(&maintenance::Request::PrepareUpdate {
            candidate: digest(&candidate)?,
        })? != maintenance::Reply::AlreadyApproved
        {
            return Err("the approved artifact offered another update".into());
        }
        println!(
            "Updating the stopped server left it stopped on the exact candidate, and resolution \
             selected no other artifact."
        );

        let started = outcome(exchange(&maintenance::Request::Start {
            operation: operation(4),
        })?)?;
        if started.resolution != OperationResolution::Completed {
            return Err("explicit Start did not complete".into());
        }
        let mut resumed = connect(&client)?;
        if inventory(&mut resumed)?
            .workspaces
            .iter()
            .all(|workspace| workspace.id != target)
        {
            return Err("the updated server lost its Workspace organization".into());
        }
        if !matches!(
            resumed.exchange(&Request::SelectPane {
                pane: root,
                explicit: false,
                size: SIZE,
                style: None,
            })?,
            Reply::Restored { .. }
        ) {
            return Err("a restored Pane started a shell without explicit selection".into());
        }
        if maintenance
            .unresolved()
            .map_err(|refusal| format!("retained originals unreadable: {refusal:?}"))?
        {
            return Err("a validated original stayed unresolved".into());
        }
        drop(resumed);
        service.stop()?;
        service.uninstall()?;
        println!(
            "Explicit Start reversed the deliberate Stop and kept the organization; disposable \
             remote maintenance check completed, service stopped and uninstalled."
        );
        Ok(())
    }
}

#[cfg(feature = "client")]
fn main() {
    if let Err(error) = exercise::run() {
        eprintln!("Remote maintenance check did not complete: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "client"))]
fn main() {
    eprintln!("server_maintenance requires --features client");
    std::process::exit(2);
}
