//! Authenticated real-host directory recovery. Uses account-local authority and an idle Development server.
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "this exercise claims named Panes and never uses `support::attach`"
)]
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm_server::{
    ServerConnection, ServerError,
    connection::link::ServerLink,
    hierarchy::{Mutation, Outcome},
    pane::*,
    protocol::Operation,
};
use std::{error::Error, path::PathBuf, time::Duration};
use support::{Claimed, Pane};

fn inventory(
    link: &ServerLink,
) -> Result<(spaceterm_server::hierarchy::Inventory, u64), Box<dyn Error>> {
    Ok(support::inventory(link)?)
}
fn mutate(
    link: &ServerLink,
    sequence: u64,
    mutation: Mutation,
) -> Result<[u8; 32], Box<dyn Error>> {
    let outcome = support::mutate(link, sequence, mutation)?;
    let Outcome::Accepted { target } = outcome.outcome else {
        return Err("mutation refused".into());
    };
    link.acknowledge(sequence)?;
    Ok(target)
}
fn select(link: &ServerLink, pane: [u8; 32], explicit: bool) -> Result<Claimed, ServerError> {
    support::select(link, pane, explicit, size())
}
fn attached(claimed: Claimed, failure: &'static str) -> Result<Pane, Box<dyn Error>> {
    claimed.attached().ok_or_else(|| failure.into())
}
fn target(link: &ServerLink, pane: [u8; 32]) -> Result<directories::Target, Box<dyn Error>> {
    match select(link, pane, false)? {
        Claimed::Restored(restored) => restored
            .recovery
            .ok_or_else(|| "retained recovery target missing".into()),
        _ => Err("retained recovery target missing".into()),
    }
}
fn repair(
    link: &ServerLink,
    target: directories::Target,
    replacement: Option<PathBuf>,
) -> Result<Claimed, ServerError> {
    let pane = target.pane;
    support::claim(
        link,
        pane,
        AttachmentClaim::Repair {
            target,
            replacement,
        },
        size(),
    )
}
fn cleanup_exercise(connection: &ServerLink) -> Result<(), Box<dyn Error>> {
    let (inventory, sequence) = inventory(connection)?;
    if inventory.workspaces.len() != 1 {
        return Err("cleanup requires exactly the exercise Workspace".into());
    }
    let workspace = &inventory.workspaces[0];
    let root = workspace
        .pinned_directory
        .as_deref()
        .and_then(|path| path.parent())
        .ok_or("exercise directory missing")?;
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    if workspace.name != "Directory recovery"
        || root.parent() != Some(temporary.as_path())
        || !root.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with("spaceterm-directory-recovery-")
        })
    {
        return Err("unrelated Workspace was preserved".into());
    }
    let target = spaceterm_server::hierarchy::CloseTarget::KillWorkspace {
        workspace: workspace.id,
    };
    let confirmation = inventory
        .close_confirmation(target)
        .ok_or("close confirmation")?;
    mutate(
        connection,
        sequence,
        Mutation::Close {
            target,
            confirmation,
        },
    )?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}
