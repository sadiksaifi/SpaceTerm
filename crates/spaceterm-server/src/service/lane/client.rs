//! The client half of the service lane: one request at a time, and the confirmed-operation flow
//! of Stop, Restart and Update. The flow never sends `Execute` with a new operation ID. When the
//! connection is lost after `Execute` was sent, the operation may already be durable and running,
//! so the flow reconnects and follows that same operation ID.
use super::{LaneAction, LaneReceipt, LaneReply, LaneRequest, MAX_FRAME, OperationId};
use crate::ServerError;
use crate::protocol::Preamble;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

/// How long a lane request may take before the client gives up on its connection. Staging a
/// fetched archive is bounded by its own download limits instead.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How often the flow asks for an admitted operation's receipt.
const POLL: Duration = Duration::from_secs(1);
/// A session idle this long is replaced before `Execute`. The server closes a lane that stays
/// silent for 30 s, for example while a person answers a confirmation prompt.
const STALE: Duration = Duration::from_secs(20);
/// How often [`within`] reads its deadline again while its work blocks.
const CHECK: Duration = Duration::from_millis(100);
/// The thread name of lane I/O that [`within`] runs.
const LANE_IO: &str = "spaceterm-lane-io";

/// Opens and admits a fresh service-lane connection.
pub(crate) trait Connector: Send + Sync {
    fn connect(&self, deadline: Instant) -> Result<Box<dyn Preamble + Send>, ServerError>;
}

/// Why a lane request did not produce the reply it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneError {
    /// The server answered `Error { code }`, a code from the CLI output contract.
    Code(String),
    /// The connection failed, or the reply was not the one requested.
    Server(ServerError),
    /// The caller was interrupted before `Execute` was sent, so nothing was dispatched.
    Interrupted,
}

impl From<ServerError> for LaneError {
    fn from(error: ServerError) -> Self {
        Self::Server(error)
    }
}

/// One admitted lane connection.
pub struct Session {
    /// `None` once a request was abandoned at its deadline, which leaves the connection
    /// mid-frame.
    stream: Option<Box<dyn Preamble + Send>>,
    /// When the last request was sent, or the connection opened.
    used: Instant,
}

impl Session {
    pub(crate) fn open(connector: &dyn Connector, deadline: Instant) -> Result<Self, ServerError> {
        Ok(Self {
            stream: Some(connector.connect(deadline)?),
            used: Instant::now(),
        })
    }

    /// Like [`Self::open`], but stops waiting as soon as `deadline()` passes.
    fn open_within(
        connector: &Arc<dyn Connector>,
        deadline: &dyn Fn() -> Instant,
    ) -> Result<Self, ServerError> {
        let connector = Arc::clone(connector);
        let fixed = deadline();
        let stream = within(LANE_IO, &|| Some(deadline()), move || {
            connector.connect(fixed)
        })
        .ok_or(ServerError::Protocol)??;
        Ok(Self {
            stream: Some(stream),
            used: Instant::now(),
        })
    }

    /// Sends one request and reads its reply. A lane `Error` is returned as a reply.
    pub fn request(
        &mut self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<LaneReply, ServerError> {
        let bytes = frame(request)?;
        let stream = self.stream.as_mut().ok_or(ServerError::Protocol)?;
        self.used = Instant::now();
        stream.send(&bytes, deadline)?;
        let reply = stream.receive(MAX_FRAME, deadline)?;
        serde_json::from_slice(&reply).map_err(|_| ServerError::Protocol)
    }

    /// Like [`Self::request`], but stops waiting as soon as `deadline()` passes. A request
    /// abandoned that way ends this session.
    fn request_within(
        &mut self,
        request: &LaneRequest,
        deadline: &dyn Fn() -> Instant,
    ) -> Result<LaneReply, ServerError> {
        let bytes = frame(request)?;
        let mut stream = self.stream.take().ok_or(ServerError::Protocol)?;
        self.used = Instant::now();
        let fixed = deadline();
        let (stream, reply) = within(LANE_IO, &|| Some(deadline()), move || {
            let reply = stream
                .send(&bytes, fixed)
                .and_then(|()| stream.receive(MAX_FRAME, fixed));
            (stream, reply)
        })
        .ok_or(ServerError::Protocol)?;
        self.stream = Some(stream);
        serde_json::from_slice(&reply?).map_err(|_| ServerError::Protocol)
    }

    /// Sends one request and turns a lane `Error` into [`LaneError::Code`].
    pub fn call(
        &mut self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<LaneReply, LaneError> {
        match self.request(request, deadline)? {
            LaneReply::Error { code } => Err(LaneError::Code(code)),
            reply => Ok(reply),
        }
    }
}

fn frame(request: &LaneRequest) -> Result<Vec<u8>, ServerError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ServerError::Protocol)?;
    if bytes.len() > MAX_FRAME {
        return Err(ServerError::Protocol);
    }
    Ok(bytes)
}

