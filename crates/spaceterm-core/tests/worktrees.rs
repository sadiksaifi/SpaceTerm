mod support;

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::organization::*;
use spaceterm_core::server_directories::DirectoryContainment;
use spaceterm_core::worktrees::assessment::*;
use spaceterm_core::worktrees::*;

fn snapshot(workspaces: u64) -> OrganizationSnapshot {
    let mut state = support::organization();
    let server = state.server;
    let repository = RepositoryId::new(server, 1).unwrap();
    state.repositories.push(RepositoryRecord {
        id: repository,
        directory: directory(b"/repo"),
    });
    state.worktrees = vec![
        WorktreeRecord {
            id: WorktreeId::new(server, 1).unwrap(),
            repository,
            directory: directory(b"/repo"),
            main: true,
            missing: false,
        },
        WorktreeRecord {
            id: WorktreeId::new(server, 2).unwrap(),
            repository,
            directory: directory(b"/linked"),
            main: false,
            missing: false,
        },
    ];
    state.tabs[0].worktree = Some(state.worktrees[1].id);
    for id in 2..=workspaces {
        let mut workspace = state.workspaces[0].clone();
        workspace.id = WorkspaceId::new(server, id).unwrap();
        workspace.root_tab = TabId::new(server, id).unwrap();
        workspace.creation_ordinal = CreationOrdinal::new(id * 3 - 2);
        let mut tab = state.tabs[0].clone();
        tab.id = workspace.root_tab;
        tab.workspace = workspace.id;
        tab.root_pane = PaneId::new(server, id).unwrap();
        tab.creation_ordinal = CreationOrdinal::new(id * 3 - 1);
        let mut pane = state.panes[0].clone();
        pane.id = tab.root_pane;
        pane.tab = tab.id;
        pane.creation_ordinal = CreationOrdinal::new(id * 3);
        state.workspaces.push(workspace);
        state.tabs.push(tab);
        state.panes.push(pane);
    }
    let runtime = state
        .panes
        .iter()
        .map(|pane| PaneRuntimeRecord {
            pane: pane.id,
            session_generation: SessionGeneration::new(0),
            phase: SessionPhase::NotStarted,
            control_generation: ControlGeneration::new(0),
            control: ControlRecord::Unclaimed,
        })
        .collect();
    OrganizationSnapshot::new(ServerEpoch::new(1), state, runtime).unwrap()
}
fn directory(bytes: &[u8]) -> ServerDirectory {
    ServerDirectory::new(support::server(), bytes.to_vec()).unwrap()
}
fn inputs(
    snapshot: &OrganizationSnapshot,
) -> (RuntimeWorkSet, WorktreeRemovalFacts, AffectedWorktreeWork) {
    let runtime = RuntimeWorkSet {
        panes: snapshot
            .runtime()
            .iter()
            .map(|pane| RuntimeWorkStamp {
                pane: pane.pane,
                session: None,
                context_generation: ContextGeneration::new(1),
                work_generation: CloseWorkGeneration::new(1),
                startup_reserved: false,
                confirmed_directory: None,
            })
            .collect(),
    };
    let facts = WorktreeRemovalFacts {
        target: snapshot.state().worktrees[1].clone(),
        listed: true,
        locked: false,
        head: None,
        changes: false,
        submodules: false,
    };
    let affected = AffectedWorktreeWork {
        pinned: vec![],
        panes: snapshot
            .state()
            .panes
            .iter()
            .map(|pane| PaneDirectoryContainment {
                pane: pane.id,
                directory: pane.restoration_directory().clone(),
                containment: DirectoryContainment::Outside,
            })
            .collect(),
        fallbacks: vec![FallbackWorktree {
            record: snapshot.state().worktrees[0].clone(),
            bare: false,
            containment: DirectoryContainment::Outside,
            available: true,
        }],
    };
    (runtime, facts, affected)
}

#[test]
fn removal_scope_includes_unattached_workspaces() {
    let snapshot = snapshot(2);
    let (runtime, target, affected) = inputs(&snapshot);
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    assert_eq!(scope.retained().parts().tabs.len(), 2);
    assert_eq!(scope.retained().parts().fallbacks.len(), 2);
}

#[test]
fn removal_scope_includes_confirmed_directory_work_outside_saved_tag() {
    let original = snapshot(2);
    let mut state = original.state().clone();
    state.tabs[1].worktree = Some(state.worktrees[0].id);
    let snapshot =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    let (runtime, target, mut affected) = inputs(&snapshot);
    affected.panes[1].containment = DirectoryContainment::Inside;
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected)
            .unwrap()
            .retained()
            .parts()
            .tabs
            .len(),
        2
    );
}

