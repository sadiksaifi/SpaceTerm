//! Decodes server screens into desktop snapshots and encodes the presentation style.
use super::*;

/// Receives one lease's server screens for the desktop presentation.
pub(super) struct ScreenReceiver<'a> {
    /// Row changes apply only to this exact lease's own previous screen.
    decoder: pane::presentation::ScreenDecoder,
    local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
    remote_metadata: Option<&'a super::super::metadata::RemoteTerminalMetadataContext>,
}
impl<'a> ScreenReceiver<'a> {
    pub(super) fn new(
        local_files: Option<&'a crate::platform::local_filesystem::LocalFilesystemAuthority>,
        remote_metadata: Option<&'a super::super::metadata::RemoteTerminalMetadataContext>,
    ) -> Self {
        Self {
            decoder: pane::presentation::ScreenDecoder::default(),
            local_files,
            remote_metadata,
        }
    }

    pub(super) fn baseline(&self) -> Option<u64> {
        self.decoder.baseline()
    }

    /// A changed demand restarts row changes from a complete screen.
    pub(super) fn restart(&mut self) {
        self.decoder = pane::presentation::ScreenDecoder::default();
    }

    /// Completes a row-change reply against the previous screen.
    pub(super) fn apply(&mut self, reply: Reply) -> Result<Reply, ServerError> {
        self.decoder.apply(reply)
    }

