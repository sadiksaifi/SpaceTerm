//! Development-only Machine updates, chosen by `SPACETERM_MACHINE_PREVIEW=<scenario>`. Every
//! Machine, fact and event is synthetic; no Core is contacted.
//!
//! The adapter plays its scenario on timers and answers actions with these transitions:
//! `Install` becomes `Installing` with progress over 2 seconds, then a Pending Version.
//! `Restart` becomes `Restarting`, then `running` equals the former Pending Version after 3
//! seconds. `CheckAgain` becomes `Checking` for 1 second, then `Succeeded`. `Stop` becomes
//! `Stopping`, then `Stopped`. `Start` becomes `Connected`. `DismissFailure` finishes `Ok`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use spaceterm_core::protocol::capability::{Capabilities, Capability, Unavailable};
use spaceterm_core::protocol::version::{self as protocol, Incompatible, ProtocolVersion};
use spaceterm_core::update::facts::{
    CheckFailure, FailureKind, InstallFailure, Operation, PendingVersion, Platform, Protocols,
    StartFailure, StrandedDevice, UpdateCheck, UpdateFacts, UpdateFailure, Version,
};

use super::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesError, MachineUpdatesEvent,
};

const THIS_MACHINE: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];
const DAY: u64 = 86_400;
/// Display fixtures, never a release version authority.
const NEWEST: &str = "0.9.0";
const PREVIOUS: &str = "0.8.0";
const INSTALL_STEPS: u64 = 10;
const INSTALL_BYTES: u64 = 31_457_280;
const INSTALL_STEP: Duration = Duration::from_millis(200);
/// How long an `Install` action's progress takes.
const INSTALL: Duration = Duration::from_secs(2);
/// How long the `installing` scenario's progress takes, so its state stays on screen.
const SCENARIO_INSTALL: Duration = Duration::from_secs(10);
const RESTART: Duration = Duration::from_secs(3);
const CHECK: Duration = Duration::from_secs(1);
const SETTLE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scenario {
    UpToDate,
    UpdateReady,
    SeveralReady,
    Installing,
    Restarting,
    RestartOverdue,
    Breaking,
    CoreTooOld,
    DesktopTooOld,
    CantCheck,
    CantCheckProxy,
    InstallFailedDiskFull,
    InstallFailedDownload,
    InstallFailedVerification,
    UpdateFailed,
    CantConfirm,
    RestartAllPartial,
    Stopped,
    Offline,
    CapabilityMissing,
    UninstallUnavailable,
}

impl Scenario {
    pub(crate) const ALL: [Self; 21] = [
        Self::UpToDate,
        Self::UpdateReady,
        Self::SeveralReady,
        Self::Installing,
        Self::Restarting,
        Self::RestartOverdue,
        Self::Breaking,
        Self::CoreTooOld,
        Self::DesktopTooOld,
        Self::CantCheck,
        Self::CantCheckProxy,
        Self::InstallFailedDiskFull,
        Self::InstallFailedDownload,
        Self::InstallFailedVerification,
        Self::UpdateFailed,
        Self::CantConfirm,
        Self::RestartAllPartial,
        Self::Stopped,
        Self::Offline,
        Self::CapabilityMissing,
        Self::UninstallUnavailable,
    ];

    /// The value `SPACETERM_MACHINE_PREVIEW` names it by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::UpToDate => "up-to-date",
            Self::UpdateReady => "update-ready",
            Self::SeveralReady => "several-ready",
            Self::Installing => "installing",
            Self::Restarting => "restarting",
            Self::RestartOverdue => "restart-overdue",
            Self::Breaking => "breaking",
            Self::CoreTooOld => "core-too-old",
            Self::DesktopTooOld => "desktop-too-old",
            Self::CantCheck => "cant-check",
            Self::CantCheckProxy => "cant-check-proxy",
            Self::InstallFailedDiskFull => "install-failed-disk-full",
            Self::InstallFailedDownload => "install-failed-download",
            Self::InstallFailedVerification => "install-failed-verification",
            Self::UpdateFailed => "update-failed",
            Self::CantConfirm => "cant-confirm",
            Self::RestartAllPartial => "restart-all-partial",
            Self::Stopped => "stopped",
            Self::Offline => "offline",
            Self::CapabilityMissing => "capability-missing",
            Self::UninstallUnavailable => "uninstall-unavailable",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scenario| scenario.name() == value)
    }
}