#[test]
fn removal_rejects_pin_without_worktree_listing() {
    let original = snapshot(2);
    let mut state = original.state().clone();
    state.workspaces[1].pinned_directory.value = Some(directory(b"/alias"));
    let snapshot =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    let (runtime, target, mut affected) = inputs(&snapshot);
    affected.pinned.push(PinDirectoryContainment {
        workspace: snapshot.state().workspaces[1].id,
        directory: directory(b"/alias"),
        version: FieldVersion::new(1),
        containment: DirectoryContainment::Inside,
    });
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected),
        Err(GitFailure::HoldsPinnedDirectory)
    );
    affected.pinned.clear();
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected),
        Err(GitFailure::Unchecked)
    );
}

#[test]
fn unrelated_tab_order_does_not_change_scope_or_roots() {
    let original = snapshot(2);
    let (runtime, target, affected) = inputs(&original);
    let scope = assess_removal(&original, &runtime, &target, &affected).unwrap();
    let mut state = original.state().clone();
    state.tabs.reverse();
    state.panes.reverse();
    state.workspaces.reverse();
    state.revision = OrganizationRevision::new(900);
    let reordered =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    assert_eq!(
        revalidate_removal(&scope, &reordered, &runtime, &affected),
        Ok(())
    );
}

#[test]
fn session_replacement_and_work_generation_invalidate_confirmation() {
    let snapshot = snapshot(1);
    let (mut runtime, target, affected) = inputs(&snapshot);
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    runtime.panes[0].work_generation = CloseWorkGeneration::new(2);
    assert_eq!(
        revalidate_removal(&scope, &snapshot, &runtime, &affected),
        Err(GitFailure::StaleAssessment)
    );
}

#[test]
fn removal_plans_nonstarting_fallback_for_final_worktree_tabs() {
    let snapshot = snapshot(1);
    let (runtime, target, affected) = inputs(&snapshot);
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    assert_eq!(
        scope.retained().parts().fallbacks[0].worktree,
        snapshot.state().worktrees[0].id
    );
}

#[test]
fn bare_repository_without_fallback_refuses_before_effects() {
    let snapshot = snapshot(1);
    let (runtime, target, mut affected) = inputs(&snapshot);
    affected.fallbacks[0].bare = true;
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected),
        Err(GitFailure::NoFallback)
    );
}

#[test]
fn unknown_containment_is_not_outside() {
    let snapshot = snapshot(1);
    let (runtime, target, mut affected) = inputs(&snapshot);
    affected.panes[0].containment = DirectoryContainment::Unchecked;
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected),
        Err(GitFailure::Unchecked)
    );
}

#[test]
fn force_is_derived_only_from_confirmed_risks() {
    let snapshot = snapshot(1);
    let (runtime, mut target, affected) = inputs(&snapshot);
    target.submodules = true;
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    assert_eq!(scope.approval(), ApprovedRemoval::DeleteSubmoduleCheckouts);
    assert!(scope.approval().require_clean());
    target.changes = true;
    assert_eq!(
        revalidate_git_facts(&scope, &target),
        Err(GitFailure::GitStateChanged)
    );
}

#[test]
fn inventory_keeps_missing_and_unlisted_referenced_ids() {
    use spaceterm_core::worktrees::reconcile::*;
    let snapshot = snapshot(1);
    let inventory = ValidatedWorktreeInventory {
        repository: snapshot.state().repositories[0].id,
        records: vec![ValidatedInventoryEntry {
            retained: Some(snapshot.state().worktrees[0].id),
            directory: directory(b"/repo"),
            main: true,
            missing: false,
        }],
    };
    let plan = plan_inventory_reconciliation(&snapshot, &inventory).unwrap();
    assert!(plan.writes.iter().any(|write| matches!(write, OrganizationWrite::PutWorktree(w) if w.id == snapshot.state().worktrees[1].id && w.missing)));
    assert!(
        plan.writes
            .iter()
            .all(|write| !matches!(write, OrganizationWrite::DeleteWorktree(_)))
    );
}

