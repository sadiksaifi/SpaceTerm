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
        match &self.location {
            WorkspaceLocation::Local => {
                let directory = self
                    .local_display_directory()
                    .expect("Local Workspace has a local directory");
                if Some(directory) == self.local_home_directory() {
                    return "Default".to_owned();
                }
                directory
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "/".into())
            }
            WorkspaceLocation::Remote {
                remote_home_identity,
                ..
            } => {
                let directory = self
                    .remote_display_directory()
                    .expect("Remote Workspace has a remote directory")
                    .as_str();
                if matches!(directory, "~" | "~/") || directory == remote_home_identity.as_str() {
                    return "Default".to_owned();
                }
                let basename = directory
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("/");
                basename.to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_should_be_reused_without_renumbering_survivors() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        let second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/home/test"), |_, _| ())
            .unwrap();
        workspaces
            .close_workspace(WorkspaceId::new(1), PathBuf::from("/home/test"), |_, _| ())
            .unwrap();
        let third = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/home/test"), |_, _| ())
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
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        let first = workspaces.active_workspace_id();
        workspaces
            .update_automatic_directory(
                first,
                CurrentDirectory::Local(PathBuf::from("/projects/api")),
            )
            .unwrap();
        assert_eq!(workspaces.workspace(first).unwrap().name(), "api");
        let second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/home/test"), |_, _| ())
            .unwrap();
        workspaces
            .name_workspace_for_creation(second, "Default".into())
            .unwrap();
        workspaces
            .update_automatic_directory(first, CurrentDirectory::Local(PathBuf::from("/home/test")))
            .unwrap();
        assert_eq!(workspaces.workspace(first).unwrap().name(), "Default (2)");
        workspaces
            .update_automatic_directory(
                second,
                CurrentDirectory::Local(PathBuf::from("/projects/other")),
            )
            .unwrap();
        assert_eq!(workspaces.workspace(second).unwrap().name(), "Default");
    }

    #[test]
    fn automatic_directory_should_reject_cross_machine_values_without_mutation() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        let id = workspaces.active_workspace_id();
        assert_eq!(
            workspaces.update_automatic_directory(
                id,
                CurrentDirectory::Remote(RemoteDirectory::new("/srv/api".into()).unwrap())
            ),
            Err(WorkspaceError::DirectoryLocationMismatch(id))
        );
        assert_eq!(
            workspaces.active_workspace().local_display_directory(),
            Some(Path::new("/home/test"))
        );
    }

    #[test]
    fn automatic_names_should_avoid_generated_suffixes_and_custom_names() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        for (index, directory) in ["/one/project", "/two/project", "/three/project (2)"]
            .into_iter()
            .enumerate()
        {
            let id = workspaces
                .create_local_workspace_unchecked(PathBuf::from("/home/test"), |_, _| ())
                .unwrap();
            workspaces
                .set_pinned_directory(
                    id,
                    Some(PinnedDirectory::Local(ValidatedLocalDirectory::new(
                        PathBuf::from(directory),
                        LocalDirectoryIdentity::for_test(index as u64 + 1),
                    ))),
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

    #[test]
    fn remote_name_should_match_local_basename_behavior() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/local"), |_, _| ());
        let home = RemoteDirectoryIdentity::new("/home/remote".into()).unwrap();
        let id = workspaces
            .create_remote_workspace(
                RemoteWorkspaceTarget::new(
                    SshDestination::new("build".into()).unwrap(),
                    home.clone(),
                ),
                RemoteUser::new("remote".into()).unwrap(),
                RemoteDirectory::new("/srv/project".into()).unwrap(),
                home,
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();

        assert_eq!(workspaces.workspace(id).unwrap().name(), "project");
    }

    #[test]
    fn automatic_name_collisions_should_span_all_machines() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/local"), |_, _| ());
        let home = RemoteDirectoryIdentity::new("/home/remote".into()).unwrap();
        let mut create = |destination: &str| {
            workspaces
                .create_remote_workspace(
                    RemoteWorkspaceTarget::new(
                        SshDestination::new(destination.into()).unwrap(),
                        RemoteDirectoryIdentity::new("/srv/project".into()).unwrap(),
                    ),
                    RemoteUser::new("remote".into()).unwrap(),
                    RemoteDirectory::new("/srv/project".into()).unwrap(),
                    home.clone(),
                    RemoteConnectionState::connected(1),
                    |_| (),
                )
                .unwrap()
        };

        let first = create("build");
        let second = create("build");
        let other_machine = create("build-alias");

        assert_eq!(workspaces.workspace(first).unwrap().name(), "project");
        assert_eq!(workspaces.workspace(second).unwrap().name(), "project (2)");
        assert_eq!(
            workspaces.workspace(other_machine).unwrap().name(),
            "project (3)"
        );
    }
}
