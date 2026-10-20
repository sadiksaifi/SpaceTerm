//! Signed native update evidence. Run only against disposable Development account state.
#[expect(
    dead_code,
    reason = "Update evidence uses shared hierarchy instead of legacy Attach."
)]
mod support;

use spaceterm_server::{
    LocalServer, ServerError,
    client_view::{ClientView, WorkspaceView},
    hierarchy::{Mutation, Outcome},
    pane::*,
    service::{RetainedInstallation, Service},
};
use std::{
    error::Error,
    path::Path,
    time::{Duration, Instant},
};

const IDENTITY: &str = "spaceterm-development";
const SIZE: PaneSize = PaneSize {
    columns: 100,
    rows: 30,
    cell_width: 8,
    cell_height: 18,
};

fn connect(client: &LocalServer) -> Result<PaneConnection, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match client.connect_pane() {
            Ok(connection) => return Ok(connection),
            Err(ServerError::NotRunning | ServerError::NotSetUp) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn retain(service: &Service, args: &[String]) -> Result<RetainedInstallation, ServerError> {
    service.provision_candidate(
        Path::new(&args[0]),
        Path::new(&args[1]),
        &args[2],
        1,
        &["workspace_policy"],
    )
}

fn inventory(
    connection: &mut PaneConnection,
) -> Result<spaceterm_server::hierarchy::Inventory, Box<dyn Error>> {
    match connection.exchange(&Request::Inventory)? {
        Reply::Inventory { inventory, .. } => Ok(inventory),
        _ => Err("inventory unavailable".into()),
    }
}

fn create_workspace(
    connection: &mut PaneConnection,
    name: &str,
) -> Result<spaceterm_server::hierarchy::Id, Box<dyn Error>> {
    let Reply::Inventory { next_sequence, .. } = connection.exchange(&Request::Inventory)? else {
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

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 7 || args[6] != "--confirm-disposable-development" {
        return Err("provide two signed archive/signature/SHA256 triples and --confirm-disposable-development".into());
    }
    let service = Service::for_identity(IDENTITY)?;
    if service.status()?.installed {
        return Err("uninstall the disposable Development service before this check".into());
    }
    let previous = retain(&service, &args[..3])?;
    let installed = service.install(&previous.directory)?;
    if installed.stopped {
        service.start()?;
    }
    let first = LocalServer::for_development_client("update-smoke-a", 1)?;
    let second = LocalServer::for_development_client("update-smoke-b", 1)?;
    first.setup()?;
    second.setup()?;
    let mut a = connect(&first)?;
    let mut b = connect(&second)?;
    let target = create_workspace(&mut a, "Signed update check")?;
    let before = inventory(&mut a)?;
    let workspace = before
        .workspaces
        .iter()
        .find(|workspace| workspace.id == target)
        .ok_or("workspace missing")?;
    let pane = workspace.tabs[0].root_pane;
    first.attachments()?.initialize(&before, Some(target))?;
    second.attachments()?.initialize(&before, Some(target))?;
    let saved_view = ClientView {
        workspaces: vec![
            WorkspaceView {
                workspace: target,
                tabs: Vec::new(),
            }
            .reconcile(workspace),
        ],
    };
    first.client_view_store()?.save(saved_view.clone())?;
    let Reply::Attached { lease } = a.exchange(&Request::SelectPane {
        pane,
        explicit: true,
        size: SIZE,
        style: None,
    })?
    else {
        return Err("selection refused".into());
    };
    support::command(
        &mut a,
        lease,
        "stty -echo; printf '\\033[2J\\033[HUPDATE-BEGIN\\n'; sleep 3; printf '\\nSTAGING-CONTINUED\\n'; sleep 120",
    )?;
    support::await_output(&mut a, lease, "UPDATE-BEGIN")?;
    let archive = Path::new(&args[3]);
    let signature = Path::new(&args[4]);
    if !matches!(
        service.inspect_candidate(
            archive,
            signature,
            &"0".repeat(64),
            1,
            &["workspace_policy"]
        ),
        Err(ServerError::CandidateChanged)
    ) {
        return Err("a changed client pin was accepted".into());
    }
    if !matches!(
        service.inspect_candidate(
            archive,
            Path::new(&args[1]),
            &args[5],
            1,
            &["workspace_policy"]
        ),
        Err(ServerError::RuntimeUnavailable)
    ) {
        return Err("an inconsistent signed pair was accepted".into());
    }
    if !matches!(
        service.inspect_candidate(archive, signature, &args[5], 2, &["workspace_policy"]),
        Err(ServerError::IncompatibleProtocol)
    ) {
        return Err("an unsupported required major was accepted".into());
    }
    if !matches!(
        service.inspect_candidate(
            archive,
            signature,
            &args[5],
            1,
            &["unavailable_required_feature"]
        ),
        Err(ServerError::MissingRequiredCapability)
    ) {
        return Err("a missing required capability was accepted".into());
    }
    let candidate = retain(&service, &args[3..6])?;
    if previous.directory == candidate.directory {
        return Err("use different signed archive bytes".into());
    }
    if service.prepare_activation(&previous)?.is_some() {
        return Err("identical approved artifact offered an update".into());
    }
    let approval = service
        .prepare_activation(&candidate)?
        .ok_or("candidate was already approved")?;
    support::await_output(&mut a, lease, "STAGING-CONTINUED")?;
    if inventory(&mut b)?
        .workspaces
        .iter()
        .all(|workspace| workspace.id != target)
    {
        return Err("staging changed shared organization".into());
    }
    let created_while_staged = create_workspace(&mut b, "Created while staged")?;
    let newcomer = LocalServer::for_development_client("update-smoke-c", 1)?;
    newcomer.setup()?;
    let mut c = connect(&newcomer)?;
    if inventory(&mut c)?
        .workspaces
        .iter()
        .all(|workspace| workspace.id != created_while_staged)
    {
        return Err("new clients could not use the approved runtime while staged".into());
    }
    drop(c);
    drop(approval); // Cancellation preserves the running command and approved installation.
    if !service.status()?.running {
        return Err("staging stopped the approved runtime".into());
    }
    let approval = service
        .prepare_activation(&candidate)?
        .ok_or("candidate was selected before activation")?;
    println!(
        "Signed staging and cancelled confirmation preserved the running command and both clients."
    );
    service.activate(approval)?;
    service.recover_activation()?;
    if a.exchange(&Request::Poll { lease }).is_ok() {
        return Err("old connection survived runtime replacement".into());
    }
    drop(a);
    drop(b);
    let mut fresh = connect(&first)?;
    let after = inventory(&mut fresh)?;
    if after
        .workspaces
        .iter()
        .all(|workspace| workspace.id != created_while_staged)
    {
        return Err("Workspace created during staging was lost".into());
    }
    let restored = after
        .workspaces
        .iter()
        .find(|workspace| workspace.id == target)
        .ok_or("organization lost")?;
    if restored.tabs[0].root_pane != pane {
        return Err("Pane identity changed".into());
    }
    if !first.attachments()?.is_attached(target) || !second.attachments()?.is_attached(target) {
        return Err("client attachments changed".into());
    }
    if first.client_view_store()?.view() != &saved_view {
        return Err("personal arrangement changed".into());
    }
    if !matches!(
        fresh.exchange(&Request::SelectPane {
            pane,
            explicit: false,
            size: SIZE,
            style: None
        })?,
        Reply::Restored { .. }
    ) {
        return Err("replacement started a shell without explicit selection".into());
    }
    if service.prepare_activation(&candidate)?.is_some() {
        return Err("approved candidate offered another update".into());
    }
    println!(
        "Exact activation fenced the old connection and preserved organization, attachments and personal views; restored Pane stayed dormant."
    );
    // A fresh stopped-state approval cannot authorize a subsequently started run.
    service.stop()?;
    let stale = service
        .prepare_activation(&previous)?
        .ok_or("previous artifact unexpectedly approved")?;
    service.start()?;
    if !matches!(service.activate(stale), Err(ServerError::Changed)) {
        return Err("stopped approval accepted a new run".into());
    }
    service.stop()?;
    let stopped = service
        .prepare_activation(&previous)?
        .ok_or("stopped candidate unexpectedly approved")?;
    service.activate(stopped)?;
    service.recover_activation()?;
    if !service.status()?.stopped || service.status()?.running {
        return Err("stopped activation started the service".into());
    }
    println!(
        "Changed run observations required fresh approval; stopped activation preserved deliberate Stop."
    );
    service.uninstall()?;
    println!("Disposable signed native update check completed; service stopped and uninstalled.");
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Signed native server update check did not complete: {error}");
        std::process::exit(1);
    }
}
