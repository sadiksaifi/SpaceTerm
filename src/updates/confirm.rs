//! Restart and Stop confirmations from the Cores' own previews (dialogs D1 and D2), and the
//! approval that binds a confirmation to the work it described.
use gpui::SharedString;
use spaceterm_core::update::facts::Version;

use super::copy::dialogs;
use super::machines::{MachineAction, MachineId, MachinePreview, MachineUpdate};
use super::state::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirmation {
    pub(crate) title: SharedString,
    pub(crate) message: SharedString,
    /// The destructive button: "Restart Core" or "Stop".
    pub(crate) button: SharedString,
    /// The other button, the alert's Cancel: "Remind Me Tomorrow" when it hides a Machine's
    /// Ask step for a day ([`Confirmation::defers`]), otherwise "Cancel", which changes nothing.
    pub(crate) cancel: SharedString,
    pub(crate) actions: Vec<(MachineId, MachineAction)>,
    /// The work the alert described. Approving it authorizes exactly that work.
    pub(crate) approval: Approval,
}

impl Confirmation {
    /// Names the other button "Remind Me Tomorrow" when Cancel hides the step of a Machine this
    /// confirmation restarts, as `defers` says for each Machine. The same check decides whether
    /// Cancel records the deferral, so the button never promises one that does not happen.
    pub(crate) fn defers(mut self, defers: impl Fn(MachineId) -> bool) -> Self {
        if self
            .actions
            .iter()
            .any(|(machine, action)| *action == MachineAction::Restart && defers(*machine))
        {
            self.cancel = dialogs::REMIND_ME_TOMORROW.into();
        }
        self
    }
}

/// An alert with the full sentence. `proceed` names the button that runs `then`, "OK" when
/// None. `dismiss` runs when the alert closes any other way, or after OK without `then`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Explanation {
    pub(crate) title: SharedString,
    pub(crate) message: SharedString,
    pub(crate) then: Option<Box<Command>>,
    pub(crate) proceed: Option<SharedString>,
    pub(crate) dismiss: Option<Box<Command>>,
}

impl Explanation {
    /// An alert with only OK, which runs nothing.
    pub(crate) fn ok(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            then: None,
            proceed: None,
            dismiss: None,
        }
    }

    /// The label of the button that runs `then`, or None for an alert with only OK.
    pub(crate) fn follow_up(&self) -> Option<SharedString> {
        let then = self.then.as_deref()?;
        Some(self.proceed.clone().unwrap_or_else(|| {
            match then {
                Command::Machine(_, MachineAction::CheckAgain) => dialogs::CHECK_AGAIN,
                _ => dialogs::TRY_AGAIN,
            }
            .into()
        }))
    }

    /// The command an answer runs, the same on every surface: `then` on its follow-up button,
    /// and `dismiss` on Cancel, on closing the alert, or on OK.
    pub(crate) fn answer(self, proceeded: bool) -> Option<Command> {
        match (proceeded, self.then) {
            (true, Some(then)) => Some(*then),
            _ => self.dismiss.map(|dismiss| *dismiss),
        }
    }
}

/// What a confirmation described for each Machine it affects whose facts are known: the sessions
/// it ends, the Core that runs and the Pending Version it restarts into. A surface checks
/// [`Approval::holds`] against the current facts before it executes the approved previews, and
/// previews again when it does not hold. The Core refuses its own preview's token once the
/// sessions or the Pending Version changed, which also covers a Machine without facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Approval {
    machines: Vec<Described>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Described {
    machine: MachineId,
    sessions: u32,
    running: Option<Version>,
    target: Option<Version>,
    /// None while the target is unobserved, so no facts match it.
    breaking: Option<bool>,
}

impl Described {
    fn of(update: &MachineUpdate) -> Self {
        let facts = update.facts.as_ref();
        let pending = facts.and_then(|facts| facts.pending.as_ref());
        Self {
            machine: update.machine,
            sessions: facts.map_or(0, |facts| facts.sessions),
            running: facts.map(|facts| facts.running.clone()),
            target: pending.map(|pending| pending.version.clone()),
            breaking: Some(pending.is_some_and(|pending| pending.breaking)),
        }
    }
}

impl Approval {
    /// Whether the person's approval still covers a new `preview` of its work. A restart the
    /// alert described into a Pending Version is not covered once the Core previews none: that
    /// update already ran or went away.
    pub(crate) fn still_approves(&self, preview: &MachinePreview) -> bool {
        !(preview.action == MachineAction::Restart
            && preview.target.is_none()
            && self
                .target(preview.machine)
                .is_some_and(|target| target.is_some()))
    }

