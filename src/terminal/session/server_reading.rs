//! Prepares selections on the terminal worker and versions their content independently of frames.
use super::*;

pub(super) type PreparedSelection = Result<Option<Arc<[u8]>>, SelectionCopyError>;

pub(super) struct ServerReading {
    revision: u64,
    copy: Result<Option<SelectionCopy>, SelectionCopyError>,
    bytes: PreparedSelection,
}

impl Default for ServerReading {
    fn default() -> Self {
        Self {
            revision: 1,
            copy: Ok(None),
            bytes: Ok(None),
        }
    }
}

impl ServerReading {
    pub(super) fn update(&mut self, emulator: &mut TerminalEmulator) {
        if emulator.synchronized_output_deadline().is_some() {
            return;
        }
        let copy = emulator
            .selection_copy(SelectionCopyOptions::default())
            .map_err(|_| SelectionCopyError::Formatting);
        if copy != self.copy {
            self.revision = self.revision.saturating_add(1);
            self.bytes = copy.as_ref().map_err(|error| *error).and_then(|copy| {
                copy.as_ref()
                    .map(|copy| {
                        let bytes =
                            serde_json::to_vec(&spaceterm_server::pane::reading::Selection {
                                plain_text: copy.plain_text.to_string(),
                                html: copy.html.as_ref().map(|text| text.to_string()),
                            })
                            .map_err(|_| SelectionCopyError::Formatting)?;
                        if bytes.len() > spaceterm_server::pane::reading::MAX_SELECTION_BYTES {
                            return Err(SelectionCopyError::Formatting);
                        }
                        Ok(Arc::from(bytes))
                    })
                    .transpose()
            });
            self.copy = copy;
        }
        emulator.set_selection_revision(self.revision);
    }

    pub(super) fn prepared(&self, revision: u64) -> PreparedSelection {
        if revision == self.revision {
            self.bytes.clone()
        } else {
            Ok(None)
        }
    }
}
