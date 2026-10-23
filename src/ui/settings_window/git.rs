//! The Git section. Repository Status and Pull Requests are Client Settings, and the GitHub CLI
//! runs on this computer, so those rows ignore the server picker. "Git on <server>" follows the
//! picker: its Git row reads the selected server's Git, and [`super::server_location`] edits its
//! New Worktree Location.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, SharedString, Text};
use spaceterm_ui::{Switch, ToggleSize};

use super::{SettingsRowId, SettingsWindow};
use crate::repository_status::{GitHubCliStatus, GitToolStatus, ToolVersion};
use crate::settings::git::GitPreferences;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::repository_status_store::RepositoryTools;
use crate::ui::sidebar_window::form::badge;
use crate::worktrees::path_template::WorktreePathTemplateError;

/// New Worktree Location's description, beside its field.
pub(super) const WORKTREE_LOCATION_DESCRIPTION: &str = "Use {repository} and {branch}.";

/// Why New Worktree Location cannot be saved, shown under its field.
pub(super) fn worktree_location_problem(problem: WorktreePathTemplateError) -> &'static str {
    match problem {
        WorktreePathTemplateError::Empty => "Enter a location.",
        WorktreePathTemplateError::TooLong => "Use 1,024 characters or fewer.",
        WorktreePathTemplateError::NotAbsolute => "Start the location with ~/ or /.",
        WorktreePathTemplateError::ParentSegment => "Remove \"..\" from the location.",
        WorktreePathTemplateError::UnclosedBrace => "Close each placeholder with }.",
        WorktreePathTemplateError::UnknownPlaceholder => {
            "Use only the {repository} and {branch} placeholders."
        }
        WorktreePathTemplateError::MissingBranch => {
            "Include {branch}, so each Worktree gets its own folder."
        }
        WorktreePathTemplateError::InvalidCharacter => {
            "Remove tabs, line breaks, and other control characters."
        }
        WorktreePathTemplateError::NotWritable => "The server can\u{2019}t write to this folder.",
    }
}

/// A tool row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ToolPresentation {
    /// The installed version, shown before the badge.
    pub(super) version: Option<SharedString>,
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: &'static str,
}

/// The selected server's Git, which runs Repository Status and Worktrees on that server.
pub(super) fn git_presentation(status: GitToolStatus) -> ToolPresentation {
    let (version, state, state_selector, explanation) = match status {
        GitToolStatus::Unknown => (
            None,
            "Not checked",
            "settings-git-tool-state-unknown",
            "SpaceTerm checks Git while this server is connected.",
        ),
        GitToolStatus::Ready(version) => (
            Some(version),
            "Installed",
            "settings-git-tool-state-ready",
            "Repository Status and Worktrees use this Git.",
        ),
        GitToolStatus::TooOld(version) => (
            Some(version),
            "Too old",
            "settings-git-tool-state-too-old",
            "Repository Status needs Git 2.15 or later.",
        ),
        GitToolStatus::NotFound => (
            None,
            "Not found",
            "settings-git-tool-state-not-found",
            "Install Git on this server to use Repository Status.",
        ),
    };
    ToolPresentation {
        version: version.map(version_text),
        state,
        state_selector,
        explanation,
    }
}

/// The GitHub CLI on this computer, which holds the GitHub login Pull Requests use.
pub(super) fn github_cli_presentation(status: GitHubCliStatus) -> ToolPresentation {
    let (state, state_selector, explanation) = match status {
        GitHubCliStatus::Unknown => (
            "Not checked",
            "settings-github-cli-state-unknown",
            "SpaceTerm checks the GitHub CLI while Pull Requests are shown.",
        ),
        GitHubCliStatus::SignedIn => (
            "Signed in",
            "settings-github-cli-state-signed-in",
            "Pull Requests use your GitHub CLI login.",
        ),
        GitHubCliStatus::SignedOut => (
            "Not signed in",
            "settings-github-cli-state-signed-out",
            "Run \"gh auth login\" in a terminal.",
        ),
        GitHubCliStatus::NotFound => (
            "Not found",
            "settings-github-cli-state-not-found",
            "Install the GitHub CLI to show Pull Requests.",
        ),
        GitHubCliStatus::Unavailable => (
            "Unavailable",
            "settings-github-cli-state-unavailable",
            "SpaceTerm asks the GitHub CLI again when a window becomes active.",
        ),
    };
    ToolPresentation {
        version: None,
        state,
        state_selector,
        explanation,
    }
}

fn version_text(version: ToolVersion) -> SharedString {
    format!("{}.{}.{}", version.major, version.minor, version.patch).into()
}

fn github_cli(cx: &App) -> GitHubCliStatus {
    cx.try_global::<RepositoryTools>()
        .copied()
        .unwrap_or_default()
        .github_cli
}

impl SettingsWindow {
    pub(super) fn render_git_preference(
        &mut self,
        row: SettingsRowId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.editor.document().git.repository_status();
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
        let selector = row.descriptor().selector;
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        gpui::div()
            .debug_selector(move || format!("{selector}-control"))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .children(presentation.version.map(|version| {
                gpui::div()
                    .chrome_text(appearance.typography.style(TextRole::Body))
                    .text_color(gpui_color(colors.text_muted))
                    .child(Text::new(
                        SharedString::from(format!("{selector}-version")).into(),
                        version,
                    ))
            }))
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

    /// The Git row follows the selected server. The GitHub CLI row is this computer's.
    pub(super) fn git_tool_presentation(&self, row: SettingsRowId, cx: &App) -> ToolPresentation {
        if row == SettingsRowId::GitHubCli {
            github_cli_presentation(github_cli(cx))
        } else {
            git_presentation(self.server_git())
        }
    }

    pub(super) fn git_preference_differs(&self, row: SettingsRowId) -> Option<bool> {
        let current = &self.editor.document().git;
        let defaults = GitPreferences::default();
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
        let defaults = GitPreferences::default();
        let value = if row == SettingsRowId::ShowPullRequests {
            defaults.show_pull_requests
        } else {
            defaults.show_repository_status
        };
        self.edit(move |draft| set(row, &mut draft.git, value), cx);
    }
}

fn set(row: SettingsRowId, preferences: &mut GitPreferences, value: bool) {
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
    fn the_servers_git_shows_its_version_beside_whether_it_is_usable() {
        let ready = git_presentation(GitToolStatus::Ready(ToolVersion {
            major: 2,
            minor: 47,
            patch: 0,
        }));
        assert_eq!(
            (ready.version.as_deref(), ready.state),
            (Some("2.47.0"), "Installed")
        );
        let old = git_presentation(GitToolStatus::TooOld(ToolVersion {
            major: 2,
            minor: 14,
            patch: 6,
        }));
        assert_eq!(
            (old.version.as_deref(), old.explanation),
            (Some("2.14.6"), "Repository Status needs Git 2.15 or later.")
        );
        let missing = git_presentation(GitToolStatus::NotFound);
        assert_eq!((missing.version, missing.state), (None, "Not found"));
    }

    #[test]
    fn a_signed_out_github_cli_names_the_fix() {
        let presentation = github_cli_presentation(GitHubCliStatus::SignedOut);

        assert_eq!(
            (presentation.state, presentation.explanation),
            ("Not signed in", "Run \"gh auth login\" in a terminal.")
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
