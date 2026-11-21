//! The client half of the service lane: one request at a time, and the confirmed-operation flow
//! of Stop, Restart and Update. The flow never sends `Execute` with a new operation ID. When the
//! connection is lost after `Execute` was sent, the operation may already be durable and running,
//! so the flow reconnects and follows that same operation ID.
use super::{LaneAction, LaneReceipt, LaneReply, LaneRequest, MAX_FRAME, OperationId};
use crate::CoreError;
use crate::protocol::capability::Admissions;
use crate::protocol::version::ProtocolVersion;
use crate::protocol::{Lane as Admitted, Preamble};
use std::sync::Arc;
use std::sync::Mutex;
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
/// The lane code of a refusal because another service operation holds SpaceTerm Core.
pub(crate) const BUSY: &str = "service_busy";

/// An admitted service-lane connection, the lane version its admission selected, and the
/// service version the Core offered.
pub(crate) type Connected = (Box<dyn Preamble + Send>, ProtocolVersion, ProtocolVersion);

/// Opens and admits a fresh service-lane connection.
pub(crate) trait Connector: Send + Sync {
    fn connect(&self, deadline: Instant) -> Result<Connected, CoreError>;
}

/// Why a lane request did not produce the reply it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneError {
    /// The server answered `Error { code }`, a code from the CLI output contract.
    Code(String),
    /// The server refused a version whose Update Floor is above its running version, and named
    /// that floor: the CLI output contract's `below_floor`.
    BelowFloor(crate::update::ordering::Floor),
    /// The connection failed, or the reply was not the one requested.
    Core(CoreError),
    /// The caller was interrupted before `Execute` was sent, so nothing was dispatched.
    Interrupted,
    /// The caller's deadline passed before the request completed. When it was an `Execute`,
    /// it passed before `Execute` was sent, so nothing was dispatched.
    Deadline,
}

impl From<CoreError> for LaneError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

/// One admitted lane connection.
pub struct Session {
    /// `None` once a request was abandoned at its deadline, which leaves the connection
    /// mid-frame.
    stream: Option<Box<dyn Preamble + Send>>,
    /// The lane version admission selected. No request it lacks is sent.
    selected: ProtocolVersion,
    /// The service version the Core offered at admission.
    core: ProtocolVersion,
    /// When the last request was sent, or the connection opened.
    used: Instant,
}

impl Session {
    pub(crate) fn open(connector: &dyn Connector, deadline: Instant) -> Result<Self, CoreError> {
        let (stream, selected, core) = connector.connect(deadline)?;
        Ok(Self {
            stream: Some(stream),
            selected,
            core,
            used: Instant::now(),
        })
    }

    /// The service lane version this session's admission selected.
    pub fn selected(&self) -> ProtocolVersion {
        self.selected
    }

    /// `request` as a frame, or `Unsupported` when the selected version lacks it.
    fn frame(&self, request: &LaneRequest) -> Result<Vec<u8>, CoreError> {
        if request.since() > self.selected {
            return Err(CoreError::Unsupported);
        }
        frame(request)
    }

    /// Like [`Self::open`], but stops waiting as soon as `deadline()` passes.
    fn open_within(
        connector: &Arc<dyn Connector>,
        deadline: &dyn Fn() -> Instant,
    ) -> Result<Self, CoreError> {
        let connector = Arc::clone(connector);
        let fixed = deadline();
        let (stream, selected, core) = within(LANE_IO, &|| Some(deadline()), move || {
            connector.connect(fixed)
        })
        .ok_or(CoreError::Protocol)??;
        Ok(Self {
            stream: Some(stream),
            selected,
            core,
            used: Instant::now(),
        })
    }

