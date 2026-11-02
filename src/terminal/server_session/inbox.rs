//! The Pane worker's pushed channel events and the view demand it declares.
//!
//! The connection's I/O thread delivers each channel event to an [`Inbox`], which queues it and
//! wakes the worker. The worker waits for that wake, for input, or for a view change, never for a
//! timer. It drains the inbox in order: each frame is applied, then acknowledged so the server may
//! send the next, and the newest screen that changed something visible is handed to the view once
//! per drain, before any Pane event that follows it. Semantic messages are staged in the same
//! order, and a revision is acknowledged once its tail applies it.
//!
//! A semantic revision can span more messages than the event bound, and the worker may be
//! waiting for an input reply while it arrives. The revision's one credit bounds it instead: the
//! inbox queues every message of the one revision the server may send without counting it, and
//! a message that starts another revision of the current demand before the worker drained the
//! first one's tail floods the channel.
use spaceterm_server::connection::link::{ChannelEvent, ChannelSink};
use spaceterm_server::pane::accessibility::{Header, Update};
use spaceterm_server::protocol::{Frame, PaneEvent};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Wakes a Pane worker. A notification before the wait is never lost.
#[derive(Default)]
pub(super) struct Wake {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Wake {
    pub(super) fn notify(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.ready.notify_all();
    }

    /// Returns on the first notification since the previous wait, or at `deadline`.
    pub(super) fn wait(&self, deadline: Option<Instant>) {
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

/// Pane events the worker has not drained yet, besides semantic messages. Frames take one slot
/// each and arrive one at a time under the channel's credit; more than this means the server
/// floods the channel.
const MAX_QUEUED: usize = 1024;

/// Wakes the worker that owns the inbox. It must not block.
pub(super) type Notify = Arc<dyn Fn() + Send + Sync>;

/// One channel's undrained events. The connection delivers into it from its I/O thread.
pub(super) struct Inbox {
    queue: Mutex<Queue>,
    notify: Notify,
    /// How long each event waits before the worker may drain it. Zero outside Development
    /// builds started with a push delay.
    delay: Duration,
}

#[derive(Default)]
struct Queue {
    /// Each event with the instant the worker may drain it.
    events: VecDeque<(Instant, ChannelEvent)>,
    /// How many of `events` are semantic messages, which `MAX_QUEUED` does not count.
    semantic: usize,
    admission: Admission,
    overflowed: bool,
}

impl Queue {
    /// Whether the queued events other than semantic messages reached their bound.
    fn full(&self) -> bool {
        self.events.len() - self.semantic >= MAX_QUEUED
    }
}

/// Which semantic messages the channel may queue: those of the current demand generation, one
/// revision at a time, as its one semantic credit allows (ADR 0062).
#[derive(Default)]
struct Admission {
    /// The current demand generation, or `None` while demand is ended.
    generation: Option<u64>,
    /// The revision whose messages are queued, until the worker drains its tail.
    revision: Option<Header>,
    /// Its tail is queued.
    complete: bool,
}

enum Admit {
    Queue,
    /// Of another demand generation, or while demand is ended: the worker would discard it
    /// unacknowledged.
    Discard,
    /// Another revision before the worker drained the queued one: the server ignored its credit.
    Flood,
}

impl Admission {
    fn admit(&mut self, update: &Update) -> Admit {
        if self.generation != Some(update.header.demand_generation) {
            return Admit::Discard;
        }
        match self.revision {
            Some(header) if header != update.header || self.complete => return Admit::Flood,
            _ => {}
        }
        self.revision = Some(update.header);
        self.complete = update.tail.is_some();
        Admit::Queue
    }

    /// The worker drained `update`: after its revision's tail, the next revision may follow.
    fn drained(&mut self, update: &Update) {
        if update.tail.is_some() && self.revision == Some(update.header) {
            self.revision = None;
            self.complete = false;
        }
    }
}

/// Holds every pushed event back by `SPACETERM_DEVELOPER_PUSH_DELAY_MS`, so a local Pane shows
/// a distant server's round trip and its predictive echo. Development builds only.
#[cfg(feature = "developer-tools")]
fn push_delay() -> Duration {
    static DELAY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *DELAY.get_or_init(|| {
        std::env::var("SPACETERM_DEVELOPER_PUSH_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .map_or(Duration::ZERO, |millis| {
                Duration::from_millis(u64::from(millis))
            })
    })
}

#[cfg(not(feature = "developer-tools"))]
fn push_delay() -> Duration {
    Duration::ZERO
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
        Self::delayed(notify, push_delay())
    }

