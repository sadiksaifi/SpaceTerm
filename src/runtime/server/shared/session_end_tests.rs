use super::*;
use crate::runtime::server::tests::{
    await_screen, await_text, command, focus, history_text, host, lines, screen, shared_inventory,
    size,
};
use spaceterm_server::hierarchy::{Axis, Mutation, Outcome};

fn select(host: &mut SharedWorkspaceHost, device: u8, pane: Id, explicit: bool) -> Reply {
    host.request(
        [device; 32],
        [device; 32],
        Request::SelectPane {
            pane,
            explicit,
            size: size(),
            style: None,
        },
    )
}

/// Commits one edit, then ticks as the server does, which ends the work a retained edit deleted.
fn mutate(host: &mut SharedWorkspaceHost, device: u8, sequence: u64, mutation: Mutation) -> Id {
    let reply = host.request(
        [device; 32],
        [device; 32],
        Request::Mutate { sequence, mutation },
    );
    host.tick(Instant::now());
    match reply {
        Reply::Outcome(result) => match result.outcome {
            Outcome::Accepted { target } => target,
            other => panic!("mutation refused: {other:?}"),
        },
        other => panic!("mutation unavailable: {other:?}"),
    }
}

fn end_all(host: &mut SharedWorkspaceHost, device: u8, sequence: u64) {
    let confirmation = shared_inventory(host).close.unwrap();
    let confirmed: Vec<_> = confirmation
        .sessions
        .iter()
        .filter_map(|pane| pane.session)
        .collect();
    mutate(
        host,
        device,
        sequence,
        Mutation::EndAllSessions { confirmation },
    );
    // The edit ends the confirmed Sessions once it is on disk, which a loaded host may finish
    // after the tick.
    let until = Instant::now() + Duration::from_secs(12);
    while shared_inventory(host).close.is_some_and(|close| {
        close.sessions.iter().any(|pane| {
            pane.session
                .is_some_and(|session| confirmed.contains(&session))
        })
    }) {
        assert!(Instant::now() < until, "End All Sessions did not end them");
        std::thread::sleep(Duration::from_millis(10));
        host.tick(Instant::now());
    }
}

/// Chooses Start Session, the only request that starts a shell in an ended Pane.
fn start_ended(host: &mut SharedWorkspaceHost, device: u8, pane: Id) -> Reply {
    let server = host.prototype.boot;
    host.request(
        [device; 32],
        [device; 32],
        Request::StartRestored {
            server,
            pane,
            size: size(),
            style: None,
        },
    )
}

/// Starts the Pane's shell: by first use, or by Start Session once its Session ended.
fn start(host: &mut SharedWorkspaceHost, device: u8, pane: Id) -> Lease {
    let reply = match select(host, device, pane, true) {
        Reply::Restored {
            session_ended: Some(_),
            ..
        } => start_ended(host, device, pane),
        // Another client's grant, or none, holds the ended Pane. Its Start Session takes over.
        Reply::Takeover {
            session_ended: Some(_),
            target,
            ..
        } => host.request(
            [device; 32],
            [device; 32],
            Request::TakeOver {
                target,
                size: size(),
                style: None,
            },
        ),
        reply => reply,
    };
    match reply {
        Reply::Attached { lease } => lease,
        other => panic!("explicit startup unavailable: {other:?}"),
    }
}

