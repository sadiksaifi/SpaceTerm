//! The Pane worker against a real server over an in-process connection.
use super::*;
use crate::runtime::{HostCommands as _, Served};
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalSize};
use spaceterm_server::connection::carrier::{
    Carrier, CarrierError, Flush, Offer, Priority, Readiness,
};
use spaceterm_server::connection::io;
use spaceterm_server::connection::link::LinkSink;
use spaceterm_server::connection::loopback::Loopback;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::server::AdmittedConnection;
use spaceterm_server::connection::service::{Peer, SharedPaneService};
use spaceterm_server::connection::sync::SyncWorker;
use spaceterm_server::protocol::{self, RawFrame};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

#[derive(Default)]
struct StartReplyLoss {
    armed: AtomicBool,
    starts: AtomicUsize,
    accepted: Mutex<Option<[u8; 32]>>,
    observed: Mutex<Vec<[u8; 32]>>,
}

/// Frames and bytes on one connection, in each direction.
#[derive(Default)]
struct Traffic {
    /// The client's requests, input, views, acknowledgements and pings.
    client_frames: AtomicUsize,
    client_bytes: AtomicUsize,
    /// The server's replies, frames, Pane events, inventory and pings.
    server_frames: AtomicUsize,
    server_bytes: AtomicUsize,
}

impl Traffic {
    /// Client frames, client bytes, server frames and server bytes so far.
    fn totals(&self) -> [usize; 4] {
        [
            &self.client_frames,
            &self.client_bytes,
            &self.server_frames,
            &self.server_bytes,
        ]
        .map(|count| count.load(Ordering::SeqCst))
    }
}

/// The server's end of the connection, counting everything that crosses it.
struct Counting {
    inner: Loopback,
    traffic: Arc<Traffic>,
    start_loss: Arc<StartReplyLoss>,
    requests: protocol::frame::Assembler,
    starting: Option<protocol::RequestId>,
}

