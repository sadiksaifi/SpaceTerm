//! A shell that exits with code zero closes its Pane. Any other exit keeps the Pane, which every
//! client presents as Session Ended until someone starts a fresh Session or closes it.
use super::*;
use crate::runtime::server::tests::{await_text, command, host, screen, shared_inventory, size};
use spaceterm_server::hierarchy::{Axis, CloseTarget, Mutation, Outcome};
use spaceterm_server::pane::SessionEnd;

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

fn start(host: &mut SharedWorkspaceHost, device: u8, pane: Id) -> Lease {
    match select(host, device, pane, true) {
        Reply::Attached { lease } => lease,
        other => panic!("explicit startup unavailable: {other:?}"),
    }
}

/// Splits the initial Workspace's Root Pane and returns the Tab and both Panes.
fn split(host: &mut SharedWorkspaceHost) -> (Id, Id, Id, Id) {
    host.prepare_initial_organization().unwrap();
    let inventory = shared_inventory(host);
    let workspace = &inventory.workspaces[0];
    let tab = &workspace.tabs[0];
    let reply = host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::SplitPane {
                workspace: workspace.id,
                tab: tab.id,
                source: tab.root_pane,
                axis: Axis::Right,
                control: spaceterm_server::hierarchy::CreateControl::Creator,
            },
        },
    );
    let Reply::Outcome(result) = reply else {
        panic!("split unavailable: {reply:?}");
    };
    let Outcome::Accepted { target } = result.outcome else {
        panic!("split refused: {:?}", result.outcome);
    };
    (workspace.id, tab.id, tab.root_pane, target)
}

/// Runs `line` in the shell behind `lease` and returns its channel's last event once it exits.
fn exit_with(host: &mut SharedWorkspaceHost, device: u8, lease: Lease, line: &str) -> PaneEvent {
    command(host, device, lease, line);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        host.tick(Instant::now());
        if let Err(event) = screen(host, [device; 32], lease) {
            return event;
        }
        assert!(Instant::now() < deadline, "the shell did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn panes(host: &mut SharedWorkspaceHost) -> Vec<Id> {
    shared_inventory(host)
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .flat_map(|t| &t.panes)
        .map(|p| p.id)
        .collect()
}

fn session(host: &mut SharedWorkspaceHost, pane: Id) -> Option<Id> {
    shared_inventory(host)
        .close
        .unwrap()
        .sessions
        .iter()
        .find(|p| p.id == pane)
        .unwrap()
        .session
}

#[test]
fn a_zero_exit_closes_only_its_pane_even_without_a_connected_client() {
    let mut host = SharedWorkspaceHost::new(host());
    let (_, tab, root, second) = split(&mut host);
    let lease = start(&mut host, 1, second);

    command(&mut host, 1, lease, "exit 0");
    host.disconnected([1; 32]);
    // Only the host's own ticks run. No client observes the Pane.
    let deadline = Instant::now() + Duration::from_secs(15);
    while panes(&mut host) != vec![root] {
        assert!(Instant::now() < deadline, "the exited Pane did not close");
        host.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(shared_inventory(&mut host).workspaces[0].tabs[0].id, tab);
    assert!(matches!(
        screen(&mut host, [1; 32], lease),
        Err(PaneEvent::Ended)
    ));
    host.shutdown().unwrap();
}

#[test]
fn a_nonzero_exit_keeps_the_last_pane_and_says_why_to_every_client() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    host.admitted([1; 32], "Studio".into());
    let before = shared_inventory(&mut host).workspaces;
    let pane = before[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);

    assert!(matches!(
        exit_with(&mut host, 1, lease, "exit 3"),
        PaneEvent::SessionEnded
    ));
    host.tick(Instant::now());

    let after = shared_inventory(&mut host);
    assert_eq!(after.workspaces.len(), 1, "the Workspace stays");
    assert_eq!(after.workspaces[0].tabs.len(), 1, "the Tab stays");
    assert_eq!(panes(&mut host), vec![pane], "the Pane stays");
    assert_eq!(session(&mut host, pane), None, "no shell runs");
    let ended = Some(SessionEnd::Exited { code: 3 });
    assert!(
        matches!(select(&mut host, 1, pane, false), Reply::Restored {
            session_ended, fresh: false, ..
        } if session_ended == ended),
        "the controlling client presents Session Ended"
    );
    assert!(
        matches!(select(&mut host, 2, pane, false), Reply::Takeover {
            session_ended, controller: control::Controller::Client { name, .. }, ..
        } if session_ended == ended && name.as_str() == "Studio"),
        "another client learns why the Session ended"
    );
    assert_eq!(session(&mut host, pane), None, "observing starts nothing");
    host.shutdown().unwrap();
}

#[test]
fn a_signal_keeps_the_pane() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);

    assert!(matches!(
        exit_with(&mut host, 1, lease, "kill -KILL $$"),
        PaneEvent::SessionEnded
    ));

    assert!(matches!(
        select(&mut host, 1, pane, false),
        Reply::Restored {
            session_ended: Some(SessionEnd::Signaled),
            ..
        }
    ));
    host.shutdown().unwrap();
}