#[test]
fn end_all_sessions_missing_original_cannot_end_replacement_work_during_recovery() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    let confirmation = shared_inventory(&mut host).close.unwrap();
    let pending = Mutation::EndAllSessions { confirmation };
    // Client 1 retains this original locally, but its first dispatch never reaches the server.
    mutate(&mut host, 2, 1, pending.clone());
    host.request(
        [2; 32],
        [2; 32],
        Request::TakeOver {
            target: PaneTarget {
                server: old.server,
                session: pane,
            },
            size: size(),
            style: None,
        },
    );
    let replacement = start(&mut host, 2, pane);
    assert!(matches!(
        host.request([1; 32], [1; 32], Request::Resolve { sequence: 1 }),
        Reply::Refused(Refusal::Missing)
    ));
    assert!(
        matches!(host.request([1; 32], [1; 32], Request::Mutate { sequence: 1, mutation: pending }),
        Reply::Outcome(result) if result.outcome == Outcome::StaleConfirmation)
    );
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        Some(replacement.session)
    );
    assert!(
        matches!(select(&mut host, 2, pane, false), Reply::Attached { lease } if lease.session == replacement.session)
    );
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_retains_directory_confirmed_between_admission_and_shutdown() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);
    let confirmation = shared_inventory(&mut host).close.unwrap();
    // Stage the admitted mutation before cleanup, matching the production commit boundary.
    let mut next = host.state.clone();
    next.inventory.close = Some(confirmation.clone());
    next.apply(
        [2; 32],
        1,
        Mutation::EndAllSessions { confirmation },
        &host.prototype.directory,
        |_| true,
    )
    .unwrap();
    host.commit(next).unwrap();
    // The shell confirms its directory after admission. No shared request copies it early.
    let child = host.pane_host(pane).unwrap();
    command(
        child,
        1,
        lease,
        "cd /; printf '\\033]7;file://localhost/\\007\\033]133;A;spaceterm=%s\\007' \"$SPACETERM_PROMPT_OWNER\"\n",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if child
            .pane
            .as_ref()
            .unwrap()
            .runtime
            .metadata_snapshot()
            .and_then(|metadata| metadata.confirmed_directory.clone())
            .is_some_and(|directory| directory.as_ref() == "/")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shell directory was not confirmed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        host.request([2; 32], [2; 32], Request::Resolve { sequence: 1 }),
        Reply::Outcome(_)
    ));
    let saved = shared_inventory(&mut host);
    assert_eq!(
        saved.workspaces[0].tabs[0].panes[0]
            .confirmed_directory
            .as_deref(),
        Some(std::path::Path::new("/"))
    );
    let fresh = start(&mut host, 1, pane);
    command(
        &mut host,
        1,
        fresh,
        "stty -echo; printf '\\nRESTART-DIRECTORY:'; pwd\n",
    );
    await_text(&mut host, fresh, "RESTART-DIRECTORY:/");
    host.shutdown().unwrap();
}

#[test]
fn retry_after_end_all_uses_restoration_directory_instead_of_live_osc7_directory() {
    for confirmed in [false, true] {
        ended_directory_recovery(confirmed, false);
    }
}

#[test]
fn choose_after_end_all_uses_restoration_directory_instead_of_live_osc7_directory() {
    for confirmed in [false, true] {
        ended_directory_recovery(confirmed, true);
    }
}

