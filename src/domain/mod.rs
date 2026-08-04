mod terminal_window;

pub(crate) use terminal_window::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef,
    SplitAxis, SplitId, TerminalWindow, WindowId, ZoomState,
};
