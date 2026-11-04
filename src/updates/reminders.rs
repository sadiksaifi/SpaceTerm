//! Daily notifications about overdue updates. The desktop's own reminder follows its update
//! history: one day after the release, then daily, and once when the next launch starts to
//! require it. A Machine whose Pending Version waits a day or more is named once a day with its
//! session count; SpaceTerm never restarts it while sessions are open. Clicking a notification
//! emits [`ReminderActivated`], and the window layer runs what the item's control does now.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, Subscription, Task};

use super::UpdateService;
use super::machines::store::MachineUpdates;
use super::policy::DAY;
use super::presentation::{self, ItemKey, Reminder};

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
    /// When each Machine's overdue restart was last named.
    named: HashMap<ItemKey, u64>,
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
        self.named
            .retain(|key, _| reminders.iter().any(|reminder| reminder.key == *key));
        for reminder in reminders {
            let due = match reminder.key {
                ItemKey::Desktop => desktop_due,
                ItemKey::Machine(_) => self
                    .named
                    .get(&reminder.key)
                    .is_none_or(|last| now.saturating_sub(*last) >= DAY),
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
                ItemKey::Machine(_) => {
                    self.named.insert(reminder.key, now);
                }
            }
        }
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
}
