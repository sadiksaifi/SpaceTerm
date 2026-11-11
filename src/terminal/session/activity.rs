//! Work observations and receipt generations for the idle-restart lease of ADR 0065.
//! The native reader and host admission stamp activity before worker processing.
use super::*;
use crate::platform::native_pty::SessionProbe;
use std::io;

/// How long a Terminal Session has no input and no output before it can have no work going on:
/// the idle period of ADR 0065, which SpaceTerm Core's update engine owns.
pub(crate) const IDLE_PERIOD: Duration = spaceterm_core::service::updates::IDLE_PERIOD;

/// What the worker last recorded for the work-going-on predicate.
pub(super) struct ActivityState {
    pub(super) receipt: spaceterm_core::idle_restart::Activity,
    /// The last input to the PTY or output from it. None before the first.
    last: Option<Instant>,
    /// The owning shell waited at its own empty prompt after `last`.
    empty_prompt: bool,
    probe: Option<Arc<dyn SessionProbe>>,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            receipt: spaceterm_core::idle_restart::Activity::new(Instant::now()),
            last: None,
            empty_prompt: false,
            probe: None,
        }
    }
}

impl ActivityState {
    pub(super) fn set_probe(&mut self, probe: Option<Arc<dyn SessionProbe>>) {
        self.probe = probe;
    }

    /// Input or output arrived at `now`, after which the owning shell `empty_prompt` waits at
    /// its own empty prompt or not.
    pub(super) fn note(&mut self, now: Instant, empty_prompt: bool) {
        self.last = Some(now);
        self.empty_prompt = empty_prompt;
    }
}

/// A Terminal Session has no work going on only when its shell owns the PTY foreground, waits
/// at its own empty prompt, shares its terminal session with no other process, and had no
/// input or output for [`IDLE_PERIOD`]. `others` reads the terminal session's processes; it runs
/// last, and only when every other condition holds. An error counts as work.
pub(super) fn work_going_on(
    quiet: Option<Duration>,
    shell_foreground: bool,
    empty_prompt: bool,
    others: impl FnOnce() -> io::Result<bool>,
) -> bool {
    let quiet = quiet.is_some_and(|quiet| quiet >= IDLE_PERIOD);
    !(quiet && shell_foreground && empty_prompt && matches!(others(), Ok(false)))
}

impl TerminalRuntime {
    pub(crate) fn receipt_activity(&self) -> spaceterm_core::idle_restart::Activity {
        self.event_publisher.receipt_activity()
    }
    pub(crate) fn idle_session(&self) -> Option<Arc<dyn spaceterm_core::idle_restart::Session>> {
        let state = self
            .event_publisher
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.activity.probe.as_ref()?.activity()?;
        drop(state);
        Some(Arc::new(self.event_publisher.clone()))
    }

    /// Whether this Terminal Session has work going on at `now`, by [`work_going_on`]. A
    /// stopped Terminal Session, a platform without process facts, and a shell without
    /// prompt markers count as work.
    pub(crate) fn work_going_on(&self, now: Instant) -> bool {
        !self
            .event_publisher
            .receipt_activity()
            .quiet(now, IDLE_PERIOD)
            || self.event_publisher.work_going_on(now)
    }
}

impl TerminalEventPublisher {
    pub(super) fn receipt_activity(&self) -> spaceterm_core::idle_restart::Activity {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .receipt
            .clone()
    }

    /// A no-work observation requires fresh foreground and process reads. The published line mode
    /// and an earlier absence of processes cannot prove that a session is still quiet.
    pub(super) fn work_going_on(&self, now: Instant) -> bool {
        let (quiet, running, empty_prompt, probe, last) = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let activity = &state.activity;
            (
                activity
                    .last
                    .map(|last| now.saturating_duration_since(last)),
                !state.stopped,
                activity.empty_prompt,
                activity.probe.clone(),
                activity.last,
            )
        };
        let working = work_going_on(quiet, running, empty_prompt, || {
            let probe = probe.ok_or_else(|| io::Error::other("no process facts"))?;
            if !probe.shell_foreground()? {
                return Ok(true);
            }
            probe.others()
        });
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        working || state.stopped || state.activity.last != last
    }

    pub(super) fn set_probe(&self, probe: Option<Arc<dyn SessionProbe>>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.set_probe(probe);
    }

    pub(super) fn note_activity(&self, now: Instant, empty_prompt: bool) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.note(now, empty_prompt);
    }
}

impl spaceterm_core::idle_restart::Session for TerminalEventPublisher {
    fn activity(&self) -> spaceterm_core::idle_restart::Activity {
        self.receipt_activity()
    }
    fn working(&self, now: Instant) -> bool {
        !self.receipt_activity().quiet(now, IDLE_PERIOD) || self.work_going_on(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Option<Duration> = Some(IDLE_PERIOD);

    fn nothing_else() -> io::Result<bool> {
        Ok(false)
    }

    #[test]
    fn only_a_bare_shell_at_an_empty_prompt_after_an_hour_of_silence_has_no_work_going_on() {
        assert!(!work_going_on(HOUR, true, true, nothing_else));
    }

    #[test]
    fn input_or_output_within_the_hour_is_work() {
        let recent = Some(IDLE_PERIOD - Duration::from_secs(1));
        assert!(work_going_on(recent, true, true, nothing_else));
        assert!(
            work_going_on(None, true, true, nothing_else),
            "no I/O seen yet"
        );
    }

    #[test]
    fn a_program_in_the_foreground_is_work() {
        // An editor, SSH, a nested multiplexer client, a REPL or a password prompt holds the
        // foreground; a failed tcgetpgrp reports no shell foreground either.
        assert!(work_going_on(HOUR, false, true, nothing_else));
    }

    #[test]
    fn a_shell_away_from_its_empty_prompt_is_work() {
        // A typed but unsent line, a quiet running command, or a shell without markers.
        assert!(work_going_on(HOUR, true, false, nothing_else));
    }

    #[test]
    fn another_process_in_the_terminal_session_is_work() {
        // A stopped job, a background job, or a helper the prompt starts.
        assert!(work_going_on(HOUR, true, true, || Ok(true)));
    }

    #[test]
    fn a_terminal_session_whose_processes_cannot_be_read_is_work() {
        assert!(work_going_on(HOUR, true, true, || Err(io::Error::other(
            "unreadable"
        ))));
    }

    #[test]
    fn the_processes_are_read_only_once_every_other_condition_holds() {
        let read = std::cell::Cell::new(false);
        let probe = || {
            read.set(true);
            Ok(false)
        };
        assert!(work_going_on(HOUR, false, true, probe));
        assert!(!read.get());
    }
}
