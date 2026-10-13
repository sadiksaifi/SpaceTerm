#![allow(
    dead_code,
    reason = "shared fixtures support independently scoped projection and authority suites"
)]
use spaceterm_client::{pane_channel::*, projection::*, testing::*};
use spaceterm_core::{
    authority::*, content::*, content_queries::*, geometry::*, identity::*, operations::*,
    organization::*,
};
use std::sync::Arc;

pub struct Fixture {
    pub server: Arc<FakeServer>,
    pub peer: Arc<FakePaneConnection>,
    pub channels: Arc<PaneChannels>,
    pub lease: WorkspaceLease,
    pub pane: PaneChannel,
    pub workspace: WorkspaceId,
}
impl Fixture {
    pub async fn new(running: bool) -> Self {
        Self::with_budgets(running, ProjectionBudgets::supported().unwrap()).await
    }
    pub async fn with_budgets(running: bool, budgets: ProjectionBudgets) -> Self {
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
        let workspace = snapshot.state().workspaces[0].id;
        let channels = peer.channels(budgets).unwrap();
        let lease = channels
            .attach(
                workspace,
                spaceterm_client::view::AttachmentIncarnation::new(1).unwrap(),
            )
            .unwrap();
        let pane = lease.pane(snapshot.state().panes[0].id).unwrap();
        pane.update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: true,
            presentable: true,
        })
        .unwrap();
        if running {
            let intent = pane
                .explicit_use(ExplicitUseSource::StartSession)
                .await
                .unwrap()
                .startup()
                .unwrap();
            pane.complete_intent(&intent, grid()).await.unwrap();
            let fence = pane_fence(&pane);
            pane.publish_content_base(
                fence,
                ContentBase {
                    session: fence.session(),
                    revision: ContentRevision::new(0),
                    screen: Screen::Primary,
                    screen_generation: 1,
                    topology_revision: 1,
                },
            )
            .unwrap();
        }
        Self {
            server,
            peer,
            channels,
            lease,
            pane,
            workspace,
        }
    }
    pub fn frame(&self, revision: u64) -> QueryStamp {
        let fence = pane_fence(&self.pane);
        let base = ContentBase {
            session: fence.session(),
            revision: ContentRevision::new(revision),
            screen: Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        };
        self.pane.publish_content_base(fence, base).unwrap();
        let permit = self.pane.permit(EffectKind::Read).unwrap();
        let stamp = self
            .pane
            .query_stamp(&permit, RequestGeneration::new(1))
            .unwrap();
        self.pane
            .projection()
            .lock()
            .unwrap()
            .accept_page(stamp, page(base, 1, false))
            .unwrap();
        stamp
    }
    pub async fn takeover(&self) {
        let binding = self.server.add_client(2).unwrap();
        let id = self.server.next_operation(2).unwrap();
        self.server
            .command(
                id,
                Command::TakeOver(spaceterm_core::control::TakeOverRequest {
                    epoch: ServerEpoch::new(1),
                    pane: self.pane.pane(),
                    client: binding.target().client(),
                }),
            )
            .unwrap();
        self.peer.publish().unwrap();
    }
}
pub fn grid() -> GridSize {
    GridSize::new(
        30,
        100,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}
pub fn row(base: ContentBase, serial: u64) -> RowId {
    RowId {
        screen: base.screen,
        screen_generation: base.screen_generation,
        node_serial: serial,
        page_row: 0,
    }
}
pub fn page(base: ContentBase, serial: u64, continues: bool) -> ContentPage {
    let mut parts = ContentPageParts::new(base, 2);
    let cells = ["a", "b"]
        .iter()
        .map(|text| {
            StyledCell::new(
                CellContent::grapheme((*text).into(), CellWidth::Single).unwrap(),
                CellStyle::default(),
                None,
            )
        })
        .collect();
    let row = ContentRow::new(row(base, serial), 1, false, cells).unwrap();
    if continues {
        parts.continuation = Some(PageContinuation::new(base, &row).unwrap());
    }
    parts.rows.push(row);
    ContentPage::new(parts).unwrap()
}
pub fn selection(stamp: QueryStamp) -> ContentSelection {
    let start = CellRef {
        session: stamp.base().session,
        row: row(stamp.base(), 1),
        row_revision: 1,
        column: 0,
    };
    ContentSelection::new(stamp, start, CellRef { column: 1, ..start }, false).unwrap()
}
pub fn copy_request(stamp: QueryStamp, rich: bool) -> CopyRequest {
    CopyRequest::new(
        stamp,
        selection(stamp),
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: rich,
        },
    )
    .unwrap()
}

pub fn pane_fence(pane: &PaneChannel) -> EffectFence {
    let state = *pane.state().borrow();
    let info = state.connection.unwrap();
    let runtime = state.runtime.unwrap();
    let ControlRecord::Held { client, generation } = runtime.control else {
        panic!("held control required")
    };
    EffectFence::new(
        SessionKey {
            pane: pane.pane(),
            epoch: info.epoch,
            generation: runtime.session_generation,
        },
        ControlGrant::new(pane.pane(), client, generation).unwrap(),
        info.binding,
        state.context,
        None,
    )
    .unwrap()
}
