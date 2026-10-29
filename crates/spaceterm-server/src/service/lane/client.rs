//! The client half of the service lane: one request at a time, and the confirmed-operation flow
//! of Stop, Restart and Update. The flow never sends `Execute` with a new operation ID. When the
//! connection is lost after `Execute` was sent, the operation may already be durable and running,
//! so the flow reconnects and follows that same operation ID.
use super::{LaneAction, LaneReceipt, LaneReply, LaneRequest, MAX_FRAME, OperationId};
use crate::ServerError;
use crate::protocol::Preamble;
use std::time::{Duration, Instant};

/// How long a lane request may take before the client gives up on its connection. Staging a
/// fetched archive is bounded by its own download limits instead.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How often the flow asks for an admitted operation's receipt.
const POLL: Duration = Duration::from_secs(1);
/// A session idle this long is replaced before `Execute`. The server closes a lane that stays
/// silent for 30 s, for example while a person answers a confirmation prompt.
const STALE: Duration = Duration::from_secs(20);

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
    stream: Box<dyn Preamble + Send>,
    /// When the last request was sent, or the connection opened.
    used: Instant,
}

impl Session {
    pub(crate) fn open(connector: &dyn Connector, deadline: Instant) -> Result<Self, ServerError> {
        Ok(Self {
            stream: connector.connect(deadline)?,
            used: Instant::now(),
        })
    }

    /// Sends one request and reads its reply. A lane `Error` is returned as a reply.
    pub fn request(
        &mut self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<LaneReply, ServerError> {
        let bytes = serde_json::to_vec(request).map_err(|_| ServerError::Protocol)?;
        if bytes.len() > MAX_FRAME {
            return Err(ServerError::Protocol);
        }
        self.used = Instant::now();
        self.stream.send(&bytes, deadline)?;
        let reply = self.stream.receive(MAX_FRAME, deadline)?;
        serde_json::from_slice(&reply).map_err(|_| ServerError::Protocol)
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
/// interrupted. From then on the flow only resolves: it never sends `Execute` again.
pub(crate) fn perform(
    connector: &dyn Connector,
    session: Session,
    action: LaneAction,
    token: String,
    wait: Duration,
    admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    let mut session = if session.used.elapsed() >= STALE {
        Session::open(connector, Instant::now() + REQUEST_LIMIT)?
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
    let request_deadline = || deadline().min(Instant::now() + REQUEST_LIMIT);
    let (mut boot, mut current) = match session.request(&execute, request_deadline()) {
        Ok(LaneReply::Accepted { boot, .. }) => (Some(boot), Some(session)),
        // A refused Execute admitted nothing.
        Ok(LaneReply::Error { code }) => return Err(LaneError::Code(code)),
        // Execute was sent, so the operation may be durable: follow the same operation ID.
        Ok(_) | Err(_) => (None, None),
    };
    let mut last = None;
    loop {
        if current.is_none() && (boot.is_none() || !stop) {
            current = Session::open(connector, request_deadline()).ok();
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
                match session.request(&execute, request_deadline()) {
                    Ok(LaneReply::Accepted { boot: admitted, .. }) => {
                        boot = Some(admitted);
                        continue;
                    }
                    Ok(LaneReply::Error { code }) => {
                        match resolve(session, &operation, request_deadline()) {
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
            Some(session) => match resolve(session, &operation, request_deadline()) {
                Ok(LaneReply::Receipt(receipt)) if settled(&receipt) => {
                    let _ = session.request(
                        &LaneRequest::Acknowledge {
                            operation: operation.clone(),
                        },
                        request_deadline(),
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
            },
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
    deadline: Instant,
) -> Result<LaneReply, ServerError> {
    session.request(
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
    connector: Box<dyn Connector>,
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
            connector: Box::new(Local {
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
            connector: Box::new(Paired {
                connector,
                paired,
                name,
                agent,
            }),
        }
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
            self.connector.as_ref(),
            session,
            action,
            token,
            wait,
            admitted,
            interrupted,
        )
    }
}
