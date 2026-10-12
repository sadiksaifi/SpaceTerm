mod support;

use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::organization::*;
use spaceterm_core::organization_retained::*;
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
    versioned_snapshot(ServerEpoch::new(1), state, runtime)
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
    let original = snapshot(1);
    let mut records = original.runtime().to_vec();
    records[0].session_generation = SessionGeneration::new(1);
    records[0].phase = SessionPhase::Running {
        generation: SessionGeneration::new(1),
    };
    let snapshot = versioned_snapshot(original.epoch(), original.state().clone(), records);
    let (mut runtime, target, affected) = inputs(&snapshot);
    runtime.panes[0].session = Some(SessionKey {
        epoch: snapshot.epoch(),
        pane: snapshot.state().panes[0].id,
        generation: SessionGeneration::new(1),
    });
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    runtime.panes[0].work_generation = CloseWorkGeneration::new(2);
    assert_eq!(
        revalidate_removal(&scope, &snapshot, &runtime, &affected),
        Err(GitFailure::StaleAssessment)
    );
    runtime.panes[0].work_generation = CloseWorkGeneration::new(1);
    let mut records = snapshot.runtime().to_vec();
    records[0].session_generation = SessionGeneration::new(2);
    records[0].phase = SessionPhase::Running {
        generation: SessionGeneration::new(2),
    };
    let replacement = versioned_snapshot(snapshot.epoch(), snapshot.state().clone(), records);
    runtime.panes[0].session.as_mut().unwrap().generation = SessionGeneration::new(2);
    assert_eq!(
        revalidate_removal(&scope, &replacement, &runtime, &affected),
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

#[test]
fn durable_removal_intent_preserves_directory_contained_tabs_and_generations() {
    use spaceterm_core::operations::*;
    let original = snapshot(2);
    let mut state = original.state().clone();
    state.tabs[1].worktree = Some(state.worktrees[0].id);
    let snapshot =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    let (runtime, target, mut affected) = inputs(&snapshot);
    affected.panes[1].containment = DirectoryContainment::Inside;
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    let binding = OperationBinding {
        id: support::operation(1),
        digest: RequestDigest::for_command(&support::create_command()).unwrap(),
    };
    let intent = GitWriteIntent::new(GitWriteIntentParts {
        binding,
        repository: RepositoryLocator::new(RepositoryLocatorParts {
            scope: RepositoryScope {
                server: support::server(),
                repository: target.target.repository,
            },
            main_root: directory(b"/repo"),
            common_directory: directory(b"/repo/.git"),
        })
        .unwrap(),
        action: RetainedGitAction::Remove {
            worktree: target.target.id,
            directory: target.target.directory,
            head: target.head,
            approval: scope.approval(),
        },
        phase: GitIntentPhase::Prepared,
        assessment_digest: GitAssessmentDigest::from_bytes([2; 32]),
        affected: scope.retained().clone(),
        allocation: binding,
        external_result: None,
    })
    .unwrap();
    let recovered: GitWriteIntent =
        serde_json::from_slice(&serde_json::to_vec(&intent).unwrap()).unwrap();
    assert_eq!(recovered.parts().affected, *scope.retained());
    assert_eq!(
        recovered.parts().affected.parts().tabs[1].worktree,
        Some(snapshot.state().worktrees[0].id)
    );
    let mut changed = runtime.clone();
    changed.panes[1].context_generation = ContextGeneration::new(2);
    assert_eq!(
        revalidate_removal(&scope, &snapshot, &changed, &affected),
        Err(GitFailure::StaleAssessment)
    );
    let mut changed = snapshot.state().clone();
    changed.tabs[1].worktree = None;
    let changed =
        OrganizationSnapshot::new(snapshot.epoch(), changed, snapshot.runtime().to_vec()).unwrap();
    assert_eq!(
        revalidate_removal(&scope, &changed, &runtime, &affected),
        Err(GitFailure::StaleAssessment)
    );
}

#[test]
fn retained_removal_scope_rejects_invalid_or_mixed_session_incarnations() {
    let snapshot = snapshot(2);
    let (runtime, target, affected) = inputs(&snapshot);
    let scope = assess_removal(&snapshot, &runtime, &target, &affected).unwrap();
    for epochs in [[0, 1], [1, 2]] {
        let mut parts = scope.retained().parts().clone();
        for (pane, epoch) in parts.panes.iter_mut().zip(epochs) {
            pane.hierarchy.session_generation = SessionGeneration::new(1);
            pane.session = Some(SessionKey {
                epoch: ServerEpoch::new(epoch),
                pane: pane.hierarchy.pane,
                generation: SessionGeneration::new(1),
            });
        }
        assert_eq!(
            RetainedRemovalScope::new(parts),
            Err(GitFailure::InvalidResponse)
        );
    }
    let mut parts = scope.retained().parts().clone();
    parts.panes[0].session = Some(SessionKey {
        epoch: snapshot.epoch(),
        pane: parts.panes[0].hierarchy.pane,
        generation: SessionGeneration::new(0),
    });
    assert_eq!(
        RetainedRemovalScope::new(parts),
        Err(GitFailure::InvalidResponse)
    );
}

fn versioned_snapshot(
    epoch: ServerEpoch,
    mut state: OrganizationState,
    runtime: Vec<PaneRuntimeRecord>,
) -> OrganizationSnapshot {
    use spaceterm_core::remote_access::RemoteAccessPolicy;
    let mut facts = state
        .retained
        .as_ref()
        .map(|v| v.parts().clone())
        .unwrap_or_else(|| OrganizationRetainedParts {
            server: state.server,
            workspace_names: state
                .workspaces
                .iter()
                .map(|w| {
                    WorkspaceNameBasis::new(
                        w.id,
                        w.name.value.display.clone(),
                        w.name.value.display.clone(),
                    )
                })
                .collect(),
            creation_defaults: state
                .tabs
                .iter()
                .map(|t| TabCreationDefault::new(t.id, vec![t.root_pane]).unwrap())
                .collect(),
            pane_generations: vec![],
            tab_membership_versions: state
                .tabs
                .iter()
                .map(|t| TabMembershipVersion::new(t.id, FieldVersion::new(1)))
                .collect(),
            repository_inventory_versions: state
                .repositories
                .iter()
                .map(|r| RepositoryInventoryVersion::new(r.id, FieldVersion::new(1)))
                .collect(),
            worktree_inventory: state
                .worktrees
                .iter()
                .map(|w| WorktreeInventoryFacts::new(w.id, true, FieldVersion::new(1)))
                .collect(),
            remote_access: Versioned {
                value: RemoteAccessPolicy::disabled(state.server),
                version: FieldVersion::new(1),
            },
        });
    facts.pane_generations = runtime
        .iter()
        .map(|r| PaneGenerationHighWater::new(r.pane, r.session_generation, r.control_generation))
        .collect();
    state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    OrganizationSnapshot::new(epoch, state, runtime).unwrap()
}

#[test]
fn removal_rejects_affected_membership_aba_but_allows_unrelated_membership_edits() {
    use spaceterm_core::operations::*;
    use spaceterm_core::reducer::*;
    let initial = snapshot(3);
    let mut state = initial.state().clone();
    state.settings.worktree_location.value =
        WorktreeLocationTemplate::new("~/.worktrees/{repository}/{branch}".into()).unwrap();
    for workspace in &mut state.workspaces {
        workspace.name.value.display =
            NameText::new(format!("work-{}", workspace.id.local())).unwrap();
    }
    state.retained = None;
    // Tab 2 is affected through containment, not its saved Worktree tag. Tab 3 is unrelated.
    state.tabs[1].worktree = Some(state.worktrees[0].id);
    state.tabs[2].worktree = None;
    let initial = versioned_snapshot(initial.epoch(), state, initial.runtime().to_vec());
    let marks = IdentityHighWater {
        workspace: 3,
        tab: 3,
        pane: 3,
        creation_ordinal: 9,
        repository: 1,
        worktree: 2,
        pairing: 1,
        client: 1,
        operation_stream: 1,
    };
    let mut reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(initial.state().clone(), marks, directory(b"/home")),
        StoreRevision::new(0),
        initial.epoch(),
        OrganizationLimits::default(),
    )
    .unwrap();
    let original = reducer.snapshot();
    let (runtime, facts, mut affected) = inputs(&original);
    affected.panes[1].containment = DirectoryContainment::Inside;
    let scope = assess_removal(&original, &runtime, &facts, &affected).unwrap();
    let edits = [
        (
            original.state().tabs[2].id,
            Some(original.state().worktrees[0].id),
        ),
        (
            original.state().tabs[1].id,
            Some(original.state().worktrees[1].id),
        ),
        (
            original.state().tabs[1].id,
            Some(original.state().worktrees[0].id),
        ),
    ];
    for (index, (tab, worktree)) in edits.into_iter().enumerate() {
        let current = reducer.snapshot();
        let retained = current.state().retained.as_ref().unwrap();
        let version = retained
            .tab_membership_versions()
            .iter()
            .find(|v| v.id() == tab)
            .unwrap()
            .version();
        let patch = WorktreeOrganizationPatch {
            binding: OperationBinding {
                id: support::operation(index as u64 + 1),
                digest: RequestDigest::from_bytes([9; 32]),
            },
            scope: RepositoryScope {
                server: support::server(),
                repository: facts.target.repository,
            },
            expected_inventory_version: Some(retained.repository_inventory_versions()[0].version()),
            repositories: vec![],
            worktrees: vec![],
            memberships: vec![WorktreeMembershipEdit {
                tab,
                expected_version: version,
                worktree,
            }],
            settlement: None,
        };
        let prepared = reducer
            .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
                patch,
            )))
            .unwrap();
        let receipt = spaceterm_core::storage::CommitReceipt {
            store_revision: prepared
                .expected_store_revision()
                .checked_successor()
                .unwrap(),
            organization_revision: prepared.resulting_organization_revision(),
        };
        reducer.apply_committed(prepared, &receipt).unwrap();
        if index == 0 {
            assert_eq!(
                revalidate_removal(&scope, &reducer.snapshot(), &runtime, &affected),
                Ok(())
            );
        }
    }
    let changed = reducer.snapshot();
    assert_eq!(changed.state().tabs[1], original.state().tabs[1]);
    assert_eq!(changed.runtime(), original.runtime());
    assert_eq!(
        revalidate_removal(&scope, &changed, &runtime, &affected),
        Err(GitFailure::StaleAssessment)
    );
}