/// The scenario `SPACETERM_MACHINE_PREVIEW` names, if any.
pub(crate) fn from_environment() -> Option<PreviewMachines> {
    let value = std::env::var("SPACETERM_MACHINE_PREVIEW").ok()?;
    Scenario::parse(&value).map(PreviewMachines::new)
}

/// One change a timer applies to one Machine.
type Change = Box<dyn FnOnce(&mut MachineUpdate) + Send>;

#[derive(Default)]
struct Shared {
    events: Option<async_channel::Sender<MachineUpdatesEvent>>,
    machines: Vec<MachineUpdate>,
    closed: bool,
}

impl Shared {
    fn send(&self, event: MachineUpdatesEvent) {
        if let Some(events) = &self.events {
            let _ = events.try_send(event);
        }
    }
}

pub(crate) struct PreviewMachines {
    scenario: Scenario,
    shared: Arc<Mutex<Shared>>,
}

impl PreviewMachines {
    pub(crate) fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            shared: Arc::new(Mutex::new(Shared {
                machines: machines(scenario, super::super::now()),
                ..Shared::default()
            })),
        }
    }

    /// Applies `changes` to `machine` on timers, sending `Changed` after each, then `finish`.
    fn play(
        &self,
        machine: MachineId,
        changes: Vec<(Duration, Change)>,
        finish: Option<MachineAction>,
    ) {
        let shared = Arc::clone(&self.shared);
        let worker = move || {
            for (delay, change) in changes {
                std::thread::sleep(delay);
                let Ok(mut shared) = shared.lock() else {
                    return;
                };
                if shared.closed {
                    return;
                }
                let Some(update) = shared.machines.iter_mut().find(|m| m.machine == machine) else {
                    return;
                };
                change(update);
                if let Some(facts) = &mut update.facts {
                    facts.revision += 1;
                }
                let changed = Box::new(update.clone());
                shared.send(MachineUpdatesEvent::Changed(changed));
            }
            if let (Some(action), Ok(shared)) = (finish, shared.lock()) {
                shared.send(MachineUpdatesEvent::Finished {
                    machine,
                    action,
                    outcome: Ok(()),
                });
            }
        };
        if std::thread::Builder::new()
            .name("machine-preview".into())
            .spawn(worker)
            .is_err()
        {
            self.finish_now(machine, None, Err(ActionFailure::Failed));
        }
    }

    fn finish_now(
        &self,
        machine: MachineId,
        action: Option<MachineAction>,
        outcome: Result<(), ActionFailure>,
    ) {
        if let (Some(action), Ok(shared)) = (action, self.shared.lock()) {
            shared.send(MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome,
            });
        }
    }

    /// The scenario's own timeline, which starts with the adapter.
    fn timeline(&self) -> Option<(MachineId, Vec<(Duration, Change)>)> {
        match self.scenario {
            Scenario::Installing => Some((PENGUIN, install(version(NEWEST), SCENARIO_INSTALL))),
            Scenario::Restarting => Some((PENGUIN, restart_into(version(NEWEST)))),
            _ => None,
        }
    }
}

impl MachineUpdatesAdapter for PreviewMachines {
    fn start(
        &self,
        events: async_channel::Sender<MachineUpdatesEvent>,
    ) -> Result<(), MachineUpdatesError> {
        {
            let mut shared = self
                .shared
                .lock()
                .map_err(|_| MachineUpdatesError::Unavailable)?;
            shared.events = Some(events);
            let machines = shared.machines.clone();
            shared.send(MachineUpdatesEvent::Machines(machines));
        }
        if let Some((machine, changes)) = self.timeline() {
            self.play(machine, changes, None);
        }
        Ok(())
    }

    fn perform(&self, machine: MachineId, action: MachineAction) {
        let planned = self.shared.lock().ok().and_then(|shared| {
            let update = shared.machines.iter().find(|m| m.machine == machine)?;
            Some(plan(update, &action))
        });
        match planned {
            Some(Ok(changes)) => self.play(machine, changes, Some(action)),
            Some(Err(failure)) => self.finish_now(machine, Some(action), Err(failure)),
            None => self.finish_now(machine, Some(action), Err(ActionFailure::Unreachable)),
        }
    }
}

