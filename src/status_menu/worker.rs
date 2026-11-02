//! Runs every service read and operation away from the UI thread, one at a time.
use super::model::Command;
use super::service_changes::{self, StartWatch};
#[cfg(target_os = "macos")]
use spaceterm_server::service::control::{Action, Confirmation};
use spaceterm_server::{
    ServerError,
    connection::link::{Health, LinkEvent, LinkSink},
    service::{ServiceStatus, control::Controller},
};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU8, Ordering},
    mpsc,
};
use std::time::Duration;

pub(crate) enum Request {
    /// Reads the service, and whether a verified server build is offered to update to.
    Refresh,
    /// Reads the service only, after the local server's link changed. Verifying the included
    /// build is left to reads the person causes, such as opening the menu.
    Poll,
    Start,
    /// On Linux the server control window prepares and confirms instead.
    #[cfg(target_os = "macos")]
    Prepare(Action),
    /// Performs the confirmed operation the menu's command prepared.
    #[cfg(target_os = "macos")]
    Confirm(Command, Confirmation),
}

pub(crate) enum Event {
    Status(Result<ServiceStatus, ServerError>),
    /// Whether the Server version row offers Update Server.
    Update(bool),
    /// A confirmation to present, or why the operation is no longer available.
    #[cfg(target_os = "macos")]
    Prepared(Action, Result<Confirmation, ServerError>),
    Finished(Command, Result<ServiceStatus, ServerError>),
}

/// The service operations the worker runs. [`Controller`] is the local service.
pub(crate) trait StatusService: Send + 'static {
    fn status(&self) -> Result<ServiceStatus, ServerError>;
    fn start(&self) -> Result<ServiceStatus, ServerError>;
    #[cfg(target_os = "macos")]
    fn prepare(&self, action: Action) -> Result<Confirmation, ServerError>;
    #[cfg(target_os = "macos")]
    fn confirm(&self, confirmation: Confirmation) -> Result<ServiceStatus, ServerError>;
}

impl StatusService for Controller {
    fn status(&self) -> Result<ServiceStatus, ServerError> {
        Controller::status(self)
    }
    fn start(&self) -> Result<ServiceStatus, ServerError> {
        Controller::start(self)
    }
    #[cfg(target_os = "macos")]
    fn prepare(&self, action: Action) -> Result<Confirmation, ServerError> {
        Controller::prepare(self, action)
    }
    #[cfg(target_os = "macos")]
    fn confirm(&self, confirmation: Confirmation) -> Result<ServiceStatus, ServerError> {
        Controller::confirm(self, confirmation)
    }
}

/// What one wait for a request ended with.
pub(crate) enum Next {
    Request(Request),
    /// The wait's limit passed with no request.
    Idle,
    /// Every sender is gone, so the worker ends.
    Closed,
}

/// Where the worker waits for its next request.
pub(crate) trait Requests {
    /// Waits for the next request, for at most `limit` when one is given.
    fn next(&mut self, limit: Option<Duration>) -> Next;
}

impl Requests for mpsc::Receiver<Request> {
    fn next(&mut self, limit: Option<Duration>) -> Next {
        match limit {
            None => self.recv().map_or(Next::Closed, Next::Request),
            Some(limit) => match self.recv_timeout(limit) {
                Ok(request) => Next::Request(request),
                Err(mpsc::RecvTimeoutError::Timeout) => Next::Idle,
                Err(mpsc::RecvTimeoutError::Disconnected) => Next::Closed,
            },
        }
    }
}

/// The worker reads the service only when asked: when the menu opens, after an operation, when
/// the service's directories or its server's process change, or when [`Worker::changes`] reports
/// that the local server's link changed. It never reads on a timer, so an idle client does no
/// status work.
pub(crate) struct Worker {
    requests: mpsc::Sender<Request>,
    changes: Arc<ServerChanges>,
}

impl Worker {
    /// `deliver` runs on the worker thread and hands each event to the UI thread.
    /// `update_offered` reads whether a verified build is offered, without staging anything.
    /// `watch` is the platform's watch over the service's directories and server process.
    pub(crate) fn spawn(
        controller: Controller,
        watch: StartWatch,
        update_offered: impl Fn() -> bool + Send + 'static,
        deliver: impl Fn(Event) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, mut received) = mpsc::channel();
        {
            let directories = controller
                .state_directories()
                .map(std::path::Path::to_path_buf)
                .to_vec();
            let controller = controller.clone();
            let requests = requests.clone();
            service_changes::follow(
                watch,
                directories,
                move || controller.server_process().ok().flatten(),
                move || requests.send(Request::Poll).is_ok(),
            )?;
        }
        std::thread::Builder::new()
            .name("server-status".into())
            .spawn(move || serve(&mut received, &controller, &update_offered, &deliver))?;
        let changes = Arc::new(ServerChanges {
            link: AtomicU8::new(UNOBSERVED),
            requests: requests.clone(),
        });
        Ok(Self { requests, changes })
    }

    pub(crate) fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    /// The sink to subscribe to the local server's shared link. It lives as long as the worker.
    pub(crate) fn changes(&self) -> Weak<dyn LinkSink> {
        Arc::downgrade(&self.changes) as Weak<dyn LinkSink>
    }
}

