//! Injected port-composition test only. This is not PTY/worker acceptance evidence.
use spaceterm_core::{
    authority::*, content::*, content_queries::*, control::*, geometry::*, identity::*,
    observations::*, operations::*, organization::*, retirement::*, runtime_authority::*,
};
use spaceterm_server::contracts::*;
use spaceterm_terminal::{
    appearance::{Appearance, AppearanceGeneration},
    runtime_contracts::*,
};
use std::sync::{Arc, Mutex};

fn failure() -> ServerFailure {
    ServerFailure {
        class: ServerFailureClass::RuntimeUnavailable,
        expected: None,
        actual: None,
        limit: None,
    }
}
struct State {
    authority: RuntimeAuthority,
    first_retirement: Option<SessionCleanupObservation>,
    submissions: usize,
    completion: Option<RuntimeCompletion>,
    metadata: Option<RuntimeMetadataSnapshot>,
}
struct InjectedRuntime {
    state: Arc<Mutex<State>>,
    barrier: Arc<tokio::sync::Notify>,
}
struct Events(Option<RuntimePresentationEvent>);
impl RuntimeStream<RuntimePresentationEvent> for Events {
    fn next(
        &mut self,
    ) -> ServerFuture<'_, Option<Result<RuntimePresentationEvent, ServerFailure>>> {
        let event = self.0.take();
        Box::pin(async move { event.map(Ok) })
    }
}
impl RuntimeObservationSource for InjectedRuntime {
    fn session(&self) -> SessionKey {
        self.state.lock().unwrap().authority.session()
    }
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation> {
        None
    }
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation> {
        None
    }
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation> {
        None
    }
    fn cleanup_snapshot(&self) -> Option<SessionCleanupObservation> {
        let state = self.state.lock().unwrap();
        state
            .completion
            .map(RuntimeCompletion::cleanup)
            .or(state.first_retirement)
    }
}
impl RuntimeHandlePort for InjectedRuntime {
    fn try_submit(
        &self,
        command: FencedRuntimeCommand,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        let mut state = self.state.lock().unwrap();
        let parts = state.authority.parts();
        let disposition = if command.fence.session() == parts.session
            && Some(command.fence.grant()) == parts.grant
            && Some(command.fence.connection()) == parts.connection
            && command.fence.context() == parts.context
        {
            state.submissions += 1;
            EffectDisposition::Applied
        } else {
            EffectDisposition::StaleAuthority
        };
        Ok(Box::pin(async move { Ok(disposition) }))
    }
    fn complete_native(
        &self,
        _: spaceterm_core::native_requests::NativeCompletion,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        Err(failure())
    }
    fn read_page(
        &self,
        request: ContentPageRequest,
    ) -> Result<ServerFuture<'_, Result<ContentPageResult, ServerFailure>>, ServerFailure> {
        let page = ContentPage::new(ContentPageParts::new(request.stamp().base(), 80)).unwrap();
        let reply = ContentPageReply::new(&request, page).unwrap();
        Ok(Box::pin(async move { Ok(ContentReadResult::Ready(reply)) }))
    }
    fn read_topology(
        &self,
        _: TopologyPageRequest,
    ) -> Result<ServerFuture<'_, Result<TopologyPageResult, ServerFailure>>, ServerFailure> {
        Err(failure())
    }
    fn prepare_copy(
        &self,
        _: CopyRequest,
    ) -> Result<Box<dyn RuntimeStream<CopyStreamItem>>, ServerFailure> {
        Err(failure())
    }
    fn find(
        &self,
        _: FindRequest,
    ) -> Result<Box<dyn RuntimeStream<FindStreamItem>>, ServerFailure> {
        Err(failure())
    }
    fn read_graphics(
        &self,
        _: GraphicsRequest,
    ) -> Result<Box<dyn RuntimeStream<GraphicsChunk>>, ServerFailure> {
        Err(failure())
    }
}
impl RuntimeOwnerPort for InjectedRuntime {
    fn handle(&self) -> Arc<dyn RuntimeHandlePort> {
        Arc::new(Self {
            state: self.state.clone(),
            barrier: self.barrier.clone(),
        })
    }
    fn subscribe(
        &self,
        _: EffectFence,
    ) -> Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure> {
        let event = self
            .metadata_snapshot()
            .map(RuntimePresentationEvent::Metadata)
            .unwrap_or_else(|| RuntimePresentationEvent::Lifecycle(self.lifecycle_snapshot()));
        Ok(Box::new(Events(Some(event))))
    }
    fn lifecycle_snapshot(&self) -> RuntimeLifecycle {
        self.state
            .lock()
            .unwrap()
            .completion
            .map(RuntimeLifecycle::Finished)
            .unwrap_or(RuntimeLifecycle::Running)
    }
    fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot> {
        self.state.lock().unwrap().metadata.clone()
    }
    fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<ServerFuture<'_, Result<AuthorityApplied, ServerFailure>>, ServerFailure> {
        if self.state.lock().unwrap().authority != transition.expected() {
            return Err(failure());
        }
        Ok(Box::pin(async move {
            self.barrier.notified().await;
            self.state.lock().unwrap().authority = transition.next();
            Ok(AuthorityApplied::new(transition))
        }))
    }
    fn retire<'a>(
        &'a mut self,
        request: RetirementRequest<'a>,
    ) -> ServerFuture<'a, Result<SessionCleanupObservation, ServerFailure>> {
        let session = self.session();
        Box::pin(async move {
            if !request
                .sessions()
                .iter()
                .any(|record| record.session() == session)
            {
                return Err(failure());
            }
            let mut state = self.state.lock().unwrap();
            let observed = *state.first_retirement.get_or_insert(
                SessionCleanupObservation::new(
                    session,
                    request.reason(),
                    CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                )
                .unwrap(),
            );
            Ok(observed)
        })
    }
}
struct InjectedFactory {
    barrier: Arc<tokio::sync::Notify>,
}
impl RuntimeFactoryPort for InjectedFactory {
    fn start<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            Ok(Box::new(InjectedRuntime {
                state: Arc::new(Mutex::new(State {
                    authority: request.authority(),
                    first_retirement: None,
                    submissions: 0,
                    completion: None,
                    metadata: None,
                })),
                barrier: self.barrier.clone(),
            }) as Box<dyn RuntimeOwnerPort>)
        })
    }
}
fn start_request() -> RuntimeStartRequest {
    let server = ServerId::new([7; 16]);
    let client = ClientId::new(server, 1).unwrap();
    let session = SessionKey {
        pane: PaneId::new(server, 1).unwrap(),
        epoch: ServerEpoch::new(3),
        generation: SessionGeneration::new(1),
    };
    let grant = ControlGrant::new(session.pane, client, ControlGeneration::new(1)).unwrap();
    let connection = ConnectionBinding::new(client, ConnectionGeneration::new(1));
    let context = ContextGeneration::new(1);
    let fence = EffectFence::new(session, grant, connection, context, None).unwrap();
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server, 1).unwrap(),
        PairingId::new(server, 1).unwrap(),
        client,
    )
    .unwrap();
    let intent = StartupIntent::new(
        OperationId::new(stream, OperationSequence::new(1).unwrap()),
        fence,
        StartupSource::StartSession,
    )
    .unwrap();
    let grid = MeasuredGrid::new(
        24,
        80,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let reserved = SessionKey {
        generation: SessionGeneration::new(2),
        ..session
    };
    let reservation = MeasuredStartupReservation::new(
        intent,
        MeasuredStartupMeasurement {
            intent: intent.id(),
            fence,
            grid,
        },
        reserved,
    )
    .unwrap();
    let authority = RuntimeAuthority::new(RuntimeAuthorityParts {
        session: reserved,
        grant: Some(grant),
        connection: Some(connection),
        context,
        focused_pane: true,
        terminal_input_focus: true,
        native_effects_eligible: true,
    })
    .unwrap();
    let appearance = RuntimeAppearance {
        generation: AppearanceGeneration::INITIAL,
        foreground: Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        background: Rgb { r: 0, g: 0, b: 0 },
        palette: [Rgb { r: 0, g: 0, b: 0 }; 256],
        bold_as_bright: false,
        appearance: Appearance::Dark,
    };
    RuntimeStartRequest::new(
        reservation,
        ServerDirectory::new(server, b"/work".to_vec()).unwrap(),
        authority,
        appearance,
    )
    .unwrap()
}
fn fence_for(authority: RuntimeAuthority) -> EffectFence {
    let parts = authority.parts();
    EffectFence::new(
        parts.session,
        parts.grant.unwrap(),
        parts.connection.unwrap(),
        parts.context,
        Some(ContentRevision::new(1)),
    )
    .unwrap()
}
#[tokio::test]
async fn erased_runtime_factory_keeps_submission_render_subscription_and_takeover_acknowledgement()
{
    let barrier = Arc::new(tokio::sync::Notify::new());
    let factory: Arc<dyn RuntimeFactoryPort> = Arc::new(InjectedFactory {
        barrier: barrier.clone(),
    });
    let request = start_request();
    let mut owner = factory.start(&request).await.unwrap();
    let handle = owner.handle();
    let fence = fence_for(request.authority());
    assert_eq!(
        handle
            .try_submit(FencedRuntimeCommand {
                fence,
                command: RuntimeCommand::FocusReport(true)
            })
            .unwrap()
            .await
            .unwrap(),
        EffectDisposition::Applied
    );
    let base = ContentBase {
        session: owner.session(),
        revision: ContentRevision::new(1),
        screen: Screen::Primary,
        screen_generation: 0,
        topology_revision: 0,
    };
    let stamp = QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(1).unwrap(),
        RequestGeneration::new(0),
    )
    .unwrap();
    let read = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(1, 80, 16384).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        handle.read_page(read).unwrap().await.unwrap(),
        ContentReadResult::Ready(_)
    ));
    let mut subscription = owner.subscribe(fence).unwrap();
    assert!(matches!(
        subscription.next().await.unwrap().unwrap(),
        RuntimePresentationEvent::Lifecycle(RuntimeLifecycle::Running)
    ));
    let mut parts = request.authority().parts();
    let client = ClientId::new(parts.session.pane.server(), 2).unwrap();
    parts.grant =
        Some(ControlGrant::new(parts.session.pane, client, ControlGeneration::new(2)).unwrap());
    parts.connection = Some(ConnectionBinding::new(client, ConnectionGeneration::new(2)));
    let next = RuntimeAuthority::new(parts).unwrap();
    let transition = RuntimeAuthorityTransition::new(request.authority(), next).unwrap();
    let mut reply = owner.transition_authority(transition).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut reply)
            .await
            .is_err()
    );
    barrier.notify_one();
    assert_eq!(reply.await.unwrap().transition(), transition);
    assert_eq!(
        handle
            .try_submit(FencedRuntimeCommand {
                fence,
                command: RuntimeCommand::FocusReport(true)
            })
            .unwrap()
            .await
            .unwrap(),
        EffectDisposition::StaleAuthority
    );
    assert_eq!(
        handle
            .try_submit(FencedRuntimeCommand {
                fence: fence_for(next),
                command: RuntimeCommand::FocusReport(true)
            })
            .unwrap()
            .await
            .unwrap(),
        EffectDisposition::Applied
    );
    drop(subscription);
    assert_eq!(owner.session(), next.session());
}

