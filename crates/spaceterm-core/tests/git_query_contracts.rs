use spaceterm_core::{authority::*, identity::*, operations::RepositoryScope, worktrees::*};

fn server() -> ServerId {
    ServerId::new([7; 16])
}

#[test]
fn status_targets_require_no_repository_and_keep_worktrees_distinct() {
    let targets = [
        GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap()),
        GitStatusTarget::Workspace(WorkspaceId::new(server(), 1).unwrap()),
        GitStatusTarget::Worktree(WorktreeId::new(server(), 1).unwrap()),
        GitStatusTarget::Worktree(WorktreeId::new(server(), 2).unwrap()),
    ];
    for target in targets {
        let query = GitQuery::Status { target };
        assert_eq!(query.server(), server());
        assert_eq!(query.validate_scope(server()), Ok(()));
        assert_eq!(
            query.validate_scope(ServerId::new([8; 16])),
            Err(GitFailure::WrongServer)
        );
    }
    assert_ne!(targets[2], targets[3]);
}

#[test]
fn workspace_inventory_is_not_a_repository_global_query() {
    let query = GitQuery::Inventory {
        workspace: WorkspaceId::new(server(), 2).unwrap(),
        cursor: None,
    };
    assert_eq!(query.validate_scope(server()), Ok(()));
    assert_eq!(
        query.validate_scope(ServerId::new([8; 16])),
        Err(GitFailure::WrongServer)
    );
}

#[test]
fn repository_pages_reject_a_cursor_from_a_different_repository() {
    let repository = RepositoryScope {
        server: server(),
        repository: RepositoryId::new(server(), 1).unwrap(),
    };
    let cursor = GitPageCursor::new(GitPageCursorParts {
        scope: RepositoryScope {
            repository: RepositoryId::new(server(), 2).unwrap(),
            ..repository
        },
        epoch: ServerEpoch::new(1),
        observation: 1,
        random: [1; 32],
    })
    .unwrap();
    assert_eq!(
        GitQuery::Branches {
            repository,
            cursor: Some(cursor)
        }
        .validate_scope(server()),
        Err(GitFailure::StaleAssessment)
    );
}

#[test]
fn git_cursor_checks_binding_and_nonzero_comparison_fields() {
    let parts = GitPageCursorParts {
        scope: RepositoryScope {
            server: server(),
            repository: RepositoryId::new(server(), 1).unwrap(),
        },
        epoch: ServerEpoch::new(1),
        observation: 1,
        random: [1; 32],
    };
    let mut wrong = parts.clone();
    wrong.scope.server = ServerId::new([8; 16]);
    assert_eq!(GitPageCursor::new(wrong), Err(GitFailure::WrongServer));
    for wrong in [
        GitPageCursorParts {
            epoch: ServerEpoch::new(0),
            ..parts.clone()
        },
        GitPageCursorParts {
            observation: 0,
            ..parts.clone()
        },
        GitPageCursorParts {
            random: [0; 32],
            ..parts
        },
    ] {
        assert_eq!(GitPageCursor::new(wrong), Err(GitFailure::InvalidResponse));
    }
}