/// Runs blocking `work` on its own thread named `name`, so the caller stops waiting as soon as
/// `deadline()` passes. `deadline()` is read again every [`CHECK`], so an interrupt that sets it
/// or moves it closer ends the wait while a stalled server or activation still holds `work`.
/// `None` from `deadline()` means no deadline yet. Abandoned work runs on until it ends by
/// itself, such as lane I/O at its own fixed deadline. `None` means the deadline passed first.
pub fn within<T: Send + 'static>(
    name: &str,
    deadline: &dyn Fn() -> Option<Instant>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    loop {
        let wait = match deadline() {
            None => CHECK,
            Some(until) => until.checked_duration_since(Instant::now())?.min(CHECK),
        };
        match receiver.recv_timeout(wait) {
            Ok(value) => return Some(value),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// How a confirmed operation ended, as far as the client could observe. `boot` is empty when
/// the flow never learned which run of the server admitted the operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stop was admitted, or may have been, and the server closed the lane. Completion is
    /// confirmed on the machine that runs the server, after its next start.
    Shutdown { operation: String, boot: String },
    /// The operation settled. `boot` is the run of the server that admitted it.
    Settled { receipt: LaneReceipt, boot: String },
    /// No settled receipt arrived before the deadline.
    Unconfirmed {
        operation: String,
        boot: String,
        last: Option<LaneReceipt>,
    },
}

/// Executes a previewed action and follows it until it settles or `wait` passes. `admitted` runs
/// before `Execute` is sent, so the caller can record the operation for later settlement, or
/// refuse to dispatch it.
///
/// `interrupted` returns the instant by which an interrupted caller must settle, once it was
/// interrupted. From then on the flow only resolves: it never sends `Execute` again. Every
/// connection and request reads it again while it waits, so a stalled server cannot hold the
/// flow past that instant.
pub(crate) fn perform(
    connector: &Arc<dyn Connector>,
    session: Session,
    action: LaneAction,
    token: String,
    wait: Duration,
    admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    let mut session = if session.used.elapsed() >= STALE {
        Session::open(connector.as_ref(), Instant::now() + REQUEST_LIMIT)?
    } else {
        session
    };
    let id = OperationId::random()?;
    admitted(&id)?;
    let operation = id.to_string();
    let stop = action == LaneAction::Stop;
    let execute = LaneRequest::Execute {
        action,
        token,
        operation: operation.clone(),
    };
    let started = Instant::now();
    let deadline = || interrupted().into_iter().fold(started + wait, Instant::min);
    // Fixed per request from when it is sent, and moved closer by an interrupt.
    let request_deadline = |sent: Instant| move || deadline().min(sent + REQUEST_LIMIT);
    let (mut boot, mut current) =
        match session.request_within(&execute, &request_deadline(Instant::now())) {
            Ok(LaneReply::Accepted { boot, .. }) => (Some(boot), Some(session)),
            // A refused Execute admitted nothing.
            Ok(LaneReply::Error { code }) => return Err(LaneError::Code(code)),
            // Execute was sent, so the operation may be durable: follow the same operation ID.
            Ok(_) | Err(_) => (None, None),
        };
    let mut last = None;
    loop {
        if current.is_none() && (boot.is_none() || !stop) {
            current = Session::open_within(connector, &request_deadline(Instant::now())).ok();
        }
        match current.as_mut() {
            // Without the lane, a Stop that may have been admitted is reported as in progress.
            None if stop => {
                return Ok(Outcome::Shutdown {
                    operation,
                    boot: boot.unwrap_or_default(),
                });
            }
            None => {}
            // The first reply was lost. A repeated Execute returns the existing admission, or
            // admits the operation now if the first never arrived. An interrupted caller only
            // resolves, so nothing new is dispatched after the interrupt.
            Some(session) if boot.is_none() && interrupted().is_none() => {
                match session.request_within(&execute, &request_deadline(Instant::now())) {
                    Ok(LaneReply::Accepted { boot: admitted, .. }) => {
                        boot = Some(admitted);
                        continue;
                    }
                    Ok(LaneReply::Error { code }) => {
                        match resolve(session, &operation, &request_deadline(Instant::now())) {
                            Ok(LaneReply::Receipt(receipt)) => boot = Some(receipt.boot),
                            // Executes are serialized, so no record means none was admitted.
                            Ok(LaneReply::Error { code: missing }) if missing == "not_found" => {
                                return Err(LaneError::Code(code));
                            }
                            Ok(_) | Err(_) => current = None,
                        }
                    }
                    Ok(_) | Err(_) => current = None,
                }
            }
            Some(session) => {
                match resolve(session, &operation, &request_deadline(Instant::now())) {
                    Ok(LaneReply::Receipt(receipt)) if settled(&receipt) => {
                        let _ = session.request_within(
                            &LaneRequest::Acknowledge {
                                operation: operation.clone(),
                            },
                            &request_deadline(Instant::now()),
                        );
                        let boot = boot.unwrap_or_else(|| receipt.boot.clone());
                        return Ok(Outcome::Settled { receipt, boot });
                    }
                    Ok(LaneReply::Receipt(receipt)) => {
                        boot.get_or_insert_with(|| receipt.boot.clone());
                        last = Some(receipt);
                    }
                    // The operation may still arrive on the lost connection, so this proves nothing.
                    Ok(LaneReply::Error { code }) if code == "not_found" && boot.is_none() => {}
                    Ok(LaneReply::Error { code }) => return Err(LaneError::Code(code)),
                    Ok(_) => return Err(LaneError::Server(ServerError::Protocol)),
                    // The server closed the lane: it is stopping, or it already stopped.
                    Err(_) if stop => {
                        return Ok(Outcome::Shutdown {
                            operation,
                            boot: boot.unwrap_or_default(),
                        });
                    }
                    Err(_) => current = None,
                }
            }
        }
        if Instant::now() + POLL >= deadline() {
            return Ok(Outcome::Unconfirmed {
                operation,
                boot: boot.unwrap_or_default(),
                last,
            });
        }
        std::thread::sleep(POLL);
    }
}

fn resolve(
    session: &mut Session,
    operation: &str,
    deadline: &dyn Fn() -> Instant,
) -> Result<LaneReply, ServerError> {
    session.request_within(
        &LaneRequest::Resolve {
            operation: operation.to_owned(),
        },
        deadline,
    )
}

/// Whether a receipt will never change again.
pub fn settled(receipt: &LaneReceipt) -> bool {
    use super::LaneOperationState as State;
    matches!(
        receipt.state,
        State::Done | State::Failed | State::NotPerformed | State::Unresolved
    )
}

/// What became of an operation an earlier command admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Leftover {
    /// It settled, and this call acknowledged it.
    Settled(LaneReceipt),
    /// It has not settled yet. A later command asks again.
    Pending,
    /// The server no longer holds it, for example after its pairing was revoked.
    Gone,
}

