use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui::{App, Task};
use std::time::{Duration, Instant};

use crate::terminal::attention::AttentionEffects;

const DOCK_ATTENTION_RATE_LIMIT: Duration = Duration::from_secs(1);
const NOTIFICATION_AGGREGATION: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AttentionPaneId(u64);

impl AttentionPaneId {
    #[cfg(test)]
    const fn test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DockAttentionSchedule {
    token: u64,
    deadline: Instant,
}

impl DockAttentionSchedule {
    pub(crate) fn delay_from(self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

/// Native request identity remains private to the effect adapter.
pub(crate) trait DockAttentionDriver {
    fn request(&mut self) -> Result<(), AttentionFailure>;
    fn cancel(&mut self);
}

pub(crate) trait AudioBell {
    fn play(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttentionFailure {
    Unavailable,
    Denied,
    DeliveryFailed,
}

impl DockAttentionDriver for Box<dyn DockAttentionDriver> {
    fn request(&mut self) -> Result<(), AttentionFailure> {
        self.as_mut().request()
    }
    fn cancel(&mut self) {
        self.as_mut().cancel();
    }
}

struct DockAttentionCoordinator<D: DockAttentionDriver> {
    driver: D,
    owners: BTreeSet<AttentionPaneId>,
    outstanding: bool,
    last_request: Option<Instant>,
    last_failure: Option<AttentionFailure>,
    application_active: bool,
    scheduled: Option<DockAttentionSchedule>,
    next_schedule_token: u64,
}

impl<D: DockAttentionDriver> DockAttentionCoordinator<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            owners: BTreeSet::new(),
            outstanding: false,
            last_request: None,
            last_failure: None,
            application_active: false,
            scheduled: None,
            next_schedule_token: 0,
        }
    }

    fn request(&mut self, pane: AttentionPaneId, now: Instant) -> Option<DockAttentionSchedule> {
        self.owners.insert(pane);
        self.reconcile(now)
    }

    fn reconcile(&mut self, now: Instant) -> Option<DockAttentionSchedule> {
        if self.application_active || self.owners.is_empty() || self.outstanding {
            self.invalidate_schedule();
            return None;
        }

        if let Some(last) = self.last_request
            && now.saturating_duration_since(last) < DOCK_ATTENTION_RATE_LIMIT
        {
            return self.schedule_at(last + DOCK_ATTENTION_RATE_LIMIT);
        }

        self.invalidate_schedule();
        let result = self.driver.request();
        self.outstanding = result.is_ok();
        self.last_failure = result.err();
        self.last_request = Some(now);
        None
    }

    fn clear(&mut self, pane: AttentionPaneId) {
        if !self.owners.remove(&pane) || !self.owners.is_empty() {
            return;
        }
        self.invalidate_schedule();
        self.cancel_outstanding();
    }

    fn set_application_active(
        &mut self,
        active: bool,
        now: Instant,
    ) -> Option<DockAttentionSchedule> {
        self.application_active = active;
        if active {
            self.invalidate_schedule();
            self.cancel_outstanding();
            None
        } else {
            self.reconcile(now)
        }
    }

    fn reconcile_scheduled(
        &mut self,
        schedule: DockAttentionSchedule,
        now: Instant,
    ) -> Option<DockAttentionSchedule> {
        if self.scheduled != Some(schedule) {
            return None;
        }
        self.scheduled = None;
        self.reconcile(now)
    }

    fn schedule_at(&mut self, deadline: Instant) -> Option<DockAttentionSchedule> {
        if self.scheduled.is_some() {
            return None;
        }
        self.next_schedule_token = self.next_schedule_token.wrapping_add(1);
        let schedule = DockAttentionSchedule {
            token: self.next_schedule_token,
            deadline,
        };
        self.scheduled = Some(schedule);
        Some(schedule)
    }

    fn invalidate_schedule(&mut self) {
        self.scheduled = None;
    }

    fn cancel_outstanding(&mut self) {
        if std::mem::take(&mut self.outstanding) {
            self.driver.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NotificationDelivery {
    pub(crate) aggregate_count: u32,
}

pub(crate) trait NotificationDriver {
    fn deliver(&mut self, delivery: NotificationDelivery);
    fn clear(&mut self);
}

impl NotificationDriver for Box<dyn NotificationDriver> {
    fn deliver(&mut self, delivery: NotificationDelivery) {
        self.as_mut().deliver(delivery);
    }
    fn clear(&mut self) {
        self.as_mut().clear();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NotificationSchedule {
    token: u64,
    deadline: Instant,
}

impl NotificationSchedule {
    fn delay_from(self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

struct NotificationCoordinator<D: NotificationDriver> {
    driver: D,
    pending_by_pane: BTreeMap<AttentionPaneId, u32>,
    delivered_by_pane: BTreeMap<AttentionPaneId, u32>,
    scheduled: Option<NotificationSchedule>,
    next_schedule_token: u64,
    application_active: bool,
}

impl<D: NotificationDriver> NotificationCoordinator<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            pending_by_pane: BTreeMap::new(),
            delivered_by_pane: BTreeMap::new(),
            scheduled: None,
            next_schedule_token: 0,
            application_active: false,
        }
    }

    fn request(
        &mut self,
        pane: AttentionPaneId,
        application_active: bool,
        now: Instant,
    ) -> Option<NotificationSchedule> {
        if application_active {
            let needs_clear = !self.application_active
                || !self.pending_by_pane.is_empty()
                || !self.delivered_by_pane.is_empty()
                || self.scheduled.is_some();
            self.application_active = true;
            self.cancel_all();
            self.delivered_by_pane.clear();
            if needs_clear {
                self.driver.clear();
            }
            return None;
        }
        self.application_active = false;

        self.pending_by_pane
            .entry(pane)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        if self.scheduled.is_some() {
            return None;
        }
        self.next_schedule_token = self.next_schedule_token.wrapping_add(1);
        let schedule = NotificationSchedule {
            token: self.next_schedule_token,
            deadline: now + NOTIFICATION_AGGREGATION,
        };
        self.scheduled = Some(schedule);
        Some(schedule)
    }

    fn clear(&mut self, pane: AttentionPaneId) {
        self.pending_by_pane.remove(&pane);
        let delivered = self.delivered_by_pane.remove(&pane).is_some();
        if self.pending_by_pane.is_empty() {
            self.invalidate_schedule();
        }
        if delivered {
            let remaining_count = self
                .delivered_by_pane
                .values()
                .copied()
                .fold(0_u32, u32::saturating_add);
            if remaining_count == 0 {
                self.driver.clear();
            } else {
                self.driver.deliver(NotificationDelivery {
                    aggregate_count: remaining_count,
                });
            }
        }
    }

    fn set_application_active(&mut self, active: bool) {
        let activated = active && !self.application_active;
        self.application_active = active;
        if activated {
            self.cancel_all();
            self.delivered_by_pane.clear();
            self.driver.clear();
        }
    }

    fn reconcile_scheduled(
        &mut self,
        schedule: NotificationSchedule,
        now: Instant,
        application_active: bool,
    ) -> Option<NotificationSchedule> {
        if self.scheduled != Some(schedule) {
            return None;
        }
        if application_active {
            self.set_application_active(true);
            return None;
        }
        self.application_active = false;
        if now < schedule.deadline {
            return Some(schedule);
        }
        self.scheduled = None;
        for (pane, count) in std::mem::take(&mut self.pending_by_pane) {
            self.delivered_by_pane
                .entry(pane)
                .and_modify(|delivered| *delivered = delivered.saturating_add(count))
                .or_insert(count);
        }
        let aggregate_count = self
            .delivered_by_pane
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add);
        if aggregate_count > 0 {
            self.driver
                .deliver(NotificationDelivery { aggregate_count });
        }
        None
    }

    fn cancel_all(&mut self) {
        self.pending_by_pane.clear();
        self.invalidate_schedule();
    }

    fn invalidate_schedule(&mut self) {
        self.scheduled = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttentionSchedule {
    Dock(DockAttentionSchedule),
    Notification(NotificationSchedule),
}

impl AttentionSchedule {
    pub(crate) fn delay_from(self, now: Instant) -> Duration {
        match self {
            Self::Dock(schedule) => schedule.delay_from(now),
            Self::Notification(schedule) => schedule.delay_from(now),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AttentionSchedules {
    dock: Option<DockAttentionSchedule>,
    notification: Option<NotificationSchedule>,
}

impl AttentionSchedules {
    pub(crate) fn into_array(self) -> [Option<AttentionSchedule>; 2] {
        [
            self.dock.map(AttentionSchedule::Dock),
            self.notification.map(AttentionSchedule::Notification),
        ]
    }
}

/// Shared application ownership. Panes retain registrations, never native resources.
#[derive(Clone)]
pub(crate) struct AttentionRuntime(Rc<RefCell<Runtime>>);

struct Runtime {
    audio: Box<dyn AudioBell>,
    dock: DockAttentionCoordinator<Box<dyn DockAttentionDriver>>,
    notifications: NotificationCoordinator<Box<dyn NotificationDriver>>,
    activity: Rc<dyn crate::platform::application_activity::ApplicationActivity>,
    panes: BTreeSet<AttentionPaneId>,
    next_pane: u64,
    tasks: Vec<(AttentionSchedule, Task<()>)>,
}

impl Runtime {
    fn cancel_invalid_tasks(&mut self) {
        let dock = self.dock.scheduled;
        let notification = self.notifications.scheduled;
        self.tasks.retain(|(schedule, _)| match schedule {
            AttentionSchedule::Dock(schedule) => dock == Some(*schedule),
            AttentionSchedule::Notification(schedule) => notification == Some(*schedule),
        });
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.tasks.clear();
        self.dock.cancel_outstanding();
        self.notifications.driver.clear();
    }
}

impl AttentionRuntime {
    pub(crate) fn new(
        audio: Box<dyn AudioBell>,
        dock: Box<dyn DockAttentionDriver>,
        notifications: Box<dyn NotificationDriver>,
        activity: Rc<dyn crate::platform::application_activity::ApplicationActivity>,
    ) -> Self {
        Self(Rc::new(RefCell::new(Runtime {
            audio,
            dock: DockAttentionCoordinator::new(dock),
            notifications: NotificationCoordinator::new(notifications),
            activity,
            panes: BTreeSet::new(),
            next_pane: 0,
            tasks: Vec::new(),
        })))
    }

    #[cfg(test)]
    pub(crate) fn testing(
        activity: Rc<dyn crate::platform::application_activity::ApplicationActivity>,
    ) -> Self {
        struct Silent;
        impl AudioBell for Silent {
            fn play(&mut self) {}
        }
        impl DockAttentionDriver for Silent {
            fn request(&mut self) -> Result<(), AttentionFailure> {
                Ok(())
            }
            fn cancel(&mut self) {}
        }
        impl NotificationDriver for Silent {
            fn deliver(&mut self, _: NotificationDelivery) {}
            fn clear(&mut self) {}
        }
        Self::new(
            Box::new(Silent),
            Box::new(Silent),
            Box::new(Silent),
            activity,
        )
    }

    pub(crate) fn register_pane(&self) -> AttentionPaneId {
        let mut runtime = self.0.borrow_mut();
        // Exhausting this identity space requires creating more Panes than the process can retain.
        runtime.next_pane = runtime
            .next_pane
            .checked_add(1)
            .expect("attention identity exhausted");
        let pane = AttentionPaneId(runtime.next_pane);
        runtime.panes.insert(pane);
        pane
    }

    pub(crate) fn remove_pane(&self, pane: AttentionPaneId) {
        let mut runtime = self.0.borrow_mut();
        if !runtime.panes.remove(&pane) {
            return;
        }
        runtime.dock.clear(pane);
        runtime.notifications.clear(pane);
        runtime.cancel_invalid_tasks();
    }

    pub(crate) fn apply(
        &self,
        pane: AttentionPaneId,
        effects: AttentionEffects,
        now: Instant,
    ) -> AttentionSchedules {
        let mut runtime = self.0.borrow_mut();
        if !runtime.panes.contains(&pane) {
            return AttentionSchedules::default();
        }
        let mut schedules = AttentionSchedules::default();
        if effects.audio_bell {
            runtime.audio.play();
        }
        if effects.request_dock_attention {
            schedules.dock = runtime.dock.request(pane, now);
        }
        if effects.cancel_dock_attention {
            runtime.dock.clear(pane);
            schedules.dock = None;
        }
        if effects.notification.is_some() {
            let active = runtime.notifications.application_active;
            schedules.notification = runtime.notifications.request(pane, active, now);
        }
        if effects.cancel_notification {
            runtime.notifications.clear(pane);
            schedules.notification = None;
        }
        runtime.cancel_invalid_tasks();
        schedules
    }

    pub(crate) fn update_application_activation(
        &self,
        active: bool,
        now: Instant,
    ) -> AttentionSchedules {
        let mut runtime = self.0.borrow_mut();
        let dock = runtime.dock.set_application_active(active, now);
        runtime.notifications.set_application_active(active);
        runtime.cancel_invalid_tasks();
        AttentionSchedules {
            dock,
            notification: None,
        }
    }

    pub(crate) fn reconcile_scheduled(
        &self,
        schedule: AttentionSchedule,
        now: Instant,
    ) -> AttentionSchedules {
        let mut runtime = self.0.borrow_mut();
        let schedules = match schedule {
            AttentionSchedule::Dock(schedule) => AttentionSchedules {
                dock: runtime.dock.reconcile_scheduled(schedule, now),
                notification: None,
            },
            AttentionSchedule::Notification(schedule) => {
                let active = runtime.notifications.application_active;
                AttentionSchedules {
                    dock: None,
                    notification: runtime
                        .notifications
                        .reconcile_scheduled(schedule, now, active),
                }
            }
        };
        runtime.cancel_invalid_tasks();
        schedules
    }

    /// Timers belong to application-wide demand, so retiring one Pane cannot strand another.
    /// Each callback holds only weak ownership and validates its exact generation before effects.
    pub(crate) fn schedule(&self, schedules: AttentionSchedules, cx: &mut App) {
        for schedule in schedules.into_array().into_iter().flatten() {
            let mut runtime = self.0.borrow_mut();
            let current = match schedule {
                AttentionSchedule::Dock(schedule) => runtime.dock.scheduled == Some(schedule),
                AttentionSchedule::Notification(schedule) => {
                    runtime.notifications.scheduled == Some(schedule)
                }
            };
            if !current
                || runtime
                    .tasks
                    .iter()
                    .any(|(current, _)| *current == schedule)
            {
                continue;
            }
            let weak = Rc::downgrade(&self.0);
            let task = cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(schedule.delay_from(Instant::now()))
                    .await;
                let _ = cx.update(|cx| {
                    let Some(runtime) = weak.upgrade() else {
                        return;
                    };
                    let handle = AttentionRuntime(runtime);
                    let activity = Rc::clone(&handle.0.borrow().activity);
                    // Remove this completed timer before any new schedule can reuse its slot.
                    handle
                        .0
                        .borrow_mut()
                        .tasks
                        .retain(|(current, _)| *current != schedule);
                    let activation = handle
                        .update_application_activation(activity.is_active(cx), Instant::now());
                    let next = handle.reconcile_scheduled(schedule, Instant::now());
                    handle.schedule(activation, cx);
                    handle.schedule(next, cx);
                });
            });
            runtime.tasks.push((schedule, task));
        }
    }
}

#[cfg(test)]
impl AttentionRuntime {
    pub(crate) fn same_coordinator(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct RecordingNotificationDriver {
        deliveries: Vec<NotificationDelivery>,
        clears: usize,
    }

    impl NotificationDriver for RecordingNotificationDriver {
        fn deliver(&mut self, delivery: NotificationDelivery) {
            self.deliveries.push(delivery);
        }

        fn clear(&mut self) {
            self.clears += 1;
        }
    }

    #[derive(Default)]
    struct RecordingDockDriver {
        next_request: u64,
        requests: usize,
        cancellations: Vec<u64>,
        current: Option<u64>,
    }

    impl DockAttentionDriver for RecordingDockDriver {
        fn request(&mut self) -> Result<(), AttentionFailure> {
            self.next_request += 1;
            self.requests += 1;
            self.current = Some(self.next_request);
            Ok(())
        }

        fn cancel(&mut self) {
            self.cancellations
                .push(self.current.take().expect("live request"));
        }
    }

    #[test]
    fn multiple_panes_share_one_dock_request_until_the_last_owner_clears() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = DockAttentionCoordinator::new(RecordingDockDriver::default());

        let _ = coordinator.request(first, epoch);
        let _ = coordinator.request(second, epoch + Duration::from_millis(100));
        coordinator.clear(first);
        let after_first_clear = (
            coordinator.driver.requests,
            coordinator.driver.cancellations.clone(),
            coordinator.outstanding,
        );
        coordinator.clear(second);

        assert_eq!(after_first_clear, (1, Vec::new(), true));
        assert_eq!(
            (
                coordinator.driver.requests,
                coordinator.driver.cancellations,
                coordinator.outstanding,
            ),
            (1, vec![1], false)
        );
    }

    #[test]
    fn dock_requests_are_rate_limited_across_panes_after_cancellation() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = DockAttentionCoordinator::new(RecordingDockDriver::default());

        let _ = coordinator.request(first, epoch);
        coordinator.clear(first);
        let schedule = coordinator
            .request(second, epoch + Duration::from_millis(999))
            .expect("rate-limited demand should schedule one retry");
        let duplicate = coordinator.request(second, epoch + Duration::from_millis(999));
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline);

        assert_eq!(
            (
                coordinator.driver.requests,
                coordinator.driver.cancellations,
                coordinator.outstanding,
                duplicate,
            ),
            (2, vec![1], true, None)
        );
    }

    #[test]
    fn deactivation_reissues_suspended_owner_demand_without_another_terminal_event() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = DockAttentionCoordinator::new(RecordingDockDriver::default());

        let _ = coordinator.request(pane, epoch);
        let _ = coordinator.set_application_active(true, epoch + Duration::from_millis(100));
        let schedule = coordinator.set_application_active(false, epoch + DOCK_ATTENTION_RATE_LIMIT);

        assert_eq!(
            (
                coordinator.driver.requests,
                coordinator.driver.cancellations,
                coordinator.owners,
                coordinator.outstanding,
                schedule,
            ),
            (
                2,
                vec![1],
                BTreeSet::from([AttentionPaneId::test(1)]),
                true,
                None,
            )
        );
    }

    #[test]
    fn rate_limited_deactivation_retains_owner_demand_for_later_reconciliation() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = DockAttentionCoordinator::new(RecordingDockDriver::default());

        let _ = coordinator.request(pane, epoch);
        let _ = coordinator.set_application_active(true, epoch + Duration::from_millis(100));
        let schedule = coordinator
            .set_application_active(false, epoch + Duration::from_millis(999))
            .expect("rate-limited deactivation should schedule one retry");
        let rate_limited = (
            coordinator.driver.requests,
            coordinator.owners.clone(),
            coordinator.outstanding,
            coordinator.scheduled,
            schedule.delay_from(epoch + Duration::from_millis(999)),
        );
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline);

        assert_eq!(
            rate_limited,
            (
                1,
                BTreeSet::from([AttentionPaneId::test(1)]),
                false,
                Some(schedule),
                Duration::from_millis(1),
            )
        );
        assert_eq!(
            (coordinator.driver.requests, coordinator.outstanding),
            (2, true)
        );
    }

    #[test]
    fn activation_and_last_owner_clear_cancel_a_scheduled_retry_token() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = DockAttentionCoordinator::new(RecordingDockDriver::default());

        let _ = coordinator.request(pane, epoch);
        coordinator.cancel_outstanding();
        let activation_schedule = coordinator
            .request(pane, epoch + Duration::from_millis(999))
            .expect("rate-limited owner demand should schedule one retry");
        let _ = coordinator.set_application_active(true, epoch + Duration::from_millis(999));
        let after_activation =
            coordinator.reconcile_scheduled(activation_schedule, activation_schedule.deadline);
        let requests_after_activation = coordinator.driver.requests;

        let _ = coordinator.set_application_active(false, activation_schedule.deadline);
        coordinator.cancel_outstanding();
        let clear_schedule = coordinator
            .request(
                pane,
                activation_schedule.deadline + Duration::from_millis(999),
            )
            .expect("remaining owner demand should schedule one retry");
        coordinator.clear(pane);
        let after_clear = coordinator.reconcile_scheduled(clear_schedule, clear_schedule.deadline);

        assert_eq!(
            (
                after_activation,
                requests_after_activation,
                after_clear,
                coordinator.driver.requests,
                coordinator.owners,
            ),
            (None, 1, None, 2, BTreeSet::new())
        );
    }

    #[test]
    fn notification_events_flush_as_one_batch_at_the_original_five_second_deadline() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());

