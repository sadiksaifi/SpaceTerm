use super::*;
use crate::runtime::server::tests::{host, size, workspace_inventory};
use spaceterm_server::hierarchy::{CloseTarget, Mutation, Outcome};

#[test]
fn a_direct_close_refuses_a_changed_control_grant_without_ending_the_same_session() {
    for close_tab in [false, true] {
        for take_back in [false, true] {
            let mut host = WorkspaceHost::new(host());
            host.prepare_initial_organization().unwrap();
            let initial = workspace_inventory(&mut host);
            let workspace = &initial.workspaces[0];
            let tab = workspace.root_tab;
            let pane = workspace.tabs[0].root_pane;
            let workspace = workspace.id;
            let Reply::Attached { lease: original } = host.request(
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
                sequence: 1, mutation: Mutation::CreateTab { workspace, source: None, control: spaceterm_server::hierarchy::CreateControl::Creator, },
            }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
            let target = if close_tab {
                CloseTarget::Tab { workspace, tab }
            } else {
                CloseTarget::Pane {
                    workspace,
                    tab,
                    pane,
                }
            };
            let reviewed = workspace_inventory(&mut host);
            let mut confirmation = reviewed.close_confirmation(target).unwrap();
            confirmation.own_controls = Some(
                reviewed
                    .close
                    .as_ref()
                    .unwrap()
                    .controls
                    .iter()
                    .filter(|control| control.pane == pane)
                    .cloned()
                    .collect(),
            );
            assert!(
                matches!(host.request([1; 32], [3; 32], Request::SelectPane {
                pane, explicit: false, size: size(), style: None,
            }), Reply::Attached { lease } if lease.session == original.session)
            );
            assert_eq!(
                workspace_inventory(&mut host).close,
                reviewed.close,
                "resuming the same grant does not invalidate an idle own close"
            );
            for device in if take_back { vec![2, 1] } else { vec![2] } {
                assert!(
                    matches!(host.request([device; 32], [device; 32], Request::TakeOver {
                    target: original.into(), size: size(), style: None,
                }), Reply::Attached { lease } if lease.session == original.session)
                );
            }
            let current = workspace_inventory(&mut host);
            assert_eq!(
                current.close.as_ref().unwrap().sessions,
                reviewed.close.as_ref().unwrap().sessions
            );
            assert_ne!(
                current.close.as_ref().unwrap().controls[0].epoch,
                confirmation.own_controls.as_ref().unwrap()[0].epoch
            );
            assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
                sequence: 2, mutation: Mutation::Close { target, confirmation },
            }), Reply::Outcome(result) if result.outcome == Outcome::StaleConfirmation));
            let after = workspace_inventory(&mut host);
            assert_eq!(
                after.workspaces[0]
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>(),
                current.workspaces[0]
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                after.close, current.close,
                "refusal leaves the other client's Session and control intact"
            );

            // Explicit review can end another client's work.
            let confirmation = after.close_confirmation(target).unwrap();
            assert!(confirmation.own_controls.is_none());
            assert!(matches!(host.request([1; 32], [1; 32], Request::Mutate {
                sequence: 3, mutation: Mutation::Close { target, confirmation },
            }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
            assert_eq!(workspace_inventory(&mut host).workspaces[0].tabs.len(), 1);
            host.shutdown().unwrap();
        }
    }
}

#[test]
fn end_all_sessions_ignores_control_changes_after_its_review() {
    let mut host = WorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let pane = workspace_inventory(&mut host).workspaces[0].tabs[0].root_pane;
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
        panic!("Pane starts");
    };
    let confirmation = workspace_inventory(&mut host).close.unwrap();
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
    assert!(matches!(host.request([3; 32], [3; 32], Request::Mutate {
        sequence: 1, mutation: Mutation::EndAllSessions { confirmation },
    }), Reply::Outcome(result) if matches!(result.outcome, Outcome::Accepted { .. })));
    assert_eq!(
        workspace_inventory(&mut host).close.unwrap().sessions[0].session,
        None
    );
    host.shutdown().unwrap();
}
