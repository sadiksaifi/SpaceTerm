//! Daily notifications about overdue updates. The desktop's own reminder follows its update
//! history: one day after the release, then daily, and once when the next launch starts to
//! require it. A Machine whose breaking Pending Version waits a day or more is named once a day
//! with its session count. A Pending Version that is not breaking waits only on open Terminal
//! Sessions, because the Core restarts into it by itself once none is open, so it sends no
//! notification and keeps only its menu row. Clicking a notification emits
//! [`ReminderActivated`], and the window layer runs what the item's control does now.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};

use super::UpdateService;
use super::machines::MachineId;
use super::machines::store::MachineUpdates;
use super::policy::DAY;
use super::presentation::{self, ItemKey, Reminder};
use spaceterm_core::update::facts::{PendingVersion, Version};

/// The Pending Version `machine` waits with now.
fn pending(machine: MachineId, cx: &App) -> Option<PendingVersion> {
    let store = MachineUpdates::global(cx)?;
    let store = store.read(cx);
    let update = store
        .machines()
        .iter()
        .find(|update| update.machine == machine)?;
    update.facts.as_ref()?.pending.clone()
}

/// How often the schedule is read again when nothing else changes.
const RECHECK: Duration = Duration::from_secs(60 * 60);

/// Shows operating-system notifications.
pub(crate) trait UpdateReminderAdapter {
    /// Sends the `key` of each notification the person clicks to `activated`. Called once,
    /// before the first notification.
    fn activations(&self, activated: async_channel::Sender<String>);
    /// Shows `title` and `body`, replacing an earlier notification with the same `key`.
    fn notify(&self, key: &str, title: &str, body: &str);
}

/// The person clicked the notification about this item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReminderActivated(pub(crate) ItemKey);

