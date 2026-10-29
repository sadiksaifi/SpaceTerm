//! Native service evidence through the desktop's authenticated transport without credential storage.
#[expect(
    dead_code,
    reason = "Service evidence uses the shared hierarchy and never uses `support::attach`."
)]
mod support;
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::{Mutation, Outcome},
    pane::*,
    service::Service,
};
use std::{error::Error, time::Duration};
use support::Claimed;

fn run() -> Result<(), Box<dyn Error>> {
    let identity = std::env::args()
        .nth(1)
        .ok_or("application identity required")?;
    let delay = std::env::var("SPACETERM_SERVICE_CHECK_DELAY")
        .ok()
        .map(|v| v.parse::<u64>())
        .transpose()
        .map_err(|_| "invalid check delay")?
        .unwrap_or(3);
    if !(3..=60).contains(&delay) {
        return Err("invalid check delay".into());
    }
    let service = Service::for_identity(&identity)?;
    if let Some(installation) = std::env::args_os().nth(2) {
        let status = service.install(std::path::Path::new(&installation))?;
        if !status.running {
            return Err("service did not start".into());
        }
        println!(
            "Signed retained account service installed and started without administrator authorization."
        );
    }
    let client = ServerConnection::for_identity(&identity)?;
    client.setup()?;
    let first = support::connect(&client)?;
    let (_, next_sequence) = support::inventory(&first)?;
    let outcome = support::mutate(
        &first,
        next_sequence,
        Mutation::CreateWorkspace {
            name: "Service lifecycle check".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    )?;
    let Outcome::Accepted { target } = outcome.outcome else {
        return Err("creation refused".into());
    };
    first.acknowledge(next_sequence)?;
    let (inventory, _) = support::inventory(&first)?;
    let pane = inventory
        .workspaces
        .iter()
        .find(|v| v.id == target)
        .ok_or("workspace unavailable")?
        .tabs[0]
        .root_pane;
    let size = PaneSize {
        columns: 100,
        rows: 30,
        cell_width: 8,
        cell_height: 18,
    };
    let Claimed::Attached(selected) = support::select(&first, pane, true, size)? else {
        return Err("selection refused".into());
    };
    let lease = selected.lease();
    selected.command(&format!(
            "stty -echo; printf '\\033[2J\\033[HBEGIN\\n'; sleep {delay}; printf '\\nSERVICE-CONTINUED\\n'"
    ))?;
    selected.await_output("BEGIN")?;
    let automatic_start = service.status()?.enabled;
    for enabled in [false, true, automatic_start] {
        let status = service.set_automatic_start(enabled)?;
        if !status.installed || !status.running || status.stopped || status.enabled != enabled {
            return Err("automatic-start change altered the service lifecycle".into());
        }
        selected.await_output("BEGIN")?;
    }
    drop(selected);
    first.close();
    println!("Client disconnected; the server-owned command is running.");
    std::thread::sleep(Duration::from_secs(delay + 1));
    let second = support::connect(&client)?;
    let Claimed::Attached(resumed) = support::select(&second, pane, true, size)? else {
        return Err("reattachment refused".into());
    };
    if (lease.server, lease.session) != (resumed.lease().server, resumed.lease().session) {
        return Err("session was replaced".into());
    }
    resumed.await_output("SERVICE-CONTINUED")?;
    println!("Authenticated native reattachment preserved the service-owned running command.");
    #[cfg(target_os = "macos")]
    {
        resumed.command(
            "command -v git >/dev/null && command -v open >/dev/null && command -v pbcopy >/dev/null && command -v pbpaste >/dev/null && test -S \"$SSH_AUTH_SOCK\" && printf '\\nNATIVE-TOOLS-OK\\n'",
        )?;
        resumed.await_output("NATIVE-TOOLS-OK")?;
        println!("Service-launched shell resolved native tools and the login agent socket.");
    }
    service.set_automatic_start(false)?;
    service.stop()?;
    if !matches!(
        spaceterm_server::LocalStartup::for_identity(&identity)?
            .run(|| Err(ServerError::NotInstalled), || false)?,
        spaceterm_server::StartupOutcome::Stopped
    ) {
        return Err("normal launch reversed Stop".into());
    }
    if !service.status()?.stopped {
        return Err("Stop intent was lost".into());
    }
    let started = service.start()?;
    if started.enabled || started.stopped || !started.running {
        return Err("explicit Start changed the automatic-start preference".into());
    }
    let fresh = support::connect(&client)?;
    let Claimed::Restored(_) = support::select(&fresh, pane, false, size)? else {
        return Err("restart did not retain lazy organization".into());
    };
    service.set_automatic_start(automatic_start)?;
    service.stop()?;
    println!(
        "Stop survived client setup; explicit Start retained organization without restarting shells."
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Native service lifecycle check did not complete: {error}");
        std::process::exit(1);
    }
}
