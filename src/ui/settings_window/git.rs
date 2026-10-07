//! The Git section: whether Repository Status and Pull Requests show, and the state of the tools
//! that provide them.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString};
use spaceterm_ui::{Switch, ToggleSize};

use super::{SettingsRowId, SettingsWindow};
use crate::repository_status::{
    GitHubCliStatus, GitToolStatus, RepositoryStatusPreferences, ToolVersion,
};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::repository_status_store::RepositoryTools;
use crate::ui::sidebar_window::form::badge;

/// A tool row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ToolPresentation {
    pub(super) state: SharedString,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: &'static str,
}

pub(super) fn git_presentation(status: GitToolStatus) -> ToolPresentation {
    let (state, state_selector, explanation) = match status {
        GitToolStatus::Unknown => (
            SharedString::new_static("Not Checked"),
            "settings-git-tool-state-unknown",
            "SpaceTerm looks for Git while Repository Status is on.",
        ),
        GitToolStatus::Ready(version) => (
            version_text(version),
            "settings-git-tool-state-ready",
            "SpaceTerm reads repositories with this Git and never changes them.",
        ),
        GitToolStatus::TooOld(version) => (
            version_text(version),
            "settings-git-tool-state-too-old",
            "Repository Status needs Git 2.15 or later.",
        ),
        GitToolStatus::NotFound => (
            SharedString::new_static("Not Found"),
            "settings-git-tool-state-not-found",
            "Install Git to show Repository Status.",
        ),
    };
    ToolPresentation {
        state,
        state_selector,
        explanation,
    }
}

pub(super) fn github_cli_presentation(status: GitHubCliStatus) -> ToolPresentation {
    let (state, state_selector, explanation) = match status {
        GitHubCliStatus::Unknown => (
            "Not Checked",
            "settings-github-cli-state-unknown",
            "SpaceTerm looks for the GitHub CLI while Pull Requests are shown.",
        ),
        GitHubCliStatus::SignedIn => (
            "Logged In",
            "settings-github-cli-state-signed-in",
            "Pull requests come from your GitHub CLI login. SpaceTerm stores no tokens.",
        ),
        GitHubCliStatus::SignedOut => (
            "Not Logged In",
            "settings-github-cli-state-signed-out",
            "Run \"gh auth login\" in a terminal.",
        ),
        GitHubCliStatus::NotFound => (
            "Not Found",
            "settings-github-cli-state-not-found",
            "Install the GitHub CLI to show pull requests.",
        ),
        GitHubCliStatus::Unavailable => (
            "Unavailable",
            "settings-github-cli-state-unavailable",
            "The GitHub CLI didn't answer. SpaceTerm asks again when a window becomes active.",
        ),
    };
    ToolPresentation {
        state: SharedString::new_static(state),
        state_selector,
        explanation,
    }
}

fn version_text(version: ToolVersion) -> SharedString {
    format!("{}.{}.{}", version.major, version.minor, version.patch).into()
}

fn tools(cx: &App) -> RepositoryTools {
    cx.try_global::<RepositoryTools>()
        .copied()
        .unwrap_or_default()
}

impl SettingsWindow {
    pub(super) fn render_git_preference(
        &mut self,
        row: SettingsRowId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.editor.document().git;
        let (value, selector, disabled) = if row == SettingsRowId::ShowPullRequests {
            (
                current.show_pull_requests,
                "settings-show-pull-requests",
                !current.show_repository_status,
            )
        } else {
            (
                current.show_repository_status,
                "settings-show-repository-status",
                false,
            )
        };
        let owner = cx.weak_entity();
        Switch::new(
            selector,
            row.descriptor().label(self.permission_access.naming()),
            value,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!self.editor.editable() || disabled)
        .debug_selector(selector)
        .on_change(move |change, _, cx| {
            let value = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| set(row, &mut draft.git, value), cx);
            });
        })
        .into_any_element()
    }

    pub(super) fn render_git_tool(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let presentation = self.git_tool_presentation(row, cx);
        gpui::div()
            .debug_selector(|| format!("{}-control", row.descriptor().selector))
            .child(
                gpui::div()
                    .debug_selector(move || presentation.state_selector.to_owned())
                    .child(badge(
                        presentation.state_selector,
                        presentation.state,
                        appearance,
                    )),
            )
            .into_any_element()
    }

    pub(super) fn git_tool_presentation(&self, row: SettingsRowId, cx: &App) -> ToolPresentation {
        let tools = tools(cx);
        if row == SettingsRowId::GitHubCli {
            github_cli_presentation(tools.github_cli)
        } else {
            git_presentation(tools.git)
        }
    }

    pub(super) fn git_preference_differs(&self, row: SettingsRowId) -> Option<bool> {
        let current = self.editor.document().git;
        let defaults = RepositoryStatusPreferences::default();
        match row {
            SettingsRowId::ShowRepositoryStatus => {
                Some(current.show_repository_status != defaults.show_repository_status)
            }
            SettingsRowId::ShowPullRequests => {
                Some(current.show_pull_requests != defaults.show_pull_requests)
            }
            _ => None,
        }
    }

    pub(super) fn reset_git_preference(&mut self, row: SettingsRowId, cx: &mut Context<Self>) {
        let defaults = RepositoryStatusPreferences::default();
        let value = if row == SettingsRowId::ShowPullRequests {
            defaults.show_pull_requests
        } else {
            defaults.show_repository_status
        };
        self.edit(move |draft| set(row, &mut draft.git, value), cx);
    }
}

fn set(row: SettingsRowId, preferences: &mut RepositoryStatusPreferences, value: bool) {
    match row {
        SettingsRowId::ShowRepositoryStatus => preferences.show_repository_status = value,
        SettingsRowId::ShowPullRequests => preferences.show_pull_requests = value,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::MINIMUM_GIT_VERSION;

    #[test]
    fn every_git_state_reads_one_way() {
        let ready = git_presentation(GitToolStatus::Ready(ToolVersion {
            major: 2,
            minor: 47,
            patch: 0,
        }));
        assert_eq!(ready.state.as_ref(), "2.47.0");
        let old = git_presentation(GitToolStatus::TooOld(ToolVersion {
            major: 2,
            minor: 14,
            patch: 6,
        }));
        assert_eq!(old.state.as_ref(), "2.14.6");
        assert_eq!(
            old.explanation,
            "Repository Status needs Git 2.15 or later."
        );
        assert_eq!(
            git_presentation(GitToolStatus::NotFound).explanation,
            "Install Git to show Repository Status."
        );
    }

    #[test]
    fn a_logged_out_github_cli_names_the_fix() {
        let presentation = github_cli_presentation(GitHubCliStatus::SignedOut);

        assert_eq!(presentation.state.as_ref(), "Not Logged In");
        assert_eq!(
            presentation.explanation,
            "Run \"gh auth login\" in a terminal."
        );
    }

    #[test]
    fn the_minimum_git_version_matches_the_explanation() {
        assert_eq!(
            (MINIMUM_GIT_VERSION.major, MINIMUM_GIT_VERSION.minor),
            (2, 15)
        );
    }
}