        let schedule = coordinator
            .request(pane, false, epoch)
            .expect("first inactive event should schedule the batch deadline");
        let second_schedule = coordinator.request(pane, false, epoch + Duration::from_secs(2));
        let before_deadline = coordinator.driver.deliveries.clone();
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        assert_eq!(schedule.deadline, epoch + NOTIFICATION_AGGREGATION);
        assert_eq!(second_schedule, None);
        assert_eq!(before_deadline, Vec::new());
        assert_eq!(
            coordinator.driver.deliveries,
            vec![NotificationDelivery { aggregate_count: 2 }]
        );
    }

    #[test]
    fn notification_reconciliation_before_deadline_keeps_the_same_schedule() {
        let epoch = Instant::now();
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(AttentionPaneId::test(1), false, epoch)
            .expect("first inactive event should schedule the batch deadline");

        let retry = coordinator.reconcile_scheduled(
            schedule,
            schedule.deadline - Duration::from_millis(1),
            false,
        );

        assert_eq!(retry, Some(schedule));
        assert!(coordinator.driver.deliveries.is_empty());
    }

    #[test]
    fn repeated_or_stale_notification_schedule_cannot_deliver_twice() {
        let epoch = Instant::now();
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(AttentionPaneId::test(1), false, epoch)
            .expect("first inactive event should schedule the batch deadline");

        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        assert_eq!(coordinator.driver.deliveries.len(), 1);
    }

