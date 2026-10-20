//! Prepares selections on the terminal worker and versions their content independently of frames.
use super::*;

pub(super) type PreparedSelection = Result<Option<Arc<[u8]>>, SelectionCopyError>;

pub(super) struct ServerReading {
    revision: u64,
    observed: Option<u64>,
    copy: Result<Option<SelectionCopy>, SelectionCopyError>,
    bytes: PreparedSelection,
}

impl Default for ServerReading {
    fn default() -> Self {
        Self {
            revision: 1,
            observed: None,
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
        let generation = emulator.selection_copy_generation();
        if generation.is_some() && generation == self.observed {
            emulator.set_selection_revision(self.revision);
            return;
        }
        self.observed = generation;
        let copy = emulator
            .selection_copy(SelectionCopyOptions::default())
            .map_err(|_| SelectionCopyError::Formatting);
        if copy != self.copy {
            self.revision = self.revision.saturating_add(1);
            self.bytes = copy.as_ref().map_err(|error| *error).and_then(|copy| {
                copy.as_ref()
                    .map(|copy| encode(copy, spaceterm_server::pane::reading::MAX_SELECTION_BYTES))
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

fn encode(copy: &SelectionCopy, limit: usize) -> Result<Arc<[u8]>, SelectionCopyError> {
    #[derive(serde::Serialize)]
    struct Selection<'a> {
        plain_text: &'a str,
        html: Option<&'a str>,
    }
    struct BoundedBytes {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for BoundedBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(
        &mut output,
        &Selection {
            plain_text: &copy.plain_text,
            html: copy.html.as_deref(),
        },
    )
    .map_err(|_| SelectionCopyError::Formatting)?;
    Ok(output.bytes.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_selection_enforces_the_serialized_byte_limit_with_escaping() {
        let copy = SelectionCopy {
            plain_text: "界\n\"\u{0001}".into(),
            html: Some("<b>界</b>".into()),
        };
        let expected = serde_json::to_vec(&spaceterm_server::pane::reading::Selection {
            plain_text: copy.plain_text.to_string(),
            html: copy.html.as_ref().map(ToString::to_string),
        })
        .unwrap();
        assert_eq!(encode(&copy, expected.len()).unwrap().as_ref(), expected);
        assert_eq!(
            encode(&copy, expected.len() - 1),
            Err(SelectionCopyError::Formatting)
        );
    }
}
