//! Owns launch authority and the complete process-plan to worker handoff.
use super::*;

enum SessionProcess {
    Local {
        planner: ShellLaunchPlanner,
        directory: std::path::PathBuf,
    },
    Remote {
        home: std::path::PathBuf,
        command: crate::ssh::command::SshCommandSpec,
    },
}

impl SessionProcess {
    fn prepare(self) -> Result<PreparedShellLaunch, NativePtyStartupFailure> {
        match self {
            Self::Local { planner, directory } => Ok(planner.local(&directory)?),
            Self::Remote { home, command } => Ok(PreparedShellLaunch::remote(&home, command)?),
        }
    }
}

struct SessionLaunch {
    metadata: TerminalMetadataContext,
    fallback_title: String,
    process: SessionProcess,
}

impl SessionLaunch {
    fn local(
        planner: ShellLaunchPlanner,
        directory: &Path,
        hostname: Option<&str>,
        filesystem: &LocalFilesystemAuthority,
    ) -> Self {
        Self {
            metadata: TerminalMetadataContext::local(
                filesystem.path_semantics(),
                &directory.to_string_lossy(),
                hostname,
            ),
            fallback_title: planner.fallback_title(),
            process: SessionProcess::Local {
                planner,
                directory: directory.to_owned(),
            },
        }
    }

