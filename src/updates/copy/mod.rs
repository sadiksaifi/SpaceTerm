//! Every person-facing update string of the desktop. Each submodule renders one family of
//! surfaces from [`super::state::UpdateView`]; no other module writes update text.

pub(crate) mod dialogs;
pub(crate) mod pane;
pub(crate) mod reminders;
pub(crate) mod rows;
pub(crate) mod settings;
pub(crate) mod title_bar;