/// Resolves an operation an earlier command admitted, and acknowledges it once settled.
pub fn settle_leftover(
    session: &mut Session,
    operation: &OperationId,
) -> Result<Leftover, LaneError> {
    let deadline = Instant::now() + REQUEST_LIMIT;
    let reply = session.request(
        &LaneRequest::Resolve {
            operation: operation.to_string(),
        },
        deadline,
    )?;
    match reply {
        LaneReply::Receipt(receipt) if settled(&receipt) => {
            session.call(
                &LaneRequest::Acknowledge {
                    operation: operation.to_string(),
                },
                deadline,
            )?;
            Ok(Leftover::Settled(receipt))
        }
        LaneReply::Receipt(_) => Ok(Leftover::Pending),
        LaneReply::Error { code } if code == "not_found" => Ok(Leftover::Gone),
        LaneReply::Error { code } => Err(LaneError::Code(code)),
        _ => Err(LaneError::Server(ServerError::Protocol)),
    }
}

/// The service lane of this account's own server, over account IPC.
struct Local {
    paths: crate::host::Paths,
    server: [u8; 32],
    client: [u8; 32],
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
}

impl Connector for Local {
    fn connect(&self, deadline: Instant) -> Result<Box<dyn Preamble + Send>, ServerError> {
        if Instant::now() >= deadline {
            return Err(ServerError::NotRunning);
        }
        let mut stream = crate::host::connect(&self.paths)?;
        crate::protocol::admit_service_lane(
            &mut stream,
            crate::protocol::Caller {
                client: &self.client,
                name: &self.name,
                agent: self.agent.as_ref(),
            },
            &self.server,
            &self.paths.identity,
        )?;
        Ok(Box::new(stream))
    }
}

