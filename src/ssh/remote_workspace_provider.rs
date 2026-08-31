use std::sync::Arc;

use gpui::{BackgroundExecutor, Task};

use super::cancellation::SshCancellationToken;
use super::remote_utility::{
    PreparedSshRemoteUtilityCommand, RemoteDirectoryProbe, RemoteUtilityError,
    SshRemoteUtilityClient, SshRemoteUtilityRunner,
};
use crate::domain::{RemoteDirectoryIdentity, RemoteWorkspaceDirectory};
use crate::ui::remote_workspace_picker::{
    RemoteWorkspaceAccount, RemoteWorkspaceDirectoryListing, RemoteWorkspaceDirectoryRow,
    RemoteWorkspaceExactPathState, RemoteWorkspaceProvider, RemoteWorkspaceProviderError,
};

pub(crate) struct SshRemoteWorkspaceProvider<R: SshRemoteUtilityRunner> {
    client: Arc<SshRemoteUtilityClient<R>>,
    executor: BackgroundExecutor,
}

impl<R: SshRemoteUtilityRunner> SshRemoteWorkspaceProvider<R> {
    pub(crate) fn new(
        command: PreparedSshRemoteUtilityCommand,
        runner: Arc<R>,
        cancellation: SshCancellationToken,
        executor: BackgroundExecutor,
    ) -> Self {
        Self {
            client: Arc::new(SshRemoteUtilityClient::new(command, runner, cancellation)),
            executor,
        }
    }
}

impl<R: SshRemoteUtilityRunner> RemoteWorkspaceProvider for SshRemoteWorkspaceProvider<R> {
    fn discover_account(
        &self,
    ) -> Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>> {
        let client = Arc::clone(&self.client);
        self.executor.spawn(async move {
            let metadata = client.discover_account().await.map_err(map_error)?;
            let home_identity = RemoteDirectoryIdentity::new(metadata.physical_home().to_owned())
                .map_err(|_| RemoteWorkspaceProviderError::InvalidResponse)?;
            RemoteWorkspaceAccount::new(
                metadata.user().to_owned(),
                home_identity,
                metadata.login_shell().to_owned(),
            )
            .map_err(|_| RemoteWorkspaceProviderError::InvalidResponse)
        })
    }