fn ended_directory_recovery(confirmed: bool, choose: bool) {
    use crate::runtime::server::repositories;
    use crate::runtime::server::repository_tests::{GitFixture, git_facts};

    let git = GitFixture::new();
    let restoration = git.0.join("restoration");
    let live = git.0.join("live");
    std::fs::create_dir(&restoration).unwrap();
    std::fs::create_dir(&live).unwrap();
    let mut prototype = host();
    prototype.directory = if confirmed {
        git.0.clone()
    } else {
        restoration.clone()
    };
    let mut host = SharedWorkspaceHost::new(prototype);
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    if confirmed {
        command(
            &mut host,
            1,
            old,
            &format!(
                "stty -echo; cd '{}'; printf '\\033]7;file://localhost%s\\007\\033]133;A;spaceterm=%s\\007' \"$PWD\" \"$SPACETERM_PROMPT_OWNER\"; printf '\\033[2J\\033[HRESTORATION-CONFIRMED\\n'",
                restoration.display(),
            ),
        );
        await_text(&mut host, old, "RESTORATION-CONFIRMED");
    }
    // Automatic shell integration is disabled. This later directory has no owned marker.
    command(
        &mut host,
        1,
        old,
        &format!(
            "stty -echo; cd '{}'; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; printf '\\033[2J\\033[HLIVE-DIRECTORY\\n'",
            live.display(),
        ),
    );
    await_text(&mut host, old, "LIVE-DIRECTORY");
    let source = if confirmed { &restoration } else { &live };
    let before = git_facts(&mut host, source, |facts| facts.worktrees.is_some());
    let workspace = &before.workspaces[0];
    let tab = &workspace.tabs[0];
    assert_eq!(tab.worktree.as_ref(), Some(&git.0));
    assert_eq!(&repositories::pane_directory(&tab.panes[0]), source);
    assert_eq!(tab.panes[0].presented_directory(), live);
    assert_eq!(
        tab.panes[0].confirmed_directory.as_ref(),
        confirmed.then_some(&restoration)
    );
    end_all(&mut host, 1, 1);
    std::fs::remove_dir(&restoration).unwrap();
    assert!(matches!(
        start_ended(&mut host, 1, pane),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    let Reply::Restored {
        recovery: Some(target),
        directory_unavailable: true,
        ..
    } = select(&mut host, 1, pane, false)
    else {
        panic!("unavailable restoration directory offers recovery");
    };
    assert_eq!(target.original, restoration);
    assert!(
        matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::RepairDirectory {
                    target: target.clone(),
                    replacement: None,
                    size: size(),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::DirectoryUnavailable)
        ),
        "Retry must revalidate the restoration target rather than reject it as stale"
    );
    let missing = git.0.join("missing-replacement");
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::RepairDirectory {
                target: target.clone(),
                replacement: Some(missing.clone()),
                size: size(),
                style: None,
            }
        ),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    assert!(!missing.exists(), "a repair never creates its replacement");
    let mut wrong = target.clone();
    wrong.original = live.clone();
    assert!(
        matches!(
            host.request(
                [1; 32],
                [1; 32],
                Request::RepairDirectory {
                    target: wrong,
                    replacement: Some(live.clone()),
                    size: size(),
                    style: None,
                }
            ),
            Reply::Refused(Refusal::Stale)
        ),
        "live presentation cannot authorize recovery"
    );
    let unavailable = shared_inventory(&mut host);
    assert_eq!(
        &repositories::pane_directory(&unavailable.workspaces[0].tabs[0].panes[0]),
        source
    );
    assert!(unavailable.close.unwrap().sessions[0].session.is_none());
    if !choose {
        std::fs::create_dir(&restoration).unwrap();
    }
    let Reply::Attached { lease: fresh } = host.request(
        [1; 32],
        [1; 32],
        Request::RepairDirectory {
            target,
            replacement: choose.then(|| live.clone()),
            size: size(),
            style: None,
        },
    ) else {
        panic!("recovery should start the same Pane in the validated directory");
    };
    assert_ne!(fresh.session, old.session);
    let directory = if choose { &live } else { &restoration };
    command(
        &mut host,
        1,
        fresh,
        &format!(
            "stty -echo; if [ \"$PWD\" = '{}' ]; then printf '\\033[2J\\033[HRECOVERED-DIRECTORY\\n'; fi",
            directory.display(),
        ),
    );
    await_text(&mut host, fresh, "RECOVERED-DIRECTORY");
    let after = shared_inventory(&mut host);
    assert_eq!(after.workspaces.len(), 1);
    let recovered = &after.workspaces[0];
    assert_eq!(recovered.id, workspace.id);
    assert_eq!(recovered.pinned_directory, workspace.pinned_directory);
    assert_eq!(recovered.root_tab, workspace.root_tab);
    assert_eq!(recovered.tabs.len(), 1);
    assert_eq!(recovered.tabs[0].id, tab.id);
    assert_eq!(recovered.tabs[0].root_pane, pane);
    assert_eq!(recovered.tabs[0].panes.len(), 1);
    assert_eq!(recovered.tabs[0].worktree, tab.worktree);
    assert_eq!(
        recovered.tabs[0].worktree_repository,
        tab.worktree_repository
    );
    assert_eq!(
        recovered.tabs[0].panes[0].confirmed_directory.as_ref(),
        if choose {
            Some(&live)
        } else {
            confirmed.then_some(&restoration)
        }
    );
    host.shutdown().unwrap();
}

