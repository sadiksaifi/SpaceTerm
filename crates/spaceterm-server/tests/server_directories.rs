use spaceterm_core::authority::*;
use spaceterm_core::identity::*;
use spaceterm_core::observations::*;
use spaceterm_core::organization::*;
use spaceterm_server::git::directories::*;
#[test]
fn git_observes_only_current_committed_directory_without_persisting_it_again() {
    let server = ServerId::new([4; 16]);
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let tab = TabId::new(server, 1).unwrap();
    let pane = PaneId::new(server, 1).unwrap();
    let directory = ServerDirectory::new(server, b"/confirmed".to_vec()).unwrap();
    let state = OrganizationState {
        server,
        revision: OrganizationRevision::new(1),
        settings: ServerSettingsRecord {
            worktree_location: Versioned {
                value: WorktreeLocationTemplate::new("~/{branch}".into()).unwrap(),
                version: FieldVersion::new(1),
            },
        },
        workspaces: vec![WorkspaceRecord {
            id: workspace,
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(1),
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: NameText::new("work".into()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
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
            creation: PaneCreation::Initial,
            creation_ordinal: CreationOrdinal::new(3),
            starting_directory: directory.clone(),
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }],
        worktrees: vec![],
        repositories: vec![],
        retained: None,
    };
    let epoch = ServerEpoch::new(1);
    let generation = SessionGeneration::new(1);
    let runtime = vec![PaneRuntimeRecord {
        pane,
        session_generation: generation,
        phase: SessionPhase::Running { generation },
        control_generation: ControlGeneration::new(0),
        control: ControlRecord::Unclaimed,
    }];
    let snapshot = OrganizationSnapshot::new(epoch, state, runtime.clone()).unwrap();
    let event = ConfirmedDirectoryObservation::new(
        SessionKey {
            epoch,
            pane,
            generation,
        },
        RuntimeObservationSequence::new(1).unwrap(),
        MonotonicTick::new(1),
        directory,
    )
    .unwrap();
    let mut observer = CommittedDirectoryObserver::default();
    assert_eq!(
        observer.accept_confirmed_directory(&snapshot, &event),
        ConfirmedDirectoryDisposition::Ignored
    );
    let mut committed = snapshot.state().clone();
    committed.panes[0].confirmed_restoration_directory.value = Some(event.directory().clone());
    let committed = OrganizationSnapshot::new(epoch, committed, runtime).unwrap();
    assert_eq!(
        observer.accept_confirmed_directory(&committed, &event),
        ConfirmedDirectoryDisposition::Scheduled
    );
    assert_eq!(
        observer.accept_confirmed_directory(&committed, &event),
        ConfirmedDirectoryDisposition::Ignored
    );
    assert_eq!(committed.state().revision, snapshot.state().revision);
}
