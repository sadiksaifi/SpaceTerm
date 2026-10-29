//! `server start`, `stop` and `restart`.
//!
//! Start runs only on the server's machine and calls the service directly. Stop and Restart
//! preview their effects, take a confirmation, then run through the service lane's operation
//! flow, which survives the server's own stop.
use super::{Context, Opened, Pending, Reply, Routed};
use crate::contract::{
    self, Code, Confirmation, ConfirmationData, Envelope, Failure, Observed, Receipt, ReceiptState,
    RecoveredOperation, ServerRef, ServerStart, ServiceAction, ServiceEffects, ServiceOperation,
};
use spaceterm_server::service::Service;
use spaceterm_server::service::control::{Action as ControlAction, Controller, Wording};
use spaceterm_server::service::lane::client::Outcome;
use spaceterm_server::service::lane::{
    LaneAction, LaneEffects, LaneOperationState, LaneReceipt, LaneReply, LaneRequest,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stop,
    Restart,
}

impl Kind {
    fn command(self) -> &'static str {
        match self {
            Self::Stop => "server.stop",
            Self::Restart => "server.restart",
        }
    }
    fn action(self) -> LaneAction {
        match self {
            Self::Stop => LaneAction::Stop,
            Self::Restart => LaneAction::Restart,
        }
    }
    fn control(self) -> ControlAction {
        match self {
            Self::Stop => ControlAction::Stop,
            Self::Restart => ControlAction::Restart,
        }
    }
    /// Stop waits for the server to close the lane. Restart waits for it to return.
    fn wait(self) -> Duration {
        match self {
            Self::Stop => Duration::from_secs(90),
            Self::Restart => Duration::from_secs(180),
        }
    }
}

pub fn start(context: &Context<'_>) -> Reply {
    const COMMAND: &str = "server.start";
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    let server = super::local_ref(context.identity).ok();
    let started = Service::for_identity(context.identity)
        .and_then(|service| Controller::new(service).start())
        .map_err(contract::from_server_error);
    match started {
        Ok(status) => Reply::new(
            &Envelope::ok(
                COMMAND,
                server,
                ServerStart {
                    running: status.running,
                },
            ),
            "Server started.\n".to_owned(),
        ),
        Err(failure) => Reply::failure(COMMAND, server, failure),
    }
}

pub fn stop_or_restart(context: &Context<'_>, kind: Kind, token: Option<&str>) -> Reply {
    let command = kind.command();
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err(error) => return Reply::routing_failure(command, error),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(command, Some(routed.server), failure),
    };
    let opened = match super::open_lane(context, &routed.connection, &routed.route, &pending) {
        Ok(opened) => opened,
        Err(failure) => return not_running(context, &routed, command, failure),
    };
    match token {
        Some(token) => execute(context, kind, routed, opened, &pending, token.to_owned()),
        None => preview(context, kind, routed, opened, &pending),
    }
}

/// A lane that does not answer: a stopped local server says so, and Stop of a stopped server
/// changes nothing.
fn not_running(context: &Context<'_>, routed: &Routed, command: &str, failure: Failure) -> Reply {
    if routed.local
        && let Ok(Some(_)) =
            Service::for_identity(context.identity).and_then(|service| service.stop_if_stopped())
    {
        return Reply::failure(
            command,
            Some(routed.server.clone()),
            Failure::new(Code::Stopped),
        );
    }
    let failure = match failure.code {
        Code::Unreachable if routed.local => Failure::new(Code::NotRunning),
        _ => failure,
    };
    Reply::failure(command, Some(routed.server.clone()), failure)
}

fn preview(
    context: &Context<'_>,
    kind: Kind,
    routed: Routed,
    mut opened: Opened,
    pending: &Pending,
) -> Reply {
    let command = kind.command();
    let reply = opened.session.request(
        &LaneRequest::Preview {
            action: kind.action(),
        },
        Instant::now() + Duration::from_secs(30),
    );
    let (token, expires_at, effects) = match reply {
        Ok(LaneReply::Preview {
            token,
            expires_at,
            effects,
        }) => (token, expires_at, effects),
        Ok(LaneReply::Error { code }) => {
            return Reply::failure(
                command,
                Some(routed.server),
                contract::from_lane_code(&code),
            );
        }
        Ok(_) => {
            return Reply::failure(
                command,
                Some(routed.server),
                Failure::new(Code::ProtocolError),
            );
        }
        Err(error) => {
            return Reply::failure(
                command,
                Some(routed.server),
                contract::from_server_error(error),
            );
        }
    };
    let wording = Wording::for_server(
        &routed.server.name,
        kind.control(),
        effects.ends_sessions > 0,
        spaceterm_server::service::ServiceStartup::AtLogin,
    );
    let summary = format!("{}\n{}\n", wording.message(), wording.detail());
    if context.interactive() {
        eprint!("{summary}");
        if super::confirm(wording.confirm_label()) {
            return execute(context, kind, routed, opened, pending, token);
        }
        return Reply::failure(
            command,
            Some(routed.server),
            Failure::new(Code::ConfirmationRequired),
        );
    }
    let text = format!(
        "{summary}Confirm with: spaceterm {} --confirm {token}\n",
        command.replace('.', " ")
    );
    Reply::new(
        &preview_envelope(
            command,
            routed.server,
            token,
            expires_at,
            &effects,
            opened.recovered,
        ),
        text,
    )
}