impl Drop for PreviewMachines {
    fn drop(&mut self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.closed = true;
        }
    }
}

/// The changes `action` makes to `update`, or why it is refused.
fn plan(
    update: &MachineUpdate,
    action: &MachineAction,
) -> Result<Vec<(Duration, Change)>, ActionFailure> {
    if let MachineAction::DismissFailure(_) = action {
        return Ok(Vec::new());
    }
    match (&update.link, action) {
        (MachineLink::Offline { .. }, _) => return Err(ActionFailure::Unreachable),
        (MachineLink::Stopped, MachineAction::Start) => {
            return Ok(vec![(
                SETTLE,
                Box::new(|update: &mut MachineUpdate| {
                    update.link = MachineLink::Connected;
                    update.capabilities = Capabilities::fake(&Capability::ALL);
                }),
            )]);
        }
        (MachineLink::Stopped, _) => return Err(ActionFailure::Unreachable),
        (_, MachineAction::Start) => return Ok(Vec::new()),
        _ => {}
    }
    let facts = update.facts.as_ref().ok_or(ActionFailure::Unreachable)?;
    if facts.operation.is_some() {
        return Err(ActionFailure::Busy);
    }
    let gate = |capability| {
        update
            .capabilities
            .check(capability)
            .map_err(ActionFailure::Unavailable)
    };
    Ok(match action {
        MachineAction::Install => {
            gate(Capability::Install)?;
            let to = update.push.clone().unwrap_or_else(|| version(NEWEST));
            install(to, INSTALL)
        }
        MachineAction::Restart => {
            gate(Capability::Restart)?;
            let to = facts
                .pending
                .as_ref()
                .map_or_else(|| facts.running.clone(), |pending| pending.version.clone());
            restart_into(to)
        }
        MachineAction::CheckAgain => check_again(),
        MachineAction::Stop if !update.local => return Err(ActionFailure::Failed),
        MachineAction::Stop => stop(),
        MachineAction::Start | MachineAction::DismissFailure(_) => Vec::new(),
    })
}

/// `Installing` with progress over `duration`, then a Pending Version of `to`.
fn install(to: Version, duration: Duration) -> Vec<(Duration, Change)> {
    let steps = INSTALL_STEPS;
    let started = {
        let to = to.clone();
        Box::new(move |update: &mut MachineUpdate| {
            facts(update).operation = Some(Operation::Installing {
                version: to,
                received: 0,
                total: INSTALL_BYTES,
            });
        }) as Change
    };
    let mut changes = vec![(Duration::ZERO, started)];
    let step = duration / INSTALL_STEPS as u32;
    for n in 1..=steps {
        let to = to.clone();
        changes.push((
            step,
            Box::new(move |update: &mut MachineUpdate| {
                facts(update).operation = Some(Operation::Installing {
                    version: to,
                    received: INSTALL_BYTES * n / steps,
                    total: INSTALL_BYTES,
                });
            }),
        ));
    }
    changes.push((
        INSTALL_STEP,
        Box::new(move |update: &mut MachineUpdate| {
            let facts = facts(update);
            facts.operation = None;
            facts.pending = Some(pending(to, super::super::now()));
            update.push = None;
        }),
    ));
    changes
}

/// `Restarting`, then `running` equals `to` after 3 seconds.
fn restart_into(to: Version) -> Vec<(Duration, Change)> {
    let restarting = {
        let to = to.clone();
        Box::new(move |update: &mut MachineUpdate| {
            facts(update).operation = Some(Operation::Restarting { to });
        }) as Change
    };
    let restarted = Box::new(move |update: &mut MachineUpdate| {
        let facts = facts(update);
        facts.running = to;
        facts.pending = None;
        facts.operation = None;
        facts.sessions = 0;
        update.link = MachineLink::Connected;
        update.capabilities = Capabilities::fake(&Capability::ALL);
        update.push = None;
    }) as Change;
    vec![(Duration::ZERO, restarting), (RESTART, restarted)]
}