#[test]
fn start_session_replaces_an_exited_shell_for_its_controller() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    exit_with(&mut host, 1, old, "exit 1");
    let Reply::Restored { server, .. } = select(&mut host, 1, pane, false) else {
        panic!("the ended Pane waits");
    };

    let Reply::Attached { lease: fresh } = host.request(
        [1; 32],
        [1; 32],
        Request::StartRestored {
            server,
            pane,
            size: size(),
            style: None,
        },
    ) else {
        panic!("Start Session starts a fresh shell");
    };

    assert_ne!(fresh.session, old.session);
    assert_eq!(session(&mut host, pane), Some(fresh.session));
    command(
        &mut host,
        1,
        fresh,
        "stty -echo; printf '\\033[2J\\033[HFRESH\\n'",
    );
    await_text(&mut host, fresh, "FRESH");
    host.shutdown().unwrap();
}

#[test]
fn start_session_from_another_client_takes_over_the_exited_pane() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    host.admitted([2; 32], "Laptop".into());
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let old = start(&mut host, 1, pane);
    exit_with(&mut host, 1, old, "exit 2");
    let Reply::Takeover { target, .. } = select(&mut host, 2, pane, false) else {
        panic!("another client holds the grant");
    };

    let Reply::Attached { lease: fresh } = host.request(
        [2; 32],
        [2; 32],
        Request::TakeOver {
            target,
            size: size(),
            style: None,
        },
    ) else {
        panic!("Take Over of an ended Pane starts its Session");
    };

    assert_ne!(fresh.session, old.session);
    assert!(
        matches!(select(&mut host, 1, pane, false), Reply::Takeover {
            session_ended: None, controller: control::Controller::Client { name, .. }, ..
        } if name.as_str() == "Laptop"),
        "the former controller sees the new shell's controller"
    );
    host.shutdown().unwrap();
}

