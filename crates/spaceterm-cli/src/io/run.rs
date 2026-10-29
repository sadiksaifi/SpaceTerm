//! `pane run`: run one command at the prompt and bind its exit status to it.
//!
//! The command is one verbatim string in the shell's own language. The server writes it as one
//! bracketed paste at an empty prompt of the owning shell, and binds the first command start and
//! the finish after it. The run is recorded in this identity's `runs.json` before dispatch, so a
//! lost reply is resolved with `RunStatus`, never by sending the command again. An exit status
//! of 0 is never inferred.
use super::channel::{self, Channel, Claimed, Event, pane_size};
use super::read::{self, Live, Mode};
use super::{Deadline, Done, PaneTarget, controlled, hex, request, session_failure};
use crate::args::RunArgs;
use crate::contract::{
    Code, CommandFailedDetails, ControlledReason, Coverage, Details, Failure, Format, PaneRun, Run,
    RunDoubtName, RunStateName, RunUnconfirmedDetails, SessionStateName, UnsupportedSessionDetails,
    UnsupportedSessionReason, UsageDetails, UsageKind,
};
use crate::state::{PendingRun, Runs};
use spaceterm_server::connection::link::ServerLink;
use spaceterm_server::pane::{AttachmentClaim, Lease};
use spaceterm_server::protocol::{
    HistoryPosition, MAX_RUN_COMMAND_BYTES, Operation, PaneEvent, ReplyBody, RunDoubt, RunId,
    RunReceipt, RunRefusal, RunReply, RunState,
};
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Claiming the Pane gives up after this long; the run itself waits as long as it takes.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(30);
/// How long output waits for the frame that shows the finish.
const SCREEN_SETTLE: Duration = Duration::from_secs(1);
/// How often a lost reply is resolved again.
const RESOLVE_INTERVAL: Duration = Duration::from_millis(500);

fn usage(argument: &str) -> Failure {
    Failure::with(
        Code::InvalidArguments,
        Details::Usage(UsageDetails {
            argument: Some(argument.into()),
            kind: UsageKind::InvalidValue,
        }),
    )
}

/// `COMMAND`, or with `-` one line of stdin. It is refused when empty, longer than 16 KiB, or
/// holding a control character.
pub fn command_line(command: &str, stdin: &mut dyn BufRead) -> Result<String, Failure> {
    let command = if command == "-" {
        let mut line = Vec::new();
        stdin
            .take(MAX_RUN_COMMAND_BYTES as u64 + 2)
            .read_until(b'\n', &mut line)
            .map_err(|_| usage("<COMMAND>"))?;
        let line = line
            .strip_suffix(b"\n")
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            .unwrap_or(&line);
        String::from_utf8(line.to_vec()).map_err(|_| usage("<COMMAND>"))?
    } else {
        command.to_owned()
    };
    if command.is_empty()
        || command.len() > MAX_RUN_COMMAND_BYTES
        || command.chars().any(char::is_control)
    {
        return Err(usage("<COMMAND>"));
    }
    Ok(command)
}

/// The claim a run holds: its channel and lease, and what it did to the Pane.
struct Claim {
    channel: Channel,
    lease: Lease,
    started_shell: bool,
    repaired: bool,
}

/// Claims the target for a run. Only a run starts a shell: on a restored Pane at first use, and
/// as Start Session on a Pane whose Session ended. `directory` repairs a Pane whose directory is
/// unavailable.
fn claim(
    link: &ServerLink,
    target: &PaneTarget,
    args: &RunArgs,
    directory: Option<PathBuf>,
    deadline: Deadline,
) -> Result<Claim, Failure> {
    let size = args.size.map(pane_size);
    let channel = Channel::open(link)?;
    let select = AttachmentClaim::Select { explicit: true };
    let second = |claim: AttachmentClaim| -> Result<(Channel, Lease), Failure> {
        // A claim that did not attach closed its channel; the next one uses a new channel.
        let channel = Channel::open(link)?;
        match channel.claim(target, claim, size, deadline)? {
            Claimed::Attached(lease) => Ok((channel, lease)),
            Claimed::Takeover(_) => Err(controlled(ControlledReason::Other)),
            Claimed::Restored(restored) if restored.directory_unavailable => {
                Err(Failure::new(Code::DirectoryUnavailable))
            }
            Claimed::Restored(_) => Err(session_failure(
                Code::SessionNotStarted,
                SessionStateName::NotStarted,
            )),
        }
    };
    match channel.claim(target, select, size, deadline)? {
        Claimed::Attached(lease) => {
            if directory.is_some() {
                return Err(Failure::new(Code::RepairNotNeeded));
            }
            Ok(Claim {
                channel,
                lease,
                started_shell: !target.running,
                repaired: false,
            })
        }
        Claimed::Takeover(takeover)
            if takeover.session_ended.is_some()
                || args.take_over
                || channel::own_grant(&takeover) =>
        {
            let (channel, lease) = second(AttachmentClaim::TakeOver {
                target: takeover.target,
            })?;
            Ok(Claim {
                channel,
                lease,
                started_shell: takeover.session_ended.is_some(),
                repaired: false,
            })
        }
        Claimed::Takeover(_) => Err(controlled(ControlledReason::Other)),
        Claimed::Restored(restored) => match (restored.recovery, directory) {
            (Some(recovery), Some(directory)) if restored.directory_unavailable => {
                let (channel, lease) = second(AttachmentClaim::Repair {
                    target: recovery,
                    replacement: Some(directory),
                })?;
                Ok(Claim {
                    channel,
                    lease,
                    started_shell: true,
                    repaired: true,
                })
            }
            _ if restored.directory_unavailable => Err(Failure::new(Code::DirectoryUnavailable)),
            (_, Some(_)) => Err(Failure::new(Code::RepairNotNeeded)),
            _ => Err(session_failure(
                Code::SessionNotStarted,
                SessionStateName::NotStarted,
            )),
        },
    }
}