/// The service lane of a paired server, over its authenticated direct connection.
struct Paired {
    connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
    paired: crate::direct::PairedDevice,
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
}

impl Connector for Paired {
    fn connect(&self, deadline: Instant) -> Result<Box<dyn Preamble + Send>, ServerError> {
        if Instant::now() >= deadline {
            return Err(crate::direct::DirectError::Transport.into());
        }
        let connection = self.connector.connect()?;
        if connection.paired() != &self.paired {
            return Err(ServerError::AuthenticationRefused);
        }
        let stream = connection.admit_service_lane(crate::protocol::Caller {
            client: &self.paired.client,
            name: &self.name,
            agent: self.agent.as_ref(),
        })?;
        Ok(stream)
    }
}

/// A route to one server's service lane. Opening it never starts a server.
pub struct Lane {
    connector: Arc<dyn Connector>,
}

impl Lane {
    /// The lane of the account server at `paths`, as base identity `client` named `name`, or as
    /// `agent` when given.
    pub(crate) fn local(
        paths: crate::host::Paths,
        server: [u8; 32],
        client: [u8; 32],
        name: crate::pane::control::ClientName,
        agent: Option<crate::protocol::AgentName>,
    ) -> Self {
        Self {
            connector: Arc::new(Local {
                paths,
                server,
                client,
                name,
                agent,
            }),
        }
    }

    /// The lane of a paired server, as its pairing's Client Identity or as `agent`.
    pub(crate) fn paired(
        connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
        paired: crate::direct::PairedDevice,
        name: crate::pane::control::ClientName,
        agent: Option<crate::protocol::AgentName>,
    ) -> Self {
        Self {
            connector: Arc::new(Paired {
                connector,
                paired,
                name,
                agent,
            }),
        }
    }

    /// A lane over `connector`, for flows tested against a served fixture lane.
    #[cfg(test)]
    pub(crate) fn over(connector: Arc<dyn Connector>) -> Self {
        Self { connector }
    }

    /// Opens and admits one connection.
    pub fn session(&self) -> Result<Session, ServerError> {
        Session::open(self.connector.as_ref(), Instant::now() + REQUEST_LIMIT)
    }

    /// Executes a previewed action and follows it. See [`perform`].
    pub fn perform(
        &self,
        session: Session,
        action: LaneAction,
        token: String,
        wait: Duration,
        admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
        interrupted: &dyn Fn() -> Option<Instant>,
    ) -> Result<Outcome, LaneError> {
        perform(
            &self.connector,
            session,
            action,
            token,
            wait,
            admitted,
            interrupted,
        )
    }
}