/// Runs each request in order until every sender is gone. Waiting has no limit, so only a
/// request wakes the worker.
fn serve(
    requests: &mut impl Requests,
    service: &impl StatusService,
    update_offered: &impl Fn() -> bool,
    deliver: &impl Fn(Event),
) {
    loop {
        let request = match requests.next(None) {
            Next::Request(request) => request,
            Next::Idle => continue,
            Next::Closed => return,
        };
        match request {
            Request::Refresh => {
                deliver(Event::Status(service.status()));
                deliver(Event::Update(update_offered()));
            }
            Request::Poll => deliver(Event::Status(service.status())),
            Request::Start => finish(service, deliver, Command::Start, service.start()),
            #[cfg(target_os = "macos")]
            Request::Prepare(action) => deliver(Event::Prepared(action, service.prepare(action))),
            #[cfg(target_os = "macos")]
            Request::Confirm(command, confirmation) => {
                let result = service.confirm(confirmation);
                finish(service, deliver, command, result)
            }
        }
    }
}

/// No link event has reached the sink yet. It may have subscribed after the link's first
/// inventory, so the first close still asks for a read.
const UNOBSERVED: u8 = 0;
/// The last link event was an inventory, so a server admitted the client.
const ADMITTED: u8 = 1;
/// The last link event closed the link.
const CLOSED: u8 = 2;

/// Asks for a status read when the local server's link ends, and when a later link's first
/// inventory shows a server admitted the client again. A stopped, crashed, restarted or updated
/// server changes the link. Repeated closes while no server admits the client ask once.
struct ServerChanges {
    /// [`UNOBSERVED`], [`ADMITTED`] or [`CLOSED`].
    link: AtomicU8,
    requests: mpsc::Sender<Request>,
}

impl LinkSink for ServerChanges {
    fn deliver(&self, event: LinkEvent) {
        let changed = match event {
            LinkEvent::Health(Health::Closed(_)) => {
                self.link.swap(CLOSED, Ordering::AcqRel) != CLOSED
            }
            LinkEvent::Inventory(_) => self.link.swap(ADMITTED, Ordering::AcqRel) != ADMITTED,
            LinkEvent::Health(Health::Live) => false,
        };
        if changed {
            let _ = self.requests.send(Request::Poll);
        }
    }
}