/// 128 random bits from the system.
fn run_id() -> Result<RunId, Failure> {
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(|_| Failure::new(Code::StorageUnavailable))?;
    Ok(RunId(bytes))
}

fn refusal(refusal: RunRefusal) -> Failure {
    let unsupported = |reason| {
        Failure::with(
            Code::UnsupportedSession,
            Details::UnsupportedSession(UnsupportedSessionDetails { reason }),
        )
    };
    match refusal {
        RunRefusal::NotAtPrompt | RunRefusal::InputPending | RunRefusal::RunPending => {
            Failure::new(Code::NotAtPrompt)
        }
        RunRefusal::NoIntegration => unsupported(UnsupportedSessionReason::NoIntegration),
        RunRefusal::NoBracketedPaste => unsupported(UnsupportedSessionReason::NoBracketedPaste),
        RunRefusal::SessionChanged => Failure::new(Code::SessionChanged),
        RunRefusal::Capacity => Failure::new(Code::Capacity),
    }
}

fn doubt(doubt: RunDoubt) -> RunDoubtName {
    match doubt {
        RunDoubt::NoExitStatus => RunDoubtName::NoExitStatus,
        RunDoubt::NoCommandStart => RunDoubtName::NoCommandStart,
        RunDoubt::CmdlineMismatch => RunDoubtName::CmdlineMismatch,
        RunDoubt::IntegrationLost => RunDoubtName::IntegrationLost,
    }
}

/// Whether the receipt is final. A run another client took over still follows its command.
fn settled(state: RunState) -> bool {
    !matches!(
        state,
        RunState::Admitted | RunState::Running | RunState::TakenOver
    )
}

/// The `run` data and the failure a receipt state means.
fn outcome(run: RunId, state: Option<RunState>, claim: (bool, bool)) -> (Run, Option<Failure>) {
    let (started_shell, repaired) = claim;
    let mut data = Run {
        id: hex(&run.0),
        state: RunStateName::Running,
        reason: None,
        exit_status: None,
        duration_ms: None,
        started_shell,
        repaired,
    };
    let unconfirmed = |data: &mut Run, reason| {
        data.state = RunStateName::Unconfirmed;
        data.reason = Some(reason);
        Some(Failure::with(
            Code::RunUnconfirmed,
            Details::RunUnconfirmed(RunUnconfirmedDetails { reason }),
        ))
    };
    let failure = match state {
        None => unconfirmed(&mut data, RunDoubtName::Disconnected),
        Some(RunState::Admitted) => {
            data.state = RunStateName::Admitted;
            None
        }
        Some(RunState::Running) => None,
        Some(RunState::Finished {
            exit_status: Some(status),
            duration_ms,
        }) => {
            data.state = RunStateName::Finished;
            data.exit_status = Some(status);
            data.duration_ms = Some(duration_ms);
            (status != 0).then(|| {
                Failure::with(
                    Code::CommandFailed,
                    Details::CommandFailed(CommandFailedDetails {
                        exit_status: status,
                    }),
                )
            })
        }
        Some(RunState::Finished {
            exit_status: None,
            duration_ms,
        }) => {
            data.duration_ms = Some(duration_ms);
            unconfirmed(&mut data, RunDoubtName::NoExitStatus)
        }
        Some(RunState::Unconfirmed(reason)) => unconfirmed(&mut data, doubt(reason)),
        Some(RunState::SessionEnded) => {
            data.state = RunStateName::SessionEnded;
            Some(session_failure(Code::SessionEnded, SessionStateName::Ended))
        }
        Some(RunState::TakenOver) => {
            data.state = RunStateName::TakenOver;
            Some(controlled(ControlledReason::TakenOver))
        }
    };
    (data, failure)
}