/// `Checking` for 1 second, then `Succeeded`. A Machine that could not confirm its restart
/// confirms it.
fn check_again() -> Vec<(Duration, Change)> {
    let checking = Box::new(|update: &mut MachineUpdate| {
        facts(update).operation = Some(Operation::Checking);
    }) as Change;
    let checked = Box::new(|update: &mut MachineUpdate| {
        let now = super::super::now();
        let link = std::mem::replace(&mut update.link, MachineLink::Connected);
        let facts = facts(update);
        facts.operation = None;
        facts.check = UpdateCheck::Succeeded { at: now };
        if let MachineLink::Unconfirmed { expected, .. } = link {
            facts.running = expected;
            facts.pending = None;
            update.capabilities = Capabilities::fake(&Capability::ALL);
        } else {
            update.link = link;
        }
    }) as Change;
    vec![(Duration::ZERO, checking), (CHECK, checked)]
}

/// `Stopping`, then `Stopped`.
fn stop() -> Vec<(Duration, Change)> {
    let stopping = Box::new(|update: &mut MachineUpdate| {
        facts(update).operation = Some(Operation::Stopping);
    }) as Change;
    let stopped = Box::new(|update: &mut MachineUpdate| {
        let facts = facts(update);
        facts.operation = None;
        facts.sessions = 0;
        update.link = MachineLink::Stopped;
        update.capabilities = disconnected();
    }) as Change;
    vec![(Duration::ZERO, stopping), (SETTLE, stopped)]
}

fn facts(update: &mut MachineUpdate) -> &mut UpdateFacts {
    update
        .facts
        .as_mut()
        .expect("a preview Machine changes only while it has facts")
}

fn version(text: &str) -> Version {
    Version::parse(text).expect("preview versions are valid")
}

fn disconnected() -> Capabilities {
    Capabilities::new(
        Err(Unavailable::NotConnected),
        Err(Unavailable::NotConnected),
    )
}

fn pending(to: Version, installed_at: u64) -> PendingVersion {
    PendingVersion {
        version: to,
        protocols: Protocols::CURRENT,
        breaking: false,
        installed_at,
        overdue_since: None,
        stranded: Vec::new(),
    }
}

fn connected_facts(platform: Platform, sessions: u32, now: u64) -> UpdateFacts {
    UpdateFacts {
        revision: 1,
        platform,
        running: version(NEWEST),
        pending: None,
        sessions,
        operation: None,
        check: UpdateCheck::Succeeded { at: now - 3600 },
        skipped: None,
        last_failure: None,
    }
}

fn machine(
    machine: MachineId,
    name: &'static str,
    local: bool,
    platform: Platform,
    sessions: u32,
    now: u64,
) -> MachineUpdate {
    MachineUpdate {
        machine,
        name: name.into(),
        local,
        link: MachineLink::Connected,
        facts: Some(connected_facts(platform, sessions, now)),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
    }
}

