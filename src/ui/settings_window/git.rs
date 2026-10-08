//! The Git section: whether Repository Status and Pull Requests show, the state of the tools
//! that provide them, and where new Worktrees go.

use std::path::Path;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Subscription, Window};
use spaceterm_ui::{FieldState, Switch, TextInput, TextInputEvent, TextInputVariant, ToggleSize};

use super::{SettingsRowId, SettingsWindow};
use crate::repository_status::{GitHubCliStatus, GitToolStatus, ToolVersion};
use crate::settings::git::GitPreferences;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::repository_status_store::RepositoryTools;
use crate::ui::sidebar_window::form::{CaptionTone, badge};
use crate::worktrees::path_template::{WorktreePathTemplateError, expand, validate};

/// The repository and branch the location row's example expands, so the example reads like a
/// real Worktree without naming one of the person's repositories.
const EXAMPLE_REPOSITORY: &str = "my-app";
const EXAMPLE_BRANCH: &str = "feature/login";

/// The field editing the Worktree Path Template. It saves on Return or when focus leaves, and
/// only a valid template; Escape returns it to the saved one.
pub(super) struct WorktreeLocationField {
    pub(super) input: Entity<TextInput>,
    /// Why the field's text cannot be saved, shown while the person edits it.
    pub(super) problem: Option<WorktreePathTemplateError>,
    _events: Subscription,
}

impl WorktreeLocationField {
    pub(super) fn new(
        template: &str,
        window: &mut Window,
        cx: &mut Context<SettingsWindow>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(
                "settings-worktree-location",
                "New Worktree Location",
                template.to_owned(),
                window,
                cx,
            )
            .variant(TextInputVariant::Bare)
            .input_length_limit(Some(1024))
            .debug_selector("settings-worktree-location")
        });
        let events = cx.subscribe_in(
            &input,
            window,
            |settings, input, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    settings.worktree_location.problem = validate(input.read(cx).value()).err();
                    cx.notify();
                }
                TextInputEvent::Submitted | TextInputEvent::FocusLost => {
                    settings.commit_worktree_location(cx);
                }
                TextInputEvent::Cancelled => {
                    let saved = settings
                        .editor
                        .document()
                        .git
                        .worktree_path_template
                        .clone();
                    settings.worktree_location.problem = None;
                    input.update(cx, |input, cx| input.set_value(saved, cx));
                    cx.notify();
                }
                _ => {}
            },
        );
        Self {
            input,
            problem: None,
            _events: events,
        }
    }
}

/// What the person reads under the location field.
pub(super) fn worktree_location_caption(
    text: &str,
    problem: Option<WorktreePathTemplateError>,
) -> (SharedString, CaptionTone) {
    let explanation = match problem {
        None => {
            let example = expand(text, Path::new("~"), EXAMPLE_REPOSITORY, EXAMPLE_BRANCH)
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            return (
                format!("Use {{repository}} and {{branch}}. For example: {example}").into(),
                CaptionTone::Guidance,
            );
        }
        Some(WorktreePathTemplateError::Empty) => "Enter a location.",
        Some(WorktreePathTemplateError::TooLong) => "Use 1,024 characters or fewer.",
        Some(WorktreePathTemplateError::NotAbsolute) => "Start the location with ~/ or /.",
        Some(WorktreePathTemplateError::ParentSegment) => "Remove \"..\" from the location.",
        Some(WorktreePathTemplateError::UnclosedBrace) => "Close each placeholder with }.",
        Some(WorktreePathTemplateError::UnknownPlaceholder) => {
            "Use only the {repository} and {branch} placeholders."
        }
        Some(WorktreePathTemplateError::MissingBranch) => {
            "Include {branch}, so each Worktree gets its own folder."
        }
    };
    (SharedString::new_static(explanation), CaptionTone::Error)
}

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

    pub(super) fn render_worktree_location(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let saved = &self.editor.document().git.worktree_path_template;
        let input = self.worktree_location.input.clone();
        // An import, a reset, or the settings file can change the template while the field rests.
        let resting = !input.read(cx).is_focused() && self.worktree_location.problem.is_none();
        if resting && input.read(cx).value() != saved {
            let saved = saved.clone();
            input.update(cx, |input, cx| input.set_value(saved, cx));
        }
        let editable = self.editor.editable();
        input.update(cx, |input, cx| input.set_editable(editable, cx));
        let invalid = self.worktree_location.problem.is_some();
        let focus = input.read(cx).focus_handle();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Card);
        spaceterm_ui::field_frame(
            "settings-worktree-location-frame",
            &focus,
            FieldState::default().disabled(!editable).invalid(invalid),
            RadiusRole::Control.pixels(),
            cx,
        )
        .debug_selector(move || {
            if invalid {
                "settings-worktree-location-invalid"
            } else {
                "settings-worktree-location-frame"
            }
            .to_owned()
        })
        .h(appearance.height(28.0, 13.0))
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(colors.text))
        .on_click(move |_, window, cx| {
            focus.focus(window, cx);
            cx.stop_propagation();
        })
        .child(input)
        .into_any_element()
    }

    /// Saves the field's template when it is valid and differs from the saved one.
    fn commit_worktree_location(&mut self, cx: &mut Context<Self>) {
        let text = self.worktree_location.input.read(cx).value().to_owned();
        if self.worktree_location.problem.is_some()
            || text == self.editor.document().git.worktree_path_template
        {
            return;
        }
        self.edit(move |draft| draft.git.worktree_path_template = text, cx);
        cx.notify();
    }

    pub(super) fn worktree_location_caption(&self, cx: &App) -> (SharedString, CaptionTone) {
        worktree_location_caption(
            self.worktree_location.input.read(cx).value(),
            self.worktree_location.problem,
        )
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
        let current = &self.editor.document().git;
        let defaults = GitPreferences::default();
        match row {
            SettingsRowId::ShowRepositoryStatus => {
                Some(current.show_repository_status != defaults.show_repository_status)
            }
            SettingsRowId::ShowPullRequests => {
                Some(current.show_pull_requests != defaults.show_pull_requests)
            }
            SettingsRowId::WorktreeLocation => {
                Some(current.worktree_path_template != defaults.worktree_path_template)
            }
            _ => None,
        }
    }

    pub(super) fn reset_git_preference(&mut self, row: SettingsRowId, cx: &mut Context<Self>) {
        let defaults = GitPreferences::default();
        if row == SettingsRowId::WorktreeLocation {
            self.worktree_location.problem = None;
            let template = defaults.worktree_path_template;
            self.worktree_location
                .input
                .update(cx, |input, cx| input.set_value(template.clone(), cx));
            self.edit(move |draft| draft.git.worktree_path_template = template, cx);
            return;
        }
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
    fn the_worktree_location_caption_shows_an_example_or_the_fix() {
        use crate::worktrees::path_template::DEFAULT_WORKTREE_PATH_TEMPLATE;

        assert_eq!(
            worktree_location_caption(DEFAULT_WORKTREE_PATH_TEMPLATE, None),
            (
                SharedString::from(
                    "Use {repository} and {branch}. For example: ~/.worktrees/my-app/feature-login"
                ),
                CaptionTone::Guidance
            )
        );
        assert_eq!(
            worktree_location_caption("~/wt", Some(WorktreePathTemplateError::MissingBranch)),
            (
                SharedString::from("Include {branch}, so each Worktree gets its own folder."),
                CaptionTone::Error
            )
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
