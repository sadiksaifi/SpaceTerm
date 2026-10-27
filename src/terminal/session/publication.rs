//! When a Terminal Session publishes its screen.
//!
//! The first output after idle publishes at once, so an echo never waits. Output is idle when
//! nothing waits to publish, the previous output was at least [`QUIET`] ago, and one frame
//! interval has passed since the previous publication. Output that continues coalesces: it
//! publishes once it has been quiet for [`QUIET`], or one frame interval after the burst began,
//! whichever comes first, but never sooner than one frame interval after the previous
//! publication. So no screen publishes faster than the client's frame rate. Other changes, such
//! as a selection drag, publish at most once per frame interval. The schedule only computes
//! deadlines; the session worker waits for the earliest one and never polls. Synchronized output
//! (DEC 2026) is held by the worker, which removes this deadline while the engine reports a
//! synchronized update.
use std::time::{Duration, Instant};

/// How long output must pause before a coalesced publication.
pub(super) const QUIET: Duration = Duration::from_millis(4);
/// The frame rate before a client declares one.
pub(crate) const DEFAULT_FRAME_RATE: u8 = 60;

pub(super) struct PublicationSchedule {
    presentable: bool,
    pending: bool,
    interval: Duration,
    /// The earliest paced publication: one frame interval after the previous publication.
    paced: Instant,
    /// When unpublished output is due on its own. `None` while no output is unpublished.
    output_due: Option<Instant>,
    /// One frame interval after the unpublished burst's first output.
    burst_cap: Option<Instant>,
    /// When the latest output arrived.
    last_output: Option<Instant>,
}

impl PublicationSchedule {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            presentable: true,
            pending: false,
            interval: frame_interval(DEFAULT_FRAME_RATE),
            paced: now,
            output_due: None,
            burst_cap: None,
            last_output: None,
        }
    }

    /// Sets the frame interval from a frame rate in hertz. It applies from the next publication.
    pub(super) fn set_frame_rate(&mut self, hz: u8) {
        self.interval = frame_interval(hz);
    }

    /// Requests a paced publication for a change that is not PTY output.
    pub(super) fn request(&mut self) {
        self.pending = true;
    }

    /// Records PTY output. Output after idle is due now; continued output is due once quiet.
    pub(super) fn output(&mut self, now: Instant) {
        let idle = !self.pending
            && now >= self.paced
            && self
                .last_output
                .is_none_or(|last| now.saturating_duration_since(last) >= QUIET);
        self.burst_cap.get_or_insert(now + self.interval);
        let due = if idle { now } else { now + QUIET };
        // More output never postpones a publication that is already due.
        self.output_due = Some(match self.output_due {
            Some(previous) if previous <= now => previous,
            _ => due,
        });
        self.last_output = Some(now);
        self.pending = true;
    }

    pub(super) fn set_presentable(&mut self, presentable: bool, now: Instant) {
        if presentable && !self.presentable {
            self.paced = now;
        }
        self.presentable = presentable;
    }

    pub(super) fn is_presentable(&self) -> bool {
        self.presentable
    }

    pub(super) fn mark_presented(&mut self, now: Instant) {
        self.pending = false;
        self.output_due = None;
        self.burst_cap = None;
        self.paced = now + self.interval;
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        if !self.presentable || !self.pending {
            return None;
        }
        let output = self
            .output_due
            .map(|due| self.burst_cap.map_or(due, |cap| due.min(cap)));
        Some(output.map_or(self.paced, |output| output.max(self.paced)))
    }

    pub(super) fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.take_pending()
        } else {
            false
        }
    }

    pub(super) fn take_pending(&mut self) -> bool {
        self.output_due = None;
        self.burst_cap = None;
        std::mem::take(&mut self.pending)
    }

    pub(super) fn take_visible_pending(&mut self) -> bool {
        self.presentable && self.take_pending()
    }
}