    fn delayed(notify: Notify, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::default(),
            notify,
            delay,
        })
    }

    /// Follows the channel's semantic demand: `Some(generation)` after a start, `None` after an
    /// end. Call it before the server can see the demand.
    pub(super) fn demand(&self, generation: Option<u64>) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.admission = Admission {
            generation,
            ..Admission::default()
        };
    }

    /// Takes the events that are due, in order.
    fn take(&self) -> Result<Vec<ChannelEvent>, Overflow> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if queue.overflowed {
            return Err(Overflow);
        }
        let now = Instant::now();
        let due = queue
            .events
            .iter()
            .take_while(|(due, _)| *due <= now)
            .count();
        let taken: Vec<_> = queue.events.drain(..due).map(|(_, event)| event).collect();
        for event in &taken {
            if let ChannelEvent::Accessibility(update) = event {
                queue.semantic -= 1;
                queue.admission.drained(update);
            }
        }
        Ok(taken)
    }

    /// When the oldest held event becomes due, if one waits.
    pub(super) fn due(&self) -> Option<Instant> {
        let queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.events.front().map(|(due, _)| *due)
    }

    /// Applies every queued event in order. Each frame is applied, then acknowledged, and each
    /// semantic revision is acknowledged once its last message applies it. The newest frame that
    /// changed something visible reaches `deliver` once, before any Pane event after it. Stops
    /// after the channel's last event, which `deliver` receives, discarding an unfinished
    /// semantic revision.
    pub(super) fn drain<A, E>(
        &self,
        applier: &mut A,
        mut acknowledge: impl FnMut(Ack) -> Result<(), E>,
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
                ChannelEvent::Accessibility(update) => {
                    if let Some(ack) = applier.accessibility(*update)? {
                        acknowledge(ack)?;
                    }
                }
                ChannelEvent::Frame(frame) => {
                    let number = frame.frame;
                    if let Some(screen) = applier.apply(*frame)? {
                        changed = Some(screen);
                    }
                    acknowledge(Ack::Frame(number))?;
                }
                ChannelEvent::Pane(event) => {
                    applier.event(&event);
                    if let Some(screen) = changed.take() {
                        deliver(Delivery::Screen(screen));
                    }
                    let last = event.last();
                    deliver(Delivery::Pane(event));
                    if last {
                        applier.end();
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
            let admitted = match &event {
                ChannelEvent::Accessibility(update) => queue.admission.admit(update),
                _ if queue.full() => Admit::Flood,
                _ => Admit::Queue,
            };
            match admitted {
                _ if queue.overflowed => {}
                Admit::Flood => {
                    queue.overflowed = true;
                    queue.events.clear();
                    queue.semantic = 0;
                }
                Admit::Discard => {}
                Admit::Queue => {
                    queue.semantic += usize::from(matches!(event, ChannelEvent::Accessibility(_)));
                    queue.events.push_back((Instant::now() + self.delay, event));
                }
            }
        }
        (self.notify)();
    }
}

/// Applies a pushed frame to the screen the client holds, and stages semantic messages.
pub(super) trait Apply {
    type Screen;
    type Error;
    /// Returns the new screen when the frame changed anything visible, otherwise `None`.
    fn apply(&mut self, frame: Frame) -> Result<Option<Self::Screen>, Self::Error>;
    /// Sees each Pane event in channel order with the frames, before the worker handles it.
    fn event(&mut self, _event: &PaneEvent) {}
    /// Stages one message of a semantic revision. Returns the revision's acknowledgement when
    /// the message completed and applied it.
    fn accessibility(&mut self, update: Update) -> Result<Option<Ack>, Self::Error>;
    /// Sees the channel's last event: an unfinished semantic revision is discarded.
    fn end(&mut self);
}