/// How waiting for the run ended.
enum Finish {
    Receipt(RunReceipt),
    /// The deadline passed while the command still ran.
    Running,
    /// The connection was lost, and `RunStatus` did not settle the run before the deadline.
    Lost,
}

/// `pane run`. `connect` opens a new link when the reply or the run's channel is lost.
#[allow(clippy::too_many_arguments, reason = "one command's whole context")]
pub fn run(
    link: &ServerLink,
    connect: &mut dyn FnMut() -> Result<ServerLink, Failure>,
    target: &PaneTarget,
    args: &RunArgs,
    stdin: &mut dyn BufRead,
    runs: &mut Runs,
    deadline: Deadline,
) -> Done<PaneRun> {
    let command = match command_line(&args.command, stdin) {
        Ok(command) => command,
        Err(failure) => return Done::failed(failure),
    };
    let directory = match &args.directory {
        Some(directory) if directory.is_relative() => match std::env::current_dir() {
            Ok(current) => Some(current.join(directory)),
            Err(_) => return Done::failed(usage("--directory")),
        },
        directory => directory.clone(),
    };
    let claimed = match claim(link, target, args, directory, deadline.or(CLAIM_TIMEOUT)) {
        Ok(claimed) => claimed,
        Err(failure) => return Done::failed(failure),
    };
    let Claim {
        mut channel,
        lease,
        started_shell,
        repaired,
    } = claimed;
    let flags = (started_shell, repaired);
    if let Some(prefix) = &args.session
        && !hex(&lease.session).starts_with(prefix.as_str())
    {
        channel.keep();
        return Done::failed(Failure::new(Code::SessionChanged));
    }
    let run = match run_id() {
        Ok(run) => run,
        Err(failure) => return Done::failed(failure),
    };
    let pending = PendingRun {
        run,
        pane: target.pane,
        session: lease.session,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    };
    if let Err(failure) = runs.add(pending) {
        channel.keep();
        return Done::failed(failure);
    }
    let shown = channel.show();
    let admitted = shown.and_then(|()| {
        request(
            link,
            Operation::Run {
                channel: channel.number(),
                run,
                command,
                session: lease.session,
            },
            deadline,
        )
    });
    let start = match admitted {
        Ok(ReplyBody::Run(RunReply::Admitted { start })) => Some(start),
        Ok(ReplyBody::Run(RunReply::Refused(refused))) => {
            let _ = runs.remove(run);
            channel.keep();
            return Done::failed(refusal(refused));
        }
        // A reused id was answered; this one is new, so this cannot be our run.
        Ok(_) => {
            let _ = runs.remove(run);
            channel.keep();
            return Done::failed(Failure::new(Code::ProtocolError));
        }
        // A lost reply may follow a written command. It is resolved, never sent again.
        Err(failure) if matches!(failure.code, Code::Timeout | Code::Unreachable) => None,
        Err(failure) => {
            let _ = runs.remove(run);
            channel.keep();
            return Done::failed(failure);
        }
    };
    // The link the run is resolved on, which a lost connection replaces.
    let mut current = link.clone();
    let finish = match start {
        Some(_) => follow(&mut channel, &mut current, connect, run, deadline),
        None => resolve(&mut current, connect, run, deadline),
    };
    let receipt = match finish {
        Finish::Receipt(receipt) => receipt,
        Finish::Running => {
            channel.keep();
            let (data, _) = outcome(run, Some(RunState::Running), flags);
            return Done {
                data: Some(PaneRun {
                    target: target.steps.clone(),
                    run: data,
                    output: None,
                }),
                warnings: Vec::new(),
                failure: Some(deadline.timeout()),
            };
        }
        Finish::Lost => {
            let (data, failure) = outcome(run, None, flags);
            return Done {
                data: Some(PaneRun {
                    target: target.steps.clone(),
                    run: data,
                    output: None,
                }),
                warnings: Vec::new(),
                failure,
            };
        }
    };
    let output = start.and_then(|start| output(&mut channel, target, &receipt, start, args.tail));
    if settled(receipt.state) {
        // The receipt is retired only once it reached this process; a later command of this
        // identity retires one that did not.
        let retired = request(
            &current,
            Operation::RunAcknowledge { run },
            deadline.or(CLAIM_TIMEOUT),
        );
        if retired.is_ok() {
            let _ = runs.remove(run);
        }
    }
    channel.keep();
    let (data, failure) = outcome(run, Some(receipt.state), flags);
    let output = output.filter(|_| {
        failure
            .as_ref()
            .is_none_or(|failure| failure.code == Code::CommandFailed)
    });
    Done {
        data: Some(PaneRun {
            target: target.steps.clone(),
            run: data,
            output,
        }),
        warnings: Vec::new(),
        failure,
    }
}

