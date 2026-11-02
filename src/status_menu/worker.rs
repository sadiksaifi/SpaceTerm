//! Runs every service read and operation away from the UI thread, one at a time.
use super::model::Command;
#[cfg(target_os = "macos")]
use spaceterm_server::service::control::{Action, Confirmation};
use spaceterm_server::{
    ServerError,
    connection::link::{Health, LinkEvent, LinkSink},
    service::{ServiceStatus, control::Controller},
};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

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

/// The worker reads the service only when asked: when the menu opens, after an operation, or when
/// [`Worker::changes`] reports that the local server's link changed. It never reads on a timer,
/// so an idle client does no status work.
pub(crate) struct Worker {
    requests: mpsc::Sender<Request>,
    changes: Arc<ServerChanges>,
}

impl Worker {
    /// `deliver` runs on the worker thread and hands each event to the UI thread.
    /// `update_offered` reads whether a verified build is offered, without staging anything.
    pub(crate) fn spawn(
        controller: Controller,
        update_offered: impl Fn() -> bool + Send + 'static,
        deliver: impl Fn(Event) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (requests, received) = mpsc::channel();
        std::thread::Builder::new()
            .name("server-status".into())
            .spawn(move || {
                loop {
                    let Ok(request) = received.recv() else {
                        return;
                    };
                    match request {
                        Request::Refresh => {
                            deliver(Event::Status(controller.status()));
                            deliver(Event::Update(update_offered()));
                        }
                        Request::Poll => deliver(Event::Status(controller.status())),
                        Request::Start => {
                            finish(&controller, &deliver, Command::Start, controller.start())
                        }
                        #[cfg(target_os = "macos")]
                        Request::Prepare(action) => {
                            deliver(Event::Prepared(action, controller.prepare(action)))
                        }
                        #[cfg(target_os = "macos")]
                        Request::Confirm(command, confirmation) => {
                            let result = controller.confirm(confirmation);
                            finish(&controller, &deliver, command, result)
                        }
                    }
                }
            })?;
        let changes = Arc::new(ServerChanges {
            connected: AtomicBool::new(false),
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

/// Asks for a status read when the local server's link ends, and when a later link's first
/// inventory shows a server admitted the client again. A stopped, crashed, restarted or updated
/// server changes the link, so these two moments cover every change the item shows while its
/// menu is closed.
struct ServerChanges {
    /// Whether an inventory arrived since the last link ended.
    connected: AtomicBool,
    requests: mpsc::Sender<Request>,
}

impl LinkSink for ServerChanges {
    fn deliver(&self, event: LinkEvent) {
        let changed = match event {
            LinkEvent::Health(Health::Closed(_)) => self.connected.swap(false, Ordering::AcqRel),
            LinkEvent::Inventory(_) => !self.connected.swap(true, Ordering::AcqRel),
            LinkEvent::Health(Health::Live) => false,
        };
        if changed {
            let _ = self.requests.send(Request::Poll);
        }
    }
}

/// A refused operation leaves the service as it was, so the item reads it again.
fn finish(
    controller: &Controller,
    deliver: &impl Fn(Event),
    command: Command,
    result: Result<ServiceStatus, ServerError>,
) {
    let refused = result.is_err();
    deliver(Event::Finished(command, result));
    if refused {
        deliver(Event::Status(controller.status()));
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
            connected: AtomicBool::new(false),
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
}
