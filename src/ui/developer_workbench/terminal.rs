//! The Terminal section: display fixtures the Workspace windows render in place of host facts.
//! Each fixture replaces only what a Pane shows, never what it does.

use gpui::prelude::*;
use gpui::{AnyElement, App, Global, Window};
use spaceterm_ui::{Switch, ToggleSize};

use super::DeveloperWorkbench;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::repository_status::{
    ChangeEntry, ChangeKind, ChangeState, ChangeSummary, ChangeTotal, Divergence, Freshness,
    PullRequest, RepositoryHead, RepositoryKey, RepositoryMachine, RepositoryRoot,
    RepositoryStatus, RepositoryView, Upstream,
};
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::sidebar_window::form::{FormGroup, FormRow, action_button};
use crate::ui::terminal_pane::{PaneCaptionFacts, PaneOrigin};

#[derive(Default)]
struct TerminalFixtures {
    caption: bool,
    link_preview: bool,
    repository: bool,
}
impl Global for TerminalFixtures {}

fn fixtures(cx: &App) -> Option<&TerminalFixtures> {
    cx.try_global::<TerminalFixtures>()
}

pub(crate) fn set_caption_fixture(enabled: bool, cx: &mut App) {
    cx.default_global::<TerminalFixtures>().caption = enabled;
    cx.refresh_windows();
}

pub(crate) fn set_link_preview_fixture(enabled: bool, cx: &mut App) {
    cx.default_global::<TerminalFixtures>().link_preview = enabled;
    cx.refresh_windows();
}

/// Ends every fixture, so Workspace windows show host facts again.
pub(super) fn reset_fixtures(cx: &mut App) {
    if cx.has_global::<TerminalFixtures>() {
        cx.remove_global::<TerminalFixtures>();
        cx.refresh_windows();
    }
}

/// Display text only; this fixture never supplies a terminal hyperlink target.
pub(crate) fn link_preview_fixture(cx: &App) -> Option<&'static str> {
    fixtures(cx)
        .is_some_and(|fixtures| fixtures.link_preview)
        .then_some("https://example.invalid/workbench-fixture")
}

/// Synthetic display facts in place of the host account and directory.
pub(crate) fn caption_fixture(cx: &App) -> Option<PaneCaptionFacts> {
    fixtures(cx)
        .is_some_and(|fixtures| fixtures.caption)
        .then(|| PaneCaptionFacts {
            origin: PaneOrigin {
                user: "fixture".into(),
                host: "local".into(),
                remote: false,
            },
            directory: "workbench-fixture".into(),
            label: "Workbench fixture".into(),
            glyph: None,
            running: false,
            progress: Default::default(),
            repository: None,
        })
}

pub(crate) fn set_repository_fixture(enabled: bool, cx: &mut App) {
    cx.default_global::<TerminalFixtures>().repository = enabled;
    cx.refresh_windows();
}

/// A synthetic repository with changes, divergence, and a draft Pull Request, in place of every
/// Pane's and sidebar row's Repository Status.
pub(crate) fn repository_view_fixture(cx: &App) -> Option<RepositoryView> {
    let change = |kind, path: &str| ChangeEntry {
        kind,
        path: path.into(),
    };
    fixtures(cx)
        .is_some_and(|fixtures| fixtures.repository)
        .then(|| {
            RepositoryView::Repository(Arc::new(RepositoryStatus {
                key: RepositoryKey {
                    machine: RepositoryMachine::Local,
                    root: RepositoryRoot::Local(PathBuf::from("/workbench/fixture")),
                },
                head: RepositoryHead::Branch("feat/workbench-fixture".into()),
                commit: Some("a1b2c3d".into()),
                upstream: Some(Upstream {
                    name: "origin/feat/workbench-fixture".into(),
                    divergence: Some(Divergence {
                        ahead: 1,
                        behind: 2,
                    }),
                }),
                operation: None,
                changes: ChangeState::Known(ChangeSummary {
                    total: ChangeTotal::Exact(4),
                    staged: 1,
                    modified: 2,
                    untracked: 1,
                    entries: vec![
                        change(ChangeKind::Staged, "src/fixture/caption.rs"),
                        change(ChangeKind::Modified, "src/fixture/popover.rs"),
                        change(ChangeKind::Modified, "src/fixture/sidebar.rs"),
                        change(ChangeKind::Untracked, "notes.txt"),
                    ],
                    ..ChangeSummary::default()
                }),
                freshness: Freshness::Current,
                read_at: Instant::now(),
                pull_request: Some(PullRequest {
                    number: 478,
                    title: "Workbench fixture Pull Request".into(),
                    draft: true,
                    url: "https://example.invalid/pull/478".into(),
                    head: "feat/workbench-fixture".into(),
                    base: "main".into(),
                }),
                read_failure: None,
            }))
        })
}

