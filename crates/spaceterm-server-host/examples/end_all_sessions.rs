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
    use super::support::{self, Claimed, Ending, Pane};
    use spaceterm_server::{
        ServerConnection, ServerError,
        connection::link::ServerLink,
        hierarchy::{Axis, CloseTarget, Id, Inventory, Mutation, Outcome},
        pane::{control::Controller, *},
        protocol::PaneEvent,
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

    fn inventory(link: &ServerLink) -> Result<(Inventory, u64)> {
        Ok(support::inventory(link)?)
    }

    fn select(link: &ServerLink, pane: Id, explicit: bool) -> Result<Claimed> {
        Ok(support::select(link, pane, explicit, size())?)
    }

    fn attached(claimed: Claimed) -> Result<Pane> {
        claimed
            .attached()
            .ok_or_else(|| "expected an explicitly started Terminal Session".into())
    }

    fn mutate(server: &ServerConnection, link: &ServerLink, mutation: Mutation) -> Result<Id> {
        let mut journal = server.shared_journal()?;
        if journal.pending().is_some() {
            return Err("resolve the original fixture operation before continuing".into());
        }
        let (_, sequence) = inventory(link)?;
        journal.submit(sequence, mutation.clone())?;
        let outcome = support::mutate(link, sequence, mutation.clone())?;
        if outcome.sequence != sequence || outcome.mutation != mutation {
            return Err("fixture operation identity changed".into());
        }
        journal.retire(outcome.clone())?;
        link.acknowledge(sequence)?;
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

    fn named_clients(first: &str, second: &str) -> Result<(ServerConnection, ServerConnection)> {
        let previous = std::env::var_os("SPACETERM_DEVELOPMENT_CLIENT");
        // SAFETY: single-threaded startup, before connections or native PTY workers exist.
        // ServerConnection construction only resolves paths and captures this nonsecret name.
        unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", first) };
        let a = ServerConnection::for_identity(IDENTITY);
        // SAFETY: the same single-threaded startup boundary still holds.
        unsafe { std::env::set_var("SPACETERM_DEVELOPMENT_CLIENT", second) };
        let b = ServerConnection::for_identity(IDENTITY);
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

    fn takeover(claimed: Claimed, name: Option<&str>) -> Result<()> {
        let Claimed::Takeover(takeover) = claimed else {
            return Err("ended Pane did not retain its Takeover Screen".into());
        };
        if !takeover.session_ended {
            return Err("ended Pane did not retain its Takeover Screen".into());
        }
        match (takeover.controller, name) {
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

    fn restored(claimed: Claimed) -> Result<()> {
        if !matches!(claimed, Claimed::Restored(restored) if restored.session_ended) {
            return Err("controller did not observe an ended retained Pane".into());
        }
        Ok(())
    }

    /// End All Sessions ends the channel of each Pane whose Session it ended, so the channel takes
    /// no more input.
    fn stale(pane: &Pane) -> Result<()> {
        if !matches!(pane.ending()?, Ending::Pane(PaneEvent::SessionEnded))
            || !matches!(pane.paste(1, "STALE_557"), Err(ServerError::StaleControl))
        {
            return Err("ended Session accepted an old lease or input".into());
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
        let a = first.link()?;
        let initial = inventory(&a)?.0;
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
            &a,
            Mutation::CreateWorkspace {
                name: name.clone(),
                directory: Some(directory.clone()),
            },
        )?;
        println!(
            "Prepared disposable Workspace {name:?}. On failure preserve it for inspection; this helper never stops the service."
        );
        let prepared = inventory(&a)?.0;
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
            &a,
            Mutation::SplitPane {
                workspace,
                tab,
                source: root,
                axis: Axis::Right,
            },
        )?;
        let unclaimed = mutate(
            &first,
            &a,
            Mutation::SplitPane {
                workspace,
                tab,
                source: root,
                axis: Axis::Down,
            },
        )?;
        let mut panes = Vec::new();
        for (index, pane) in [root, other, unclaimed].into_iter().enumerate() {
            let claimed = support::select(
                &a,
                pane,
                true,
                PaneSize {
                    columns: 80,
                    rows: 24,
                    ..size()
                },
            )?;
            let started = attached(claimed)?;
            let marker = format!("END_ALL_557_{stamp}_{index}");
            started.command(&format!(
                "stty -echo; printf '\\033[2J\\033[H{marker}\\n'; while :; do sleep 60; done"
            ))?;
            started.await_output(&marker)?;
            panes.push((started, marker));
        }
        let b = second.link()?;
        let transferred = attached(support::claim(
            &b,
            other,
            AttachmentClaim::TakeOver {
                target: panes[1].0.lease().into(),
            },
            size(),
        )?)?;
        let markers: Vec<_> = panes.iter().map(|(_, marker)| marker.clone()).collect();
        let first_lease = panes[0].0.lease();
        let (released, _) = panes.remove(2);
        released.release();
        b.close(); // Its grant survives this deliberate disconnect until the actual UI action.
        drop(transferred);
        let before = inventory(&a)?.0;
        let boot = before.close.as_ref().ok_or("live boot unavailable")?.boot;
        println!(
            "READY: three marked PTYs; controllers {first_name:?}, disconnected {second_name:?}, and nobody. Invoke End All Sessions in Preflight Settings within five minutes."
        );
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let current = inventory(&a)?.0;
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
        restored(select(&a, root, false)?)?;
        takeover(select(&a, other, false)?, Some(&second_name))?;
        takeover(select(&a, unclaimed, false)?, None)?;
        let b = second.link()?;
        restored(select(&b, other, false)?)?;
        takeover(select(&b, root, false)?, Some(&first_name))?;
        // The second client's channel ended with its connection, so only the first client's
        // channels still name ended leases.
        stale(&panes[0].0)?;
        stale(&panes[1].0)?;
        std::thread::sleep(Duration::from_millis(500));
        if inventory(&a)?
            .0
            .close
            .as_ref()
            .is_none_or(|close| close.sessions.iter().any(|p| p.session.is_some()))
        {
            return Err("passive reconciliation started a shell".into());
        }
        let (ended, _) = panes.remove(0);
        let fresh = attached(select(&a, root, true)?)?;
        if fresh.lease().session == first_lease.session
            || fresh.lease().control == first_lease.control
        {
            return Err("explicit restart reused an ended Session identity".into());
        }
        stale(&ended)?;
        fresh.command("stty -echo; stty size; printf 'FRESH_557\\n'")?;
        let output = fresh.await_output("FRESH_557")?;
        if !output.lines().any(|line| line.trim_end() == "31 97")
            || markers.iter().any(|marker| output.contains(marker))
        {
            return Err("restart did not use measured dimensions and cleared output".into());
        }
        // Read the whole fresh screen and Scrollback as a client caches it.
        let screen = fresh
            .await_screen(Duration::from_secs(10), |screen| screen.frame.is_some())?
            .ok_or("fresh screen did not arrive")?;
        if fresh
            .scrollback(screen.history)?
            .iter()
            .chain(&screen.rows)
            .any(|row| row.contains(&format!("END_ALL_557_{stamp}")))
        {
            return Err("ended output survived in Scrollback".into());
        }
        if fs::read(directory.join("retained-file"))? != b"repository-file-witness" {
            return Err("End All Sessions changed a repository-file witness".into());
        }
        let current = inventory(&a)?.0;
        let target = CloseTarget::Workspace { workspace };
        let confirmation = current
            .close_confirmation(target)
            .ok_or("fixture cleanup confirmation unavailable")?;
        mutate(
            &first,
            &a,
            Mutation::Close {
                target,
                confirmation,
            },
        )?;
        if inventory(&a)?
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
