//! A close that asks nobody first rechecks the exact Terminal Session's activity when SpaceTerm
//! Core admits it, after every input it accepted before.
use super::*;
use crate::runtime::core_host::tests::{
    await_text, command, host, host_with_shell, size, workspace_inventory,
};
use spaceterm_core::hierarchy::{CloseTarget, Foreground, Mutation, Outcome};

/// A started own Root Pane beside a second Tab, and the direct close of that Pane.
fn own_pane(host: &mut WorkspaceHost) -> (Lease, CloseTarget) {
    host.prepare_initial_organization().unwrap();
    let initial = workspace_inventory(host);
    let workspace = &initial.workspaces[0];
    let (workspace, tab, pane) = (
        workspace.id,
        workspace.root_tab,
        workspace.tabs[0].root_pane,
    );
    let Reply::Attached { lease } = host.request(
        [1; 32],
        [1; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("own Pane starts");
    };
    assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
        sequence: 1,
        mutation: Mutation::CreateTab {
            workspace,
            source: None,
            control: spaceterm_core::hierarchy::CreateControl::Creator,
        },
    }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
    (
        lease,
        CloseTarget::Pane {
            workspace,
            tab,
            pane,
        },
    )
}

fn direct_close(host: &mut WorkspaceHost, target: CloseTarget) -> Outcome {
    let reviewed = workspace_inventory(host);
    let mut confirmation = reviewed.close_confirmation(target).unwrap();
    let closed: Vec<Id> = confirmation.panes.iter().map(|pane| pane.id).collect();
    confirmation.own_controls = Some(
        reviewed
            .close
            .as_ref()
            .unwrap()
            .controls
            .iter()
            .filter(|control| closed.contains(&control.pane))
            .cloned()
            .collect(),
    );
    let Reply::Outcome(result) = host.request(
        [1; 32],
        [1; 32],
        Request::Mutate {
            sequence: 2,
            mutation: Mutation::Close {
                target,
                confirmation,
            },
        },
    ) else {
        panic!("the close was answered");
    };
    result.outcome
}

#[test]
fn an_own_shell_with_nothing_running_closes_directly() {
    let mut host = WorkspaceHost::new(host());
    let (_, target) = own_pane(&mut host);
    assert!(matches!(
        direct_close(&mut host, target),
        Outcome::Accepted { .. }
    ));
    host.shutdown().unwrap();
}

#[test]
fn a_direct_close_right_after_enter_asks_for_review_and_ends_nothing() {
    let mut host = WorkspaceHost::new(host());
    let (lease, target) = own_pane(&mut host);
    // The person runs a command and closes the Pane before any screen shows it running.
    assert!(matches!(
        command(&mut host, 1, lease, "sleep 30"),
        Reply::Unchanged
    ));
    assert_eq!(direct_close(&mut host, target), Outcome::StaleConfirmation);
    assert_eq!(
        workspace_inventory(&mut host).workspaces[0].tabs.len(),
        2,
        "the Pane and its command stay"
    );
    host.shutdown().unwrap();
}

#[test]
fn inventory_publishes_a_foreground_program_without_shell_integration() {
    check_foreground_without_shell_integration("exec /bin/sleep 30", Foreground::Program);
}

#[test]
fn inventory_reports_ambiguous_foreground_when_a_job_shares_the_shell_group() {
    check_foreground_without_shell_integration("set +m; /bin/sleep 10", Foreground::Unknown);
}

