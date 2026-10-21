use super::*;
use crate::runtime::server::tests::{await_text, command, focus, host, shared_inventory, size};
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

fn mutate(host: &mut SharedWorkspaceHost, device: u8, sequence: u64, mutation: Mutation) -> Id {
    match host.request(
        [device; 32],
        [device; 32],
        Request::Mutate { sequence, mutation },
    ) {
        Reply::Outcome(result) => match result.outcome {
            Outcome::Accepted { target } => target,
            other => panic!("mutation refused: {other:?}"),
        },
        other => panic!("mutation unavailable: {other:?}"),
    }
}

fn start(host: &mut SharedWorkspaceHost, device: u8, pane: Id) -> Lease {
    match select(host, device, pane, true) {
        Reply::Attached { lease } => lease,
        other => panic!("explicit startup unavailable: {other:?}"),
    }
}

fn ended_takeover_waits_for_explicit_start(attached: bool) {
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
    mutate(
        &mut host,
        1,
        1,
        Mutation::EndAllSessions { server: old.server },
    );
    let Reply::Takeover {
        target,
        session_ended: true,
        ..
    } = select(&mut host, 2, pane, false)
    else {
        panic!("ended Pane offers takeover");
    };
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
    assert!(
        matches!(
            host.request([2; 32], [2; 32], takeover),
            Reply::Restored {
                session_ended: true,
                ..
            }
        ),
        "takeover transfers ended control without starting a Session"
    );
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        None
    );
    assert!(
        matches!(select(&mut host, 1, pane, false), Reply::Takeover {
        session_ended: true, controller: control::Controller::Client { name, .. }, ..
    } if name.as_str() == "Laptop")
    );
    let passive = if attached {
        Request::ClaimPane {
            attachment,
            pane,
            claim: AttachmentClaim::Select { explicit: false },
            size: size(),
            style: None,
        }
    } else {
        Request::SelectPane {
            pane,
            explicit: false,
            size: size(),
            style: None,
        }
    };
    assert!(matches!(
        host.request([2; 32], [2; 32], passive),
        Reply::Restored {
            session_ended: true,
            ..
        }
    ));
    let start = if attached {
        Request::ClaimPane {
            attachment,
            pane,
            claim: AttachmentClaim::Start { server: old.server },
            size: size(),
            style: None,
        }
    } else {
        Request::StartRestored {
            server: old.server,
            pane,
            size: size(),
            style: None,
        }
    };
    let Reply::Attached { lease: fresh } = host.request([2; 32], [2; 32], start) else {
        panic!("new controller explicitly starts the Session");
    };
    assert_ne!(fresh.session, old.session);
    assert_eq!(
        shared_inventory(&mut host).close.unwrap().sessions[0].session,
        Some(fresh.session)
    );
    host.shutdown().unwrap();
}

#[test]
fn end_all_sessions_direct_takeover_changes_control_without_starting() {
    ended_takeover_waits_for_explicit_start(false);
}

#[test]
fn end_all_sessions_attached_takeover_changes_control_without_starting() {
    ended_takeover_waits_for_explicit_start(true);
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
        },
    );
    let first_lease = start(&mut selected, 1, first);
    start(&mut selected, 2, second);
    let released = start(&mut selected, 2, unclaimed);
    selected.request([2; 32], [2; 32], Request::Release { lease: released });
    let before = shared_inventory(&mut selected).workspaces;
    mutate(
        &mut selected,
        3,
        1,
        Mutation::EndAllSessions {
            server: first_lease.server,
        },
    );
    assert_eq!(shared_inventory(&mut selected).workspaces, before);
    assert!(
        shared_inventory(&mut selected)
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
                session_ended: true,
                ..
            }
        ));
        assert!(matches!(
            select(&mut selected, 3, pane, true),
            Reply::Takeover {
                session_ended: true,
                ..
            }
        ));
    }
    assert!(matches!(
        select(&mut selected, 3, unclaimed, false),
        Reply::Takeover {
            session_ended: true,
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
    let mutation = Mutation::EndAllSessions {
        server: first.server,
    };
    let mut previous = first;
    for sequence in 1..=3 {
        mutate(&mut host, 2, sequence, mutation.clone());
        assert!(matches!(
            select(&mut host, 1, pane, false),
            Reply::Restored {
                session_ended: true,
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
            mutate(&mut host, 2, old, mutation.clone());
        }
        let record: Organization = serde_json::from_slice(
            &serde_json::to_vec(&host.state.envelope(&host.prototype.directory).unwrap()).unwrap(),
        )
        .unwrap();
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
    await_text(&mut host, 1, old, "OLD-SESSION-OUTPUT");
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
    mutate(
        &mut host,
        2,
        1,
        Mutation::EndAllSessions { server: old.server },
    );
    let replacement = start(&mut host, 1, pane);
    let retired = [
        Request::Poll { lease: old },
        Request::PollChanges {
            lease: old,
            base: Some(1),
        },
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
        Request::NativeEffects {
            lease: old,
            generation: 1,
        },
        Request::CompleteClipboard {
            lease: old,
            generation: 1,
            id: 1,
            completion: pane::effects::Completion::Text("old".into()),
        },
        Request::SelectionChunk {
            lease: old,
            generation: 1,
            offset: 0,
        },
        Request::ReadingStatus { lease: old },
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
    // Search the entire fresh screen and Scrollback before any client input can clear it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        host.request(
            [1; 32],
            [1; 32],
            Request::Read {
                lease: replacement,
                operation: pane::reading::Operation::Query {
                    generation: 7,
                    text: "OLD-SESSION-OUTPUT".into(),
                },
            },
        );
        if let Reply::Screen(screen) =
            host.request([1; 32], [1; 32], Request::Poll { lease: replacement })
            && let Some(find) = screen
                .reading
                .as_ref()
                .and_then(|reading| reading.find.as_ref())
            && find.generation == 7
        {
            assert_eq!(
                find.total, 0,
                "the ended Session cannot survive in screen or Scrollback"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fresh Find did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
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
    mutate(
        &mut host,
        1,
        2,
        Mutation::EndAllSessions {
            server: lease.server,
        },
    );
    std::fs::remove_dir(&root).unwrap();
    assert!(matches!(
        select(&mut host, 2, pane, true),
        Reply::Refused(Refusal::DirectoryUnavailable)
    ));
    let Reply::Restored {
        recovery: Some(target),
        session_ended: true,
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
        session_ended: true, controller: control::Controller::Client { name, .. }, ..
    } if name.as_str() == "Laptop")
    );
    assert!(matches!(
        host.request([2; 32], [2; 32], repair),
        Reply::Attached { .. }
    ));
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
    mutate(
        &mut host,
        1,
        1,
        Mutation::EndAllSessions { server: old.server },
    );
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
            session_ended: true,
            directory_unavailable: true,
            ..
        }
    ));
    assert!(matches!(
        select(&mut host, 1, pane, true),
        Reply::Takeover {
            session_ended: true,
            ..
        }
    ));
    host.shutdown().unwrap();
}