    /// Sends one request and reads its reply. A lane `Error` is returned as a reply.
    pub fn request(
        &mut self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<LaneReply, CoreError> {
        let bytes = self.frame(request)?;
        let stream = self.stream.as_mut().ok_or(CoreError::Protocol)?;
        self.used = Instant::now();
        stream.send(&bytes, deadline)?;
        let reply = stream.receive(MAX_FRAME, deadline)?;
        self.decode(&reply)
    }

    /// Reads one reply. A reply this client can't read from a Core whose lane minor is newer
    /// than this client's means this client is the older side, which the person updates; any
    /// other unreadable reply breaks the protocol.
    fn decode(&self, reply: &[u8]) -> Result<LaneReply, CoreError> {
        serde_json::from_slice(reply).map_err(|_| {
            let ours = crate::protocol::version::SERVICE;
            if self.core.major == ours.major && self.core.minor > ours.minor {
                CoreError::IncompatibleProtocol(
                    crate::protocol::version::Incompatible::CallerTooOld,
                )
            } else {
                CoreError::Protocol
            }
        })
    }

    /// Like [`Self::call`], but stops waiting as soon as `deadline()` passes. A request
    /// abandoned that way ends this session.
    pub fn call_within(
        &mut self,
        request: &LaneRequest,
        deadline: &dyn Fn() -> Instant,
    ) -> Result<LaneReply, LaneError> {
        match self.request_within(request, deadline)? {
            LaneReply::Error { code } => Err(LaneError::Code(code)),
            LaneReply::BelowFloor { floor } => Err(LaneError::BelowFloor(floor)),
            reply => Ok(reply),
        }
    }

    /// Like [`Self::request`], but stops waiting as soon as `deadline()` passes. A request
    /// abandoned that way ends this session.
    fn request_within(
        &mut self,
        request: &LaneRequest,
        deadline: &dyn Fn() -> Instant,
    ) -> Result<LaneReply, CoreError> {
        self.send_within(request, deadline).0
    }

    /// [`Self::request_within`] that also says whether any byte of `request` may have reached
    /// the transport. Nothing is written once the deadline fixed when it starts has passed,
    /// even by I/O the caller stopped waiting for.
    fn send_within(
        &mut self,
        request: &LaneRequest,
        deadline: &dyn Fn() -> Instant,
    ) -> (Result<LaneReply, CoreError>, bool) {
        let bytes = match self.frame(request) {
            Ok(bytes) => bytes,
            Err(error) => return (Err(error), false),
        };
        let fixed = deadline();
        if Instant::now() >= fixed {
            return (Err(CoreError::Protocol), false);
        }
        let Some(mut stream) = self.stream.take() else {
            return (Err(CoreError::Protocol), false);
        };
        self.used = Instant::now();
        let progress = Arc::new(Progress::default());
        let sending = Arc::clone(&progress);
        let finished = within(LANE_IO, &|| Some(deadline()), move || {
            // Marked before the clock is read, so a caller that saw the deadline pass and then
            // found this unmarked knows the check below refuses to write.
            sending.mark(Progress::STARTED);
            if Instant::now() >= fixed {
                sending.mark(Progress::UNSENT);
                return (stream, Err(CoreError::Protocol), false);
            }
            // The transport checks the deadline again before each write, and says whether any
            // byte left before it passed.
            match stream.send_frame(&bytes, fixed) {
                Ok(()) => {
                    sending.mark(Progress::SENT);
                    let reply = stream.receive(MAX_FRAME, fixed);
                    (stream, reply, true)
                }
                Err(unsent) if !unsent.written => {
                    sending.mark(Progress::UNSENT);
                    (stream, Err(unsent.error), false)
                }
                Err(unsent) => {
                    sending.mark(Progress::SENT);
                    (stream, Err(unsent.error), true)
                }
            }
        });
        let Some((stream, reply, wrote)) = finished else {
            // The caller stopped waiting while the I/O may still run.
            return (Err(CoreError::Protocol), progress.may_have_written(fixed));
        };
        self.stream = Some(stream);
        (reply.and_then(|reply| self.decode(&reply)), wrote)
    }

    /// Sends one request and turns a lane `Error` into [`LaneError::Code`], and a `BelowFloor`
    /// refusal into [`LaneError::BelowFloor`].
    pub fn call(
        &mut self,
        request: &LaneRequest,
        deadline: Instant,
    ) -> Result<LaneReply, LaneError> {
        match self.request(request, deadline)? {
            LaneReply::Error { code } => Err(LaneError::Code(code)),
            LaneReply::BelowFloor { floor } => Err(LaneError::BelowFloor(floor)),
            reply => Ok(reply),
        }
    }
}

/// How far lane I/O that [`Session::send_within`] runs got with its request's bytes.
#[derive(Default)]
struct Progress(std::sync::atomic::AtomicU8);

impl Progress {
    const STARTED: u8 = 1;
    /// It ended without writing a byte, and writes none later.
    const UNSENT: u8 = 2;
    /// A byte may have left.
    const SENT: u8 = 3;

