//! The Worktree Path Template this device's Settings name, which seeds a server that has none.

use gpui::{App, Global};

use crate::settings::Settings;

struct InstalledWorktreePathTemplate(Settings);

impl Global for InstalledWorktreePathTemplate {}

pub(crate) fn install(settings: &Settings, cx: &mut App) {
    cx.set_global(InstalledWorktreePathTemplate(settings.clone()));
}

/// Where the person wants new Worktrees, once Settings are installed.
pub(crate) fn path_template(cx: &App) -> Option<String> {
    cx.try_global::<InstalledWorktreePathTemplate>()
        .map(|installed| {
            installed
                .0
                .snapshot()
                .candidate
                .git
                .worktree_path_template
                .clone()
        })
}
