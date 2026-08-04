mod terminal_window;
mod window_collection;

pub(crate) use terminal_window::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef,
    SplitAxis, SplitId, TerminalWindow, WindowId, ZoomState,
};
pub(crate) use window_collection::{CloseWindowOutcome, WindowCollection, WindowError};