fn check_foreground_without_shell_integration(line: &str, expected: Foreground) {
    use spaceterm_core::protocol::{CoreMessage, RawFrame, frame::Assembler, version::WORK};

    let mut host = WorkspaceHost::new(host_with_shell(
        "/bin/bash",
        native_pty_factory(Arc::default()),
    ));
    let (lease, target) = own_pane(&mut host);
    let observer = [2; 32];
    let outbox = Outbox::new(Arc::new(|| {}), WORK);
    HostCommands::connected(&mut host, observer, observer, &outbox);
    let mut assembler = Assembler::default();
    let mut wait = |host: &mut WorkspaceHost, foreground, needs_review| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            HostCommands::tick(host, Instant::now());
            while let Some((frame, _)) = outbox.next_frame() {
                let frame = RawFrame::parse(&frame).unwrap();
                let Some(CoreMessage::Inventory(revision)) = assembler.core(frame).unwrap() else {
                    continue;
                };
                let pane = &revision.inventory.workspaces[0].tabs[0].panes[0];
                if pane.activity.is_some_and(|activity| {
                    activity.session == Some(lease.session)
                        && activity.foreground == foreground
                        && activity.needs_close_review == needs_review
                }) {
                    assert!(
                        pane.presentation
                            .as_ref()
                            .is_none_or(|pane| pane.command.is_none())
                    );
                    return revision.inventory;
                }
            }
            assert!(
                Instant::now() < deadline,
                "inventory never reported {foreground:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    assert_eq!(
        wait(&mut host, Foreground::Shell, false).close_would_end_work(target),
        Some(false)
    );
    assert!(matches!(
        command(&mut host, 1, lease, line),
        Reply::Unchanged
    ));
    assert_eq!(
        wait(&mut host, expected, true).close_would_end_work(target),
        Some(true)
    );
    assert_eq!(direct_close(&mut host, target), Outcome::StaleConfirmation);
    host.shutdown().unwrap();
}

#[test]
fn an_idle_close_of_an_unclaimed_pane_checks_for_work_before_ending_the_session() {
    let mut host = WorkspaceHost::new(host());
    let (first_lease, target) = own_pane(&mut host);
    let CloseTarget::Pane { pane, .. } = target else {
        unreachable!()
    };
    host.request([1; 32], [1; 32], Request::Release { lease: first_lease });
    let reviewed = workspace_inventory(&mut host);
    let confirmation = reviewed.close_confirmation(target).unwrap();
    assert!(
        !reviewed
            .close
            .unwrap()
            .controls
            .iter()
            .any(|control| control.pane == pane)
    );

    let Reply::Attached { lease } = host.request(
        [1; 32],
        [2; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    ) else {
        panic!("the other client takes control")
    };
    assert!(matches!(
        command(
            &mut host,
            2,
            lease,
            "printf '%s%s\\n' TASK- RUNNING; sleep 30"
        ),
        Reply::Unchanged
    ));
    await_text(&mut host, lease, "TASK-RUNNING");
    host.request([1; 32], [2; 32], Request::Release { lease });
    let current = workspace_inventory(&mut host);
    assert_eq!(
        current.close_confirmation(target),
        Some(confirmation.clone())
    );
    assert!(
        !current
            .close
            .unwrap()
            .controls
            .iter()
            .any(|control| control.pane == pane)
    );

    let request_close = |host: &mut WorkspaceHost, sequence, mutation| {
        let Reply::Outcome(result) =
            host.request([3; 32], [3; 32], Request::Mutate { sequence, mutation })
        else {
            panic!("the close was answered")
        };
        result.outcome
    };
    let idle = Mutation::close_if_idle(target, confirmation.clone());
    assert_eq!(
        request_close(&mut host, 1, idle),
        Outcome::StaleConfirmation
    );
    assert_eq!(
        request_close(
            &mut host,
            2,
            Mutation::Close {
                target,
                confirmation
            }
        ),
        Outcome::Accepted { target: pane }
    );
    host.shutdown().unwrap();
}

#[test]
fn an_unclaimed_idle_pane_closes_without_a_control_grant() {
    let mut host = WorkspaceHost::new(host());
    let (lease, target) = own_pane(&mut host);
    let CloseTarget::Pane { pane, .. } = target else {
        unreachable!()
    };
    host.request([1; 32], [1; 32], Request::Release { lease });
    let confirmation = workspace_inventory(&mut host)
        .close_confirmation(target)
        .unwrap();
    let Reply::Outcome(result) = host.request(
        [2; 32],
        [2; 32],
        Request::Mutate {
            sequence: 1,
            mutation: Mutation::close_if_idle(target, confirmation),
        },
    ) else {
        panic!("the close was answered")
    };
    assert_eq!(result.outcome, Outcome::Accepted { target: pane });
    host.shutdown().unwrap();
}
