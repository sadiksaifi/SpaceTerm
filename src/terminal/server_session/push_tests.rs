//! The Pane worker against a real server over an in-process connection.
use super::*;
use crate::runtime::{HostCommands as _, Served, SharedWorkspaceHost};
use crate::terminal::geometry::{BackingScale, CellGridSize, LogicalCellSize, LogicalSize};
use spaceterm_server::connection::link::{LinkEvent, LinkSink};
use spaceterm_server::connection::loopback::Loopback;
use spaceterm_server::connection::outbox::Outbox;
use spaceterm_server::connection::server::AdmittedConnection;
use spaceterm_server::connection::service::{Dispatch, PaneService, Peer, SharedPaneService};
use spaceterm_server::connection::sync::SyncWorker;
use spaceterm_server::connection::{Wake, io};
use spaceterm_server::protocol::{EffectDone, Input, Operation, RequestId, View};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

/// Counts every client message the service handles.
struct Counting {
    inner: Served<SharedWorkspaceHost>,
    received: Arc<AtomicUsize>,
}

impl PaneService for Counting {
    fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        self.inner.set_wake(wake);
    }
    fn admitted(&mut self, peer: &Peer, name: pane::control::ClientName, outbox: Outbox) {
        self.inner.admitted(peer, name, outbox);
    }
    fn request(&mut self, peer: &Peer, id: RequestId, operation: Operation) -> Dispatch {
        self.received.fetch_add(1, Ordering::SeqCst);
        self.inner.request(peer, id, operation)
    }
    fn input(&mut self, peer: &Peer, input: Input) {
        self.received.fetch_add(1, Ordering::SeqCst);
        self.inner.input(peer, input);
    }
    fn view(&mut self, peer: &Peer, view: View) {
        self.received.fetch_add(1, Ordering::SeqCst);
        self.inner.view(peer, view);
    }
    fn effect_done(&mut self, peer: &Peer, done: EffectDone) {
        self.received.fetch_add(1, Ordering::SeqCst);
        self.inner.effect_done(peer, done);
    }
    fn acknowledge(&mut self, peer: &Peer, sequence: u64) {
        self.received.fetch_add(1, Ordering::SeqCst);
        self.inner.acknowledge(peer, sequence);
    }
    fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.inner.tick(now)
    }
    fn disconnected(&mut self, peer: &Peer) {
        self.inner.disconnected(peer);
    }
    fn shutdown(&mut self) -> Result<(), spaceterm_server::ServerError> {
        self.inner.shutdown()
    }
}

struct Ignore;
impl LinkSink for Ignore {
    fn deliver(&self, _: LinkEvent) {}
}

/// Runs the service's ticks when its wake rings or its deadline passes, as the host's accept
/// loop does.
fn ticking(service: SharedPaneService, running: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let bell = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    service.lock().unwrap().set_wake(Arc::new({
        let bell = Arc::clone(&bell);
        move || {
            *bell.0.lock().unwrap() = true;
            bell.1.notify_all();
        }
    }));
    std::thread::spawn(move || {
        while running.load(Ordering::Acquire) {
            let deadline = service.lock().unwrap().tick(Instant::now());
            let mut rung = bell.0.lock().unwrap();
            while !*rung {
                let wait = deadline.map_or(Duration::from_millis(100), |deadline| {
                    deadline.saturating_duration_since(Instant::now())
                });
                if wait.is_zero() {
                    break;
                }
                rung = bell.1.wait_timeout(rung, wait).unwrap().0;
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
    let service: SharedPaneService = Arc::new(Mutex::new(Box::new(Counting {
        inner: Served::new(host),
        received: Arc::clone(&received),
    })));
    let running = Arc::new(AtomicBool::new(true));
    let ticks = ticking(Arc::clone(&service), Arc::clone(&running));

    let (client, server) = Loopback::pair(1 << 20).unwrap();
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
    // The polling worker sent about 66 requests in this time.
    std::thread::sleep(Duration::from_secs(1));
    let idle = received.load(Ordering::SeqCst) - settled;

    drop(started);
    running.store(false, Ordering::Release);
    stopping.store(true, Ordering::Release);
    wake.wake();
    link.close();
    let _ = serving.join();
    let _ = ticks.join();
    service.lock().unwrap().shutdown().unwrap();
    assert!(settled > 0, "the claim itself is counted");
    assert_eq!(idle, 0, "a visible idle Pane sent {idle} messages");
}