#[tokio::test]
async fn native_retirement_keeps_native_identity_and_shared_first_cleanup_evidence() {
    use spaceterm_core::native_management::*;
    for (action, expected) in [
        (NativeRetirementAction::Stop, RetirementReason::Stop),
        (
            NativeRetirementAction::RestartApproved,
            RetirementReason::Stop,
        ),
        (
            NativeRetirementAction::ActivateUpdate,
            RetirementReason::ServerUpdate,
        ),
    ] {
        let factory: Arc<dyn RuntimeFactoryPort> = Arc::new(InjectedFactory {
            barrier: Arc::new(tokio::sync::Notify::new()),
        });
        let request = start_request();
        let mut owner = factory.start(&request).await.unwrap();
        let session = owner.session();
        let server = session.pane.server();
        let descriptor = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
            request: NativeManagementRequestRef::new(
                server,
                [9; 16],
                RequestDigest::from_bytes([8; 32]),
            )
            .unwrap(),
            batch: RetirementBatchId::new(server, session.epoch, 1).unwrap(),
            action,
            acceptance_revision: OrganizationRevision::new(4),
            panes: vec![
                RetirementPane::new(
                    WorkspaceId::new(server, 1).unwrap(),
                    TabId::new(server, 1).unwrap(),
                    session.pane,
                    None,
                    SessionGeneration::new(3),
                )
                .unwrap(),
            ],
            sessions: vec![
                RetiredSession::new(session, expected, RetirementCleanupState::AwaitingEvidence)
                    .unwrap(),
            ],
        })
        .unwrap();
        let first = owner
            .retire(RetirementRequest::Native(&descriptor))
            .await
            .unwrap();
        assert_eq!(first.first_reason(), expected);
        let mut parts = descriptor.parts().clone();
        parts.action = NativeRetirementAction::ActivateUpdate;
        parts.batch = RetirementBatchId::new(server, session.epoch, 2).unwrap();
        let overlapping = NativeRetirementDescriptor::new(parts).unwrap();
        assert_eq!(
            owner
                .retire(RetirementRequest::Native(&overlapping))
                .await
                .unwrap(),
            first
        );
    }
}

