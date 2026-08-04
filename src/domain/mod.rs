mod terminal_window;

pub(crate) use terminal_window::{
    ClosePaneOutcome, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef, SplitAxis,
    SplitId, TerminalWindow, WindowId, ZoomState,
};