#[test]
fn close_pane_from_another_client_removes_an_exited_pane() {
    let mut host = SharedWorkspaceHost::new(host());
    let (workspace, tab, root, second) = split(&mut host);
    let lease = start(&mut host, 1, second);
    exit_with(&mut host, 1, lease, "exit 1");
    host.tick(Instant::now());
    let target = CloseTarget::Pane {
        workspace,
        tab,
        pane: second,
    };
    let confirmation = shared_inventory(&mut host)
        .close_confirmation(target)
        .unwrap();
    assert!(!confirmation.removes_tab);

    assert!(matches!(host.request([2; 32], [2; 32], Request::Mutate {
        sequence: 1, mutation: Mutation::Close { target, confirmation },
    }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
    host.tick(Instant::now());

    assert_eq!(panes(&mut host), vec![root]);
    assert!(
        matches!(
            select(&mut host, 1, second, false),
            Reply::Refused(Refusal::Missing)
        ),
        "nothing of the closed Pane remains"
    );
    host.shutdown().unwrap();
}

/// The exit's reason belongs to the running server. A restart ended every Session, so the Pane
/// waits for first use like every other retained Pane.
#[test]
fn a_server_restart_keeps_the_exited_pane_but_not_its_exit() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);
    exit_with(&mut host, 1, lease, "exit 4");
    host.tick(Instant::now());
    let record: Organization = serde_json::from_slice(
        &serde_json::to_vec(&host.state.envelope()).unwrap(),
    )
    .unwrap();
    host.shutdown().unwrap();

    let mut restarted = SharedWorkspaceHost::new(super::super::tests::host());
    restarted.restore_organization(Some(&record));

    assert_eq!(panes(&mut restarted), vec![pane]);
    assert!(matches!(
        select(&mut restarted, 1, pane, false),
        Reply::Restored {
            fresh: false,
            session_ended: None,
            ..
        }
    ));
    assert!(
        matches!(
            select(&mut restarted, 1, pane, true),
            Reply::Attached { .. }
        ),
        "after a restart the retained Pane starts on first use"
    );
    restarted.shutdown().unwrap();
}

/// Switching back to an ended Pane or opening its Tab selects it explicitly. That keeps the
/// Session Ended screen and its cause on every client. Only Start Session starts a shell.
#[test]
fn selecting_an_ended_pane_keeps_its_cause_until_start_session() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let workspace = shared_inventory(&mut host).workspaces[0].id;
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);
    exit_with(&mut host, 1, lease, "exit 5");
    let ended = Some(SessionEnd::Exited { code: 5 });

    for _ in 0..2 {
        assert!(
            matches!(select(&mut host, 1, pane, true), Reply::Restored {
                session_ended, ..
            } if session_ended == ended),
            "the controlling client's selection keeps Session Ended"
        );
        assert!(
            matches!(select(&mut host, 2, pane, true), Reply::Takeover {
                session_ended, ..
            } if session_ended == ended),
            "another client's selection keeps Session Ended"
        );
    }
    let attached = Request::ClaimPane {
        attachment: spaceterm_server::attachments::Attachment {
            workspace,
            generation: 1,
        },
        pane,
        claim: AttachmentClaim::Select { explicit: true },
        size: size(),
        style: None,
    };
    assert!(matches!(
        host.request([1; 32], [1; 32], attached),
        Reply::Restored { session_ended, .. } if session_ended == ended
    ));
    assert_eq!(
        session(&mut host, pane),
        None,
        "no selection started a shell"
    );

    let Reply::Restored { server, .. } = select(&mut host, 1, pane, false) else {
        panic!("the ended Pane waits");
    };
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::StartRestored {
                server,
                pane,
                size: size(),
                style: None,
            },
        ),
        Reply::Attached { .. }
    ));
    host.shutdown().unwrap();
}

/// End All Sessions leaves the same Session Ended screen, which selection keeps too.
#[test]
fn selecting_a_pane_end_all_sessions_ended_starts_nothing() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    start(&mut host, 1, pane);
    let confirmation = shared_inventory(&mut host).close.unwrap();
    assert!(matches!(
        host.request(
            [1; 32],
            [1; 32],
            Request::Mutate {
                sequence: 1,
                mutation: Mutation::EndAllSessions { confirmation },
            },
        ),
        Reply::Outcome(_)
    ));
    host.tick(Instant::now());

    assert!(matches!(
        select(&mut host, 1, pane, true),
        Reply::Restored {
            session_ended: Some(SessionEnd::EndAllSessions),
            ..
        }
    ));
    assert_eq!(session(&mut host, pane), None);
    host.shutdown().unwrap();
}

/// Another client's Take Over can reach the Pane after its shell exited and before the server
/// noticed. It answers Stale, so that client claims again and sees Session Ended.
#[test]
fn take_over_racing_a_shell_exit_leads_to_session_ended() {
    let mut host = SharedWorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = shared_inventory(&mut host).workspaces[0].tabs[0].root_pane;
    let lease = start(&mut host, 1, pane);
    command(&mut host, 1, lease, "exit 1");
    // Wait for the exit without draining it, as no tick or observation has run yet.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !host.panes[&pane]
        .pane
        .as_ref()
        .unwrap()
        .subscription
        .events
        .is_closed()
    {
        assert!(Instant::now() < deadline, "the shell did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None,
            },
        ),
        Reply::Refused(Refusal::Stale)
    ));
    assert!(matches!(
        select(&mut host, 2, pane, false),
        Reply::Takeover {
            session_ended: Some(SessionEnd::Exited { code: 1 }),
            ..
        }
    ));
    assert!(matches!(
        host.request(
            [2; 32],
            [2; 32],
            Request::TakeOver {
                target: lease.into(),
                size: size(),
                style: None,
            },
        ),
        Reply::Refused(Refusal::Stale)
    ));
    host.shutdown().unwrap();
}
