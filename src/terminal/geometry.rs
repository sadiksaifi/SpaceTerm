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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BackingScale(f32);

impl BackingScale {
    pub(crate) fn new(factor: f32) -> Option<Self> {
        (factor.is_finite() && factor > 0.0).then_some(Self(factor))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl GridSize {
    pub(crate) const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
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
    grid: GridSize,
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
        minimum_grid: GridSize,
    ) -> Self {
        let cols = grid_dimension(viewport.width, cell.width, minimum_grid.cols);
        let rows = grid_dimension(viewport.height, cell.height, minimum_grid.rows);
        let grid = GridSize::new(cols, rows);
        let backing_cell = backing_size(LogicalSize::new(cell.width, cell.height), backing_scale);
        let backing_grid = backing_size(
            LogicalSize::new(cell.width * f32::from(cols), cell.height * f32::from(rows)),
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

    pub(crate) const fn grid(self) -> GridSize {
        self.grid
    }

    pub(crate) const fn backing_cell_size(self) -> BackingSize {
        self.backing_cell
    }

    pub(crate) const fn backing_grid_size(self) -> BackingSize {
        self.backing_grid
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
            GridSize::new(2, 2),
        );

        assert_eq!(
            (
                geometry.grid(),
                geometry.backing_cell_size(),
                geometry.backing_grid_size(),
            ),
            (
                GridSize::new(13, 3),
                BackingSize::new(12, 26),
                BackingSize::new(147, 78),
            )
        );
    }
}
