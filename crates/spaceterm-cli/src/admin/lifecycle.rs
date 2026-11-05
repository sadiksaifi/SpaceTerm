//! `start`, `stop` and `restart`.
//!
//! Start and Stop run only on this machine; Restart also reaches a paired machine. Start calls
//! the service directly. Stop and Restart preview their effects, take a confirmation, then run
//! through the service lane's operation flow, which survives SpaceTerm Core's own stop.
use super::{Context, Opened, Pending, Reply, Routed};
use crate::contract::{
    self, Code, Confirmation, ConfirmationData, CoreStart, Envelope, Failure, MachineRef, Observed,
    Receipt, ReceiptState, RecoveredOperation, RestartTarget, ServiceAction, ServiceEffects,
    ServiceOperation, Warning, WarningCode, WarningDetails,
};
use spaceterm_core::service::Service;
use spaceterm_core::service::control::Controller;
use spaceterm_core::service::lane::admin::{self, Verdict};
use spaceterm_core::service::lane::client::Outcome;
use spaceterm_core::service::lane::{LaneAction, LaneEffects, LaneOperationState, LaneReceipt};
use spaceterm_core::update::facts::UpdateFacts;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stop,
    Restart,
}

impl Kind {
    fn command(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
    fn action(self) -> LaneAction {
        match self {
            Self::Stop => LaneAction::Stop,
            Self::Restart => LaneAction::Restart,
        }
    }
    fn confirm_label(self) -> &'static str {
        match self {
            Self::Stop => "Stop SpaceTerm Core",
            Self::Restart => "Restart SpaceTerm Core",
        }
    }
    /// Stop waits for SpaceTerm Core to close the lane. Restart waits for it to return.
    fn wait(self) -> Duration {
        admin::wait(&self.action())
    }
}

pub fn start(context: &Context<'_>) -> Reply {
    const COMMAND: &str = "start";
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    let core = super::local_ref(context.identity).ok();
    if let Err(failure) = crate::interrupt::dispatch() {
        return Reply::failure(COMMAND, core, failure);
    }
    let started = Service::for_identity(context.identity)
        .and_then(|service| Controller::new(service).start())
        .map_err(contract::from_core_error);
    match started {
        Ok(status) => Reply::new(
            &Envelope::ok(
                COMMAND,
                core,
                CoreStart {
                    running: status.running,
                },
            ),
            "SpaceTerm Core started.\n".to_owned(),
        ),
        Err(failure) => Reply::failure(COMMAND, core, failure),
    }
}