    /// Whether a new `preview` restarts into another Pending Version than the alert described.
    pub(crate) fn retargets(&self, preview: &MachinePreview) -> bool {
        preview.action == MachineAction::Restart
            && preview.target.is_some()
            && self
                .target(preview.machine)
                .is_some_and(|target| target != preview.target.as_ref())
    }

    fn target(&self, machine: MachineId) -> Option<Option<&Version>> {
        self.machines
            .iter()
            .find(|described| described.machine == machine)
            .map(|described| described.target.as_ref())
    }

    /// Whether `machines` still show the work the alert described: the same sessions, running
    /// Core and Pending Version on every affected Machine. Other changes keep the approval.
    pub(crate) fn holds(&self, machines: &[MachineUpdate]) -> bool {
        self.machines.iter().all(|described| {
            machines
                .iter()
                .find(|update| update.machine == described.machine)
                .is_some_and(|update| Described::of(update) == *described)
        })
    }
}

/// One previewed Machine, as its confirmation describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Previewed {
    pub(crate) machine: MachineId,
    pub(crate) name: SharedString,
    pub(crate) sessions: u32,
    pub(crate) running: Option<Version>,
    /// The Pending Version a Restart starts; None for a same-version restart or a Stop.
    pub(crate) target: Option<Version>,
    /// None while the target is unobserved: the facts did not describe it yet.
    pub(crate) breaking: Option<bool>,
    pub(crate) stranded: Vec<String>,
}

impl Previewed {
    fn of(preview: &MachinePreview, update: &MachineUpdate) -> Self {
        let facts = update.facts.as_ref();
        let observed = facts
            .and_then(|facts| facts.pending.as_ref())
            .filter(|pending| {
                preview.action == MachineAction::Restart
                    && Some(&pending.version) == preview.target.as_ref()
            });
        let target = (preview.action == MachineAction::Restart)
            .then(|| preview.target.clone())
            .flatten();
        Self {
            machine: preview.machine,
            name: update.name.clone(),
            sessions: preview.sessions,
            running: preview
                .running
                .clone()
                .or_else(|| facts.map(|facts| facts.running.clone())),
            breaking: match (&target, observed) {
                (None, _) => Some(false),
                (Some(_), Some(pending)) => Some(pending.breaking),
                (Some(_), None) => None,
            },
            stranded: observed.map_or_else(Vec::new, |pending| {
                pending
                    .stranded
                    .iter()
                    .map(|device| device.name.clone())
                    .collect()
            }),
            target,
        }
    }

    /// Whether a Restart asks first: it ends sessions, crosses a breaking change, or restarts
    /// into a version the facts have not described.
    fn restart_asks(&self) -> bool {
        self.sessions > 0 || self.breaking != Some(false)
    }

    fn described(&self) -> Described {
        Described {
            machine: self.machine,
            sessions: self.sessions,
            running: self.running.clone(),
            target: self.target.clone(),
            breaking: self.breaking,
        }
    }
}

/// The confirmation for the Restarts or the Stop `previews` describe, with the sessions each
/// Core itself will end. None when nothing asks: no session ends and no breaking change waits,
/// unless `always`, as after a Core refused a preview the person never saw. A Machine no
/// longer listed is left out. Its other button reads "Cancel" until [`Confirmation::defers`]
/// names it for a deferral.
pub(crate) fn previewed(
    machines: &[MachineUpdate],
    previews: &[MachinePreview],
    always: bool,
) -> Option<Confirmation> {
    let described: Vec<Previewed> = previews
        .iter()
        .filter_map(|preview| {
            let update = machines
                .iter()
                .find(|update| update.machine == preview.machine)?;
            Some(Previewed::of(preview, update))
        })
        .collect();
    let action = previews.first().map(|preview| preview.action.clone())?;
    let asks = match action {
        MachineAction::Stop => described.iter().any(|one| one.sessions > 0),
        MachineAction::Restart => described.iter().any(Previewed::restart_asks),
        _ => return None,
    };
    if described.is_empty() || !(always || asks) {
        return None;
    }
    let (title, message, button) = match action {
        MachineAction::Stop => dialogs::stop(&described),
        _ => dialogs::restart(&described),
    };
    Some(Confirmation {
        title,
        message,
        button,
        cancel: dialogs::CANCEL.into(),
        actions: described
            .iter()
            .map(|one| (one.machine, action.clone()))
            .collect(),
        approval: Approval {
            machines: described
                .iter()
                // Without facts there is nothing to compare; the Core's token guards alone.
                .filter(|one| one.running.is_some())
                .map(Previewed::described)
                .collect(),
        },
    })
}
