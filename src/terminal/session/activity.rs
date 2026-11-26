//! Observations of work that closing or restarting a Terminal Session would end.
//! The native reader and host admission stamp activity before worker processing.
use super::*;
use crate::platform::native_pty::{CloseActivityRead, SessionProbe};
use spaceterm_core::hierarchy::Foreground;
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
    prompt: PromptProgress,
    probe: Option<Arc<dyn SessionProbe>>,
    /// Lines submitted, such as by Enter, counted where input is accepted and before the worker
    /// writes them, so a close the host admits after that input sees it.
    submitted: Submissions,
    /// Latest close observation sampled by the Terminal Session worker.
    close: CloseActivity,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CloseActivity {
    pub(crate) foreground: Foreground,
    pub(crate) needs_close_review: bool,
}

/// The ordered evidence that answers each submitted line. Input reaches the worker in the order
/// it was accepted, so the `n`th line the PTY accepts whole is the `n`th one accepted. Only
/// evidence the worker observes after the PTY accepted a line answers it. A line the owning shell
/// reads is answered only by the fresh prompt that follows the prompt it is read at, since each
/// fresh prompt follows exactly one line the shell read. A program's return to the shell answers
/// none of them: the line that started the program, or a line typed ahead of it, may go on in the
/// shell, as `sleep 3; read answer` does. That return answers only the lines the program read
/// itself. Output alone answers nothing, since the shell itself echoes and ends the line before
/// the program it runs takes the foreground.
#[derive(Default)]
struct Submissions {
    accepted: u64,
    written: u64,
    answered: u64,
    /// Each written line nothing answered yet, oldest first.
    awaiting: std::collections::VecDeque<Line>,
    /// A program other than the shell owned the foreground after the last written line.
    started: bool,
    /// Who held the PTY just before the worker's latest write while a submitted line waited to
    /// be written. None before any such write, and where nothing could be read.
    before_write: Option<Holder>,
}

/// Who held the PTY at one moment: whether a program other than the owning shell owned its
/// foreground, and its ICANON mode when it could be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) program: bool,
    pub(crate) canonical: Option<bool>,
}

/// Who reads a written line, by who held the PTY when the PTY accepted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reader {
    /// The owning shell, at a prompt or once it finishes what it runs.
    Shell,
    /// A program that reads keys itself, such as an editor, a pager or SSH. Its return to the
    /// shell answers the line.
    Program,
    /// A program the kernel collects whole lines for, such as `sleep` or `cat`. It may never read
    /// the line, and then the shell reads and runs it after the program ends, so the program's
    /// return answers nothing.
    LineProgram,
}

impl Reader {
    /// Who reads a line, from who held the PTY just before the write that completed it and just
    /// after. A program reads keys itself only when it held the foreground with ICANON off on
    /// both sides of that write. A program that took the foreground during the write, as one the
    /// line itself starts can before the worker reads it, read none of the line, and a program
    /// that gave the foreground back during it left the line to the shell. A program that held
    /// the foreground across the write in any other mode is one the kernel collects lines for.
    /// A side nothing could read leaves the line to the shell, whose own prompt must answer it.
    pub(crate) fn at_write(before: Option<Holder>, after: Holder) -> Self {
        match before {
            Some(before) if before.program && after.program => {
                if before.canonical == Some(false) && after.canonical == Some(false) {
                    Self::Program
                } else {
                    Self::LineProgram
                }
            }
            _ => Self::Shell,
        }
    }
}

struct Line {
    /// The count of fresh prompts the owning shell must have drawn before one answers it. Only a
    /// line the shell may read has one.
    needs: Option<u64>,
}

impl Line {
    /// The program holding the foreground reads this line itself.
    const fn program(&self) -> bool {
        self.needs.is_none()
    }
}

impl Submissions {
    const fn pending(&self) -> bool {
        self.accepted > self.answered
    }

    /// The PTY accepted the next line whole while the owning shell was as `progress` says, and
    /// `reader` was to read it.
    fn written(&mut self, progress: PromptProgress, reader: Reader) {
        self.written = (self.written + 1).min(self.accepted);
        // A line typed at a prompt is read there and answered by the next fresh prompt. A line
        // written while the shell runs a command, starts, or runs a program that may leave the
        // line unread is read at a prompt yet to come, which may already wait unread, so only the
        // fresh prompt after that one answers it. A program that reads keys answers its lines
        // itself by giving the foreground back.
        let reads_at = match reader {
            Reader::Program => {
                self.started = true;
                self.awaiting.push_back(Line { needs: None });
                return;
            }
            Reader::LineProgram => {
                self.started = true;
                progress.fresh + 1
            }
            Reader::Shell => match progress.zone {
                ShellZone::AtPrompt => progress.fresh,
                ShellZone::Running | ShellZone::Starting => progress.fresh + 1,
            },
        };
        // Each line the shell reads is read at the prompt that answers the one before it, at the
        // earliest.
        let needs = self
            .awaiting
            .iter()
            .rev()
            .find_map(|before| before.needs)
            .map_or(reads_at + 1, |before| (reads_at + 1).max(before + 1));
        self.awaiting.push_back(Line { needs: Some(needs) });
    }