#[tokio::test]
async fn erased_owner_retains_classified_failure_independently_of_cleanup() {
    for cleanup in [
        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
        CleanupResult::Failed(FailureClass::RuntimeUnavailable),
    ] {
        let authority = start_request().authority();
        let terminal = SessionTerminalResult::new(
            authority.session(),
            RuntimeTerminalStatus::RuntimeFailed {
                class: FailureClass::ResourceLimit,
            },
        )
        .unwrap();
        let completion = RuntimeCompletion::new(
            terminal,
            SessionCleanupObservation::new(
                authority.session(),
                RetirementReason::RuntimeFailure,
                cleanup,
            )
            .unwrap(),
        )
        .unwrap();
        let owner: Box<dyn RuntimeOwnerPort> = Box::new(InjectedRuntime {
            state: Arc::new(Mutex::new(State {
                authority,
                first_retirement: None,
                submissions: 0,
                completion: Some(completion),
                metadata: None,
            })),
            barrier: Arc::new(tokio::sync::Notify::new()),
        });
        let mut subscription = owner.subscribe(fence_for(authority)).unwrap();
        assert!(matches!(subscription.next().await.unwrap().unwrap(),
            RuntimePresentationEvent::Lifecycle(RuntimeLifecycle::Finished(value))
                if value.terminal() == terminal && value.cleanup().result() == cleanup));
        drop(subscription);
        assert_eq!(owner.lifecycle_snapshot().terminal_result(), Some(terminal));
        assert_eq!(owner.cleanup_snapshot().unwrap().result(), cleanup);
    }
}

