use super::*;

impl<T> WorkspaceCollection<T> {
    pub(super) fn recalculate_automatic_names(&mut self) {
        let mut occupied: std::collections::HashSet<String> = self
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.custom_name.clone())
            .collect();
        let mut pending = Vec::new();
        for workspace in &mut self.workspaces {
            if let Some(custom_name) = &workspace.custom_name {
                workspace.name.clone_from(custom_name);
                continue;
            }
            let base = workspace.automatic_name();
            // Reserve unchanged identities before allocating names for moving Workspaces.
            if workspace.automatic_name_base == base && occupied.insert(workspace.name.clone()) {
                continue;
            }
            pending.push((workspace, base));
        }
        for (workspace, base) in pending {
            let mut name = base.clone();
            let mut ordinal = 2;
            while !occupied.insert(name.clone()) {
                name = format!("{base} ({ordinal})");
                ordinal += 1;
            }
            workspace.name = name;
            workspace.automatic_name_base = base;
        }
    }
}

impl<T> WorkspaceEntry<T> {
    fn automatic_name(&self) -> String {
        let directory = self.home_directory.path();
        let directory = self.local_display_directory().unwrap_or(directory);
        if Some(directory) == self.local_home_directory() {
            return "Default".to_owned();
        }
        directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "/".into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::PresentedWorkspace;
    use super::*;

    fn validated(path: &str, identity_label: u64) -> ValidatedLocalDirectory {
        ValidatedLocalDirectory::new(
            PathBuf::from(path),
            LocalDirectoryIdentity::for_test(identity_label),
        )
    }

    #[test]
    fn default_should_be_reused_without_renumbering_survivors() {
        let mut workspaces = crate::domain::workspace_collection::tests::local_collection(
            validated("/home/test", 0),
            |_, _| (),
        );
        let second = workspaces
            .create_local_workspace(validated("/home/test", 0), |_, _| ())
            .unwrap();
        workspaces.remove_workspace(WorkspaceId::new(1)).unwrap();
        let third = workspaces
            .create_local_workspace(validated("/home/test", 0), |_, _| ())
            .unwrap();
        assert_eq!(
            (
                workspaces.workspace(second).unwrap().name(),
                workspaces.workspace(third).unwrap().name()
            ),
            ("Default (2)", "Default")
        );
    }

    #[test]
    fn automatic_name_should_follow_directory_and_return_to_an_available_default() {
        let mut workspaces = crate::domain::workspace_collection::tests::local_collection(
            validated("/home/test", 0),
            |_, _| (),
        );
        let first = workspaces.active_id();
        workspaces
            .update_automatic_directory(
                first,
                PathBuf::from("/projects/api"),
            )
            .unwrap();
        assert_eq!(workspaces.workspace(first).unwrap().name(), "api");
        let second = workspaces
            .create_local_workspace(validated("/home/test", 0), |_, _| ())
            .unwrap();
        workspaces
            .rename_workspace(second, "Default".into())
            .unwrap();
        workspaces
            .update_automatic_directory(first, PathBuf::from("/home/test"))
            .unwrap();
        assert_eq!(workspaces.workspace(first).unwrap().name(), "Default (2)");
        workspaces
            .update_automatic_directory(
                second,
                PathBuf::from("/projects/other"),
            )
            .unwrap();
        assert_eq!(workspaces.workspace(second).unwrap().name(), "Default");
    }

    #[test]
    fn automatic_names_should_avoid_generated_suffixes_and_custom_names() {
        let mut workspaces = crate::domain::workspace_collection::tests::local_collection(
            validated("/home/test", 0),
            |_, _| (),
        );
        for (index, directory) in ["/one/project", "/two/project", "/three/project (2)"]
            .into_iter()
            .enumerate()
        {
            let id = workspaces
                .create_local_workspace(validated("/home/test", 0), |_, _| ())
                .unwrap();
            workspaces
                .set_pinned_directory(
                    id,
                    Some(ValidatedLocalDirectory::new(
                        PathBuf::from(directory),
                        LocalDirectoryIdentity::for_test(index as u64 + 1),
                    )),
                )
                .unwrap();
        }
        workspaces
            .rename_workspace(WorkspaceId::new(1), "project".into())
            .unwrap();
        let names: Vec<_> = workspaces
            .iter()
            .map(|workspace| workspace.name())
            .collect();
        assert_eq!(
            names,
            ["project", "project (3)", "project (2)", "project (2) (2)"]
        );
    }
}
