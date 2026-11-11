//! Update Reminders: operating-system notifications about a waiting update. SpaceTerm and every
//! SpaceTerm Core follow one cadence by ladder stage: none at Offer, one a day at Ask, one every
//! four hours at Insist, for any waiting Pending Version, breaking or not. Reminders post only
//! while a SpaceTerm window is active, and each one replaces the previous one for the same
//! update. SpaceTerm's own cadence stays with its update history; a Machine's lives in memory for
//! this run.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};

use super::machines::store::MachineUpdates;
use super::machines::{MachineId, MachineLink, MachineUpdate};
use super::presentation::ItemKey;
use super::{UpdateService, UpdateState, copy, ladder};
use spaceterm_core::update::facts::PendingVersion;

/// The longest the schedule waits before it reads the clock again, so a changed clock is noticed.
const RECHECK: Duration = Duration::from_secs(60 * 60);

/// A clicked notification's key, with the activation token the notification server passed for
/// the click, if any.
pub(crate) type ReminderClick = (String, Option<String>);

/// Shows operating-system notifications.
pub(crate) trait UpdateReminderAdapter {
    /// Sends the `key` of each notification the person clicks to `activated`, with the activation
    /// token the notification server passed for the click, if any. Called once, before the first
    /// notification.
    fn activations(&self, activated: async_channel::Sender<ReminderClick>);
    /// Shows `title` and `body`, replacing an earlier notification with the same `key`.
    fn notify(&self, key: &str, title: &str, body: &str);
    /// Seconds since the Unix epoch, the clock Machine reminders are timed by.
    fn now(&self) -> u64 {
        super::now()
    }
}

/// The person clicked the notification about this item. `token` lets the window that answers
/// the click come forward where the desktop requires one, such as Wayland.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReminderActivated {
    pub(crate) key: ItemKey,
    pub(crate) token: Option<String>,
}

pub(crate) struct UpdateReminders {
    adapter: Rc<dyn UpdateReminderAdapter>,
    /// When each Machine's waiting update was last named. A newer Pending Version that replaces
    /// a waiting one inherits its clock, so it keeps this cadence too.
    named: HashMap<MachineId, u64>,
    _subscriptions: Vec<Subscription>,
    _schedule: Task<()>,
}

/// Keeps the scheduler alive for the application's life.
struct InstalledReminders {
    scheduler: Entity<UpdateReminders>,
}
impl Global for InstalledReminders {}

impl EventEmitter<ReminderActivated> for UpdateReminders {}

