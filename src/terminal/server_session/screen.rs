//! Applies pushed screen frames to desktop snapshots and encodes the presentation style.
use super::screen_decoder::{Presenter, ScreenDecoder, UpdateError};
use super::*;
use spaceterm_server::protocol::screen::{Cursor, History, ScreenUpdate};
use spaceterm_server::protocol::{Frame, PaneEvent};

/// Receives one lease's server screens for the desktop presentation.
pub(super) struct ScreenReceiver<'a> {
    /// Updates apply only to this exact lease's own previous frame.
    decoder: ScreenDecoder,
    presenter: Presenter,
    local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
}
impl<'a> ScreenReceiver<'a> {
    pub(super) fn new(
        local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
        remote_metadata: Option<&super::super::metadata::RemoteTerminalMetadataContext>,
    ) -> Self {
        Self {
            decoder: ScreenDecoder::default(),
            presenter: Presenter::new(remote_metadata),
            local_files,
        }
    }

    /// Applies a frame with its graphics, and maps it to the server's generation and size.
    /// Returns the screen, its mapping and its accessibility revision. A frame whose base this
    /// receiver does not hold breaks the channel's credit rule.
    pub(super) fn receive(
        &mut self,
        frame: Frame,
        epoch: u64,
        generation: &mut PresentationGeneration,
        active_size: PaneSize,
        graphics: &mut graphics::Graphics,
        appearance: &Mutex<TerminalAppearanceUpdate>,
    ) -> Result<(Arc<ScreenSnapshot>, ScreenMapping, u64), ServerError> {
        let decoded =
            match self
                .decoder
                .apply(frame.frame, frame.base, frame.update, self.local_files)
            {
                Ok(decoded) => decoded,
                Err(UpdateError::UnknownBase | UpdateError::Malformed) => {
                    return Err(ServerError::Protocol);
                }
            };
        let graphics_error = graphics
            .update(decoded.graphics.clone(), decoded.modes.alternate_screen)
            .err();
        if let Some(error) = graphics_error {
            graphics.clear();
            graphics.failure(error);
        }
        graphics.recovered();
        *generation = generation.next();
        let mapping = ScreenMapping {
            epoch,
            local: *generation,
            wire: decoded.generation,
            size: PaneSize {
                columns: decoded.geometry.columns,
                rows: decoded.geometry.rows,
                cell_width: decoded
                    .geometry
                    .cell_pixels
                    .map_or(active_size.cell_width, |p| p.0),
                cell_height: decoded
                    .geometry
                    .cell_pixels
                    .map_or(active_size.cell_height, |p| p.1),
            },
            mouse_tracking: decoded.modes.mouse_tracking,
            alternate: decoded.modes.alternate_screen,
            alternate_scroll: decoded.modes.alternate_scroll,
        };
        let mut screen = self.presenter.snapshot(
            decoded,
            *generation,
            &appearance.lock().unwrap_or_else(|e| e.into_inner()),
        );
        screen.graphics = graphics.snapshot();
        Ok((Arc::new(screen), mapping, decoded.accessibility_revision))
    }
}

/// Applies a Pane worker's pushed frames to its screen, for [`super::inbox::Inbox::drain`].
pub(super) struct Frames<'a, 'b> {
    pub(super) screens: &'a mut ScreenReceiver<'b>,
    pub(super) epoch: u64,
    pub(super) generation: &'a mut PresentationGeneration,
    pub(super) active_size: PaneSize,
    pub(super) graphics: &'a mut graphics::Graphics,
    pub(super) appearance: &'a Mutex<TerminalAppearanceUpdate>,
    pub(super) semantic: &'a mut super::accessibility::Fetcher,
    /// What the view shows of the last visible frame.
    pub(super) shown: &'a mut Option<Shown>,
    /// Judges predictions against each frame and follows the line state in channel order.
    pub(super) predictor: &'a mut super::prediction::Predictor,
}

/// The parts of a frame the view shows besides its rows, modes and graphics.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Shown {
    cursor: Cursor,
    history: History,
    generation: u64,
}

impl Shown {
    fn of(update: &ScreenUpdate) -> Self {
        Self {
            cursor: update.cursor,
            history: update.history,
            generation: update.generation,
        }
    }
}

/// Whether `update` changes nothing the view shows, such as only the accessibility revision.
fn invisible(update: &ScreenUpdate, shown: Option<Shown>) -> bool {
    shown == Some(Shown::of(update))
        && update.rows.is_empty()
        && update.shift.is_none()
        && update.geometry.is_none()
        && update.modes.is_none()
        && update.colors.is_none()
        && update.graphics.is_none()
}