    fn remote(remote: RemoteTerminalLaunchPlan) -> Result<Self, SessionError> {
        let command = remote.pane_channel.take()?;
        Ok(Self {
            metadata: TerminalMetadataContext::Remote(remote.metadata_context),
            fallback_title: remote.fallback_title,
            process: SessionProcess::Remote {
                home: remote.local_home.path().to_owned(),
                command,
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A local Terminal Session launch bound to one validated local Workspace Directory.
///
/// Its directory is local filesystem authority and is the only launch directory passed to local
/// process `chdir` and identity validation.
pub(crate) struct LocalTerminalLaunchPlan {
    working_directory: crate::domain::ValidatedWorkspaceDirectory,
}

#[derive(Clone, Eq, PartialEq)]
/// A Remote Terminal Session launch bound to one prepared OpenSSH Pane channel.
///
/// `local_home` is used only as the local SSH process working directory. Destination and Remote
/// Workspace Directory remain typed remote metadata and must never enter `PathBuf`, local `chdir`,
/// or local filesystem validation. The prepared channel is single-use and sensitive Debug output
/// is deliberately redacted.
pub(crate) struct RemoteTerminalLaunchPlan {
    local_home: crate::domain::ValidatedWorkspaceDirectory,
    metadata_context: RemoteTerminalMetadataContext,
    fallback_title: String,
    pane_channel: crate::ssh::command::PreparedSshPaneChannelCommand,
}

impl fmt::Debug for RemoteTerminalLaunchPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteTerminalLaunchPlan")
            .finish_non_exhaustive()
    }
}

impl RemoteTerminalLaunchPlan {
    pub(crate) const fn new(
        local_home: crate::domain::ValidatedWorkspaceDirectory,
        destination: crate::domain::SshDestination,
        remote_directory: crate::domain::RemoteWorkspaceDirectory,
        fallback_title: String,
        pane_channel: crate::ssh::command::PreparedSshPaneChannelCommand,
    ) -> Self {
        Self {
            local_home,
            metadata_context: RemoteTerminalMetadataContext::new(destination, remote_directory),
            fallback_title,
            pane_channel,
        }
    }

    #[cfg(test)]
    pub(crate) const fn local_home(&self) -> &crate::domain::ValidatedWorkspaceDirectory {
        &self.local_home
    }

    #[cfg(test)]
    pub(crate) const fn destination(&self) -> &crate::domain::SshDestination {
        self.metadata_context.destination()
    }

    #[cfg(test)]
    pub(crate) const fn remote_directory(&self) -> &crate::domain::RemoteWorkspaceDirectory {
        self.metadata_context.initial_directory()
    }

    #[cfg(test)]
    pub(crate) fn fallback_title(&self) -> &str {
        &self.fallback_title
    }

    #[cfg(test)]
    pub(crate) const fn metadata_context(&self) -> &RemoteTerminalMetadataContext {
        &self.metadata_context
    }

    #[cfg(test)]
    pub(crate) fn take_pane_channel(
        &self,
    ) -> Result<crate::ssh::command::SshCommandSpec, crate::ssh::command::PreparedSshPaneChannelError>
    {
        self.pane_channel.take()
    }
}

impl LocalTerminalLaunchPlan {
    pub(crate) const fn new(working_directory: crate::domain::ValidatedWorkspaceDirectory) -> Self {
        Self { working_directory }
    }

    pub(crate) const fn working_directory(&self) -> &crate::domain::ValidatedWorkspaceDirectory {
        &self.working_directory
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The exhaustive Local or Remote launch authority consumed by a Terminal Session factory.
///
/// Matching this enum is the boundary at which local path capabilities and remote channel
/// ownership diverge; callers must not reconstruct one variant from the other's directory data.
pub(crate) enum TerminalLaunchPlan {
    Local(LocalTerminalLaunchPlan),
    Remote(Box<RemoteTerminalLaunchPlan>),
}

#[derive(Clone)]
pub(crate) struct NativeTerminalSessionFactory {
    local_filesystem: LocalFilesystemAuthority,
    local_hostname: Option<String>,
    native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
    launch_planner: ShellLaunchPlanner,
    osc52_clipboard_factory: Arc<dyn Osc52ClipboardFactory>,
}

impl NativeTerminalSessionFactory {
    pub(crate) fn new(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        osc52_clipboard_factory: Arc<dyn Osc52ClipboardFactory>,
        local_filesystem: LocalFilesystemAuthority,
        local_hostname: Option<String>,
    ) -> Self {
        Self {
            local_filesystem,
            local_hostname,
            native_pty_adapter_factory,
            launch_planner,
            osc52_clipboard_factory,
        }
    }
}

impl TerminalSessionFactory for NativeTerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        launch_plan: TerminalLaunchPlan,
    ) -> Result<StartedTerminalSession, SessionError> {
        self.start_observed(geometry, launch_plan, None)
    }

    fn start_observed(
        &self,
        geometry: TerminalGeometry,
        launch_plan: TerminalLaunchPlan,
        observation: Option<crate::observation::SessionObservationLease>,
    ) -> Result<StartedTerminalSession, SessionError> {
        let observation =
            observation.and_then(crate::observation::SessionObservationLease::consume);

        let launch = match launch_plan {
            TerminalLaunchPlan::Local(local) => SessionLaunch::local(
                self.launch_planner.clone(),
                local.working_directory().path(),
                self.local_hostname.as_deref(),
                &self.local_filesystem,
            ),
            TerminalLaunchPlan::Remote(remote) => SessionLaunch::remote(*remote)?,
        };
        let (session, events, accessibility) = TerminalSession::start_launch(
            Arc::clone(&self.native_pty_adapter_factory),
            geometry,
            launch,
            observation,
            Arc::clone(&self.osc52_clipboard_factory),
            self.local_filesystem.clone(),
        )?;
        Ok(StartedTerminalSession {
            handle: Box::new(session),
            events,
            accessibility,
        })
    }

    fn fallback_title(&self) -> String {
        self.launch_planner.fallback_title()
    }
}

impl TerminalSession {
    #[expect(
        clippy::too_many_arguments,
        reason = "independent session dependencies are injected at construction"
    )]
    #[cfg(test)]
    pub(crate) fn start(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        geometry: TerminalGeometry,
        working_directory: &Path,
        local_hostname: Option<&str>,
        runtime_observation: Option<RuntimeObservation>,
        osc52_clipboard_factory: Arc<dyn Osc52ClipboardFactory>,
        local_filesystem: LocalFilesystemAuthority,
    ) -> Result<StartedSession, SessionError> {
        let launch = SessionLaunch::local(
            launch_planner,
            working_directory,
            local_hostname,
            &local_filesystem,
        );
        Self::start_launch(
            native_pty_adapter_factory,
            geometry,
            launch,
            runtime_observation,
            osc52_clipboard_factory,
            local_filesystem,
        )
    }

    fn start_launch(
        factory: Arc<dyn NativePtyAdapterFactory>,
        geometry: TerminalGeometry,
        launch: SessionLaunch,
        observation: Option<RuntimeObservation>,
        clipboard: Arc<dyn Osc52ClipboardFactory>,
        filesystem: LocalFilesystemAuthority,
    ) -> Result<StartedSession, SessionError> {
        Self::start_deferred_with_context(
            geometry,
            launch.metadata,
            launch.fallback_title,
            observation,
            clipboard,
            filesystem,
            move |size, output, close_handle| {
                let prepared = launch.process.prepare()?;
                let terminal_name = prepared.terminal_name();
                let owner =
                    NativePtyOwner::start(factory.as_ref(), prepared, size, output, close_handle)?;
                Ok((owner, terminal_name))
            },
        )
    }

    #[cfg(test)]
    pub(super) fn start_deferred_with(
        geometry: TerminalGeometry,
        working_directory: &Path,
        runtime_observation: Option<RuntimeObservation>,
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        ) -> Result<NativePtyOwner, NativePtyStartupFailure>
        + Send
        + 'static,
    ) -> Result<StartedSession, SessionError> {
        let initial_directory = working_directory.to_string_lossy();
        let metadata_context = TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            &initial_directory,
            Some("fixture.test"),
        );
        Self::start_deferred_with_context(
            geometry,
            metadata_context,
            test_launch_planner().fallback_title(),
            runtime_observation,
            Arc::new(UnavailableOsc52ClipboardFactory),
            LocalFilesystemAuthority::testing(),
            move |size, output, close_handle| {
                start_native_pty(size, output, close_handle)
                    .map(|owner| (owner, identity::TERM_FALLBACK))
            },
        )
    }

    pub(super) fn start_deferred_with_context(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: String,
        runtime_observation: Option<RuntimeObservation>,
        osc52_clipboard_factory: Arc<dyn Osc52ClipboardFactory>,
        local_filesystem: LocalFilesystemAuthority,
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        )
            -> Result<(NativePtyOwner, &'static str), NativePtyStartupFailure>
        + Send
        + 'static,
    ) -> Result<StartedSession, SessionError> {
        let (command_tx, command_rx) = mpsc::channel();
        let reader_transport = ReaderTransport::new(command_tx.clone());
        let resizes = ResizeMailbox::default();
        let worker_resizes = resizes.clone();
        let find_queries = FindQueryMailbox::default();
        let worker_find_queries = find_queries.clone();
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (accessibility_tx, accessibility_rx) = async_channel::bounded(1);
        let native_pty_close = NativePtyCloseHandle::default();
        let worker_native_pty_close = native_pty_close.clone();
        let native_pty_output = reader_transport.output_sink();
        let worker_events = event_tx.clone();
        let worker_observation = runtime_observation.clone();

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                let (native_pty, terminal_name) = match start_native_pty(
                    pty_size(geometry),
                    native_pty_output,
                    &worker_native_pty_close,
                ) {
                    Ok(owner) => owner,
                    Err(error) => {
                        let stage = match error.stage() {
                            NativePtyStartupStage::Adapter => SessionStartupStage::Pty,
                            NativePtyStartupStage::Reader => SessionStartupStage::Reader,
                            NativePtyStartupStage::ReaderThread => {
                                SessionStartupStage::ReaderThread
                            }
                        };
                        send_session_event(
                            &worker_events,
                            SessionEvent::Failed(SessionFailure::Startup {
                                stage,
                                message: error.to_string(),
                            }),
                            worker_observation.as_ref(),
                        );
                        return;
                    }
                };
                TerminalWorker::run(
                    native_pty,
                    TerminalWorkerContext {
                        initial_geometry: geometry,
                        metadata_context,
                        fallback_title,
                        terminal_name,
                        osc52_clipboard_factory,
                        local_filesystem,
                    },
                    command_rx,
                    reader_transport,
                    TerminalWorkerMailboxes {
                        resizes: worker_resizes,
                        find_queries: worker_find_queries,
                    },
                    TerminalWorkerPublishers {
                        events: event_tx,
                        accessibility: accessibility_tx,
                        runtime_observation: worker_observation.clone(),
                    },
                    StartupReporter::Events(worker_events, worker_observation),
                );
            })
            .map_err(SessionError::SpawnWorker)?;

        Ok((
            Self {
                commands: Some(command_tx),
                worker: Some(worker),
                native_pty_close: Some(native_pty_close),
                resizes,
                find_queries,
                runtime_observation,
            },
            event_rx,
            accessibility_rx,
        ))
    }

    #[cfg(test)]
    pub(super) fn start_with(
        geometry: TerminalGeometry,
        working_directory: &Path,
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        ) -> Result<NativePtyOwner, NativePtyStartupFailure>,
    ) -> Result<StartedSession, SessionError> {
        let worker_directory = working_directory.to_owned();
        let metadata_context = TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            &worker_directory.to_string_lossy(),
            Some("fixture.test"),
        );
        let terminal_name = identity::TERM_FALLBACK;
        let (command_tx, command_rx) = mpsc::channel();
        let reader_transport = ReaderTransport::new(command_tx.clone());
        let native_pty_close = NativePtyCloseHandle::default();
        let native_pty = start_native_pty(
            pty_size(geometry),
            reader_transport.output_sink(),
            &native_pty_close,
        )
        .map_err(|error| SessionError::EmulatorStartup(error.to_string()))?;
        // Two slots retain the latest screen and a final lifecycle event without
        // allowing sustained PTY output to build an unbounded UI backlog.
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (accessibility_tx, accessibility_rx) = async_channel::bounded(1);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let resizes = ResizeMailbox::default();
        let worker_resizes = resizes.clone();
        let find_queries = FindQueryMailbox::default();
        let worker_find_queries = find_queries.clone();

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                TerminalWorker::run(
                    native_pty,
                    TerminalWorkerContext {
                        initial_geometry: geometry,
                        metadata_context,
                        fallback_title: "Terminal".to_owned(),
                        terminal_name,
                        osc52_clipboard_factory: Arc::new(UnavailableOsc52ClipboardFactory),
                        local_filesystem: LocalFilesystemAuthority::testing(),
                    },
                    command_rx,
                    reader_transport,
                    TerminalWorkerMailboxes {
                        resizes: worker_resizes,
                        find_queries: worker_find_queries,
                    },
                    TerminalWorkerPublishers {
                        events: event_tx,
                        accessibility: accessibility_tx,
                        runtime_observation: None,
                    },
                    StartupReporter::Blocking(startup_tx),
                )
            })
            .map_err(SessionError::SpawnWorker)?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    commands: Some(command_tx),
                    worker: Some(worker),
                    native_pty_close: Some(native_pty_close),
                    resizes,
                    find_queries,
                    runtime_observation: None,
                },
                event_rx,
                accessibility_rx,
            )),
            Ok(Err(message)) => {
                join_worker(worker);
                Err(SessionError::EmulatorStartup(message))
            }
            Err(_) => {
                join_worker(worker);
                Err(SessionError::StartupChannelClosed)
            }
        }
    }
}