pub(super) fn render(
    surface: &SettingsAppearance,
    window: &Window,
    cx: &mut Context<DeveloperWorkbench>,
) -> Vec<AnyElement> {
    let appearance = &surface.chrome;
    let caption = fixtures(cx).is_some_and(|fixtures| fixtures.caption);
    let link_preview = fixtures(cx).is_some_and(|fixtures| fixtures.link_preview);
    let repository = fixtures(cx).is_some_and(|fixtures| fixtures.repository);
    let fixtures = vec![
        FormRow::new(
            "workbench-row-terminal-caption",
            "Pane Caption fixture",
            Switch::new(
                "workbench-terminal-caption",
                "Pane Caption fixture",
                caption,
            )
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .debug_selector("workbench-terminal-caption")
            .on_change(|change, _, cx| set_caption_fixture(change.requested(), cx)),
        )
        .description("Captions show a synthetic user, host, and directory.")
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new(
            "workbench-row-terminal-link-preview",
            "Link preview fixture",
            Switch::new(
                "workbench-terminal-link-preview",
                "Link preview fixture",
                link_preview,
            )
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .debug_selector("workbench-terminal-link-preview")
            .on_change(|change, _, cx| set_link_preview_fixture(change.requested(), cx)),
        )
        .description("The focused Pane shows a hyperlink preview with no target.")
        .render(appearance, window, cx)
        .into_any_element(),
        FormRow::new(
            "workbench-row-terminal-repository",
            "Repository Status fixture",
            Switch::new(
                "workbench-terminal-repository",
                "Repository Status fixture",
                repository,
            )
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .debug_selector("workbench-terminal-repository")
            .on_change(|change, _, cx| set_repository_fixture(change.requested(), cx)),
        )
        .description(
            "Every Pane and sidebar row shows a synthetic repository with changes and a draft \
             Pull Request.",
        )
        .render(appearance, window, cx)
        .into_any_element(),
    ];
    let owner = cx.weak_entity();
    let show = action_button(
        "workbench-show-workspace",
        "Show Workspace",
        true,
        move |_, cx| {
            let _ = owner.update(cx, |workbench, cx| workbench.show_workspace(cx));
        },
    );
    let owner = cx.weak_entity();
    let reload_fonts = action_button(
        "workbench-reload-fonts",
        "Reload Fonts",
        true,
        move |_, cx| {
            let _ = owner.update(cx, |workbench, cx| workbench.reload_fonts(cx));
        },
    );
    let actions = vec![
        FormRow::new("workbench-row-terminal-workspace", "Workspace window", show)
            .description("Brings the frontmost Workspace window forward to inspect it.")
            .render(appearance, window, cx)
            .into_any_element(),
        FormRow::new("workbench-row-terminal-fonts", "Fonts", reload_fonts)
            .description("Reads the installed fonts again, as after a font install.")
            .render(appearance, window, cx)
            .into_any_element(),
    ];
    vec![
        FormGroup::new(
            "workbench-group-terminal-fixtures".to_owned(),
            "Fixtures",
            fixtures,
        )
        .render(surface)
        .into_any_element(),
        FormGroup::new(
            "workbench-group-terminal-actions".to_owned(),
            "Actions",
            actions,
        )
        .render(surface)
        .into_any_element(),
    ]
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn caption_and_link_display_fixtures_toggle_independently(cx: &mut TestAppContext) {
        cx.update(|cx| {
            assert!(caption_fixture(cx).is_none());
            assert!(link_preview_fixture(cx).is_none());

            set_caption_fixture(true, cx);
            let facts = caption_fixture(cx).expect("the fixture should supply display facts");
            assert_eq!(facts.origin.user.as_ref(), "fixture");
            assert_eq!(facts.origin.host.as_ref(), "local");
            assert!(!facts.origin.remote);
            assert!(link_preview_fixture(cx).is_none());

            set_link_preview_fixture(true, cx);
            set_caption_fixture(false, cx);
            assert!(caption_fixture(cx).is_none());
            assert_eq!(
                link_preview_fixture(cx),
                Some("https://example.invalid/workbench-fixture")
            );
            reset_fixtures(cx);
            assert!(caption_fixture(cx).is_none());
            assert!(link_preview_fixture(cx).is_none());
        });
    }
}
