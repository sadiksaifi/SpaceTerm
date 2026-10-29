//! `pane send`: type text or keys into a running Terminal Session. It never starts a shell.
//!
//! Send claims with the running size, focuses the terminal, and writes its input in batches.
//! Delivery is confirmed once a frame or progress reports the last Input serial written to the
//! PTY. Input is never replayed: a connection lost before then is `input_unconfirmed`.
use super::channel::{self, Channel, Claimed, Event, pane_size};
use super::keys;
use super::{Deadline, Done, PaneTarget, controlled, link_failure, session_failure};
use crate::args::SendArgs;
use crate::contract::{
    Code, ControlledReason, Delivery, Details, Failure, PaneSend, SessionStateName, UsageDetails,
    UsageKind, from_server_error,
};
use spaceterm_server::connection::link::ServerLink;
use spaceterm_server::pane::{AttachmentClaim, FocusTarget, PasteOutcome};
use spaceterm_server::protocol::limits::MAX_PASTE_BYTES;
use spaceterm_server::protocol::{InputEvent, InputOutcome, Refusal};
use std::io::Read;
use std::time::Duration;

/// `pane send` gives up after this long.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The focus generation a command's claim uses. Each claim starts from no focus.
const GENERATION: u64 = 1;

/// What one `pane send` types.
enum Typing {
    Keys(Vec<spaceterm_server::pane::Key>),
    Paste(String),
}

fn usage(argument: &str) -> Failure {
    Failure::with(
        Code::InvalidArguments,
        Details::Usage(UsageDetails {
            argument: Some(argument.into()),
            kind: UsageKind::InvalidValue,
        }),
    )
}

/// The input the arguments name. `-` with `--paste` reads stdin, up to 1 MiB.
fn typing(args: &SendArgs, stdin: &mut dyn Read) -> Result<Typing, Failure> {
    if args.paste {
        let text = match args.text.as_deref() {
            Some("-") => {
                let mut bytes = Vec::new();
                stdin
                    .take(MAX_PASTE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| usage("<TEXT>"))?;
                if bytes.len() > MAX_PASTE_BYTES {
                    return Err(usage("<TEXT>"));
                }
                String::from_utf8(bytes).map_err(|_| usage("<TEXT>"))?
            }
            Some(text) => text.to_owned(),
            None => return Err(usage("<TEXT>")),
        };
        return Ok(Typing::Paste(text));
    }
    let mut typed = args.text.as_deref().map(keys::typed).unwrap_or_default();
    typed.extend(keys::parse(&args.keys)?);
    if args.enter {
        typed.extend(keys::parse(&["Enter".to_owned()])?);
    }
    if typed.is_empty() {
        return Err(usage("<TEXT>"));
    }
    Ok(Typing::Keys(typed))
}

/// Claims the target's running Session for input on a new channel. It never starts one.
pub(crate) fn claim_running(
    link: &ServerLink,
    target: &PaneTarget,
    take_over: bool,
    size: Option<crate::args::TerminalSize>,
    deadline: Deadline,
) -> Result<(Channel, spaceterm_server::pane::Lease), Failure> {
    let size = size.map(pane_size);
    let channel = Channel::open(link)?;
    let claim = AttachmentClaim::Select { explicit: false };
    match channel.claim(target, claim, size, deadline)? {
        Claimed::Attached(lease) => Ok((channel, lease)),
        Claimed::Takeover(takeover) if takeover.session_ended.is_some() => {
            Err(session_failure(Code::SessionEnded, SessionStateName::Ended))
        }
        Claimed::Takeover(_) if !take_over => Err(controlled(ControlledReason::Other)),
        // A claim that did not attach closed its channel; Take Over uses a new one.
        Claimed::Takeover(takeover) => {
            let channel = Channel::open(link)?;
            let claim = AttachmentClaim::TakeOver {
                target: takeover.target,
            };
            match channel.claim(target, claim, size, deadline)? {
                Claimed::Attached(lease) => Ok((channel, lease)),
                _ => Err(controlled(ControlledReason::Other)),
            }
        }
        Claimed::Restored(restored) if restored.session_ended.is_some() => {
            Err(session_failure(Code::SessionEnded, SessionStateName::Ended))
        }
        Claimed::Restored(_) => Err(session_failure(
            Code::SessionNotStarted,
            SessionStateName::NotStarted,
        )),
    }
}

/// A discarded Input message, as a failure.
pub(crate) fn discarded(refusal: Refusal) -> Failure {
    match refusal {
        Refusal::Stale => controlled(ControlledReason::Stale),
        Refusal::Unsupported => usage("<TEXT>"),
        refusal => from_server_error(refusal.into()),
    }
}

