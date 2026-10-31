//! Real-host server restart exercise. Requires an idle Development server and account-local authority.
#[path = "support/lifecycle.rs"]
mod lifecycle;
#[expect(
    dead_code,
    reason = "the restart exercise claims only the inventory's first Pane"
)]
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm_server::{
    ServerConnection, ServerError, connection::link::ServerLink, pane::*, protocol::Operation,
};
use std::{error::Error, time::Duration};
use support::{Claimed, attach};

/// Selects the inventory's first Pane. Without `create`, an empty inventory has no Pane.
fn select(link: &ServerLink, create: bool) -> Result<Claimed, ServerError> {
    let pane = support::first_pane(link, create)?;
    support::select(link, pane, create, size())
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    if std::env::args().nth(1).as_deref() == Some("--fault-server") {
        faults::FLAG
            .set(std::env::args_os().nth(2).map(std::path::PathBuf::from))
            .map_err(|_| "fault setup")?;
        return spaceterm_server::run_local_with_panes(
            "spaceterm-development",
            spaceterm_server::panes(|sync| {
                Ok(spaceterm::runtime::Served::new(
                    spaceterm::runtime::WorkspaceHost::local(sync)?,
                ))
            }),
        )
        .map_err(Into::into);
    }
    let major = spaceterm_server::protocol::MAJOR;
    let name = std::env::var("SPACETERM_DEVELOPMENT_CLIENT").unwrap_or("restore-smoke".into());
    let client = ServerConnection::for_development_client(&name)?;
    require_idle(&client)?;
    client.setup()?;
    let mut host = Server::start()?;
    let setup = connect(&client)?;
    // The server creates an initial Workspace at startup. A host whose only work is that one
    // Workspace, with one Pane and no running Session, is empty.
    let (inventory, _) = support::inventory(&setup)?;
    let live = inventory
        .close
        .as_ref()
        .is_some_and(|close| close.sessions.iter().any(|pane| pane.session.is_some()));
    let initial = match inventory.workspaces.as_slice() {
        [] => true,
        [only] => only.tabs.len() == 1 && only.tabs[0].panes.len() == 1,
        _ => false,
    };
    if live
        || !initial
        || !matches!(
            select(&setup, false),
            Err(ServerError::SessionUnavailable) | Ok(Claimed::Restored(_))
        )
    {
        return Err(
            "Retained Development work exists; it was preserved. Use an empty Development host."
                .into(),
        );
    }
    support::done(&setup, Operation::Shutdown)?;
    drop(setup);
    host.0.wait()?;
    #[cfg(target_os = "linux")]
    let uncertain_ids = faults::exercise(&client)?;
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    let original = attach(&connection, true, size())?;
    original.command(
        "stty -echo; cd /; printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HOLD-PROGRAM\\n'; sleep 60",
    )?;
    original.await_output("OLD-PROGRAM")?;
    // Allow the background directory publication to complete before forced termination.
    std::thread::sleep(Duration::from_millis(200));
    host.kill()?;
    connection.close();
    let original = original.lease();
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    // A claim names a Pane, not a prior lease, so this passive selection stands in for resuming
    // `original`. Only an explicit selection may start a shell.
    let Claimed::Restored(restored) = select(&connection, false)? else {
        return Err("retained Pane missing".into());
    };
    let (server, workspace, tab, pane) = (
        restored.server,
        restored.workspace,
        restored.tab,
        restored.pane,
    );
    #[cfg(target_os = "linux")]
    assert_eq!((workspace, tab, pane), uncertain_ids);
    for _ in 0..3 {
        assert!(
            matches!(select(&connection, false)?, Claimed::Restored(r) if (r.workspace, r.tab, r.pane) == (workspace, tab, pane))
        );
    }
    let Claimed::Attached(fresh) =
        support::claim(&connection, pane, AttachmentClaim::Start { server }, size())?
    else {
        return Err("fresh shell missing".into());
    };
    let lease = fresh.lease();
    assert_ne!(original.server, lease.server);
    assert_ne!(original.session, lease.session);
    fresh.command("printf '\\033[2J\\033[HFRESH-SHELL\\n'; pwd; stty size")?;
    let output = fresh.await_output("24 80")?;
    assert!(output.lines().any(|line| line.trim() == "/"));
    assert!(!output.contains("OLD-PROGRAM"));
    // A request names a channel of its own connection, so the prior boot's lease can no longer
    // be presented. The Session comparison above shows the restart replaced it.
    support::done(&connection, Operation::Shutdown)?;
    host.0.wait()?;
    drop(fresh);
    drop(connection);
    let mut host = Server::start()?;
    let connection = connect(&client)?;
    assert!(
        matches!(select(&connection, false)?, Claimed::Restored(r) if (r.workspace, r.tab, r.pane) == (workspace, tab, pane))
    );
    let last = attach(&connection, true, size())?;
    last.close()?;
    support::done(&connection, Operation::Shutdown)?;
    host.0.wait()?;
    println!(
        "Protocol {major}: kill/reopen and shutdown/reopen retained Workspace, Tab, Pane and client identity; reconnect stayed dormant; explicit first use started one fresh shell in the confirmed directory with measured dimensions; the prior boot's Session was replaced."
    );
    Ok(())
}

