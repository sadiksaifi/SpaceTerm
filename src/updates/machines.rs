//! Machine updates: what the desktop knows about each Machine's SpaceTerm Core updates, and the
//! actions it takes on them. The desktop's own update stays on [`super::UpdateAdapter`].
//!
//! Restart All is the UI performing [`MachineAction::Restart`] for each waiting Machine.
//! Confirmation alerts read `facts.sessions` and `pending.breaking` before `perform`.
#![allow(
    dead_code,
    reason = "#799 composes the adapter and builds the UI that consumes it"
)]

#[cfg(feature = "developer-tools")]
pub(crate) mod preview;
#[cfg(test)]
pub(crate) mod testing;

use gpui::SharedString;
use spaceterm_core::protocol::capability::{Capabilities, Unavailable};
use spaceterm_core::protocol::version::Incompatible;
use spaceterm_core::update::facts::{UpdateFacts, Version};

/// The Core's server ID.
pub(crate) type MachineId = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineUpdate {
    pub(crate) machine: MachineId,
    /// "This Mac", "This Computer" or the paired name.
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    pub(crate) link: MachineLink,
    /// The latest facts received in this run over either lane. None before the first.
    pub(crate) facts: Option<UpdateFacts>,
    pub(crate) capabilities: Capabilities,
    /// This desktop's embedded Core version for the Machine's platform, when it is newer than
    /// `facts.running` and not `facts.skipped`. It drives "Update from This Mac".
    pub(crate) push: Option<Version>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineLink {
    /// A shared work major.
    Connected,
    /// No shared work major. Facts arrive over the service lane when it admits.
    Incompatible(Incompatible),
    /// This Mac's or This Computer's Core is deliberately stopped.
    Stopped,
    /// No route answers.
    Offline { since: u64 },
    /// A restart began, and the Core did not come back on `expected` before the deadline
    /// ("Can't confirm").
    Unconfirmed { expected: Version, since: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MachineAction {
    /// Pushes `push` when set, otherwise asks the Core to fetch from its feed.
    Install,
    Restart,
    CheckAgain,
    /// Retires `last_failure.id` on this desktop only.
    DismissFailure(u64),
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionFailure {
    Unavailable(Unavailable),
    /// Another service operation runs.
    Busy,
    /// The sessions or the Pending Version changed since the confirmation. Ask again.
    Changed,
    Unreachable,
    Failed,
}

#[derive(Clone, Debug)]
pub(crate) enum MachineUpdatesEvent {
    /// Every known Machine. The first event, and again after a Machine is added or removed.
    Machines(Vec<MachineUpdate>),
    Changed(Box<MachineUpdate>),
    Finished {
        machine: MachineId,
        action: MachineAction,
        outcome: Result<(), ActionFailure>,
    },
}

/// The adapter cannot report Machine updates in this build or session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum MachineUpdatesError {
    #[error("Machine updates are unavailable.")]
    Unavailable,
}

pub(crate) trait MachineUpdatesAdapter {
    fn start(
        &self,
        events: async_channel::Sender<MachineUpdatesEvent>,
    ) -> Result<(), MachineUpdatesError>;
    fn perform(&self, machine: MachineId, action: MachineAction);
}