impl super::inbox::Apply for Frames<'_, '_> {
    type Screen = (Arc<ScreenSnapshot>, ScreenMapping);
    type Error = ServerError;

    fn apply(&mut self, frame: Frame) -> Result<Option<Self::Screen>, ServerError> {
        let invisible = frame.base.is_some() && invisible(&frame.update, *self.shown);
        let shown = Shown::of(&frame.update);
        let input = frame.input;
        let previous = *self.generation;
        let (screen, mapping, revision) = self.screens.receive(
            frame,
            self.epoch,
            self.generation,
            self.active_size,
            self.graphics,
            self.appearance,
        )?;
        self.semantic.note_revision(revision);
        self.predictor
            .frame(&screen, input, std::time::Instant::now());
        if invisible {
            // The view keeps its screen, so pointer coordinates keep its generation.
            *self.generation = previous;
            return Ok(None);
        }
        *self.shown = Some(shown);
        Ok(Some((screen, mapping)))
    }

    fn event(&mut self, event: &PaneEvent) {
        if let PaneEvent::Line(line) = event {
            self.predictor.line(*line);
        }
    }
}

pub(super) fn clear_presentation(
    events: &async_channel::Sender<TerminalSessionEvent>,
    generation: &mut PresentationGeneration,
    appearance: &Mutex<TerminalAppearanceUpdate>,
) {
    *generation = generation.next();
    let mut empty = (*ScreenSnapshot::empty_with_appearance(
        crate::local_path::LocalPathSemantics::Posix,
        &appearance.lock().unwrap_or_else(|e| e.into_inner()),
    ))
    .clone();
    empty.generation = *generation;
    let _ = events.force_send(TerminalSessionEvent::Screen(Arc::new(empty)));
}

pub(super) fn encode_style(update: &TerminalAppearanceUpdate) -> pane::PresentationStyle {
    let colors = &update.appearance.colors;
    let rgb = |c: crate::appearance::Color| [c.r, c.g, c.b];
    let mut palette = [[0; 3]; 16];
    for (index, color) in colors
        .normal
        .iter()
        .chain(colors.bright.iter())
        .copied()
        .enumerate()
    {
        palette[index] = rgb(color);
    }
    pane::PresentationStyle {
        light: update.appearance.appearance == crate::appearance::Appearance::Light,
        foreground: rgb(colors.foreground),
        background: rgb(colors.background),
        palette,
        cursor: rgb(colors.cursor),
        cursor_text: colors.cursor_text.map(rgb),
        bold_as_bright: update.appearance.bold_as_bright,
    }
}
pub(super) fn decode_hyperlink(
    link: &pane::effects::Hyperlink,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Option<Arc<super::super::hyperlink::HyperlinkTarget>> {
    use super::super::hyperlink::{HyperlinkTarget, MAX_LINK_BYTES};
    let target = match link {
        pane::effects::Hyperlink::Url(value) => HyperlinkTarget::url(value)?,
        pane::effects::Hyperlink::File(value) => {
            let filesystem = local_files?;
            if value.len() > MAX_LINK_BYTES
                || !filesystem
                    .path_semantics()
                    .is_absolute(std::path::Path::new(value))
            {
                return None;
            }
            HyperlinkTarget::from_local_file(
                filesystem.local_file(value, std::path::Path::new("/"))?,
            )?
        }
    };
    Some(Arc::new(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_file_links_need_client_authority_and_revalidate_before_preview() {
        use crate::terminal::{TerminalLocalFileCapabilities, native_services::FilePreviewTarget};
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-client-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = std::fs::canonicalize(directory).unwrap();
        let path = directory.join("preview");
        std::fs::write(&path, b"fixture").unwrap();
        let link = pane::effects::Hyperlink::File(path.to_str().unwrap().into());
        assert!(decode_hyperlink(&link, None).is_none());
        let filesystem = crate::platform::runtime_local_filesystem();
        let target = decode_hyperlink(&link, Some(&filesystem)).unwrap();
        assert!(
            FilePreviewTarget::from_link(&target, TerminalLocalFileCapabilities::Disabled)
                .is_none()
        );
        let preview =
            FilePreviewTarget::from_link(&target, TerminalLocalFileCapabilities::Enabled).unwrap();
        assert_eq!(preview.revalidated_path(), Some(path.clone()));
        std::fs::rename(&path, directory.join("original")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(preview.revalidated_path().is_none());
        assert!(
            decode_hyperlink(
                &pane::effects::Hyperlink::File("relative".into()),
                Some(&filesystem)
            )
            .is_none()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