impl Carrier for Counting {
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
        // Claim replies fit one frame. Lose the accepted start's reply at the carrier,
        // after the real host created its Session and before the worker can confirm it.
        let raw = RawFrame::parse(frame)?;
        let attached = if raw.kind == protocol::frame::Kind::Reply as u8 {
            match protocol::frame::Assembler::default().server(raw)? {
                Some(protocol::ServerMessage::Reply(reply)) => match reply.result {
                    Ok(protocol::ReplyBody::Claim(protocol::ClaimReply::Attached { lease })) => {
                        if self.starting == Some(reply.id)
                            && self.start_loss.armed.swap(false, Ordering::AcqRel)
                        {
                            *self.start_loss.accepted.lock().unwrap() = Some(lease.session);
                            self.inner.close();
                            return Err(CarrierError::Closed);
                        }
                        Some(lease.session)
                    }
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        let offer = self.inner.offer(frame, priority)?;
        if offer == Offer::Accepted
            && let Some(session) = attached
        {
            self.start_loss.observed.lock().unwrap().push(session);
        }
        if offer == Offer::Accepted {
            self.traffic.server_frames.fetch_add(1, Ordering::SeqCst);
            self.traffic
                .server_bytes
                .fetch_add(frame.len(), Ordering::SeqCst);
        }
        Ok(offer)
    }
    fn flush(&mut self) -> Result<Flush, CarrierError> {
        self.inner.flush()
    }
    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        let frame = self.inner.receive()?;
        if let Some(frame) = &frame {
            if let Some(protocol::ClientMessage::Request(request)) =
                self.requests.client(frame.clone())?
                && matches!(&request.operation, protocol::Operation::Claim(claim)
                    if matches!(claim.claim, pane::AttachmentClaim::Start { .. }
                        | pane::AttachmentClaim::Select { explicit: true }))
            {
                self.start_loss.starts.fetch_add(1, Ordering::SeqCst);
                self.starting = Some(request.id);
            }
            self.traffic.client_frames.fetch_add(1, Ordering::SeqCst);
            // The length prefix and the kind byte, then the body.
            self.traffic
                .client_bytes
                .fetch_add(5 + frame.body.len(), Ordering::SeqCst);
        }
        Ok(frame)
    }
    fn readiness(&self) -> Readiness {
        self.inner.readiness()
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

type Bell = Arc<(Mutex<bool>, std::sync::Condvar)>;
type Stops = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

fn ring(bell: &Bell) {
    *bell.0.lock().unwrap() = true;
    bell.1.notify_all();
}

/// Runs the service's ticks when its wake rings or its deadline passes, as the host's accept
/// loop does. It waits without a deadline when the service has no timed work.
fn ticking(
    service: SharedPaneService,
    running: Arc<AtomicBool>,
    bell: Bell,
) -> std::thread::JoinHandle<()> {
    service.lock().unwrap().ticker(Arc::new({
        let bell = Arc::clone(&bell);
        move || ring(&bell)
    }));
    std::thread::spawn(move || {
        while running.load(Ordering::Acquire) {
            let deadline = service.lock().unwrap().tick(Instant::now());
            let mut rung = bell.0.lock().unwrap();
            while !*rung && running.load(Ordering::Acquire) {
                rung = match deadline {
                    None => bell.1.wait(rung).unwrap(),
                    Some(deadline) => {
                        let wait = deadline.saturating_duration_since(Instant::now());
                        if wait.is_zero() {
                            break;
                        }
                        bell.1.wait_timeout(rung, wait).unwrap().0
                    }
                };
            }
            *rung = false;
        }
    })
}

/// A real server and loopback connection shared by Pane presentation tests.
pub(crate) struct ConnectedPane {
    pub(crate) factory: ServerSessionFactory,
    traffic: Arc<Traffic>,
    start_loss: Arc<StartReplyLoss>,
    stop: Option<Box<dyn FnOnce()>>,
    pub(crate) reconnect: Box<dyn Fn() -> spaceterm_server::connection::link::ServerLink>,
    service: SharedPaneService,
    watches: Arc<Mutex<Vec<Arc<claim::ControlWatch>>>>,
    controls: PaneControls,
    attachments: spaceterm_server::attachments::Attachments,
    stops: Stops,
    bell: Bell,
}

impl ConnectedPane {
    pub(crate) fn new() -> Self {
        Self::with_connection(ServerConnection::for_identity("spaceterm-development").unwrap())
    }

    fn with_connection(connection: ServerConnection) -> Self {
        let mut host = crate::runtime::directory_testing::terminal_host();
        host.prepare_initial_organization().unwrap();
        Self::with_host(connection, host)
    }

    pub(crate) fn with_host(
        connection: ServerConnection,
        mut host: crate::runtime::WorkspaceHost,
    ) -> Self {
        let Reply::Inventory { inventory, .. } = host.request([1; 32], [1; 32], Request::Inventory)
        else {
            panic!("fixture inventory unavailable");
        };
        let workspace = inventory.workspaces[0].id;
        let pane_id = inventory.workspaces[0].tabs[0].root_pane;
        let attachments = spaceterm_server::attachments::Attachments::memory();
        attachments.initialize(&inventory, Some(workspace)).unwrap();
        let permit = attachments.permit(pane_id).unwrap();
        let controls = PaneControls::default();
        let (factory, state) = ServerSessionFactory::for_pane(
            connection.clone(),
            &controls,
            crate::platform::runtime_local_filesystem(),
            pane_id,
        );
        state.start_created();
        let watches = Arc::new(Mutex::new(vec![claim::ControlWatch::new(
            Arc::clone(&state),
            pane_id,
        )]));
        let factory = factory.with_attachment(permit);
        let traffic = Arc::new(Traffic::default());
        let start_loss = Arc::new(StartReplyLoss::default());
        let service: SharedPaneService = Arc::new(Mutex::new(Box::new(Served::new(host))));
        let running = Arc::new(AtomicBool::new(true));
        let bell = Bell::default();
        let ticks = ticking(
            Arc::clone(&service),
            Arc::clone(&running),
            Arc::clone(&bell),
        );
        let stops = Stops::default();
        let reconnect = Box::new({
            let (service, traffic, stops) = (
                Arc::clone(&service),
                Arc::clone(&traffic),
                Arc::clone(&stops),
            );
            let start_loss = Arc::clone(&start_loss);
            let watches = Arc::clone(&watches);
            let count = AtomicUsize::new(0);
            move || {
                let (client, server) = Loopback::pair(1 << 20).unwrap();
                let server = Counting {
                    inner: server,
                    traffic: Arc::clone(&traffic),
                    start_loss: Arc::clone(&start_loss),
                    requests: protocol::frame::Assembler::default(),
                    starting: None,
                };
                let (waker, wake) = io::Waker::new().unwrap();
                let outbox = Outbox::new(Arc::clone(&wake));
                let mut id = [1; 32];
                id[..8]
                    .copy_from_slice(&(count.fetch_add(1, Ordering::SeqCst) as u64).to_le_bytes());
                let peer = Peer {
                    client: [1; 32],
                    connection: id,
                    pairing: None,
                };
                service
                    .lock()
                    .unwrap()
                    .admitted(&peer, "Test".into(), outbox.clone());
                let (sync, _sync_thread) = SyncWorker::spawn().unwrap();
                let stopping = Arc::new(AtomicBool::new(false));
                // Adopt before serving, so an inventory wake can only find this admitted link.
                let events = Arc::clone(&watches);
                let link = io::spawn_link(
                    Box::new(client),
                    Arc::new(move |event| {
                        for watch in events.lock().unwrap().iter() {
                            watch.deliver(match &event {
                                spaceterm_server::connection::link::LinkEvent::Inventory(
                                    revision,
                                ) => spaceterm_server::connection::link::LinkEvent::Inventory(
                                    Arc::clone(revision),
                                ),
                                spaceterm_server::connection::link::LinkEvent::Health(health) => {
                                    spaceterm_server::connection::link::LinkEvent::Health(*health)
                                }
                            });
                        }
                    }),
                )
                .unwrap();
                connection.adopt(link.clone());
                let serving = {
                    let (service, stopping) = (Arc::clone(&service), Arc::clone(&stopping));
                    std::thread::spawn(move || {
                        let admitted =
                            AdmittedConnection::new(peer, outbox, sync, None, Instant::now());
                        io::serve(Box::new(server), admitted, &waker, &service, &stopping);
                    })
                };
                let closed = link.clone();
                stops.lock().unwrap().push(Box::new(move || {
                    stopping.store(true, Ordering::Release);
                    wake.wake();
                    serving.join().unwrap();
                    closed.close();
                }));
                link
            }
        });
        reconnect();
        Self {
            factory,
            traffic,
            start_loss,
            reconnect,
            service: Arc::clone(&service),
            watches,
            controls,
            attachments,
            stops: Arc::clone(&stops),
            bell: Arc::clone(&bell),
            stop: Some(Box::new(move || {
                running.store(false, Ordering::Release);
                ring(&bell);
                for stop in std::mem::take(&mut *stops.lock().unwrap()) {
                    stop();
                }
                ticks.join().unwrap();
                service.lock().unwrap().shutdown().unwrap();
            })),
        }
    }

    pub(crate) fn connection(&self) -> ServerConnection {
        self.factory.server.clone()
    }

    pub(crate) fn pane(&self, pane: [u8; 32]) -> (ServerSessionFactory, Arc<ServerPaneState>) {
        let (factory, state) = ServerSessionFactory::for_pane(
            self.connection(),
            &self.controls,
            crate::platform::runtime_local_filesystem(),
            pane,
        );
        self.watches
            .lock()
            .unwrap()
            .push(claim::ControlWatch::new(Arc::clone(&state), pane));
        (
            factory.with_attachment(self.attachments.permit(pane).unwrap()),
            state,
        )
    }

    pub(crate) fn stop_server(&self) {
        for stop in std::mem::take(&mut *self.stops.lock().unwrap()) {
            stop();
        }
        self.service.lock().unwrap().shutdown().unwrap();
    }

    pub(crate) fn restore_server(&self, host: crate::runtime::WorkspaceHost) {
        let mut service = self.service.lock().unwrap();
        *service = Box::new(Served::new(host));
        let bell = Arc::clone(&self.bell);
        service.ticker(Arc::new(move || ring(&bell)));
        ring(&self.bell);
    }

    pub(crate) fn starts(&self) -> usize {
        self.start_loss.starts.load(Ordering::SeqCst)
    }

    pub(crate) fn sessions(&self) -> Vec<[u8; 32]> {
        self.service.lock().unwrap().sessions()
    }
}

pub(crate) fn paired_connection(
    connector: Arc<dyn spaceterm_server::direct::DirectConnector>,
) -> ServerConnection {
    use spaceterm_server::direct::{PairedDevice, ServerPin};
    ServerConnection::direct(
        "spaceterm-development",
        [4; 32],
        PairedDevice {
            pin: ServerPin {
                build: "spaceterm-development".into(),
                server: [9; 32],
                public_key: [2; 32],
                certificate: [3; 32],
            },
            pairing: [5; 32],
            client: [1; 32],
        },
        connector,
    )
    .unwrap()
}

pub(crate) fn await_status(state: &ServerPaneState, expected: ServerPaneStatus) {
    let until = Instant::now() + Duration::from_secs(10);
    while state.status() != expected {
        assert!(
            Instant::now() < until,
            "expected {expected:?}, got {:?}",
            state.status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_live_paired_pane_recovers_after_transport_loss() {
    use spaceterm_server::connection::link::LinkError;
    use spaceterm_server::direct::{DirectConnection, DirectConnector, DirectError};
    struct Away;
    impl DirectConnector for Away {
        fn connect(&self) -> Result<DirectConnection, DirectError> {
            Err(DirectError::Transport)
        }
    }
    for lost in [
        LinkError::Closed,
        LinkError::Unresponsive,
        LinkError::Stalled,
    ] {
        let server = ConnectedPane::with_connection(paired_connection(Arc::new(Away)));
        let state = Arc::clone(&server.factory.state);
        let started = server
            .factory
            .start(
                *super::tests::Adapter::new().handle.geometry.lock().unwrap(),
                TerminalAppearanceUpdate::builtin().unwrap(),
            )
            .unwrap();
        await_status(&state, ServerPaneStatus::Connected);
        let generation = state.connection_generation();
        let link = server.factory.server.link().unwrap();
        link.fail(lost);
        let expected = ServerPaneStatus::Failed(ServerError::Direct(DirectError::Transport));
        let until = Instant::now() + Duration::from_secs(8);
        while !matches!(state.status(), ServerPaneStatus::Failed(_)) {
            assert!(
                Instant::now() < until,
                "the outage never exhausted recovery"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(state.status(), expected, "live loss {lost:?}");
        (server.reconnect)();
        await_status(&state, ServerPaneStatus::Connected);
        assert!(state.connection_generation() > generation);
        drop(started);
    }
}

/// Exercise classification before the worker records a submitted start's outcome. Losing the
/// real host's accepted reply must not become a protocol-recovery episode or a second start.
#[test]
fn a_paired_worker_recovers_the_original_session_after_losing_its_start_reply() {
    use spaceterm_server::direct::{DirectConnection, DirectConnector, DirectError};
    struct Away;
    impl DirectConnector for Away {
        fn connect(&self) -> Result<DirectConnection, DirectError> {
            Err(DirectError::Transport)
        }
    }
    let server = ConnectedPane::with_connection(paired_connection(Arc::new(Away)));
    server.start_loss.armed.store(true, Ordering::Release);
    let state = Arc::clone(&server.factory.state);
    let started = server
        .factory
        .start(
            *super::tests::Adapter::new().handle.geometry.lock().unwrap(),
            TerminalAppearanceUpdate::builtin().unwrap(),
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !matches!(state.status(), ServerPaneStatus::Failed(_)) {
        assert!(
            Instant::now() < until,
            "the lost start did not exhaust recovery"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let original = server
        .start_loss
        .accepted
        .lock()
        .unwrap()
        .expect("the host accepted a start before its reply was lost");
    assert_eq!(server.start_loss.starts.load(Ordering::SeqCst), 1);
    assert!(
        state.start_unconfirmed(),
        "the lost reply must leave the original start unconfirmed"
    );
    assert_eq!(
        state.status(),
        ServerPaneStatus::Failed(ServerError::Direct(DirectError::Transport)),
        "transport classification must happen before claim bookkeeping"
    );
    assert!(
        server.start_loss.observed.lock().unwrap().is_empty(),
        "no successful claim reply reached the old link"
    );

    (server.reconnect)();
    await_status(&state, ServerPaneStatus::Connected);
    assert!(!state.start_unconfirmed());
    assert_eq!(
        server.start_loss.starts.load(Ordering::SeqCst),
        1,
        "recovery must not submit a second shell start"
    );
    let observed = server.start_loss.observed.lock().unwrap();
    assert!(
        !observed.is_empty(),
        "recovery must observe a running Session"
    );
    assert!(
        observed.iter().all(|session| *session == original),
        "recovery must find the original Session"
    );
    drop(started);
}

impl Drop for ConnectedPane {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// A visible idle Pane sends no periodic client requests after its shell appears.
#[test]
fn a_visible_idle_pane_sends_no_periodic_requests() {
    let server = ConnectedPane::new();
    let traffic = &server.traffic;
    let started = server
        .factory
        .start(
            TerminalGeometry::from_viewport(
                LogicalSize::new(640.0, 432.0),
                LogicalCellSize::new(8.0, 18.0),
                BackingScale::ONE,
                CellGridSize::new(2, 2),
            ),
            TerminalAppearanceUpdate::builtin().unwrap(),
        )
        .unwrap();
    started.handle.set_presentable(true);

    let deadline = Instant::now() + Duration::from_secs(10);
    let shell_shown = || {
        while let Ok(event) = started.events.try_recv() {
            if let TerminalSessionEvent::Screen(screen) = event
                && screen
                    .rows
                    .iter()
                    .any(|row| row.iter().any(|cell| !cell.text.trim().is_empty()))
            {
                return true;
            }
        }
        false
    };
    while !shell_shown() {
        assert!(
            Instant::now() < deadline,
            "the shell's prompt never arrived"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The connection settles once it stays quiet both ways, so the claim's follow-up messages,
    // such as the view and focus, and any late prompt redraw are behind it.
    let settle_by = Instant::now() + Duration::from_secs(10);
    let mut last = traffic.totals();
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(300) {
        assert!(
            Instant::now() < settle_by,
            "the connection never went quiet"
        );
        std::thread::sleep(Duration::from_millis(10));
        let now = traffic.totals();
        if now != last {
            last = now;
            quiet_since = Instant::now();
        }
    }
    let settled = traffic.totals();
    // The polling worker sent about 66 requests in this time.
    std::thread::sleep(Duration::from_secs(1));
    let idle = traffic.totals();

    drop(started);
    let [frames, bytes, ..] = settled;
    assert!(frames > 0 && bytes > 0, "the claim itself is counted");
    assert_eq!(
        idle[..2],
        settled[..2],
        "a visible idle Pane sent frames or bytes"
    );
}
