use super::*;

impl<T> WorkspaceCollection<T> {
    pub(crate) fn update_identity_directory(
        &mut self,
        workspace_id: WorkspaceId,
        directory: CurrentDirectory,
    ) -> bool {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return false;
        };
        let same_machine = matches!(
            (&workspace.location, &directory),
            (WorkspaceLocation::Local, CurrentDirectory::Local(_))
                | (
                    WorkspaceLocation::Remote { .. },
                    CurrentDirectory::Remote(_)
                )
        );
        if !same_machine || workspace.identity_directory == directory {
            return false;
        }
        workspace.identity_directory = directory;
        self.recalculate_automatic_names();
        true
    }

    pub(super) fn recalculate_automatic_names(&mut self) {
        let mut occupied: std::collections::HashSet<String> = self
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.custom_name.clone())
            .collect();
        for workspace in &mut self.workspaces {
            if let Some(custom_name) = &workspace.custom_name {
                workspace.name.clone_from(custom_name);
                continue;
            }
            let base = workspace.automatic_name();
            let mut name = base.clone();
            let mut ordinal = 2;
            while !occupied.insert(name.clone()) {
                name = format!("{base} {ordinal}");
                ordinal += 1;
            }
            workspace.name = name;
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
                    return self.fallback_name.clone();
                }
                directory
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "/".into())
            }
            WorkspaceLocation::Remote {
                key,
                remote_home_identity,
                ..
            } => {
                let directory = self
                    .remote_display_directory()
                    .expect("Remote Workspace has a remote directory")
                    .as_str();
                let destination = key.destination().as_str();
                if matches!(directory, "~" | "~/") || directory == remote_home_identity.as_str() {
                    return destination.to_owned();
                }
                let basename = directory
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("/");
                format!("{basename} · {destination}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_directory(path: &str) -> CurrentDirectory {
        CurrentDirectory::Local(PathBuf::from(path))
    }

    #[test]
    fn identity_name_and_path_should_follow_pin_and_resume_latest_primary_directory() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        let id = workspaces.active_workspace_id();
        workspaces.update_identity_directory(id, local_directory("/projects/alpha"));
        assert_eq!(workspaces.active_workspace().name(), "alpha");
        workspaces
            .set_pinned_directory(
                id,
                Some(PinnedDirectory::Local(ValidatedLocalDirectory::new(
                    PathBuf::from("/projects/pinned"),
                    LocalDirectoryIdentity::for_test(1),
                ))),
            )
            .unwrap();
        workspaces.update_identity_directory(id, local_directory("/projects/beta"));
        assert_eq!(workspaces.active_workspace().name(), "pinned");
        assert_eq!(
            workspaces.active_workspace().local_display_directory(),
            Some(Path::new("/projects/pinned"))
        );
        workspaces.rename_workspace(id, "Custom".into()).unwrap();
        workspaces.set_pinned_directory(id, None).unwrap();
        assert_eq!(workspaces.active_workspace().name(), "Custom");
        assert_eq!(
            workspaces.active_workspace().local_display_directory(),
            Some(Path::new("/projects/beta"))
        );
        workspaces.rename_workspace(id, String::new()).unwrap();
        assert_eq!(workspaces.active_workspace().name(), "beta");
        assert_eq!(
            workspaces.active_workspace().local_home_directory(),
            Some(Path::new("/home/test"))
        );
    }

    #[test]
    fn automatic_names_should_avoid_generated_suffixes_and_custom_names() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        for directory in ["/one/project", "/two/project", "/three/project 2"] {
            let id = workspaces
                .create_local_workspace_unchecked(PathBuf::from("/home/test"), |_, _| ())
                .unwrap();
            workspaces.update_identity_directory(id, local_directory(directory));
        }
        workspaces
            .rename_workspace(WorkspaceId::new(1), "project".into())
            .unwrap();
        let names: Vec<_> = workspaces
            .iter()
            .map(|workspace| workspace.name())
            .collect();
        assert_eq!(names, ["project", "project 2", "project 3", "project 2 2"]);
    }

    #[test]
    fn remote_identity_should_preserve_destination_and_reject_local_reports() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/local"), |_, _| ());
        let home = RemoteDirectoryIdentity::new("/home/remote".into()).unwrap();
        let id = workspaces
            .create_remote_workspace(
                RemoteWorkspaceTarget::new(
                    SshDestination::new("build".into()).unwrap(),
                    home.clone(),
                ),
                RemoteDirectory::new("~".into()).unwrap(),
                home,
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();
        workspaces.update_identity_directory(
            id,
            CurrentDirectory::Remote(RemoteDirectory::new("/srv/project".into()).unwrap()),
        );
        assert_eq!(workspaces.active_workspace().name(), "project · build");
        workspaces.update_identity_directory(id, local_directory("/wrong/machine"));
        assert_eq!(workspaces.active_workspace().name(), "project · build");
        assert!(
            workspaces
                .active_workspace()
                .local_display_directory()
                .is_none()
        );
    }
}