/// This Mac or This Computer, penguin and build-box, as `scenario` begins.
fn machines(scenario: Scenario, now: u64) -> Vec<MachineUpdate> {
    let (local_name, local_platform) = if cfg!(target_os = "macos") {
        ("This Mac", Platform::MacosArm64)
    } else {
        ("This Computer", Platform::LinuxX86_64)
    };
    let mut this = machine(THIS_MACHINE, local_name, true, local_platform, 3, now);
    let mut penguin = machine(PENGUIN, "penguin", false, Platform::LinuxX86_64, 0, now);
    let mut build_box = machine(BUILD_BOX, "build-box", false, Platform::LinuxX86_64, 4, now);
    let waiting = |update: &mut MachineUpdate, installed_at: u64| {
        let facts = facts(update);
        facts.running = version(PREVIOUS);
        facts.pending = Some(pending(version(NEWEST), installed_at));
    };
    let failed = |update: &mut MachineUpdate, kind: FailureKind| {
        let facts = facts(update);
        facts.running = version(PREVIOUS);
        facts.last_failure = Some(UpdateFailure {
            id: 1,
            at: now - 600,
            version: version(NEWEST),
            kind,
        });
        if kind == FailureKind::Start(StartFailure::NotReady) {
            facts.skipped = Some(version(NEWEST));
        }
    };
    let unconfirmed = |update: &mut MachineUpdate| {
        waiting(update, now - 3600);
        update.link = MachineLink::Unconfirmed {
            expected: version(NEWEST),
            since: now - 120,
        };
        update.capabilities = disconnected();
    };
    let failing = |update: &mut MachineUpdate, reason| {
        let facts = facts(update);
        facts.running = version(PREVIOUS);
        facts.check = UpdateCheck::Failing {
            since: now - 2 * DAY,
            last_success: Some(now - 3 * DAY),
            reason,
        };
    };
    match scenario {
        Scenario::UpToDate => {}
        Scenario::UpdateReady => {
            waiting(&mut penguin, now - 3600);
            facts(&mut penguin).sessions = 3;
        }
        Scenario::SeveralReady => {
            for update in [&mut this, &mut penguin, &mut build_box] {
                waiting(update, now - 3600);
            }
        }
        Scenario::Installing => {
            let facts = facts(&mut penguin);
            facts.running = version(PREVIOUS);
            facts.operation = Some(Operation::Installing {
                version: version(NEWEST),
                received: 0,
                total: INSTALL_BYTES,
            });
        }
        Scenario::Restarting => {
            waiting(&mut penguin, now - 3600);
            facts(&mut penguin).operation = Some(Operation::Restarting {
                to: version(NEWEST),
            });
        }
        Scenario::RestartOverdue => {
            let installed_at = now - 2 * DAY;
            waiting(&mut penguin, installed_at);
            let facts = facts(&mut penguin);
            facts.sessions = 3;
            if let Some(pending) = &mut facts.pending {
                pending.overdue_since = Some(installed_at + DAY);
            }
        }
        Scenario::Breaking => {
            facts(&mut penguin).pending = Some(PendingVersion {
                protocols: Protocols {
                    work: ProtocolVersion {
                        major: protocol::WORK.major + 1,
                        minor: 0,
                    },
                    ..Protocols::CURRENT
                },
                breaking: true,
                stranded: vec![
                    StrandedDevice {
                        pairing: [0xa1; 32],
                        name: "Sadik\u{2019}s iPhone".into(),
                    },
                    StrandedDevice {
                        pairing: [0xa2; 32],
                        name: "old-mini".into(),
                    },
                ],
                ..pending(version("1.0.0"), now - 3600)
            });
        }
        Scenario::CoreTooOld => {
            facts(&mut penguin).running = version("0.6.0");
            penguin.link = MachineLink::Incompatible(Incompatible::ListenerTooOld);
            penguin.capabilities =
                Capabilities::new(Err(Unavailable::CoreTooOld), Ok(protocol::SERVICE));
            penguin.push = Some(version(NEWEST));
        }
        Scenario::DesktopTooOld => {
            penguin.link = MachineLink::Incompatible(Incompatible::CallerTooOld);
            penguin.capabilities =
                Capabilities::new(Err(Unavailable::ClientTooOld), Ok(protocol::SERVICE));
        }
        Scenario::CantCheck => {
            failing(&mut penguin, CheckFailure::Network);
            penguin.push = Some(version(NEWEST));
        }
        Scenario::CantCheckProxy => {
            failing(&mut penguin, CheckFailure::Proxy);
        }
        Scenario::InstallFailedDiskFull => {
            failed(&mut penguin, FailureKind::Install(InstallFailure::DiskFull));
        }
        Scenario::InstallFailedDownload => {
            failed(&mut penguin, FailureKind::Install(InstallFailure::Download));
        }
        Scenario::InstallFailedVerification => {
            failed(
                &mut penguin,
                FailureKind::Install(InstallFailure::Verification),
            );
        }
        Scenario::UpdateFailed => {
            failed(&mut penguin, FailureKind::Start(StartFailure::NotReady));
        }
        Scenario::CantConfirm => unconfirmed(&mut build_box),
        Scenario::RestartAllPartial => {
            failed(&mut penguin, FailureKind::Start(StartFailure::NotReady));
            unconfirmed(&mut build_box);
        }
        Scenario::Stopped => {
            facts(&mut this).sessions = 0;
            this.link = MachineLink::Stopped;
            this.capabilities = disconnected();
        }
        Scenario::Offline => {
            build_box.link = MachineLink::Offline { since: now - 600 };
            build_box.facts = None;
            build_box.capabilities = disconnected();
        }
        Scenario::CapabilityMissing => {
            waiting(&mut penguin, now - 3600);
            penguin.capabilities = Capabilities::fake(&without(&[
                Capability::SwapPanes,
                Capability::MovePaneToTab,
            ]));
        }
        Scenario::UninstallUnavailable => {
            penguin.capabilities = Capabilities::fake(&without(&[Capability::Uninstall]));
        }
    }
    vec![this, penguin, build_box]
}

