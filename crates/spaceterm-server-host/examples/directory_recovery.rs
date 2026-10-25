//! Authenticated real-host directory recovery. Uses account-local authority and an idle Development host.
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "this exercise uses named Pane claims rather than legacy attach"
)]
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm_server::{
    ServerConnection, ServerError,
    hierarchy::{Mutation, Outcome},
    pane::*,
};
use std::{error::Error, path::PathBuf, time::Duration};
use support::{await_output, command};

fn inventory(
    connection: &mut PaneConnection,
) -> Result<(spaceterm_server::hierarchy::Inventory, u64), Box<dyn Error>> {
    match connection.exchange(&Request::Inventory)? {
        Reply::Inventory {
            inventory,
            next_sequence,
        } => Ok((inventory, next_sequence)),
        _ => Err("inventory unavailable".into()),
    }
}
fn mutate(
    connection: &mut PaneConnection,
    sequence: u64,
    mutation: Mutation,
) -> Result<[u8; 32], Box<dyn Error>> {
    let Reply::Outcome(outcome) = connection.exchange(&Request::Mutate { sequence, mutation })?
    else {
        return Err("mutation reply missing".into());
    };
    let Outcome::Accepted { target } = outcome.outcome else {
        return Err("mutation refused".into());
    };
    connection.exchange(&Request::Acknowledge { sequence })?;
    Ok(target)
}
fn select(
    connection: &mut PaneConnection,
    pane: [u8; 32],
    explicit: bool,
) -> Result<Reply, ServerError> {
    connection.exchange(&Request::SelectPane {
        pane,
        explicit,
        size: size(),
        style: None,
    })
}
fn target(
    connection: &mut PaneConnection,
    pane: [u8; 32],
) -> Result<directories::Target, Box<dyn Error>> {
    match select(connection, pane, false)? {
        Reply::Restored {
            recovery: Some(target),
            ..
        } => Ok(target),
        _ => Err("retained recovery target missing".into()),
    }
}
fn repair(target: directories::Target, replacement: Option<PathBuf>) -> Request {
    Request::RepairDirectory {
        target,
        replacement,
        size: size(),
        style: None,
    }
}
fn cleanup_exercise(connection: &mut PaneConnection) -> Result<(), Box<dyn Error>> {
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
    let target = spaceterm_server::hierarchy::CloseTarget::Workspace {
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
    let major = std::env::args().nth(1).unwrap_or("1".into()).parse()?;
    let client = ServerConnection::for_development_client("directory-recovery", major)?;
    require_idle(&client)?;
    client.setup()?;
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    if !connection.supports_directory_recovery() {
        return Err(
            "Running server does not support directory recovery; use this branch's server build."
                .into(),
        );
    }
    if std::env::args().any(|arg| arg == "--cleanup") {
        cleanup_exercise(&mut connection)?;
        connection.exchange(&Request::Shutdown)?;
        host.0.wait()?;
        println!("Removed only the retained directory-recovery exercise.");
        return Ok(());
    }
    if !inventory(&mut connection)?.0.workspaces.is_empty() {
        return Err("Existing Development work was preserved. Directory recovery requires an empty Development server.".into());
    }
    connection.exchange(&Request::Shutdown)?;
    drop(connection);
    host.0.wait()?;
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
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
    let sequence = inventory(&mut connection)?.1;
    let workspace = mutate(
        &mut connection,
        sequence,
        Mutation::CreateWorkspace {
            name: "Directory recovery".into(),
            directory: None,
        },
    )?;
    mutate(
        &mut connection,
        sequence + 1,
        Mutation::PinWorkspace {
            workspace,
            version: 0,
            directory: Some(original.clone()),
        },
    )?;
    // Pinning sets the starting directory of the next Tab, so create it explicitly.
    let tab = mutate(
        &mut connection,
        sequence + 2,
        Mutation::CreateTab {
            workspace,
            source: None,
        },
    )?;
    let before = inventory(&mut connection)?.0;
    let pane = before.workspaces[0]
        .tabs
        .iter()
        .find(|t| t.id == tab)
        .ok_or("Tab missing")?
        .root_pane;
    let Reply::Attached { lease } = select(&mut connection, pane, true)? else {
        return Err("shell missing".into());
    };
    command(
        &mut connection,
        lease,
        "stty -echo; printf '\\033[2J\\033[HOWNING-SHELL\\n'; pwd",
    )?;
    await_output(&mut connection, lease, "OWNING-SHELL")?;
    // A foreign shell's marker cannot overwrite the owning directory.
    command(
        &mut connection,
        lease,
        "printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=foreign\\007'; printf 'NESTED-IGNORED\\n'",
    )?;
    await_output(&mut connection, lease, "NESTED-IGNORED")?;
    std::thread::sleep(Duration::from_millis(200));
    host.kill()?;
    drop(connection);
    std::fs::remove_dir(&original)?;
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    for _ in 0..3 {
        assert!(matches!(
            select(&mut connection, pane, false)?,
            Reply::Restored { .. }
        ));
    }
    assert_eq!(
        select(&mut connection, pane, true).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    let retained = target(&mut connection, pane)?;
    assert_eq!(retained.original, original);
    assert_eq!(
        connection.exchange(&repair(retained.clone(), None)).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    std::fs::write(&original, b"not a directory")?;
    assert_eq!(
        connection.exchange(&repair(retained.clone(), None)).err(),
        Some(ServerError::DirectoryUnavailable)
    );
    std::fs::remove_file(&original)?;
    // Retry only starts after an explicit request, even once the original returns.
    std::fs::create_dir(&original)?;
    drop(connection);
    let mut connection = connect(&client)?;
    assert!(matches!(
        select(&mut connection, pane, false)?,
        Reply::Restored { .. }
    ));
    let mut rival = connect(&client)?;
    let request = repair(retained, None);
    let barrier = std::sync::Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let second = scope.spawn(|| {
            barrier.wait();
            rival.exchange(&request)
        });
        barrier.wait();
        (connection.exchange(&request), second.join().unwrap())
    });
    let (Reply::Attached { lease: first }, Reply::Attached { lease: second }) = (first?, second?)
    else {
        return Err("Retry did not start a shell".into());
    };
    assert_eq!(
        first.session, second.session,
        "concurrent first use must start only one Session"
    );
    drop(rival);
    let Reply::Attached { lease } = select(&mut connection, pane, false)? else {
        return Err("concurrent first-use Session could not be resumed".into());
    };
    assert_eq!(lease.session, first.session);
    command(
        &mut connection,
        lease,
        "printf '\\033[2J\\033[HRETRIED\\n'; pwd",
    )?;
    await_output(&mut connection, lease, "RETRIED")?;
    connection.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    drop(connection);
    std::fs::remove_dir(&original)?;
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    let retained = target(&mut connection, pane)?;
    assert_eq!(
        connection
            .exchange(&repair(retained.clone(), Some(original.clone())))
            .err(),
        Some(ServerError::DirectoryUnavailable)
    );
    assert!(!original.exists());
    let Reply::Attached { lease } =
        connection.exchange(&repair(retained.clone(), Some(replacement.clone())))?
    else {
        return Err("replacement did not start a shell".into());
    };
    let after = inventory(&mut connection)?.0;
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
    command(
        &mut connection,
        lease,
        &format!(
            "if [ \"$PWD\" = '{quoted}' ]; then printf '\\033[2J\\033[HREPLACED\\n'; else printf 'WRONG-DIRECTORY\\n'; fi"
        ),
    )?;
    await_output(&mut connection, lease, "REPLACED")?;
    drop(connection);
    let mut connection = connect(&client)?;
    assert!(
        matches!(select(&mut connection, pane, false)?, Reply::Attached { lease: resumed } if resumed.session == lease.session)
    );
    // Cleanup removes only the Workspace created by this exercise.
    let (inventory, sequence) = inventory(&mut connection)?;
    let confirmation = inventory
        .close_confirmation(spaceterm_server::hierarchy::CloseTarget::Workspace { workspace })
        .ok_or("close confirmation")?;
    mutate(
        &mut connection,
        sequence,
        Mutation::Close {
            target: spaceterm_server::hierarchy::CloseTarget::Workspace { workspace },
            confirmation,
        },
    )?;
    connection.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    std::fs::remove_dir_all(root)?;
    println!(
        "Protocol {major}: authenticated restart, unavailable/file-changed directories, concurrent first-use Retry, replacement, nested reports, identity preservation and passive reconnect passed with real native PTYs without credential storage."
    );
    Ok(())
}
