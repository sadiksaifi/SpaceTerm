//! Owns launch authority and the complete process-plan to worker handoff.
use super::*;

enum TerminalSessionProcess {
    Local {
        planner: ShellLaunchPlanner,
        directory: std::path::PathBuf,
        prompt_owner: Option<crate::terminal::metadata::PromptOwner>,
    },
    Remote {
        home: std::path::PathBuf,
        command: crate::ssh::command::SshCommandSpec,
    },
}

impl TerminalSessionProcess {
    fn prepare(self) -> Result<PreparedShellLaunch, NativePtyStartupFailure> {
        match self {
            Self::Local {
                planner,
                directory,
                prompt_owner,
            } => {
                let launch = planner.local(&directory)?;
                Ok(match prompt_owner {
                    Some(owner) => launch.with_environment_value(
                        crate::terminal::metadata::PromptOwner::ENVIRONMENT,
                        owner.value(),
                    ),
                    None => launch,
                })
            }
            Self::Remote { home, command } => Ok(PreparedShellLaunch::remote(&home, command)?),
        }
    }
}

struct TerminalSessionLaunch {
    metadata: TerminalMetadataContext,
    fallback_title: String,
    process: TerminalSessionProcess,
}

impl TerminalSessionLaunch {
    fn local(
        planner: ShellLaunchPlanner,
        directory: &Path,
        machine: LocalMachine,
        filesystem: &LocalFilesystemAuthority,
    ) -> Self {
        let prompt_owner = crate::terminal::metadata::PromptOwner::generate();
        let metadata = TerminalMetadataContext::local(
            filesystem.path_semantics(),
            &directory.to_string_lossy(),
            machine,
        );
        Self {
            metadata: match &prompt_owner {
                Some(owner) => metadata.with_prompt_owner(owner.clone()),
                None => metadata,
            },
            fallback_title: planner.fallback_title(),
            process: TerminalSessionProcess::Local {
                planner,
                directory: directory.to_owned(),
                prompt_owner,
            },
        }
    }