    /// Decodes a complete screen with its graphics, and maps it to the server's generation and size.
    pub(super) fn decode(
        &self,
        mut screen: Box<pane::Screen>,
        epoch: u64,
        generation: &mut PresentationGeneration,
        active_size: PaneSize,
        graphics: &mut graphics::Graphics,
        appearance: &Mutex<TerminalAppearanceUpdate>,
    ) -> Result<(Arc<ScreenSnapshot>, ScreenMapping), ServerError> {
        let graphics_error = graphics
            .update(screen.graphics.take(), screen.alternate)
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
            wire: screen.generation,
            selection: screen
                .reading
                .as_ref()
                .map_or(0, |reading| reading.selection_revision),
            size: PaneSize {
                columns: screen.columns,
                rows: screen.rows.len() as u16,
                cell_width: screen.cell_pixels.map_or(active_size.cell_width, |p| p.0),
                cell_height: screen.cell_pixels.map_or(active_size.cell_height, |p| p.1),
            },
            mouse_tracking: screen.mouse_tracking,
            alternate: screen.alternate,
            alternate_scroll: screen.alternate_scroll,
        };
        let mut screen = decode_screen(
            *screen,
            *generation,
            &appearance.lock().unwrap_or_else(|e| e.into_inner()),
            self.local_files,
        )?;
        if let Some(metadata) = self.remote_metadata {
            Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata).context =
                super::super::metadata::TerminalMetadataContext::Remote(metadata.clone());
        }
        Arc::make_mut(&mut screen).graphics = graphics.snapshot();
        Ok((screen, mapping))
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
fn color(c: [u8; 3]) -> crate::appearance::Color {
    crate::appearance::Color::from_rgb_components(c[0], c[1], c[2])
}
fn decode_color(c: pane::Color) -> TerminalColor {
    match c {
        pane::Color::Default => TerminalColor::Default,
        pane::Color::Palette(i) => TerminalColor::Palette(i),
        pane::Color::Rgb(c) => TerminalColor::Rgb(color(c)),
    }
}
fn decode_hyperlink(
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

fn decode_screen(
    wire: pane::Screen,
    generation: PresentationGeneration,
    appearance: &TerminalAppearanceUpdate,
    local_files: Option<&crate::platform::local_filesystem::LocalFilesystemAuthority>,
) -> Result<Arc<ScreenSnapshot>, ServerError> {
    if !(2..=pane::MAX_COLUMNS).contains(&wire.columns)
        || wire.rows.len() > usize::from(pane::MAX_ROWS)
        || wire.rows.len() < 2
        || wire.palette.len() != 256
        || wire.palette_overrides.len() != 256
        || wire.wrapped.len() != wire.rows.len()
        || wire.rows.iter().any(|r| {
            r.len() != usize::from(wire.columns) || r.iter().any(|c| c.text.chars().count() > 64)
        })
    {
        return Err(ServerError::Protocol);
    }
    let mut screen = (*ScreenSnapshot::empty_with_appearance(
        crate::local_path::LocalPathSemantics::Posix,
        appearance,
    ))
    .clone();
    screen.generation = generation;
    if let Some((width, height)) = wire.cell_pixels {
        if !(1..=256).contains(&width) || !(1..=256).contains(&height) {
            return Err(ServerError::Protocol);
        }
        screen.cell_pixels = (u32::from(width), u32::from(height));
    }
    if let Some(reading) = wire.reading {
        if reading.visible_rows != wire.rows.len() as u64
            || reading.offset_rows > reading.total_rows.saturating_sub(reading.visible_rows)
        {
            return Err(ServerError::Protocol);
        }
        screen.viewport = ViewportSnapshot {
            offset_rows: reading.offset_rows,
            visible_rows: reading.visible_rows,
        };
        screen.scrollbar = ScrollbarSnapshot {
            total_rows: reading.total_rows,
            offset_rows: reading.offset_rows,
            visible_rows: reading.visible_rows,
        };
        screen.selection_present = reading.selection_present;
        screen.find = reading.find.map(|find| {
            Arc::new(super::super::TerminalFindSnapshot {
                generation: super::super::FindQueryGeneration::from_value(find.generation),
                total_matches: find.total,
                current_match: find.current,
                visible_spans: find
                    .spans
                    .into_iter()
                    .map(|(row, start_column, end_column, current)| {
                        super::super::FindHighlightSpan {
                            row,
                            start_column,
                            end_column,
                            current,
                        }
                    })
                    .collect(),
            })
        });
    }
    screen.mouse_tracking = wire.mouse_tracking;
    screen.alternate_scroll = wire.alternate_scroll;
    screen.size = ScreenSizeSnapshot {
        cols: wire.columns,
        rows: wire.rows.len() as u16,
    };
    let links = wire
        .hyperlinks
        .iter()
        .take(pane::effects::MAX_SCREEN_HYPERLINKS)
        .map(|link| decode_hyperlink(link, local_files))
        .collect::<Vec<_>>();
    screen.rows = wire
        .rows
        .into_iter()
        .map(|row| {
            Arc::from(
                row.into_iter()
                    .map(|c| CellSnapshot {
                        text: c.text,
                        foreground_source: decode_color(c.foreground),
                        background_source: decode_color(c.background),
                        underline_source: decode_color(c.underline_color),
                        inverse: c.flags & 1 != 0,
                        bold: c.flags & 2 != 0,
                        faint: c.flags & 4 != 0,
                        italic: c.flags & 8 != 0,
                        blinking: c.flags & 16 != 0,
                        invisible: c.flags & 32 != 0,
                        strikethrough: c.flags & 64 != 0,
                        overline: c.flags & 128 != 0,
                        spacer_tail: c.flags & 256 != 0,
                        underline: match c.underline {
                            1 => TerminalUnderlineSnapshot::Single,
                            2 => TerminalUnderlineSnapshot::Double,
                            3 => TerminalUnderlineSnapshot::Curly,
                            4 => TerminalUnderlineSnapshot::Dotted,
                            5 => TerminalUnderlineSnapshot::Dashed,
                            _ => TerminalUnderlineSnapshot::None,
                        },
                        selected: c.flags & 512 != 0,
                        semantic_content: CellSemanticSnapshot::Output,
                        hyperlink: c
                            .hyperlink
                            .and_then(|index| links.get(usize::from(index)))
                            .cloned()
                            .flatten(),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>()
        .into();
    screen.row_soft_wrapped = wire.wrapped.into();
    screen.colors.foreground_source = if wire.foreground_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.background_source = if wire.background_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.cursor_source = if wire.cursor_overridden {
        TerminalDefaultColorSource::ProgramOverride
    } else {
        TerminalDefaultColorSource::HostDefault
    };
    screen.colors.foreground = if wire.foreground_overridden {
        color(wire.foreground)
    } else {
        appearance.appearance.colors.foreground
    };
    screen.colors.background = if wire.background_overridden {
        color(wire.background)
    } else {
        appearance.appearance.colors.background
    };
    let mut palette = [crate::appearance::Color::BLACK; 256];
    for (dest, source) in palette.iter_mut().zip(wire.palette) {
        *dest = color(source);
    }
    for (index, destination) in palette.iter_mut().enumerate().take(16) {
        if !wire.palette_overrides[index] {
            *destination = if index < 8 {
                appearance.appearance.colors.normal[index]
            } else {
                appearance.appearance.colors.bright[index - 8]
            };
        }
    }
    screen.colors.palette_overrides = Arc::new(
        wire.palette_overrides
            .try_into()
            .map_err(|_| ServerError::Protocol)?,
    );
    screen.colors.palette = Arc::new(palette);
    screen.colors.reversed = wire.reversed;
    screen.background = screen.colors.effective_background();
    screen.active_screen = if wire.alternate {
        ActiveScreenSnapshot::Alternate
    } else {
        ActiveScreenSnapshot::Primary
    };
    screen.cursor = CursorSnapshot {
        position: wire
            .cursor
            .map(|(column, row, width_cells)| CursorPositionSnapshot {
                column,
                row,
                width_cells,
            }),
        visible: wire.cursor_visible,
        blinking: wire.cursor_blinking,
        password_input: false,
        shape: match wire.cursor_shape {
            1 => CursorShapeSnapshot::Bar,
            2 => CursorShapeSnapshot::Underline,
            3 => CursorShapeSnapshot::BlockHollow,
            _ => CursorShapeSnapshot::Block,
        },
        color: if wire.cursor_overridden {
            color(wire.cursor_color)
        } else {
            appearance.appearance.colors.cursor
        },
        text_color: if wire.cursor_overridden {
            color(wire.cursor_text)
        } else {
            appearance
                .appearance
                .colors
                .cursor_text
                .unwrap_or(screen.background)
        },
    };
    screen.text_blinking = screen.rows.iter().any(|row| row.iter().any(|c| c.blinking));
    Ok(Arc::new(screen))
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

    #[test]
    fn screen_reconciliation_keeps_program_colors_and_projects_client_defaults() {
        let mut appearance = TerminalAppearanceUpdate::builtin().unwrap();
        let configured = Arc::make_mut(&mut appearance.appearance);
        configured.colors.background = crate::appearance::Color::rgb(0xffffff);
        configured.colors.normal[0] = crate::appearance::Color::rgb(0x112233);
        let cell = pane::Cell {
            hyperlink: None,
            text: "x".into(),
            foreground: pane::Color::Default,
            background: pane::Color::Default,
            underline_color: pane::Color::Default,
            flags: 0,
            underline: 0,
        };
        let mut overrides = vec![false; 256];
        overrides[1] = true;
        let screen = pane::Screen {
            accessibility_revision: 0,
            graphics: None,
            cell_pixels: None,
            reading: None,
            hyperlinks: Vec::new(),
            generation: 1,
            mouse_tracking: false,
            alternate_scroll: false,
            columns: 2,
            rows: vec![vec![cell.clone(); 2]; 2],
            wrapped: vec![false; 2],
            foreground: [10, 20, 30],
            background: [0, 0, 0],
            palette: vec![[40, 50, 60]; 256],
            reversed: false,
            foreground_overridden: true,
            background_overridden: false,
            cursor_overridden: false,
            palette_overrides: overrides,
            cursor: None,
            cursor_visible: false,
            cursor_blinking: false,
            cursor_shape: 0,
            cursor_color: [0, 0, 0],
            cursor_text: [0, 0, 0],
            alternate: false,
        };
        let mut linked = screen.clone();
        linked.hyperlinks = vec![
            pane::effects::Hyperlink::Url("https://example.test".into()),
            pane::effects::Hyperlink::File("/etc/passwd".into()),
        ];
        linked.rows[0][0].hyperlink = Some(0);
        linked.rows[0][1].hyperlink = Some(1);
        linked.rows[1][0].hyperlink = Some(u16::MAX);
        let linked =
            decode_screen(linked, PresentationGeneration::test(1), &appearance, None).unwrap();
        assert_eq!(
            linked.rows[0][0].hyperlink.as_ref().unwrap().kind,
            super::super::super::hyperlink::HyperlinkKind::Url
        );
        assert!(linked.rows[0][1].hyperlink.is_none());
        assert!(linked.rows[1][0].hyperlink.is_none());
        let decoded =
            decode_screen(screen, PresentationGeneration::test(1), &appearance, None).unwrap();
        assert_eq!(
            decoded.colors.foreground,
            crate::appearance::Color::rgb(0x0a141e)
        );
        assert_eq!(decoded.background, crate::appearance::Color::rgb(0xffffff));
        assert_eq!(
            decoded.colors.palette[0],
            crate::appearance::Color::rgb(0x112233)
        );
        assert_eq!(
            decoded.colors.palette[1],
            crate::appearance::Color::rgb(0x28323c)
        );
    }
}
