//! Screen snapshot encoding, presentation style and palette for the Development host.
use super::*;

pub(super) fn appearance(
    style: Option<PresentationStyle>,
    generation: u64,
) -> Result<TerminalAppearanceUpdate, TerminalFailure> {
    let mut update = TerminalAppearanceUpdate::builtin()?;
    update.generation = AppearanceGeneration::new(generation);
    if let Some(style) = style {
        let resolved = Arc::make_mut(&mut update.appearance);
        resolved.appearance = if style.light {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        resolved.colors.foreground = from_rgb(style.foreground);
        resolved.colors.background = from_rgb(style.background);
        resolved.colors.cursor = from_rgb(style.cursor);
        resolved.colors.cursor_text = style.cursor_text.map(from_rgb);
        resolved.colors.normal = std::array::from_fn(|i| from_rgb(style.palette[i]));
        resolved.colors.bright = std::array::from_fn(|i| from_rgb(style.palette[i + 8]));
        resolved.bold_as_bright = style.bold_as_bright;
    }
    Ok(update)
}
fn from_rgb(c: [u8; 3]) -> super::super::Color {
    super::super::Color::from_rgb_components(c[0], c[1], c[2])
}
fn rgb(color: super::super::Color) -> [u8; 3] {
    [color.r, color.g, color.b]
}
fn encode_color(color: TerminalColor) -> pane::Color {
    match color {
        TerminalColor::Default => pane::Color::Default,
        TerminalColor::Palette(i) => pane::Color::Palette(i),
        TerminalColor::Rgb(c) => pane::Color::Rgb(rgb(c)),
    }
}
pub(crate) fn encode_screen(screen: &ScreenSnapshot) -> Screen {
    let mut hyperlinks = Vec::new();
    let mut link_indices = std::collections::HashMap::new();
    let mut link_bytes = 0;
    let mut encoded = Screen {
        accessibility_revision: 0,
        graphics: Some(graphics::encode(&screen.graphics)),
        cell_pixels: Some((screen.cell_pixels.0 as u16, screen.cell_pixels.1 as u16)),
        reading: Some(pane::reading::Snapshot {
            total_rows: screen.scrollbar.total_rows,
            offset_rows: screen.scrollbar.offset_rows,
            visible_rows: screen.scrollbar.visible_rows,
            selection_present: screen.selection_present,
            selection_revision: screen.selection_revision,
            find: screen.find.as_ref().map(|find| pane::reading::Find {
                generation: find.generation.value(),
                total: find.total_matches,
                current: find.current_match,
                spans: find
                    .visible_spans
                    .iter()
                    .map(|s| (s.row, s.start_column, s.end_column, s.current))
                    .collect(),
            }),
        }),
        hyperlinks: Vec::new(),
        generation: 0,
        mouse_tracking: screen.mouse_tracking,
        alternate_scroll: screen.alternate_scroll,
        columns: screen.size.cols,
        rows: screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| Cell {
                        hyperlink: c.hyperlink.as_ref().and_then(|link| {
                            let key = (link.kind == HyperlinkKind::LocalPath, link.value.as_str());
                            if let Some(index) = link_indices.get(&key) {
                                return Some(*index);
                            }
                            // Link metadata must never make an otherwise renderable screen exceed
                            // the transport frame. Excess targets lose their action, not their text.
                            if hyperlinks.len() >= pane::effects::MAX_SCREEN_HYPERLINKS
                                || link.value.len() > crate::terminal::hyperlink::MAX_LINK_BYTES
                                || link_bytes + link.value.len()
                                    > pane::effects::MAX_SCREEN_HYPERLINK_BYTES
                            {
                                return None;
                            }
                            let index = hyperlinks.len() as u16;
                            link_bytes += link.value.len();
                            hyperlinks.push(match link.kind {
                                HyperlinkKind::Url => {
                                    pane::effects::Hyperlink::Url(link.value.clone())
                                }
                                HyperlinkKind::LocalPath => {
                                    pane::effects::Hyperlink::File(link.value.clone())
                                }
                            });
                            link_indices.insert(key, index);
                            Some(index)
                        }),
                        text: c.text.chars().take(64).collect(),
                        foreground: encode_color(c.foreground_source),
                        background: encode_color(c.background_source),
                        underline_color: encode_color(c.underline_source),
                        flags: u16::from(c.inverse)
                            | (u16::from(c.bold) << 1)
                            | (u16::from(c.faint) << 2)
                            | (u16::from(c.italic) << 3)
                            | (u16::from(c.blinking) << 4)
                            | (u16::from(c.invisible) << 5)
                            | (u16::from(c.strikethrough) << 6)
                            | (u16::from(c.overline) << 7)
                            | (u16::from(c.spacer_tail) << 8)
                            | (u16::from(c.selected) << 9),
                        underline: match c.underline {
                            TerminalUnderlineSnapshot::None => 0,
                            TerminalUnderlineSnapshot::Single => 1,
                            TerminalUnderlineSnapshot::Double => 2,
                            TerminalUnderlineSnapshot::Curly => 3,
                            TerminalUnderlineSnapshot::Dotted => 4,
                            TerminalUnderlineSnapshot::Dashed => 5,
                        },
                    })
                    .collect()
            })
            .collect(),
        wrapped: screen.row_soft_wrapped.to_vec(),
        foreground: rgb(screen.colors.foreground),
        background: rgb(screen.colors.background),
        palette: screen.colors.palette.iter().copied().map(rgb).collect(),
        reversed: screen.colors.reversed,
        foreground_overridden: screen.colors.foreground_source
            == TerminalDefaultColorSource::ProgramOverride,
        background_overridden: screen.colors.background_source
            == TerminalDefaultColorSource::ProgramOverride,
        cursor_overridden: screen.colors.cursor_source
            == TerminalDefaultColorSource::ProgramOverride,
        palette_overrides: screen.colors.palette_overrides.to_vec(),
        cursor: screen
            .cursor
            .position
            .map(|p| (p.column, p.row, p.width_cells)),
        cursor_visible: screen.cursor.visible,
        cursor_blinking: screen.cursor.blinking,
        cursor_shape: match screen.cursor.shape {
            CursorShapeSnapshot::Block => 0,
            CursorShapeSnapshot::Bar => 1,
            CursorShapeSnapshot::Underline => 2,
            CursorShapeSnapshot::BlockHollow => 3,
        },
        cursor_color: rgb(screen.cursor.color),
        cursor_text: rgb(screen.cursor.text_color),
        alternate: screen.active_screen == ActiveScreenSnapshot::Alternate,
    };
    encoded.hyperlinks = hyperlinks;
    encoded
}
