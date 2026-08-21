#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicalSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl LogicalSize {
    pub(crate) const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogicalCellSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl LogicalCellSize {
    pub(crate) const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct LogicalPosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl LogicalPosition {
    pub(crate) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BackingScale(f32);

impl BackingScale {
    pub(crate) const ONE: Self = Self(1.0);

    pub(crate) fn new(factor: f32) -> Option<Self> {
        (factor.is_finite() && factor > 0.0).then_some(Self(factor))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BackingPosition {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl BackingPosition {
    pub(crate) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CellGridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl CellGridSize {
    pub(crate) const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CellGridPosition {
    pub(crate) col: u16,
    pub(crate) row: u16,
}

impl CellGridPosition {
    pub(crate) const fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackingSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl BackingSize {
    pub(crate) const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerminalGeometry {
    grid: CellGridSize,
    logical_cell: LogicalCellSize,
    backing_scale: BackingScale,
    backing_cell: BackingSize,
    backing_grid: BackingSize,
}

impl TerminalGeometry {
    pub(crate) fn from_viewport(
        viewport: LogicalSize,
        cell: LogicalCellSize,
        backing_scale: BackingScale,
        minimum_grid: CellGridSize,
    ) -> Self {
        let cols = grid_dimension(viewport.width, cell.width, minimum_grid.cols);
        let rows = grid_dimension(viewport.height, cell.height, minimum_grid.rows);
        Self::from_grid(CellGridSize::new(cols, rows), cell, backing_scale)
    }

    pub(crate) fn from_grid(
        grid: CellGridSize,
        cell: LogicalCellSize,
        backing_scale: BackingScale,
    ) -> Self {
        let backing_cell = backing_size(LogicalSize::new(cell.width, cell.height), backing_scale);
        let backing_grid = backing_size(
            LogicalSize::new(
                cell.width * f32::from(grid.cols),
                cell.height * f32::from(grid.rows),
            ),
            backing_scale,
        );

        Self {
            grid,
            logical_cell: cell,
            backing_scale,
            backing_cell,
            backing_grid,
        }
    }

    pub(crate) const fn grid(self) -> CellGridSize {
        self.grid
    }

    pub(crate) const fn backing_cell_size(self) -> BackingSize {
        self.backing_cell
    }

    pub(crate) const fn backing_grid_size(self) -> BackingSize {
        self.backing_grid
    }

    pub(crate) fn logical_grid_size(self) -> LogicalSize {
        LogicalSize::new(
            self.logical_cell.width * f32::from(self.grid.cols),
            self.logical_cell.height * f32::from(self.grid.rows),
        )
    }

    pub(crate) fn to_backing_position(self, logical: LogicalPosition) -> BackingPosition {
        BackingPosition::new(
            logical.x * self.backing_scale.0,
            logical.y * self.backing_scale.0,
        )
    }

    pub(crate) fn cell_at_backing_position(self, position: BackingPosition) -> CellGridPosition {
        let cell_width = self.logical_cell.width * self.backing_scale.0;
        let cell_height = self.logical_cell.height * self.backing_scale.0;
        let col = (position.x / cell_width)
            .floor()
            .clamp(0.0, f32::from(self.grid.cols.saturating_sub(1))) as u16;
        let row = (position.y / cell_height)
            .floor()
            .clamp(0.0, f32::from(self.grid.rows.saturating_sub(1))) as u16;
        CellGridPosition::new(col, row)
    }
}

fn grid_dimension(available: f32, cell: f32, minimum: u16) -> u16 {
    let calculated = if available.is_finite() && cell.is_finite() && cell > 0.0 {
        (available.max(cell) / cell).floor() as u16
    } else {
        minimum
    };
    calculated.max(minimum)
}

fn backing_size(logical: LogicalSize, scale: BackingScale) -> BackingSize {
    BackingSize::new(
        backing_dimension(logical.width, scale),
        backing_dimension(logical.height, scale),
    )
}

fn backing_dimension(logical: f32, scale: BackingScale) -> u32 {
    (logical * scale.0).ceil().clamp(1.0, u32::MAX as f32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_logical_cells_scale_the_complete_grid_without_accumulating_rounding() {
        let geometry = TerminalGeometry::from_viewport(
            LogicalSize::new(101.0, 52.0),
            LogicalCellSize::new(7.5, 17.25),
            BackingScale::new(1.5).unwrap(),
            CellGridSize::new(2, 2),
        );

        assert_eq!(
            (
                geometry.grid(),
                geometry.logical_grid_size(),
                geometry.backing_cell_size(),
                geometry.backing_grid_size(),
            ),
            (
                CellGridSize::new(13, 3),
                LogicalSize::new(97.5, 51.75),
                BackingSize::new(12, 26),
                BackingSize::new(147, 78),
            )
        );
    }

    #[test]
    fn backing_scale_changes_pixels_without_changing_the_cell_grid() {
        let viewport = LogicalSize::new(80.0, 40.0);
        let cell = LogicalCellSize::new(8.0, 20.0);
        let minimum = CellGridSize::new(2, 2);
        let one_x = TerminalGeometry::from_viewport(
            viewport,
            cell,
            BackingScale::new(1.0).unwrap(),
            minimum,
        );
        let two_x = TerminalGeometry::from_viewport(
            viewport,
            cell,
            BackingScale::new(2.0).unwrap(),
            minimum,
        );

        assert_eq!(
            (
                one_x.grid(),
                two_x.grid(),
                one_x.backing_grid_size(),
                two_x.backing_grid_size(),
            ),
            (
                CellGridSize::new(10, 2),
                CellGridSize::new(10, 2),
                BackingSize::new(80, 40),
                BackingSize::new(160, 80),
            )
        );
    }

    #[test]
    fn logical_mouse_positions_use_the_same_fractional_backing_scale_as_grid_dimensions() {
        let geometry = TerminalGeometry::from_grid(
            CellGridSize::new(10, 2),
            LogicalCellSize::new(7.5, 17.25),
            BackingScale::new(1.5).unwrap(),
        );

        assert_eq!(
            (
                geometry.to_backing_position(LogicalPosition::new(3.75, 8.625)),
                geometry.backing_grid_size(),
            ),
            (
                BackingPosition::new(5.625, 12.9375),
                BackingSize::new(113, 52),
            )
        );
    }
}
