#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalSize {
    pub width: f32,
    pub height: f32,
}

impl LogicalSize {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicalCellSize {
    pub width: f32,
    pub height: f32,
}

impl LogicalCellSize {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalPosition {
    pub x: f32,
    pub y: f32,
}

impl LogicalPosition {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackingScale(f32);

impl BackingScale {
    pub const ONE: Self = Self(1.0);

    pub fn new(factor: f32) -> Option<Self> {
        (factor.is_finite() && factor > 0.0).then_some(Self(factor))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BackingPosition {
    pub x: f32,
    pub y: f32,
}

impl BackingPosition {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellGridSize {
    pub cols: u16,
    pub rows: u16,
}

impl CellGridSize {
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellGridPosition {
    pub col: u16,
    pub row: u16,
}

impl CellGridPosition {
    pub const fn new(col: u16, row: u16) -> Self {
        Self { col, row }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackingSize {
    pub width: u32,
    pub height: u32,
}

impl BackingSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalGeometry {
    grid: CellGridSize,
    logical_cell: LogicalCellSize,
    backing_scale: BackingScale,
    backing_cell: BackingSize,
    backing_grid: BackingSize,
}

impl TerminalGeometry {
    pub fn from_viewport(
        viewport: LogicalSize,
        cell: LogicalCellSize,
        backing_scale: BackingScale,
        minimum_grid: CellGridSize,
    ) -> Self {
        let cols = grid_dimension(viewport.width, cell.width, minimum_grid.cols);
        let rows = grid_dimension(viewport.height, cell.height, minimum_grid.rows);
        let grid = CellGridSize::new(cols, rows);
        let fitted_cell = Self::fitted_cell_size(viewport, cell, grid);
        Self::from_grid_with_logical_cell(grid, cell, fitted_cell, backing_scale)
    }

    pub fn fitted_cell_size(
        viewport: LogicalSize,
        nominal: LogicalCellSize,
        grid: CellGridSize,
    ) -> LogicalCellSize {
        LogicalCellSize::new(
            fitted_cell_dimension(viewport.width, nominal.width, grid.cols),
            fitted_cell_dimension(viewport.height, nominal.height, grid.rows),
        )
    }

    #[cfg(test)]
    pub fn from_grid(
        grid: CellGridSize,
        cell: LogicalCellSize,
        backing_scale: BackingScale,
    ) -> Self {
        Self::from_grid_with_logical_cell(grid, cell, cell, backing_scale)
    }

    fn from_grid_with_logical_cell(
        grid: CellGridSize,
        nominal_cell: LogicalCellSize,
        logical_cell: LogicalCellSize,
        backing_scale: BackingScale,
    ) -> Self {
        let backing_cell = backing_size(
            LogicalSize::new(nominal_cell.width, nominal_cell.height),
            backing_scale,
        );
        let backing_grid = backing_size(
            LogicalSize::new(
                logical_cell.width * f32::from(grid.cols),
                logical_cell.height * f32::from(grid.rows),
            ),
            backing_scale,
        );

        Self {
            grid,
            logical_cell,
            backing_scale,
            backing_cell,
            backing_grid,
        }
    }

    pub const fn grid(self) -> CellGridSize {
        self.grid
    }

    pub const fn logical_cell_size(self) -> LogicalCellSize {
        self.logical_cell
    }

    pub const fn backing_cell_size(self) -> BackingSize {
        self.backing_cell
    }

    pub const fn backing_grid_size(self) -> BackingSize {
        self.backing_grid
    }

    #[cfg(test)]
    pub fn logical_grid_size(self) -> LogicalSize {
        LogicalSize::new(
            self.logical_cell.width * f32::from(self.grid.cols),
            self.logical_cell.height * f32::from(self.grid.rows),
        )
    }

    pub fn to_backing_position(self, logical: LogicalPosition) -> BackingPosition {
        BackingPosition::new(
            logical.x * self.backing_scale.0,
            logical.y * self.backing_scale.0,
        )
    }

    pub fn cell_at_backing_position(self, position: BackingPosition) -> CellGridPosition {
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

/// The cell length that spreads `cells` across `available`. A grid with room for another nominal
/// cell was laid out for a smaller viewport and keeps nominal cells until it is resized.
fn fitted_cell_dimension(available: f32, nominal: f32, cells: u16) -> f32 {
    if !available.is_finite()
        || !nominal.is_finite()
        || nominal <= 0.0
        || cells == 0
        || grid_dimension(available, nominal, 0) > cells
    {
        return nominal;
    }
    (available / f32::from(cells)).max(nominal)
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
    fn viewport_remainder_should_be_distributed_across_real_cells() {
        let geometry = TerminalGeometry::from_viewport(
            LogicalSize::new(101.0, 52.0),
            LogicalCellSize::new(7.5, 17.25),
            BackingScale::new(1.5).unwrap(),
            CellGridSize::new(2, 2),
        );

        assert_eq!(
            (
                geometry.grid(),
                geometry.logical_cell,
                geometry.logical_grid_size(),
                geometry.backing_cell_size(),
                geometry.backing_grid_size(),
            ),
            (
                CellGridSize::new(13, 3),
                LogicalCellSize::new(101.0 / 13.0, 52.0 / 3.0),
                LogicalSize::new(101.0, 52.0),
                BackingSize::new(12, 26),
                BackingSize::new(152, 78),
            )
        );
    }

    #[test]
    fn a_grid_with_room_for_another_cell_should_keep_its_nominal_cells() {
        let nominal = LogicalCellSize::new(7.5, 17.25);

        assert_eq!(
            (
                TerminalGeometry::fitted_cell_size(
                    LogicalSize::new(202.0, 104.0),
                    nominal,
                    CellGridSize::new(13, 3),
                ),
                TerminalGeometry::fitted_cell_size(
                    LogicalSize::new(101.0, 104.0),
                    nominal,
                    CellGridSize::new(13, 3),
                ),
            ),
            (nominal, LogicalCellSize::new(101.0 / 13.0, 17.25)),
            "a grid laid out for a smaller viewport must not stretch until it is resized"
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
