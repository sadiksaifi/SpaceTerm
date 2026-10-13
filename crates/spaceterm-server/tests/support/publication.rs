use spaceterm_core::{authority::*, identity::*, organization::*, organization_retained::*};
use std::sync::Arc;
pub fn snapshot(revision: u64) -> Arc<OrganizationSnapshot> {
    let server = ServerId::new([1; 16]);
    Arc::new(
        OrganizationSnapshot::new(
            ServerEpoch::new(1),
            OrganizationState {
                server,
                revision: OrganizationRevision::new(revision),
                settings: ServerSettingsRecord {
                    worktree_location: Versioned {
                        version: spaceterm_core::authority::FieldVersion::new(1),
                        value: WorktreeLocationTemplate::new(
                            "~/.worktrees/{repository}/{branch}".into(),
                        )
                        .unwrap(),
                    },
                },
                workspaces: vec![],
                tabs: vec![],
                panes: vec![],
                repositories: vec![],
                worktrees: vec![],
                retained: Some(
                    OrganizationRetainedFacts::new(OrganizationRetainedParts {
                        server,
                        workspace_names: vec![],
                        creation_defaults: vec![],
                        pane_generations: vec![],
                        tab_membership_versions: vec![],
                        repository_inventory_versions: vec![],
                        worktree_inventory: vec![],
                        remote_access: Versioned {
                            version: spaceterm_core::authority::FieldVersion::new(1),
                            value: spaceterm_core::remote_access::RemoteAccessPolicy::disabled(
                                server,
                            ),
                        },
                    })
                    .unwrap(),
                ),
            },
            vec![],
        )
        .unwrap(),
    )
}
pub fn large_snapshot() -> Arc<OrganizationSnapshot> {
    snapshot_with_workspaces(24)
}
pub fn snapshot_with_workspaces(count: u64) -> Arc<OrganizationSnapshot> {
    let empty = snapshot(7);
    let mut state = empty.state().clone();
    let mut facts = state.retained.take().unwrap().parts().clone();
    let mut runtime = Vec::new();
    let client = ClientId::new(state.server, 1).unwrap();
    let directory = ServerDirectory::new(state.server, vec![b'd'; 3800]).unwrap();
    for n in 1..=count {
        let workspace = WorkspaceId::new(state.server, n).unwrap();
        let tab = TabId::new(state.server, n).unwrap();
        let root = PaneId::new(state.server, n * 4 - 3).unwrap();
        let panes: Vec<_> = (n * 4 - 3..=n * 4)
            .map(|id| PaneId::new(state.server, id).unwrap())
            .collect();
        let name = NameText::new(format!("{n}{}", "w".repeat(900))).unwrap();
        state.workspaces.push(WorkspaceRecord {
            id: workspace,
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(n * 6 - 5),
            name: Versioned {
                version: FieldVersion::new(2),
                value: WorkspaceName {
                    mode: NameMode::Custom,
                    display: name,
                },
            },
            pinned_directory: Versioned {
                version: FieldVersion::new(3),
                value: Some(directory.clone()),
            },
        });
        facts.workspace_names.push(WorkspaceNameBasis::new(
            workspace,
            NameText::new("Shell".into()).unwrap(),
            NameText::new(format!("Shell {n}")).unwrap(),
        ));
        state.tabs.push(TabRecord {
            id: tab,
            workspace,
            root_pane: root,
            worktree: None,
            creation_ordinal: CreationOrdinal::new(n * 6 - 4),
        });
        facts
            .creation_defaults
            .push(TabCreationDefault::new(tab, panes.clone()).unwrap());
        facts
            .tab_membership_versions
            .push(TabMembershipVersion::new(tab, FieldVersion::new(4)));
        for (index, pane) in panes.into_iter().enumerate() {
            let creation = if index == 0 {
                PaneCreation::Initial
            } else {
                PaneCreation::SplitOf {
                    source: root,
                    axis: SplitAxis::Vertical,
                    edge: SplitEdge::After,
                }
            };
            let ordinal = n * 6 - 3 + index as u64;
            let phase = if index == 0 {
                SessionPhase::Running {
                    generation: SessionGeneration::new(5),
                }
            } else {
                SessionPhase::Failed {
                    class: FailureClass::ResourceLimit,
                }
            };
            state.panes.push(PaneRecord {
                id: pane,
                tab,
                creation,
                creation_ordinal: CreationOrdinal::new(ordinal),
                starting_directory: directory.clone(),
                confirmed_restoration_directory: Versioned {
                    version: FieldVersion::new(6),
                    value: Some(directory.clone()),
                },
            });
            facts.pane_generations.push(PaneGenerationHighWater::new(
                pane,
                SessionGeneration::new(5),
                ControlGeneration::new(8),
            ));
            runtime.push(PaneRuntimeRecord {
                pane,
                session_generation: SessionGeneration::new(5),
                phase,
                control_generation: ControlGeneration::new(8),
                control: ControlRecord::Held {
                    client,
                    generation: ControlGeneration::new(8),
                },
            });
        }
    }
    state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    Arc::new(OrganizationSnapshot::new(empty.epoch(), state, runtime).unwrap())
}
