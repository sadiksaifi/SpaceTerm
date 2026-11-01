//! Owns launch authority and the complete process-plan to worker handoff.
use super::*;

struct TerminalSessionProcess {
    planner: ShellLaunchPlanner,
    directory: std::path::PathBuf,
    prompt_owner: Option<crate::terminal::metadata::PromptOwner>,
    session: Option<crate::platform::shell_launch::SessionEnvironment>,
    directory_guard: Option<(
        crate::domain::ValidatedLocalDirectory,
        LocalFilesystemAuthority,
    )>,
}

impl TerminalSessionProcess {
    fn prepare(self) -> Result<PreparedShellLaunch, NativePtyStartupFailure> {
        if let Some((expected, filesystem)) = self.directory_guard {
            filesystem.revalidate_directory(&expected).map_err(|_| {
                crate::platform::shell_launch::ShellLaunchFailure::DirectoryUnavailable
            })?;
        }
        let mut launch = self.planner.local(&self.directory)?;
        if let Some(session) = &self.session {
            launch = launch.with_session_environment(session);
        }
        Ok(match self.prompt_owner {
            Some(owner) => launch.with_environment_value(
                crate::terminal::metadata::PromptOwner::ENVIRONMENT,
                owner.value(),
            ),
            None => launch,
        })
    }
}

struct TerminalSessionLaunch {
    metadata: TerminalMetadataContext,
    fallback_title: String,
    process: TerminalSessionProcess,
    restricted: bool,
}

