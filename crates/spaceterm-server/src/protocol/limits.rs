//! Bounds both peers enforce. A peer that exceeds one commits a protocol violation, except where
//! a limit names a typed refusal instead.
use std::time::Duration;

/// Bytes in a frame header: `len:u32be | kind:u8`.
pub const HEADER_BYTES: usize = 5;
/// Largest frame on the wire, header included.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest body one frame carries. A larger message continues in further frames.
pub const MAX_FRAME_BODY: usize = MAX_FRAME_BYTES - HEADER_BYTES;
/// Largest frame body a writer emits for bulk replies, so input and presentation interleave.
pub const BULK_CHUNK_BYTES: usize = 64 * 1024;

/// Largest client `Request` message.
pub const MAX_REQUEST_BYTES: usize = MAX_FRAME_BODY;
/// Largest client `Input` message.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
/// Most events in one `Input` message.
pub const MAX_INPUT_EVENTS: usize = 32;
/// Largest text in one `InputEvent::Paste` chunk.
pub const MAX_PASTE_CHUNK_BYTES: usize = 60 * 1024;
/// Largest paste across all of its chunks.
pub const MAX_PASTE_BYTES: usize = 1024 * 1024;
/// Largest small client message: acknowledgements, views, pings and cancellations.
pub const MAX_CONTROL_BYTES: usize = 256;
/// Largest `EffectDone`, which can carry clipboard text.
pub const MAX_EFFECT_DONE_BYTES: usize = crate::pane::effects::MAX_CLIPBOARD_BYTES + 1024;

/// Largest single screen update across all of its frames.
pub const MAX_UPDATE_BYTES: usize = 16 * 1024 * 1024;
/// Largest reply across all of its frames.
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
/// Largest Pane event, which can carry a clipboard write.
pub const MAX_PANE_EVENT_BYTES: usize = crate::pane::effects::MAX_CLIPBOARD_BYTES + 64 * 1024;
/// Largest Pane presentation metadata: every bounded string at its character limit, four bytes
/// per character.
pub const MAX_PANE_METADATA_BYTES: usize = 4
    * (2 * crate::pane::metadata::MAX_TITLE_CHARS
        + crate::pane::metadata::MAX_TITLE_CHARS
        + 2 * crate::pane::metadata::MAX_DIRECTORY_CHARS
        + 2 * crate::pane::metadata::MAX_MACHINE_CHARS
        + crate::pane::metadata::MAX_COMMAND_CHARS);
/// Largest path a Pane carries, four of which it may carry.
pub const MAX_PATH_BYTES: usize = 4096;
/// Largest Workspace inventory revision: every Pane at its metadata and path limits, doubled for
/// Workspaces, Tabs, repository facts and encoding overhead.
pub const MAX_INVENTORY_BYTES: usize =
    2 * crate::hierarchy::MAX_PANES * (MAX_PANE_METADATA_BYTES + 4 * MAX_PATH_BYTES);

/// Requests one client keeps in flight on one connection.
pub const MAX_REQUESTS_IN_FLIGHT: usize = 64;
/// Channels open at once on one connection.
pub const MAX_CHANNELS: usize = 128;
/// Native effect requests a server queues per channel.
pub const MAX_EFFECTS_PER_CHANNEL: usize = 8;
/// Style definitions one channel keeps.
pub const MAX_STYLES: usize = 4096;
/// Hyperlink definitions one channel keeps.
pub const MAX_LINKS: usize = crate::pane::effects::MAX_SCREEN_HYPERLINKS;
/// Largest hyperlink target in a link definition.
pub const MAX_LINK_BYTES: usize = 4096;
/// Largest text of one cell. Servers shorten longer grapheme clusters at a character boundary, so
/// a complete screen at the largest grid stays under [`MAX_UPDATE_BYTES`].
pub const MAX_CELL_BYTES: usize = 64;
/// Rows in the first Scrollback page a client requests after opening a channel or Find.
pub const FIRST_PAGE_ROWS: u16 = 200;
/// Rows in one Scrollback page.
pub const MAX_PAGE_ROWS: u16 = 500;
/// Scrollback pages in flight per channel.
pub const MAX_PAGES_IN_FLIGHT: usize = 4;
/// Scrollback rows a client caches per Pane.
pub const MAX_CACHED_ROWS: usize = 10_000;
/// Frame rates a client may declare in `View::max_hz`.
pub const FRAME_RATES: [u8; 2] = [60, 120];

/// L0 frames a writer queues before the connection is `Stalled`.
pub const MAX_L0_FRAMES: usize = 256;
/// L0 bytes a server queues before the connection is `Stalled`.
pub const MAX_L0_BYTES: usize = 1024 * 1024;
/// Client L0 bytes queued for input, acknowledgements, views and pings, excluding requests,
/// which [`MAX_REQUESTS_IN_FLIGHT`] bounds. Holds a whole [`MAX_PASTE_BYTES`] paste with headroom.
pub const MAX_CLIENT_L0_BYTES: usize = 2 * MAX_PASTE_BYTES;
/// L1 messages a server queues before the connection is `Stalled`.
pub const MAX_L1_MESSAGES: usize = 4096;
/// L2 messages a server queues before the connection is `Stalled`.
pub const MAX_L2_MESSAGES: usize = MAX_REQUESTS_IN_FLIGHT;

/// A peer pings after receiving nothing for this long.
pub const PING_AFTER: Duration = Duration::from_secs(5);
/// A connection is dead after receiving nothing for this long.
pub const DEAD_AFTER: Duration = Duration::from_secs(15);
/// A connection is dead after its writes or frame acknowledgements stall for this long.
pub const STALL_AFTER: Duration = Duration::from_secs(10);
