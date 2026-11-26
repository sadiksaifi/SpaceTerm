//! Read-only requests for the visible grid, ordered with that worker's PTY output.

use super::*;
use crate::terminal::emulator::PreviewGrid;
use spaceterm_core::protocol::Refusal;

/// An active synchronized transaction can keep the screen held indefinitely. Preview callers
/// get a bounded wait for a complete screen without ending the transaction.
const MAX_PREVIEW_WAIT: Duration = Duration::from_secs(1);

pub(crate) enum PreviewCapture {
    Live(Option<Box<PreviewGrid>>),
    Ended,
}

pub(super) struct PreviewRead {
    events: TerminalEventPublisher,
    answer: Option<Box<dyn FnOnce(PreviewCapture) + Send>>,
    deadline: Instant,
}

impl TerminalRuntime {
    pub(crate) fn read_preview(
        &self,
        answer: impl FnOnce(PreviewCapture) + Send + 'static,
    ) -> Result<(), Refusal> {
        if !self.event_publisher.begin_preview() {
            return Err(Refusal::Capacity);
        }
        let read = PreviewRead {
            events: self.event_publisher.clone(),
            answer: Some(Box::new(answer)),
            deadline: Instant::now() + MAX_PREVIEW_WAIT,
        };
        if let Some(commands) = &self.commands {
            // A worker that stops before reading drops this request and returns its status.
            let _ = commands.send(Command::ReadPreview(read));
        }
        Ok(())
    }
}

impl TerminalWorker {
    pub(super) fn read_preview(&mut self, read: PreviewRead) -> bool {
        match self.emulator.preview_grid() {
            Ok(None) if Instant::now() < read.deadline => self.schedules.preview = Some(read),
            grid => read.finish(grid.ok().flatten()),
        }
        true
    }

    pub(super) fn preview_deadline(&self) -> Option<Instant> {
        self.schedules.preview.as_ref().map(|read| read.deadline)
    }

    /// A held grid is not a complete preview. The current grid becomes readable when the
    /// transaction or prompt redraw ends, including when the Pane has no display channel.
    pub(super) fn settle_preview(&mut self, now: Instant) {
        if self.preview_deadline().is_some_and(|deadline| {
            now >= deadline || self.emulator.synchronized_output_deadline().is_none()
        }) && let Some(read) = self.schedules.preview.take()
        {
            self.read_preview(read);
        }
    }
}

impl PreviewRead {
    fn finish(mut self, grid: Option<PreviewGrid>) {
        if let Some(answer) = self.answer.take() {
            self.events.finish_preview();
            answer(PreviewCapture::Live(grid.map(Box::new)));
        }
    }
}

impl Drop for PreviewRead {
    fn drop(&mut self) {
        if let Some(answer) = self.answer.take() {
            self.events.finish_preview();
            answer(PreviewCapture::Ended);
        }
    }
}
