use super::*;

impl<T> WorkspaceCollection<T> {
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

    #[test]
    fn automatic_names_should_avoid_generated_suffixes_and_custom_names() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/test"), |_, _| ());
        for (index, directory) in ["/one/project", "/two/project", "/three/project 2"]
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
        assert_eq!(names, ["project", "project 2", "project 3", "project 2 2"]);
    }

    #[test]
    fn remote_name_should_preserve_destination_and_starting_directory() {
        let mut workspaces = WorkspaceCollection::new(PathBuf::from("/home/local"), |_, _| ());
        let home = RemoteDirectoryIdentity::new("/home/remote".into()).unwrap();
        let id = workspaces
            .create_remote_workspace(
                RemoteWorkspaceTarget::new(
                    SshDestination::new("build".into()).unwrap(),
                    home.clone(),
                ),
                RemoteDirectory::new("/srv/project".into()).unwrap(),
                home,
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();

        assert_eq!(workspaces.workspace(id).unwrap().name(), "project · build");
    }
}