/// Waits on the run's channel for its receipt.
fn follow(
    channel: &mut Channel,
    link: &mut ServerLink,
    connect: &mut dyn FnMut() -> Result<ServerLink, Failure>,
    run: RunId,
    deadline: Deadline,
) -> Finish {
    loop {
        match channel.next(deadline) {
            Ok(None) => return Finish::Running,
            Ok(Some(Event::Run(receipt))) if receipt.run == run => {
                return Finish::Receipt(receipt);
            }
            Ok(Some(Event::Ended(PaneEvent::Takeover(_)))) => {
                return match status(link, run, deadline) {
                    Ok(Some(receipt)) if settled(receipt.state) => Finish::Receipt(receipt),
                    _ => Finish::Receipt(RunReceipt {
                        run,
                        session: [0; 32],
                        state: RunState::TakenOver,
                        end: None,
                    }),
                };
            }
            // The channel ended without the receipt, so the server holds it.
            Ok(Some(Event::Ended(_))) => return resolve(link, connect, run, deadline),
            Ok(Some(Event::Closed(_))) | Err(_) => return resolve(link, connect, run, deadline),
            Ok(Some(_)) => {}
        }
    }
}

/// The server's receipt for `run`, or `None` when it has none.
fn status(
    link: &ServerLink,
    run: RunId,
    deadline: Deadline,
) -> Result<Option<RunReceipt>, Failure> {
    match request(
        link,
        Operation::RunStatus { run },
        deadline.or(CLAIM_TIMEOUT),
    ) {
        Ok(ReplyBody::Run(RunReply::Receipt(receipt))) => Ok(Some(receipt)),
        Ok(_) => Err(Failure::new(Code::ProtocolError)),
        Err(failure) if failure.code == Code::SessionEnded => Ok(None),
        Err(failure) => Err(failure),
    }
}

/// Resolves `run` with `RunStatus` until it settles or the deadline passes, reconnecting when
/// the link is lost. It never sends the command again.
fn resolve(
    link: &mut ServerLink,
    connect: &mut dyn FnMut() -> Result<ServerLink, Failure>,
    run: RunId,
    deadline: Deadline,
) -> Finish {
    loop {
        match status(link, run, deadline) {
            Ok(Some(receipt)) if settled(receipt.state) || receipt.state == RunState::TakenOver => {
                return Finish::Receipt(receipt);
            }
            // The server never admitted the run.
            Ok(None) => return Finish::Lost,
            Ok(Some(_)) => {}
            Err(failure)
                if failure.code == Code::Unreachable || failure.code == Code::ProtocolError =>
            {
                if let Ok(fresh) = connect() {
                    *link = fresh;
                }
            }
            Err(_) => {}
        }
        if deadline.passed() {
            return Finish::Lost;
        }
        let pause = deadline.until().map_or(RESOLVE_INTERVAL, |until| {
            RESOLVE_INTERVAL.min(until.saturating_duration_since(Instant::now()))
        });
        std::thread::sleep(pause);
    }
}

/// The run's output: rows from the admission position to its finish, then the screen.
fn output(
    channel: &mut Channel,
    target: &PaneTarget,
    receipt: &RunReceipt,
    start: HistoryPosition,
    tail: Option<u32>,
) -> Option<crate::contract::Rows> {
    let deadline = Deadline::after(Some(CLAIM_TIMEOUT));
    channel.await_screen(deadline).ok()?;
    let end = receipt.end.filter(|end| end.epoch == start.epoch);
    // The finish can arrive before the frame that shows it. Wait briefly for a screen whose
    // cursor reached the finish row.
    if let Some(end) = end {
        let settle = Deadline::after(Some(SCREEN_SETTLE));
        while channel.screen.history.total
            + channel.screen.cursor.map_or(0, |(row, _)| u64::from(row))
            < end.row
        {
            match channel.next(settle) {
                Ok(Some(Event::Frame | Event::Progress | Event::Input { .. })) => {}
                _ => break,
            }
        }
    }
    let mode = Mode::Range {
        start: start.row,
        end: end.map_or(u64::MAX, |end| end.row),
    };
    let mut captured = read::capture(
        &mut Live { channel, deadline },
        mode,
        Some(start.epoch),
        read::DEFAULT_MAX_BYTES,
        Format::Text,
    )
    .ok()?;
    if let Some(tail) = tail {
        let keep = usize::try_from(tail).unwrap_or(usize::MAX);
        let dropped = captured.rows.len().saturating_sub(keep);
        if dropped > 0 {
            captured.rows.drain(..dropped);
            captured.truncated = true;
        }
    }
    Some(read::rows(
        captured,
        target,
        receipt.session,
        Format::Text,
        Coverage::Complete,
    ))
}
