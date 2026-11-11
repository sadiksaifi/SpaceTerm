//! Whether a Terminal Session has work going on, which SpaceTerm Core's idle restart reads
//! (ADR 0065). The worker records each input and output; the Core reads the rest on demand,
//! without the worker.
use super::*;
use crate::platform::native_pty::SessionProbe;
use std::io;

/// How long a Terminal Session has no input and no output before it can have no work going on:
/// the idle period of ADR 0065, which SpaceTerm Core's update engine owns.
pub(crate) const IDLE_PERIOD: Duration = spaceterm_core::service::updates::IDLE_PERIOD;
/// How long one reading of the processes in a terminal session stays valid while no input or
/// output arrives.
const PROBE_VALID: Duration = Duration::from_secs(60);

/// What the worker last recorded for the work-going-on predicate.
#[derive(Default)]
pub(super) struct ActivityState {
    /// The last input to the PTY or output from it. None before the first.
    last: Option<Instant>,
    /// The owning shell waited at its own empty prompt after `last`.
    empty_prompt: bool,
    probe: Option<Arc<dyn SessionProbe>>,
    /// The last reading of `probe`, while no input or output arrived since.
    others: Option<(Instant, bool)>,
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
        self.others = None;
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
    /// Whether this Terminal Session has work going on at `now`, by [`work_going_on`]. A
    /// stopped Terminal Session, a platform without process facts, and a shell without
    /// prompt markers count as work.
    pub(crate) fn work_going_on(&self, now: Instant) -> bool {
        self.event_publisher.work_going_on(now)
    }
}

impl TerminalEventPublisher {
    /// See [`TerminalRuntime::work_going_on`]. One negative process reading stays valid for
    /// [`PROBE_VALID`] while no input or output arrives; nothing that ends a Terminal Session
    /// relies on it.
    pub(super) fn work_going_on(&self, now: Instant) -> bool {
        let (quiet, shell_foreground, empty_prompt, probe, reading) = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let activity = &state.activity;
            (
                activity
                    .last
                    .map(|last| now.saturating_duration_since(last)),
                !state.stopped && state.line.mode.shell_foreground,
                activity.empty_prompt,
                activity.probe.clone(),
                activity
                    .others
                    .filter(|(at, _)| now.saturating_duration_since(*at) < PROBE_VALID),
            )
        };
        work_going_on(quiet, shell_foreground, empty_prompt, || {
            if let Some((_, others)) = reading {
                return Ok(others);
            }
            let probe = probe.ok_or_else(|| io::Error::other("no process facts"))?;
            let others = probe.others()?;
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            // Input or output that arrived meanwhile cleared the reading; keep it cleared.
            if state.activity.last.is_some_and(|last| last <= now) {
                state.activity.others = Some((now, others));
            }
            Ok(others)
        })
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
