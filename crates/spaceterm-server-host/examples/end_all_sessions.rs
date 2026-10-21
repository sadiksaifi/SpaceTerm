//! Native evidence for the actual Settings action, on an explicitly disposable Preflight server.
//! Run with `--features client -- spaceterm-preflight --confirm-disposable-preflight`.
//! The helper prepares real PTYs, waits five minutes for the UI, then verifies and cleans up.

#[cfg(feature = "client")]
#[expect(
    dead_code,
    reason = "This check uses named Panes rather than legacy attach."
)]
mod support;

#[cfg(feature = "client")]
mod exercise {
    use super::support::{await_output, command};
    use spaceterm_server::{
        LocalServer, ServerError,
        hierarchy::{Axis, CloseTarget, Id, Inventory, Mutation, Outcome},
        pane::{control::Controller, *},
    };
    use std::{
        error::Error,
        fs,
        os::unix::fs::{DirBuilderExt, MetadataExt},
        path::Path,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    type Result<T> = std::result::Result<T, Box<dyn Error>>;
    const IDENTITY: &str = "spaceterm-preflight";

    fn size() -> PaneSize {
        PaneSize {
            columns: 97,
            rows: 31,
            cell_width: 8,
            cell_height: 18,
        }
    }

    fn inventory(connection: &mut PaneConnection) -> Result<(Inventory, u64)> {
        match connection.exchange(&Request::Inventory)? {
            Reply::Inventory {
                inventory,
                next_sequence,
            } => Ok((inventory, next_sequence)),
            _ => Err("inventory unavailable".into()),
        }
    }

    fn select(connection: &mut PaneConnection, pane: Id, explicit: bool) -> Result<Reply> {
        Ok(connection.exchange(&Request::SelectPane {
            pane,
            explicit,
            size: size(),
            style: None,
        })?)
    }

    fn attached(reply: Reply) -> Result<Lease> {
        match reply {
            Reply::Attached { lease } => Ok(lease),
            _ => Err("expected an explicitly started Terminal Session".into()),
        }
    }

    fn mutate(
        server: &LocalServer,
        connection: &mut PaneConnection,
        mutation: Mutation,
    ) -> Result<Id> {
        let mut journal = server.shared_journal()?;
        if journal.pending().is_some() {
            return Err("resolve the original fixture operation before continuing".into());
        }
        let (_, sequence) = inventory(connection)?;
        journal.submit(sequence, mutation.clone())?;
        let mut reply = connection.exchange(&Request::Mutate {
            sequence,
            mutation: mutation.clone(),
        })?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while matches!(reply, Reply::MutationPending) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            reply = connection.exchange(&Request::Resolve { sequence })?;
        }
        let Reply::Outcome(outcome) = reply else {
            return Err("fixture operation remains unresolved".into());
        };
        if outcome.sequence != sequence || outcome.mutation != mutation {
            return Err("fixture operation identity changed".into());
        }
        journal.complete(outcome.clone())?;
        connection.exchange(&Request::Acknowledge { sequence })?;
        journal.retire()?;
        match outcome.outcome {
            Outcome::Accepted { target } => Ok(target),
            _ => Err("fixture mutation refused".into()),
        }
    }

