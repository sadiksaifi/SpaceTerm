//! One bounded session-bus connection shared by Linux desktop capabilities.
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::mpsc::{self, SyncSender};
use std::task::Poll;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::export::futures_core::Stream;

pub(super) const METHOD_TIMEOUT: Duration = Duration::from_millis(500);
/// A service that creates a desktop object, such as a notification or a preview window, can
/// answer after `METHOD_TIMEOUT` while starting. Its owner waits this long on a dedicated
/// connection, so a slow reply still transfers ownership without stalling shared work.
pub(super) const RETAINED_REPLY_TIMEOUT: Duration = Duration::from_secs(25);
const QUEUE_LIMIT: usize = 32;
type Job = Box<dyn FnOnce(&Connection) + Send>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum SessionBusError {
    #[error("the desktop service is unavailable")]
    Unavailable,
    #[error("the desktop service rejected the operation")]
    Rejected,
    #[error("the desktop service did not respond")]
    TimedOut,
}

impl From<zbus::Error> for SessionBusError {
    fn from(error: zbus::Error) -> Self {
        match error {
            zbus::Error::InputOutput(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                Self::TimedOut
            }
            zbus::Error::InputOutput(_) | zbus::Error::Connection(_, _) => Self::Unavailable,
            zbus::Error::FDO(error)
                if matches!(
                    *error,
                    zbus::fdo::Error::NoReply(_) | zbus::fdo::Error::Timeout(_)
                ) =>
            {
                Self::TimedOut
            }
            zbus::Error::MethodError(name, _, _)
                if name.as_str().ends_with("NoReply") || name.as_str().ends_with("Timeout") =>
            {
                Self::TimedOut
            }
            zbus::Error::MethodError(name, _, _)
                if name.as_str().ends_with("ServiceUnknown")
                    || name.as_str().ends_with("NameHasNoOwner") =>
            {
                Self::Unavailable
            }
            _ => Self::Rejected,
        }
    }
}

#[derive(Clone)]
pub(super) struct SessionBus {
    jobs: SyncSender<Job>,
    /// `None` is the user's session bus.
    address: Option<String>,
}

impl SessionBus {
    pub(super) fn connect() -> Result<Self, SessionBusError> {
        Self::connect_to(None)
    }
    pub(super) fn connect_to(address: Option<String>) -> Result<Self, SessionBusError> {
        Self::connect_with(address, METHOD_TIMEOUT)
    }
    /// A separate connection and worker on the same bus, for one service's ordered calls.
    pub(super) fn dedicated(&self, method_timeout: Duration) -> Result<Self, SessionBusError> {
        Self::connect_with(self.address.clone(), method_timeout)
    }
    fn connect_with(
        address: Option<String>,
        method_timeout: Duration,
    ) -> Result<Self, SessionBusError> {
        let (jobs, receiver) = mpsc::sync_channel::<Job>(QUEUE_LIMIT);
        let (ready, result) = mpsc::sync_channel::<Result<(), SessionBusError>>(1);
        let connection_address = address.clone();
        std::thread::Builder::new()
            .name("desktop-session-bus".into())
            .spawn(move || {
                let connection = match connection_address {
                    Some(address) => zbus::blocking::connection::Builder::address(address.as_str()),
                    None => zbus::blocking::connection::Builder::session(),
                }
                .and_then(|builder| {
                    builder
                        .method_timeout(method_timeout)
                        .max_queued(QUEUE_LIMIT)
                        .build()
                });
                match connection {
                    Ok(connection) => {
                        if ready.send(Ok(())).is_err() {
                            return;
                        }
                        while let Ok(job) = receiver.recv() {
                            job(&connection);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error.into()));
                    }
                }
            })
            .map_err(|_| SessionBusError::Unavailable)?;
        result
            .recv_timeout(METHOD_TIMEOUT)
            .map_err(|_| SessionBusError::TimedOut)??;
        Ok(Self { jobs, address })
    }

    /// Enqueues work without blocking the UI or retaining an unbounded request backlog.
    pub(super) fn dispatch(
        &self,
        job: impl FnOnce(&Connection) + Send + 'static,
    ) -> Result<(), SessionBusError> {
        self.jobs
            .try_send(Box::new(job))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => SessionBusError::Rejected,
                mpsc::TrySendError::Disconnected(_) => SessionBusError::Unavailable,
            })
    }

    /// Startup-only bounded discovery. Runtime operations use `dispatch`.
    pub(super) fn query<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Connection) -> Result<T, SessionBusError> + Send + 'static,
    ) -> Result<T, SessionBusError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.dispatch(move |connection| {
            let _ = sender.send(job(connection));
        })?;
        receiver
            .recv_timeout(METHOD_TIMEOUT * 2)
            .map_err(|_| SessionBusError::TimedOut)?
    }

    pub(super) fn available(&self, name: &'static str) -> bool {
        self.query(move |connection| {
            let proxy =
                zbus::blocking::fdo::DBusProxy::new(connection).map_err(SessionBusError::from)?;
            Ok(proxy
                .list_names()
                .map_err(|_| SessionBusError::Unavailable)?
                .iter()
                .any(|value| value.as_str() == name)
                || proxy
                    .list_activatable_names()
                    .map_err(|_| SessionBusError::Unavailable)?
                    .iter()
                    .any(|value| value.as_str() == name))
        })
        .unwrap_or(false)
    }

    /// Signal setup uses the same connection as calls. Cancellation wakes the worker even
    /// when the service never sends another signal, and deregisters the match rule on exit.
    pub(super) fn subscribe(
        &self,
        rule: zbus::OwnedMatchRule,
        mut receive: impl FnMut(zbus::Message) + Send + 'static,
    ) -> Result<BusSubscription, SessionBusError> {
        let iterator = self.query(move |connection| {
            MessageIterator::for_match_rule(rule, connection, Some(QUEUE_LIMIT)).map_err(Into::into)
        })?;
        let (stop, stopped) = async_channel::bounded::<()>(1);
        std::thread::Builder::new()
            .name("desktop-bus-signals".into())
            .spawn(move || {
                let mut stream = iterator.into_inner();
                pollster::block_on(async {
                    let stopped = stopped.recv();
                    let mut stopped = std::pin::pin!(stopped);
                    loop {
                        let message = poll_fn(|cx| {
                            if stopped.as_mut().poll(cx).is_ready() {
                                return Poll::Ready(None);
                            }
                            Pin::new(&mut stream).poll_next(cx)
                        })
                        .await;
                        match message {
                            Some(Ok(message)) => receive(message),
                            Some(Err(_)) | None => break,
                        }
                    }
                    zbus::AsyncDrop::async_drop(stream).await;
                });
            })
            .map_err(|_| SessionBusError::Unavailable)?;
        Ok(BusSubscription { stop })
    }
}

pub(super) struct BusSubscription {
    stop: async_channel::Sender<()>,
}
impl Drop for BusSubscription {
    fn drop(&mut self) {
        self.stop.close();
    }
}
