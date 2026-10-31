//! The Pane worker against a real server over an in-process connection.
use super::*;
use crate::runtime::{HostCommands as _, Served};
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalSize};
use spaceterm_server::connection::carrier::{
    Carrier, CarrierError, Flush, Offer, Priority, Readiness,
};
use spaceterm_server::connection::io;
use spaceterm_server::connection::link::{LinkEvent, LinkSink};
use spaceterm_server::connection::loopback::Loopback;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::server::AdmittedConnection;
use spaceterm_server::connection::service::{Peer, SharedPaneService};
use spaceterm_server::connection::sync::SyncWorker;
use spaceterm_server::protocol::RawFrame;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

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
}

impl Carrier for Counting {
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
        let offer = self.inner.offer(frame, priority)?;
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

struct Ignore;
impl LinkSink for Ignore {
    fn deliver(&self, _: LinkEvent) {}
}

type Bell = Arc<(Mutex<bool>, std::sync::Condvar)>;

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
    pub(crate) factory: DevelopmentServerSessionFactory,
    traffic: Arc<Traffic>,
    stop: Option<Box<dyn FnOnce()>>,
}

impl ConnectedPane {
    pub(crate) fn new() -> Self {
        let mut host = crate::runtime::directory_testing::terminal_host();
        host.prepare_initial_organization().unwrap();
        let Reply::Inventory { inventory, .. } = host.request([1; 32], [1; 32], Request::Inventory)
        else {
            panic!("fixture inventory unavailable");
        };
        let workspace = inventory.workspaces[0].id;
        let pane_id = inventory.workspaces[0].tabs[0].root_pane;
        let traffic = Arc::new(Traffic::default());
        let service: SharedPaneService = Arc::new(Mutex::new(Box::new(Served::new(host))));
        let running = Arc::new(AtomicBool::new(true));
        let bell = Bell::default();
        let ticks = ticking(
            Arc::clone(&service),
            Arc::clone(&running),
            Arc::clone(&bell),
        );

        let (client, server) = Loopback::pair(1 << 20).unwrap();
        let server = Counting {
            inner: server,
            traffic: Arc::clone(&traffic),
        };
        let (waker, wake) = io::Waker::new().unwrap();
        let outbox = Outbox::new(Arc::clone(&wake));
        let peer = Peer {
            client: [1; 32],
            connection: [1; 32],
            pairing: None,
        };
        service
            .lock()
            .unwrap()
            .admitted(&peer, "Test".into(), outbox.clone());
        let (sync, _sync_thread) = SyncWorker::spawn().unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let serving = {
            let (service, stopping) = (Arc::clone(&service), Arc::clone(&stopping));
            std::thread::spawn(move || {
                let connection = AdmittedConnection::new(peer, outbox, sync, None, Instant::now());
                io::serve(Box::new(server), connection, &waker, &service, &stopping);
            })
        };
        let link = io::spawn_link(Box::new(client), Arc::new(Ignore)).unwrap();
        let connection = ServerConnection::for_identity("spaceterm-development").unwrap();
        connection.adopt(link.clone());

        let attachments = spaceterm_server::attachments::Attachments::memory();
        attachments.initialize(&inventory, Some(workspace)).unwrap();
        let permit = attachments
            .permit(pane_id)
            .expect("an attached Pane has a permit");
        let controls = PaneControls::default();
        let (factory, state) = DevelopmentServerSessionFactory::for_pane(
            connection,
            &controls,
            crate::platform::runtime_local_filesystem(),
            pane_id,
        );
        state.start_created();
        let factory = factory.with_attachment(permit);
        Self {
            factory,
            traffic,
            stop: Some(Box::new(move || {
                running.store(false, Ordering::Release);
                ring(&bell);
                stopping.store(true, Ordering::Release);
                wake.wake();
                link.close();
                let _ = serving.join();
                let _ = ticks.join();
                service.lock().unwrap().shutdown().unwrap();
            })),
        }
    }
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
