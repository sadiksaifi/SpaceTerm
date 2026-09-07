//! Scratch directory authority, validation, and child-directory publication form one operation.
use super::*;

#[derive(Clone, Copy)]
pub(crate) enum DirectoryChange {
    Report(DirectoryAuthority),
    PaneClosed {
        removed: DirectoryAuthority,
        successor: DirectoryAuthority,
    },
    TabClosed {
        removed: super::super::TabId,
        successor: DirectoryAuthority,
    },
}

impl DirectoryChange {
    fn successor(self, current: DirectoryAuthority) -> Option<DirectoryAuthority> {
        match self {
            Self::Report(authority) if authority == current => Some(current),
            Self::PaneClosed { removed, successor } if removed == current => Some(successor),
            Self::TabClosed { removed, successor } if removed == current.tab_id() => {
                Some(successor)
            }
            _ => None,
        }
    }
}

impl<T> WorkspaceCollection<T> {
    /// Reject stale and non-Scratch owners before validation. Publish the accepted directory to
    /// children only after authority, availability, and automatic names have been committed.
    pub(crate) fn apply_directory_change(
        &mut self,
        workspace_id: WorkspaceId,
        change: DirectoryChange,
        report: Option<&Path>,
        validate: impl FnOnce(&Path) -> Result<ValidatedWorkspaceDirectory, String>,
        publish: impl FnOnce(&T, &ValidatedWorkspaceDirectory),
    ) -> bool {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return false;
        };
        let WorkspaceKind::Scratch {
            directory_authority,
        } = &mut workspace.kind
        else {
            return false;
        };
        let Some(successor) = change.successor(*directory_authority) else {
            return false;
        };
        let promotion = !matches!(change, DirectoryChange::Report(_));
        let validation = report.map(validate);
        let WorkspaceDirectoryLocation::Local(directory) = &mut workspace.directory_location else {
            unreachable!("a Scratch Workspace must own a local Workspace Directory")
        };
        let mut changed = promotion;
        let mut publish_directory = promotion;
        *directory_authority = successor;
        match validation {
            Some(Ok(next)) => {
                let updated = *directory != next || !workspace.availability.is_available();
                changed |= updated;
                publish_directory |= updated;
                *directory = next;
                workspace.availability = WorkspaceDirectoryAvailability::Available;
            }
            Some(Err(reason)) => {
                workspace.availability = WorkspaceDirectoryAvailability::Unavailable { reason };
                changed = true;
            }
            None => {}
        }
        if !changed {
            return false;
        }
        self.recalculate_automatic_names();
        if publish_directory {
            let workspace = self
                .workspace(workspace_id)
                .expect("the operation retains its Workspace");
            let WorkspaceDirectoryLocation::Local(directory) = &workspace.directory_location else {
                unreachable!("a Scratch Workspace must own a local Workspace Directory")
            };
            publish(&workspace.payload, directory);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn validated(path: &str, label: u64) -> ValidatedWorkspaceDirectory {
        ValidatedWorkspaceDirectory::new(
            PathBuf::from(path),
            WorkspaceDirectoryIdentity::for_test(label),
        )
    }
    use crate::domain::{PaneId, TabId};

    #[test]
    fn directory_promotion_rejects_stale_owners_before_io_and_publication() {
        let authority = DirectoryAuthority::new(TabId::new(1), PaneId::new(1));
        let stale = DirectoryAuthority::new(TabId::new(1), PaneId::new(2));
        let mut workspaces =
            WorkspaceCollection::new_scratch(validated("/old", 1), authority, |_, _| ());
        assert!(!workspaces.apply_directory_change(
            WorkspaceId::new(1),
            DirectoryChange::Report(stale),
            Some(Path::new("/ignored")),
            |_| panic!("stale report must not validate"),
            |_, _| panic!("stale report must not publish")
        ));
    }

    #[test]
    fn directory_promotion_preserves_exact_spelling_and_publishes_after_validation() {
        let authority = DirectoryAuthority::new(TabId::new(1), PaneId::new(1));
        let mut workspaces =
            WorkspaceCollection::new_scratch(validated("/project", 1), authority, |_, _| ());
        let published = std::cell::RefCell::new(None);
        assert!(workspaces.apply_directory_change(
            WorkspaceId::new(1),
            DirectoryChange::Report(authority),
            Some(Path::new("/project/.")),
            |path| Ok(validated(path.to_str().unwrap(), 1)),
            |_, directory| *published.borrow_mut() = Some(directory.path().to_owned())
        ));
        assert_eq!(
            published.into_inner().as_deref(),
            Some(Path::new("/project/."))
        );
    }

    #[test]
    fn directory_promotion_invalid_successor_retains_path_and_transfers_authority() {
        let first = DirectoryAuthority::new(TabId::new(1), PaneId::new(1));
        let successor = DirectoryAuthority::new(TabId::new(2), PaneId::new(1));
        let mut workspaces =
            WorkspaceCollection::new_scratch(validated("/old", 1), first, |_, _| ());
        assert!(workspaces.apply_directory_change(
            WorkspaceId::new(1),
            DirectoryChange::TabClosed {
                removed: TabId::new(1),
                successor
            },
            Some(Path::new("/missing")),
            |_| Err("missing".into()),
            |_, directory| assert_eq!(directory.path(), Path::new("/old"))
        ));
        let workspace = workspaces.active_workspace();
        assert!(
            matches!(workspace.kind(), WorkspaceKind::Scratch { directory_authority } if *directory_authority == successor)
        );
        assert!(!workspace.availability().is_available());
        assert!(workspaces.apply_directory_change(
            WorkspaceId::new(1),
            DirectoryChange::Report(successor),
            Some(Path::new("/new")),
            |_| Ok(validated("/new", 2)),
            |_, directory| assert_eq!(directory.path(), Path::new("/new"))
        ));
        assert!(workspaces.active_workspace().availability().is_available());
    }
}