    #[test]
    fn notification_batch_is_application_wide_across_panes() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());

        let schedule = coordinator
            .request(first, false, epoch)
            .expect("first Pane should schedule the shared batch");
        let duplicate = coordinator.request(second, false, epoch + Duration::from_secs(2));
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        assert_eq!(duplicate, None);
        assert_eq!(
            coordinator.driver.deliveries,
            vec![NotificationDelivery { aggregate_count: 2 }]
        );
    }

    #[test]
    fn successive_notification_batches_preserve_all_unread_owners() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let first_batch = coordinator.request(first, false, epoch).unwrap();
        coordinator.reconcile_scheduled(first_batch, first_batch.deadline, false);
        let second_batch = coordinator
            .request(second, false, epoch + Duration::from_secs(6))
            .unwrap();
        coordinator.request(first, false, epoch + Duration::from_secs(7));
        coordinator.reconcile_scheduled(second_batch, second_batch.deadline, false);
        coordinator.clear(second);
        assert_eq!(
            coordinator.driver.deliveries,
            [
                NotificationDelivery { aggregate_count: 1 },
                NotificationDelivery { aggregate_count: 3 },
                NotificationDelivery { aggregate_count: 2 },
            ]
        );
        assert_eq!(coordinator.driver.clears, 0);
        coordinator.clear(first);
        assert_eq!(coordinator.driver.clears, 1);
    }

    #[test]
    fn activation_at_flush_cancels_delivery_without_a_replacement_event() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(pane, false, epoch)
            .expect("inactive attention should schedule one batch");

        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, true);

        assert!(coordinator.driver.deliveries.is_empty());
        assert!(coordinator.pending_by_pane.is_empty());
        assert_eq!(coordinator.scheduled, None);
    }

    #[test]
    fn focus_input_or_teardown_removes_only_the_owning_pane_contribution() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(first, false, epoch)
            .expect("first Pane should schedule the shared batch");
        let _ = coordinator.request(first, false, epoch + Duration::from_secs(1));
        let _ = coordinator.request(second, false, epoch + Duration::from_secs(2));

        coordinator.clear(first);
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        assert_eq!(coordinator.driver.clears, 0);
        assert_eq!(
            coordinator.driver.deliveries,
            vec![NotificationDelivery { aggregate_count: 1 }]
        );
    }

    #[test]
    fn clearing_the_last_pane_invalidates_the_notification_timer() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(pane, false, epoch)
            .expect("inactive attention should schedule one batch");

        coordinator.clear(pane);
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        assert!(coordinator.driver.deliveries.is_empty());
        assert_eq!(coordinator.scheduled, None);
    }

    #[test]
    fn focus_input_or_teardown_clears_an_already_delivered_native_notification() {
        let epoch = Instant::now();
        let pane = AttentionPaneId::test(1);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(pane, false, epoch)
            .expect("inactive attention should schedule one batch");
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        coordinator.clear(pane);

        assert_eq!(coordinator.driver.deliveries.len(), 1);
        assert_eq!(coordinator.driver.clears, 1);
        assert!(coordinator.delivered_by_pane.is_empty());
    }

    #[test]
    fn clearing_one_delivered_pane_replaces_the_row_with_remaining_demand() {
        let epoch = Instant::now();
        let first = AttentionPaneId::test(1);
        let second = AttentionPaneId::test(2);
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());
        let schedule = coordinator
            .request(first, false, epoch)
            .expect("first Pane should schedule the shared batch");
        let _ = coordinator.request(first, false, epoch + Duration::from_secs(1));
        let _ = coordinator.request(second, false, epoch + Duration::from_secs(2));
        let _ = coordinator.reconcile_scheduled(schedule, schedule.deadline, false);

        coordinator.clear(first);

        assert_eq!(
            coordinator.driver.deliveries,
            vec![
                NotificationDelivery { aggregate_count: 3 },
                NotificationDelivery { aggregate_count: 1 },
            ]
        );
        assert_eq!(coordinator.driver.clears, 0);
        assert_eq!(coordinator.delivered_by_pane, BTreeMap::from([(second, 1)]));
    }

    #[test]
    fn active_application_rejects_notification_demand_without_scheduling() {
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());

        let schedule = coordinator.request(AttentionPaneId::test(1), true, Instant::now());

        assert_eq!(schedule, None);
        assert!(coordinator.pending_by_pane.is_empty());
        assert!(coordinator.driver.deliveries.is_empty());
    }

    #[test]
    fn repeated_active_observations_do_not_repeat_native_clear_side_effects() {
        let mut coordinator = NotificationCoordinator::new(RecordingNotificationDriver::default());

        coordinator.set_application_active(true);
        coordinator.set_application_active(true);
        coordinator.set_application_active(false);
        coordinator.set_application_active(true);

        assert_eq!(coordinator.driver.clears, 2);
    }
    #[derive(Default)]
    struct RecordedEffects {
        bells: usize,
        dock_requests: usize,
        dock_cancels: usize,
        deliveries: Vec<u32>,
        notification_clears: usize,
    }

    struct RecordingEffects(Rc<RefCell<RecordedEffects>>);
    impl AudioBell for RecordingEffects {
        fn play(&mut self) {
            self.0.borrow_mut().bells += 1;
        }
    }
    impl DockAttentionDriver for RecordingEffects {
        fn request(&mut self) -> Result<(), AttentionFailure> {
            self.0.borrow_mut().dock_requests += 1;
            Ok(())
        }
        fn cancel(&mut self) {
            self.0.borrow_mut().dock_cancels += 1;
        }
    }
    impl NotificationDriver for RecordingEffects {
        fn deliver(&mut self, delivery: NotificationDelivery) {
            self.0
                .borrow_mut()
                .deliveries
                .push(delivery.aggregate_count);
        }
        fn clear(&mut self) {
            self.0.borrow_mut().notification_clears += 1;
        }
    }
    struct InactiveApplication;
    impl crate::platform::application_activity::ApplicationActivity for InactiveApplication {
        fn is_active(&self, _: &App) -> bool {
            false
        }
    }

    fn recording_runtime() -> (AttentionRuntime, Rc<RefCell<RecordedEffects>>) {
        let effects = Rc::new(RefCell::new(RecordedEffects::default()));
        let runtime = AttentionRuntime::new(
            Box::new(RecordingEffects(Rc::clone(&effects))),
            Box::new(RecordingEffects(Rc::clone(&effects))),
            Box::new(RecordingEffects(Rc::clone(&effects))),
            Rc::new(InactiveApplication),
        );
        (runtime, effects)
    }

    fn bell_effects() -> AttentionEffects {
        AttentionEffects {
            audio_bell: true,
            request_dock_attention: true,
            notification: Some(crate::terminal::attention::AttentionEvent::Bell),
            ..AttentionEffects::default()
        }
    }

    #[test]
    fn removed_registration_cannot_request_effects_or_target_successor() {
        let epoch = Instant::now();
        let (runtime, effects) = recording_runtime();
        let old = runtime.register_pane();
        let pending = runtime.apply(old, bell_effects(), epoch);
        runtime.remove_pane(old);
        let successor = runtime.register_pane();
        let stale = runtime.apply(old, bell_effects(), epoch);
        for schedule in pending.into_array().into_iter().flatten() {
            let _ = runtime.reconcile_scheduled(schedule, epoch + Duration::from_secs(5));
        }
        assert_ne!(old, successor);
        assert_eq!(stale, AttentionSchedules::default());
        let effects = effects.borrow();
        assert_eq!(
            (effects.bells, effects.dock_requests, effects.dock_cancels),
            (1, 1, 1)
        );
        assert!(effects.deliveries.is_empty());
    }

    #[test]
    fn closing_one_window_keeps_other_window_attention_and_deadline() {
        let epoch = Instant::now();
        let (first_window, effects) = recording_runtime();
        let second_window = first_window.clone();
        let first = first_window.register_pane();
        let second = second_window.register_pane();
        let pending = first_window.apply(first, bell_effects(), epoch);
        let _ = second_window.apply(second, bell_effects(), epoch + Duration::from_secs(2));
        first_window.remove_pane(first);
        drop(first_window);
        for schedule in pending.into_array().into_iter().flatten() {
            let _ = second_window.reconcile_scheduled(schedule, epoch + Duration::from_secs(5));
        }
        let effects = effects.borrow();
        assert_eq!((effects.dock_requests, effects.dock_cancels), (1, 0));
        assert_eq!(effects.deliveries, vec![1]);
    }

    #[test]
    fn activation_cancels_all_delayed_notifications_and_native_dock_effects() {
        let epoch = Instant::now();
        let (runtime, effects) = recording_runtime();
        let pane = runtime.register_pane();
        let pending = runtime.apply(pane, bell_effects(), epoch);
        let _ = runtime.update_application_activation(true, epoch);
        for schedule in pending.into_array().into_iter().flatten() {
            let _ = runtime.reconcile_scheduled(schedule, epoch + Duration::from_secs(5));
        }
        let effects = effects.borrow();
        assert_eq!(
            (
                effects.dock_requests,
                effects.dock_cancels,
                effects.notification_clears
            ),
            (1, 1, 1)
        );
        assert!(effects.deliveries.is_empty());
    }

    #[test]
    fn failed_dock_request_retains_no_native_cancellation_authority() {
        struct Unavailable;
        impl DockAttentionDriver for Unavailable {
            fn request(&mut self) -> Result<(), AttentionFailure> {
                Err(AttentionFailure::Unavailable)
            }
            fn cancel(&mut self) {
                panic!("failed request has no cancellation authority");
            }
        }
        let mut coordinator = DockAttentionCoordinator::new(Unavailable);
        let pane = AttentionPaneId::test(1);
        let _ = coordinator.request(pane, Instant::now());
        coordinator.clear(pane);
        assert!(!coordinator.outstanding);
        assert_eq!(
            coordinator.last_failure,
            Some(AttentionFailure::Unavailable)
        );
    }

    #[gpui::test]
    fn shared_timer_survives_origin_removal_and_is_cancelled_by_last_owner(
        cx: &mut gpui::TestAppContext,
    ) {
        let epoch = Instant::now();
        let (runtime, _) = recording_runtime();
        let first = runtime.register_pane();
        let second = runtime.register_pane();
        let schedules = runtime.apply(first, bell_effects(), epoch);
        let _ = runtime.apply(second, bell_effects(), epoch);
        cx.update(|cx| runtime.schedule(schedules, cx));
        runtime.remove_pane(first);
        assert_eq!(runtime.0.borrow().tasks.len(), 1);
        runtime.remove_pane(second);
        assert!(runtime.0.borrow().tasks.is_empty());
        // A caller racing removal cannot reinstall an invalidated task.
        cx.update(|cx| runtime.schedule(schedules, cx));
        assert!(runtime.0.borrow().tasks.is_empty());
    }

    #[gpui::test]
    fn shared_timer_delivers_once_after_origin_is_retired(cx: &mut gpui::TestAppContext) {
        let epoch = Instant::now() - NOTIFICATION_AGGREGATION;
        let (runtime, effects) = recording_runtime();
        let first = runtime.register_pane();
        let second = runtime.register_pane();
        let schedules = runtime.apply(first, bell_effects(), epoch);
        let _ = runtime.apply(second, bell_effects(), epoch);
        cx.update(|cx| runtime.schedule(schedules, cx));
        runtime.remove_pane(first);
        cx.run_until_parked();
        assert_eq!(effects.borrow().deliveries, vec![1]);
        assert!(runtime.0.borrow().tasks.is_empty());
        cx.run_until_parked();
        assert_eq!(effects.borrow().deliveries, vec![1]);
    }
}