fn frame_interval(hz: u8) -> Duration {
    Duration::from_secs(1) / u32::from(hz.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn published(start: Instant) -> PublicationSchedule {
        let mut schedule = PublicationSchedule::new(start);
        schedule.mark_presented(start);
        schedule
    }

    /// Feeds output at each instant of `outputs`, publishing whenever the schedule is due, and
    /// returns the publication times.
    fn run(
        schedule: &mut PublicationSchedule,
        outputs: impl IntoIterator<Item = Instant>,
    ) -> Vec<Instant> {
        let mut publications = Vec::new();
        for now in outputs {
            schedule.output(now);
            if schedule.take_due(now) {
                schedule.mark_presented(now);
                publications.push(now);
            }
        }
        publications
    }

    #[test]
    fn the_first_output_after_idle_is_due_at_once() {
        let start = Instant::now();
        let mut schedule = published(start);
        let typed = start + ms(500);

        schedule.output(typed);

        assert_eq!(schedule.deadline(), Some(typed));
        assert!(schedule.take_due(typed));
    }

    #[test]
    fn output_within_a_frame_of_a_publication_waits_for_the_frame() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        assert_eq!(run(&mut schedule, [start]), [start]);

        schedule.output(start + ms(1));
        schedule.output(start + ms(2));

        let frame = start + Duration::from_secs(1) / 60;
        assert_eq!(schedule.deadline(), Some(frame));
        assert!(!schedule.take_due(frame - Duration::from_micros(1)));
        assert!(schedule.take_due(frame));
    }

    #[test]
    fn continuous_output_publishes_once_per_frame_interval() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        let publications = run(&mut schedule, (0..=50).map(|millis| start + ms(millis)));

        // 60 Hz: each burst publishes one frame interval after it began.
        assert_eq!(publications, [start, start + ms(18), start + ms(36)]);
    }

    #[test]
    fn bursty_output_never_publishes_faster_than_the_frame_rate() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        let publications = run(&mut schedule, (0..200).map(|step| start + ms(5 * step)));

        assert!(
            publications.len() <= 60,
            "{} publications in one second",
            publications.len()
        );
        for pair in publications.windows(2) {
            assert!(pair[1] - pair[0] >= Duration::from_secs(1) / 60);
        }
    }

    #[test]
    fn a_faster_frame_rate_shortens_the_cap() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        schedule.set_frame_rate(120);
        assert_eq!(run(&mut schedule, [start]), [start]);

        schedule.output(start + ms(1));
        schedule.output(start + ms(4));
        schedule.output(start + ms(7));

        // The burst began at 1 ms, so it publishes one 120 Hz frame later.
        assert_eq!(
            schedule.deadline(),
            Some(start + ms(1) + Duration::from_secs(1) / 120)
        );
    }

    #[test]
    fn more_output_never_postpones_a_due_publication() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        assert_eq!(run(&mut schedule, [start]), [start]);
        schedule.output(start + ms(1));

        // The worker was busy past the frame deadline when more output arrived.
        schedule.output(start + ms(20));

        assert!(schedule.take_due(start + ms(20)));
    }

    #[test]
    fn a_hidden_session_holds_its_output_and_publishes_it_when_revealed() {
        let start = Instant::now();
        let mut schedule = published(start);
        schedule.set_presentable(false, start);

        schedule.output(start + ms(1));
        assert_eq!(schedule.deadline(), None);
        assert!(!schedule.take_due(start + ms(500)));

        let revealed = start + ms(500);
        schedule.set_presentable(true, revealed);
        assert_eq!(schedule.deadline(), Some(revealed));
        assert!(schedule.take_due(revealed));
        assert!(!schedule.take_due(revealed));
    }

    #[test]
    fn other_changes_publish_at_most_once_per_frame_interval() {
        let start = Instant::now();
        let mut schedule = published(start);

        schedule.request();
        schedule.request();

        let next_frame = start + Duration::from_secs(1) / 60;
        assert_eq!(schedule.deadline(), Some(next_frame));
        assert!(!schedule.take_due(next_frame - Duration::from_micros(1)));
        assert!(schedule.take_due(next_frame));
        assert!(!schedule.take_due(next_frame));
    }

    #[test]
    fn output_after_a_paced_request_waits_for_the_frame() {
        let start = Instant::now();
        let mut schedule = published(start);
        schedule.request();

        schedule.output(start + ms(1));

        assert_eq!(
            schedule.deadline(),
            Some(start + Duration::from_secs(1) / 60)
        );
    }
}
