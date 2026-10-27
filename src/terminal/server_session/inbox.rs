//! The Pane worker's pushed channel events and the view demand it declares.
//!
//! The connection's I/O thread delivers each channel event to an [`Inbox`], which queues it and
//! wakes the worker. The worker waits for that wake, for input, or for a view change, never for a
//! timer. It drains the inbox in order: each frame is applied, then acknowledged so the server may
//! send the next, and the newest screen that changed something visible is handed to the view once
//! per drain, before any Pane event that follows it.
use spaceterm_server::connection::link::{ChannelEvent, ChannelSink};
use spaceterm_server::protocol::{Frame, PaneEvent};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Rechecks reading work the server is still applying, such as an operation's presentation
/// barrier or a selection being prepared. A quarter of a 60 Hz frame bounds each wait without
/// turning the worker into a busy loop.
pub(super) const READING_RECHECK: Duration = Duration::from_millis(4);

/// Wakes a Pane worker. A notification before the wait is never lost.
#[derive(Default)]
pub(super) struct Wake {
    pending: Mutex<bool>,
    ready: Condvar,
    /// How many waits ended, so a test can prove an idle worker does not turn.
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
}

impl Wake {
    pub(super) fn notify(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.ready.notify_all();
    }

    #[cfg(test)]
    pub(super) fn waits(&self) -> usize {
        self.waits.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns on the first notification since the previous wait, or at `deadline`.
    pub(super) fn wait(&self, deadline: Option<Instant>) {
        #[cfg(test)]
        self.waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        while !*pending {
            pending = match deadline {
                None => self.ready.wait(pending).unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return;
                    };
                    self.ready
                        .wait_timeout(pending, remaining)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
            };
        }
        *pending = false;
    }
}

/// Pane events the worker has not drained yet. Frames take one slot each and arrive one at a
/// time under the channel's credit; more than this means the server floods the channel.
const MAX_QUEUED: usize = 1024;

/// Wakes the worker that owns the inbox. It must not block.
pub(super) type Notify = Arc<dyn Fn() + Send + Sync>;

/// One channel's undrained events. The connection delivers into it from its I/O thread.
pub(super) struct Inbox {
    queue: Mutex<Queue>,
    notify: Notify,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<ChannelEvent>,
    overflowed: bool,
}

/// The channel broke a bound the client relies on. The worker ends the claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Overflow;

impl From<Overflow> for crate::server_connection::ServerError {
    fn from(_: Overflow) -> Self {
        Self::Protocol
    }
}

impl Inbox {
    pub(super) fn new(notify: Notify) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::default(),
            notify,
        })
    }

    fn take(&self) -> Result<VecDeque<ChannelEvent>, Overflow> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.overflowed {
            return Err(Overflow);
        }
        Ok(std::mem::take(&mut queue.events))
    }

    /// Applies every queued event in order. Each frame is applied, then acknowledged. The newest
    /// frame that changed something visible reaches `deliver` once, before any Pane event after
    /// it. Stops after the channel's last event, which `deliver` receives.
    pub(super) fn drain<A, E>(
        &self,
        applier: &mut A,
        mut acknowledge: impl FnMut(u64) -> Result<(), E>,
        mut deliver: impl FnMut(Delivery<A::Screen>),
    ) -> Result<Flow, E>
    where
        A: Apply,
        E: From<Overflow> + From<A::Error>,
    {
        let mut changed = None;
        for event in self.take()? {
            match event {
                // The worker learns of the link's end from its own link.
                ChannelEvent::Closed(_) => {}
                ChannelEvent::Frame(frame) => {
                    let number = frame.frame;
                    if let Some(screen) = applier.apply(*frame)? {
                        changed = Some(screen);
                    }
                    acknowledge(number)?;
                }
                ChannelEvent::Pane(event) => {
                    if let Some(screen) = changed.take() {
                        deliver(Delivery::Screen(screen));
                    }
                    let last = event.last();
                    deliver(Delivery::Pane(event));
                    if last {
                        return Ok(Flow::Ended);
                    }
                }
            }
        }
        if let Some(screen) = changed {
            deliver(Delivery::Screen(screen));
        }
        Ok(Flow::Open)
    }
}

impl ChannelSink for Inbox {
    fn deliver(&self, event: ChannelEvent) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if queue.events.len() >= MAX_QUEUED {
                queue.overflowed = true;
                queue.events.clear();
            } else if !queue.overflowed {
                queue.events.push_back(event);
            }
        }
        (self.notify)();
    }
}

/// Applies a pushed frame to the screen the client holds.
pub(super) trait Apply {
    type Screen;
    type Error;
    /// Returns the new screen when the frame changed anything visible, otherwise `None`.
    fn apply(&mut self, frame: Frame) -> Result<Option<Self::Screen>, Self::Error>;
}

/// What a drain hands to the view and the worker, in channel order.
#[derive(Debug)]
pub(super) enum Delivery<S> {
    Screen(S),
    Pane(PaneEvent),
}

/// Whether the channel stays open after a drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Flow {
    Open,
    /// The channel delivered its last event: `Ended`, `SessionEnded`, `Failed` or `Takeover`.
    Ended,
}

/// The view a Pane worker declares to the server: whether its channel presents, and at what
/// frame rate. A Pane prepared while hidden still presents, so its rows are ready to show.
pub(super) struct Demand {
    visible: bool,
    preparation: bool,
    max_hz: u8,
    sent: Option<(bool, u8)>,
}