/// What the worker acknowledges, each on the channel's own credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ack {
    Frame(u64),
    Semantics {
        demand_generation: u64,
        revision: u64,
    },
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
    use spaceterm_server::pane::LineState;
    use spaceterm_server::protocol::{Channel, ScreenUpdate};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Applies a frame's generation as the screen; generation 0 changes nothing visible. Stages
    /// each semantic message, completing its revision at the tail, and records what it saw.
    #[derive(Default)]
    struct Generations {
        staged: Vec<u64>,
        ended: bool,
    }
    impl Apply for Generations {
        type Screen = u64;
        type Error = Overflow;
        fn apply(&mut self, frame: Frame) -> Result<Option<u64>, Overflow> {
            Ok((frame.update.generation != 0).then_some(frame.update.generation))
        }
        fn accessibility(&mut self, update: Update) -> Result<Option<Ack>, Overflow> {
            self.staged.push(update.header.revision);
            Ok(update.tail.map(|_| Ack::Semantics {
                demand_generation: update.header.demand_generation,
                revision: update.header.revision,
            }))
        }
        fn end(&mut self) {
            self.ended = true;
        }
    }

    fn semantic(revision: u64, tail: bool) -> ChannelEvent {
        semantic_in(1, revision, tail)
    }

    fn semantic_in(demand_generation: u64, revision: u64, tail: bool) -> ChannelEvent {
        use spaceterm_server::pane::accessibility::{Screen, Tail};
        ChannelEvent::Accessibility(Box::new(Update {
            header: Header {
                demand_generation,
                revision,
                base: None,
                screen: Screen::Primary,
                screen_generation: 1,
            },
            splices: Vec::new(),
            rows: Vec::new(),
            tail: tail.then_some(Tail {
                visible_lines: 0..0,
                cursor: None,
                selection: None,
            }),
        }))
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

    /// An inbox whose channel's semantic demand stands in generation 1.
    fn inbox() -> (Arc<Inbox>, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let inbox = Inbox::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        inbox.demand(Some(1));
        (inbox, wakes)
    }

    fn drain(inbox: &Inbox) -> (Result<Flow, Overflow>, Vec<u64>, Vec<Delivery<u64>>) {
        let (flow, acknowledged, delivered) = drain_all(inbox, &mut Generations::default());
        let frames = acknowledged
            .into_iter()
            .filter_map(|ack| match ack {
                Ack::Frame(frame) => Some(frame),
                Ack::Semantics { .. } => None,
            })
            .collect();
        (flow, frames, delivered)
    }

    fn drain_all(
        inbox: &Inbox,
        applier: &mut Generations,
    ) -> (Result<Flow, Overflow>, Vec<Ack>, Vec<Delivery<u64>>) {
        let mut acknowledged = Vec::new();
        let mut delivered = Vec::new();
        let flow = inbox.drain(
            applier,
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
        inbox.deliver(ChannelEvent::Pane(PaneEvent::Line(LineState::default())));
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
        inbox.deliver(ChannelEvent::Pane(PaneEvent::Line(LineState::default())));

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
            inbox.deliver(ChannelEvent::Pane(PaneEvent::Line(LineState::default())));
        }

        let (flow, acknowledged, delivered) = drain(&inbox);

        assert_eq!(flow, Err(Overflow));
        assert!(acknowledged.is_empty() && delivered.is_empty());
    }

    #[test]
    fn a_push_delay_holds_each_event_until_it_is_due() {
        let delay = Duration::from_millis(40);
        let inbox = Inbox::delayed(Arc::new(|| {}), delay);
        let delivered_at = Instant::now();
        inbox.deliver(frame(1, 1));
        assert!(inbox.due().is_some_and(|due| due >= delivered_at + delay));

        let (flow, acknowledged, delivered) = drain(&inbox);
        assert_eq!(flow, Ok(Flow::Open));
        assert!(acknowledged.is_empty() && delivered.is_empty());

        std::thread::sleep(delay);
        let (_, acknowledged, delivered) = drain(&inbox);
        assert_eq!(acknowledged, [1]);
        assert!(matches!(delivered.as_slice(), [Delivery::Screen(1)]));
        assert_eq!(inbox.due(), None);
    }

    #[test]
    fn semantic_messages_are_staged_in_channel_order_and_acknowledged_at_their_tail() {
        let (inbox, _) = inbox();
        inbox.deliver(semantic(7, false));
        inbox.deliver(frame(1, 1));
        inbox.deliver(semantic(7, true));
        inbox.deliver(frame(2, 2));

        let mut applier = Generations::default();
        let (flow, acknowledged, delivered) = drain_all(&inbox, &mut applier);

        assert_eq!(flow, Ok(Flow::Open));
        assert_eq!(applier.staged, [7, 7]);
        assert_eq!(
            acknowledged,
            [
                Ack::Frame(1),
                Ack::Semantics {
                    demand_generation: 1,
                    revision: 7
                },
                Ack::Frame(2)
            ],
            "a revision is acknowledged once, after its tail, in order with frames"
        );
        assert!(matches!(delivered.as_slice(), [Delivery::Screen(2)]));
        assert!(!applier.ended);
    }

    #[test]
    fn the_channels_last_event_discards_an_unfinished_semantic_revision() {
        let (inbox, _) = inbox();
        inbox.deliver(semantic(3, false));
        inbox.deliver(ChannelEvent::Pane(PaneEvent::Ended));
        inbox.deliver(semantic(3, true));

        let mut applier = Generations::default();
        let (flow, acknowledged, _) = drain_all(&inbox, &mut applier);

        assert_eq!(flow, Ok(Flow::Ended));
        assert!(applier.ended, "the applier hears of the channel's end");
        assert_eq!(
            applier.staged,
            [3],
            "nothing after the last event is staged"
        );
        assert!(acknowledged.is_empty());
    }

    #[test]
    fn a_valid_revision_longer_than_the_event_bound_queues_whole_while_the_worker_waits() {
        use spaceterm_server::pane::accessibility::{CellSpan, Packer, RowUpdate, Run, Tail};
        // Each row holds 512 cells of 33 bytes, so no message holds two of them.
        let cell = format!("e{}", "\u{301}".repeat(16));
        let rows = MAX_QUEUED as u32 + 64;
        let mut packer = Packer::new(Header {
            demand_generation: 1,
            revision: 1,
            base: None,
            screen: spaceterm_server::pane::accessibility::Screen::Primary,
            screen_generation: 1,
        });
        let mut messages = packer.splice(
            0,
            0,
            vec![Run {
                node_serial: 1,
                page_row: 0,
                count: rows as u16,
            }],
        );
        for line in 0..rows {
            messages.extend(
                packer.row(RowUpdate::new(
                    line,
                    1,
                    false,
                    cell.repeat(512),
                    (0..512)
                        .map(|column| CellSpan {
                            column,
                            width: 1,
                            bytes: cell.len() as u16,
                        })
                        .collect::<Vec<_>>(),
                )),
            );
        }
        messages.push(packer.finish(Tail {
            visible_lines: rows - 24..rows,
            cursor: None,
            selection: None,
        }));
        assert!(messages.len() > MAX_QUEUED);
        assert!(messages.iter().all(|message| message.validate().is_ok()));

        // The worker waits for an input reply while the whole revision arrives, with a frame
        // and a Pane event in its midst.
        let (inbox, _) = inbox();
        let count = messages.len();
        let mut messages = messages.into_iter();
        for message in messages.by_ref().take(count / 2) {
            inbox.deliver(ChannelEvent::Accessibility(Box::new(message)));
        }
        inbox.deliver(frame(1, 1));
        inbox.deliver(ChannelEvent::Pane(PaneEvent::Line(LineState::default())));
        for message in messages {
            inbox.deliver(ChannelEvent::Accessibility(Box::new(message)));
        }

        let mut applier = Generations::default();
        let (flow, acknowledged, delivered) = drain_all(&inbox, &mut applier);

        assert_eq!(flow, Ok(Flow::Open), "one valid revision is no flood");
        assert_eq!(applier.staged.len(), count, "every message is staged");
        assert_eq!(
            acknowledged,
            [
                Ack::Frame(1),
                Ack::Semantics {
                    demand_generation: 1,
                    revision: 1
                }
            ],
            "the frame keeps its place in channel order"
        );
        assert!(matches!(
            delivered.as_slice(),
            [Delivery::Screen(1), Delivery::Pane(PaneEvent::Line(_))]
        ));
    }

    #[test]
    fn a_second_revision_before_the_first_is_drained_floods_the_channel() {
        let (channel, _) = inbox();
        channel.deliver(semantic(7, true));
        channel.deliver(semantic(8, false));
        assert_eq!(
            drain(&channel).0,
            Err(Overflow),
            "revision 7 holds the credit"
        );

        // Once the worker drained revision 7, the server may send the next one.
        let (channel, _) = inbox();
        channel.deliver(semantic(7, true));
        assert_eq!(drain(&channel).0, Ok(Flow::Open));
        channel.deliver(semantic(8, true));
        let mut applier = Generations::default();
        assert_eq!(drain_all(&channel, &mut applier).0, Ok(Flow::Open));
        assert_eq!(applier.staged, [8]);

        // A restart abandons an unfinished revision: the new generation's document is admitted
        // and late messages of the earlier one are not queued.
        let (channel, _) = inbox();
        channel.deliver(semantic(7, false));
        channel.demand(Some(2));
        channel.deliver(semantic(7, true));
        channel.deliver(semantic_in(2, 1, true));
        let mut applier = Generations::default();
        assert_eq!(drain_all(&channel, &mut applier).0, Ok(Flow::Open));
        assert_eq!(applier.staged, [7, 1]);

        // Nothing is queued while demand is ended.
        channel.demand(None);
        channel.deliver(semantic_in(2, 2, true));
        let mut applier = Generations::default();
        assert_eq!(drain_all(&channel, &mut applier).0, Ok(Flow::Open));
        assert!(applier.staged.is_empty());
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
