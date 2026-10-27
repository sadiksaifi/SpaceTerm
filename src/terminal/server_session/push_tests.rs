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

/// Counts every frame the client writes: requests, input, views, acknowledgements and pings.
struct Counting {
    inner: Loopback,
    received: Arc<AtomicUsize>,
}

impl Carrier for Counting {
    fn offer(&mut self, frame: &[u8], priority: Priority) -> Result<Offer, CarrierError> {
        self.inner.offer(frame, priority)
    }
    fn flush(&mut self) -> Result<Flush, CarrierError> {
        self.inner.flush()
    }
    fn receive(&mut self) -> Result<Option<RawFrame>, CarrierError> {
        let frame = self.inner.receive()?;
        if frame.is_some() {
            self.received.fetch_add(1, Ordering::SeqCst);
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

/// Once a visible Pane shows its shell, an idle second sends nothing on the connection: no
/// polls, no effect checks, no view repeats. Pushed frames and pings are the server's.
#[test]
fn a_visible_idle_pane_sends_no_periodic_requests() {
    let mut host = crate::runtime::directory_testing::terminal_host();
    host.prepare_initial_organization().unwrap();
    let Reply::Inventory { inventory, .. } = host.request([1; 32], [1; 32], Request::Inventory)
    else {
        panic!("fixture inventory unavailable");
    };
    let workspace = inventory.workspaces[0].id;
    let pane_id = inventory.workspaces[0].tabs[0].root_pane;
    let received = Arc::new(AtomicUsize::new(0));
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
        received: Arc::clone(&received),
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
    let started = factory
        .with_attachment(permit)
        .start(
            TerminalGeometry::from_viewport(
                LogicalSize::new(640.0, 432.0),
                LogicalCellSize::new(8.0, 18.0),
                BackingScale::ONE,
                CellGridSize::new(2, 2),
            ),
            LocalTerminalLaunchPlan::new(
                crate::platform::runtime_local_filesystem()
                    .validate_directory(&std::env::temp_dir())
                    .unwrap(),
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
    // Let the claim's follow-up messages, such as the view and focus, settle.
    std::thread::sleep(Duration::from_millis(500));
    let settled = received.load(Ordering::SeqCst);
    let turns = state.wake.waits();
    // The polling worker sent about 66 requests in this time.
    std::thread::sleep(Duration::from_secs(1));
    let idle = received.load(Ordering::SeqCst) - settled;
    let spins = state.wake.waits() - turns;

    drop(started);
    running.store(false, Ordering::Release);
    ring(&bell);
    stopping.store(true, Ordering::Release);
    wake.wake();
    link.close();
    let _ = serving.join();
    let _ = ticks.join();
    service.lock().unwrap().shutdown().unwrap();
    assert!(settled > 0, "the claim itself is counted");
    assert_eq!(idle, 0, "a visible idle Pane sent {idle} frames");
    assert_eq!(spins, 0, "an idle Pane worker woke {spins} times");
}