    fn list_directories(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>> {
        let client = Arc::clone(&self.client);
        self.executor.spawn(async move {
            let listing = client
                .list_directories(directory)
                .await
                .map_err(map_error)?;
            let rows = listing
                .names()
                .iter()
                .map(|name| RemoteWorkspaceDirectoryRow::new(name.clone()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| RemoteWorkspaceProviderError::InvalidResponse)?;
            Ok(RemoteWorkspaceDirectoryListing::from_remote(
                rows,
                listing.is_truncated(),
            ))
        })
    }

    fn probe_exact_path(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>> {
        let client = Arc::clone(&self.client);
        self.executor.spawn(async move {
            client
                .probe_exact_path(directory)
                .await
                .map(|state| match state {
                    RemoteDirectoryProbe::ReadableDirectory => {
                        RemoteWorkspaceExactPathState::ReadableDirectory
                    }
                    RemoteDirectoryProbe::Missing => RemoteWorkspaceExactPathState::Missing,
                })
                .map_err(map_error)
        })
    }

    fn create_directory_recursively(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<(), RemoteWorkspaceProviderError>> {
        let client = Arc::clone(&self.client);
        self.executor.spawn(async move {
            client
                .create_directory_recursively(directory)
                .await
                .map_err(map_error)
        })
    }

    fn validate_physical_identity(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>> {
        let client = Arc::clone(&self.client);
        self.executor.spawn(async move {
            let physical = client
                .resolve_physical_directory(directory)
                .await
                .map_err(map_error)?;
            RemoteDirectoryIdentity::new(physical)
                .map_err(|_| RemoteWorkspaceProviderError::InvalidResponse)
        })
    }
}

fn map_error(error: RemoteUtilityError) -> RemoteWorkspaceProviderError {
    match error {
        RemoteUtilityError::Cancelled | RemoteUtilityError::Transport => {
            RemoteWorkspaceProviderError::ConnectionLost
        }
        RemoteUtilityError::CommandFailed(Some(255)) => {
            RemoteWorkspaceProviderError::ConnectionLost
        }
        RemoteUtilityError::Missing => RemoteWorkspaceProviderError::Missing,
        RemoteUtilityError::NotDirectory => RemoteWorkspaceProviderError::NotDirectory,
        RemoteUtilityError::PermissionDenied => RemoteWorkspaceProviderError::PermissionDenied,
        RemoteUtilityError::RequestTooLarge
        | RemoteUtilityError::OutputTooLarge
        | RemoteUtilityError::InvalidResponse => RemoteWorkspaceProviderError::InvalidResponse,
        RemoteUtilityError::CommandFailed(_) | RemoteUtilityError::RemoteFailed => {
            RemoteWorkspaceProviderError::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use gpui::TestAppContext;

    use super::*;
    use crate::domain::SshDestination;
    use crate::ssh::command::{SshCommandContext, SshCommandSpec};
    use crate::ssh::process::ProcessExit;
    use crate::ssh::remote_utility::{
        RemoteUtilityProcessOutput, RemoteUtilityRunError, SshRemoteUtilityRunner,
    };

    struct FakeRunner {
        outputs: Mutex<VecDeque<Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>>>,
    }

    impl FakeRunner {
        fn new(outputs: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                outputs: Mutex::new(
                    outputs
                        .into_iter()
                        .map(|stdout| {
                            Ok(RemoteUtilityProcessOutput::new(
                                ProcessExit::successful(),
                                stdout,
                            ))
                        })
                        .collect(),
                ),
            }
        }
    }

    impl SshRemoteUtilityRunner for FakeRunner {
        fn run(
            &self,
            _command: Arc<SshCommandSpec>,
            _script: Vec<u8>,
            _maximum_output_bytes: usize,
            _cancellation: SshCancellationToken,
        ) -> impl Future<Output = Result<RemoteUtilityProcessOutput, RemoteUtilityRunError>> + Send
        {
            let output = self.outputs.lock().unwrap().pop_front().unwrap();
            async move { output }
        }
    }

    fn response(kind: &str, status: &str, fields: &[&str], tail: &str) -> Vec<u8> {
        let mut response = format!("SPACETERM-REMOTE/1\n{kind}\n{status}\n").into_bytes();
        for field in fields {
            response.extend_from_slice(format!("{}:", field.len()).as_bytes());
            response.extend_from_slice(field.as_bytes());
            response.push(b',');
        }
        response.extend_from_slice(b".\n");
        response.extend_from_slice(tail.as_bytes());
        response
    }

    fn provider(
        cx: &TestAppContext,
        outputs: impl IntoIterator<Item = Vec<u8>>,
    ) -> SshRemoteWorkspaceProvider<FakeRunner> {
        let command = SshCommandContext::new(
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            SshDestination::new("remote".to_owned()).unwrap(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap()
        .remote_utility();
        SshRemoteWorkspaceProvider::new(
            PreparedSshRemoteUtilityCommand::new(command),
            Arc::new(FakeRunner::new(outputs)),
            SshCancellationToken::default(),
            cx.executor(),
        )
    }

    fn directory(value: &str) -> RemoteWorkspaceDirectory {
        RemoteWorkspaceDirectory::new(value.to_owned()).unwrap()
    }

    #[gpui::test]
    fn provider_should_map_all_picker_operations(cx: &mut TestAppContext) {
        let provider = provider(
            cx,
            [
                response(
                    "account",
                    "ok",
                    &["tester", "501", "/home/tester", "/bin/zsh", "/home/tester"],
                    "",
                ),
                response("list", "ok", &["Space Term", "-archive"], "1\n"),
                response("probe", "missing", &[], ""),
                response("mkdir", "ok", &[], ""),
                response("physical", "ok", &["/srv/physical"], ""),
            ],
        );

        let account = cx.executor().block(provider.discover_account()).unwrap();
        assert_eq!(account.user(), "tester");
        assert_eq!(account.home_identity().as_str(), "/home/tester");
        assert_eq!(account.login_shell(), "/bin/zsh");

        let listing = cx
            .executor()
            .block(provider.list_directories(directory("/srv")))
            .unwrap();
        assert_eq!(
            listing
                .rows()
                .iter()
                .map(RemoteWorkspaceDirectoryRow::name)
                .collect::<Vec<_>>(),
            ["Space Term", "-archive"]
        );
        assert!(listing.is_truncated());
        assert_eq!(
            cx.executor()
                .block(provider.probe_exact_path(directory("/srv/missing")))
                .unwrap(),
            RemoteWorkspaceExactPathState::Missing
        );
        cx.executor()
            .block(provider.create_directory_recursively(directory("/srv/new")))
            .unwrap();
        assert_eq!(
            cx.executor()
                .block(provider.validate_physical_identity(directory("/srv/link")))
                .unwrap()
                .as_str(),
            "/srv/physical"
        );
    }

    #[gpui::test]
    fn provider_should_reject_remote_names_the_picker_cannot_represent(cx: &mut TestAppContext) {
        let provider = provider(cx, [response("list", "ok", &["line\nbreak"], "0\n")]);

        assert_eq!(
            cx.executor()
                .block(provider.list_directories(directory("/srv")))
                .unwrap_err(),
            RemoteWorkspaceProviderError::InvalidResponse
        );
    }
}