fn without(missing: &[Capability]) -> Vec<Capability> {
    Capability::ALL
        .into_iter()
        .filter(|capability| !missing.contains(capability))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(
        scenario: Scenario,
    ) -> (
        PreviewMachines,
        async_channel::Receiver<MachineUpdatesEvent>,
        Vec<MachineUpdate>,
    ) {
        let preview = PreviewMachines::new(scenario);
        let (sender, receiver) = async_channel::unbounded();
        preview.start(sender).unwrap();
        let Ok(MachineUpdatesEvent::Machines(machines)) = receiver.recv_blocking() else {
            panic!("the first event lists every Machine");
        };
        (preview, receiver, machines)
    }

    /// The last `Changed` before `action` finishes on `machine`, and its outcome.
    fn finish(
        receiver: &async_channel::Receiver<MachineUpdatesEvent>,
        machine: MachineId,
    ) -> (Option<MachineUpdate>, Result<(), ActionFailure>) {
        let mut last = None;
        loop {
            match receiver.recv_blocking().unwrap() {
                MachineUpdatesEvent::Changed(update) if update.machine == machine => {
                    last = Some(*update);
                }
                MachineUpdatesEvent::Finished {
                    machine: finished,
                    outcome,
                    ..
                } if finished == machine => return (last, outcome),
                _ => {}
            }
        }
    }

    #[test]
    fn every_scenario_is_named_once() {
        for scenario in Scenario::ALL {
            assert_eq!(Scenario::parse(scenario.name()), Some(scenario));
        }
        assert_eq!(Scenario::parse("update-ready "), None);
    }

    #[test]
    fn every_scenario_starts_with_three_machines_whose_facts_are_valid() {
        for scenario in Scenario::ALL {
            let (_preview, _events, machines) = started(scenario);
            assert_eq!(machines.len(), 3, "{}", scenario.name());
            for facts in machines.iter().filter_map(|update| update.facts.as_ref()) {
                assert_eq!(facts.validate(), Ok(()), "{}", scenario.name());
            }
        }
    }

    #[test]
    fn install_then_restart_moves_a_machine_to_the_pushed_version() {
        let (preview, events, _) = started(Scenario::CantCheck);
        preview.perform(PENGUIN, MachineAction::Install);
        let (installed, outcome) = finish(&events, PENGUIN);
        assert_eq!(outcome, Ok(()));
        let installed = installed.unwrap();
        let facts = installed.facts.as_ref().unwrap();
        assert_eq!(facts.pending.as_ref().unwrap().version, version(NEWEST));
        assert_eq!(facts.operation, None);
        assert_eq!(installed.push, None);

        preview.perform(PENGUIN, MachineAction::Restart);
        let (restarted, outcome) = finish(&events, PENGUIN);
        assert_eq!(outcome, Ok(()));
        let facts = restarted.unwrap().facts.unwrap();
        assert_eq!(facts.running, version(NEWEST));
        assert_eq!(facts.pending, None);
    }

    #[test]
    fn refused_actions_finish_with_their_reason() {
        let (preview, events, _) = started(Scenario::Offline);
        preview.perform(BUILD_BOX, MachineAction::Restart);
        assert_eq!(
            finish(&events, BUILD_BOX),
            (None, Err(ActionFailure::Unreachable))
        );

        let (preview, events, _) = started(Scenario::Installing);
        preview.perform(PENGUIN, MachineAction::Restart);
        assert_eq!(finish(&events, PENGUIN).1, Err(ActionFailure::Busy));

        let (preview, events, _) = started(Scenario::DesktopTooOld);
        preview.perform(PENGUIN, MachineAction::Stop);
        assert_eq!(finish(&events, PENGUIN), (None, Err(ActionFailure::Failed)));
    }
}
