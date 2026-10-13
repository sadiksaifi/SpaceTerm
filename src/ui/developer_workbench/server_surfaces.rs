//! The Server Surfaces section: what a Workspace window shows for server sessions, in every state,
//! over synthetic names and Terminal content. Disclosure works; every other action is inert.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div, px};
use spaceterm_ui::{ControlHost, ControlThemeScope};

use super::DeveloperWorkbench;
use crate::appearance::Color;
use crate::ui::appearance::settings::SettingsAppearance;
use crate::ui::appearance::{ChromeAppearance, gpui_color, shared_chrome};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::empty_workspace_window::EmptyWorkspaceContent;
use crate::ui::pane_presentation::{
    DisplayName, OfflinePrior, PaneFailureClass, PanePresentation, PanePresentationLayer,
    SessionEndReason,
};
use crate::ui::sidebar_window::form::{FormGroup, FormRow, FormRowLayout};
use crate::ui::workspace_sidebar::server_group::{
    ServerConnection, ServerSection, ServerSectionId, ServerSectionModel, UpdateRequired,
};

const PANE_WIDTH: f32 = 380.0;
const PANE_HEIGHT: f32 = 230.0;
const SIDEBAR_WIDTH: f32 = 280.0;
const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 320.0;
const GALLERY_GAP: f32 = 16.0;

/// Synthetic Terminal output under each Pane presentation, so a frozen screen has a screen to
/// freeze.
const TERMINAL_LINES: [&str; 6] = [
    "$ cargo test --workspace",
    "   Compiling spaceterm v0.1.0",
    "    Finished test profile in 4.2s",
    "running 128 tests",
    "test result: ok. 128 passed; 0 failed",
    "$ ",
];

fn pane_fixtures() -> [(&'static str, PanePresentation); 10] {
    let device = || DisplayName::reported("MacBook Air");
    [
        ("Starting", PanePresentation::Starting),
        ("Takeover", PanePresentation::Takeover(device())),
        (
            "Ended by End All Sessions",
            PanePresentation::Ended {
                reason: SessionEndReason::EndAllSessions,
            },
        ),
        (
            "Ended by a server restart",
            PanePresentation::Ended {
                reason: SessionEndReason::Restored,
            },
        ),
        (
            "Failed to launch",
            PanePresentation::Failed {
                class: PaneFailureClass::Launch,
            },
        ),
        (
            "Directory unavailable",
            PanePresentation::DirectoryUnavailable,
        ),
        (
            "Offline",
            PanePresentation::Offline {
                prior: OfflinePrior::Frozen,
                cause: super::super::pane_presentation::OfflineCause::Unreachable,
            },
        ),
        (
            "Offline over a Takeover Screen",
            PanePresentation::Offline {
                prior: OfflinePrior::Takeover(device()),
                cause: super::super::pane_presentation::OfflineCause::Unreachable,
            },
        ),
        (
            "Reconciling",
            PanePresentation::Reconciling {
                prior: OfflinePrior::Frozen,
            },
        ),
        (
            "Takeover from an unnamed device",
            PanePresentation::Takeover(DisplayName::unnamed()),
        ),
    ]
}

/// The server sections each sidebar fixture lists, with the Workspace rows it attached.
fn section_fixtures() -> [(ServerSectionModel, &'static [&'static str]); 7] {
    let section = |id, name, connection, expanded| ServerSectionModel {
        id: ServerSectionId::new(id),
        name: DisplayName::reported(name),
        connection,
        expanded,
    };
    [
        (
            section(1, "Studio", ServerConnection::Connected, true),
            &["spaceterm", "dotfiles"][..],
        ),
        (
            section(2, "Build Box", ServerConnection::Connecting, false),
            &["ci"][..],
        ),
        (
            section(3, "New Server", ServerConnection::Connected, true),
            &[][..],
        ),
        (
            section(4, "MacBook Air", ServerConnection::Offline, true),
            &[][..],
        ),
        (
            section(
                5,
                "Lab Server",
                ServerConnection::Incompatible(UpdateRequired::Server),
                true,
            ),
            &[][..],
        ),
        (
            section(
                6,
                "Mac mini",
                ServerConnection::Incompatible(UpdateRequired::ThisComputer),
                false,
            ),
            &[][..],
        ),
        (
            section(7, "Home Server", ServerConnection::Stopped, false),
            &["media"][..],
        ),
    ]
}

