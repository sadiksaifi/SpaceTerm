use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cocoa::appkit::NSApplication;
use cocoa::base::nil;
use cocoa::foundation::NSInteger;
use objc::{msg_send, sel, sel_impl};

use crate::terminal::attention::AttentionEffects;
#[cfg(test)]
use crate::terminal::attention::AttentionEvent;

const DOCK_ATTENTION_RATE_LIMIT: Duration = Duration::from_secs(1);
const NOTIFICATION_AGGREGATION: Duration = Duration::from_secs(5);

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

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

trait DockAttentionDriver {
    type Request: Copy;

    fn request(&mut self) -> Self::Request;
    fn cancel(&mut self, request: Self::Request);
}

struct AppKitDockAttention;

impl DockAttentionDriver for AppKitDockAttention {
    type Request = NSInteger;

    fn request(&mut self) -> Self::Request {
        // SAFETY: This runs from GPUI's main thread and sends documented NSApplication messages.
        unsafe {
            let application = NSApplication::sharedApplication(nil);
            msg_send![application, requestUserAttention: 10_u64]
        }
    }

    fn cancel(&mut self, request: Self::Request) {
        // SAFETY: The request identity came from NSApplication and cancellation stays on UI.
        unsafe {
            let application = NSApplication::sharedApplication(nil);
            let _: () = msg_send![application, cancelUserAttentionRequest: request];
        }
    }
}

struct DockAttentionCoordinator<D: DockAttentionDriver> {
    driver: D,
    owners: BTreeSet<AttentionPaneId>,
    outstanding: Option<D::Request>,
    last_request: Option<Instant>,
    application_active: bool,
    scheduled: Option<DockAttentionSchedule>,
    next_schedule_token: u64,
}

