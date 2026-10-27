//! Reconnect and semantic terminal proof confined to one test-created Workspace over private IPC.
#[path = "support/repository.rs"]
#[expect(
    dead_code,
    reason = "this exercise uses only shared hierarchy fixture operations"
)]
mod repository;
#[expect(
    dead_code,
    reason = "this exercise selects shared Panes and never uses `support::attach`"
)]
mod support;
#[path = "support/terminal_semantics.rs"]
mod terminal_semantics;
#[path = "support/worktree.rs"]
#[expect(
    dead_code,
    reason = "this exercise uses only journaled hierarchy mutations"
)]
mod worktree_fixture;
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::{CloseTarget, Mutation, Outcome},
    pane::*,
    protocol::{Channel, Operation},
};
use std::{error::Error, time::Duration};
use support::Claimed;
use terminal_semantics::{Failure, stage};

fn main() -> Result<(), Box<dyn Error>> {
    if !spaceterm_server::service::Service::for_identity("spaceterm-development")?
        .status()?
        .installed
    {
        return Err(ServerError::NotInstalled.into());
    }
    let server = ServerConnection::for_development_client("persistent-pane-native", 2)?;
    let first = repository::connect(&server)?;
    let baseline = repository::inventory(&first)?.0;
    let shutdown = std::env::args().any(|argument| argument == "--shutdown");
    if shutdown
        && baseline
            .close
            .as_ref()
            .is_none_or(|close| close.sessions.iter().any(|pane| pane.session.is_some()))
    {
        return Err("shutdown fixture refuses existing terminal work".into());
    }
    let created = worktree_fixture::journaled_mutation(
        &server,
        &first,
        Mutation::CreateWorkspace {
            name: "IPC native semantics".into(),
            directory: None,
        },
    )
    .map_err(|_| "fixture Workspace creation unavailable; preserve any original")?;
    let Outcome::Accepted { target: workspace } = created.outcome else {
        return Err("fixture Workspace creation refused".into());
    };
    let exercise = (|| -> Result<_, Failure> {
        let current = stage("ipc-fixture-inventory", repository::inventory(&first))?.0;
        let pane = current
            .workspaces
            .iter()
            .find(|work| work.id == workspace)
            .and_then(|work| work.tabs.first())
            .ok_or("fixture Pane missing")?
            .root_pane;
        let size = PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        };
        let Claimed::Attached(started) = stage(
            "ipc-select-fixture-pane",
            support::select(&first, pane, true, size),
        )?
        else {
            return Err("fixture Pane did not start".into());
        };
        let original = started.lease();
        stage(
            "ipc-reconnect-command",
            started.command(
                "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 2; printf 'CONTINUED\\n'; printf 'PID=%s\\n' \"$$\"",
            ),
        )?;
        stage("ipc-started-witness", started.await_output("STARTED"))?;
        first.close();
        drop(started);
        std::thread::sleep(Duration::from_secs(3));
        let second = stage("ipc-reconnect-transport", repository::connect(&server))?;
        let Claimed::Attached(resumed) = stage(
            "ipc-resume-fixture-pane",
            support::select(&second, pane, true, size),
        )?
        else {
            return Err("fixture Pane did not resume".into());
        };
        let lease = resumed.lease();
        if (original.server, original.session, original.control)
            != (lease.server, lease.session, lease.control)
        {
            return Err("reconnect changed the running fixture Session".into());
        }
        let output = stage("ipc-continued-witness", resumed.await_output("CONTINUED"))?;
        let pid: i32 = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("PID="))
            .ok_or("fixture shell identity missing")?
            .parse()
            .map_err(|_| "fixture shell identity invalid")?;
        if pid <= 1 {
            return Err("fixture shell identity invalid".into());
        }
        // Requests name a channel, not a lease, and a channel exists only on its own connection.
        // The original channel ended with its connection, so a channel the new connection never
        // bound stands in for it. Channels start at 1, so `u32::MAX` was never bound.
        if !matches!(
            support::request(
                &second,
                Operation::ClearScrollback {
                    channel: Channel(u32::MAX),
                },
            ),
            Err(ServerError::StaleControl)
        ) {
            return Err("stale fixture channel was accepted".into());
        }
        let report = terminal_semantics::exercise(&resumed)?;
        Ok((second, pid, report))
    })();
    if let Err(failure) = &exercise {
        eprintln!("IPC fixture exercise failed: {failure}");
    }
    // Never erase an unresolved original or close its Workspace while a write may still run.
    if server.shared_journal()?.pending().is_some() {
        return Err("fixture cleanup waits for original-operation recovery".into());
    }
    let cleanup = repository::connect(&server)?;
    let current = repository::inventory(&cleanup)?.0;
    let target = CloseTarget::Workspace { workspace };
    let confirmation = current
        .close_confirmation(target)
        .ok_or("fixture close target missing")?;
    let closed = worktree_fixture::journaled_mutation(
        &server,
        &cleanup,
        Mutation::Close {
            target,
            confirmation,
        },
    )
    .map_err(|_| "fixture close unavailable; preserve any original")?;
    if !matches!(closed.outcome, Outcome::Accepted { .. }) {
        return Err("fixture close refused".into());
    }
    let after = repository::inventory(&cleanup)?.0;
    if !baseline
        .workspaces
        .iter()
        .all(|before| after.workspaces.iter().any(|work| work.id == before.id))
        || !baseline.close.as_ref().is_some_and(|before| {
            after.close.as_ref().is_some_and(|after| {
                before
                    .sessions
                    .iter()
                    .all(|pane| after.sessions.contains(pane))
            })
        })
    {
        return Err("baseline terminal work changed during the fixture".into());
    }
    let (second, pid, report) = exercise?;
    // SAFETY: signal zero observes only the fixture shell; it never signals a process.
    if unsafe { libc::kill(pid, 0) } != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        return Err("fixture close did not reap its shell".into());
    }
    if shutdown {
        support::done(&second, Operation::Shutdown)
            .map_err(|_| "explicit fixture shutdown failed")?;
    }
    println!(
        "PASS IPC reconnect, stale channel refusal, Copy rows={}, accessible rows={}, graphics supported={}, graphics bytes={}, fixture cleanup; baseline work retained",
        report.copied_rows,
        report.accessible_rows,
        report.graphics_supported,
        report.graphics_bytes
    );
    Ok(())
}
