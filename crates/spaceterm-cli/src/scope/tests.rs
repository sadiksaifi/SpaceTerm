use super::fixtures::{fixture, id};
use super::*;

fn args(
    workspace: Option<&str>,
    worktree: Option<&str>,
    tab: Option<&str>,
    pane: Option<&str>,
) -> ScopeArgs {
    ScopeArgs {
        workspace: workspace.map(|s| Selector::parse(s).unwrap()),
        worktree: worktree.map(|s| Selector::parse(s).unwrap()),
        tab: tab.map(|s| Selector::parse(s).unwrap()),
        pane: pane.map(|s| Selector::parse(s).unwrap()),
    }
}

#[test]
fn explicit_children_search_the_server_and_infer_their_ancestors() {
    let snapshot = fixture();
    let result = resolve(
        &snapshot,
        &args(None, None, None, Some("Editor")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.workspace, Some(id(2)));
    assert_eq!(result.tab, Some(id(13)));
    assert_eq!(result.pane, Some(id(23)));
    assert_eq!(result.sources.workspace, Some(Source::Flag));
}

#[test]
fn duplicate_labels_narrow_to_the_explicit_worktree() {
    let result = resolve(
        &fixture(),
        &args(None, Some("feature/x"), None, Some("cargo test")),
        None,
        Want::Pane,
        Effect::Mutating,
    )
    .unwrap();
    assert_eq!(result.pane, Some(id(22)));
    assert_eq!(result.worktree, Some("/trees/feature".into()));
}

#[test]
fn narrowing_never_restarts_the_handle_search() {
    let mut snapshot = fixture();
    snapshot.inventory.workspaces[1].tabs[0].panes[0]
        .presentation
        .as_mut()
        .unwrap()
        .title = "cargo test again".into();
    let error = resolve(
        &snapshot,
        &args(Some("tools"), None, None, Some("cargo test")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap_err();
    assert!(
        matches!(error, TargetError::Ambiguous { level: Level::Pane, candidates } if candidates.is_empty())
    );
}

#[test]
fn disagreeing_explicit_scope_reports_the_actual_parent() {
    let error = resolve(
        &fixture(),
        &args(Some("api"), None, None, Some("Editor")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap_err();
    assert!(
        matches!(error, TargetError::Mismatch { actual, .. } if actual.level == Level::Workspace && actual.id == Some(resolution::hex(&id(2))))
    );
}

#[test]
fn a_branch_with_slashes_is_not_a_path() {
    let snapshot = fixture();
    let result = resolve(
        &snapshot,
        &args(None, Some("feature/x"), None, None),
        None,
        Want::Worktree,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.worktree, Some("/trees/feature".into()));
    assert!(matches!(
        resolve(
            &snapshot,
            &args(None, Some("path:feature/x"), None, None),
            None,
            Want::Worktree,
            Effect::Passive
        ),
        Err(TargetError::NotFound {
            level: Level::Worktree
        })
    ));
    assert!(
        resolve(
            &snapshot,
            &args(None, Some("path:/trees/empty"), None, None),
            None,
            Want::Worktree,
            Effect::Passive
        )
        .is_ok()
    );
}

#[test]
fn current_requires_a_matching_server_and_surviving_pane() {
    for current in [
        None,
        Some(Current {
            server: id(7),
            pane: id(21),
        }),
        Some(Current {
            server: id(9),
            pane: id(99),
        }),
    ] {
        assert_eq!(
            resolve(
                &fixture(),
                &args(None, None, None, Some("current")),
                current.as_ref(),
                Want::Pane,
                Effect::Passive
            ),
            Err(TargetError::ContextUnavailable)
        );
    }
}

#[test]
fn current_fills_untouched_levels_for_tab_creation_in_its_worktree() {
    let current = Current {
        server: id(9),
        pane: id(22),
    };
    let result = resolve(
        &fixture(),
        &args(Some("api"), None, None, None),
        Some(&current),
        Want::Workspace,
        Effect::Mutating,
    )
    .unwrap();
    assert_eq!(result.worktree, Some("/trees/feature".into()));
    assert_eq!(result.pane, Some(id(22)));
    assert_eq!(result.sources.workspace, Some(Source::Flag));
    assert_eq!(result.sources.worktree, Some(Source::Current));
}

#[test]
fn an_explicit_child_blocks_current_ancestors_but_not_a_singleton() {
    let current = Current {
        server: id(9),
        pane: id(22),
    };
    let result = resolve(
        &fixture(),
        &args(None, None, Some("Editor"), None),
        Some(&current),
        Want::Pane,
        Effect::Mutating,
    )
    .unwrap();
    assert_eq!(result.workspace, Some(id(2)));
    assert_eq!(result.pane, Some(id(23)));
    assert_eq!(result.sources.pane, Some(Source::Only));
}

#[test]
fn destructive_targets_reject_fuzzy_and_bare_id_handles() {
    let snapshot = fixture();
    for handle in ["editor", "Edit", "171717"] {
        assert!(matches!(
            resolve(
                &snapshot,
                &args(None, None, None, Some(handle)),
                None,
                Want::Pane,
                Effect::Confirmed
            ),
            Err(TargetError::NotFound { level: Level::Pane })
        ));
    }
    for handle in ["Editor", "id:171717"] {
        assert!(
            resolve(
                &snapshot,
                &args(None, None, None, Some(handle)),
                None,
                Want::Pane,
                Effect::Confirmed
            )
            .is_ok()
        );
    }
}

#[test]
fn destructive_target_never_fills_from_a_single_candidate() {
    let snapshot = fixture();
    assert_eq!(
        resolve(
            &snapshot,
            &args(Some("tools"), None, None, None),
            None,
            Want::Pane,
            Effect::Confirmed
        ),
        Err(TargetError::TargetRequired { level: Level::Pane })
    );
    assert_eq!(
        resolve(
            &snapshot,
            &ScopeArgs::default(),
            None,
            Want::Workspace,
            Effect::Confirmed
        ),
        Err(TargetError::TargetRequired {
            level: Level::Workspace
        })
    );
    let current = Current {
        server: id(9),
        pane: id(23),
    };
    assert!(
        resolve(
            &snapshot,
            &ScopeArgs::default(),
            Some(&current),
            Want::Pane,
            Effect::Confirmed
        )
        .is_ok()
    );
}

#[test]
fn ambiguous_current_and_duplicate_labels_report_operational_candidates() {
    let error = resolve(
        &fixture(),
        &args(None, None, None, Some("cargo test")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap_err();
    let (failure, data) = error.into_parts();
    let bounded = serde_json::to_string(&crate::contract::ErrorBody::from(failure)).unwrap();
    assert!(!bounded.contains("cargo test"));
    assert!(!bounded.contains("/repo"));
    let TargetData::Candidates(data) = data else {
        panic!("expected candidates")
    };
    assert_eq!(data.candidates.len(), 2);
    assert!(
        data.candidates
            .iter()
            .all(|c| c.label.starts_with("cargo test"))
    );
}

#[test]
fn exact_handles_win_before_substrings_and_id_fallbacks() {
    let mut snapshot = fixture();
    snapshot.inventory.workspaces[0].tabs[0].panes[0]
        .presentation
        .as_mut()
        .unwrap()
        .title = "Editor tools".into();
    let result = resolve(
        &snapshot,
        &args(None, None, None, Some("Editor")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.pane, Some(id(23)));
    let result = resolve(
        &snapshot,
        &args(Some("API"), None, None, Some("id:151515")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.pane, Some(id(21)));
}

#[test]
fn server_resolution_without_selectors_keeps_the_whole_tree() {
    let current = Current {
        server: id(9),
        pane: id(22),
    };
    let result = resolve(
        &fixture(),
        &ScopeArgs::default(),
        Some(&current),
        Want::Server,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.workspace, None);
    assert_eq!(result.pane, None);
}

#[test]
fn reserved_main_handle_yields_to_a_branch_with_that_literal_name() {
    let mut snapshot = fixture();
    let tree = &mut snapshot.repositories.as_mut().unwrap().directories[0]
        .worktrees
        .as_mut()
        .unwrap()
        .worktrees[1];
    tree.head = spaceterm_server::repository::WorktreeHead::Branch("main-worktree".into());
    let result = resolve(
        &snapshot,
        &args(None, Some("main-worktree"), None, None),
        None,
        Want::Worktree,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.worktree, Some("/trees/feature".into()));
}

#[test]
fn a_nonrepository_tab_cannot_satisfy_an_explicit_worktree() {
    let snapshot = fixture();
    let error = resolve(
        &snapshot,
        &args(None, Some("feature/x"), Some("Editor"), None),
        None,
        Want::Tab,
        Effect::Passive,
    )
    .unwrap_err();
    let (failure, _) = error.into_parts();
    assert_eq!(
        failure.text(),
        "error: The Tab is not in the given Worktree. (scope_mismatch)"
    );
}

#[test]
fn ambiguous_parent_labels_can_be_narrowed_by_an_explicit_child() {
    let mut snapshot = fixture();
    snapshot.inventory.workspaces[1].name = "api".into();
    let result = resolve(
        &snapshot,
        &args(Some("api"), None, None, Some("Editor")),
        None,
        Want::Pane,
        Effect::Passive,
    )
    .unwrap();
    assert_eq!(result.workspace, Some(id(2)));
}
