//! Measured geometry admission before runtime or memory reservations.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Supports an 8K (7680 x 4320) desktop even at 4 x 8 pixel cells
/// (1920 x 540 = 1,036,800 cells). The row/column ceilings leave headroom
/// without permitting arbitrary u16 x u16 allocations. Aggregate residency
/// admission is separate and may negotiate lower limits. PTY rows, columns,
/// and multiplied pixel dimensions must still fit the existing u16 ABI.
pub const SUPPORTED_GRID_LIMITS: GridLimits = GridLimits {
    max_rows: 1024,
    max_columns: 2048,
    max_cells: 1_048_576,
};

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GridError {
    #[error("grid dimensions must be nonzero")]
    ZeroDimension,
    #[error("negotiated grid limits are unsupported")]
    UnsupportedLimits,
    #[error("grid dimensions exceed negotiated limits")]
    TooLarge,
    #[error("grid exceeds negotiated cell limit")]
    TooManyCells,
    #[error("grid arithmetic or PTY pixel dimensions overflow")]
    Overflow,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "LimitParts", into = "LimitParts")]
pub struct GridLimits {
    max_rows: u32,
    max_columns: u32,
    max_cells: u32,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct LimitParts {
    max_rows: u32,
    max_columns: u32,
    max_cells: u32,
}
impl GridLimits {
    pub fn new(max_rows: u32, max_columns: u32, max_cells: u32) -> Result<Self, GridError> {
        if max_rows == 0
            || max_columns == 0
            || max_cells == 0
            || max_rows > SUPPORTED_GRID_LIMITS.max_rows
            || max_columns > SUPPORTED_GRID_LIMITS.max_columns
            || max_cells > SUPPORTED_GRID_LIMITS.max_cells
        {
            return Err(GridError::UnsupportedLimits);
        }
        Ok(Self {
            max_rows,
            max_columns,
            max_cells,
        })
    }
    pub const fn max_rows(self) -> u32 {
        self.max_rows
    }
    pub const fn max_columns(self) -> u32 {
        self.max_columns
    }
    pub const fn max_cells(self) -> u32 {
        self.max_cells
    }
    /// Negotiation may only reduce either endpoint's limits.
    pub fn intersect(self, other: Self) -> Self {
        Self {
            max_rows: self.max_rows.min(other.max_rows),
            max_columns: self.max_columns.min(other.max_columns),
            max_cells: self.max_cells.min(other.max_cells),
        }
    }
}
impl TryFrom<LimitParts> for GridLimits {
    type Error = GridError;
    fn try_from(value: LimitParts) -> Result<Self, Self::Error> {
        Self::new(value.max_rows, value.max_columns, value.max_cells)
    }
}
impl From<GridLimits> for LimitParts {
    fn from(value: GridLimits) -> Self {
        Self {
            max_rows: value.max_rows,
            max_columns: value.max_columns,
            max_cells: value.max_cells,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "PixelParts", into = "PixelParts")]
pub struct CellPixels {
    width: u16,
    height: u16,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct PixelParts {
    width: u16,
    height: u16,
}
impl CellPixels {
    pub fn new(width: u16, height: u16) -> Result<Self, GridError> {
        if width == 0 || height == 0 {
            return Err(GridError::ZeroDimension);
        }
        Ok(Self { width, height })
    }
    pub const fn width(self) -> u16 {
        self.width
    }
    pub const fn height(self) -> u16 {
        self.height
    }
}
impl TryFrom<PixelParts> for CellPixels {
    type Error = GridError;
    fn try_from(value: PixelParts) -> Result<Self, Self::Error> {
        Self::new(value.width, value.height)
    }
}
impl From<CellPixels> for PixelParts {
    fn from(value: CellPixels) -> Self {
        Self {
            width: value.width,
            height: value.height,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GridParts", into = "GridParts")]
pub struct GridSize {
    rows: u16,
    columns: u16,
    cell_pixels: CellPixels,
}
#[derive(Clone, Copy, Deserialize, Serialize)]
struct GridParts {
    rows: u16,
    columns: u16,
    cell_pixels: CellPixels,
}
impl GridSize {
    pub fn new(
        rows: u32,
        columns: u32,
        cell_pixels: CellPixels,
        limits: GridLimits,
    ) -> Result<Self, GridError> {
        if rows == 0 || columns == 0 {
            return Err(GridError::ZeroDimension);
        }
        if rows > limits.max_rows || columns > limits.max_columns {
            return Err(GridError::TooLarge);
        }
        let cells = rows.checked_mul(columns).ok_or(GridError::Overflow)?;
        if cells > limits.max_cells {
            return Err(GridError::TooManyCells);
        }
        let width = columns
            .checked_mul(u32::from(cell_pixels.width))
            .ok_or(GridError::Overflow)?;
        let height = rows
            .checked_mul(u32::from(cell_pixels.height))
            .ok_or(GridError::Overflow)?;
        if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
            return Err(GridError::Overflow);
        }
        Ok(Self {
            rows: u16::try_from(rows).map_err(|_| GridError::Overflow)?,
            columns: u16::try_from(columns).map_err(|_| GridError::Overflow)?,
            cell_pixels,
        })
    }
    pub const fn rows(self) -> u16 {
        self.rows
    }
    pub const fn columns(self) -> u16 {
        self.columns
    }
    pub const fn cell_pixels(self) -> CellPixels {
        self.cell_pixels
    }
    pub fn cells(self) -> u32 {
        u32::from(self.rows) * u32::from(self.columns)
    }
    /// Wire/durable decoding checks hard limits. Admission checks negotiated limits again.
    pub fn validate(self, limits: GridLimits) -> Result<Self, GridError> {
        Self::new(
            u32::from(self.rows),
            u32::from(self.columns),
            self.cell_pixels,
            limits,
        )
    }
}
impl TryFrom<GridParts> for GridSize {
    type Error = GridError;
    fn try_from(value: GridParts) -> Result<Self, Self::Error> {
        Self::new(
            u32::from(value.rows),
            u32::from(value.columns),
            value.cell_pixels,
            SUPPORTED_GRID_LIMITS,
        )
    }
}
impl From<GridSize> for GridParts {
    fn from(value: GridSize) -> Self {
        Self {
            rows: value.rows,
            columns: value.columns,
            cell_pixels: value.cell_pixels,
        }
    }
}

/// Server measurement preserving valid cell dimensions even when a multiplied
/// PTY pixel extent cannot fit u16. Each unrepresentable extent becomes zero.
/// Legacy GridSize retains its original overflow rejection and serialized form.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "GridParts", into = "GridParts")]
pub struct MeasuredGrid {
    rows: u16,
    columns: u16,
    cell_pixels: CellPixels,
}
impl MeasuredGrid {
    pub fn new(
        rows: u32,
        columns: u32,
        cell_pixels: CellPixels,
        limits: GridLimits,
    ) -> Result<Self, GridError> {
        if rows == 0 || columns == 0 {
            return Err(GridError::ZeroDimension);
        }
        if rows > limits.max_rows || columns > limits.max_columns {
            return Err(GridError::TooLarge);
        }
        if rows.checked_mul(columns).ok_or(GridError::Overflow)? > limits.max_cells {
            return Err(GridError::TooManyCells);
        }
        Ok(Self {
            rows: u16::try_from(rows).map_err(|_| GridError::Overflow)?,
            columns: u16::try_from(columns).map_err(|_| GridError::Overflow)?,
            cell_pixels,
        })
    }
    pub const fn rows(self) -> u16 {
        self.rows
    }
    pub const fn columns(self) -> u16 {
        self.columns
    }
    pub const fn cell_pixels(self) -> CellPixels {
        self.cell_pixels
    }
    pub fn cells(self) -> u32 {
        u32::from(self.rows) * u32::from(self.columns)
    }
    /// Width and height are independent: an overflowing width does not discard
    /// the representable height or the engine's actual cell metrics.
    pub fn pty_pixels(self) -> (u16, u16) {
        (
            self.columns
                .checked_mul(self.cell_pixels.width)
                .unwrap_or(0),
            self.rows.checked_mul(self.cell_pixels.height).unwrap_or(0),
        )
    }
    pub fn validate(self, limits: GridLimits) -> Result<Self, GridError> {
        Self::new(
            u32::from(self.rows),
            u32::from(self.columns),
            self.cell_pixels,
            limits,
        )
    }
}
impl From<GridSize> for MeasuredGrid {
    fn from(value: GridSize) -> Self {
        Self {
            rows: value.rows,
            columns: value.columns,
            cell_pixels: value.cell_pixels,
        }
    }
}
impl TryFrom<GridParts> for MeasuredGrid {
    type Error = GridError;
    fn try_from(value: GridParts) -> Result<Self, Self::Error> {
        Self::new(
            u32::from(value.rows),
            u32::from(value.columns),
            value.cell_pixels,
            SUPPORTED_GRID_LIMITS,
        )
    }
}
impl From<MeasuredGrid> for GridParts {
    fn from(value: MeasuredGrid) -> Self {
        Self {
            rows: value.rows,
            columns: value.columns,
            cell_pixels: value.cell_pixels,
        }
    }
}