impl<D: DockAttentionDriver> DockAttentionCoordinator<D> {
    fn new(driver: D) -> Self {
        Self {
            driver,
            owners: BTreeSet::new(),
            outstanding: None,
            last_request: None,
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
        if self.application_active || self.owners.is_empty() || self.outstanding.is_some() {
            self.invalidate_schedule();
            return None;
        }

        if let Some(last) = self.last_request
            && now.saturating_duration_since(last) < DOCK_ATTENTION_RATE_LIMIT
        {
            return self.schedule_at(last + DOCK_ATTENTION_RATE_LIMIT);
        }

        self.invalidate_schedule();
        self.outstanding = Some(self.driver.request());
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
        if let Some(request) = self.outstanding.take() {
            self.driver.cancel(request);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NotificationDelivery {
    aggregate_count: u32,
}

trait NotificationDriver {
    fn deliver(&mut self, delivery: NotificationDelivery);
    fn clear(&mut self);
}

struct UserNotificationDriver;

impl NotificationDriver for UserNotificationDriver {
    fn deliver(&mut self, delivery: NotificationDelivery) {
        crate::platform::macos_notification::deliver_terminal_attention(delivery.aggregate_count);
    }

    fn clear(&mut self) {
        crate::platform::macos_notification::clear_terminal_attention();
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
        let aggregate_count = self
            .pending_by_pane
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add);
        let delivered_by_pane = std::mem::take(&mut self.pending_by_pane);
        if aggregate_count > 0 {
            self.delivered_by_pane = delivered_by_pane;
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

thread_local! {
    static DOCK_COORDINATOR: RefCell<DockAttentionCoordinator<AppKitDockAttention>> =
        RefCell::new(DockAttentionCoordinator::new(AppKitDockAttention));
    static NOTIFICATION_COORDINATOR: RefCell<NotificationCoordinator<UserNotificationDriver>> =
        RefCell::new(NotificationCoordinator::new(UserNotificationDriver));
}

pub(crate) fn register_pane() -> AttentionPaneId {
    AttentionPaneId(NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn remove_pane(pane: AttentionPaneId) {
    DOCK_COORDINATOR.with_borrow_mut(|coordinator| coordinator.clear(pane));
    NOTIFICATION_COORDINATOR.with_borrow_mut(|coordinator| coordinator.clear(pane));
}

pub(crate) fn update_application_activation(active: bool) -> AttentionSchedules {
    let dock = DOCK_COORDINATOR
        .with_borrow_mut(|coordinator| coordinator.set_application_active(active, Instant::now()));
    NOTIFICATION_COORDINATOR
        .with_borrow_mut(|coordinator| coordinator.set_application_active(active));
    AttentionSchedules {
        dock,
        notification: None,
    }
}

pub(crate) fn reconcile_scheduled(schedule: AttentionSchedule) -> AttentionSchedules {
    match schedule {
        AttentionSchedule::Dock(schedule) => AttentionSchedules {
            dock: DOCK_COORDINATOR.with_borrow_mut(|coordinator| {
                coordinator.reconcile_scheduled(schedule, Instant::now())
            }),
            notification: None,
        },
        AttentionSchedule::Notification(schedule) => {
            let notification = NOTIFICATION_COORDINATOR.with_borrow_mut(|coordinator| {
                coordinator.reconcile_scheduled(
                    schedule,
                    Instant::now(),
                    crate::platform::macos_application::is_active(),
                )
            });
            AttentionSchedules {
                dock: None,
                notification,
            }
        }
    }
}

pub(crate) trait AttentionPlatform {
    fn audio_bell(&mut self);
    fn request_dock_attention(&mut self) -> Option<DockAttentionSchedule>;
    fn cancel_dock_attention(&mut self);
    fn request_notification(&mut self) -> Option<NotificationSchedule>;
    fn cancel_notification(&mut self);
}

pub(crate) fn apply_attention_effects(
    platform: &mut impl AttentionPlatform,
    effects: AttentionEffects,
) -> AttentionSchedules {
    let mut schedules = AttentionSchedules::default();
    if effects.audio_bell {
        platform.audio_bell();
    }
    if effects.request_dock_attention {
        schedules.dock = platform.request_dock_attention();
    }
    if effects.cancel_dock_attention {
        platform.cancel_dock_attention();
        schedules.dock = None;
    }
    if effects.notification.is_some() {
        schedules.notification = platform.request_notification();
    }
    if effects.cancel_notification {
        platform.cancel_notification();
        schedules.notification = None;
    }
    schedules
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) struct MacosAttentionPlatform {
    pane: AttentionPaneId,
}

impl MacosAttentionPlatform {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) const fn new(pane: AttentionPaneId) -> Self {
        Self { pane }
    }
}

impl AttentionPlatform for MacosAttentionPlatform {
    fn audio_bell(&mut self) {
        unsafe extern "C" {
            #[cfg_attr(test, allow(dead_code))]
            fn NSBeep();
        }
        // SAFETY: AppKit's process-global bell takes no arguments and may be called on the UI thread.
        unsafe { NSBeep() };
    }

    fn request_dock_attention(&mut self) -> Option<DockAttentionSchedule> {
        DOCK_COORDINATOR
            .with_borrow_mut(|coordinator| coordinator.request(self.pane, Instant::now()))
    }

    fn cancel_dock_attention(&mut self) {
        DOCK_COORDINATOR.with_borrow_mut(|coordinator| coordinator.clear(self.pane));
    }

    fn request_notification(&mut self) -> Option<NotificationSchedule> {
        NOTIFICATION_COORDINATOR.with_borrow_mut(|coordinator| {
            coordinator.request(
                self.pane,
                crate::platform::macos_application::is_active(),
                Instant::now(),
            )
        })
    }

    fn cancel_notification(&mut self) {
        NOTIFICATION_COORDINATOR.with_borrow_mut(|coordinator| coordinator.clear(self.pane));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct FakePlatform {
        audio: usize,
        dock: usize,
        cancelled: usize,
        notification_requests: usize,
        notification_cancellations: usize,
    }

    impl AttentionPlatform for FakePlatform {
        fn audio_bell(&mut self) {
            self.audio += 1;
        }
        fn request_dock_attention(&mut self) -> Option<DockAttentionSchedule> {
            self.dock += 1;
            None
        }
        fn cancel_dock_attention(&mut self) {
            self.cancelled += 1;
        }
        fn request_notification(&mut self) -> Option<NotificationSchedule> {
            self.notification_requests += 1;
            None
        }
        fn cancel_notification(&mut self) {
            self.notification_cancellations += 1;
        }
    }

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
        next_request: NSInteger,
        requests: usize,
        cancellations: Vec<NSInteger>,
    }

    impl DockAttentionDriver for RecordingDockDriver {
        type Request = NSInteger;

        fn request(&mut self) -> Self::Request {
            self.next_request += 1;
            self.requests += 1;
            self.next_request
        }

        fn cancel(&mut self, request: Self::Request) {
            self.cancellations.push(request);
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

        assert_eq!(after_first_clear, (1, Vec::new(), Some(1)));
        assert_eq!(
            (
                coordinator.driver.requests,
                coordinator.driver.cancellations,
                coordinator.outstanding,
            ),
            (1, vec![1], None)
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
            (2, vec![1], Some(2), None)
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
                Some(2),
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
                None,
                Some(schedule),
                Duration::from_millis(1),
            )
        );
        assert_eq!(
            (coordinator.driver.requests, coordinator.outstanding),
            (2, Some(2))
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

    #[test]
    fn platform_effects_queue_and_cancel_content_free_notification_demand() {
        let event = AttentionEvent::CommandFinished {
            exit_status: Some(1),
            duration: Duration::from_secs(2),
        };
        let mut platform = FakePlatform::default();
        let schedules = apply_attention_effects(
            &mut platform,
            AttentionEffects {
                audio_bell: true,
                request_dock_attention: true,
                cancel_dock_attention: true,
                notification: Some(event),
                cancel_notification: true,
                unread_count: 3,
                ..AttentionEffects::default()
            },
        );

        assert_eq!(schedules, AttentionSchedules::default());
        assert_eq!(
            (platform.audio, platform.dock, platform.cancelled),
            (1, 1, 1)
        );
        assert_eq!(platform.notification_requests, 1);
        assert_eq!(platform.notification_cancellations, 1);
    }
}