/// An ended Session has nothing to take over, so Take Over is the other client's Start Session:
/// it moves the grant and starts a fresh shell in one request.
fn ended_takeover_starts_a_session(attached: bool) {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    host.admitted([1; 32], "Studio".into());
    host.admitted([2; 32], "Laptop".into());
    let inventory = shared_inventory(&mut host);
    let workspace = &inventory.workspaces[0];
    let pane = workspace.tabs[0].root_pane;
    let attachment = spaceterm_server::attachments::Attachment {
        workspace: workspace.id,
        generation: 7,
    };
    let old = start(&mut host, 1, pane);
    end_all(&mut host, 1, 1);
    let Reply::Takeover {
        target,
        session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
        ..
    } = select(&mut host, 2, pane, false)
    else {
        panic!("ended Pane offers takeover");
    };
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        None,
        "observing the ended Pane starts nothing"
    );
    let takeover = if attached {
        Request::ClaimPane {
            attachment,
            pane,
            claim: AttachmentClaim::TakeOver { target },
            size: size(),
            style: None,
        }
    } else {
        Request::TakeOver {
            target,
            size: size(),
            style: None,
        }
    };
    let Reply::Attached { lease: fresh } = host.request([2; 32], [2; 32], takeover) else {
        panic!("Take Over of an ended Pane starts its Session");
    };
    assert_ne!(fresh.session, old.session);
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        Some(fresh.session)
    );
    assert!(
        matches!(select(&mut host, 1, pane, false), Reply::Takeover {
        session_ended: None, controller: control::Controller::Client { name, .. }, ..
    } if name.as_str() == "Laptop")
    );
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_direct_takeover_starts_a_session() {
    ended_takeover_starts_a_session(false);
}

#[test]
fn end_all_sessions_attached_takeover_starts_a_session() {
    ended_takeover_starts_a_session(true);
}

