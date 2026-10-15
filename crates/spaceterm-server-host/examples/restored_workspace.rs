//! Real-host server restart exercise. Requires an idle Development server and native credentials.
#[path = "support/lifecycle.rs"]
mod lifecycle;
mod support;
use lifecycle::{Server, connect, require_idle, size};
use spaceterm_server::{LocalServer, ServerError, pane::*};
use std::{error::Error, time::Duration};
use support::{attach, await_output, command};

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    if std::env::args().nth(1).as_deref() == Some("--fault-server") {
        faults::FLAG
            .set(std::env::args_os().nth(2).map(std::path::PathBuf::from))
            .map_err(|_| "fault setup")?;
        return spaceterm_server::run_local_with_panes(
            "spaceterm-development",
            spaceterm::runtime::DevelopmentPaneHost::local()?,
        )
        .map_err(Into::into);
    }
    let major = std::env::args().nth(1).unwrap_or("2".into()).parse()?;
    let name = std::env::var("SPACETERM_DEVELOPMENT_CLIENT").unwrap_or("restore-smoke".into());
    let client = LocalServer::for_development_client(&name, major)?;
    require_idle(&client)?;
    // Setup locates its sibling binary. Stage the built host beside this example, including
    // when Cargo uses a shared target directory outside the working tree.
    let directory = std::env::current_exe()?
        .parent()
        .ok_or("example directory")?
        .to_owned();
    let companion = directory.join("spaceterm-server");
    if !companion.try_exists()? {
        std::os::unix::fs::symlink("../spaceterm-server", &companion)?;
    }
    client.setup_and_start()?;
    let mut setup = connect(&client)?;
    match setup.exchange(&Request::Select {
        create: false,
        size: size(),
        resume: None,
        style: None,
    }) {
        Err(ServerError::SessionUnavailable) => {}
        _ => return Err(
            "Retained Development work exists; it was preserved. Use an empty Development host."
                .into(),
        ),
    }
    setup.exchange(&Request::Shutdown)?;
    drop(setup);
    std::thread::sleep(Duration::from_millis(200));
    #[cfg(target_os = "linux")]
    let uncertain_ids = faults::exercise(&client)?;
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    let original = attach(&mut connection, true)?;
    command(
        &mut connection,
        original,
        "stty -echo; cd /; printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HOLD-PROGRAM\\n'; sleep 60",
    )?;
    await_output(&mut connection, original, "OLD-PROGRAM")?;
    // Allow the background directory publication to complete before forced termination.
    std::thread::sleep(Duration::from_millis(200));
    host.kill()?;
    drop(connection);
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    let restored = connection.exchange(&Request::Select {
        create: false,
        size: size(),
        resume: Some(original.into()),
        style: None,
    })?;
    let Reply::Restored {
        server,
        workspace,
        tab,
        pane,
    } = restored
    else {
        return Err("retained Pane missing".into());
    };
    #[cfg(target_os = "linux")]
    assert_eq!((workspace, tab, pane), uncertain_ids);
    for _ in 0..3 {
        assert!(
            matches!(connection.exchange(&Request::Select { create: false, size: size(), resume: None, style: None })?, Reply::Restored { workspace: w, tab: t, pane: p, .. } if (w,t,p) == (workspace,tab,pane))
        );
    }
    let Reply::Attached { lease } = connection.exchange(&Request::StartRestored {
        server,
        pane,
        size: size(),
        style: None,
    })?
    else {
        return Err("fresh shell missing".into());
    };
    assert_ne!(original.server, lease.server);
    assert_ne!(original.session, lease.session);
    command(
        &mut connection,
        lease,
        "printf '\\033[2J\\033[HFRESH-SHELL\\n'; pwd; stty size",
    )?;
    let output = await_output(&mut connection, lease, "24 80")?;
    assert!(output.lines().any(|line| line.trim() == "/"));
    assert!(!output.contains("OLD-PROGRAM"));
    assert_eq!(
        connection
            .exchange(&Request::Poll { lease: original })
            .err(),
        Some(ServerError::StaleControl)
    );
    connection.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    drop(connection);
    let mut host = Server::start()?;
    let mut connection = connect(&client)?;
    assert!(
        matches!(connection.exchange(&Request::Select { create: false, size: size(), resume: None, style: None })?, Reply::Restored { workspace: w, tab: t, pane: p, .. } if (w,t,p) == (workspace,tab,pane))
    );
    let final_lease = attach(&mut connection, true)?;
    connection.exchange(&Request::Close { lease: final_lease })?;
    connection.exchange(&Request::Shutdown)?;
    host.0.wait()?;
    println!(
        "Protocol {major}: kill/reopen and shutdown/reopen retained Workspace, Tab, Pane and pairing; reconnect stayed dormant; explicit first use started one fresh shell in the confirmed directory with measured dimensions; old leases were refused."
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

    pub fn exercise(client: &LocalServer) -> Result<OrganizationIds, Box<dyn Error>> {
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
        let mut connection = connect(client)?;
        let select = |create| Request::Select {
            create,
            size: size(),
            resume: None,
            style: None,
        };
        for mode in [b"file".as_slice(), b"directory".as_slice()] {
            std::fs::write(&flag, mode)?;
            assert_eq!(
                connection.exchange(&select(true)).err(),
                Some(ServerError::StorageUnavailable)
            );
            assert_eq!(
                connection.exchange(&select(true)).err(),
                Some(ServerError::StorageUnavailable)
            );
            std::fs::remove_file(&flag)?;
            if mode == b"file" {
                assert_eq!(
                    connection.exchange(&select(false)).err(),
                    Some(ServerError::SessionUnavailable)
                );
            }
        }
        let Reply::Restored {
            workspace,
            tab,
            pane,
            ..
        } = connection.exchange(&select(false))?
        else {
            return Err("uncertain publication was replaced or launched".into());
        };
        host.kill()?;
        std::fs::remove_dir(&root)?;
        println!(
            "Injected pending-file and post-rename fsync failures refused mutations until recovery validated the actual outcome."
        );
        Ok((workspace, tab, pane))
    }
}
