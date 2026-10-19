//! Runs every service read and operation away from the UI thread, one at a time.
use crate::{StatusError, menu::Command};
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

pub enum Request {
    Refresh,
    Start,
    /// On Linux the desktop's control-only window prepares and confirms instead.
    #[cfg(target_os = "macos")]
    Prepare(Action),
    /// Performs the confirmed operation the menu's command prepared.
    #[cfg(target_os = "macos")]
    Confirm(Command, Confirmation),
}

pub enum Event {
    Status(Result<ServiceStatus, ServerError>),
    /// A confirmation to present, or why the operation is no longer available.
    #[cfg(target_os = "macos")]
    Prepared(Action, Result<Confirmation, ServerError>),
    Finished(Command, Result<ServiceStatus, ServerError>),
}

pub struct Worker {
    requests: mpsc::Sender<Request>,
}

impl Worker {
    /// `deliver` runs on the worker thread and hands each event to the UI thread.
    pub fn spawn(
        controller: Controller,
        deliver: impl Fn(Event) + Send + 'static,
    ) -> Result<Self, StatusError> {
        let (requests, received) = mpsc::channel();
        std::thread::Builder::new()
            .name("server-status".into())
            .spawn(move || {
                loop {
                    let request = match received.recv_timeout(REFRESH) {
                        Ok(request) => request,
                        Err(RecvTimeoutError::Timeout) => Request::Refresh,
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    match request {
                        Request::Refresh => deliver(Event::Status(controller.status())),
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
            })
            .map_err(|_| StatusError::Unavailable)?;
        Ok(Self { requests })
    }

    pub fn send(&self, request: Request) {
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
