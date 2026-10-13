use super::*;
use spaceterm_client::{pane_channel::*, projection::ProjectionBudgets, testing::*};
use spaceterm_core::{authority::*, content::*, operations::*, organization::*};
use std::sync::Arc;

pub(crate) struct Fixture {
    pub runtime: Option<tokio::runtime::Runtime>,
    pub server: Arc<FakeServer>,
    pub peer: Arc<FakePaneConnection>,
    pub channels: Arc<PaneChannels>,
    pub lease: WorkspaceLease,
    pub channel: PaneChannel,
    pub token: PaneViewToken,
}
impl Fixture {
    pub(crate) async fn new(running: bool) -> Self {
        let server = FakeServer::new(1, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
        peer.submit(
            Command::CreateWorkspace {
                server: server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: server.server(),
                },
                name: NameDraft::Automatic,
            },
            None,
        )
        .await
        .unwrap();
        let snapshot = server.snapshot().unwrap();
        let incarnation = spaceterm_client::view::AttachmentIncarnation::new(1).unwrap();
        let channels = peer
            .channels(ProjectionBudgets::supported().unwrap())
            .unwrap();
        let lease = channels
            .attach(snapshot.state().workspaces[0].id, incarnation)
            .unwrap();
        let channel = lease.pane(snapshot.state().panes[0].id).unwrap();
        let context = channel
            .update_context(PaneContextFacts {
                focused_pane: true,
                terminal_input_focus: true,
                presentable: true,
            })
            .unwrap();
        if running {
            let intent = channel
                .explicit_use(ExplicitUseSource::StartSession)
                .await
                .unwrap()
                .startup()
                .unwrap();
            channel.complete_intent(&intent, grid()).await.unwrap();
            let state = *channel.state().borrow();
            let runtime = state.runtime.unwrap();
            let ControlRecord::Held { client, generation } = runtime.control else {
                panic!("missing grant")
            };
            let grant = ControlGrant::new(channel.pane(), client, generation).unwrap();
            let info = state.connection.unwrap();
            let fence = EffectFence::new(
                SessionKey {
                    pane: channel.pane(),
                    epoch: info.epoch,
                    generation: runtime.session_generation,
                },
                grant,
                info.binding,
                state.context,
                None,
            )
            .unwrap();
            channel
                .publish_content_base(
                    fence,
                    ContentBase {
                        session: fence.session(),
                        revision: ContentRevision::new(1),
                        screen: Screen::Primary,
                        screen_generation: 1,
                        topology_revision: 1,
                    },
                )
                .unwrap();
        }
        Self {
            runtime: None,
            server,
            peer,
            channels,
            lease,
            token: PaneViewToken {
                pane: channel.pane(),
                attachment_generation: incarnation,
                context_generation: context,
            },
            channel,
        }
    }
}
fn grid() -> MeasuredGrid {
    MeasuredGrid::new(
        3,
        5,
        CellPixels::new(10, 20).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}

#[tokio::test]
async fn desktop_server_bind_accepts_current_channel_without_starting_a_session() {
    let fixture = Fixture::new(false).await;
    assert!(ServerPaneSession::bind(fixture.channel.clone(), fixture.token).is_ok());
    assert!(matches!(
        fixture.server.snapshot().unwrap().runtime()[0].phase,
        SessionPhase::NotStarted
    ));
}

impl Fixture {
    pub(crate) fn publish_base(&self, revision: u64) {
        let state = *self.channel.state().borrow();
        let info = state.connection.unwrap();
        let runtime = state.runtime.unwrap();
        let ControlRecord::Held { client, generation } = runtime.control else {
            panic!("missing grant");
        };
        let fence = EffectFence::new(
            SessionKey {
                pane: self.channel.pane(),
                epoch: info.epoch,
                generation: runtime.session_generation,
            },
            ControlGrant::new(self.channel.pane(), client, generation).unwrap(),
            info.binding,
            state.context,
            None,
        )
        .unwrap();
        self.channel
            .publish_content_base(
                fence,
                ContentBase {
                    session: fence.session(),
                    revision: ContentRevision::new(revision),
                    screen: Screen::Primary,
                    screen_generation: 1,
                    topology_revision: 1,
                },
            )
            .unwrap();
    }
}

impl Fixture {
    pub(crate) fn frame(&self) -> spaceterm_core::content_queries::QueryStamp {
        let permit = self.channel.permit(EffectKind::Read).unwrap();
        let stamp = self
            .channel
            .query_stamp(
                &permit,
                spaceterm_core::content_queries::RequestGeneration::new(1),
            )
            .unwrap();
        let base = stamp.base();
        let row = RowId {
            screen: base.screen,
            screen_generation: base.screen_generation,
            node_serial: 1,
            page_row: 0,
        };
        let cells = ["p", "r", "i", "v", "a"]
            .iter()
            .map(|text| {
                StyledCell::new(
                    CellContent::grapheme((*text).to_owned(), CellWidth::Single).unwrap(),
                    CellStyle {
                        bold: true,
                        semantic: SemanticContent::Prompt,
                        ..CellStyle::default()
                    },
                    None,
                )
            })
            .collect();
        let mut parts = ContentPageParts::new(base, 5);
        parts
            .rows
            .push(ContentRow::new(row, 1, false, cells).unwrap());
        self.channel
            .projection()
            .lock()
            .unwrap()
            .accept_page(stamp, ContentPage::new(parts).unwrap())
            .unwrap();
        stamp
    }
}

/// Echoes the admitted query stamp, like the real content provider. Only the remote boundary is
/// replaced; PaneChannel transfer admission, the projection and desktop completion checks stay real.
pub(crate) struct EchoContentConnection {
    inner: Arc<FakePaneConnection>,
    pub page_reads: std::sync::atomic::AtomicUsize,
    pub copy_gate: std::sync::Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}
impl Fixture {
    pub(crate) fn echo_content(mut self) -> (Self, Arc<EchoContentConnection>) {
        let peer = Arc::new(EchoContentConnection {
            inner: self.peer.clone(),
            page_reads: std::sync::atomic::AtomicUsize::new(0),
            copy_gate: std::sync::Mutex::new(None),
        });
        let info = self.channel.state().borrow().connection.unwrap();
        let channels = Arc::new(
            PaneChannels::new(peer.clone(), ProjectionBudgets::supported().unwrap()).unwrap(),
        );
        channels
            .publish(info, self.server.replica().unwrap())
            .unwrap();
        let lease = channels
            .attach(self.lease.workspace(), self.lease.incarnation())
            .unwrap();
        let channel = lease.pane(self.channel.pane()).unwrap();
        let context = channel
            .update_context(PaneContextFacts {
                focused_pane: true,
                terminal_input_focus: true,
                presentable: true,
            })
            .unwrap();
        self.channel = channel;
        self.channels = channels;
        self.lease = lease;
        self.token.context_generation = context;
        if matches!(
            self.channel.state().borrow().runtime.unwrap().phase,
            SessionPhase::Running { .. }
        ) {
            self.publish_base(1);
        }
        (self, peer)
    }
}
impl PaneConnectionPort for EchoContentConnection {
    fn grid_limits(
        &self,
        info: spaceterm_client::api::ConnectedInfo,
    ) -> Result<spaceterm_core::geometry::GridLimits, spaceterm_client::failure::ClientFailure>
    {
        self.inner.grid_limits(info)
    }
    fn explicit_use(
        &self,
        request: PaneUseRequest,
    ) -> spaceterm_client::ports::ClientFuture<'_, PaneUseReply> {
        self.inner.explicit_use(request)
    }
    fn complete_intent(
        &self,
        workspace: spaceterm_core::identity::WorkspaceId,
        measurement: spaceterm_core::control::MeasuredStartupMeasurement,
    ) -> spaceterm_client::ports::ClientFuture<'_, spaceterm_client::operations::OperationTicket>
    {
        self.inner.complete_intent(workspace, measurement)
    }
    fn cancel_intent(&self, intent: spaceterm_core::control::StartupIntent) {
        self.inner.cancel_intent(intent);
    }
    fn take_over(
        &self,
        workspace: spaceterm_core::identity::WorkspaceId,
        request: spaceterm_core::control::TakeOverRequest,
    ) -> spaceterm_client::ports::ClientFuture<'_, spaceterm_client::operations::OperationTicket>
    {
        self.inner.take_over(workspace, request)
    }
    fn try_effect(
        &self,
        permit: EffectPermit,
        command: spaceterm_terminal::runtime_contracts::FencedRuntimeCommand,
    ) -> Result<EffectAdmission, spaceterm_client::failure::ClientFailure> {
        self.inner.try_effect(permit, command)
    }
    fn read(
        &self,
        permit: EffectPermit,
        request: PaneReadRequest,
    ) -> spaceterm_client::ports::ClientFuture<'_, Box<dyn PaneReadSource>> {
        use sha2::{Digest, Sha256};
        let (items, gate) = match request {
            PaneReadRequest::Page(request) => {
                self.page_reads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let base = request.stamp().base();
                let row = RowId {
                    screen: base.screen,
                    screen_generation: base.screen_generation,
                    node_serial: 1,
                    page_row: 0,
                };
                let cell = StyledCell::new(
                    CellContent::grapheme("R".to_owned(), CellWidth::Single).unwrap(),
                    CellStyle::default(),
                    None,
                );
                let mut parts = ContentPageParts::new(base, 5);
                parts
                    .rows
                    .push(ContentRow::new(row, 1, false, vec![cell; 5]).unwrap());
                (
                    vec![PaneReadItem::Page(ContentReadResult::Ready(
                        ContentPageReply::new(&request, ContentPage::new(parts).unwrap()).unwrap(),
                    ))],
                    None,
                )
            }
            PaneReadRequest::Copy(request) => {
                let transfer = ResourceTransferId::new(1).unwrap();
                (
                    vec![
                        PaneReadItem::Copy(CopyStreamItem::Chunk(
                            CopyChunk::new(
                                &request,
                                transfer,
                                CopyPart::Plain,
                                0,
                                5,
                                b"priva".to_vec(),
                            )
                            .unwrap(),
                        )),
                        PaneReadItem::Copy(CopyStreamItem::Complete(
                            CopyCompletion::new(
                                &request,
                                transfer,
                                5,
                                Sha256::digest(b"priva").into(),
                                Some((0, Sha256::digest(b"").into())),
                            )
                            .unwrap(),
                        )),
                    ],
                    self.copy_gate.lock().unwrap().clone(),
                )
            }
            _ => return self.inner.read(permit, request),
        };
        Box::pin(async move {
            Ok(Box::new(EchoContentRead {
                items: items.into(),
                gate,
            }) as Box<dyn PaneReadSource>)
        })
    }
}
struct EchoContentRead {
    items: std::collections::VecDeque<PaneReadItem>,
    gate: Option<Arc<tokio::sync::Semaphore>>,
}
impl PaneReadSource for EchoContentRead {
    fn next(&mut self) -> spaceterm_client::ports::ClientFuture<'_, Option<PaneReadItem>> {
        Box::pin(async move {
            if let Some(gate) = self.gate.take() {
                gate.acquire_owned().await.unwrap().forget();
            }
            Ok(self.items.pop_front())
        })
    }
}