#[test]
fn forgotten_worktree_id_is_never_reused() {
    use spaceterm_core::worktrees::reconcile::*;
    let snapshot = snapshot(1);
    let inventory = ValidatedWorktreeInventory {
        repository: snapshot.state().repositories[0].id,
        records: vec![ValidatedInventoryEntry {
            retained: Some(WorktreeId::new(support::server(), 3).unwrap()),
            directory: directory(b"/replacement"),
            main: false,
            missing: false,
        }],
    };
    assert_eq!(
        plan_inventory_reconciliation(&snapshot, &inventory),
        Err(GitFailure::IdentityReplaced)
    );
}

#[test]
fn new_affected_tab_invalidates_confirmation() {
    let original = snapshot(1);
    let (runtime, target, affected) = inputs(&original);
    let scope = assess_removal(&original, &runtime, &target, &affected).unwrap();
    let expanded = snapshot(2);
    let (runtime, _, affected) = inputs(&expanded);
    assert_eq!(
        revalidate_removal(&scope, &expanded, &runtime, &affected),
        Err(GitFailure::StaleAssessment)
    );
}

#[test]
fn assessment_scope_bounds_plus_one_never_truncate() {
    let snapshot = snapshot(MAX_GIT_TABS as u64 + 1);
    let (runtime, target, affected) = inputs(&snapshot);
    assert_eq!(
        assess_removal(&snapshot, &runtime, &target, &affected),
        Err(GitFailure::Capacity)
    );
}

#[test]
fn intent_recovery_rejects_phase_disagreement_and_completed_identity() {
    use spaceterm_core::operations::*;
    use spaceterm_core::storage::StoreFailure;
    use spaceterm_core::worktrees::reconcile::*;
    struct Ledger {
        reference: GitIntentReference,
        completed: bool,
    }
    impl GitRecoveryLedger for Ledger {
        fn pending_git_progress(
            &self,
            _: OperationBinding,
        ) -> Result<Option<GitIntentReference>, StoreFailure> {
            Ok(Some(self.reference))
        }
        fn is_awaiting_git_intent(&self, _: OperationBinding) -> Result<bool, StoreFailure> {
            Ok(false)
        }
        fn has_completed_git_result(&self, _: OperationId) -> Result<bool, StoreFailure> {
            Ok(self.completed)
        }
    }
    let binding = OperationBinding {
        id: support::operation(1),
        digest: RequestDigest::for_command(&support::create_command()).unwrap(),
    };
    let old = IdentityHighWater {
        worktree: 1,
        repository: 1,
        pairing: 1,
        client: 1,
        operation_stream: 1,
        ..IdentityHighWater::default()
    };
    let marks = IdentityHighWater { worktree: 2, ..old };
    let repository = RepositoryLocator::new(RepositoryLocatorParts {
        scope: RepositoryScope {
            server: support::server(),
            repository: RepositoryId::new(support::server(), 1).unwrap(),
        },
        main_root: directory(b"/repo"),
        common_directory: directory(b"/repo/.git"),
    })
    .unwrap();
    let allocation = GitAllocationReservation::new(GitAllocationReservationParts {
        binding,
        expected_high_water: old,
        resulting_high_water: marks,
        repository: None,
        worktree: Some(WorktreeId::new(support::server(), 2).unwrap()),
        fallbacks: vec![],
    })
    .unwrap();
    let intent = GitWriteIntent::new(GitWriteIntentParts {
        binding,
        repository: repository.clone(),
        action: RetainedGitAction::Create {
            worktree: allocation.parts().worktree.unwrap(),
            directory: directory(b"/new"),
            branch: WorktreeBranchChoice::Existing {
                name: BranchName::new("topic".into()).unwrap(),
            },
            target: CreationTargetObservation::Absent,
            template_version: None,
        },
        phase: GitIntentPhase::Started,
        assessment_digest: GitAssessmentDigest::from_bytes([2; 32]),
        affected: RetainedRemovalScope::empty(support::server()),
        allocation: binding,
        external_result: None,
    })
    .unwrap();
    let mut ledger = Ledger {
        reference: GitIntentReference::new(binding, GitIntentPhase::Prepared),
        completed: false,
    };
    let validate = |ledger: &Ledger| {
        validate_git_recovery(
            support::server(),
            marks,
            std::slice::from_ref(&repository),
            std::slice::from_ref(&intent),
            std::slice::from_ref(&allocation),
            ledger,
        )
    };
    assert_eq!(validate(&ledger), Err(StoreFailure::InvalidDurableState));
    ledger.reference = intent.reference();
    assert_eq!(validate(&ledger), Ok(()));
    ledger.completed = true;
    assert_eq!(validate(&ledger), Err(StoreFailure::InvalidDurableState));
}
