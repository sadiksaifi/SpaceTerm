//! Machine updates: what the desktop knows about each Machine's SpaceTerm Core updates, and the
//! actions it takes on them. The desktop's own update stays on [`super::UpdateAdapter`].
//!
//! Restart All is the UI performing [`MachineAction::Restart`] for each waiting Machine.
//! Confirmation alerts read `facts.sessions` and `pending.breaking` before `perform`.
#![allow(
    dead_code,
    reason = "#799 composes the adapter and builds the UI that consumes it"
)]

pub(crate) mod live;
#[cfg(feature = "developer-tools")]
pub(crate) mod preview;
pub(crate) mod store;
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
    /// This desktop's embedded Core version for the Machine's platform, when it is newer than
    /// every version the Core has and not `facts.skipped`, whether or not its Update Floor
    /// admits `facts.running`. It differs from `push` while the Core must first reach that floor
    /// from its own feed through a Stepping Stone.
    pub(crate) bundled: Option<Version>,
    /// The last push of `push` that failed. The shared store records it, because a push that
    /// breaks off or runs out of disk space records nothing in the Core's facts. Adapters leave
    /// it None.
    pub(crate) failed_push: Option<FailedPush>,
}

/// A push of this desktop's embedded Core build that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailedPush {
    pub(crate) version: Version,
    pub(crate) failure: ActionFailure,
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
    /// The Core's service lane admits this desktop but answers `Facts` with `unsupported`: it
    /// predates update facts, so SpaceTerm can't check its updates ("Can't check").
    Unreported,
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
    /// The Machine ran out of disk space.
    DiskFull,
    /// This Mac's Core is off in Login Items, so it refused to start. Only the person turns it on,
    /// there, and the surface opens that list instead of explaining a failure.
    LoginItemOff,
    /// The running app's embedded Core was replaced on disk.
    RelaunchRequired,
    ClockBehind,
    SteppingStoneSkipped([u64; 3]),
    /// The Machine must fetch this release before it can accept the bundled Core.
    NeedsFloor(spaceterm_core::update::ordering::Floor),
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

/// The adapter the desktop composes: in a `developer-tools` build the preview
/// `SPACETERM_MACHINE_PREVIEW` names, otherwise This Mac or This Computer and each paired
/// Machine over its SpaceTerm Core's service lane.
pub(crate) fn composed(
    identity: &str,
    direct: &crate::direct_connections::DirectConnections,
) -> std::rc::Rc<dyn MachineUpdatesAdapter> {
    #[cfg(feature = "developer-tools")]
    if let Some(preview) = preview::from_environment() {
        return std::rc::Rc::new(preview);
    }
    std::rc::Rc::new(live::LiveMachines::new(
        std::sync::Arc::new(live::desktop::DesktopSources::new(identity, direct.clone())),
        super::push::EmbeddedCores::new(
            crate::platform::launch_host::core_artifact_catalog(),
            identity,
        ),
    ))
}

/// A Restart or Stop the Machine's SpaceTerm Core previewed, which the person confirms before
/// [`MachineUpdatesAdapter::execute`] runs exactly it. The Core binds its preview to the
/// Terminal Sessions and Pending Version it observed and refuses a later change with
/// [`ActionFailure::Changed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachinePreview {
    pub(crate) machine: MachineId,
    pub(crate) action: MachineAction,
    /// The Terminal Sessions it ends.
    pub(crate) sessions: u32,
    /// The running version the Core's preview observed.
    pub(crate) running: Option<Version>,
    /// The version a Restart starts, when the Core names one.
    pub(crate) target: Option<Version>,
    /// The adapter's own name for the retained preview.
    pub(crate) id: u64,
}

/// The answer to [`MachineUpdatesAdapter::preview`], sent once.
pub(crate) type PreviewAnswer = async_channel::Receiver<Result<MachinePreview, ActionFailure>>;

pub(crate) trait MachineUpdatesAdapter {
    fn start(
        &self,
        events: async_channel::Sender<MachineUpdatesEvent>,
    ) -> Result<(), MachineUpdatesError>;
    /// Performs an action that needs no confirmation: Install, Check Again, Dismiss Failure or
    /// Start. Restart and Stop run only through [`Self::preview`] and [`Self::execute`].
    fn perform(&self, machine: MachineId, action: MachineAction);
    /// Asks the Machine's Core to preview a Restart or Stop.
    fn preview(&self, machine: MachineId, action: MachineAction) -> PreviewAnswer;
    /// Executes exactly `preview`. It ends with one [`MachineUpdatesEvent::Finished`] for
    /// `preview.action`.
    fn execute(&self, preview: MachinePreview);
}