    fn no_pending_journals(path: &Path, remaining: &mut usize) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            *remaining = remaining
                .checked_sub(1)
                .ok_or("client storage exceeds fixture bound")?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err("client storage contains a symlink".into());
            }
            if kind.is_dir() {
                no_pending_journals(&entry.path(), remaining)?;
            } else if entry.file_name() == "operations.json" {
                let metadata = entry.metadata()?;
                // SAFETY: geteuid has no preconditions.
                if metadata.len() > 1024 * 1024 || metadata.uid() != unsafe { libc::geteuid() } {
                    return Err("client journal ownership or size refused".into());
                }
                let record: serde_json::Value = serde_json::from_slice(&fs::read(entry.path())?)?;
                if record
                    .get("pending")
                    .is_none_or(|pending| !pending.is_null())
                {
                    return Err("unresolved client original was preserved; prepare refused".into());
                }
            }
        }
        Ok(())
    }

    fn named_clients(first: &str, second: &str) -> Result<(LocalServer, LocalServer)> {
        let previous = std::env::var_os("SPACETERM_DEVELOPMENT_CLIENT");
        // SAFETY: single-threaded startup, before connections or native PTY workers exist.
        // LocalServer construction only resolves paths and captures this nonsecret name.
        unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", first) };
        let a = LocalServer::for_identity(IDENTITY);
        // SAFETY: the same single-threaded startup boundary still holds.
        unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", second) };
        let b = LocalServer::for_identity(IDENTITY);
        // SAFETY: restore the launch environment before any worker is created.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", value),
                None => std::env::remove_var("SPACETERM_DEVELOPMENT_CLIENT"),
            }
        }
        Ok((a?, b?))
    }

    fn organization(inventory: &Inventory) -> Vec<spaceterm_server::hierarchy::Workspace> {
        let mut workspaces = inventory.workspaces.clone();
        for pane in workspaces
            .iter_mut()
            .flat_map(|w| &mut w.tabs)
            .flat_map(|t| &mut t.panes)
        {
            // Directory metadata may be confirmed while stopping; control is checked separately.
            pane.confirmed_directory = None;
            pane.creator = None;
        }
        workspaces
    }

    fn takeover(reply: Reply, name: Option<&str>) -> Result<()> {
        let Reply::Takeover {
            controller,
            session_ended: true,
            ..
        } = reply
        else {
            return Err("ended Pane did not retain its Takeover Screen".into());
        };
        match (controller, name) {
            (Controller::Nobody, None) => Ok(()),
            (
                Controller::Client {
                    name,
                    this_client: false,
                },
                Some(expected),
            ) if name.as_str().ends_with(&format!("({expected})")) => Ok(()),
            _ => Err("ended Pane changed its named Controlling Client".into()),
        }
    }

    fn restored(reply: Reply) -> Result<()> {
        if !matches!(
            reply,
            Reply::Restored {
                session_ended: true,
                ..
            }
        ) {
            return Err("controller did not observe an ended retained Pane".into());
        }
        Ok(())
    }

    fn stale(connection: &mut PaneConnection, lease: Lease) -> Result<()> {
        for request in [
            Request::Poll { lease },
            Request::Input {
                lease,
                generation: 1,
                input: InteractiveInput::Paste {
                    text: "STALE_557".into(),
                },
            },
        ] {
            if !matches!(
                connection.exchange(&request),
                Err(ServerError::StaleControl)
            ) {
                return Err("ended Session accepted an old lease or input".into());
            }
        }
        Ok(())
    }

    pub(super) fn run() -> Result<()> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if args != [IDENTITY, "--confirm-disposable-preflight"] {
            return Err(
                "usage: end_all_sessions spaceterm-preflight --confirm-disposable-preflight".into(),
            );
        }
        let home = std::env::var_os("HOME").ok_or("account home unavailable")?;
        no_pending_journals(
            &Path::new(&home)
                .join(".local/share")
                .join(IDENTITY)
                .join("server/clients"),
            &mut 1024,
        )?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let name = format!("End All Sessions {stamp}");
        let first_name = format!("end557-{}-a", std::process::id());
        let second_name = format!("end557-{}-b", std::process::id());
        let (first, second) = named_clients(&first_name, &second_name)?;
        let mut a = first.connect_shared_pane()?;
        if !a.supports_end_all_sessions() {
            return Err("server lacks End All Sessions capability".into());
        }
        let initial = inventory(&mut a)?.0;
        if initial
            .close
            .as_ref()
            .is_none_or(|close| close.sessions.iter().any(|p| p.session.is_some()))
        {
            return Err("unrelated live Sessions were preserved; prepare refused".into());
        }
        let directory =
            std::env::temp_dir().join(format!("spaceterm-end-all-{stamp}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        fs::write(directory.join("retained-file"), b"repository-file-witness")?;
        let workspace = mutate(
            &first,
            &mut a,
            Mutation::CreateWorkspace {
                name: name.clone(),
                directory: Some(directory.clone()),
            },
        )?;
        println!(
            "Prepared disposable Workspace {name:?}. On failure preserve it for inspection; this helper never stops the service."
        );
        let prepared = inventory(&mut a)?.0;
        let tab = prepared
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .ok_or("created Workspace missing")?
            .tabs
            .first()
            .ok_or("created Tab missing")?;
        let root = tab.root_pane;
        let tab = tab.id;
        let other = mutate(
            &first,
            &mut a,
            Mutation::SplitPane {
                workspace,
                tab,
                source: root,
                axis: Axis::Right,
            },
        )?;
        let unclaimed = mutate(
            &first,
            &mut a,
            Mutation::SplitPane {
                workspace,
                tab,
                source: root,
                axis: Axis::Down,
            },
        )?;
        let mut leases = Vec::new();
        for (index, pane) in [root, other, unclaimed].into_iter().enumerate() {
            let lease = attached(a.exchange(&Request::SelectPane {
                pane,
                explicit: true,
                size: PaneSize {
                    columns: 80,
                    rows: 24,
                    ..size()
                },
                style: None,
            })?)?;
            let marker = format!("END_ALL_557_{stamp}_{index}");
            command(
                &mut a,
                lease,
                &format!(
                    "stty -echo; printf '\\033[2J\\033[H{marker}\\n'; while :; do sleep 60; done"
                ),
            )?;
            await_output(&mut a, lease, &marker)?;
            leases.push((lease, marker));
        }
        let mut b = second.connect_shared_pane()?;
        let transferred = attached(b.exchange(&Request::TakeOver {
            target: leases[1].0.into(),
            size: size(),
            style: None,
        })?)?;
        leases[1].0 = transferred;
        a.exchange(&Request::Release { lease: leases[2].0 })?;
        drop(b); // Its grant survives this deliberate disconnect until the actual UI action.
        let before = inventory(&mut a)?.0;
        let boot = before.close.as_ref().ok_or("live boot unavailable")?.boot;
        println!(
            "READY: three marked PTYs; controllers {first_name:?}, disconnected {second_name:?}, and nobody. Invoke End All Sessions in Preflight Settings within five minutes."
        );
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let current = inventory(&mut a)?.0;
            let close = current
                .close
                .as_ref()
                .ok_or("live session inventory unavailable")?;
            if close.boot != boot {
                return Err("server restarted instead of ending Sessions".into());
            }
            if [root, other, unclaimed].iter().all(|id| {
                close
                    .sessions
                    .iter()
                    .any(|p| p.id == *id && p.session.is_none())
            }) {
                if organization(&current) != organization(&before) {
                    return Err("End All Sessions changed saved organization".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("actual Settings action was not observed; fixture retained".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        restored(select(&mut a, root, false)?)?;
        takeover(select(&mut a, other, false)?, Some(&second_name))?;
        takeover(select(&mut a, unclaimed, false)?, None)?;
        let mut b = second.connect_shared_pane()?;
        restored(select(&mut b, other, false)?)?;
        takeover(select(&mut b, root, false)?, Some(&first_name))?;
        stale(&mut a, leases[0].0)?;
        stale(&mut b, leases[1].0)?;
        std::thread::sleep(Duration::from_millis(500));
        if inventory(&mut a)?
            .0
            .close
            .as_ref()
            .is_none_or(|close| close.sessions.iter().any(|p| p.session.is_some()))
        {
            return Err("passive reconciliation started a shell".into());
        }
        let fresh = attached(select(&mut a, root, true)?)?;
        if fresh.session == leases[0].0.session || fresh.control == leases[0].0.control {
            return Err("explicit restart reused an ended Session identity".into());
        }
        stale(&mut a, leases[0].0)?;
        command(
            &mut a,
            fresh,
            "stty -echo; stty size; printf 'FRESH_557\\n'",
        )?;
        let output = await_output(&mut a, fresh, "FRESH_557")?;
        if !output.lines().any(|line| line.trim_end() == "31 97")
            || leases.iter().any(|(_, marker)| output.contains(marker))
        {
            return Err("restart did not use measured dimensions and cleared output".into());
        }
        a.exchange(&Request::Read {
            lease: fresh,
            operation: reading::Operation::Query {
                generation: 1,
                text: format!("END_ALL_557_{stamp}"),
            },
        })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Reply::Screen(screen) = a.exchange(&Request::Poll { lease: fresh })?
                && let Some(find) = screen.reading.and_then(|reading| reading.find)
                && find.generation == 1
            {
                if find.total != 0 {
                    return Err("ended output survived in Scrollback".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("cleared Scrollback query did not finish".into());
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if fs::read(directory.join("retained-file"))? != b"repository-file-witness" {
            return Err("End All Sessions changed a repository-file witness".into());
        }
        let current = inventory(&mut a)?.0;
        let target = CloseTarget::Workspace { workspace };
        let confirmation = current
            .close_confirmation(target)
            .ok_or("fixture cleanup confirmation unavailable")?;
        mutate(
            &first,
            &mut a,
            Mutation::Close {
                target,
                confirmation,
            },
        )?;
        if inventory(&mut a)?
            .0
            .workspaces
            .iter()
            .any(|w| w.id == workspace)
        {
            return Err("exact fixture cleanup was not confirmed".into());
        }
        fs::remove_file(directory.join("retained-file"))?;
        fs::remove_dir(directory)?;
        println!(
            "PASS: actual UI end retained organization and named/disconnected/unclaimed grants; passive reconciliation stayed dormant; old leases were refused; explicit restart measured 31x97 with cleared output and Scrollback; exact fixture cleanup completed. Server remains running."
        );
        Ok(())
    }
}

#[cfg(feature = "client")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    exercise::run()
}

#[cfg(not(feature = "client"))]
fn main() {
    eprintln!("end_all_sessions requires --features client");
    std::process::exit(2);
}