fn main() -> Result<(), Box<dyn Error>> {
    let major = spaceterm_server::protocol::MAJOR;
    let client = ServerConnection::for_development_client("directory-recovery")?;
    require_idle(&client)?;
    client.setup()?;
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    if std::env::args().any(|arg| arg == "--cleanup") {
        cleanup_exercise(&connection)?;
        support::done(&connection, Operation::Shutdown)?;
        host.0.wait()?;
        println!("Removed only the retained directory-recovery exercise.");
        return Ok(());
    }
    if !inventory(&connection)?.0.workspaces.is_empty() {
        return Err("Existing Development work was preserved. Directory recovery requires an empty Development server.".into());
    }
    support::done(&connection, Operation::Shutdown)?;
    drop(connection);
    host.0.wait()?;
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    let root = std::env::temp_dir().join(format!(
        "spaceterm-directory-recovery-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let original = root.join("original");
    let replacement = root.join("replacement");
    std::fs::create_dir(&original)?;
    std::fs::create_dir(&replacement)?;
    let sequence = inventory(&connection)?.1;
    let workspace = mutate(
        &connection,
        sequence,
        Mutation::CreateWorkspace {
            name: "Directory recovery".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    )?;
    mutate(
        &connection,
        sequence + 1,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(original.clone()),
        },
    )?;
    // Pinning sets the starting directory of the next Tab, so create it explicitly.
    let tab = mutate(
        &connection,
        sequence + 2,
        Mutation::CreateTab {
            workspace,
            source: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    )?;
    let before = inventory(&connection)?.0;
    let pane = before.workspaces[0]
        .tabs
        .iter()
        .find(|t| t.id == tab)
        .ok_or("Tab missing")?
        .root_pane;
    let shell = attached(select(&connection, pane, true)?, "shell missing")?;
    shell.command("stty -echo; printf '\\033[2J\\033[HOWNING-SHELL\\n'; pwd")?;
    shell.await_output("OWNING-SHELL")?;
    // A foreign shell's marker cannot overwrite the owning directory.
    shell.command(
        "printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=foreign\\007'; printf 'NESTED-IGNORED\\n'",
    )?;
    shell.await_output("NESTED-IGNORED")?;
    std::thread::sleep(Duration::from_millis(200));
    host.kill()?;
    connection.close();
    drop(shell);
    std::fs::remove_dir(&original)?;
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    for _ in 0..3 {
        assert!(matches!(
            select(&connection, pane, false)?,
            Claimed::Restored(_)
        ));
    }
    assert_eq!(
        select(&connection, pane, true).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    let retained = target(&connection, pane)?;
    assert_eq!(retained.original, original);
    assert_eq!(
        repair(&connection, retained.clone(), None).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    std::fs::write(&original, b"not a directory")?;
    assert_eq!(
        repair(&connection, retained.clone(), None).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    std::fs::remove_file(&original)?;
    // Retry only starts after an explicit request, even once the original returns.
    std::fs::create_dir(&original)?;
    connection.close();
    let connection = connect(&client)?;
    assert!(matches!(
        select(&connection, pane, false)?,
        Claimed::Restored(_)
    ));
    let rival = connect(&client)?;
    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let second = scope.spawn(|| {
            barrier.wait();
            repair(&rival, retained.clone(), None)
        });
        barrier.wait();
        (
            repair(&connection, retained.clone(), None),
            second.join().unwrap(),
        )
    });
    let (Claimed::Attached(first), Claimed::Attached(second)) = (first?, second?) else {
        return Err("Retry did not start a shell".into());
    };
    let first = first.lease();
    assert_eq!(
        first.session,
        second.lease().session,
        "concurrent first use must start only one Session"
    );
    rival.close();
    drop(second);
    let resumed = attached(
        select(&connection, pane, false)?,
        "concurrent first-use Session could not be resumed",
    )?;
    assert_eq!(resumed.lease().session, first.session);
    resumed.command("printf '\\033[2J\\033[HRETRIED\\n'; pwd")?;
    resumed.await_output("RETRIED")?;
    support::done(&connection, Operation::Shutdown)?;
    host.0.wait()?;
    drop(resumed);
    drop(connection);
    std::fs::remove_dir(&original)?;
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    let retained = target(&connection, pane)?;
    assert_eq!(
        repair(&connection, retained.clone(), Some(original.clone())).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    assert!(!original.exists());
    let replaced = attached(
        repair(&connection, retained.clone(), Some(replacement.clone()))?,
        "replacement did not start a shell",
    )?;
    let after = inventory(&connection)?.0;
    let w = &after.workspaces[0];
    assert_eq!(
        (w.id, w.name.as_str(), &w.pinned_directory, w.root_tab),
        (
            workspace,
            "Directory recovery",
            &Some(original.clone()),
            before.workspaces[0].root_tab
        )
    );
    assert_eq!(w.tabs.iter().find(|t| t.id == tab).unwrap().root_pane, pane);
    let quoted = replacement
        .to_str()
        .ok_or("directory encoding")?
        .replace('\'', "'\\''");
    replaced.command(&format!(
        "if [ \"$PWD\" = '{quoted}' ]; then printf '\\033[2J\\033[HREPLACED\\n'; else printf 'WRONG-DIRECTORY\\n'; fi"
    ))?;
    replaced.await_output("REPLACED")?;
    let lease = replaced.lease();
    connection.close();
    drop(replaced);
    let connection = connect(&client)?;
    assert!(
        matches!(select(&connection, pane, false)?, Claimed::Attached(resumed) if resumed.lease().session == lease.session)
    );
    // Cleanup removes only the Workspace created by this exercise.
    let (inventory, sequence) = inventory(&connection)?;
    let confirmation = inventory
        .close_confirmation(spaceterm_server::hierarchy::CloseTarget::KillWorkspace { workspace })
        .ok_or("close confirmation")?;
    mutate(
        &connection,
        sequence,
        Mutation::Close {
            target: spaceterm_server::hierarchy::CloseTarget::KillWorkspace { workspace },
            confirmation,
        },
    )?;
    support::done(&connection, Operation::Shutdown)?;
    host.0.wait()?;
    std::fs::remove_dir_all(root)?;
    println!(
        "Protocol {major}: authenticated restart, unavailable/file-changed directories, concurrent first-use Retry, replacement, nested reports, identity preservation and passive reconnect passed with real native PTYs without credential storage."
    );
    Ok(())
}