impl Demand {
    pub(super) fn new(visible: bool, max_hz: u8) -> Self {
        Self {
            visible,
            preparation: false,
            max_hz,
            sent: None,
        }
    }

    pub(super) fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub(super) fn set_preparation(&mut self, prepare: bool) {
        self.preparation = prepare;
    }

    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    /// The `View` the server has not been told yet, as `(visible, max_hz)`.
    pub(super) fn take_view(&mut self) -> Option<(bool, u8)> {
        let view = (self.visible || self.preparation, self.max_hz);
        (self.sent != Some(view)).then(|| {
            self.sent = Some(view);
            view
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_server::protocol::{Channel, ScreenUpdate};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Applies a frame's generation as the screen; generation 0 changes nothing visible.
    struct Generations;
    impl Apply for Generations {
        type Screen = u64;
        type Error = Overflow;
        fn apply(&mut self, frame: Frame) -> Result<Option<u64>, Overflow> {
            Ok((frame.update.generation != 0).then_some(frame.update.generation))
        }
    }

    fn frame(number: u64, generation: u64) -> ChannelEvent {
        ChannelEvent::Frame(Box::new(Frame {
            channel: Channel(1),
            frame: number,
            base: number.checked_sub(1).filter(|base| *base > 0),
            input: 0,
            update: ScreenUpdate {
                generation,
                ..ScreenUpdate::default()
            },
        }))
    }

    fn inbox() -> (Arc<Inbox>, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let inbox = Inbox::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        (inbox, wakes)
    }

    fn drain(inbox: &Inbox) -> (Result<Flow, Overflow>, Vec<u64>, Vec<Delivery<u64>>) {
        let mut acknowledged = Vec::new();
        let mut delivered = Vec::new();
        let flow = inbox.drain(
            &mut Generations,
            |frame| {
                acknowledged.push(frame);
                Ok(())
            },
            |delivery| delivered.push(delivery),
        );
        (flow, acknowledged, delivered)
    }

    #[test]
    fn a_notification_before_the_wait_ends_it_immediately_and_only_once() {
        let wake = Wake::default();
        wake.notify();
        let started = Instant::now();
        wake.wait(Some(started + Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
        let started = Instant::now();
        wake.wait(Some(started + Duration::from_millis(20)));
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "a consumed notification must not end a later wait"
        );
    }

    #[test]
    fn a_worker_waiting_without_a_deadline_wakes_for_a_notification() {
        let wake = Arc::new(Wake::default());
        let waiter = {
            let wake = Arc::clone(&wake);
            std::thread::spawn(move || {
                let started = Instant::now();
                wake.wait(None);
                started.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(10));
        wake.notify();
        assert!(waiter.join().unwrap() < Duration::from_secs(5));
    }

    #[test]
    fn each_delivery_wakes_the_worker() {
        let (inbox, wakes) = inbox();
        inbox.deliver(frame(1, 1));
        inbox.deliver(ChannelEvent::Pane(PaneEvent::HiddenInput(true)));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn frames_are_acknowledged_in_order_and_the_view_is_told_once() {
        let (inbox, _) = inbox();
        inbox.deliver(frame(1, 1));
        inbox.deliver(frame(2, 2));

        let (flow, acknowledged, delivered) = drain(&inbox);

        assert_eq!(flow, Ok(Flow::Open));
        assert_eq!(acknowledged, [1, 2]);
        assert!(matches!(delivered.as_slice(), [Delivery::Screen(2)]));
    }

    #[test]
    fn a_frame_that_changes_nothing_visible_is_acknowledged_without_telling_the_view() {
        let (inbox, _) = inbox();
        inbox.deliver(frame(1, 0));

        let (flow, acknowledged, delivered) = drain(&inbox);

        assert_eq!(flow, Ok(Flow::Open));
        assert_eq!(acknowledged, [1]);
        assert!(delivered.is_empty());
    }

    #[test]
    fn the_final_screen_reaches_the_view_before_the_last_event() {
        let (inbox, _) = inbox();
        inbox.deliver(frame(1, 1));
        inbox.deliver(ChannelEvent::Pane(PaneEvent::Ended));
        inbox.deliver(ChannelEvent::Pane(PaneEvent::HiddenInput(true)));

        let (flow, _, delivered) = drain(&inbox);

        assert_eq!(flow, Ok(Flow::Ended));
        assert!(matches!(
            delivered.as_slice(),
            [Delivery::Screen(1), Delivery::Pane(PaneEvent::Ended)]
        ));
    }

    #[test]
    fn a_flooding_channel_overflows_instead_of_growing() {
        let (inbox, _) = inbox();
        for _ in 0..=MAX_QUEUED {
            inbox.deliver(ChannelEvent::Pane(PaneEvent::HiddenInput(true)));
        }

        let (flow, acknowledged, delivered) = drain(&inbox);

        assert_eq!(flow, Err(Overflow));
        assert!(acknowledged.is_empty() && delivered.is_empty());
    }

    #[test]
    fn the_view_is_declared_once_per_change() {
        let mut demand = Demand::new(false, 60);
        assert_eq!(demand.take_view(), Some((false, 60)));
        assert_eq!(demand.take_view(), None);

        demand.set_preparation(true);
        assert_eq!(demand.take_view(), Some((true, 60)));
        demand.set_visible(true);
        assert_eq!(demand.take_view(), None);

        demand.set_preparation(false);
        demand.set_visible(false);
        assert_eq!(demand.take_view(), Some((false, 60)));
    }
}