#[test]
fn legacy_removal_scope_reads_without_inventing_membership_generations() {
    let original = snapshot(1);
    let (runtime, facts, affected) = inputs(&original);
    let scope = assess_removal(&original, &runtime, &facts, &affected).unwrap();
    let mut legacy = serde_json::to_value(scope.retained()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("membership_versions");
    let retained: RetainedRemovalScope = serde_json::from_value(legacy).unwrap();
    assert!(retained.parts().membership_versions.is_empty());
    let mut state = original.state().clone();
    state.retained = None;
    let unversioned =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    assert_eq!(
        assess_removal(&unversioned, &runtime, &facts, &affected),
        Err(GitFailure::Unchecked)
    );
}

#[test]
fn removal_digest_includes_authoritative_directory_facts_without_changing_hierarchy() {
    let original = snapshot(1);
    let (runtime, facts, affected) = inputs(&original);
    let first = assess_removal(&original, &runtime, &facts, &affected).unwrap();
    let mut state = original.state().clone();
    state.panes[0].confirmed_restoration_directory = Versioned {
        value: Some(directory(b"/different")),
        version: FieldVersion::new(2),
    };
    let changed =
        OrganizationSnapshot::new(original.epoch(), state, original.runtime().to_vec()).unwrap();
    let (runtime, facts, affected) = inputs(&changed);
    let second = assess_removal(&changed, &runtime, &facts, &affected).unwrap();
    assert_eq!(first.retained(), second.retained());
    assert_ne!(first.assessment_digest(), second.assessment_digest());
}

#[test]
fn inventory_membership_rejection_does_not_publish_or_advance_allocation_marks() {
    use spaceterm_core::{operations::*, reducer::*};
    let initial = snapshot(1);
    let mut state = initial.state().clone();
    state.settings.worktree_location.value =
        WorktreeLocationTemplate::new("~/Worktrees/{repository}/{branch}".into()).unwrap();
    let marks = IdentityHighWater {
        workspace: 1,
        tab: 1,
        pane: 1,
        creation_ordinal: 3,
        repository: 1,
        worktree: 2,
        pairing: 1,
        client: 1,
        operation_stream: 1,
    };
    let mut reducer = OrganizationReducer::from_recovered(
        RetainedOrganization::new(state, marks, directory(b"/home")),
        StoreRevision::new(0),
        initial.epoch(),
        OrganizationLimits::default(),
    )
    .unwrap();
    let before = reducer.snapshot();
    let inventory = GitInventoryPatch {
        repository: Some(before.state().repositories[0].id),
        directory: before.state().repositories[0].directory.clone(),
        expected_inventory_version: Some(FieldVersion::new(1)),
        worktrees: vec![GitInventoryWorktree {
            id: None,
            directory: directory(b"/fresh"),
            main: false,
            missing: false,
            listed: true,
        }],
    };
    let context = ReductionContext {
        principal: MutationPrincipal {
            pairing: PairingId::new(support::server(), 1).unwrap(),
            client: ClientId::new(support::server(), 1).unwrap(),
        },
        operation: support::operation(1),
        request_digest: RequestDigest::from_bytes([2; 32]),
        now: ReducerTick::new(1),
        facts: ReductionFacts::default(),
    };
    let stale = [GitInventoryMembership {
        tab: before.state().tabs[0].id,
        expected_version: FieldVersion::new(99),
        destination: Some(0),
    }];
    let prepared = reducer
        .prepare_git_inventory_reconciliation(&inventory, &stale, &context)
        .unwrap();
    assert_eq!(
        prepared.resolution(),
        &Resolution::Completed(Outcome::Failed(OperationFailure::StaleValidation))
    );
    assert_eq!(prepared.writes().resulting_high_water(), marks);
    let receipt = spaceterm_core::storage::CommitReceipt {
        store_revision: StoreRevision::new(1),
        organization_revision: prepared.resulting_organization_revision(),
    };
    assert!(
        reducer
            .apply_committed(prepared, &receipt)
            .unwrap()
            .publication
            .is_none()
    );
    assert_eq!(reducer.snapshot(), before);
    let invalid = [GitInventoryMembership {
        destination: Some(1),
        expected_version: FieldVersion::new(1),
        ..stale[0]
    }];
    assert!(matches!(
        reducer.prepare_git_inventory_reconciliation(&inventory, &invalid, &context),
        Err(ReducerFailure::InvalidFacts)
    ));
}
