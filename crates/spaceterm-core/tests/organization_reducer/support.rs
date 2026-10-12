pub use spaceterm_core::{
    authority::*, identity::*, operations::*, organization::*, organization_retained::*,
    reducer::*, remote_access::RemoteAccessPolicy, storage::*,
};

pub const SERVER: ServerId = ServerId::new([1; 16]);
pub fn directory(value: &[u8]) -> ServerDirectory {
    ServerDirectory::new(SERVER, value.to_vec()).unwrap()
}
pub fn empty() -> RetainedOrganization {
    RetainedOrganization::new(
        OrganizationState {
            server: SERVER,
            revision: OrganizationRevision::new(0),
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    value: WorktreeLocationTemplate::new(
                        "~/.worktrees/{repository}/{branch}".into(),
                    )
                    .unwrap(),
                    version: FieldVersion::new(1),
                },
            },
            workspaces: vec![],
            tabs: vec![],
            panes: vec![],
            repositories: vec![],
            worktrees: vec![],
            retained: Some(
                OrganizationRetainedFacts::new(OrganizationRetainedParts {
                    server: SERVER,
                    workspace_names: vec![],
                    creation_defaults: vec![],
                    pane_generations: vec![],
                    tab_membership_versions: vec![],
                    repository_inventory_versions: vec![],
                    worktree_inventory: vec![],
                    remote_access: Versioned {
                        value: RemoteAccessPolicy::disabled(SERVER),
                        version: FieldVersion::new(1),
                    },
                })
                .unwrap(),
            ),
        },
        IdentityHighWater {
            pairing: 8,
            client: 8,
            operation_stream: 8,
            ..Default::default()
        },
        directory(b"/home/owner"),
    )
}
pub fn reducer() -> OrganizationReducer {
    OrganizationReducer::from_recovered(
        empty(),
        StoreRevision::new(0),
        ServerEpoch::new(9),
        OrganizationLimits::default(),
    )
    .unwrap()
}
pub fn context(sequence: u64) -> ReductionContext {
    let pairing = PairingId::new(SERVER, 1).unwrap();
    let client = ClientId::new(SERVER, 1).unwrap();
    let stream =
        OperationStreamId::new(OperationStreamKey::new(SERVER, 1).unwrap(), pairing, client)
            .unwrap();
    ReductionContext {
        principal: MutationPrincipal { pairing, client },
        operation: OperationId::new(stream, OperationSequence::new(sequence).unwrap()),
        request_digest: RequestDigest::from_bytes([3; 32]),
        now: ReducerTick::new(10),
        facts: ReductionFacts::default(),
    }
}
pub fn retiring_context(sequence: u64) -> ReductionContext {
    let mut context = context(sequence);
    context.facts.retirement_outcome = Some(
        AppliedRetirementOutcome::new(&spaceterm_core::operation_ledger::canonical::CanonicalCodec)
            .unwrap(),
    );
    context
}
pub fn facts(reducer: &OrganizationReducer, command: &Command, sequence: u64) -> ReductionContext {
    let mut context = context(sequence);
    let basis = reducer.directory_basis(command).unwrap();
    let worktree = basis.worktree.as_ref().map(|v| v.worktree.id);
    let fact = |directory: ServerDirectory| DirectoryFact {
        directory,
        available: true,
        physically_inside: worktree,
    };
    context.facts.directories = Some(DirectoryValidation {
        binding: context.binding(),
        home: fact(directory(b"/home/owner")),
        pin: basis.pin.as_ref().and_then(|v| v.value.clone()).map(fact),
        source: basis.source.as_ref().map(|v| fact(v.directory.clone())),
        requested: basis.requested.clone().map(fact),
        worktree: basis
            .worktree
            .as_ref()
            .map(|v| fact(v.worktree.directory.clone())),
        worktree_bare: false,
        basis,
    });
    context
}
/// The fake models only the uncontrolled commit boundary. It issues a receipt
/// after checking the reducer's public projection; it does not claim SQLite durability.
pub fn commit(
    reducer: &mut OrganizationReducer,
    prepared: PreparedReduction,
) -> CommittedReduction {
    let receipt = CommitReceipt {
        store_revision: prepared
            .expected_store_revision()
            .checked_successor()
            .unwrap(),
        organization_revision: prepared.resulting_organization_revision(),
    };
    reducer.apply_committed(prepared, &receipt).unwrap()
}
pub fn apply(
    reducer: &mut OrganizationReducer,
    command: Command,
    sequence: u64,
) -> CommittedReduction {
    let context = if matches!(
        command,
        Command::CreateWorkspace { .. }
            | Command::CreateTab { .. }
            | Command::CreatePane { .. }
            | Command::SetPinnedDirectory { .. }
            | Command::RepairPaneDirectory { .. }
    ) {
        facts(reducer, &command, sequence)
    } else if matches!(
        command,
        Command::CloseConfirmed { .. }
            | Command::KillWorkspaceConfirmed { .. }
            | Command::EndAllSessions { .. }
    ) {
        retiring_context(sequence)
    } else {
        context(sequence)
    };
    let prepared = reducer.prepare_command(&command, &context).unwrap();
    commit(reducer, prepared)
}
pub fn create(reducer: &mut OrganizationReducer, sequence: u64) -> (WorkspaceId, TabId, PaneId) {
    let result = apply(
        reducer,
        Command::CreateWorkspace {
            server: SERVER,
            source: WorkspaceCreationSource::ServerHome { server: SERVER },
            name: NameDraft::Automatic,
        },
        sequence,
    );
    match result.resolution {
        Resolution::Completed(Outcome::CreatedWithControl(created)) => match created.entities() {
            CreatedEntities::Workspace {
                workspace,
                tab,
                pane,
            } => (workspace, tab, pane),
            _ => panic!("unexpected creation kind"),
        },
        _ => panic!("creation did not complete"),
    }
}

