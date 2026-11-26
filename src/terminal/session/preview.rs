//! Read-only requests for the visible grid, ordered with that worker's PTY output.

use super::*;
use crate::terminal::emulator::PreviewGrid;
use spaceterm_core::protocol::Refusal;

pub(crate) enum PreviewCapture {
    Current(Box<PreviewGrid>),
    Published {
        ended: bool,
        screen: Option<Arc<ScreenSnapshot>>,
    },
}

pub(super) struct PreviewRead {
    events: TerminalEventPublisher,
    answer: Option<Box<dyn FnOnce(PreviewCapture) + Send>>,
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
        };
        if let Some(commands) = &self.commands {
            // A worker that stops before reading drops this request and settles its fallback.
            let _ = commands.send(Command::ReadPreview(read));
        }
        Ok(())
    }
}

impl TerminalWorker {
    pub(super) fn read_preview(&self, read: PreviewRead) -> bool {
        let grid = self.emulator.preview_grid().ok().flatten();
        read.finish(grid);
        true
    }
}

impl PreviewRead {
    fn finish(mut self, grid: Option<PreviewGrid>) {
        if let Some(answer) = self.answer.take() {
            let screen = self.events.finish_preview(grid.is_none());
            answer(match grid {
                Some(grid) => PreviewCapture::Current(Box::new(grid)),
                // Synchronized output is still collecting changes. Its last published screen
                // is the one already visible to the person.
                None => PreviewCapture::Published {
                    ended: false,
                    screen,
                },
            });
        }
    }
}

impl Drop for PreviewRead {
    fn drop(&mut self) {
        if let Some(answer) = self.answer.take() {
            answer(PreviewCapture::Published {
                ended: true,
                screen: self.events.finish_preview(true),
            });
        }
    }
}