    /// Removes the awaiting lines `answers` says are answered.
    fn answer(&mut self, answers: impl Fn(&Line) -> bool) {
        let before = self.awaiting.len();
        self.awaiting.retain(|line| !answers(line));
        let answered = (before - self.awaiting.len()) as u64;
        self.answered = (self.answered + answered).min(self.written);
    }

    /// The owning shell has drawn `fresh` fresh prompts. Each answers at most one line it read.
    fn prompted(&mut self, fresh: u64) {
        self.answer(|line| line.needs.is_some_and(|needs| needs <= fresh));
    }

    /// A program other than the shell owns the foreground.
    fn program_running(&mut self) {
        self.started = true;
    }

    /// The program gave the foreground back to the shell. It answers the lines it read itself.
    /// A line the kernel held for a program that never read it is the shell's to read now, and
    /// it already waits for the shell's fresh prompt.
    fn returned(&mut self) {
        self.started = false;
        self.answer(Line::program);
    }
}

/// Where the owning shell is, as its own prompt markers say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShellZone {
    /// No prompt of its own yet.
    #[default]
    Starting,
    /// At its prompt, with or without input.
    AtPrompt,
    /// Running a command line, or finishing one before its next prompt.
    Running,
}

/// The owning shell's fresh prompts, ever, and where it is now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptProgress {
    pub(crate) fresh: u64,
    pub(crate) zone: ShellZone,
    pub(crate) command_running: bool,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            receipt: spaceterm_core::idle_restart::Activity::new(Instant::now()),
            last: None,
            empty_prompt: false,
            prompt: PromptProgress::default(),
            probe: None,
            submitted: Submissions::default(),
            close: CloseActivity::default(),
        }
    }
}

impl ActivityState {
    pub(super) fn set_probe(&mut self, probe: Option<Arc<dyn SessionProbe>>) {
        self.probe = probe;
    }

    /// Input or output arrived at `now`, after which the owning shell `empty_prompt` waits at
    /// its own empty prompt or not, with the owning shell's latest activity.
    pub(super) fn note(&mut self, now: Instant, empty_prompt: bool, progress: PromptProgress) {
        self.last = Some(now);
        self.empty_prompt = empty_prompt;
        self.prompt = progress;
        self.submitted.prompted(progress.fresh);
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

    /// Whether current facts verify work that a direct close of this Terminal Session would end.
    pub(crate) fn running_work(&self) -> bool {
        self.event_publisher.running_work()
    }

    /// Last observed close facts published by the Terminal Session worker.
    pub(crate) fn close_activity(&self) -> CloseActivity {
        let state = self
            .event_publisher
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return CloseActivity::default();
        }
        CloseActivity {
            needs_close_review: state.activity.close.needs_close_review
                || state.activity.submitted.pending()
                || state.activity.prompt.command_running,
            ..state.activity.close
        }
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
        let checkpoint = probe.clone();
        let working = work_going_on(quiet, running, empty_prompt, || {
            let probe = probe.ok_or_else(|| io::Error::other("no process facts"))?;
            if !probe.shell_foreground()? {
                return Ok(true);
            }
            probe.others()
        });
        let working = working || checkpoint.is_none_or(|probe| probe.checkpoint().is_err());
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        working || state.stopped || state.activity.last != last
    }

    pub(super) fn set_probe(&self, probe: Option<Arc<dyn SessionProbe>>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.set_probe(probe);
    }

    pub(super) fn has_close_probe(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .probe
            .is_some()
    }