pub(super) struct StartedPty {
    owner: NativePtyOwner,
    terminal_name: &'static str,
    restricted: bool,
    integration_expected: bool,
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
            restricted: false,
            metadata: match &prompt_owner {
                Some(owner) => metadata.with_prompt_owner(owner.clone()),
                None => metadata,
            },
            fallback_title: planner.fallback_title(),
            process: TerminalSessionProcess {
                planner,
                directory: directory.to_owned(),
                prompt_owner,
                session: None,
                directory_guard: None,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A local Terminal Session launch bound to one validated local directory.
///
/// Its directory is local filesystem authority and is the only launch directory passed to local
/// process `chdir` and identity validation.
pub struct LocalTerminalLaunchPlan {
    working_directory: crate::domain::ValidatedLocalDirectory,
    restricted: bool,
    session: Option<crate::platform::shell_launch::SessionEnvironment>,
}

impl LocalTerminalLaunchPlan {
    pub const fn new(working_directory: crate::domain::ValidatedLocalDirectory) -> Self {
        Self {
            working_directory,
            restricted: false,
            session: None,
        }
    }

    pub fn without_client_effects(mut self) -> Self {
        self.restricted = true;
        self
    }

    /// A server's Terminal Session, whose shell learns where it runs.
    pub fn in_session(
        mut self,
        session: crate::platform::shell_launch::SessionEnvironment,
    ) -> Self {
        self.session = Some(session);
        self
    }

    pub const fn working_directory(&self) -> &crate::domain::ValidatedLocalDirectory {
        &self.working_directory
    }
}

#[derive(Clone)]
pub struct NativeTerminalSessionFactory {
    local_filesystem: LocalFilesystemAuthority,
    local_machine: LocalMachine,
    native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
    launch_planner: ShellLaunchPlanner,
    counters: Arc<TerminalCounters>,
}

impl NativeTerminalSessionFactory {
    /// Each Terminal Session it starts records into `counters`.
    pub fn new(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        local_filesystem: LocalFilesystemAuthority,
        local_machine: LocalMachine,
        counters: Arc<TerminalCounters>,
    ) -> Self {
        Self {
            local_filesystem,
            local_machine,
            native_pty_adapter_factory,
            launch_planner,
            counters,
        }
    }

    /// The counters its Terminal Sessions record into.
    pub fn counters(&self) -> &Arc<TerminalCounters> {
        &self.counters
    }

    /// Starts an owned runtime without requiring a presentation or graphical session.
    pub fn start_runtime(
        &self,
        geometry: TerminalGeometry,
        local: LocalTerminalLaunchPlan,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<TerminalRuntime, TerminalSessionError> {
        let (runtime, subscription) = self.start_subscribed(geometry, local, initial_appearance)?;
        drop(subscription);
        Ok(runtime)
    }

    fn start_subscribed(
        &self,
        geometry: TerminalGeometry,
        local: LocalTerminalLaunchPlan,
        initial_appearance: TerminalAppearanceUpdate,
    ) -> Result<(TerminalRuntime, TerminalSubscription), TerminalSessionError> {
        let mut launch = TerminalSessionLaunch::local(
            self.launch_planner.clone(),
            local.working_directory().path(),
            self.local_machine.clone(),
            &self.local_filesystem,
        );
        launch.process.directory_guard = Some((
            local.working_directory().clone(),
            self.local_filesystem.clone(),
        ));
        launch.restricted = local.restricted;
        launch.process.session = local.session;
        TerminalRuntime::start_launch(
            Arc::clone(&self.native_pty_adapter_factory),
            geometry,
            launch,
            self.local_filesystem.clone(),
            initial_appearance,
            Arc::clone(&self.counters),
        )
    }
}

impl TerminalRuntime {
    #[cfg(test)]
    pub fn start(
        native_pty_adapter_factory: Arc<dyn NativePtyAdapterFactory>,
        launch_planner: ShellLaunchPlanner,
        geometry: TerminalGeometry,
        working_directory: &Path,
        local_hostname: Option<&str>,
        local_filesystem: LocalFilesystemAuthority,
        counters: Arc<TerminalCounters>,
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
            counters,
        )
        .map(|(runtime, subscription)| (runtime, subscription.events, subscription.accessibility))
    }

    fn start_launch(
        factory: Arc<dyn NativePtyAdapterFactory>,
        geometry: TerminalGeometry,
        launch: TerminalSessionLaunch,
        filesystem: LocalFilesystemAuthority,
        initial_appearance: TerminalAppearanceUpdate,
        counters: Arc<TerminalCounters>,
    ) -> Result<(TerminalRuntime, TerminalSubscription), TerminalSessionError> {
        let owner_counters = Arc::clone(&counters);
        Self::start_deferred_with_context(
            geometry,
            launch.metadata,
            launch.fallback_title,
            filesystem,
            initial_appearance,
            counters,
            move |size, output, close_handle| {
                let prepared = launch.process.prepare().inspect_err(|_| {
                    close_handle.confirm_unstarted();
                })?;
                let terminal_name = prepared.terminal_name();
                let integration_expected = prepared.integration_expected();
                let owner = NativePtyOwner::start(
                    factory.as_ref(),
                    prepared,
                    size,
                    output,
                    close_handle,
                    owner_counters,
                )?;
                Ok(StartedPty {
                    owner,
                    terminal_name,
                    restricted: launch.restricted,
                    integration_expected,
                })
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
            Arc::default(),
            move |size, output, close_handle| {
                start_native_pty(size, output, close_handle).map(|owner| StartedPty {
                    owner,
                    terminal_name: identity::TERM_FALLBACK,
                    restricted: false,
                    integration_expected: false,
                })
            },
        )
        .map(|(runtime, subscription)| (runtime, subscription.events, subscription.accessibility))
    }

    pub(super) fn start_deferred_with_context(
        geometry: TerminalGeometry,
        metadata_context: TerminalMetadataContext,
        fallback_title: String,
        local_filesystem: LocalFilesystemAuthority,
        initial_appearance: TerminalAppearanceUpdate,
        counters: Arc<TerminalCounters>,
        start_native_pty: impl FnOnce(
            NativePtySize,
            Arc<dyn NativePtyOutputSink>,
            &NativePtyCloseHandle,
        ) -> Result<StartedPty, NativePtyStartupFailure>
        + Send
        + 'static,
    ) -> Result<(TerminalRuntime, TerminalSubscription), TerminalSessionError> {
        let (command_tx, command_rx) = mpsc::channel();
        let command_rx = WorkerCommands::new(command_rx, counters);
        let (clipboard, clipboard_requests) = WorkerClipboard::connect(command_tx.clone());
        let clipboard_authority = Arc::clone(&clipboard.authority);
        let reader_transport =
            ReaderTransport::new(command_tx.clone(), Arc::clone(&clipboard_authority));
        let schedule_input = ScheduleInput::default();
        let worker_schedule_input = schedule_input.clone();
        let metadata_state = TerminalSessionMetadataState::default();
        let worker_metadata_state = metadata_state.clone();
        let (event_tx, event_rx) = async_channel::bounded(2);
        clipboard_authority.observe_subscription(event_tx.clone());
        let event_tx = TerminalEventPublisher::from(event_tx);
        let (accessibility_tx, accessibility_rx) = async_channel::bounded(1);
        let native_pty_close = NativePtyCloseHandle::default();
        let worker_native_pty_close = native_pty_close.clone();
        let native_pty_output = reader_transport.output_sink();
        let worker_events = event_tx.clone();
        let event_publisher = event_tx.clone();

        let worker = thread::Builder::new()
            .name("spaceterm-terminal".to_owned())
            .spawn(move || {
                let _completion = WorkerEventLifetime(worker_events.clone());
                let StartedPty { owner: native_pty, terminal_name, restricted, integration_expected } = match start_native_pty(
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
                        let stage = if matches!(error, NativePtyStartupFailure::Launch(crate::platform::shell_launch::ShellLaunchFailure::DirectoryUnavailable)) { TerminalSessionStartupStage::Directory } else { stage };
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
                        restricted,
                        integration_expected,
                        local_filesystem,
                        initial_appearance,
                    },
                    command_rx,
                    reader_transport,
                    worker_schedule_input,
                    TerminalWorkerPublishers {
                        clipboard,
                        metadata_state: worker_metadata_state,
                        events: event_tx,
                        accessibility: accessibility_tx,
                    },
                );
            })
            .map_err(TerminalSessionError::from)?;

        Ok((
            Self {
                metadata_state,
                commands: Some(command_tx),
                worker: Some(worker),
                native_pty_close: Some(native_pty_close),
                cleanup_confirmation: None,
                schedule_input,
                event_publisher: event_publisher.clone(),
                clipboard_authority,
            },
            TerminalSubscription {
                events: event_rx,
                accessibility: accessibility_rx,
                clipboard: clipboard_requests,
                state: event_publisher,
            },
        ))
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    #[test]
    fn replacing_a_validated_directory_before_worker_launch_refuses_the_shell() {
        let root =
            std::env::temp_dir().join(format!("spaceterm-launch-directory-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let filesystem = crate::runtime::local_filesystem();
        let directory = filesystem.validate_directory(&root).unwrap();
        let launch = TerminalSessionProcess {
            planner: test_launch_planner(),
            directory: root.clone(),
            prompt_owner: None,
            session: None,
            directory_guard: Some((directory, filesystem)),
        };
        std::fs::rename(&root, root.with_extension("old")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(matches!(
            launch.prepare(),
            Err(NativePtyStartupFailure::Launch(
                crate::platform::shell_launch::ShellLaunchFailure::DirectoryUnavailable
            ))
        ));
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(std::env::temp_dir().join(format!(
            "spaceterm-launch-directory-{}.old",
            std::process::id()
        )))
        .unwrap();
    }
}