    fn mark(&self, phase: u8) {
        self.0.store(phase, std::sync::atomic::Ordering::SeqCst);
    }

    fn phase(&self) -> u8 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether a byte may have left, said by a caller that stopped waiting. I/O that had not
    /// started by the deadline `fixed` never writes. I/O that was sending then stops at `fixed`
    /// too, before its next write, so the caller waits [`SEND_GRACE`] at most for it to say
    /// whether any byte left before that.
    fn may_have_written(&self, fixed: Instant) -> bool {
        if Instant::now() < fixed {
            // An interrupt ended the wait early: the I/O may still write until `fixed`.
            return self.phase() != Progress::UNSENT;
        }
        let grace = Instant::now() + SEND_GRACE;
        loop {
            match self.phase() {
                0 => return false,
                Progress::UNSENT => return false,
                Progress::SENT => return true,
                _ if Instant::now() >= grace => return true,
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
}

/// How long a caller past its deadline waits for lane I/O that was sending to say whether any
/// byte left. The transport stops at the same deadline, so this covers only scheduling.
const SEND_GRACE: Duration = Duration::from_millis(250);

/// `error`, or [`LaneError::Deadline`] when it ended a wait that `deadline` bounded.
pub(crate) fn past(error: CoreError, deadline: Instant) -> LaneError {
    if Instant::now() >= deadline {
        LaneError::Deadline
    } else {
        LaneError::Core(error)
    }
}

fn frame(request: &LaneRequest) -> Result<Vec<u8>, CoreError> {
    let bytes = serde_json::to_vec(request).map_err(|_| CoreError::Protocol)?;
    if bytes.len() > MAX_FRAME {
        return Err(CoreError::Protocol);
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
    /// The server closed the lane after a Stop that was admitted, or may have been, or after a
    /// receipt showed an uninstall's helper launched. `boot` names the admitting run when the flow
    /// learned it. Completion is confirmed on the machine that runs the server.
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
///
/// `id` names the operation. The caller may execute the same approval again under the same ID
/// after a definite refusal, so a reply lost on an earlier attempt still settles there.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "one execution's parts, used once"
)]
pub(crate) fn perform(
    connector: &Arc<dyn Connector>,
    session: Session,
    action: LaneAction,
    token: String,
    id: OperationId,
    wait: Duration,
    admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    perform_following(
        connector,
        session,
        (action, token, id),
        Instant::now() + wait,
        admitted,
        &mut || {},
        &mut acknowledge_only,
        interrupted,
    )
}

/// Acknowledges a settled receipt on `session` for a caller that keeps no record of its
/// operations.
fn acknowledge_only(session: &mut Session, id: &OperationId, _: &LaneReceipt, by: Instant) {
    let _ = acknowledge(session, id, by);
}

/// What a flow does with its operation's settled receipt on the session that read it, by the
/// instant it passes. A caller that records its operations keeps the outcome before it
/// acknowledges the receipt.
pub type Settle<'a> = dyn FnMut(&mut Session, &OperationId, &LaneReceipt, Instant) + 'a;

/// [`perform`] that runs `accepted` once, as soon as the server is known to have admitted the
/// operation: its `Accepted` reply, or a receipt for it, and hands its settled receipt to
/// `settle` on the session that read it.
#[allow(
    clippy::too_many_arguments,
    reason = "one execution's parts, used once"
)]
fn perform_following(
    connector: &Arc<dyn Connector>,
    session: Session,
    (action, token, id): (LaneAction, String, OperationId),
    until: Instant,
    admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
    accepted: &mut dyn FnMut(),
    settle: &mut Settle<'_>,
    interrupted: &dyn Fn() -> Option<Instant>,
) -> Result<Outcome, LaneError> {
    // The caller's absolute instant, never recomputed from a remaining duration, so time spent
    // before this flow runs can't move it later.
    let deadline = || interrupted().into_iter().fold(until, Instant::min);
    // Fixed per request from when it is sent, and moved closer by an interrupt.
    let request_deadline = |sent: Instant| move || deadline().min(sent + REQUEST_LIMIT);
    // A caller whose deadline passed before `Execute` dispatches nothing.
    let mut session = if session.used.elapsed() >= STALE {
        Session::open_within(connector, &request_deadline(Instant::now()))
            .map_err(|error| past(error, deadline()))?
    } else {
        session
    };
    if Instant::now() >= deadline() {
        return Err(LaneError::Deadline);
    }
    admitted(&id)?;
    let operation = id.to_string();
    // Stop and Uninstall end the Core that answers the lane, so the flow does not reconnect once
    // the Core admitted them.
    let stop = matches!(action, LaneAction::Stop | LaneAction::Uninstall);
    let uninstall = action == LaneAction::Uninstall;
    let execute = LaneRequest::Execute {
        action,
        token,
        operation: operation.clone(),
    };
    let (mut boot, mut current) =
        match session.send_within(&execute, &request_deadline(Instant::now())) {
            (Ok(LaneReply::Accepted { boot, .. }), _) => {
                accepted();
                (Some(boot), Some(session))
            }
            // A refused Execute admitted nothing.
            (Ok(LaneReply::Error { code }), _) => return Err(LaneError::Code(code)),
            // No byte of Execute was written, so nothing was dispatched.
            (Err(error), false) => return Err(past(error, deadline())),
            // Execute was sent, so the operation may be durable: follow the same operation ID.
            (Ok(_) | Err(_), _) => (None, None),
        };
    let mut last = None;
    loop {
        if current.is_none() && (boot.is_none() || !stop) {
            current = Session::open_within(connector, &request_deadline(Instant::now())).ok();
        }
        match current.as_mut() {
            // Without the lane, a Stop that may have been admitted is reported as in progress.
            None if stop => return Ok(closed(uninstall, operation, boot, last)),
            None => {}
            // The first reply was lost. A repeated Execute returns the existing admission, or
            // admits the operation now if the first never arrived. An interrupted caller only
            // resolves, so nothing new is dispatched after the interrupt.
            Some(session) if boot.is_none() && interrupted().is_none() => {
                match session.request_within(&execute, &request_deadline(Instant::now())) {
                    Ok(LaneReply::Accepted { boot: admitted, .. }) => {
                        accepted();
                        boot = Some(admitted);
                        continue;
                    }
                    Ok(LaneReply::Error { code }) => {
                        match resolve(session, &operation, &request_deadline(Instant::now())) {
                            Ok(LaneReply::Receipt(receipt)) => {
                                accepted();
                                boot = Some(receipt.boot);
                            }
                            // Another operation holds the Core, and the first Execute may still
                            // wait behind it on the lost connection, where it can admit this
                            // same operation once that settles. Nothing proves it never will,
                            // so the flow asks again under the same ID.
                            Ok(LaneReply::Error { code: missing })
                                if missing == "not_found" && code == BUSY => {}
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
                        settle(session, &id, &receipt, request_deadline(Instant::now())());
                        let boot = boot.unwrap_or_else(|| receipt.boot.clone());
                        return Ok(Outcome::Settled { receipt, boot });
                    }
                    Ok(LaneReply::Receipt(receipt)) => {
                        if boot.is_none() {
                            accepted();
                            boot = Some(receipt.boot.clone());
                        }
                        last = Some(receipt);
                    }
                    // The operation may still arrive on the lost connection, so this proves nothing.
                    Ok(LaneReply::Error { code }) if code == "not_found" && boot.is_none() => {}
                    Ok(LaneReply::Error { code }) => return Err(LaneError::Code(code)),
                    Ok(_) => return Err(LaneError::Core(CoreError::Protocol)),
                    // The server closed the lane: it is stopping, or it already stopped.
                    Err(_) if stop => return Ok(closed(uninstall, operation, boot, last)),
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

/// How a Stop or an uninstall ends once its lane closed. A Core whose uninstall helper failed to
/// launch keeps running and records that, so a closed lane ends an uninstall only after a receipt
/// showed its helper launched. Before that, the operation stays to settle later.
fn closed(
    uninstall: bool,
    operation: String,
    boot: Option<String>,
    last: Option<LaneReceipt>,
) -> Outcome {
    let launched = last
        .as_ref()
        .is_some_and(|receipt| receipt.state != super::LaneOperationState::Admitted);
    let boot = boot.unwrap_or_default();
    if uninstall && !launched {
        Outcome::Unconfirmed {
            operation,
            boot,
            last,
        }
    } else {
        Outcome::Shutdown { operation, boot }
    }
}

fn resolve(
    session: &mut Session,
    operation: &str,
    deadline: &dyn Fn() -> Instant,
) -> Result<LaneReply, CoreError> {
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
    /// It settled.
    Settled(LaneReceipt),
    /// It has not settled yet. A later command asks again.
    Pending,
    /// The server no longer holds it, for example after its pairing was revoked.
    Gone,
}

/// What became of an operation an earlier command admitted, read by `deadline` without
/// acknowledging it.
pub(crate) fn leftover(
    session: &mut Session,
    operation: &OperationId,
    deadline: Instant,
) -> Result<Leftover, LaneError> {
    let reply = session
        .request_within(
            &LaneRequest::Resolve {
                operation: operation.to_string(),
            },
            &|| deadline,
        )
        .map_err(|error| past(error, deadline))?;
    match reply {
        LaneReply::Receipt(receipt) if settled(&receipt) => Ok(Leftover::Settled(receipt)),
        LaneReply::Receipt(_) => Ok(Leftover::Pending),
        LaneReply::Error { code } if code == "not_found" => Ok(Leftover::Gone),
        LaneReply::Error { code } => Err(LaneError::Code(code)),
        _ => Err(LaneError::Core(CoreError::Protocol)),
    }
}

/// Acknowledges a settled operation by `deadline`, which lets the server forget it.
pub(crate) fn acknowledge(
    session: &mut Session,
    operation: &OperationId,
    deadline: Instant,
) -> Result<(), LaneError> {
    session
        .call_within(
            &LaneRequest::Acknowledge {
                operation: operation.to_string(),
            },
            &|| deadline,
        )
        .map(drop)
        .map_err(|error| match error {
            LaneError::Core(error) => past(error, deadline),
            error => error,
        })
}

/// The service lane of this account's own server, over account IPC.
struct Local {
    paths: crate::host::Paths,
    core: [u8; 32],
    client: [u8; 32],
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
    admissions: Arc<Mutex<Admissions>>,
}

impl Connector for Local {
    fn connect(&self, deadline: Instant) -> Result<Connected, CoreError> {
        if Instant::now() >= deadline {
            return Err(CoreError::NotRunning);
        }
        let mut stream = crate::host::connect(&self.paths)?;
        let admitted = crate::protocol::admit_service_lane(
            &mut stream,
            crate::protocol::Caller {
                client: &self.client,
                name: &self.name,
                agent: self.agent.as_ref(),
            },
            &self.core,
            &self.paths.identity,
            deadline,
        );
        record(&self.admissions, &admitted);
        let admitted = admitted?;
        Ok((Box::new(stream), admitted.selected, admitted.core.service))
    }
}

/// Records a lane admission in the Machine's admissions.
fn record(
    admissions: &Mutex<Admissions>,
    admitted: &Result<crate::protocol::Negotiated, crate::protocol::Refused>,
) {
    admissions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .record(Admitted::Service, admitted);
}

/// The service lane of a paired server, over its authenticated direct connection.
struct Paired {
    connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
    paired: crate::direct::PairedDevice,
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
    admissions: Arc<Mutex<Admissions>>,
}

impl Connector for Paired {
    fn connect(&self, deadline: Instant) -> Result<Connected, CoreError> {
        if Instant::now() >= deadline {
            return Err(crate::direct::DirectError::Transport.into());
        }
        // Each dial follows the deadline of the lane request that needs it, never one the
        // connector fixed before the caller's approval began.
        let connection = self.connector.connect_by(deadline)?;
        if connection.paired() != &self.paired {
            return Err(CoreError::AuthenticationRefused);
        }
        let admitted = connection.admit_service_lane_by(
            crate::protocol::Caller {
                client: &self.paired.client,
                name: &self.name,
                agent: self.agent.as_ref(),
            },
            deadline,
        );
        record(
            &self.admissions,
            &admitted
                .as_ref()
                .map(|(_, negotiated)| negotiated.clone())
                .map_err(Clone::clone),
        );
        let (stream, negotiated) = admitted?;
        Ok((stream, negotiated.selected, negotiated.core.service))
    }
}

/// A route to one server's service lane. Opening it never starts a server.
#[derive(Clone)]
pub struct Lane {
    connector: Arc<dyn Connector>,
}

impl Lane {
    /// The lane of the account server at `paths`, as base identity `client` named `name`, or as
    /// `agent` when given.
    pub(crate) fn local(
        paths: crate::host::Paths,
        core: [u8; 32],
        client: [u8; 32],
        name: crate::pane::control::ClientName,
        agent: Option<crate::protocol::AgentName>,
        admissions: Arc<Mutex<Admissions>>,
    ) -> Self {
        Self {
            connector: Arc::new(Local {
                paths,
                core,
                client,
                name,
                agent,
                admissions,
            }),
        }
    }

    /// The lane of a paired server, as its pairing's Client Identity or as `agent`.
    pub(crate) fn paired(
        connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
        paired: crate::direct::PairedDevice,
        name: crate::pane::control::ClientName,
        agent: Option<crate::protocol::AgentName>,
        admissions: Arc<Mutex<Admissions>>,
    ) -> Self {
        Self {
            connector: Arc::new(Paired {
                connector,
                paired,
                name,
                agent,
                admissions,
            }),
        }
    }

    /// A lane over `connector`, for flows tested against a served fixture lane.
    #[cfg(test)]
    pub(crate) fn over(connector: Arc<dyn Connector>) -> Self {
        Self { connector }
    }

    /// Opens and admits one connection.
    pub fn session(&self) -> Result<Session, CoreError> {
        Session::open(self.connector.as_ref(), Instant::now() + REQUEST_LIMIT)
    }

    /// Opens and admits one connection by `deadline`, and stops waiting for a stalled server
    /// once it passes. A deadline that passed first is [`LaneError::Deadline`].
    pub fn session_by(&self, deadline: Instant) -> Result<Session, LaneError> {
        let opened = Instant::now();
        let by = || deadline.min(opened + REQUEST_LIMIT);
        Session::open_within(&self.connector, &by).map_err(|error| past(error, deadline))
    }

    /// Executes a previewed action as a new operation and follows it. See [`perform`].
    pub fn perform(
        &self,
        session: Session,
        action: LaneAction,
        token: String,
        wait: Duration,
        admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
        interrupted: &dyn Fn() -> Option<Instant>,
    ) -> Result<Outcome, LaneError> {
        let id = crate::service::operations::random_operation_id()?;
        self.perform_as(
            session,
            (action, token, id),
            Instant::now() + wait,
            admitted,
            (&mut || {}, &mut acknowledge_only),
            interrupted,
        )
    }

    /// Executes a previewed action as operation `id` and follows it until the absolute instant
    /// `until`. See [`perform`]. `accepted` runs once, as soon as the server is known to have
    /// admitted the operation, and `settle` takes its settled receipt on the session that read
    /// it.
    pub fn perform_as(
        &self,
        session: Session,
        (action, token, id): (LaneAction, String, OperationId),
        until: Instant,
        admitted: &mut dyn FnMut(&OperationId) -> Result<(), LaneError>,
        (accepted, settle): (&mut dyn FnMut(), &mut Settle<'_>),
        interrupted: &dyn Fn() -> Option<Instant>,
    ) -> Result<Outcome, LaneError> {
        perform_following(
            &self.connector,
            session,
            (action, token, id),
            until,
            admitted,
            accepted,
            settle,
            interrupted,
        )
    }
}
