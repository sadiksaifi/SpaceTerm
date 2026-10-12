use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::operations::*;
use spaceterm_core::organization::*;

pub fn server() -> ServerId {
    ServerId::new([7; 16])
}
pub fn operation(sequence: u64) -> OperationId {
    OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server(), 1).unwrap(),
            PairingId::new(server(), 1).unwrap(),
            ClientId::new(server(), 1).unwrap(),
        )
        .unwrap(),
        OperationSequence::new(sequence).unwrap(),
    )
}
pub fn create_command() -> Command {
    Command::CreateWorkspace {
        server: server(),
        source: WorkspaceCreationSource::ServerHome { server: server() },
        name: NameDraft::Automatic,
    }
}
pub fn organization() -> OrganizationState {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 1).unwrap();
    let pane = PaneId::new(server(), 1).unwrap();
    let directory = ServerDirectory::new(server(), b"/work".to_vec()).unwrap();
    OrganizationState {
        server: server(),
        revision: OrganizationRevision::new(1),
        settings: ServerSettingsRecord {
            worktree_location: Versioned {
                value: WorktreeLocationTemplate::new("../{repo}/{branch}".to_owned()).unwrap(),
                version: FieldVersion::new(1),
            },
        },
        workspaces: vec![WorkspaceRecord {
            id: workspace,
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: NameText::new("work".to_owned()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(1),
        }],
        tabs: vec![TabRecord {
            id: tab,
            workspace,
            worktree: None,
            root_pane: pane,
            creation_ordinal: CreationOrdinal::new(2),
        }],
        panes: vec![PaneRecord {
            id: pane,
            tab,
            creation_ordinal: CreationOrdinal::new(3),
            creation: PaneCreation::Initial,
            starting_directory: directory,
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }],
        worktrees: vec![],
        repositories: vec![],
    }
}