pub fn with_worktrees(retained: RetainedOrganization) -> RetainedOrganization {
    let mut org = retained.organization().clone();
    let mut marks = retained.high_water();
    let repository = RepositoryId::new(SERVER, 1).unwrap();
    org.repositories.push(RepositoryRecord {
        id: repository,
        directory: directory(b"/repo"),
    });
    let mut facts = org.retained.as_ref().unwrap().parts().clone();
    facts
        .repository_inventory_versions
        .push(RepositoryInventoryVersion::new(
            repository,
            FieldVersion::new(1),
        ));
    for index in 1..=2 {
        let id = WorktreeId::new(SERVER, index).unwrap();
        org.worktrees.push(WorktreeRecord {
            id,
            repository,
            directory: directory(if index == 1 {
                b"/repo/main"
            } else {
                b"/repo/feature"
            }),
            main: index == 1,
            missing: false,
        });
        facts
            .worktree_inventory
            .push(WorktreeInventoryFacts::new(id, true, FieldVersion::new(1)));
    }
    org.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    marks.repository = 1;
    marks.worktree = 2;
    RetainedOrganization::new(org, marks, retained.server_home().clone())
}

pub fn replay(snapshot: OrganizationSnapshot, delta: &OrganizationDelta) -> OrganizationSnapshot {
    assert_eq!(snapshot.state().revision, delta.base());
    let mut org = snapshot.state().clone();
    let mut runtime = snapshot.runtime().to_vec();
    let mut facts = org.retained.as_ref().unwrap().parts().clone();
    macro_rules! put {
        ($values:expr, $value:expr, $id:expr) => {{
            let value = $value;
            let key = $id;
            $values.retain(|old| key(old) != key(value));
            $values.push(value.clone());
        }};
    }
    for write in delta.writes() {
        match write {
            OrganizationWrite::PutWorkspace(v) => {
                put!(org.workspaces, v, |v: &WorkspaceRecord| v.id)
            }
            OrganizationWrite::DeleteWorkspace(id) => org.workspaces.retain(|v| v.id != *id),
            OrganizationWrite::PutTab(v) => put!(org.tabs, v, |v: &TabRecord| v.id),
            OrganizationWrite::DeleteTab(id) => org.tabs.retain(|v| v.id != *id),
            OrganizationWrite::PutPane(v) => put!(org.panes, v, |v: &PaneRecord| v.id),
            OrganizationWrite::DeletePane(id) => org.panes.retain(|v| v.id != *id),
            OrganizationWrite::PutRepository(v) => {
                put!(org.repositories, v, |v: &RepositoryRecord| v.id)
            }
            OrganizationWrite::DeleteRepository(id) => org.repositories.retain(|v| v.id != *id),
            OrganizationWrite::PutWorktree(v) => put!(org.worktrees, v, |v: &WorktreeRecord| v.id),
            OrganizationWrite::DeleteWorktree(id) => org.worktrees.retain(|v| v.id != *id),
            OrganizationWrite::SetServerSettings(v) => org.settings = v.clone(),
            OrganizationWrite::Retained(write) => match write {
                RetainedOrganizationWrite::PutWorkspaceNameBasis(v) => {
                    put!(facts.workspace_names, v, |v: &WorkspaceNameBasis| v
                        .workspace())
                }
                RetainedOrganizationWrite::DeleteWorkspaceNameBasis(id) => {
                    facts.workspace_names.retain(|v| v.workspace() != *id)
                }
                RetainedOrganizationWrite::PutCreationDefault(v) => {
                    put!(facts.creation_defaults, v, |v: &TabCreationDefault| v.tab())
                }
                RetainedOrganizationWrite::DeleteCreationDefault(id) => {
                    facts.creation_defaults.retain(|v| v.tab() != *id)
                }
                RetainedOrganizationWrite::SetPaneGenerationHighWater(v) => {
                    put!(facts.pane_generations, v, |v: &PaneGenerationHighWater| v
                        .pane())
                }
                RetainedOrganizationWrite::DeletePaneGenerationHighWater(id) => {
                    facts.pane_generations.retain(|v| v.pane() != *id)
                }
                RetainedOrganizationWrite::SetTabMembershipVersion(v) => put!(
                    facts.tab_membership_versions,
                    v,
                    |v: &TabMembershipVersion| v.id()
                ),
                RetainedOrganizationWrite::DeleteTabMembershipVersion(id) => {
                    facts.tab_membership_versions.retain(|v| v.id() != *id)
                }
                RetainedOrganizationWrite::SetRepositoryInventoryVersion(v) => put!(
                    facts.repository_inventory_versions,
                    v,
                    |v: &RepositoryInventoryVersion| v.id()
                ),
                RetainedOrganizationWrite::DeleteRepositoryInventoryVersion(id) => facts
                    .repository_inventory_versions
                    .retain(|v| v.id() != *id),
                RetainedOrganizationWrite::SetWorktreeInventoryFacts(v) => {
                    put!(facts.worktree_inventory, v, |v: &WorktreeInventoryFacts| v
                        .worktree())
                }
                RetainedOrganizationWrite::DeleteWorktreeInventoryFacts(id) => {
                    facts.worktree_inventory.retain(|v| v.worktree() != *id)
                }
                RetainedOrganizationWrite::SetRemoteAccess(v) => facts.remote_access = v.clone(),
            },
        }
    }
    org.workspaces.sort_by_key(|v| v.id);
    org.tabs.sort_by_key(|v| v.id);
    org.panes.sort_by_key(|v| v.id);
    org.repositories.sort_by_key(|v| v.id);
    org.worktrees.sort_by_key(|v| v.id);
    facts.workspace_names.sort_by_key(|v| v.workspace());
    facts.creation_defaults.sort_by_key(|v| v.tab());
    facts.pane_generations.sort_by_key(|v| v.pane());
    facts.tab_membership_versions.sort_by_key(|v| v.id());
    facts.repository_inventory_versions.sort_by_key(|v| v.id());
    facts.worktree_inventory.sort_by_key(|v| v.worktree());
    org.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    org.revision = delta.revision();
    runtime.retain(|v| org.panes.iter().any(|p| p.id == v.pane));
    for value in delta.runtime() {
        put!(runtime, value, |v: &PaneRuntimeRecord| v.pane);
    }
    runtime.sort_by_key(|v| v.pane);
    OrganizationSnapshot::new(delta.epoch(), org, runtime).unwrap()
}
