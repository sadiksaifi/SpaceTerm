//! Demand-driven terminal semantics. These values grant no native or Pane authority.
use serde::{Deserialize, Serialize};
use std::ops::Range;

pub const CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Screen {
    Primary,
    Alternate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RowId {
    pub screen: Screen,
    pub screen_generation: u64,
    pub node_serial: u64,
    pub page_row: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub column: u16,
    pub width_cells: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellRef {
    pub row: RowId,
    pub row_revision: u64,
    pub column: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionRefs {
    pub start: CellRef,
    pub end: CellRef,
    pub rectangle: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub id: RowId,
    pub revision: u64,
    pub soft_wrapped: bool,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub screen: Screen,
    pub screen_generation: u64,
    pub topology: Vec<RowId>,
    pub rows: Vec<Row>,
    pub visible_lines: Range<usize>,
    pub cursor: Option<CellRef>,
    pub selection: Option<SelectionRefs>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub revision: u64,
    pub offset: usize,
    pub total: usize,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Error {
    Pending,
    Stale,
    TooLarge,
}