/// The notification key of `key`: "desktop", or the Machine ID in hex.
fn key_text(key: ItemKey) -> String {
    match key {
        ItemKey::Desktop => "desktop".to_owned(),
        ItemKey::Machine(machine) => machine.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

fn parse_key(text: &str) -> Option<ItemKey> {
    if text == "desktop" {
        return Some(ItemKey::Desktop);
    }
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut machine = [0u8; 32];
    for (slot, pair) in machine.iter_mut().zip(bytes.chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(ItemKey::Machine(machine))
}

/// The Pending Version a Machine waits with, from facts that are current: a Machine that is
/// offline, stopped or unconfirmed gets no reminder from its last facts.
fn waiting(update: &MachineUpdate) -> Option<&PendingVersion> {
    if !matches!(
        update.link,
        MachineLink::Connected | MachineLink::Incompatible(_)
    ) {
        return None;
    }
    update.facts.as_ref()?.pending.as_ref()
}

impl UpdateReminders {
    /// Starts the schedule. The update services must already be installed.
    pub(crate) fn install(adapter: Rc<dyn UpdateReminderAdapter>, cx: &mut App) {
        let entity = cx.new(|cx: &mut Context<Self>| {
            let mut subscriptions = Vec::new();
            if let Some(service) = cx.try_global::<UpdateService>().cloned() {
                subscriptions.push(cx.observe(&service.0, |this, _, cx| this.deliver(cx)));
            }
            if let Some(machines) = MachineUpdates::global(cx) {
                subscriptions.push(cx.observe(&machines, |this, _, cx| this.deliver(cx)));
            }
            let (activated, clicks) = async_channel::unbounded();
            adapter.activations(activated);
            cx.spawn(async move |this, cx| {
                while let Ok((key, token)) = clicks.recv().await {
                    let Some(key) = parse_key(&key) else { continue };
                    if this
                        .update(cx, |_, cx| cx.emit(ReminderActivated { key, token }))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .detach();
            // One timer runs to the next Machine reminder, or at most RECHECK. SpaceTerm's own
            // reminders come from its update service, which this observes.
            let schedule = cx.spawn(async move |this, cx| {
                loop {
                    let Ok(wait) = this.update(cx, |this, cx| {
                        this.deliver(cx);
                        let now = this.adapter.now();
                        this.next_due(now, cx)
                            .map_or(RECHECK, |due| Duration::from_secs(due - now).min(RECHECK))
                    }) else {
                        return;
                    };
                    cx.background_executor().timer(wait).await;
                }
            });
            Self {
                adapter,
                named: HashMap::new(),
                _subscriptions: subscriptions,
                _schedule: schedule,
            }
        });
        cx.set_global(InstalledReminders { scheduler: entity });
    }

    pub(crate) fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<InstalledReminders>()
            .map(|installed| installed.scheduler.clone())
    }

    fn deliver(&mut self, cx: &mut Context<Self>) {
        self.forget_resolved(cx);
        // Reminders post only while the person is looking at SpaceTerm.
        if cx.active_window().is_none() {
            return;
        }
        self.deliver_app(cx);
        let now = self.adapter.now();
        let Some(store) = MachineUpdates::global(cx) else {
            return;
        };
        let due: Vec<(MachineId, String, String)> = store
            .read(cx)
            .machines()
            .iter()
            .filter_map(|update| {
                let pending = waiting(update)?;
                let stage = ladder::stage(pending.installed_at, now);
                let last = self.named.get(&update.machine).copied();
                if !ladder::reminder_due(stage, last, now) {
                    return None;
                }
                let sessions = update.facts.as_ref().map_or(0, |facts| facts.sessions);
                let (title, body) = copy::reminders::core(
                    &pending.version.to_string(),
                    &update.name,
                    sessions,
                    now.saturating_sub(pending.installed_at),
                );
                Some((update.machine, title, body))
            })
            .collect();
        for (machine, title, body) in due {
            self.adapter
                .notify(&key_text(ItemKey::Machine(machine)), &title, &body);
            self.named.insert(machine, now);
        }
    }

    /// Shows SpaceTerm's own reminder when its update service has one due, and records it, so
    /// the next one waits for the stage's cadence.
    fn deliver_app(&self, cx: &mut Context<Self>) {
        let Some(service) = cx.try_global::<UpdateService>().cloned() else {
            return;
        };
        let updates = service.0.read(cx);
        if updates.reminder().is_none() {
            return;
        }
        let Some(version) = updates.pending_version() else {
            return;
        };
        let downloaded = matches!(updates.state(), UpdateState::Ready { .. });
        let waited = updates
            .waiting_since()
            .map_or(0, |since| updates.now().saturating_sub(since));
        let (title, body) = copy::reminders::app(version, downloaded, waited);
        self.adapter
            .notify(&key_text(ItemKey::Desktop), &title, &body);
        cx.defer(move |cx| {
            service
                .0
                .update(cx, |updates, cx| updates.dismiss_reminder(cx));
        });
    }

    /// The next time after `now` at which a Machine reminder becomes due: a waiting update
    /// reaching Ask or Insist, or its stage's cadence passing since it was last named.
    fn next_due(&self, now: u64, cx: &App) -> Option<u64> {
        let store = MachineUpdates::global(cx)?;
        store
            .read(cx)
            .machines()
            .iter()
            .filter_map(|update| {
                let pending = waiting(update)?;
                let since = pending.installed_at;
                let stage = ladder::stage(since, now);
                let reminder = self
                    .named
                    .get(&update.machine)
                    .zip(ladder::cadence(stage))
                    .map(|(last, cadence)| last.saturating_add(cadence));
                [
                    Some(since.saturating_add(ladder::ASK_AFTER)),
                    Some(since.saturating_add(ladder::INSIST_AFTER)),
                    reminder,
                ]
                .into_iter()
                .flatten()
                .filter(|due| *due > now)
                .min()
            })
            .min()
    }

    /// Forgets a Machine's last reminder once it is gone or no Pending Version waits on it. A
    /// temporary state without facts, such as Offline, keeps it, so a Machine that comes back
    /// with the same wait keeps its cadence.
    fn forget_resolved(&mut self, cx: &App) {
        let Some(store) = MachineUpdates::global(cx) else {
            self.named.clear();
            return;
        };
        let machines = store.read(cx).machines();
        self.named.retain(|machine, _| {
            let Some(update) = machines.iter().find(|update| update.machine == *machine) else {
                return false;
            };
            match &update.facts {
                // No facts while unreachable: nothing says the update resolved.
                None => true,
                Some(facts) => facts.pending.is_some(),
            }
        });
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;

    /// Records each notification instead of showing it.
    #[derive(Default)]
    pub(crate) struct RecordingReminders {
        pub(crate) shown: RefCell<Vec<(String, String, String)>>,
        activated: RefCell<Option<async_channel::Sender<super::ReminderClick>>>,
        /// Replaces the system clock, in seconds since the Unix epoch.
        pub(crate) clock: std::cell::Cell<Option<u64>>,
    }

    impl RecordingReminders {
        /// Clicks the notification shown with `key`, as the person would.
        pub(crate) fn click(&self, key: &str) {
            self.activated
                .borrow()
                .as_ref()
                .expect("the scheduler started")
                .try_send((key.to_owned(), None))
                .expect("the scheduler listens");
        }
    }

    impl super::UpdateReminderAdapter for RecordingReminders {
        fn activations(&self, activated: async_channel::Sender<super::ReminderClick>) {
            *self.activated.borrow_mut() = Some(activated);
        }

        fn notify(&self, key: &str, title: &str, body: &str) {
            self.shown
                .borrow_mut()
                .push((key.to_owned(), title.to_owned(), body.to_owned()));
        }

        fn now(&self) -> u64 {
            self.clock.get().unwrap_or_else(crate::updates::now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::RecordingReminders;
    use super::*;
    use crate::updates::ladder::{ASK_AFTER, ASK_CADENCE, INSIST_AFTER, INSIST_CADENCE};
    use crate::updates::machines::testing::ScriptedMachines;
    use crate::updates::machines::{MachineLink, MachineUpdate, MachineUpdatesEvent};
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
    };

    /// When penguin's Pending Version was downloaded.
    const DOWNLOADED: u64 = 1_700_000_000;
    const PENGUIN: &str = "0202020202020202020202020202020202020202020202020202020202020202";

    /// Penguin waits with 0.9.0, which is not breaking, on `sessions` open Terminal Sessions.
    fn waiting_penguin(sessions: u32) -> MachineUpdate {
        MachineUpdate {
            detailed: None,
            machine: [2; 32],
            name: "penguin".into(),
            local: false,
            link: MachineLink::Connected,
            facts: Some(UpdateFacts {
                revision: 1,
                platform: Platform::LinuxX86_64,
                running: Version::parse("0.8.0").unwrap(),
                pending: Some(PendingVersion {
                    version: Version::parse("0.9.0").unwrap(),
                    protocols: Protocols::CURRENT,
                    breaking: false,
                    installed_at: DOWNLOADED,
                    // The desktop ignores the Core's own overdue mark.
                    overdue_since: Some(DOWNLOADED),
                    stranded: Vec::new(),
                }),
                sessions,
                operation: None,
                check: UpdateCheck::Never,
                skipped: None,
                last_failure: None,
            }),
            capabilities: Capabilities::fake(&Capability::ALL),
            push: None,
            bundled: None,
            failed_push: None,
            asked: None,
        }
    }

    struct Fixture {
        machines: Rc<ScriptedMachines>,
        reminders: Rc<RecordingReminders>,
        window: gpui::WindowHandle<gpui::EmptyView>,
    }

    impl Fixture {
        fn new(cx: &mut gpui::TestAppContext) -> Self {
            let machines = Rc::new(ScriptedMachines::default());
            let reminders = Rc::new(RecordingReminders::default());
            reminders.clock.set(Some(DOWNLOADED));
            let window = cx.add_window(|_, _| gpui::EmptyView);
            window
                .update(cx, |_, window, _| window.activate_window())
                .unwrap();
            cx.update(|cx| {
                MachineUpdates::install(machines.clone(), cx);
                UpdateReminders::install(reminders.clone(), cx);
            });
            Self {
                machines,
                reminders,
                window,
            }
        }

        /// Sets the clock to `at` and reports `update` again, as a new fact would.
        fn at(&self, at: u64, update: MachineUpdate, cx: &mut gpui::TestAppContext) {
            self.reminders.clock.set(Some(at));
            self.machines
                .emit(MachineUpdatesEvent::Changed(Box::new(update)));
            cx.run_until_parked();
        }

        fn shown(&self) -> usize {
            self.reminders.shown.borrow().len()
        }
    }

    #[gpui::test]
    fn a_waiting_core_update_reminds_daily_from_ask_and_every_four_hours_from_insist(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = Fixture::new(cx);
        fixture
            .machines
            .emit(MachineUpdatesEvent::Machines(vec![waiting_penguin(3)]));
        cx.run_until_parked();
        fixture.at(DOWNLOADED + ASK_AFTER - 1, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 0, "Offer sends no reminder");

        let ask = DOWNLOADED + ASK_AFTER;
        fixture.at(ask, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 1);
        assert_eq!(
            fixture.reminders.shown.borrow()[0],
            (
                PENGUIN.to_owned(),
                "SpaceTerm Core 0.9.0 is downloaded on penguin".to_owned(),
                "An update has waited 2 days. Restarting ends 3 Terminal Sessions.".to_owned()
            )
        );
        fixture.at(ask + ASK_CADENCE - 1, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 1, "one a day at Ask");
        fixture.at(ask + ASK_CADENCE, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 2);

        let insist = DOWNLOADED + INSIST_AFTER;
        fixture.at(insist, waiting_penguin(0), cx);
        assert_eq!(fixture.shown(), 3);
        assert_eq!(
            fixture.reminders.shown.borrow()[2].2,
            "An update has waited 7 days.",
            "no Terminal Session is open"
        );
        fixture.at(insist + INSIST_CADENCE - 1, waiting_penguin(0), cx);
        assert_eq!(fixture.shown(), 3);
        fixture.at(insist + INSIST_CADENCE, waiting_penguin(0), cx);
        assert_eq!(fixture.shown(), 4, "one every four hours at Insist");
        assert!(
            fixture
                .reminders
                .shown
                .borrow()
                .iter()
                .all(|(key, _, _)| key == PENGUIN),
            "each reminder replaces the last one for the same update"
        );
    }

    #[gpui::test]
    fn the_schedule_wakes_for_the_next_core_reminder_by_itself(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture::new(cx);
        let executor = cx.executor();
        let began = executor.now();
        fixture
            .machines
            .emit(MachineUpdatesEvent::Machines(vec![waiting_penguin(2)]));
        cx.run_until_parked();
        // The test clock follows the executor's, which only timers move.
        let ask = DOWNLOADED + ASK_AFTER;
        while fixture.shown() == 0 {
            assert!(cx.dispatcher.scheduler().advance_clock_to_next_timer());
            fixture
                .reminders
                .clock
                .set(Some(DOWNLOADED + (executor.now() - began).as_secs()));
            cx.run_until_parked();
            assert!(executor.now() - began <= Duration::from_secs(ASK_AFTER + 3600));
        }
        assert!(fixture.reminders.clock.get().unwrap() >= ask);
    }

    #[gpui::test]
    fn a_breaking_update_and_one_waiting_on_open_sessions_remind_alike(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = Fixture::new(cx);
        let mut breaking = waiting_penguin(1);
        breaking.machine = [3; 32];
        breaking.name = "build-box".into();
        breaking
            .facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .breaking = true;
        fixture.machines.emit(MachineUpdatesEvent::Machines(vec![
            waiting_penguin(2),
            breaking.clone(),
        ]));
        cx.run_until_parked();
        fixture.at(DOWNLOADED + ASK_AFTER, breaking, cx);
        let titles: Vec<String> = fixture
            .reminders
            .shown
            .borrow()
            .iter()
            .map(|(_, title, _)| title.clone())
            .collect();
        assert_eq!(
            titles,
            [
                "SpaceTerm Core 0.9.0 is downloaded on penguin",
                "SpaceTerm Core 0.9.0 is downloaded on build-box"
            ]
        );
    }

    #[gpui::test]
    fn reminders_wait_for_an_active_window(cx: &mut gpui::TestAppContext) {
        let fixture = Fixture::new(cx);
        gpui::VisualTestContext::from_window(fixture.window.into(), cx).deactivate_window();
        fixture
            .machines
            .emit(MachineUpdatesEvent::Machines(vec![waiting_penguin(3)]));
        cx.run_until_parked();
        fixture.at(DOWNLOADED + ASK_AFTER, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 0);
        fixture
            .window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        fixture.at(DOWNLOADED + ASK_AFTER + 60, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 1);
    }

    #[gpui::test]
    fn offline_keeps_the_cadence_a_newer_version_inherits_it_and_a_restart_ends_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = Fixture::new(cx);
        fixture
            .machines
            .emit(MachineUpdatesEvent::Machines(vec![waiting_penguin(3)]));
        cx.run_until_parked();
        let ask = DOWNLOADED + ASK_AFTER;
        fixture.at(ask, waiting_penguin(3), cx);
        assert_eq!(fixture.shown(), 1);

        let mut offline = waiting_penguin(3);
        offline.link = MachineLink::Offline { since: ask };
        offline.facts = None;
        fixture.at(ask + 60, offline, cx);
        fixture.at(ask + 120, waiting_penguin(3), cx);
        let mut stale = waiting_penguin(3);
        stale.link = MachineLink::Offline { since: ask };
        fixture.at(ask + ASK_CADENCE, stale, cx);
        assert_eq!(
            fixture.shown(),
            1,
            "an offline Machine's last facts remind of nothing"
        );

        let mut newer = waiting_penguin(3);
        newer
            .facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .version = Version::parse("0.9.1").unwrap();
        fixture.at(ask + 180, newer.clone(), cx);
        assert_eq!(fixture.shown(), 1, "a replacing version keeps the cadence");
        fixture.at(ask + ASK_CADENCE + 60, newer, cx);
        assert_eq!(fixture.shown(), 2);
        assert_eq!(
            fixture.reminders.shown.borrow()[1].1,
            "SpaceTerm Core 0.9.1 is downloaded on penguin"
        );

        let mut restarted = waiting_penguin(3);
        restarted.facts.as_mut().unwrap().pending = None;
        fixture.at(ask + ASK_CADENCE + 120, restarted, cx);
        let mut next = waiting_penguin(3);
        next.facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .installed_at = ask + ASK_CADENCE + 180;
        fixture.at(ask + ASK_CADENCE + 240, next, cx);
        assert_eq!(fixture.shown(), 2, "the next update starts at Offer");
    }
}
