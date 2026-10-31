use super::*;
use spaceterm_server::hierarchy::CloseTarget;

#[test]
fn a_worktree_fallback_survives_an_exact_close_and_never_removes_files() {
    let git = GitFixture::new();
    let feature = git.0.join("linked");
    git.git(&[
        "worktree",
        "add",
        "-b",
        "feature",
        feature.to_str().unwrap(),
    ]);
    let mut prototype = host();
    prototype.directory = git.0.clone();
    let mut server = WorkspaceHost::new(prototype);
    let workspace = create_workspace(&mut server, 1);
    let initial = workspace_inventory(&mut server).workspaces[0].root_tab;
    let open = |path| Operation::OpenTab {
        workspace,
        source: None,
        root: git.0.clone(),
        path, control: spaceterm_server::hierarchy::CreateControl::Creator,
    };
    let Outcome::Accepted { target: anchor } = finish_write(&mut server, 2, open(feature.clone()))
    else {
        panic!("feature Tab accepted");
    };
    let target = CloseTarget::Tab {
        workspace,
        tab: initial,
    };
    let confirmation = workspace_inventory(&mut server)
        .close_confirmation(target)
        .unwrap();
    assert!(matches!(server.request([1; 32], [1; 32], Request::Mutate {
        sequence: 3, mutation: Mutation::Close { target, confirmation },
    }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. })));

    let target = CloseTarget::Worktree {
        workspace,
        tab: anchor,
    };
    assert!(
        workspace_inventory(&mut server)
            .close_confirmation(target)
            .unwrap()
            .removes_workspace,
        "before fallback membership, the target includes every remaining Tab"
    );
    let Outcome::Accepted { target: fallback } = finish_write(&mut server, 4, open(git.0.clone()))
    else {
        panic!("Main Worktree fallback accepted");
    };
    let old = workspace_inventory(&mut server)
        .close_confirmation(target)
        .unwrap();
    assert!(!old.removes_workspace);
    assert!(matches!(
        finish_write(&mut server, 5, open(feature.clone())),
        Outcome::Accepted { .. }
    ));
    assert!(matches!(server.request([1; 32], [1; 32], Request::Mutate {
        sequence: 6, mutation: Mutation::Close { target, confirmation: old },
    }), Reply::Outcome(record) if record.outcome == Outcome::StaleConfirmation));

    let current = workspace_inventory(&mut server);
    let confirmation = current.close_confirmation(target).unwrap();
    assert_eq!(
        confirmation.panes.len(),
        2,
        "fresh consent includes both current target Tabs"
    );
    assert!(matches!(server.request([1; 32], [1; 32], Request::Mutate {
        sequence: 7, mutation: Mutation::Close { target, confirmation },
    }), Reply::Outcome(record) if matches!(record.outcome, Outcome::Accepted { .. })));
    let remaining = workspace_inventory(&mut server);
    let tabs = &remaining.workspaces[0].tabs;
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].id, fallback);
    assert_eq!(tabs[0].worktree.as_ref(), Some(&git.0));
    assert!(feature.is_dir());
    git.git(&["show-ref", "--verify", "refs/heads/feature"]);
    server.shutdown().unwrap();
}