/// `pane send`.
pub fn send(
    link: &ServerLink,
    target: &PaneTarget,
    args: &SendArgs,
    stdin: &mut dyn Read,
    deadline: Deadline,
) -> Done<PaneSend> {
    let deadline = deadline.or(DEFAULT_TIMEOUT);
    let typing = match typing(args, stdin) {
        Ok(typing) => typing,
        Err(failure) => return Done::failed(failure),
    };
    let (mut channel, lease) =
        match claim_running(link, target, args.take_over, args.size, deadline) {
            Ok(claimed) => claimed,
            Err(failure) => return Done::failed(failure),
        };
    if let Some(prefix) = &args.session
        && !super::hex(&lease.session).starts_with(prefix.as_str())
    {
        return Done::failed(Failure::new(Code::SessionChanged));
    }
    let result = deliver(&mut channel, typing, args.allow_unsafe_paste, deadline);
    let done = |serial, delivered, failure| Done {
        data: Some(PaneSend {
            target: target.steps.clone(),
            input: Delivery { serial, delivered },
        }),
        warnings: Vec::new(),
        failure,
    };
    let done = match result {
        Ok(serial) => done(serial, true, None),
        Err((serial, failure)) => done(serial, false, Some(failure)),
    };
    // The process ends without releasing: this identity keeps control.
    channel.keep();
    done
}

/// Writes the input and waits until the PTY accepted its last serial. On failure, returns the
/// last serial sent with the failure.
fn deliver(
    channel: &mut Channel,
    typing: Typing,
    allow_unsafe_paste: bool,
    deadline: Deadline,
) -> Result<u32, (u32, Failure)> {
    channel.show().map_err(|failure| (0, failure))?;
    let handle = channel.handle();
    let mut last = handle
        .input(GENERATION, vec![InputEvent::Focus(FocusTarget::Terminal)])
        .map_err(|error| (0, link_failure(error)))?;
    let pasting = match typing {
        Typing::Keys(keys) => {
            for batch in keys::batches(keys) {
                last = handle
                    .input(GENERATION, batch.into_iter().map(InputEvent::Key).collect())
                    .map_err(|error| (last, unconfirmed_or(link_failure(error))))?;
            }
            false
        }
        Typing::Paste(text) => {
            last = handle
                .paste(GENERATION, &text)
                .map_err(|error| (last, unconfirmed_or(link_failure(error))))?;
            true
        }
    };
    let mut paste_settled = !pasting;
    loop {
        if paste_settled && channel.input >= last {
            return Ok(last);
        }
        match channel.next(deadline).map_err(|failure| (last, failure))? {
            None => return Err((last, deadline.timeout())),
            Some(Event::Input { serial, outcome }) if serial == last => match outcome {
                InputOutcome::Discarded(refusal) => return Err((last, discarded(refusal))),
                InputOutcome::Paste(PasteOutcome::Written) => paste_settled = true,
                InputOutcome::Paste(PasteOutcome::ConfirmationRequired(confirmation)) => {
                    // Without --allow-unsafe-paste the paste is cancelled and nothing is written.
                    last = channel
                        .handle()
                        .input(
                            GENERATION,
                            vec![InputEvent::ResolvePaste {
                                id: confirmation.id,
                                confirm: allow_unsafe_paste,
                            }],
                        )
                        .map_err(|error| (last, unconfirmed_or(link_failure(error))))?;
                    if !allow_unsafe_paste {
                        return Err((last, Failure::new(Code::PasteConfirmationRequired)));
                    }
                }
                InputOutcome::Paste(PasteOutcome::Rejected(_)) => {
                    return Err((last, usage("<TEXT>")));
                }
                InputOutcome::Paste(PasteOutcome::Cancelled | PasteOutcome::Stale) => {
                    return Err((last, controlled(ControlledReason::Stale)));
                }
            },
            Some(Event::Input {
                outcome: InputOutcome::Discarded(refusal),
                ..
            }) => return Err((last, discarded(refusal))),
            Some(Event::Ended(event)) => return Err((last, channel::ended(&event))),
            Some(Event::Closed(_)) => return Err((last, Failure::new(Code::InputUnconfirmed))),
            Some(_) => {}
        }
    }
}

/// Input that may have left before the link failed is unconfirmed.
fn unconfirmed_or(failure: Failure) -> Failure {
    if failure.code == Code::Unreachable {
        Failure::new(Code::InputUnconfirmed)
    } else {
        failure
    }
}
