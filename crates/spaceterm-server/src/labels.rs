//! The one label formatter clients share, so the desktop and the CLI name Panes and Tabs alike.
use crate::{hierarchy, pane};

/// A Pane's `activity` is its title or running command. Its `line` is `activity · place`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    pub activity: String,
    pub line: String,
}

pub fn pane_label(_snapshot: &pane::metadata::Snapshot) -> Label {
    Label::default()
}

pub fn tab_label(_tab: &hierarchy::Tab, _panes: &[Label]) -> Label {
    Label::default()
}
