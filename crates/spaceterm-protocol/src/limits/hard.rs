pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MANAGEMENT_BYTES: usize = 64 * 1024;
pub const MAX_SEMANTIC_ROWS: usize = 256;
pub const MAX_SEMANTIC_CELLS: usize = 16_384;
pub const MAX_PALETTE_COLORS: usize = 256;
/// Per-list ceiling for semantic hyperlinks, graphics placements, and Find matches.
pub const MAX_SEMANTIC_REFERENCES: usize = 1024;
pub const MAX_RESOURCE_CHUNK_BYTES: usize = 256 * 1024;
pub const MAX_CONCURRENT_TRANSFERS: usize = 4;
pub const MAX_EGRESS_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_EGRESS_ENVELOPES: usize = 128;
/// Reserved inside (not in addition to) the egress ceiling for lifecycle/outcomes.
pub const RESERVED_LIFECYCLE_BYTES: usize = 256 * 1024;
pub const RESERVED_LIFECYCLE_ENVELOPES: usize = 8;
pub const MAX_PASTE_BYTES: usize = 1024 * 1024;
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
pub const MAX_PENDING_UNSAFE_PASTES: usize = 1;
pub const PASTE_EXPIRY_SECONDS: u64 = 30;
pub const OSC52_READ_DEADLINE_MILLIS: u64 = 1000;
pub const MAX_PENDING_PTY_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SCREEN_GRAPHICS_BYTES: usize = 96 * 1024 * 1024;
pub const MAX_GRAPHICS_TRANSMISSION_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_DECODED_GRAPHICS_BYTES: usize = 384 * 1024 * 1024;
pub const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
pub const MAX_NESTING_DEPTH: usize = 16;
/// Covers major 2's one run per cell, graphemes, rows, and all bounded page metadata.
/// Independent row and cell ceilings still apply before generated decoding.
pub const MAX_REPEATED_VALUES: usize =
    2 * MAX_SEMANTIC_CELLS + MAX_SEMANTIC_ROWS + MAX_PALETTE_COLORS + 3 * MAX_SEMANTIC_REFERENCES;
pub const MAX_FIELDS: usize = 262_144;