    pub(super) fn note_activity(&self, now: Instant, empty_prompt: bool, progress: PromptProgress) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.note(now, empty_prompt, progress);
    }

    /// Records that accepted input submits a line, before the worker writes it.
    pub(super) fn note_submitted(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.submitted.accepted += 1;
    }

    /// The worker put none of an accepted submission's bytes in the PTY's queue, as for a Run the
    /// shell's prompt refused, so it submitted no line.
    pub(super) fn retract_submitted(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let submitted = &mut state.activity.submitted;
        submitted.accepted = submitted.accepted.saturating_sub(1).max(submitted.written);
    }

    /// Whether an accepted submitted line waits for the PTY to accept it, so the next write may
    /// complete it.
    pub(super) fn awaits_written_line(&self) -> bool {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let submitted = &state.activity.submitted;
        submitted.accepted > submitted.written
    }

    /// The worker is about to write while a submitted line waits, and the PTY's ICANON mode is
    /// `canonical`. Who holds the PTY now is who held it as the write began.
    pub(super) fn note_write_start(&self, canonical: Option<bool>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let activity = &mut state.activity;
        activity.submitted.before_write = Some(Holder {
            program: Self::program_holds(activity.probe.as_deref()),
            canonical,
        });
    }

    /// Whether a fresh read finds a program other than the owning shell holding the foreground.
    fn program_holds(probe: Option<&dyn SessionProbe>) -> bool {
        probe.is_some_and(|probe| matches!(probe.shell_foreground(), Ok(false)))
    }

    /// The PTY accepted the next submitted line whole, in the order it was accepted, while the
    /// owning shell was as `progress` says. `canonical` is the PTY's ICANON mode now. Who reads
    /// the line is who held the PTY on both sides of the write that completed it, by
    /// [`Reader::at_write`].
    pub(super) fn note_written(&self, progress: PromptProgress, canonical: Option<bool>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let activity = &mut state.activity;
        let after = Holder {
            program: Self::program_holds(activity.probe.as_deref()),
            canonical,
        };
        let reader = Reader::at_write(activity.submitted.before_write, after);
        activity.submitted.written(progress, reader);
    }

    /// The worker read output after its writes. A program that took the foreground after a
    /// written line and gave it back to the shell answers the lines it ran or read.
    pub(super) fn note_output(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let activity = &mut state.activity;
        if activity.submitted.written == activity.submitted.answered {
            return;
        }
        match activity
            .probe
            .as_ref()
            .map(|probe| probe.shell_foreground())
        {
            Some(Ok(false)) => activity.submitted.program_running(),
            Some(Ok(true)) if activity.submitted.started => {
                activity.submitted.returned();
            }
            _ => {}
        }
    }

    /// Rechecks foreground work, unanswered input, and live session processes before a close.
    /// Disowned processes can still belong to the terminal session and end with it.
    fn observe_close_activity(&self, read: CloseActivityRead) -> (CloseActivity, bool) {
        let (probe, prompt, submitted, stopped) = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.activity.probe.clone(),
                state.activity.prompt,
                state.activity.submitted.pending(),
                state.stopped,
            )
        };
        if stopped {
            return (CloseActivity::default(), false);
        }
        let submitted_work = prompt.command_running || submitted;
        let (foreground, native_work) = probe
            .filter(|_| read == CloseActivityRead::Inventory || !submitted_work)
            .map_or((Foreground::Unknown, false), |probe| {
                probe.close_activity(prompt.zone == ShellZone::AtPrompt, read)
            });
        (
            CloseActivity {
                foreground,
                needs_close_review: native_work || submitted_work,
            },
            native_work,
        )
    }

    /// Publishes the same facts used by close admission. Return whether currently observed
    /// processes can make this stale without producing another input or output event.
    pub(super) fn publish_close_activity(&self) -> bool {
        let (observed, native_work) = self.observe_close_activity(CloseActivityRead::Inventory);
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.activity.close != observed {
            state.activity.close = observed;
            super::subscriptions::notify(state);
        }
        native_work
    }

    pub(super) fn running_work(&self) -> bool {
        self.observe_close_activity(CloseActivityRead::Admission)
            .0
            .needs_close_review
    }
}

impl spaceterm_core::idle_restart::Session for TerminalEventPublisher {
    fn freeze(
        &self,
        now: Instant,
    ) -> Result<
        Box<dyn spaceterm_core::idle_restart::Frozen>,
        spaceterm_core::idle_restart::Restarting,
    > {
        use spaceterm_core::idle_restart::Restarting;
        // The lease owns every receipt lock. Do not acquire one from this native operation.
        let (probe, last) = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopped
                || !state.activity.empty_prompt
                || !state
                    .activity
                    .last
                    .is_some_and(|last| now.saturating_duration_since(last) >= IDLE_PERIOD)
            {
                return Err(Restarting);
            }
            (
                state.activity.probe.clone().ok_or(Restarting)?,
                state.activity.last,
            )
        };
        let frozen = probe.freeze().map_err(|_| Restarting)?;
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped || !state.activity.empty_prompt || state.activity.last != last {
            return Err(Restarting);
        }
        Ok(frozen)
    }
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
    fn only_a_raw_reader_on_both_sides_of_the_write_reads_the_line_itself() {
        let shell = Holder {
            program: false,
            canonical: Some(false),
        };
        let raw = Holder {
            program: true,
            canonical: Some(false),
        };
        let lines = Holder {
            program: true,
            canonical: Some(true),
        };
        let unread = Holder {
            program: true,
            canonical: None,
        };
        assert_eq!(Reader::at_write(Some(raw), raw), Reader::Program);
        // A program the line itself started, which took the foreground during the write.
        assert_eq!(Reader::at_write(Some(shell), raw), Reader::Shell);
        // A program that gave the foreground back during the write.
        assert_eq!(Reader::at_write(Some(raw), shell), Reader::Shell);
        assert_eq!(Reader::at_write(None, raw), Reader::Shell);
        assert_eq!(Reader::at_write(Some(lines), lines), Reader::LineProgram);
        assert_eq!(Reader::at_write(Some(raw), lines), Reader::LineProgram);
        assert_eq!(Reader::at_write(Some(raw), unread), Reader::LineProgram);
        assert_eq!(Reader::at_write(Some(unread), raw), Reader::LineProgram);
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
