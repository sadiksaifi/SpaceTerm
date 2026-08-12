use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cocoa::appkit::NSApplication;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSInteger, NSString};
use objc::{class, msg_send, sel, sel_impl};

use crate::terminal::attention::{AttentionEffects, AttentionEvent};

const DOCK_ATTENTION_RATE_LIMIT: Duration = Duration::from_secs(1);

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DockAttentionPaneId(u64);

impl DockAttentionPaneId {
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
    owners: BTreeSet<DockAttentionPaneId>,
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

    fn request(
        &mut self,
        pane: DockAttentionPaneId,
        now: Instant,
    ) -> Option<DockAttentionSchedule> {
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

    fn clear(&mut self, pane: DockAttentionPaneId) {
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

thread_local! {
    static DOCK_COORDINATOR: RefCell<DockAttentionCoordinator<AppKitDockAttention>> =
        RefCell::new(DockAttentionCoordinator::new(AppKitDockAttention));
}

pub(crate) fn register_pane() -> DockAttentionPaneId {
    DockAttentionPaneId(NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn remove_pane(pane: DockAttentionPaneId) {
    DOCK_COORDINATOR.with_borrow_mut(|coordinator| coordinator.clear(pane));
}

pub(crate) fn update_application_activation(active: bool) -> Option<DockAttentionSchedule> {
    DOCK_COORDINATOR
        .with_borrow_mut(|coordinator| coordinator.set_application_active(active, Instant::now()))
}

pub(crate) fn reconcile_scheduled(
    schedule: DockAttentionSchedule,
) -> Option<DockAttentionSchedule> {
    DOCK_COORDINATOR
        .with_borrow_mut(|coordinator| coordinator.reconcile_scheduled(schedule, Instant::now()))
}

pub(crate) trait AttentionPlatform {
    fn audio_bell(&mut self);
    fn request_dock_attention(&mut self) -> Option<DockAttentionSchedule>;
    fn cancel_dock_attention(&mut self);
    fn notify(&mut self, event: AttentionEvent, aggregate_count: u32);
}

pub(crate) fn apply_attention_effects(
    platform: &mut impl AttentionPlatform,
    effects: AttentionEffects,
) -> Option<DockAttentionSchedule> {
    let mut schedule = None;
    if effects.audio_bell {
        platform.audio_bell();
    }
    if effects.request_dock_attention {
        schedule = platform.request_dock_attention();
    }
    if effects.cancel_dock_attention {
        platform.cancel_dock_attention();
        schedule = None;
    }
    if let Some(event) = effects.notification {
        platform.notify(event, effects.unread_count);
    }
    schedule
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) struct MacosAttentionPlatform {
    pane: DockAttentionPaneId,
}

impl MacosAttentionPlatform {
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) const fn new(pane: DockAttentionPaneId) -> Self {
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

    fn notify(&mut self, event: AttentionEvent, aggregate_count: u32) {
        let body = match event {
            AttentionEvent::Bell => format!("Terminal requested attention ({aggregate_count})"),
            AttentionEvent::CommandFinished { exit_status, .. } => match exit_status {
                Some(0) => format!("Command finished ({aggregate_count})"),
                Some(_) => format!("Command finished with an error ({aggregate_count})"),
                None => format!("Command finished ({aggregate_count})"),
            },
        };
        // SAFETY: Deprecated NSUserNotification remains native-policy mediated on supported macOS,
        // and all objects/messages are confined to GPUI's main thread.
        unsafe {
            let notification: id = msg_send![class!(NSUserNotification), new];
            let title = NSString::alloc(nil).init_str("SpaceTerm");
            let body = NSString::alloc(nil).init_str(&body);
            let _: () = msg_send![notification, setTitle: title];
            let _: () = msg_send![notification, setInformativeText: body];
            let center: id = msg_send![
                class!(NSUserNotificationCenter),
                defaultUserNotificationCenter
            ];
            let _: () = msg_send![center, deliverNotification: notification];
            let _: () = msg_send![title, release];
            let _: () = msg_send![body, release];
            let _: () = msg_send![notification, release];
        }
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
        notifications: Vec<(AttentionEvent, u32)>,
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
        fn notify(&mut self, event: AttentionEvent, count: u32) {
            self.notifications.push((event, count));
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
        let first = DockAttentionPaneId::test(1);
        let second = DockAttentionPaneId::test(2);
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
        let first = DockAttentionPaneId::test(1);
        let second = DockAttentionPaneId::test(2);
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
        let pane = DockAttentionPaneId::test(1);
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
                BTreeSet::from([DockAttentionPaneId::test(1)]),
                Some(2),
                None,
            )
        );
    }

    #[test]
    fn rate_limited_deactivation_retains_owner_demand_for_later_reconciliation() {
        let epoch = Instant::now();
        let pane = DockAttentionPaneId::test(1);
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
                BTreeSet::from([DockAttentionPaneId::test(1)]),
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
        let pane = DockAttentionPaneId::test(1);
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
    fn platform_effects_are_explicit_testable_and_content_redacted() {
        let event = AttentionEvent::CommandFinished {
            exit_status: Some(1),
            duration: Duration::from_secs(2),
        };
        let mut platform = FakePlatform::default();
        let schedule = apply_attention_effects(
            &mut platform,
            AttentionEffects {
                audio_bell: true,
                request_dock_attention: true,
                cancel_dock_attention: true,
                notification: Some(event),
                unread_count: 3,
                ..AttentionEffects::default()
            },
        );

        assert_eq!(schedule, None);
        assert_eq!(
            (platform.audio, platform.dock, platform.cancelled),
            (1, 1, 1)
        );
        assert_eq!(platform.notifications, vec![(event, 3)]);
    }
}