/// A refused operation leaves the service as it was, so the item reads it again.
fn finish(
    service: &impl StatusService,
    deliver: &impl Fn(Event),
    command: Command,
    result: Result<ServiceStatus, ServerError>,
) {
    let refused = result.is_err();
    deliver(Event::Finished(command, result));
    if refused {
        deliver(Event::Status(service.status()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::{
        connection::link::LinkError, hierarchy::Inventory, protocol::InventoryRevision,
    };

    fn inventory() -> LinkEvent {
        LinkEvent::Inventory(Arc::new(InventoryRevision {
            inventory: Inventory {
                revision: 1,
                workspaces: Vec::new(),
                close: None,
                repositories: None,
            },
            next_sequence: 1,
        }))
    }

    fn polls(received: &mpsc::Receiver<Request>) -> usize {
        received
            .try_iter()
            .filter(|request| matches!(request, Request::Poll))
            .count()
    }

    /// The status item reads the service once per link change and never because time passed,
    /// so pushed inventories on a live link and a quiet link cause no service reads.
    #[test]
    fn the_status_item_reads_the_service_only_when_the_local_link_changes() {
        let (requests, received) = mpsc::channel();
        let changes = ServerChanges {
            link: AtomicU8::new(UNOBSERVED),
            requests,
        };

        changes.deliver(inventory());
        assert_eq!(polls(&received), 1, "the first admitted link is a change");
        changes.deliver(inventory());
        changes.deliver(LinkEvent::Health(Health::Live));
        assert_eq!(polls(&received), 0, "a live link's pushes are no change");

        changes.deliver(LinkEvent::Health(Health::Closed(LinkError::Closed)));
        assert_eq!(polls(&received), 1, "an ended link is a change");
        changes.deliver(LinkEvent::Health(Health::Closed(LinkError::Closed)));
        assert_eq!(
            polls(&received),
            0,
            "a link that never admitted again is no change"
        );

        changes.deliver(inventory());
        assert_eq!(polls(&received), 1, "a later admitted link is a change");
    }

    /// A sink subscribed after the link's first inventory still asks for a read when that link
    /// closes, so a quiet server that stops is not missed. Repeated closes ask once.
    #[test]
    fn the_first_close_asks_for_a_read_when_the_first_inventory_preceded_the_subscription() {
        let (requests, received) = mpsc::channel();
        let changes = ServerChanges {
            link: AtomicU8::new(UNOBSERVED),
            requests,
        };
        changes.deliver(LinkEvent::Health(Health::Closed(LinkError::Closed)));
        assert_eq!(polls(&received), 1, "the first close is a change");
        changes.deliver(LinkEvent::Health(Health::Closed(LinkError::Closed)));
        assert_eq!(polls(&received), 0, "a repeated close is no change");
    }

    /// One step of a scripted request queue.
    enum Step {
        /// Time passes with no request.
        Idle(Duration),
        Send(Request),
    }

    /// A request queue on a virtual clock. A wait with a limit shorter than the coming idle time
    /// ends with [`Next::Idle`] at that limit, as a real queue would.
    struct Script {
        steps: std::collections::VecDeque<Step>,
        clock: Arc<std::sync::Mutex<Duration>>,
    }

    impl Requests for Script {
        fn next(&mut self, limit: Option<Duration>) -> Next {
            loop {
                match self.steps.front_mut() {
                    None => return Next::Closed,
                    Some(Step::Idle(remaining)) => {
                        let mut clock = self.clock.lock().unwrap();
                        if let Some(limit) = limit.filter(|limit| limit <= remaining) {
                            *remaining -= limit;
                            *clock += limit;
                            return Next::Idle;
                        }
                        *clock += *remaining;
                        self.steps.pop_front();
                    }
                    Some(Step::Send(_)) => {
                        let Some(Step::Send(request)) = self.steps.pop_front() else {
                            unreachable!()
                        };
                        return Next::Request(request);
                    }
                }
            }
        }
    }

    /// Records the virtual time of each service read.
    struct RecordingService {
        clock: Arc<std::sync::Mutex<Duration>>,
        reads: Arc<std::sync::Mutex<Vec<Duration>>>,
    }

    impl StatusService for RecordingService {
        fn status(&self) -> Result<ServiceStatus, ServerError> {
            let time = *self.clock.lock().unwrap();
            self.reads.lock().unwrap().push(time);
            Ok(ServiceStatus::default())
        }
        fn start(&self) -> Result<ServiceStatus, ServerError> {
            Err(ServerError::Unsupported)
        }
        #[cfg(target_os = "macos")]
        fn prepare(&self, _: Action) -> Result<Confirmation, ServerError> {
            Err(ServerError::Unsupported)
        }
        #[cfg(target_os = "macos")]
        fn confirm(&self, _: Confirmation) -> Result<ServiceStatus, ServerError> {
            Err(ServerError::Unsupported)
        }
    }

    /// The worker reads the service when the menu opens and when the service changes, and
    /// never while hours pass with no request.
    #[test]
    fn the_status_worker_reads_nothing_while_idle() {
        const HOUR: Duration = Duration::from_secs(3600);
        let clock = Arc::new(std::sync::Mutex::new(Duration::ZERO));
        let reads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = RecordingService {
            clock: Arc::clone(&clock),
            reads: Arc::clone(&reads),
        };
        let mut script = Script {
            steps: [
                Step::Idle(HOUR),
                Step::Send(Request::Refresh),
                Step::Idle(HOUR),
                Step::Send(Request::Poll),
                Step::Idle(HOUR),
            ]
            .into(),
            clock: Arc::clone(&clock),
        };
        let offers = std::cell::Cell::new(0);
        serve(
            &mut script,
            &service,
            &|| {
                offers.set(offers.get() + 1);
                false
            },
            &|_| {},
        );
        assert_eq!(
            *reads.lock().unwrap(),
            [HOUR, 2 * HOUR],
            "one read when the menu opens and one after the service changed"
        );
        assert_eq!(
            offers.get(),
            1,
            "only opening the menu reads the update offer"
        );
        assert_eq!(*clock.lock().unwrap(), 3 * HOUR);
    }
}