#[test]
fn end_all_sessions_preserves_independent_grants_unclaimed_panes_and_other_servers() {
    let mut selected = SharedWorkspaceHost::new(host());
    let mut unrelated = SharedWorkspaceHost::new(host());
    selected.prepare_initial_organization().unwrap();
    unrelated.prepare_initial_organization().unwrap();
    selected.admitted([1; 32], "Studio".into());
    selected.admitted([2; 32], "Laptop".into());
    let first = shared_inventory(&mut selected).workspaces[0].tabs[0].root_pane;
    let other_pane = shared_inventory(&mut unrelated).workspaces[0].tabs[0].root_pane;
    let other = start(&mut unrelated, 1, other_pane);
    mutate(
        &mut selected,
        2,
        1,
        Mutation::CreateWorkspace {
            name: "Second".into(),
            directory: None,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    );
    let inventory = shared_inventory(&mut selected);
    let second_workspace = &inventory.workspaces[1];
    let second_tab = &second_workspace.tabs[0];
    let second = second_tab.root_pane;
    let unclaimed = mutate(
        &mut selected,
        2,
        2,
        Mutation::SplitPane {
            workspace: second_workspace.id,
            tab: second_tab.id,
            source: second,
            axis: Axis::Right,
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    );
    let first_session = start(&mut selected, 1, first);
    start(&mut selected, 2, second);
    let released = start(&mut selected, 2, unclaimed);
    selected.request([2; 32], [2; 32], Request::Release { lease: released });
    command(
        &mut selected,
        1,
        first_session,
        "printf '\\033]2;END-ALL-LIVE-PRESENTATION\\007'\n",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let before = loop {
        let workspaces = shared_inventory(&mut selected).workspaces;
        if workspaces[0].tabs[0].panes[0]
            .presentation
            .as_ref()
            .is_some_and(|snapshot| snapshot.title == "END-ALL-LIVE-PRESENTATION")
        {
            break workspaces;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shell presentation was not published"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut expected = before;
    for pane in expected
        .iter_mut()
        .flat_map(|workspace| &mut workspace.tabs)
        .flat_map(|tab| &mut tab.panes)
    {
        pane.presentation = None;
    }
    end_all(&mut selected, 3, 1);
    let ended = shared_inventory(&mut selected);
    assert!(
        ended
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .all(|pane| pane.presentation.is_none()),
        "ended Sessions publish no live presentation"
    );
    assert_eq!(ended.workspaces, expected);
    assert!(
        ended
            .close
            .unwrap()
            .sessions
            .iter()
            .all(|p| p.session.is_none())
    );
    for (owner, pane) in [(1, first), (2, second)] {
        assert!(matches!(
            select(&mut selected, owner, pane, false),
            Reply::Restored {
                session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
                ..
            }
        ));
        assert!(matches!(
            select(&mut selected, 3, pane, true),
            Reply::Takeover {
                session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
                ..
            }
        ));
    }
    assert!(matches!(
        select(&mut selected, 3, unclaimed, false),
        Reply::Takeover {
            session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
            controller: control::Controller::Nobody,
            ..
        }
    ));
    start(&mut selected, 3, unclaimed);
    assert_eq!(
        shared_inventory(&mut unrelated).close.unwrap().sessions[0].session,
        Some(other.session)
    );
    selected.shutdown().unwrap();
    unrelated.shutdown().unwrap();
}

#[test]
fn end_all_sessions_replay_and_recovered_cleanup_never_end_replacement_sessions() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let first = start(&mut host, 1, pane);
    let mut originals = Vec::new();
    let mut previous = first;
    for sequence in 1..=3 {
        let mutation = Mutation::EndAllSessions {
            confirmation: shared_inventory(&mut host).close.unwrap(),
        };
        originals.push(mutation.clone());
        mutate(&mut host, 2, sequence, mutation);
        assert!(matches!(
            select(&mut host, 1, pane, false),
            Reply::Restored {
                session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
                ..
            }
        ));
        let replacement = start(&mut host, 1, pane);
        assert_ne!(replacement.session, previous.session);
        // Resolve and replay every retained original after later Sessions have started.
        for old in 1..=sequence {
            assert!(matches!(
                host.request([2; 32], [2; 32], Request::Resolve { sequence: old }),
                Reply::Outcome(_)
            ));
            mutate(&mut host, 2, old, originals[old as usize - 1].clone());
        }
        let record: Organization =
            serde_json::from_slice(&serde_json::to_vec(&host.state.envelope()).unwrap()).unwrap();
        host.restore_organization(Some(&record));
        assert_eq!(
            shared_inventory(&mut host).close.unwrap().sessions[0].session,
            Some(replacement.session)
        );
        previous = replacement;
    }
    for sequence in 1..=3 {
        assert!(matches!(
            host.request([2; 32], [2; 32], Request::Acknowledge { sequence }),
            Reply::Unchanged
        ));
    }
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        Some(previous.session)
    );
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_drops_output_and_fences_old_input_effects_and_snapshots() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    command(
        &mut host,
        1,
        old,
        "stty -echo; printf '\\033[2J\\033[HOLD-SESSION-OUTPUT\\n'\n",
    );
    await_text(&mut host, old, "OLD-SESSION-OUTPUT");
    focus(&mut host, old, 1, FocusTarget::Terminal);
    let Reply::Paste(PasteOutcome::ConfirmationRequired(approval)) = host.request(
        [1; 32],
        [1; 32],
        Request::Input {
            lease: old,
            generation: 1,
            input: InteractiveInput::Paste {
                text: "old\nnever\n".into(),
            },
        },
    ) else {
        panic!("unsafe paste requires approval");
    };
    end_all(&mut host, 2, 1);
    let replacement = start(&mut host, 1, pane);
    assert!(matches!(
        screen(&mut host, [1; 32], old),
        Err(PaneEvent::SessionEnded)
    ));
    let retired = [
        Request::Focus {
            lease: old,
            generation: 1,
            target: FocusTarget::Terminal,
        },
        Request::Input {
            lease: old,
            generation: 1,
            input: InteractiveInput::Paste { text: "old".into() },
        },
        Request::Input {
            lease: old,
            generation: 1,
            input: InteractiveInput::ResolvePaste {
                id: approval.id,
                confirm: true,
            },
        },
        Request::CompleteClipboard {
            lease: old,
            generation: 1,
            id: 1,
            completion: pane::effects::Completion::Text("old".into()),
        },
        Request::Resize {
            lease: old,
            size: size(),
        },
        Request::TakeOver {
            target: old.into(),
            size: size(),
            style: None,
        },
    ];
    for request in retired {
        assert!(matches!(
            host.request([1; 32], [1; 32], request),
            Reply::Refused(Refusal::Stale)
        ));
    }
    // Read the entire fresh screen and Scrollback before any client input can clear it.
    let update = await_screen(&mut host, [1; 32], replacement, |_| true);
    let history = history_text(&mut host, [1; 32], replacement, update.history);
    assert!(
        !history
            .iter()
            .chain(lines(&update).iter())
            .any(|line| line.contains("OLD-SESSION-OUTPUT")),
        "the ended Session cannot survive in screen or Scrollback"
    );
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_directory_repair_requires_the_preserved_controller() {
    let root = std::env::temp_dir().join(format!(
        "spaceterm-end-sessions-{:x}",
        u128::from_ne_bytes(nonce().unwrap()[..16].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let mut host = SharedWorkspaceHost::new(host());
    mutate(
        &mut host,
        1,
        1,
        Mutation::CreateWorkspace {
            name: "Repair".into(),
            directory: Some(root.clone()),
            control: spaceterm_server::hierarchy::CreateControl::Creator,
        },
    );
    let before = shared_inventory(&mut host).workspaces;
    let pane = before[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);
    host.admitted([2; 32], "Laptop".into());
    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None,
            }
        ),
        Reply::Attached { .. }
    ));
    host.disconnected([2; 32]);
    end_all(&mut host, 1, 2);
    std::fs::remove_dir(&root).unwrap();
    assert!(matches!(
        start_ended(&mut host, 2, pane),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    let Reply::Restored {
        recovery: Some(target),
        session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
        ..
    } = select(&mut host, 2, pane, false)
    else {
        panic!("ended Pane retains directory recovery");
    };
    let repair = Request::RepairDirectory {
        target,
        replacement: Some(std::env::temp_dir()),
        size: size(),
        style: None,
    };
    assert!(
        matches!(host.request([1; 32], [1; 32], repair.clone()), Reply::Takeover {
        session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions), controller: control::Controller::Client { name, .. }, ..
    } if name.as_str() == "Laptop")
    );
    let Reply::Attached { lease: repaired } = host.request([2; 32], [2; 32], repair.clone()) else {
        panic!("the preserved controller repairs the Pane");
    };
    assert!(
        matches!(host.request([1; 32], [1; 32], repair), Reply::Takeover { target, .. } if target == repaired.into()),
        "a repair cannot replace the running shell"
    );
    let after = shared_inventory(&mut host).workspaces;
    assert_eq!(
        (after[0].id, after[0].tabs[0].id, after[0].tabs[0].root_pane),
        (
            before[0].id,
            before[0].tabs[0].id,
            before[0].tabs[0].root_pane
        )
    );
    assert_eq!(after[0].pinned_directory, before[0].pinned_directory);
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_retains_the_controller_if_replacement_directory_disappears_during_startup() {
    let root = std::env::temp_dir().join(format!(
        "spaceterm-end-startup-{:x}",
        u128::from_ne_bytes(nonce().unwrap()[..16].try_into().unwrap())
    ));
    std::fs::create_dir(&root).unwrap();
    let directory = local_filesystem().validate_directory(&root).unwrap();
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    host.request(
        [2; 32],
        [2; 32],
        Request::TakeOver {
            target: old.into(),
            size: size(),
            style: None,
        },
    );
    end_all(&mut host, 1, 1);
    // Reproduce the real boundary between host validation and native worker preparation.
    std::fs::remove_dir(&root).unwrap();
    let child = host.pane_host(pane).unwrap();
    child
        .start_directory([2; 32], [2; 32], size(), None, directory)
        .unwrap();
    let _ = child
        .pane
        .as_mut()
        .unwrap()
        .runtime
        .shutdown_and_wait(Duration::from_secs(5));
    assert!(matches!(
        select(&mut host, 2, pane, false),
        Reply::Restored {
            session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
            directory_unavailable: true,
            ..
        }
    ));
    assert!(matches!(
        select(&mut host, 1, pane, true),
        Reply::Takeover {
            session_ended: Some(spaceterm_server::pane::SessionEnd::EndAllSessions),
            ..
        }
    ));
    host.shutdown().unwrap();
}
