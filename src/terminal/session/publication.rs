//! When a Terminal Session publishes its screen.
//!
//! The first output after idle publishes at once, so an echo never waits. Output that continues
//! coalesces: it publishes once it has been quiet for [`QUIET`], or one frame interval after the
//! previous publication, whichever comes first. Other changes, such as a selection drag, publish
//! at most once per frame interval. The schedule only computes deadlines; the session worker
//! waits for the earliest one and never polls. Synchronized output (DEC 2026) is held by the
//! worker, which removes this deadline while the engine reports a synchronized update.
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
            && self
                .last_output
                .is_none_or(|last| now.saturating_duration_since(last) >= QUIET);
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
        self.paced = now + self.interval;
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        if !self.presentable || !self.pending {
            return None;
        }
        Some(
            self.output_due
                .map_or(self.paced, |output| output.min(self.paced)),
        )
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

    #[test]
    fn the_first_output_after_idle_is_due_at_once() {
        let start = Instant::now();
        let mut schedule = published(start);
        let typed = start + ms(500);

        schedule.output(typed);

        assert!(schedule.deadline().is_some_and(|due| due <= typed));
        assert!(schedule.take_due(typed));
    }

    #[test]
    fn output_within_the_quiet_period_waits_for_quiet() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        schedule.output(start);
        assert!(schedule.take_due(start));
        schedule.mark_presented(start);

        schedule.output(start + ms(1));
        schedule.output(start + ms(2));

        let quiet = start + ms(2) + QUIET;
        assert_eq!(schedule.deadline(), Some(quiet));
        assert!(!schedule.take_due(quiet - Duration::from_micros(1)));
        assert!(schedule.take_due(quiet));
    }

    #[test]
    fn continuous_output_publishes_once_per_frame_interval() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        schedule.output(start);
        assert!(schedule.take_due(start));
        schedule.mark_presented(start);

        let mut publications = Vec::new();
        for millis in 1..=50 {
            let now = start + ms(millis);
            schedule.output(now);
            if schedule.take_due(now) {
                schedule.mark_presented(now);
                publications.push(millis);
            }
        }

        // 60 Hz: one frame is 16.667 ms, so each publication follows on the next millisecond.
        assert_eq!(publications, [17, 34]);
    }

    #[test]
    fn a_faster_frame_rate_shortens_the_cap() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        schedule.set_frame_rate(120);
        schedule.output(start);
        assert!(schedule.take_due(start));
        schedule.mark_presented(start);

        schedule.output(start + ms(1));
        schedule.output(start + ms(4));
        schedule.output(start + ms(7));

        assert_eq!(
            schedule.deadline(),
            Some(start + Duration::from_secs(1) / 120)
        );
    }

    #[test]
    fn more_output_never_postpones_a_due_publication() {
        let start = Instant::now();
        let mut schedule = published(start - ms(100));
        schedule.output(start);
        assert!(schedule.take_due(start));
        schedule.mark_presented(start);
        schedule.output(start + ms(1));

        // The worker was busy past the quiet deadline at 5 ms when more output arrived.
        schedule.output(start + ms(6));

        assert_eq!(schedule.deadline(), Some(start + ms(5)));
        assert!(schedule.take_due(start + ms(6)));
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
        assert_eq!(schedule.deadline(), Some(start + ms(1)));
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
    fn output_after_a_paced_request_is_not_idle() {
        let start = Instant::now();
        let mut schedule = published(start);
        schedule.request();

        schedule.output(start + ms(1));

        assert_eq!(schedule.deadline(), Some(start + ms(1) + QUIET));
    }
}