#[tokio::test]
async fn erased_owner_notifies_and_retains_a_title_only_metadata_update() {
    use spaceterm_terminal::metadata::*;
    let authority = start_request().authority();
    let state = Arc::new(Mutex::new(State {
        authority,
        first_retirement: None,
        submissions: 0,
        completion: None,
        metadata: None,
    }));
    let owner: Box<dyn RuntimeOwnerPort> = Box::new(InjectedRuntime {
        state: state.clone(),
        barrier: Arc::new(tokio::sync::Notify::new()),
    });
    let metadata = RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
        session: authority.session(),
        sequence: RuntimeObservationSequence::new(2).unwrap(),
        observed_at: MonotonicTick::new(25),
        title: TitleMetadata {
            value: Arc::from("OSC title only"),
            provenance: TitleProvenance::TerminalControl,
        },
        origin: RuntimeMetadataOrigin::Local {
            user: None,
            host: None,
        },
        progress: ProgressMetadata::Indeterminate,
        freshness: MetadataFreshness::Live,
        prompt_zone: PromptZone::Unknown,
        hidden_input: false,
    })
    .unwrap();
    // The provider changes only its metadata slot, not content or directory facts.
    state.lock().unwrap().metadata = Some(metadata.clone());
    let mut subscription = owner.subscribe(fence_for(authority)).unwrap();
    assert!(matches!(subscription.next().await.unwrap().unwrap(),
        RuntimePresentationEvent::Metadata(value) if value == metadata));
    drop(subscription);
    assert_eq!(owner.metadata_snapshot(), Some(metadata));
    assert!(owner.display_directory_snapshot().is_none());
    assert_eq!(owner.lifecycle_snapshot(), RuntimeLifecycle::Running);
}

#[tokio::test]
async fn clear_and_cell_metrics_do_not_retire_or_replace_the_erased_owner() {
    let factory = InjectedFactory {
        barrier: Arc::new(tokio::sync::Notify::new()),
    };
    let request = start_request();
    let owner = factory.start(&request).await.unwrap();
    let session = owner.session();
    for command in [
        RuntimeCommand::ClearScreenAndScrollback,
        RuntimeCommand::CellPixelMetrics(CellPixels::new(65535, 1).unwrap()),
    ] {
        assert_eq!(
            owner
                .handle()
                .try_submit(FencedRuntimeCommand {
                    fence: fence_for(request.authority()),
                    command,
                })
                .unwrap()
                .await
                .unwrap(),
            EffectDisposition::Applied
        );
        assert_eq!(owner.session(), session);
        assert_eq!(owner.lifecycle_snapshot(), RuntimeLifecycle::Running);
        assert!(owner.cleanup_snapshot().is_none());
    }
}
