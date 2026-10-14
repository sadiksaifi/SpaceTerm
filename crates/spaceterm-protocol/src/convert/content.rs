//! Checked semantic paging shared by both authenticated protocol majors.

pub mod reads;

use super::{
    identity::*,
    runtime::{rgb, rgb_bits},
};
use crate::ProtocolError;
use spaceterm_core::{authority::ContentRevision, content::*, content_queries::*};

fn screen(value: u32) -> Result<Screen, ProtocolError> {
    match value {
        1 => Ok(Screen::Primary),
        2 => Ok(Screen::Alternate),
        _ => Err(ProtocolError::Malformed),
    }
}
fn screen_number(value: Screen) -> u32 {
    match value {
        Screen::Primary => 1,
        Screen::Alternate => 2,
    }
}
fn narrow(value: u32) -> Result<u16, ProtocolError> {
    u16::try_from(value).map_err(|_| ProtocolError::Malformed)
}
fn content(text: &str, width: u32, spacer: bool, head: bool) -> Result<CellContent, ProtocolError> {
    if spacer {
        if width != 0 || !text.is_empty() {
            return Err(ProtocolError::Malformed);
        }
        Ok(if head {
            CellContent::spacer_head()
        } else {
            CellContent::spacer_tail()
        })
    } else {
        if head {
            return Err(ProtocolError::Malformed);
        }
        CellContent::grapheme(
            text.into(),
            match width {
                1 => CellWidth::Single,
                2 => CellWidth::Wide,
                _ => return Err(ProtocolError::Malformed),
            },
        )
        .map_err(|_| ProtocolError::Malformed)
    }
}
fn width(value: &CellContent) -> (u32, bool, bool) {
    match value.width() {
        CellWidth::Single => (1, false, false),
        CellWidth::Wide => (2, false, false),
        CellWidth::SpacerHead => (0, true, true),
        CellWidth::SpacerTail => (0, true, false),
    }
}
fn hyperlink(value: u64) -> Result<Option<HyperlinkId>, ProtocolError> {
    if value == 0 {
        Ok(None)
    } else {
        HyperlinkId::new(u32::try_from(value).map_err(|_| ProtocolError::Malformed)?)
            .map(Some)
            .map_err(|_| ProtocolError::Malformed)
    }
}
macro_rules! row_codec {
    (v1) => {
        fn encode_row(row: &ContentRow) -> wire::CanonicalSemanticRow {
            wire::CanonicalSemanticRow {
                id: Some(encode_row_id(row.id())),
                revision: row.revision(),
                soft_wrapped: row.soft_wrapped(),
                columns: u32::from(row.columns()),
                start_column: u32::from(row.start_column()),
                end_column: u32::from(row.end_column()),
                cells: row
                    .cells()
                    .iter()
                    .map(|cell| {
                        let (width, spacer, wrap_head) = width(cell.content());
                        wire::StyledCell {
                            grapheme: cell.content().text().into(),
                            width,
                            spacer,
                            wrap_head,
                            style: Some(encode_style(cell.style())),
                            hyperlink_id: cell.hyperlink().map_or(0, |id| u64::from(id.get())),
                        }
                    })
                    .collect(),
            }
        }
        fn decode_cells(
            row: &wire::CanonicalSemanticRow,
        ) -> Result<Vec<StyledCell>, ProtocolError> {
            row.cells
                .iter()
                .map(|cell| {
                    Ok(StyledCell::new(
                        content(&cell.grapheme, cell.width, cell.spacer, cell.wrap_head)?,
                        decode_style(required(&cell.style)?)?,
                        hyperlink(cell.hyperlink_id)?,
                    ))
                })
                .collect()
        }
    };
    (v2) => {
        fn encode_row(row: &ContentRow) -> wire::CanonicalSemanticRow {
            let mut runs: Vec<wire::StyledRun> = Vec::new();
            for cell in row.cells() {
                let style = encode_style(cell.style());
                let hyperlink_id = cell.hyperlink().map_or(0, |id| u64::from(id.get()));
                let (width, spacer, wrap_head) = width(cell.content());
                let glyph = wire::Grapheme {
                    text: cell.content().text().into(),
                    width,
                    spacer,
                    wrap_head,
                };
                if let Some(run) = runs.last_mut()
                    && run.style.as_ref() == Some(&style)
                    && run.hyperlink_id == hyperlink_id
                {
                    run.graphemes.push(glyph);
                } else {
                    runs.push(wire::StyledRun {
                        style: Some(style),
                        hyperlink_id,
                        graphemes: vec![glyph],
                    });
                }
            }
            wire::CanonicalSemanticRow {
                id: Some(encode_row_id(row.id())),
                revision: row.revision(),
                soft_wrapped: row.soft_wrapped(),
                columns: u32::from(row.columns()),
                start_column: u32::from(row.start_column()),
                end_column: u32::from(row.end_column()),
                runs,
            }
        }
        fn decode_cells(
            row: &wire::CanonicalSemanticRow,
        ) -> Result<Vec<StyledCell>, ProtocolError> {
            let mut cells = Vec::new();
            for run in &row.runs {
                let style = decode_style(required(&run.style)?)?;
                let link = hyperlink(run.hyperlink_id)?;
                for cell in &run.graphemes {
                    cells.push(StyledCell::new(
                        content(&cell.text, cell.width, cell.spacer, cell.wrap_head)?,
                        style,
                        link,
                    ));
                }
            }
            Ok(cells)
        }
    };
}
macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            row_codec!($p);
            pub fn encode_row_id(row: RowId) -> wire::PhysicalRowId {
                wire::PhysicalRowId {
                    screen_generation: row.screen_generation,
                    screen: screen_number(row.screen),
                    node_serial: row.node_serial,
                    page_row: u32::from(row.page_row),
                }
            }
            pub fn decode_row_id(row: &wire::PhysicalRowId) -> Result<RowId, ProtocolError> {
                Ok(RowId {
                    screen_generation: row.screen_generation,
                    screen: screen(row.screen)?,
                    node_serial: row.node_serial,
                    page_row: narrow(row.page_row)?,
                })
            }
            pub fn encode_base(base: ContentBase) -> wire::ContentBase {
                wire::ContentBase {
                    session: Some(encode_session(base.session)),
                    content_revision: base.revision.get(),
                    topology_revision: base.topology_revision,
                    screen_generation: base.screen_generation,
                    screen: screen_number(base.screen),
                }
            }
            pub fn decode_base(base: &wire::ContentBase) -> Result<ContentBase, ProtocolError> {
                Ok(ContentBase {
                    session: decode_session(required(&base.session)?)?,
                    revision: ContentRevision::new(base.content_revision),
                    topology_revision: base.topology_revision,
                    screen_generation: base.screen_generation,
                    screen: screen(base.screen)?,
                })
            }
            pub fn encode_stamp(stamp: QueryStamp) -> wire::QueryStamp {
                let base = stamp.base();
                wire::QueryStamp {
                    fence: Some(encode_fence(stamp.fence())),
                    content_revision: base.revision.get(),
                    topology_revision: base.topology_revision,
                    screen_generation: base.screen_generation,
                    screen: screen_number(base.screen),
                    request: stamp.request().get(),
                    request_generation: stamp.generation().get(),
                }
            }
            pub fn decode_stamp(value: &wire::QueryStamp) -> Result<QueryStamp, ProtocolError> {
                let fence = decode_fence(
                    required(&value.fence)?,
                    Some(ContentRevision::new(value.content_revision)),
                )?;
                let base = ContentBase {
                    session: fence.session(),
                    revision: ContentRevision::new(value.content_revision),
                    topology_revision: value.topology_revision,
                    screen_generation: value.screen_generation,
                    screen: screen(value.screen)?,
                };
                QueryStamp::new(
                    fence,
                    base,
                    ContentRequestId::new(value.request).map_err(|_| ProtocolError::Malformed)?,
                    RequestGeneration::new(value.request_generation),
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_continuation(value: ContentContinuation) -> wire::PageContinuation {
                let (row, row_revision, next_column, after_row, columns) =
                    match value.page().position {
                        PagePosition::AfterRow { row, row_revision } => {
                            (row, row_revision, None, true, 0)
                        }
                        PagePosition::WithinRow {
                            row,
                            row_revision,
                            columns,
                            next_column,
                        } => (
                            row,
                            row_revision,
                            Some(u32::from(next_column)),
                            false,
                            u32::from(columns),
                        ),
                    };
                wire::PageContinuation {
                    stamp: Some(encode_stamp(value.stamp())),
                    row: Some(encode_row_id(row)),
                    row_revision,
                    next_column,
                    after_row,
                    columns,
                }
            }
            pub fn decode_continuation(
                value: &wire::PageContinuation,
            ) -> Result<ContentContinuation, ProtocolError> {
                let stamp = decode_stamp(required(&value.stamp)?)?;
                let row = decode_row_id(required(&value.row)?)?;
                let position = if value.after_row {
                    if value.next_column.is_some() || value.columns != 0 {
                        return Err(ProtocolError::Malformed);
                    }
                    PagePosition::AfterRow {
                        row,
                        row_revision: value.row_revision,
                    }
                } else {
                    let next_column = narrow(value.next_column.ok_or(ProtocolError::Malformed)?)?;
                    let columns = narrow(value.columns)?;
                    if next_column == 0 || next_column >= columns {
                        return Err(ProtocolError::Malformed);
                    }
                    PagePosition::WithinRow {
                        row,
                        row_revision: value.row_revision,
                        columns,
                        next_column,
                    }
                };
                ContentContinuation::new(
                    stamp,
                    PageContinuation {
                        base: stamp.base(),
                        position,
                    },
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn decode_page_request(
                value: &wire::ContentQuery,
            ) -> Result<ContentPageRequest, ProtocolError> {
                let stamp = decode_stamp(required(&value.stamp)?)?;
                if !value.continuation.is_empty()
                    || !value.query.is_empty()
                    || value.case_sensitive
                    || value.regex
                    || value.topology_after.is_some()
                    || value.cancel
                    || value.copy_options.is_some()
                    || value.find_continuation.is_some()
                    || value
                        .fence
                        .as_ref()
                        .is_some_and(|fence| *fence != encode_fence(stamp.fence()))
                    || (value.expected_revision != 0
                        && value.expected_revision != stamp.base().revision.get())
                    || (value.request_generation != 0
                        && value.request_generation != stamp.generation().get())
                {
                    return Err(ProtocolError::Malformed);
                }
                let row = |value: &wire::CellRef| -> Result<RowId, ProtocolError> {
                    if decode_session(required(&value.session)?)? != stamp.base().session {
                        return Err(ProtocolError::Malformed);
                    }
                    decode_row_id(required(&value.physical_row)?)
                };
                let range = match value.kind {
                    1 if value.start.is_none() && value.end.is_none() => ContentRange::ActiveGrid,
                    2 => ContentRange::History {
                        first: value.start.as_ref().map(row).transpose()?,
                        last: value.end.as_ref().map(row).transpose()?,
                    },
                    _ => return Err(ProtocolError::Malformed),
                };
                let limits = required(&value.limits)?;
                let limits = PageLimits::new(narrow(limits.rows)?, limits.cells, limits.bytes)
                    .map_err(|_| ProtocolError::LimitExceeded)?;
                ContentPageRequest::new(
                    stamp,
                    range,
                    value
                        .page_continuation
                        .as_deref()
                        .map(decode_continuation)
                        .transpose()?,
                    limits,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_page_request(
                value: ContentPageRequest,
            ) -> Result<wire::ContentQuery, ProtocolError> {
                let endpoint = |row| wire::CellRef {
                    session: Some(encode_session(value.stamp().base().session)),
                    screen_generation: value.stamp().base().screen_generation,
                    physical_row: Some(encode_row_id(row)),
                    ..Default::default()
                };
                let (kind, start, end) = match value.range() {
                    ContentRange::ActiveGrid => (1, None, None),
                    ContentRange::History { first, last } => {
                        (2, first.map(endpoint), last.map(endpoint))
                    }
                };
                Ok(wire::ContentQuery {
                    kind,
                    start,
                    end,
                    stamp: Some(Box::new(encode_stamp(value.stamp()))),
                    limits: Some(wire::PageLimits {
                        rows: u32::from(value.limits().rows()),
                        cells: value.limits().cells(),
                        bytes: value.limits().bytes(),
                    }),
                    page_continuation: value
                        .continuation()
                        .map(|value| Box::new(encode_continuation(value))),
                    ..Default::default()
                })
            }
            fn encode_color(value: TerminalColor) -> wire::Color {
                wire::Color {
                    value: Some(match value {
                        TerminalColor::Default => wire::color::Value::DefaultColor(true),
                        TerminalColor::Palette(value) => {
                            wire::color::Value::PaletteIndex(u32::from(value))
                        }
                        TerminalColor::Rgb(value) => wire::color::Value::Rgb(rgb_bits(value)),
                    }),
                }
            }
            fn decode_color(value: &wire::Color) -> Result<TerminalColor, ProtocolError> {
                match required(&value.value)? {
                    wire::color::Value::DefaultColor(true) => Ok(TerminalColor::Default),
                    wire::color::Value::PaletteIndex(value) => Ok(TerminalColor::Palette(
                        u8::try_from(*value).map_err(|_| ProtocolError::Malformed)?,
                    )),
                    wire::color::Value::Rgb(value) => Ok(TerminalColor::Rgb(rgb(*value)?)),
                    _ => Err(ProtocolError::Malformed),
                }
            }
            fn encode_style(value: CellStyle) -> wire::Style {
                let flags = [
                    value.bold,
                    value.faint,
                    value.italic,
                    value.blinking,
                    value.inverse,
                    value.invisible,
                    value.strikethrough,
                    value.overline,
                ]
                .into_iter()
                .enumerate()
                .fold(0, |bits, (i, on)| bits | (u32::from(on) << i))
                    | (match value.semantic {
                        SemanticContent::Output => 0,
                        SemanticContent::Input => 1,
                        SemanticContent::Prompt => 2,
                    } << 8);
                wire::Style {
                    foreground: Some(encode_color(value.foreground)),
                    background: Some(encode_color(value.background)),
                    underline_color: Some(encode_color(value.underline_color)),
                    flags,
                    underline_style: match value.underline {
                        Underline::None => 0,
                        Underline::Single => 1,
                        Underline::Double => 2,
                        Underline::Curly => 3,
                        Underline::Dotted => 4,
                        Underline::Dashed => 5,
                    },
                }
            }
            fn decode_style(value: &wire::Style) -> Result<CellStyle, ProtocolError> {
                if value.flags > 0x2ff {
                    return Err(ProtocolError::Malformed);
                }
                Ok(CellStyle {
                    foreground: decode_color(required(&value.foreground)?)?,
                    background: decode_color(required(&value.background)?)?,
                    underline_color: decode_color(required(&value.underline_color)?)?,
                    underline: match value.underline_style {
                        0 => Underline::None,
                        1 => Underline::Single,
                        2 => Underline::Double,
                        3 => Underline::Curly,
                        4 => Underline::Dotted,
                        5 => Underline::Dashed,
                        _ => return Err(ProtocolError::Malformed),
                    },
                    bold: value.flags & 1 != 0,
                    faint: value.flags & 2 != 0,
                    italic: value.flags & 4 != 0,
                    blinking: value.flags & 8 != 0,
                    inverse: value.flags & 16 != 0,
                    invisible: value.flags & 32 != 0,
                    strikethrough: value.flags & 64 != 0,
                    overline: value.flags & 128 != 0,
                    semantic: match value.flags >> 8 {
                        0 => SemanticContent::Output,
                        1 => SemanticContent::Input,
                        2 => SemanticContent::Prompt,
                        _ => return Err(ProtocolError::Malformed),
                    },
                })
            }
            fn encode_resource(key: GraphicsResourceKey) -> wire::ResourceId {
                wire::ResourceId {
                    session: Some(encode_session(key.session)),
                    local_id: key.id.get(),
                    generation: key.generation,
                }
            }
            fn decode_resource(
                value: &wire::ResourceId,
            ) -> Result<GraphicsResourceKey, ProtocolError> {
                Ok(GraphicsResourceKey {
                    session: decode_session(required(&value.session)?)?,
                    id: GraphicsResourceId::new(value.local_id)
                        .map_err(|_| ProtocolError::Malformed)?,
                    generation: value.generation,
                })
            }
            pub fn encode_graphics_request(request: GraphicsRequest) -> wire::ResourceRequest {
                let resource = request.resource();
                wire::ResourceRequest {
                    fence: Some(encode_fence(request.stamp().fence())),
                    resource: Some(encode_resource(resource.key)),
                    transfer: request.transfer().get(),
                    offset: u64::from(request.offset()),
                    stamp: Some(Box::new(encode_stamp(request.stamp()))),
                    width: resource.width,
                    height: resource.height,
                    rgba_bytes: resource.rgba_bytes,
                    digest: resource.digest.to_vec(),
                }
            }
            pub fn decode_graphics_request(
                value: &wire::ResourceRequest,
            ) -> Result<GraphicsRequest, ProtocolError> {
                let stamp = decode_stamp(required(&value.stamp)?)?;
                if decode_fence(required(&value.fence)?, stamp.fence().content())? != stamp.fence()
                {
                    return Err(ProtocolError::Malformed);
                }
                GraphicsRequest::new(
                    stamp,
                    ResourceTransferId::new(value.transfer)
                        .map_err(|_| ProtocolError::Malformed)?,
                    GraphicsResourceRef {
                        key: decode_resource(required(&value.resource)?)?,
                        width: value.width,
                        height: value.height,
                        rgba_bytes: value.rgba_bytes,
                        digest: value
                            .digest
                            .as_slice()
                            .try_into()
                            .map_err(|_| ProtocolError::Malformed)?,
                    },
                    u32::try_from(value.offset).map_err(|_| ProtocolError::Malformed)?,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_graphics_chunk(chunk: &GraphicsChunk) -> wire::ResourceChunk {
                let request = chunk.request();
                let resource = request.resource();
                wire::ResourceChunk {
                    fence: Some(encode_fence(request.stamp().fence())),
                    resource: Some(encode_resource(resource.key)),
                    transfer: request.transfer().get(),
                    offset: u64::from(chunk.offset()),
                    total_bytes: u64::from(resource.rgba_bytes),
                    data: chunk.bytes().to_vec(),
                    digest: resource.digest.to_vec(),
                    complete: chunk.complete(),
                    format: wire::ResourceFormat::Rgba8 as i32,
                    width: resource.width,
                    height: resource.height,
                    stamp: Some(Box::new(encode_stamp(request.stamp()))),
                }
            }
            pub fn decode_graphics_chunk(
                value: &wire::ResourceChunk,
                request: GraphicsRequest,
            ) -> Result<GraphicsChunk, ProtocolError> {
                let resource = request.resource();
                if decode_stamp(required(&value.stamp)?)? != request.stamp()
                    || decode_fence(required(&value.fence)?, request.stamp().fence().content())?
                        != request.stamp().fence()
                    || decode_resource(required(&value.resource)?)? != resource.key
                    || value.transfer != request.transfer().get()
                    || value.total_bytes != u64::from(resource.rgba_bytes)
                    || value.digest.as_slice() != resource.digest
                    || value.format != wire::ResourceFormat::Rgba8 as i32
                    || value.width != resource.width
                    || value.height != resource.height
                {
                    return Err(ProtocolError::Malformed);
                }
                GraphicsChunk::new(
                    request,
                    u32::try_from(value.offset).map_err(|_| ProtocolError::Malformed)?,
                    value.data.clone(),
                    value.complete,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_page(
                value: &ContentPageReply,
            ) -> Result<wire::CanonicalSemanticPage, ProtocolError> {
                let page = value.page().parts();
                Ok(wire::CanonicalSemanticPage {
                    stamp: Some(encode_stamp(value.stamp())),
                    geometry: None,
                    columns: u32::from(page.columns),
                    rows: page.rows.iter().map(encode_row).collect(),
                    cursor: page.cursor.map(|cursor| wire::CanonicalCursor {
                        row: Some(encode_row_id(cursor.row)),
                        column: u32::from(cursor.column),
                        shape: match cursor.shape {
                            CursorShape::Block => 1,
                            CursorShape::Underline => 2,
                            CursorShape::Bar => 3,
                        },
                        visible: cursor.visible,
                        blinking: cursor.blinking,
                    }),
                    modes: Some(wire::Modes {
                        alternate_screen: page.base.screen == Screen::Alternate,
                        bracketed_paste: page.modes.bracketed_paste,
                        focus_events: page.modes.focus_reporting,
                        synchronized_output: page.modes.synchronized_output,
                        application_cursor: page.modes.application_cursor,
                        application_keypad: page.modes.application_keypad,
                        origin: page.modes.origin,
                        insert: page.modes.insert,
                        wraparound: page.modes.wraparound,
                        hidden_input: page.modes.hidden_input,
                        mouse_tracking: match page.modes.mouse_tracking {
                            MouseTracking::None => 0,
                            MouseTracking::Press => 1,
                            MouseTracking::Button => 2,
                            MouseTracking::Any => 3,
                        },
                        mouse_encoding: match page.modes.mouse_encoding {
                            MouseEncoding::X10 => 0,
                            MouseEncoding::Utf8 => 1,
                            MouseEncoding::Sgr => 2,
                            MouseEncoding::Urxvt => 3,
                            MouseEncoding::SgrPixels => 4,
                        },
                        ..Default::default()
                    }),
                    color_state: Some(wire::ColorState {
                        default_foreground: page.colors.foreground.map(rgb_bits),
                        default_background: page.colors.background.map(rgb_bits),
                        cursor_color: page.colors.cursor.map(rgb_bits),
                        reverse_colors: page.modes.reverse_colors,
                    }),
                    color_overrides: page
                        .colors
                        .palette
                        .iter()
                        .map(|value| wire::ColorOverride {
                            slot: u32::from(value.index),
                            rgb: rgb_bits(value.color),
                        })
                        .collect(),
                    hyperlinks: page
                        .hyperlinks
                        .iter()
                        .map(|link| wire::Hyperlink {
                            id: u64::from(link.id().get()),
                            uri: link.target().into(),
                        })
                        .collect(),
                    graphics: page
                        .placements
                        .iter()
                        .map(|value| wire::GraphicPlacement {
                            resource: Some(encode_resource(value.resource)),
                            anchor: Some(encode_row_id(value.anchor)),
                            placement_id: value.placement_id,
                            signed_column: value.column,
                            columns: u32::from(value.columns),
                            rows: u32::from(value.rows),
                            pixel_offset_x: value.pixel_offset_x,
                            pixel_offset_y: value.pixel_offset_y,
                            source_x: value.source.x,
                            source_y: value.source.y,
                            source_width: value.source.width,
                            source_height: value.source.height,
                            destination_width: value.destination_width,
                            destination_height: value.destination_height,
                            unicode_placeholder: value.unicode_placeholder,
                            z_index: value.z_index,
                            ..Default::default()
                        })
                        .collect(),
                    continuation: value
                        .continuation()
                        .map_err(|_| ProtocolError::Malformed)?
                        .map(encode_continuation),
                    complete: page.continuation.is_none(),
                    metadata: None,
                    resources: page
                        .resources
                        .iter()
                        .map(|value| wire::GraphicsResourceReference {
                            resource: Some(encode_resource(value.key)),
                            width: value.width,
                            height: value.height,
                            rgba_bytes: value.rgba_bytes,
                            digest: value.digest.to_vec(),
                        })
                        .collect(),
                })
            }
            pub fn decode_page(
                value: &wire::CanonicalSemanticPage,
                request: &ContentPageRequest,
            ) -> Result<ContentPageReply, ProtocolError> {
                let stamp = decode_stamp(required(&value.stamp)?)?;
                if stamp != request.stamp()
                    || value.complete != value.continuation.is_none()
                    || value.metadata.is_some()
                {
                    return Err(ProtocolError::Malformed);
                }
                let columns = if value.columns != 0 {
                    if value
                        .geometry
                        .as_ref()
                        .is_some_and(|geometry| geometry.columns != value.columns)
                    {
                        return Err(ProtocolError::Malformed);
                    }
                    value.columns
                } else {
                    required(&value.geometry)?.columns
                };
                let mut page = ContentPageParts::new(stamp.base(), narrow(columns)?);
                page.rows = value
                    .rows
                    .iter()
                    .map(|row| {
                        let cells = decode_cells(row)?;
                        let decoded = ContentRow::fragment(
                            decode_row_id(required(&row.id)?)?,
                            row.revision,
                            row.soft_wrapped,
                            narrow(row.columns)?,
                            narrow(row.start_column)?,
                            cells,
                        )
                        .map_err(|_| ProtocolError::Malformed)?;
                        if u32::from(decoded.end_column()) != row.end_column {
                            return Err(ProtocolError::Malformed);
                        }
                        Ok(decoded)
                    })
                    .collect::<Result<_, ProtocolError>>()?;
                page.cursor = value
                    .cursor
                    .as_ref()
                    .map(|cursor| {
                        Ok(Cursor {
                            row: decode_row_id(required(&cursor.row)?)?,
                            column: narrow(cursor.column)?,
                            shape: match cursor.shape {
                                1 => CursorShape::Block,
                                2 => CursorShape::Underline,
                                3 => CursorShape::Bar,
                                _ => return Err(ProtocolError::Malformed),
                            },
                            visible: cursor.visible,
                            blinking: cursor.blinking,
                        })
                    })
                    .transpose()?;
                let modes = required(&value.modes)?;
                if modes.alternate_screen != (stamp.base().screen == Screen::Alternate)
                    || modes.mouse_protocol != 0
                    || modes.keyboard_protocol != 0
                {
                    return Err(ProtocolError::Malformed);
                }
                page.modes = TerminalModes {
                    application_cursor: modes.application_cursor,
                    application_keypad: modes.application_keypad,
                    bracketed_paste: modes.bracketed_paste,
                    focus_reporting: modes.focus_events,
                    synchronized_output: modes.synchronized_output,
                    origin: modes.origin,
                    insert: modes.insert,
                    wraparound: modes.wraparound,
                    hidden_input: modes.hidden_input,
                    mouse_tracking: match modes.mouse_tracking {
                        0 => MouseTracking::None,
                        1 => MouseTracking::Press,
                        2 => MouseTracking::Button,
                        3 => MouseTracking::Any,
                        _ => return Err(ProtocolError::Malformed),
                    },
                    mouse_encoding: match modes.mouse_encoding {
                        0 => MouseEncoding::X10,
                        1 => MouseEncoding::Utf8,
                        2 => MouseEncoding::Sgr,
                        3 => MouseEncoding::Urxvt,
                        4 => MouseEncoding::SgrPixels,
                        _ => return Err(ProtocolError::Malformed),
                    },
                    reverse_colors: value
                        .color_state
                        .as_ref()
                        .is_some_and(|colors| colors.reverse_colors),
                };
                let colors = required(&value.color_state)?;
                page.colors = ProgramColorOverrides {
                    foreground: colors.default_foreground.map(rgb).transpose()?,
                    background: colors.default_background.map(rgb).transpose()?,
                    cursor: colors.cursor_color.map(rgb).transpose()?,
                    palette: value
                        .color_overrides
                        .iter()
                        .map(|color| {
                            Ok(PaletteOverride {
                                index: u8::try_from(color.slot)
                                    .map_err(|_| ProtocolError::Malformed)?,
                                color: rgb(color.rgb)?,
                            })
                        })
                        .collect::<Result<_, ProtocolError>>()?,
                };
                page.hyperlinks = value
                    .hyperlinks
                    .iter()
                    .map(|link| {
                        HyperlinkDescriptor::new(
                            hyperlink(link.id)?.ok_or(ProtocolError::Malformed)?,
                            link.uri.clone(),
                        )
                        .map_err(|_| ProtocolError::Malformed)
                    })
                    .collect::<Result<_, _>>()?;
                page.placements = value
                    .graphics
                    .iter()
                    .map(|value| {
                        Ok(GraphicsPlacement {
                            resource: decode_resource(required(&value.resource)?)?,
                            placement_id: value.placement_id,
                            anchor: decode_row_id(required(&value.anchor)?)?,
                            column: value.signed_column,
                            columns: narrow(value.columns)?,
                            rows: narrow(value.rows)?,
                            pixel_offset_x: value.pixel_offset_x,
                            pixel_offset_y: value.pixel_offset_y,
                            source: ImageRect {
                                x: value.source_x,
                                y: value.source_y,
                                width: value.source_width,
                                height: value.source_height,
                            },
                            destination_width: value.destination_width,
                            destination_height: value.destination_height,
                            unicode_placeholder: value.unicode_placeholder,
                            z_index: value.z_index,
                        })
                    })
                    .collect::<Result<_, ProtocolError>>()?;
                page.resources = value
                    .resources
                    .iter()
                    .map(|value| {
                        Ok(GraphicsResourceRef {
                            key: decode_resource(required(&value.resource)?)?,
                            width: value.width,
                            height: value.height,
                            rgba_bytes: value.rgba_bytes,
                            digest: value
                                .digest
                                .as_slice()
                                .try_into()
                                .map_err(|_| ProtocolError::Malformed)?,
                        })
                    })
                    .collect::<Result<_, ProtocolError>>()?;
                page.continuation = value
                    .continuation
                    .as_ref()
                    .map(decode_continuation)
                    .transpose()?
                    .map(|value| value.page());
                ContentPageReply::new(
                    request,
                    ContentPage::new(page).map_err(|_| ProtocolError::Malformed)?,
                )
                .map_err(|_| ProtocolError::Malformed)
            }
            pub fn encode_page_result(
                value: &ContentPageResult,
            ) -> Result<wire::ContentReadResponse, ProtocolError> {
                Ok(wire::ContentReadResponse {
                    result: Some(match value {
                        ContentReadResult::Ready(page) => {
                            wire::content_read_response::Result::Page(Box::new(encode_page(page)?))
                        }
                        ContentReadResult::ResyncRequired { request, current } => {
                            wire::content_read_response::Result::ResyncRequired(
                                wire::ContentResyncRequired {
                                    stamp: Some(encode_stamp(*request)),
                                    current: Some(encode_base(*current)),
                                },
                            )
                        }
                        ContentReadResult::Rejected { request, reason } => {
                            wire::content_read_response::Result::Rejected(wire::ContentFailure {
                                stamp: Some(encode_stamp(*request)),
                                reason: match reason {
                                    ContentReadFailure::StaleAuthority => 1,
                                    ContentReadFailure::Cancelled => 2,
                                    ContentReadFailure::Retired => 3,
                                    ContentReadFailure::Capacity => 4,
                                    ContentReadFailure::MissingResource => 5,
                                    ContentReadFailure::InvalidRange => 6,
                                    ContentReadFailure::Exhausted => 7,
                                },
                            })
                        }
                    }),
                })
            }
        }
    };
}
profile!(v1);
profile!(v2);
