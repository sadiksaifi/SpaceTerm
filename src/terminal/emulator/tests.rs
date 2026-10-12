#[cfg(feature = "native-tests")]
use super::*;
#[cfg(feature = "native-tests")]
use crate::terminal::{
    geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry},
    identity,
    metadata::TerminalMetadataContext,
};
#[cfg(feature = "native-tests")]
use std::{fs, time::Instant};

#[cfg(feature = "native-tests")]
fn geometry(cols: u16, rows: u16, width: f32, height: f32) -> TerminalGeometry {
    TerminalGeometry::from_grid(
        CellGridSize::new(cols, rows),
        LogicalCellSize::new(width, height),
        BackingScale::ONE,
    )
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    feature = "native-tests"
))]
mod unix_adapter_tests {
    include!("../../platform/unix_adapter_tests/emulator.rs");
}