/// The confirmation envelope of a Stop or Restart preview.
pub(crate) fn preview_envelope(
    command: &str,
    server: ServerRef,
    token: String,
    expires_at: String,
    effects: &LaneEffects,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<ConfirmationData<ServiceEffects>> {
    super::with_recovered(
        Envelope::confirmation(
            command,
            Some(server),
            Confirmation {
                token,
                expires_at,
                effects: service_effects(effects),
            },
        ),
        recovered,
    )
}

pub fn service_effects(effects: &LaneEffects) -> ServiceEffects {
    ServiceEffects {
        ends_sessions: effects.ends_sessions,
        sessions: effects.sessions.clone(),
        start_requires_machine: effects.start_requires_machine,
    }
}

fn execute(
    context: &Context<'_>,
    kind: Kind,
    routed: Routed,
    opened: Opened,
    pending: &Pending,
    token: String,
) -> Reply {
    let command = kind.command();
    let Opened {
        lane,
        session,
        recovered,
    } = opened;
    let outcome = lane.perform(
        session,
        kind.action(),
        token,
        context.timeout(kind.wait()),
        &mut |operation| pending.add(operation),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Reply::new(
                &super::with_recovered(
                    Envelope::error(
                        command,
                        Some(routed.server),
                        super::lane_failure(error),
                        contract::Empty {},
                    ),
                    recovered,
                ),
                String::new(),
            );
        }
    };
    let action = match kind {
        Kind::Stop => ServiceAction::Stop,
        Kind::Restart => ServiceAction::Restart,
    };
    let settled = settle(action, outcome);
    if settled.acknowledged {
        let _ = pending.remove(&settled.receipt.operation);
    }
    let text = match (&settled.observed, settled.failure.as_ref()) {
        (Some(Observed::Shutdown), _) => format!(
            "Stop is in progress. Confirm on {} with: spaceterm server status\n",
            routed.server.name
        ),
        (_, None) if kind == Kind::Restart => "Server restarted.\n".to_owned(),
        (_, None) => "Server stopped.\n".to_owned(),
        (_, Some(_)) => String::new(),
    };
    Reply::new(
        &operation_envelope(command, routed.server, settled, recovered),
        text,
    )
}

/// The envelope of a confirmed Stop or Restart, from its settled flow.
pub(crate) fn operation_envelope(
    command: &str,
    server: ServerRef,
    settled: Settled,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<ServiceOperation> {
    let data = ServiceOperation {
        operation: settled.receipt.operation.clone(),
        receipt: settled.receipt,
        observed: settled.observed,
    };
    let envelope = match settled.failure {
        None => Envelope::ok(command, Some(server), data),
        Some(failure) => Envelope::error(command, Some(server), failure, data),
    };
    super::with_recovered(envelope, recovered)
}

/// A finished operation flow, as the contract reports it.
pub struct Settled {
    pub receipt: Receipt,
    pub observed: Option<Observed>,
    pub failure: Option<Failure>,
    /// The flow acknowledged the receipt, so the operation is no longer pending.
    pub acknowledged: bool,
}

/// Maps the flow's outcome to a receipt and a failure.
pub fn settle(action: ServiceAction, outcome: Outcome) -> Settled {
    match outcome {
        Outcome::Shutdown { operation, boot } => Settled {
            receipt: Receipt {
                operation,
                action,
                state: ReceiptState::Launched,
                code: None,
                boot,
            },
            observed: Some(Observed::Shutdown),
            failure: Some(Failure::new(Code::OutcomeUnconfirmed)),
            acknowledged: false,
        },
        Outcome::Settled { receipt, boot } => {
            let observed = (receipt.boot != boot).then_some(Observed::Restarted);
            let failure = match receipt.state {
                LaneOperationState::Done => None,
                LaneOperationState::Failed => Some(
                    receipt
                        .code
                        .as_deref()
                        .map(contract::from_lane_code)
                        .unwrap_or(Failure::new(Code::OperationFailed)),
                ),
                LaneOperationState::NotPerformed => Some(Failure::new(Code::OperationFailed)),
                _ => Some(unconfirmed(&receipt)),
            };
            Settled {
                receipt: receipt_of(&receipt),
                observed,
                failure,
                acknowledged: true,
            }
        }
        Outcome::Unconfirmed {
            operation,
            boot,
            last,
        } => Settled {
            receipt: last.as_ref().map(receipt_of).unwrap_or(Receipt {
                operation,
                action,
                state: ReceiptState::Launched,
                code: None,
                boot,
            }),
            observed: None,
            failure: Some(Failure::new(Code::OutcomeUnconfirmed)),
            acknowledged: false,
        },
    }
}

fn unconfirmed(receipt: &LaneReceipt) -> Failure {
    match receipt.action {
        LaneAction::Update { .. } | LaneAction::Return => Failure::new(Code::ActivationUnresolved),
        LaneAction::Stop | LaneAction::Restart => Failure::new(Code::OutcomeUnconfirmed),
    }
}

pub fn receipt_of(receipt: &LaneReceipt) -> Receipt {
    Receipt {
        operation: receipt.operation.clone(),
        action: match receipt.action {
            LaneAction::Stop => ServiceAction::Stop,
            LaneAction::Restart => ServiceAction::Restart,
            LaneAction::Update { .. } => ServiceAction::Update,
            LaneAction::Return => ServiceAction::Return,
        },
        state: match receipt.state {
            LaneOperationState::Admitted => ReceiptState::Admitted,
            LaneOperationState::Launched => ReceiptState::Launched,
            LaneOperationState::Running => ReceiptState::Running,
            LaneOperationState::Done => ReceiptState::Done,
            LaneOperationState::Failed => ReceiptState::Failed,
            LaneOperationState::NotPerformed => ReceiptState::NotPerformed,
            LaneOperationState::Unresolved => ReceiptState::Unresolved,
        },
        code: receipt
            .code
            .as_deref()
            .map(|code| contract::from_lane_code(code).code),
        boot: receipt.boot.clone(),
    }
}