pub fn stop_or_restart(context: &Context<'_>, kind: Kind, token: Option<&str>) -> Reply {
    let command = kind.command();
    // A stopped SpaceTerm Core can't be started from a paired machine, so Stop runs only here.
    if kind == Kind::Stop
        && let Err(failure) = context.require_local()
    {
        return Reply::failure(command, None, failure);
    }
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err(error) => return Reply::routing_failure(command, error),
    };
    let pending = match Pending::open(context, &routed.route) {
        Ok(pending) => pending,
        Err(failure) => return Reply::failure(command, Some(routed.machine), failure),
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

/// A lane that does not answer: a stopped SpaceTerm Core says so, and Stop of a stopped one
/// changes nothing.
fn not_running(context: &Context<'_>, routed: &Routed, command: &str, failure: Failure) -> Reply {
    if routed.local
        && let Ok(Some(_)) =
            Service::for_identity(context.identity).and_then(|service| service.stop_if_stopped())
    {
        return Reply::failure(
            command,
            Some(routed.machine.clone()),
            Failure::new(Code::Stopped),
        );
    }
    let failure = match failure.code {
        Code::Unreachable if routed.local => Failure::new(Code::NotRunning),
        _ => failure,
    };
    Reply::failure(command, Some(routed.machine.clone()), failure)
}

fn preview(
    context: &Context<'_>,
    kind: Kind,
    routed: Routed,
    mut opened: Opened,
    pending: &Pending,
) -> Reply {
    let command = kind.command();
    // A Core that reports no facts still previews; its restart then names no breaking change.
    let facts = match kind {
        Kind::Restart => admin::facts(&mut opened.session)
            .ok()
            .map(|(_, facts)| facts),
        Kind::Stop => None,
    };
    let previewed = admin::preview(&mut opened.session, kind.action());
    let admin::Preview {
        token,
        expires_at,
        effects,
        ..
    } = match previewed {
        Ok(preview) => preview,
        Err(error) => {
            return Reply::failure(command, Some(routed.machine), super::lane_failure(error));
        }
    };
    let previewed = service_effects(&effects, facts.as_ref());
    let summary = summary(kind, super::display_name(&routed.machine), &previewed);
    if context.interactive() {
        let warnings: Vec<_> = major_warning(previewed.to_major)
            .into_iter()
            .chain(super::recovered_warning(&opened.recovered))
            .collect();
        contract::write_warnings(&warnings, &mut std::io::stderr().lock());
        eprint!("{summary}");
        if super::confirm(kind.confirm_label()) {
            return execute(context, kind, routed, opened, pending, token);
        }
        return Reply::failure(
            command,
            Some(routed.machine),
            Failure::new(Code::ConfirmationRequired),
        );
    }
    let text = format!(
        "{summary}Confirm with: {}\n",
        context.command_line(
            &routed.machine,
            &[
                command.split('.').collect::<Vec<_>>(),
                vec!["--confirm", &token]
            ]
            .concat()
        )
    );
    Reply::new(
        &preview_envelope(
            command,
            routed.machine,
            token,
            expires_at,
            previewed,
            opened.recovered,
        ),
        text,
    )
}

/// What a preview shows: the effects for the envelope, and the protocol major the restart
/// moves to, for the warning.
pub(crate) struct Previewed {
    pub(crate) effects: ServiceEffects,
    pub(crate) to_major: Option<u16>,
}

/// The preview text: the action, the machine it acts on, the Pending Version a restart moves
/// to and whether that breaks devices, and what ends and what is kept.
pub(crate) fn summary(kind: Kind, machine: &str, previewed: &Previewed) -> String {
    const ENDS: &str = "All Terminal Sessions on it end for every client.";
    match kind {
        Kind::Stop => format!(
            "Stop SpaceTerm Core on {machine}?\n{ENDS} Workspaces are kept, and SpaceTerm Core stays stopped until you start it again with: spaceterm start\n"
        ),
        Kind::Restart => {
            let Some(target) = &previewed.effects.pending else {
                return format!(
                    "Restart SpaceTerm Core on {machine}?\n{ENDS} Workspaces are kept, and each Pane starts a new shell when you use it.\n"
                );
            };
            let mut text = format!(
                "Restart SpaceTerm Core on {machine} from {} into {}?\n{ENDS} Workspaces are kept, and each Pane starts a new shell when you use it.\n",
                target.from, target.to
            );
            if target.breaking {
                if target.stranded.is_empty() {
                    text.push_str(&format!(
                        "{} changes the protocol. Older clients can\u{2019}t connect until they update.\n",
                        target.to
                    ));
                } else {
                    let names: Vec<&str> =
                        target.stranded.iter().map(|v| v.name.as_str()).collect();
                    text.push_str(&format!(
                        "{} changes the protocol. These devices can\u{2019}t connect until they update: {}\n",
                        target.to,
                        names.join(", ")
                    ));
                }
            }
            text
        }
    }
}

/// After a restart into another work protocol major, work commands need a matching CLI.
pub(crate) fn major_warning(to_major: Option<u16>) -> Option<Warning> {
    let core_major = to_major?;
    let client_major = spaceterm_core::protocol::MAJOR;
    (core_major != client_major).then(|| {
        Warning::new(
            WarningCode::ClientMajorDiffers,
            WarningDetails::Major {
                client_major,
                core_major,
            },
        )
    })
}

/// The confirmation envelope of a Stop or Restart preview.
pub(crate) fn preview_envelope(
    command: &str,
    core: MachineRef,
    token: String,
    expires_at: String,
    previewed: Previewed,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<ConfirmationData<ServiceEffects>> {
    let mut envelope = Envelope::confirmation(
        command,
        Some(core),
        Confirmation {
            token,
            expires_at,
            effects: previewed.effects,
        },
    );
    envelope.warnings.extend(major_warning(previewed.to_major));
    super::with_recovered(envelope, recovered)
}

/// The effects of a lane preview. A restart into a Pending Version names both versions, and
/// `facts` says whether the Pending Version is breaking and which Paired Devices it strands.
pub(crate) fn service_effects(effects: &LaneEffects, facts: Option<&UpdateFacts>) -> Previewed {
    let pending = match (&effects.from, &effects.to) {
        (Some(from), Some(to)) => {
            let waiting = facts
                .and_then(|facts| facts.pending.as_ref())
                .filter(|pending| pending.version.as_str() == to.version);
            Some(RestartTarget {
                from: from.version.clone(),
                to: to.version.clone(),
                breaking: waiting.is_some_and(|pending| pending.breaking),
                stranded: waiting
                    .map(|pending| {
                        pending
                            .stranded
                            .iter()
                            .map(|device| crate::contract::DeviceRef {
                                id: super::hex(&device.pairing),
                                name: device.name.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        }
        _ => None,
    };
    Previewed {
        effects: ServiceEffects {
            ends_sessions: effects.ends_sessions,
            sessions: effects.sessions.clone(),
            pending,
        },
        to_major: effects.to_major,
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
    let deadline = Instant::now() + context.timeout(kind.wait());
    let outcome = admin::execute(
        &lane,
        session,
        kind.action(),
        token,
        context.timeout(kind.wait()),
        pending.operations(),
        &mut *context.state.borrow_mut(),
        &mut super::dispatch,
        &crate::interrupt::settle_by,
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Reply::new(
                &super::with_recovered(
                    Envelope::error(
                        command,
                        Some(routed.machine),
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
    let mut settled = settle(action, outcome);
    if routed.local && settled.observed == Some(Observed::Shutdown) {
        settled = confirm_local_stop(context, settled, deadline);
    }
    let text = match (&settled.observed, settled.failure.as_ref()) {
        (Some(Observed::Shutdown), None) => "SpaceTerm Core stopped.\n".to_owned(),
        (Some(Observed::Shutdown), _) => {
            "Stop is in progress. Check it with: spaceterm status\n".to_owned()
        }
        (_, None) if kind == Kind::Restart => "SpaceTerm Core restarted.\n".to_owned(),
        (_, None) => "SpaceTerm Core stopped.\n".to_owned(),
        (_, Some(_)) => String::new(),
    };
    Reply::new(
        &operation_envelope(command, routed.machine, settled, recovered),
        text,
    )
}

/// The envelope of a confirmed Stop or Restart, from its settled flow.
pub(crate) fn operation_envelope(
    command: &str,
    core: MachineRef,
    settled: Settled,
    recovered: Vec<RecoveredOperation>,
) -> Envelope<ServiceOperation> {
    let data = ServiceOperation {
        operation: settled.receipt.operation.clone(),
        receipt: settled.receipt,
        observed: settled.observed,
    };
    let envelope = match settled.failure {
        None => Envelope::ok(command, Some(core), data),
        Some(failure) => Envelope::error(command, Some(core), failure, data),
    };
    super::with_recovered(envelope, recovered)
}

/// On this machine a Stop's outcome is readable without the lane: the helper records it in the
/// ledger once SpaceTerm Core has exited. The record stays for the next lane connection to
/// acknowledge, because only SpaceTerm Core acknowledges ledger records.
///
/// It follows the operation's own `deadline`, and an interrupt moves that to the settlement
/// instant. A held ledger lock never blocks it: the read is tried again until then.
fn confirm_local_stop(context: &Context<'_>, settled: Settled, deadline: Instant) -> Settled {
    use spaceterm_core::service::operations::{Ledger, OperationId, State};
    let Some(id) = OperationId::parse(&settled.receipt.operation) else {
        return settled;
    };
    loop {
        let state = Ledger::open(context.identity)
            .and_then(|ledger| ledger.resolve_now(&id))
            .ok()
            .flatten()
            .map(|record| record.state);
        let (receipt_state, failure) = match state {
            Some(State::Done) => (ReceiptState::Done, None),
            Some(State::Failed { code }) => {
                (ReceiptState::Failed, Some(contract::from_lane_code(&code)))
            }
            Some(State::NotPerformed(_)) => (
                ReceiptState::NotPerformed,
                Some(Failure::new(Code::OperationFailed)),
            ),
            Some(State::Unresolved) => (
                ReceiptState::Unresolved,
                Some(Failure::new(Code::OutcomeUnconfirmed)),
            ),
            _ => match crate::interrupt::limit(deadline).checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => {
                    std::thread::sleep(left.min(Duration::from_millis(250)));
                    continue;
                }
                _ => return settled,
            },
        };
        let mut receipt = settled.receipt;
        receipt.code = failure.as_ref().map(|v| v.code);
        receipt.state = receipt_state;
        return Settled {
            receipt,
            observed: settled.observed,
            failure,
            acknowledged: false,
        };
    }
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
            let failure = match admin::receipt_verdict(&receipt) {
                Verdict::Done => None,
                Verdict::Failed(code) => Some(
                    code.as_deref()
                        .map(contract::from_lane_code)
                        .unwrap_or(Failure::new(Code::OperationFailed)),
                ),
                Verdict::NotPerformed => Some(Failure::new(Code::OperationFailed)),
                // A settled receipt is never a shutdown, which only a lost lane reports.
                Verdict::Uncertain | Verdict::Shutdown => Some(unconfirmed(&receipt)),
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
        LaneAction::Stop
        | LaneAction::Restart
        | LaneAction::Network { .. }
        | LaneAction::Uninstall => Failure::new(Code::OutcomeUnconfirmed),
    }
}

pub fn receipt_of(receipt: &LaneReceipt) -> Receipt {
    Receipt {
        operation: receipt.operation.clone(),
        action: match receipt.action {
            LaneAction::Stop => ServiceAction::Stop,
            LaneAction::Restart => ServiceAction::Restart,
            LaneAction::Network { .. } => ServiceAction::Network,
            LaneAction::Uninstall => ServiceAction::Uninstall,
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