pub(super) fn render(
    surface: &SettingsAppearance,
    window: &mut Window,
    cx: &mut Context<DeveloperWorkbench>,
) -> Vec<AnyElement> {
    let appearance = &surface.chrome;
    // The fixtures show what a Workspace window paints, so they read the application's appearance
    // rather than the Workbench's own.
    let workspace = ControlThemeScope::Application.with_scope(|| shared_chrome(cx));
    let gallery = || div().flex().flex_row().flex_wrap().gap(px(GALLERY_GAP));
    let resolved = crate::ui::appearance_runtime::current(cx);
    let terminal = &resolved.terminal;
    let terminal_paint = TerminalPaint {
        background: terminal.colors.background,
        foreground: terminal.colors.foreground,
        family: terminal.typography.regular.primary_family.clone().into(),
    };
    let panes =
        gallery()
            .debug_selector(|| "workbench-server-panes".to_owned())
            .children(pane_fixtures().into_iter().enumerate().map(
                |(index, (label, presentation))| {
                    fixture(
                        label,
                        pane(index, presentation, &terminal_paint),
                        appearance,
                    )
                },
            ));
    let expansions = window.use_keyed_state("workbench-server-section-expansion", cx, |_, _| {
        section_fixtures().map(|(model, _)| model.expanded)
    });
    let sidebars = gallery()
        .debug_selector(|| "workbench-server-sections".to_owned())
        .child(fixture(
            "Sidebar without keyboard focus",
            sidebar(false, &expansions, &workspace, cx),
            appearance,
        ))
        .child(fixture(
            "Sidebar with keyboard focus",
            sidebar(true, &expansions, &workspace, cx),
            appearance,
        ));
    let windows = gallery()
        .debug_selector(|| "workbench-empty-workspace-windows".to_owned())
        .child(fixture(
            "No server has a Workspace to attach",
            empty_window(false, &workspace),
            appearance,
        ))
        .child(fixture(
            "A server has a Workspace to attach",
            empty_window(true, &workspace),
            appearance,
        ));
    let group = |selector: &'static str, row: &'static str, title, content: gpui::Div| {
        FormGroup::new(
            selector.to_owned(),
            title,
            vec![
                FormRow::new(row, title, content)
                    .layout(FormRowLayout::Full)
                    .render(appearance, window, cx)
                    .into_any_element(),
            ],
        )
        .render(surface)
        .into_any_element()
    };
    vec![
        group(
            "workbench-group-server-panes",
            "workbench-row-server-panes",
            "Pane presentations",
            panes,
        ),
        group(
            "workbench-group-server-sections",
            "workbench-row-server-sections",
            "Server sections",
            sidebars,
        ),
        group(
            "workbench-group-empty-workspace-window",
            "workbench-row-empty-workspace-window",
            "Empty Workspace Window",
            windows,
        ),
    ]
}

/// A captioned fixture in a gallery.
fn fixture(label: &'static str, content: AnyElement, appearance: &ChromeAppearance) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(appearance.spacing(6.0))
        .child(
            div()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(appearance.colors.text_secondary))
                .child(label),
        )
        .child(content)
        .into_any_element()
}

struct TerminalPaint {
    background: Color,
    foreground: Color,
    family: SharedString,
}

/// A Pane-sized Terminal with synthetic output and one presentation over it.
fn pane(index: usize, presentation: PanePresentation, terminal: &TerminalPaint) -> AnyElement {
    let selector = format!("workbench-server-pane-{index}");
    div()
        .debug_selector(move || selector.clone())
        .relative()
        .w(px(PANE_WIDTH))
        .h(px(PANE_HEIGHT))
        .flex_none()
        .overflow_hidden()
        .rounded(px(RadiusRole::Card.points()))
        .bg(gpui_color(terminal.background))
        .child(
            div()
                .p(px(10.0))
                .flex()
                .flex_col()
                .font_family(terminal.family.clone())
                .text_size(px(12.0))
                .line_height(px(17.0))
                .text_color(gpui_color(terminal.foreground))
                .children(TERMINAL_LINES),
        )
        .child(
            ControlThemeScope::Application.mount(PanePresentationLayer::new(
                ("workbench-server-pane", index),
                presentation,
                terminal.background,
            )),
        )
        .into_any_element()
}

/// A sidebar-wide list of every server section fixture. Its headers disclose their sections.
fn sidebar(
    emphasized: bool,
    expansions: &Entity<[bool; 7]>,
    workspace: &ChromeAppearance,
    cx: &App,
) -> AnyElement {
    let expanded = *expansions.read(cx);
    let sections =
        section_fixtures()
            .into_iter()
            .enumerate()
            .map(|(index, (mut model, workspaces))| {
                model.expanded = expanded[index];
                let expansions = expansions.clone();
                ServerSection::new(model)
                    .selected(index == 0)
                    .emphasized(emphasized)
                    .set_position(index + 1, expanded.len())
                    .workspaces(
                        workspaces
                            .iter()
                            .map(|name| workspace_row(name, workspace))
                            .collect(),
                    )
                    .on_toggle(move |expanded, _, cx| {
                        expansions.update(cx, |expansions, cx| {
                            expansions[index] = expanded;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            });
    ControlThemeScope::Application
        .mount(
            ControlHost::Panel.mount(
                div()
                    .w(px(SIDEBAR_WIDTH))
                    .py(workspace.spacing(6.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .rounded(px(RadiusRole::Card.points()))
                    .bg(gpui_color(crate::ui::workspace_frame::base_surface(
                        &workspace.colors,
                    )))
                    .children(sections),
            ),
        )
        .into_any_element()
}

/// A stand-in for a Workspace row under a server's header.
fn workspace_row(name: &'static str, workspace: &ChromeAppearance) -> AnyElement {
    let colors = workspace.host_colors(ControlHost::Panel);
    div()
        .h(workspace.spacing(32.0))
        .flex()
        .items_center()
        .pl(workspace.spacing(44.0))
        .chrome_text(workspace.typography.style(TextRole::Navigation))
        .text_color(gpui_color(colors.row_foreground))
        .child(name)
        .into_any_element()
}

/// A Workspace-window-sized surface showing the empty Workspace Window's content.
fn empty_window(attachable: bool, workspace: &ChromeAppearance) -> AnyElement {
    ControlThemeScope::Application
        .mount(
            div()
                .w(px(WINDOW_WIDTH))
                .h(px(WINDOW_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .p(workspace.spacing(24.0))
                .rounded(px(RadiusRole::Card.points()))
                .bg(gpui_color(workspace.colors.background))
                .child(EmptyWorkspaceContent::new(
                    ("workbench-empty-workspace", usize::from(attachable)),
                    attachable,
                )),
        )
        .into_any_element()
}
