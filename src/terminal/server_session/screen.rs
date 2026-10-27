//! Applies server screen frames to desktop snapshots and encodes the presentation style.
use super::screen_decoder::{ScreenDecoder, UpdateError};
use super::*;
use spaceterm_server::protocol::interim::ScreenFrame;

/// Receives one lease's server screens for the desktop presentation.
pub(super) struct ScreenReceiver<'a> {
    /// Updates apply only to this exact lease's own previous frame.
    decoder: ScreenDecoder,
    local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
    remote_metadata: Option<&'a super::super::metadata::RemoteTerminalMetadataContext>,
}
impl<'a> ScreenReceiver<'a> {
    pub(super) fn new(
        local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
        remote_metadata: Option<&'a super::super::metadata::RemoteTerminalMetadataContext>,
    ) -> Self {
        Self {
            decoder: ScreenDecoder::default(),
            local_files,
            remote_metadata,
        }
    }

    /// The last applied frame, which the next poll names as its base.
    pub(super) fn baseline(&self) -> Option<u64> {
        self.decoder.frame()
    }

    /// A changed demand restarts from a keyframe.
    pub(super) fn restart(&mut self) {
        self.decoder.reset();
    }

    /// Applies a polled frame with its graphics, and maps it to the server's generation and
    /// size. Returns the screen, its mapping and its accessibility revision, or `None` when the
    /// frame does not apply to this receiver's frame; the next poll then asks for a keyframe.
    pub(super) fn receive(
        &mut self,
        frame: ScreenFrame,
        epoch: u64,
        generation: &mut PresentationGeneration,
        active_size: PaneSize,
        graphics: &mut graphics::Graphics,
        appearance: &Mutex<TerminalAppearanceUpdate>,
    ) -> Result<Option<(Arc<ScreenSnapshot>, ScreenMapping, u64)>, ServerError> {
        let decoded =
            match self
                .decoder
                .apply(frame.frame, frame.base, frame.update, self.local_files)
            {
                Ok(decoded) => decoded,
                Err(UpdateError::UnknownBase) => return Ok(None),
                Err(UpdateError::Malformed) => return Err(ServerError::Protocol),
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
            selection: decoded
                .reading
                .as_ref()
                .map_or(0, |reading| reading.selection_revision),
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
        let mut screen = decoded.snapshot(
            *generation,
            &appearance.lock().unwrap_or_else(|e| e.into_inner()),
        );
        if let Some(metadata) = self.remote_metadata {
            Arc::make_mut(&mut screen.metadata).context =
                super::super::metadata::TerminalMetadataContext::Remote(metadata.clone());
        }
        screen.graphics = graphics.snapshot();
        Ok(Some((
            Arc::new(screen),
            mapping,
            decoded.accessibility_revision,
        )))
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
