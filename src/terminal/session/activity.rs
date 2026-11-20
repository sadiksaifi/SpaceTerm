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
    /// Lines submitted, such as by Enter, counted where input is accepted and before the worker
    /// writes them, so a close the host admits after that input sees it.
    submitted: Submissions,
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
///
/// A shell that draws no prompt of its own, as without shell integration, never answers a line it
/// reads. Its oldest line is answered instead when it was typed at the shell's line editor, a
/// program took the foreground while it was the oldest, and a fresh read then finds the shell's
/// line editor waiting again with no other process. The line editor waits only once the shell has
/// finished the line, which a builtin after the program, as in `sleep 3; read answer`, has not.
/// Each completion answers only the oldest line, since a line typed ahead of the program is read
/// by that same line editor and may run a builtin of its own.
#[derive(Default)]
struct Submissions {
    accepted: u64,
    written: u64,
    answered: u64,
    /// Each written line nothing answered yet, oldest first.
    awaiting: std::collections::VecDeque<Line>,
    /// A program other than the shell owned the foreground after the last written line.
    started: bool,
    /// The most fresh prompts of its own the owning shell has drawn.
    prompts: u64,
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

struct Line {
    /// Which written line it is, counting from one.
    number: u64,
    /// The count of fresh prompts the owning shell must have drawn before one answers it. Only a
    /// line the shell may read has one.
    needs: Option<u64>,
    /// The owning shell's line editor waited for it when the worker wrote it, and the shell had
    /// drawn no prompt of its own.
    at_line_editor: bool,
    /// A program other than the shell owned the foreground while this was the oldest line.
    ran: bool,
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
    /// `reader` was to read it. `line_editor` says the shell's line editor read it.
    fn written(&mut self, progress: PromptProgress, reader: Reader, line_editor: bool) {
        self.written = (self.written + 1).min(self.accepted);
        self.prompts = self.prompts.max(progress.fresh);
        let number = self.written;
        // A line typed at a prompt is read there and answered by the next fresh prompt. A line
        // written while the shell runs a command, starts, or runs a program that may leave the
        // line unread is read at a prompt yet to come, which may already wait unread, so only the
        // fresh prompt after that one answers it. A program that reads keys answers its lines
        // itself by giving the foreground back.
        let reads_at = match reader {
            Reader::Program => {
                self.started = true;
                self.awaiting.push_back(Line {
                    number,
                    needs: None,
                    at_line_editor: false,
                    ran: false,
                });
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
        self.awaiting.push_back(Line {
            number,
            needs: Some(needs),
            at_line_editor: reader == Reader::Shell
                && line_editor
                && progress.zone == ShellZone::Starting
                && self.prompts == 0,
            ran: false,
        });
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
        self.prompts = self.prompts.max(fresh);
        self.answer(|line| line.needs.is_some_and(|needs| needs <= fresh));
    }

    /// A program other than the shell owns the foreground.
    fn program_running(&mut self) {
        self.started = true;
        if let Some(oldest) = self.awaiting.front_mut() {
            oldest.ran = true;
        }
    }

    /// The oldest line, by number, that the shell's line editor waiting again would answer: one
    /// typed at that line editor, a program having run while it was the oldest, from a shell
    /// that draws no prompt of its own.
    fn completable(&self) -> Option<u64> {
        let oldest = self.awaiting.front()?;
        (self.prompts == 0 && oldest.at_line_editor && oldest.ran).then_some(oldest.number)
    }

    /// A fresh read found the shell's line editor waiting with no other process after the line
    /// [`Self::completable`] named. Answers that line alone, if it is still the oldest.
    fn completed(&mut self, number: u64) {
        if self.completable() == Some(number) {
            self.answer(|line| line.number == number);
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellZone {
    /// No prompt of its own yet.
    Starting,
    /// At its prompt, with or without input.
    AtPrompt,
    /// Running a command line, or finishing one before its next prompt.
    Running,
}

/// The owning shell's fresh prompts, ever, and where it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PromptProgress {
    pub(crate) fresh: u64,
    pub(crate) zone: ShellZone,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            receipt: spaceterm_core::idle_restart::Activity::new(Instant::now()),
            last: None,
            empty_prompt: false,
            probe: None,
            submitted: Submissions::default(),
        }
    }
}

impl ActivityState {
    pub(super) fn set_probe(&mut self, probe: Option<Arc<dyn SessionProbe>>) {
        self.probe = probe;
    }

    /// Input or output arrived at `now`, after which the owning shell `empty_prompt` waits at
    /// its own empty prompt or not, having drawn `fresh` fresh prompts.
    pub(super) fn note(&mut self, now: Instant, empty_prompt: bool, fresh: u64) {
        self.last = Some(now);
        self.empty_prompt = empty_prompt;
        self.submitted.prompted(fresh);
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
    /// See [`TerminalEventPublisher::running_work`].
    pub(crate) fn running_work(&self) -> bool {
        self.event_publisher.running_work()
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

    pub(super) fn note_activity(&self, now: Instant, empty_prompt: bool, fresh: u64) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.activity.note(now, empty_prompt, fresh);
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

    /// The PTY accepted the next submitted line whole, in the order it was accepted, while the
    /// owning shell was as `progress` says. `canonical` is the PTY's ICANON mode then, which says
    /// whether a program holding the foreground reads keys itself. A mode that can't be read
    /// leaves who reads the line open. `line_editor` says the owning shell held the foreground
    /// with ICANON off just before the worker wrote the line, so its line editor read it.
    pub(super) fn note_written(
        &self,
        progress: PromptProgress,
        canonical: Option<bool>,
        line_editor: bool,
    ) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let activity = &mut state.activity;
        let program = activity
            .probe
            .as_ref()
            .is_some_and(|probe| matches!(probe.shell_foreground(), Ok(false)));
        let reader = match (program, canonical) {
            (false, _) => Reader::Shell,
            (true, Some(false)) => Reader::Program,
            (true, _) => Reader::LineProgram,
        };
        activity.submitted.written(progress, reader, line_editor);
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
            Some(Ok(true)) => {
                if activity.submitted.started {
                    activity.submitted.returned();
                }
                drop(state);
                self.complete_oldest();
            }
            _ => {}
        }
    }

    /// Answers the oldest line that [`Submissions::completable`] names when a fresh read finds
    /// the shell's line editor waiting with no other process in its terminal session. A fresh
    /// read that finds a program in the foreground records that it ran.
    fn complete_oldest(&self) {
        let (probe, number) = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let activity = &mut state.activity;
            let Some(probe) = activity.probe.clone() else {
                return;
            };
            if activity.submitted.awaiting.is_empty() {
                return;
            }
            if matches!(probe.shell_foreground(), Ok(false)) {
                activity.submitted.program_running();
                return;
            }
            let Some(number) = activity.submitted.completable() else {
                return;
            };
            (probe, number)
        };
        if matches!(probe.line_editor(), Ok(true)) && matches!(probe.others(), Ok(false)) {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.activity.submitted.completed(number);
        }
    }

    /// Whether current facts verify work in this Terminal Session that a direct close would end:
    /// a fresh read finds a program other than the shell owning the foreground or other processes
    /// in its terminal session, or a submitted line that nothing answered yet. Facts it cannot
    /// read verify nothing, so a shell that is starting or waits at its prompt closes directly.
    pub(super) fn running_work(&self) -> bool {
        self.complete_oldest();
        let (probe, submitted, stopped) = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.activity.probe.clone(),
                state.activity.submitted.pending(),
                state.stopped,
            )
        };
        if stopped {
            return false;
        }
        if submitted {
            return true;
        }
        probe.is_some_and(|probe| {
            matches!(probe.shell_foreground(), Ok(false)) || matches!(probe.others(), Ok(true))
        })
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
