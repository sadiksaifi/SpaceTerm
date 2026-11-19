//! A client prepares retained Panes in the background: their first Sessions start with no client
//! in control, once per Pane, and a later selection acquires them.
use super::*;
use crate::runtime::core_host::tests::{host, size, workspace_inventory};
use spaceterm_core::attachments::Attachment;
use spaceterm_core::pane::AttachmentClaim;

/// A restarted server presenting the initial Workspace's Root Pane, retained without a Session.
fn restarted() -> (WorkspaceHost, Id, Id) {
    let mut original = WorkspaceHost::new(host());
    original.prepare_initial_organization().unwrap();
    let organization: Organization =
        serde_json::from_slice(&serde_json::to_vec(&original.state.envelope()).unwrap()).unwrap();
    original.shutdown().unwrap();
    let mut restored = WorkspaceHost::new(host());
    restored.restore_organization(Some(&organization));
    let inventory = workspace_inventory(&mut restored);
    let workspace = &inventory.workspaces[0];
    (restored, workspace.id, workspace.tabs[0].root_pane)
}

fn claim(host: &mut WorkspaceHost, device: u8, workspace: Id, pane: Id, claim: AttachmentClaim) -> Reply {
    host.request(
        [device; 32],
        [device; 32],
        Request::ClaimPane {
            attachment: Attachment {
                workspace,
                generation: 1,
            },
            pane,
            claim,
            size: size(),
            style: None,
        },
    )
}

fn session(host: &mut WorkspaceHost, pane: Id) -> Option<Id> {
    workspace_inventory(host)
        .close
        .expect("close facts")
        .sessions
        .into_iter()
        .find(|confirmed| confirmed.id == pane)
        .and_then(|confirmed| confirmed.session)
}

#[test]
fn a_prepared_pane_starts_once_without_control_and_a_selection_acquires_it() {
    let (mut host, workspace, pane) = restarted();
    let boot = host.prototype.boot;
    let reply = claim(&mut host, 1, workspace, pane, AttachmentClaim::Prepare { core: boot });
    assert!(
        matches!(
            reply,
            Reply::Takeover {
                controller: spaceterm_core::pane::control::Controller::Nobody,
                ..
            }
        ),
        "a background start grants no control: {reply:?}"
    );
    let started = session(&mut host, pane).expect("the shell started");
    assert!(
        workspace_inventory(&mut host)
            .close
            .unwrap()
            .controls
            .iter()
            .all(|control| control.pane != pane || control.owner != [1; 32]),
        "nobody controls the prepared Pane"
    );

    // Preparing again observes the running shell and starts no second one.
    let again = claim(&mut host, 2, workspace, pane, AttachmentClaim::Prepare { core: boot });
    assert!(matches!(again, Reply::Takeover { .. }), "{again:?}");
    assert_eq!(session(&mut host, pane), Some(started));

    // The person's selection acquires the unclaimed shell.
    let selected = claim(
        &mut host,
        1,
        workspace,
        pane,
        AttachmentClaim::Select { explicit: true },
    );
    let Reply::Attached { lease } = selected else {
        panic!("selection did not acquire the prepared shell: {selected:?}");
    };
    assert_eq!(lease.session, started);
    host.shutdown().unwrap();
}

#[test]
fn a_prepare_from_another_boot_is_stale() {
    let (mut host, workspace, pane) = restarted();
    let reply = claim(&mut host, 1, workspace, pane, AttachmentClaim::Prepare { core: [9; 32] });
    assert!(matches!(reply, Reply::Refused(Refusal::Stale)), "{reply:?}");
    assert_eq!(session(&mut host, pane), None);
    host.shutdown().unwrap();
}