// The example alone interposes the native filesystem call. Production has no failure switch.
#[cfg(target_os = "linux")]
mod faults {
    use super::*;
    use std::process::{Command, Stdio};
    use std::{
        path::PathBuf,
        sync::{
            OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
    };
    pub static FLAG: OnceLock<Option<PathBuf>> = OnceLock::new();
    static SYNCS: AtomicUsize = AtomicUsize::new(0);

    #[unsafe(export_name = "fsync")]
    extern "C" fn fsync_with_failure(fd: libc::c_int) -> libc::c_int {
        let mode = FLAG
            .get()
            .and_then(Option::as_ref)
            .and_then(|path| std::fs::read(path).ok());
        let mut fail = false;
        if let Some(mode) = mode {
            // SAFETY: stat is writable and fstat accepts any descriptor.
            let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut metadata) } == 0 {
                let kind = if mode == b"file" {
                    libc::S_IFREG
                } else {
                    libc::S_IFDIR
                };
                if metadata.st_mode & libc::S_IFMT == kind {
                    // First synchronize the old record. Fail the pending-file sync or the
                    // post-rename directory sync, then keep recovery blocked until disarmed.
                    fail = SYNCS.fetch_add(1, Ordering::AcqRel) >= 1;
                }
            }
        } else {
            SYNCS.store(0, Ordering::Release);
        }
        if fail {
            // SAFETY: the returned errno pointer belongs to this thread.
            unsafe {
                *libc::__errno_location() = libc::EIO;
            }
            -1
        } else {
            // SAFETY: invoke the native fsync syscall with its one descriptor argument.
            unsafe { libc::syscall(libc::SYS_fsync, fd) as libc::c_int }
        }
    }
    type OrganizationIds = ([u8; 32], [u8; 32], [u8; 32]);

    pub fn exercise(client: &ServerConnection) -> Result<OrganizationIds, Box<dyn Error>> {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-533-faults-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&root)?;
        let flag = root.join("fail");
        let mut host = Server(
            Command::new(std::env::current_exe()?)
                .arg("--fault-server")
                .arg(&flag)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let connection = connect(client)?;
        for mode in [b"file".as_slice(), b"directory".as_slice()] {
            std::fs::write(&flag, mode)?;
            assert_eq!(
                select(&connection, true).err(),
                Some(ServerError::StorageUnavailable)
            );
            assert_eq!(
                select(&connection, true).err(),
                Some(ServerError::StorageUnavailable)
            );
            std::fs::remove_file(&flag)?;
            if mode == b"file" {
                assert_eq!(
                    select(&connection, false).err(),
                    Some(ServerError::SessionUnavailable)
                );
            }
        }
        let Claimed::Restored(restored) = select(&connection, false)? else {
            return Err("uncertain publication was replaced or launched".into());
        };
        let (workspace, tab, pane) = (restored.workspace, restored.tab, restored.pane);
        host.kill()?;
        std::fs::remove_dir(&root)?;
        println!(
            "Injected pending-file and post-rename fsync failures refused mutations until recovery validated the actual outcome."
        );
        Ok((workspace, tab, pane))
    }
}
