//! A close that asks nobody first rechecks the exact Terminal Session's activity when SpaceTerm
//! Core admits it, after every input it accepted before.
use super::*;
use crate::runtime::core_host::tests::{command, host, size, workspace_inventory};
use spaceterm_core::hierarchy::{CloseTarget, Mutation, Outcome};

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