    fn remote(remote: RemoteTerminalLaunchPlan) -> Result<Self, TerminalSessionError> {
        let command = remote.terminal_session_channel.take()?;
        Ok(Self {
            metadata: TerminalMetadataContext::Remote(remote.metadata_context),
            fallback_title: remote.fallback_title,
            process: TerminalSessionProcess::Remote {
                home: remote.local_home.path().to_owned(),
                command,
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A local Terminal Session launch bound to one validated local directory.
///
/// Its directory is local filesystem authority and is the only launch directory passed to local
/// process `chdir` and identity validation.
pub(crate) struct LocalTerminalLaunchPlan {
    working_directory: crate::domain::ValidatedLocalDirectory,
}

#[derive(Clone, Eq, PartialEq)]
/// A Remote Terminal Session launch bound to one prepared Terminal Session Channel.
///
/// `local_home` is used only as the local SSH process working directory. Destination and Remote
/// Starting Directory remain typed remote metadata and must never enter `PathBuf`, local `chdir`,
/// or local filesystem validation. The prepared channel is single-use and sensitive Debug output
/// is deliberately redacted.
pub(crate) struct RemoteTerminalLaunchPlan {
    local_home: crate::domain::ValidatedLocalDirectory,
    metadata_context: RemoteTerminalMetadataContext,
    fallback_title: String,
    terminal_session_channel: crate::ssh::command::PreparedSshTerminalSessionChannelCommand,
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
        local_home: crate::domain::ValidatedLocalDirectory,
        metadata_context: RemoteTerminalMetadataContext,
        fallback_title: String,
        terminal_session_channel: crate::ssh::command::PreparedSshTerminalSessionChannelCommand,
    ) -> Self {
        Self {
            local_home,
            metadata_context,
            fallback_title,
            terminal_session_channel,
        }
    }

    #[cfg(test)]
    pub(crate) const fn local_home(&self) -> &crate::domain::ValidatedLocalDirectory {
        &self.local_home
    }

    #[cfg(test)]
    pub(crate) const fn destination(&self) -> &crate::domain::SshDestination {
        self.metadata_context.destination()
    }

    pub(crate) const fn remote_directory(&self) -> &crate::domain::RemoteDirectory {
        self.metadata_context.initial_directory()
    }

    pub(crate) fn fallback_title(&self) -> &str {
        &self.fallback_title
    }

    pub(crate) const fn metadata_context(&self) -> &RemoteTerminalMetadataContext {
        &self.metadata_context
    }

    #[cfg(test)]
    pub(crate) fn take_terminal_session_channel(
        &self,
    ) -> Result<
        crate::ssh::command::SshCommandSpec,
        crate::ssh::command::PreparedSshTerminalSessionChannelError,
    > {
        self.terminal_session_channel.take()
    }
}

impl LocalTerminalLaunchPlan {
    pub(crate) const fn new(working_directory: crate::domain::ValidatedLocalDirectory) -> Self {
        Self { working_directory }
    }

    pub(crate) const fn working_directory(&self) -> &crate::domain::ValidatedLocalDirectory {
        &self.working_directory
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The exhaustive Local or Remote launch authority consumed by a Terminal Session factory.
///
/// Matching this enum is the boundary at which local path capabilities and Terminal Session Channel
/// ownership diverge; callers must not reconstruct one variant from the other's directory data.
pub(crate) enum TerminalLaunchPlan {
    Local(LocalTerminalLaunchPlan),
    Remote(Box<RemoteTerminalLaunchPlan>),
}

#[derive(Clone)]
pub(crate) struct NativeTerminalSessionFactory {
    local_filesystem: LocalFilesystemAuthority,
    local_machine: LocalMachine,
    native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
    launch_planner: ShellLaunchPlanner,
}

impl NativeTerminalSessionFactory {
    pub(crate) fn new(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        local_filesystem: LocalFilesystemAuthority,
        local_machine: LocalMachine,
    ) -> Self {
        Self {
            local_filesystem,
            local_machine,
            native_pty_adapter_factory,
            launch_planner,
        }
    }
}

impl TerminalSessionFactory for NativeTerminalSessionFactory {
    fn start(
        &self,
        geometry: TerminalGeometry,
        launch_plan: TerminalLaunchPlan,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSession, TerminalSessionError> {
        let launch = match launch_plan {
            TerminalLaunchPlan::Local(local) => TerminalSessionLaunch::local(
                self.launch_planner.clone(),
                local.working_directory().path(),
                self.local_machine.clone(),
                &self.local_filesystem,
            ),
            TerminalLaunchPlan::Remote(remote) => TerminalSessionLaunch::remote(*remote)?,
        };
        let (session, events, accessibility) = TerminalSession::start_launch(
            Arc::clone(&self.native_pty_adapter_factory),
            geometry,
            launch,
            self.local_filesystem.clone(),
            initial_appearance,
        )?;
        Ok(StartedTerminalSession {
            clipboard: session.clipboard_requests.clone(),
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
    #[cfg(test)]
    pub(crate) fn start(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        geometry: TerminalGeometry,
        working_directory: &Path,
        local_hostname: Option<&str>,
        local_filesystem: LocalFilesystemAuthority,
    ) -> Result<StartedTerminalSessionParts, TerminalSessionError> {
        let launch = TerminalSessionLaunch::local(
            launch_planner,
            working_directory,
            LocalMachine::new(None, local_hostname, None),
            &local_filesystem,
        );
        Self::start_launch(
            native_pty_adapter_factory,
            geometry,
            launch,
            local_filesystem,
            test_terminal_appearance_update(),
        )
    }

    fn start_launch(
        factory: Arc<dyn NativePtyAdapterFactory>,
        geometry: TerminalGeometry,
        launch: TerminalSessionLaunch,
        filesystem: LocalFilesystemAuthority,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<StartedTerminalSessionParts, TerminalSessionError> {
        Self::start_deferred_with_context(
            geometry,
            launch.metadata,
            launch.fallback_title,
            filesystem,
            initial_appearance,
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
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        ) -> Result<NativePtyOwner, NativePtyStartupFailure>
        + Send
        + 'static,
    ) -> Result<StartedTerminalSessionParts, TerminalSessionError> {
        let initial_directory = working_directory.to_string_lossy();
        let metadata_context = TerminalMetadataContext::local(
            crate::local_path::LocalPathSemantics::Posix,
            &initial_directory,
            LocalMachine::new(None, Some("fixture.test"), None),
        );
        Self::start_deferred_with_context(
            geometry,
            metadata_context,
            test_launch_planner().fallback_title(),
            LocalFilesystemAuthority::testing(),
            test_terminal_appearance_update(),
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
        local_filesystem: LocalFilesystemAuthority,
        initial_appearance: TerminalAppearanceUpdate,
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        )
            -> Result<(NativePtyOwner, &'static str), NativePtyStartupFailure>
        + Send
        + 'static,
    ) -> Result<StartedTerminalSessionParts, TerminalSessionError> {
        let (command_tx, command_rx) = mpsc::channel();
        let (clipboard, clipboard_requests) = WorkerClipboard::connect(command_tx.clone());
        let clipboard_authority = Arc::clone(&clipboard.authority);
        let reader_transport =
            ReaderTransport::new(command_tx.clone(), Arc::clone(&clipboard_authority));
        let schedule_input = ScheduleInput::default();
        let worker_schedule_input = schedule_input.clone();
        let metadata_state = TerminalSessionMetadataState::default();
        let worker_metadata_state = metadata_state.clone();
        let permission_request_state = TerminalSessionPermissionRequestState::default();
        let worker_permission_request_state = permission_request_state.clone();
        let (event_tx, event_rx) = async_channel::bounded(2);
        let (accessibility_tx, accessibility_rx) = async_channel::bounded(1);
        let native_pty_close = NativePtyCloseHandle::default();
        let worker_native_pty_close = native_pty_close.clone();
        let native_pty_output = reader_transport.output_sink();
        let worker_events = event_tx.clone();

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
                            NativePtyStartupStage::Adapter => TerminalSessionStartupStage::Pty,
                            NativePtyStartupStage::Reader => TerminalSessionStartupStage::Reader,
                            NativePtyStartupStage::ReaderThread => {
                                TerminalSessionStartupStage::ReaderThread
                            }
                        };
                        send_session_event(
                            &worker_events,
                            TerminalSessionEvent::Failed(TerminalSessionFailure::Startup {
                                stage,
                                message: error.to_string(),
                            }),
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
                        local_filesystem,
                        initial_appearance,
                    },
                    command_rx,
                    reader_transport,
                    worker_schedule_input,
                    TerminalWorkerPublishers {
                        clipboard,
                        metadata_state: worker_metadata_state,
                        permission_request_state: worker_permission_request_state,
                        events: event_tx,
                        accessibility: accessibility_tx,
                    },
                );
            })
            .map_err(TerminalSessionError::from)?;

        Ok((
            Self {
                metadata_state,
                permission_request_state,
                commands: Some(command_tx),
                worker: Some(worker),
                native_pty_close: Some(native_pty_close),
                schedule_input,
                clipboard_requests,
                clipboard_authority,
            },
            event_rx,
            accessibility_rx,
        ))
    }
}
