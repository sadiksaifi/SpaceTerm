//! Terminal input and output for agents: `pane read`, `pane run`, `pane send` and
//! `pane release`.
//!
//! Each command acts on one Pane that targeting already resolved, over one link to its server.
//! `read` observes and never claims. `run` and `send` claim with the running size and keep
//! control when the process exits. Results follow the output contract: data, warnings, and the
//! failure that decides the exit status.
mod channel;
pub mod cursor;
pub mod keys;
pub mod read;
pub mod run;
pub mod send;
pub mod start;
#[cfg(test)]
mod tests;

use crate::contract::{
    Code, ControlledDetails, ControlledReason, Details, Failure, SessionDetails, SessionStateName,
    TargetStep, Warning, from_fatal, from_server_error,
};
use spaceterm_server::connection::link::{LinkError, RequestError, ServerLink};
use spaceterm_server::hierarchy::Id;
use std::time::{Duration, Instant};

/// The Pane a terminal command acts on, as targeting resolved it.
#[derive(Clone, Debug)]
pub struct PaneTarget {
    /// The server's ID.
    pub server: Id,
    pub workspace: Id,
    pub pane: Id,
    /// Whether a Terminal Session runs in the Pane, as `list` reports it. `pane run` reports
    /// `started_shell` when it did not.
    pub running: bool,
    /// How targeting reached the Pane, for `data.target`.
    pub steps: Vec<TargetStep>,
}

/// What a terminal command reached: its data, its warnings, and the failure that decides its
/// exit status. `data` may be present on failure, as the output contract allows.
#[derive(Debug)]
pub struct Done<D> {
    pub data: Option<D>,
    pub warnings: Vec<Warning>,
    pub failure: Option<Failure>,
}

impl<D> Done<D> {
    pub fn ok(data: D, warnings: Vec<Warning>) -> Self {
        Self {
            data: Some(data),
            warnings,
            failure: None,
        }
    }

    pub fn failed(failure: Failure) -> Self {
        Self {
            data: None,
            warnings: Vec::new(),
            failure: Some(failure),
        }
    }
}

/// When a command gives up. `None` waits as long as it takes.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    started: Instant,
    until: Option<Instant>,
}

impl Deadline {
    pub fn after(timeout: Option<Duration>) -> Self {
        let started = Instant::now();
        Self {
            started,
            until: timeout.map(|timeout| started + timeout),
        }
    }

    /// The deadline, or `default` from now when none was given.
    pub fn or(self, default: Duration) -> Self {
        Self {
            until: self.until.or(Some(self.started + default)),
            ..self
        }
    }

    pub fn until(self) -> Option<Instant> {
        self.until
    }

    pub fn passed(self) -> bool {
        self.until.is_some_and(|until| Instant::now() >= until)
    }

    pub fn elapsed_ms(self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// `timeout`, with how long the command waited.
    pub fn timeout(self) -> Failure {
        Failure::with(
            Code::Timeout,
            Details::Timeout(crate::contract::TimeoutDetails {
                elapsed_ms: self.elapsed_ms(),
            }),
        )
    }
}

/// A lost link is `unreachable`; a refusal or a goodbye maps through the server's error.
pub(crate) fn link_failure(error: LinkError) -> Failure {
    match error {
        LinkError::Goodbye(fatal) => from_fatal(fatal),
        LinkError::Closed | LinkError::Unresponsive | LinkError::Stalled => {
            Failure::new(Code::Unreachable)
        }
        error => from_server_error(error.into()),
    }
}

pub(crate) fn request_failure(error: RequestError) -> Failure {
    match error {
        RequestError::Link(error) => link_failure(error),
        RequestError::Refused(refusal) => from_server_error(refusal.into()),
    }
}

pub(crate) fn session_failure(code: Code, state: SessionStateName) -> Failure {
    Failure::with(code, Details::Session(SessionDetails { state }))
}

pub(crate) fn controlled(reason: ControlledReason) -> Failure {
    Failure::with(
        Code::Controlled,
        Details::Controlled(ControlledDetails { reason }),
    )
}

/// Sends one request and waits for its reply until `deadline`.
pub(crate) fn request(
    link: &ServerLink,
    operation: spaceterm_server::protocol::Operation,
    deadline: Deadline,
) -> Result<spaceterm_server::protocol::ReplyBody, Failure> {
    let pending = link.request(operation).map_err(link_failure)?;
    wait(pending, deadline)
}

pub(crate) fn wait(
    pending: spaceterm_server::connection::link::PendingReply,
    deadline: Deadline,
) -> Result<spaceterm_server::protocol::ReplyBody, Failure> {
    wait_raw(pending, deadline)?.map_err(request_failure)
}

/// The reply as the link gave it, or `timeout` once `deadline` passed.
pub(crate) fn wait_raw(
    pending: spaceterm_server::connection::link::PendingReply,
    deadline: Deadline,
) -> Result<Result<spaceterm_server::protocol::ReplyBody, RequestError>, Failure> {
    match deadline.until() {
        None => Ok(pending.wait()),
        Some(until) => pending
            .wait_for(until.saturating_duration_since(Instant::now()))
            .ok_or_else(|| deadline.timeout()),
    }
}

/// 64 lowercase hex digits.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
