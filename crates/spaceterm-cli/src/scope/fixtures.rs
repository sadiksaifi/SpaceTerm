use super::*;
use spaceterm_server::hierarchy::{Axis, Pane, Tab, Workspace, WorktreeMembership};
use spaceterm_server::repository::{self, WorktreeHead, WorktreeRecord};

pub(crate) fn id(byte: u8) -> Id {
    [byte; 32]
}

pub(crate) fn fixture() -> Snapshot {
    let mut inventory = Inventory {
        revision: 7,
        close: None,
        repositories: None,
        workspaces: vec![workspace(1, "api"), workspace(2, "tools")],
    };
    inventory.workspaces[0].tabs = vec![
        tab(11, 21, "/repo", "cargo test"),
        tab(12, 22, "/trees/feature", "cargo test"),
    ];
    inventory.workspaces[0].root_tab = id(11);
    inventory.workspaces[1].tabs = vec![tab(13, 23, "", "Editor")];
    inventory.workspaces[1].root_tab = id(13);
    let repositories = repository::Snapshot {
        boot: id(8),
        revision: 4,
        git: repository::GitToolStatus::Unknown,
        pinned_worktrees: Vec::new(),
        pin_check: repository::PinCheckState::Ready,
        directories: vec![repository::DirectoryFacts {
            directory: "/repo".into(),
            repository: None,
            failure: None,
            worktrees: Some(repository::WorktreeInventory {
                main_root: "/repo".into(),
                current: Some(0),
                worktrees: vec![
                    tree("/repo", "main"),
                    tree("/trees/feature", "feature/x"),
                    tree("/trees/empty", "empty"),
                ],
            }),
        }],
    };
    inventory.repositories = Some(repositories.clone());
    Snapshot {
        server: id(9),
        inventory,
        repositories: Some(repositories),
    }
}

fn workspace(byte: u8, name: &str) -> Workspace {
    Workspace {
        id: id(byte),
        name: name.into(),
        name_version: 2,
        pin_version: 0,
        pinned_directory: None,
        root_tab: id(0),
        tabs: Vec::new(),
    }
}

fn tree(path: &str, branch: &str) -> WorktreeRecord {
    WorktreeRecord {
        root: path.into(),
        head: WorktreeHead::Branch(branch.into()),
        locked: false,
        missing: false,
    }
}

fn tab(byte: u8, pane: u8, path: &str, activity: &str) -> Tab {
    let presentation = serde_json::from_value(serde_json::json!({
        "revision": 1, "shell_name": "zsh", "title": activity,
        "title_provenance": "TerminalControl", "directory": path,
        "directory_provenance": "Osc7", "user": null, "host": null, "home": null,
        "freshness": "Live", "finished_commands": 0, "prompt_zone": "Prompt", "command": null,
        "title_activity": false, "title_glyph": "Reported", "progress": "None"
    }))
    .unwrap();
    Tab {
        id: id(byte),
        root_pane: id(pane),
        worktree_membership: WorktreeMembership::Following,
        worktree: (!path.is_empty()).then(|| path.into()),
        worktree_repository: (!path.is_empty()).then(|| "/repo".into()),
        panes: vec![Pane {
            id: id(pane),
            source: None,
            axis: Axis::Right,
            starting_directory: path.into(),
            current_directory: Some(path.into()),
            confirmed_directory: Some(path.into()),
            repository_directory: Some(path.into()),
            presentation: Some(Box::new(presentation)),
            creator: None,
        }],
    }
}
