//! The client half of the service lane: one request at a time, and the confirmed-operation flow
//! of Stop, Restart and Update. The flow never resends `Execute` with a new operation ID. After a
//! lost connection it only resolves the operation it already admitted.
#![allow(
    dead_code,
    reason = "the CLI's server commands connect it over IPC and QUIC at integration"
)]
use super::{LaneAction, LaneReceipt, LaneReply, LaneRequest, MAX_FRAME, OperationId};
use crate::ServerError;
use crate::protocol::Preamble;
use std::time::{Duration, Instant};

/// How long a lane request may take before the client gives up on its connection. Staging a
/// fetched archive is bounded by its own download limits instead.
pub const REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// How often the flow asks for an admitted operation's receipt.
const POLL: Duration = Duration::from_secs(1);

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
}

impl From<ServerError> for LaneError {
    fn from(error: ServerError) -> Self {
        Self::Server(error)
    }
}

/// One admitted lane connection.
pub struct Session {
    stream: Box<dyn Preamble + Send>,
}

impl Session {
    pub(crate) fn open(connector: &dyn Connector, deadline: Instant) -> Result<Self, ServerError> {
        Ok(Self {
            stream: connector.connect(deadline)?,
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

/// How a confirmed operation ended, as far as the client could observe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Stop was admitted and the server closed the lane. Completion is confirmed on the machine
    /// that runs the server, after its next start.
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
/// before `Execute` is sent, so the caller can record the operation for later settlement.
pub(crate) fn perform(
    connector: &dyn Connector,
    mut session: Session,
    action: LaneAction,
    token: String,
    wait: Duration,
    admitted: &mut dyn FnMut(&OperationId) -> Result<(), ServerError>,
) -> Result<Outcome, LaneError> {
    let id = OperationId::random()?;
    admitted(&id)?;
    let operation = id.to_string();
    let stop = action == LaneAction::Stop;
    let reply = session.call(
        &LaneRequest::Execute {
            action,
            token,
            operation: operation.clone(),
        },
        Instant::now() + REQUEST_LIMIT,
    )?;
    let LaneReply::Accepted { boot, .. } = reply else {
        return Err(LaneError::Server(ServerError::Protocol));
    };
    let deadline = Instant::now() + wait;
    let mut current = Some(session);
    let mut last = None;
    loop {
        if current.is_none() && !stop {
            current = Session::open(connector, deadline.min(Instant::now() + REQUEST_LIMIT)).ok();
        }
        if let Some(session) = current.as_mut() {
            match session.request(
                &LaneRequest::Resolve {
                    operation: operation.clone(),
                },
                Instant::now() + REQUEST_LIMIT,
            ) {
                Ok(LaneReply::Receipt(receipt)) if settled(&receipt) => {
                    let _ = session.request(
                        &LaneRequest::Acknowledge {
                            operation: operation.clone(),
                        },
                        Instant::now() + REQUEST_LIMIT,
                    );
                    return Ok(Outcome::Settled { receipt, boot });
                }
                Ok(LaneReply::Receipt(receipt)) => last = Some(receipt),
                Ok(LaneReply::Error { code }) => return Err(LaneError::Code(code)),
                Ok(_) => return Err(LaneError::Server(ServerError::Protocol)),
                // The server closed the lane: it is stopping, or it already stopped.
                Err(_) if stop => return Ok(Outcome::Shutdown { operation, boot }),
                Err(_) => current = None,
            }
        }
        if Instant::now() + POLL >= deadline {
            return Ok(Outcome::Unconfirmed {
                operation,
                boot,
                last,
            });
        }
        std::thread::sleep(POLL);
    }
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

/// A route to one server's service lane. Opening it never starts a server.
pub struct Lane {
    connector: Box<dyn Connector>,
}

impl Lane {
    /// The lane of this account's server, as `client` named `name`.
    pub fn local(
        identity: &str,
        client: [u8; 32],
        name: crate::pane::control::ClientName,
    ) -> Result<Self, ServerError> {
        let paths = crate::host::Paths::for_identity(identity)?;
        let server = crate::storage::Directory::open(&paths.data, false)?
            .read()?
            .server;
        Ok(Self {
            connector: Box::new(Local {
                paths,
                server,
                client,
                name,
                agent: None,
            }),
        })
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
        admitted: &mut dyn FnMut(&OperationId) -> Result<(), ServerError>,
    ) -> Result<Outcome, LaneError> {
        perform(
            self.connector.as_ref(),
            session,
            action,
            token,
            wait,
            admitted,
        )
    }
}
