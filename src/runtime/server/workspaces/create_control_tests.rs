use super::*;
use crate::runtime::server::tests::{effects, focus, host, size, workspace_inventory};
use spaceterm_server::hierarchy::{CreateControl, Mutation, Outcome};

fn select(host: &mut WorkspaceHost, device: u8, pane: Id) -> Reply {
    host.request(
        [device; 32],
        [device; 32],
        Request::SelectPane {
            pane,
            explicit: true,
            size: size(),
            style: None,
        },
    )
}

fn create_tab(
    host: &mut WorkspaceHost,
    device: u8,
    sequence: u64,
    workspace: Id,
    control: CreateControl,
) -> Id {
    let Reply::Outcome(result) = host.request(
        [device; 32],
        [device; 32],
        Request::Mutate {
            sequence,
            mutation: Mutation::CreateTab {
                workspace,
                source: None,
                control,
            },
        },
    ) else {
        panic!("the edit is answered");
    };
    let Outcome::Accepted { target } = result.outcome else {
        panic!("the Tab is created");
    };
    workspace_inventory(host).placement(target).unwrap().pane
}

/// A CLI inside the desktop's Pane creates a Tab for the desktop: the desktop controls it at
/// once and its origin Pane's channel alone receives the Reveal.
#[test]
fn work_created_for_an_origin_pane_belongs_to_its_controller_and_reveals_there() {
    let mut host = WorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let initial = workspace_inventory(&mut host);
    let workspace = initial.workspaces[0].id;
    let origin = initial.workspaces[0].tabs[0].root_pane;
    let Reply::Attached { lease } = select(&mut host, 1, origin) else {
        panic!("the desktop starts its Pane");
    };
    focus(&mut host, lease, 1, FocusTarget::Terminal);
    let created = create_tab(
        &mut host,
        3,
        1,
        workspace,
        CreateControl::OriginController { pane: origin },
    );
    let placement = workspace_inventory(&mut host).placement(created).unwrap();
    let [request] = effects(&mut host, lease, 1).try_into().unwrap();
    let pane::effects::Effect::Reveal(reveal) = request.effect else {
        panic!("the origin's channel receives a Reveal");
    };
    assert_eq!(
        (reveal.workspace, reveal.tab, reveal.pane),
        (placement.workspace, placement.tab, placement.pane)
    );
    assert!(
        matches!(select(&mut host, 1, created), Reply::Attached { .. }),
        "the origin's controller holds the new Pane"
    );
    host.shutdown().unwrap();
}

/// Created work nobody claimed opens for the first client that selects it, with no Takeover
/// Screen, while work a creator controls offers one to everyone else.
#[test]
fn an_unclaimed_tab_shows_no_takeover_screen() {
    let mut host = WorkspaceHost::new(host());
    host.prepare_initial_organization().unwrap();
    let workspace = workspace_inventory(&mut host).workspaces[0].id;
    let unclaimed = create_tab(&mut host, 3, 1, workspace, CreateControl::Unclaimed);
    assert!(matches!(
        select(&mut host, 1, unclaimed),
        Reply::Attached { .. }
    ));
    let claimed = create_tab(&mut host, 3, 2, workspace, CreateControl::Creator);
    assert!(matches!(
        select(&mut host, 1, claimed),
        Reply::Takeover { .. }
    ));
    host.shutdown().unwrap();
}
