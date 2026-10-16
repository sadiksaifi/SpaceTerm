//! Bounded reading operations over the controlling Pane lease.
use super::{Pointer, Wheel};
use serde::{Deserialize, Serialize};

pub const MAX_QUERY_BYTES: usize = 4096;
pub const SELECTION_CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_SELECTION_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    Scroll(Scroll),
    ScrollTo { offset: u64, screen: u64 },
    Pointer(Pointer),
    Wheel(Wheel),
    Query { generation: u64, text: String },
    Navigate { generation: u64, backwards: bool },
    EndFind { generation: u64 },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Scroll {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub total_rows: u64,
    pub offset_rows: u64,
    pub visible_rows: u64,
    pub selection_present: bool,
    pub selection_revision: u64,
    pub find: Option<Find>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Find {
    pub generation: u64,
    pub total: usize,
    pub current: Option<usize>,
    pub spans: Vec<(u16, u16, u16, bool)>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub plain_text: String,
    pub html: Option<String>,
}