pub(crate) struct UpdateReminders {
    adapter: Rc<dyn UpdateReminderAdapter>,
    /// When each Machine's overdue restart was last named, and the Pending Version it named.
    named: HashMap<MachineId, (Version, u64)>,
    _subscriptions: Vec<Subscription>,
    _recheck: Task<()>,
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
                while let Ok(key) = clicks.recv().await {
                    let Some(key) = parse_key(&key) else { continue };
                    if this
                        .update(cx, |_, cx| cx.emit(ReminderActivated(key)))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .detach();
            let recheck = cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(RECHECK).await;
                    if this.update(cx, |this, cx| this.deliver(cx)).is_err() {
                        return;
                    }
                }
            });
            Self {
                adapter,
                named: HashMap::new(),
                _subscriptions: subscriptions,
                _recheck: recheck,
            }
        });
        cx.set_global(InstalledReminders { scheduler: entity });
    }

    pub(crate) fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<InstalledReminders>()
            .map(|installed| installed.scheduler.clone())
    }

    fn deliver(&mut self, cx: &mut Context<Self>) {
        let reminders = presentation::current(cx).reminders();
        let now = super::now();
        let desktop_due = cx
            .try_global::<UpdateService>()
            .is_some_and(|service| service.0.read(cx).reminder().is_some());
        self.forget_resolved(cx);
        for reminder in reminders {
            let due = match reminder.key {
                ItemKey::Desktop => desktop_due,
                ItemKey::Machine(machine) => {
                    pending(machine, cx).is_some_and(|pending| pending.breaking)
                        && self
                            .named
                            .get(&machine)
                            .is_none_or(|(_, last)| now.saturating_sub(*last) >= DAY)
                }
            };
            if !due {
                continue;
            }
            self.show(&reminder);
            match reminder.key {
                ItemKey::Desktop => {
                    if let Some(service) = cx.try_global::<UpdateService>().cloned() {
                        // Records the reminder, so the next one waits a day.
                        cx.defer(move |cx| {
                            service
                                .0
                                .update(cx, |updates, cx| updates.dismiss_reminder(cx));
                        });
                    }
                }
                ItemKey::Machine(machine) => {
                    if let Some(pending) = pending(machine, cx) {
                        self.named.insert(machine, (pending.version, now));
                    }
                }
            }
        }
    }

    /// Forgets a Machine's last reminder once it is gone or its Pending Version resolved or
    /// changed. A temporary state, such as Offline, Checking or Restarting, keeps it, so a
    /// Machine that comes back overdue is named again only a day after the last time.
    fn forget_resolved(&mut self, cx: &App) {
        let Some(store) = MachineUpdates::global(cx) else {
            self.named.clear();
            return;
        };
        let machines = store.read(cx).machines();
        self.named.retain(|machine, (named, _)| {
            let Some(update) = machines.iter().find(|update| update.machine == *machine) else {
                return false;
            };
            match &update.facts {
                // No facts while unreachable: nothing says the update resolved.
                None => true,
                Some(facts) => facts
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.version == *named),
            }
        });
    }

    fn show(&self, reminder: &Reminder) {
        self.adapter
            .notify(&key_text(reminder.key), &reminder.title, &reminder.body);
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;

    /// Records each notification instead of showing it.
    #[derive(Default)]
    pub(crate) struct RecordingReminders {
        pub(crate) shown: RefCell<Vec<(String, String, String)>>,
        activated: RefCell<Option<async_channel::Sender<String>>>,
    }

    impl RecordingReminders {
        /// Clicks the notification shown with `key`, as the person would.
        pub(crate) fn click(&self, key: &str) {
            self.activated
                .borrow()
                .as_ref()
                .expect("the scheduler started")
                .try_send(key.to_owned())
                .expect("the scheduler listens");
        }
    }

    impl super::UpdateReminderAdapter for RecordingReminders {
        fn activations(&self, activated: async_channel::Sender<String>) {
            *self.activated.borrow_mut() = Some(activated);
        }

        fn notify(&self, key: &str, title: &str, body: &str) {
            self.shown
                .borrow_mut()
                .push((key.to_owned(), title.to_owned(), body.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::RecordingReminders;
    use super::*;
    use crate::updates::machines::testing::ScriptedMachines;
    use crate::updates::machines::{MachineLink, MachineUpdate, MachineUpdatesEvent};
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
    };

    fn overdue_penguin(sessions: u32) -> MachineUpdate {
        let mut update = waiting_penguin(sessions);
        update
            .facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .breaking = true;
        update
    }

    /// Penguin's Pending Version, which is not breaking, has waited a day on open sessions.
    fn waiting_penguin(sessions: u32) -> MachineUpdate {
        let now = crate::updates::now();
        MachineUpdate {
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
                    installed_at: now - DAY - 60,
                    overdue_since: Some(now - 60),
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
        }
    }

    #[gpui::test]
    fn an_overdue_restart_is_named_once_a_day_with_its_sessions(cx: &mut gpui::TestAppContext) {
        let machines = Rc::new(ScriptedMachines::default());
        let reminders = Rc::new(RecordingReminders::default());
        cx.update(|cx| {
            MachineUpdates::install(machines.clone(), cx);
            UpdateReminders::install(reminders.clone(), cx);
        });
        machines.emit(MachineUpdatesEvent::Machines(vec![overdue_penguin(3)]));
        cx.run_until_parked();
        machines.emit(MachineUpdatesEvent::Changed(Box::new(overdue_penguin(3))));
        cx.run_until_parked();

        let shown = reminders.shown.borrow().clone();
        assert_eq!(shown.len(), 1, "one notification a day");
        assert_eq!(shown[0].1, "Restart overdue on penguin");
        assert_eq!(
            shown[0].2,
            "SpaceTerm Core 0.9.0 has waited 1 day. Restarting closes 3 running sessions."
        );
    }

    /// A Pending Version that is not breaking waits only on open Terminal Sessions. It keeps
    /// its menu row but sends no notification.
    #[gpui::test]
    fn an_update_that_waits_on_open_sessions_sends_no_notification(cx: &mut gpui::TestAppContext) {
        let machines = Rc::new(ScriptedMachines::default());
        let reminders = Rc::new(RecordingReminders::default());
        cx.update(|cx| {
            MachineUpdates::install(machines.clone(), cx);
            UpdateReminders::install(reminders.clone(), cx);
        });
        machines.emit(MachineUpdatesEvent::Machines(vec![waiting_penguin(3)]));
        cx.run_until_parked();
        machines.emit(MachineUpdatesEvent::Changed(Box::new(waiting_penguin(3))));
        cx.run_until_parked();

        assert!(reminders.shown.borrow().is_empty());
        let rows = cx.read(|cx| presentation::current(cx).extra_rows());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tone, presentation::Tone::Problem);
    }

    #[gpui::test]
    fn a_temporary_state_keeps_the_daily_interval_and_a_new_version_names_again(
        cx: &mut gpui::TestAppContext,
    ) {
        let machines = Rc::new(ScriptedMachines::default());
        let reminders = Rc::new(RecordingReminders::default());
        cx.update(|cx| {
            MachineUpdates::install(machines.clone(), cx);
            UpdateReminders::install(reminders.clone(), cx);
        });
        let changed = |update: MachineUpdate| {
            machines.emit(MachineUpdatesEvent::Changed(Box::new(update)));
        };
        machines.emit(MachineUpdatesEvent::Machines(vec![overdue_penguin(3)]));
        cx.run_until_parked();

        let mut offline = overdue_penguin(3);
        offline.link = MachineLink::Offline {
            since: crate::updates::now(),
        };
        offline.facts = None;
        changed(offline);
        cx.run_until_parked();
        changed(overdue_penguin(3));
        cx.run_until_parked();
        let mut checking = overdue_penguin(3);
        checking.facts.as_mut().unwrap().operation =
            Some(spaceterm_core::update::facts::Operation::Checking);
        changed(checking);
        cx.run_until_parked();
        changed(overdue_penguin(3));
        cx.run_until_parked();
        assert_eq!(
            reminders.shown.borrow().len(),
            1,
            "Offline and Checking keep today's reminder"
        );

        let mut newer = overdue_penguin(3);
        newer
            .facts
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .version = Version::parse("0.9.1").unwrap();
        changed(newer);
        cx.run_until_parked();
        assert_eq!(
            reminders.shown.borrow().len(),
            2,
            "a replaced Pending Version starts its own reminders"
        );
    }
}
