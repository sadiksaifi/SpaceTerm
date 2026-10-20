//! Runs every service read and operation away from the UI thread, one at a time.
use super::model::Command;
#[cfg(target_os = "macos")]
use spaceterm_server::service::control::{Action, Confirmation};
use spaceterm_server::{
    ServerError,
    service::{ServiceStatus, control::Controller},
};
use std::{
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

/// Status reads only observe the service, so the item refreshes on this period and whenever its
/// menu opens.
const REFRESH: Duration = Duration::from_secs(10);

pub(crate) enum Request {
    /// Reads the service, and whether a verified server build is offered to update to.
    Refresh,
    /// Reads the service only, as the periodic refresh does. Verifying the included build is
    /// left to reads the person causes, such as opening the menu.
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

pub(crate) struct Worker {
    requests: mpsc::Sender<Request>,
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
                    let request = match received.recv_timeout(REFRESH) {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => Request::Poll,
                        Err(RecvTimeoutError::Disconnected) => return,
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
        Ok(Self { requests })
    }

    pub(crate) fn send(&self, request: Request) {
        let _ = self.requests.send(request);
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
