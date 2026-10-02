//! The Developer Workbench: one window for inspecting SpaceTerm's appearance, controls, and
//! floating surfaces against the production Interfaces.
//!
//! Only SpaceTerm Development contains it. It uses the sidebar window layout the Settings Window
//! uses: each section presents one family of fixtures, and the toolbar holds the controls that
//! apply to every section, such as the previewed Appearance Mode and simulated system settings.
//!
//! Edits preview the Settings Document without saving it. Commit saves the preview through the
//! same path the Settings Window uses, and closing the window cancels an unsaved preview.

#[cfg(test)]
#[path = "developer_workbench/tests.rs"]
mod tests;

mod appearance;
mod controls;
mod document;
mod modals;
mod preview;
mod surfaces;
mod terminal;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, FocusHandle, Global, ScrollHandle, SharedString, Window, WindowHandle,
    actions, div, px, size,
};
use spaceterm_ui::{
    CommandPalette, CommandPaletteItem, ControlWindowActivity, Icon, IconName, Menu, MenuEntry,
    ModalLayer, OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics, SegmentedControl,
    SegmentedOption,
};

use crate::appearance::AppearanceMode;
use crate::platform::window_movement::{OperatingSystemWindowDragPlatform, WindowMovementFactory};
use crate::ui::appearance::settings::{SettingsAppearance, SettingsSurfaceRole};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::appearance_runtime::{self, AccessibilityPreviewFact, AppearanceRuntime};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::form::{action_button, section_heading};
use crate::ui::sidebar_window::{
    DetailHeading, NavigationEntry, Sidebar, SidebarNavigation, SidebarOwner, WindowMovement,
    card_gutter, group_spacing,
};

use controls::ControlStates;
use document::DocumentEditor;
use modals::ModalFixtures;
use preview::{AppearancePreview, PreviewError};
use surfaces::FloatingSurfaces;

#[cfg(test)]
pub(crate) use terminal::set_link_preview_fixture;
pub(crate) use terminal::{caption_fixture, link_preview_fixture};

actions!(
    spaceterm,
    [
        OpenDeveloperWorkbench,
        ToggleAppearancePreview,
        CloseDeveloperWorkbench
    ]
);

/// The key context the Developer Workbench publishes, so its shortcuts override Workspace ones.
pub(crate) const WORKBENCH_KEY_CONTEXT: &str = "DeveloperWorkbench";

/// Names the section to open at launch. `mise run development:workbench [section]` sets it.
const LAUNCH_VARIABLE: &str = "SPACETERM_DEVELOPER_WORKBENCH";

const WINDOW_TITLE: &str = "Developer Workbench";
const WINDOW_WIDTH: f32 = 1120.0;
const WINDOW_HEIGHT: f32 = 760.0;
/// Names the window in every selector, such as `workbench-sidebar`.
const PREFIX: &str = "workbench";

/// The command palette's fixture results.
const PALETTE_OPEN_WORKSPACE: u8 = 1;
const PALETTE_OPEN_REMOTE_WORKSPACE: u8 = 2;
const PALETTE_UNAVAILABLE: u8 = 3;

/// One family of fixtures the Developer Workbench presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkbenchSection {
    Appearance,
    Controls,
    FloatingSurfaces,
    Modals,
    Terminal,
    Document,
}

impl WorkbenchSection {
    pub(crate) const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Controls,
        Self::FloatingSurfaces,
        Self::Modals,
        Self::Terminal,
        Self::Document,
    ];

    /// The name `mise run development:workbench` accepts for this section.
    pub(crate) const fn argument(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Controls => "controls",
            Self::FloatingSurfaces => "floating-surfaces",
            Self::Modals => "modals",
            Self::Terminal => "terminal",
            Self::Document => "document",
        }
    }

    pub(crate) fn from_argument(argument: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|section| section.argument() == argument)
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Controls => "Controls",
            Self::FloatingSurfaces => "Floating Surfaces",
            Self::Modals => "Modals",
            Self::Terminal => "Terminal",
            Self::Document => "Settings Document",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Appearance => "Preview window and terminal preferences, resets, and diagnostics.",
            Self::Controls => "Every control family pinned in each interaction state.",
            Self::FloatingSurfaces => {
                "Menus, pickers, the command palette, and tooltips over detail."
            }
            Self::Modals => "Each modal family over controls it must obscure.",
            Self::Terminal => "Display fixtures for Workspace windows.",
            Self::Document => "The whole Settings Document as JSON, previewed on request.",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Appearance => "workbench-section-appearance",
            Self::Controls => "workbench-section-controls",
            Self::FloatingSurfaces => "workbench-section-floating-surfaces",
            Self::Modals => "workbench-section-modals",
            Self::Terminal => "workbench-section-terminal",
            Self::Document => "workbench-section-document",
        }
    }

    const fn icon(self) -> IconName {
        match self {
            Self::Appearance => IconName::Palette,
            Self::Controls => IconName::Columns2,
            Self::FloatingSurfaces => IconName::Layers,
            Self::Modals => IconName::AppWindow,
            Self::Terminal => IconName::Terminal,
            Self::Document => IconName::FileCode,
        }
    }
}

/// A simulated system setting the toolbar's Simulate menu toggles. System Settings ends every
/// simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Simulation {
    InactiveWindow,
    Accessibility(AccessibilityPreviewFact),
    SystemSettings,
}

const ACCESSIBILITY_SIMULATIONS: [(AccessibilityPreviewFact, &str); 5] = [
    (
        AccessibilityPreviewFact::ReduceTransparency,
        "Reduce Transparency",
    ),
    (
        AccessibilityPreviewFact::IncreaseContrast,
        "Increase Contrast",
    ),
    (AccessibilityPreviewFact::ShowBorders, "Show Borders"),
    (AccessibilityPreviewFact::ReduceMotion, "Reduce Motion"),
    (
        AccessibilityPreviewFact::DifferentiateWithoutColor,
        "Differentiate Without Color",
    ),
];

/// The one Developer Workbench, so a second request activates the existing window.
struct OpenWorkbench(WindowHandle<DeveloperWorkbench>);
impl Global for OpenWorkbench {}

/// Host-owned window movement for the Workbench's client chrome.
struct WorkbenchComposition {
    window_movement: Rc<dyn WindowMovementFactory>,
}
impl Global for WorkbenchComposition {}

pub(crate) fn configure_window_chrome(
    window_movement: Rc<dyn WindowMovementFactory>,
    cx: &mut App,
) {
    cx.set_global(WorkbenchComposition { window_movement });
}

/// Registers the application-scoped Developer Workbench actions.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &OpenDeveloperWorkbench, cx| open_or_activate(None, cx));
    cx.on_action(|_: &ToggleAppearancePreview, cx| toggle_appearance_preview(cx));
}

/// Opens the Workbench at launch when [`LAUNCH_VARIABLE`] is set. An empty value opens the first
/// section.
pub(crate) fn open_at_launch(cx: &mut App) {
    let Ok(argument) = std::env::var(LAUNCH_VARIABLE) else {
        return;
    };
    if argument.is_empty() {
        open_or_activate(None, cx);
        return;
    }
    match WorkbenchSection::from_argument(&argument) {
        Some(section) => open_or_activate(Some(section), cx),
        None => eprintln!("{LAUNCH_VARIABLE} names no Developer Workbench section"),
    }
}

/// The open Workbench window. Its root may be leased while it dispatches an action, so the window
/// list rather than a read decides whether it is still open.
fn open_workbench(cx: &App) -> Option<WindowHandle<DeveloperWorkbench>> {
    let handle = cx.try_global::<OpenWorkbench>()?.0;
    cx.windows()
        .iter()
        .any(|window| window.window_id() == handle.window_id())
        .then_some(handle)
}

/// Opens the Workbench, or activates it when it is already open. A section, when given, becomes
/// the active one.
pub(crate) fn open_or_activate(section: Option<WorkbenchSection>, cx: &mut App) {
    if !cx.has_global::<AppearanceRuntime>() {
        eprintln!("The Developer Workbench is unavailable because appearance is not installed");
        return;
    }
    if let Some(existing) = open_workbench(cx) {
        cx.defer(move |cx| {
            let _ = existing.update(cx, |workbench, window, cx| {
                if let Some(section) = section {
                    workbench.select_section(section, cx);
                }
                window.activate_window();
            });
        });
        return;
    }
    let Some(composition) = cx.try_global::<WorkbenchComposition>() else {
        eprintln!("The Developer Workbench is unavailable because window chrome is not installed");
        return;
    };
    let window_drag = composition.window_movement.create();
    let opened = cx.open_window(
        super::sidebar_window::window_options(
            WINDOW_TITLE,
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            cx,
        ),
        |window, cx| {
            cx.new(|cx| {
                let mut workbench =
                    DeveloperWorkbench::new_with_platform(Rc::clone(&window_drag), window, cx);
                if let Some(section) = section {
                    workbench.active_section = section;
                }
                workbench
            })
        },
    );
    match opened {
        Ok(handle) => {
            cx.set_global(OpenWorkbench(handle));
            cx.activate(true);
        }
        Err(_) => eprintln!("failed to open the Developer Workbench window"),
    }
}

/// Toggles the previewed Appearance Mode without activating the Workbench, so the change can be
/// watched in the window that has focus. Opens the Workbench first when it is closed.
fn toggle_appearance_preview(cx: &mut App) {
    if open_workbench(cx).is_none() {
        open_or_activate(None, cx);
    }
    let Some(workbench) = open_workbench(cx) else {
        return;
    };
    cx.defer(move |cx| {
        let _ = workbench.update(cx, |workbench, _, cx| workbench.toggle_mode(cx));
    });
}

pub(crate) struct DeveloperWorkbench {
    window_appearance: appearance_runtime::WindowAppearanceOwner,
    window_traffic_lights: appearance_runtime::WindowTrafficLightOwner,
    focus_handle: FocusHandle,
    navigation: SidebarNavigation,
    window_movement: WindowMovement,
    active_section: WorkbenchSection,
    scroll: ScrollHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    preview: AppearancePreview,
    status: SharedString,
    /// Renders this window as though another window were key, without leaving it.
    simulate_inactive: bool,
    palette: Entity<CommandPalette<u8>>,
    controls: ControlStates,
    surfaces: FloatingSurfaces,
    modals: ModalFixtures,
    document: DocumentEditor,
}

impl SidebarOwner for DeveloperWorkbench {
    type Section = WorkbenchSection;

    fn navigation(&mut self) -> &mut SidebarNavigation {
        &mut self.navigation
    }

    fn active_section(&self) -> WorkbenchSection {
        self.active_section
    }

    fn navigable_sections(&self) -> Vec<WorkbenchSection> {
        WorkbenchSection::ALL.to_vec()
    }

    fn select_section(&mut self, section: WorkbenchSection, cx: &mut Context<Self>) {
        if self.active_section != section {
            self.active_section = section;
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        }
        cx.notify();
    }
}

impl DeveloperWorkbench {
    #[cfg(test)]
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;

        Self::new_with_platform(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            window,
            cx,
        )
    }

    fn new_with_platform(
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Transparent native chrome hides this visually while retaining a stable Operating-System
        // window identity for the Window menu, accessibility clients, and development automation.
        window.set_window_title(WINDOW_TITLE);
        let mut window_appearance = appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        let mut window_traffic_lights =
            appearance_runtime::WindowTrafficLightOwner::sidebar_window();
        window_traffic_lights.apply(window, cx);
        cx.observe_global_in::<appearance_runtime::InstalledAppearance>(
            window,
            |workbench, window, cx| {
                workbench.window_appearance.apply(window, cx);
                workbench.window_traffic_lights.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        // Accessibility simulation and the terminal fixtures are application-wide, so they end
        // with the window that offers them. The preview ends with it too, when it is dropped.
        cx.on_release(|_, cx| {
            let _ = appearance_runtime::reset_accessibility_preview(cx);
            terminal::reset_fixtures(cx);
        })
        .detach();
        let settings = cx.global::<AppearanceRuntime>().settings.clone();
        let preview = AppearancePreview::new(
            settings,
            crate::host_fonts::HostFonts::get(cx).system_monospace_family,
        );
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let navigation = SidebarNavigation::new(focus_handle.clone(), window, cx);
        let window_movement = WindowMovement::new(
            operating_system_window_drag_platform,
            focus_handle.clone(),
            WINDOW_TITLE,
        );
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("workbench-scrollbar"));
        cx.subscribe(
            &scrollbar,
            |workbench, _, event: &OverlayScrollbarEvent<f32>, cx| {
                if let OverlayScrollbarEvent::OffsetRequested(offset) = event {
                    let current = workbench.scroll.offset();
                    workbench
                        .scroll
                        .set_offset(gpui::point(current.x, px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();
        let palette = cx
            .new(|cx| CommandPalette::new("Search fixture commands", palette_items(), window, cx));
        let initial_document = preview.export().unwrap_or_else(|| String::from("{}"));
        Self {
            window_appearance,
            window_traffic_lights,
            focus_handle,
            navigation,
            window_movement,
            active_section: WorkbenchSection::Appearance,
            scroll: ScrollHandle::new(),
            scrollbar,
            preview,
            status: SharedString::from("No preview. Edits preview without saving."),
            simulate_inactive: false,
            palette,
            controls: ControlStates::new(window, cx),
            surfaces: FloatingSurfaces::new(window, cx),
            modals: ModalFixtures::new(window, cx),
            document: DocumentEditor::new(initial_document, window, cx),
        }
    }

    fn close(&mut self, _: &CloseDeveloperWorkbench, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn report(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    /// Applies one preview edit and reports its outcome.
    fn apply(
        &mut self,
        edit: impl FnOnce(&mut AppearancePreview) -> Result<(), PreviewError>,
        applied: &'static str,
        cx: &mut Context<Self>,
    ) {
        let status = match edit(&mut self.preview) {
            Ok(()) => format!("{applied}. The preview is not saved."),
            Err(error) => error.message().to_owned(),
        };
        self.report(status, cx);
    }

    fn set_mode(&mut self, mode: AppearanceMode, cx: &mut Context<Self>) {
        self.apply(
            |preview| preview.set_mode(mode),
            "Appearance Mode changed",
            cx,
        );
    }

    fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        let status = match self.preview.toggle_mode() {
            Ok(mode) => format!(
                "Appearance Mode is {} in the preview.",
                appearance_mode_label(mode)
            ),
            Err(error) => error.message().to_owned(),
        };
        self.report(status, cx);
    }

    fn simulate(&mut self, simulation: Simulation, cx: &mut Context<Self>) {
        let status = match simulation {
            Simulation::InactiveWindow => {
                self.simulate_inactive = !self.simulate_inactive;
                if self.simulate_inactive {
                    "This window renders as inactive."
                } else {
                    "This window renders its real activity."
                }
                .to_owned()
            }
            Simulation::Accessibility(fact) => {
                let label = ACCESSIBILITY_SIMULATIONS
                    .iter()
                    .find_map(|(candidate, label)| (*candidate == fact).then_some(*label))
                    .unwrap_or_default();
                let enabled = !accessibility_enabled(fact, cx);
                if appearance_runtime::set_accessibility_preview(fact, enabled, cx).is_ok() {
                    format!(
                        "{label} is simulated {}. System Settings are unchanged.",
                        if enabled { "on" } else { "off" }
                    )
                } else {
                    format!("{label} could not be simulated.")
                }
            }
            Simulation::SystemSettings => {
                self.simulate_inactive = false;
                if appearance_runtime::reset_accessibility_preview(cx).is_ok() {
                    "Simulations are off. Accessibility follows System Settings.".to_owned()
                } else {
                    "Accessibility could not return to System Settings.".to_owned()
                }
            }
        };
        self.report(status, cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let status = match self.preview.cancel() {
            Ok(()) => "Preview cancelled.",
            Err(error) => error.message(),
        };
        self.report(status, cx);
    }

    /// Saves the preview. The document is small, so the write runs synchronously, as the Settings
    /// Window's close does: no save is still running when the window closes or the application
    /// quits.
    fn commit(&mut self, cx: &mut Context<Self>) {
        let status = match self.preview.commit() {
            Ok(outcome) if outcome.reload_required => {
                "Saved. Reload the settings file before the next save."
            }
            Ok(_) => "Saved.",
            Err(error) => error.message(),
        };
        self.report(status, cx);
    }

    fn show_workspace(&mut self, cx: &mut Context<Self>) {
        let workspace = cx
            .window_stack()
            .unwrap_or_else(|| cx.windows())
            .into_iter()
            .find_map(|window| window.downcast::<super::WorkspaceManager>());
        let shown = workspace.is_some_and(|workspace| {
            workspace
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        });
        self.report(
            if shown {
                "Workspace window brought forward."
            } else {
                "No Workspace window is open."
            },
            cx,
        );
    }

    fn reload_fonts(&mut self, cx: &mut Context<Self>) {
        let status = if appearance_runtime::reload_fonts(cx).is_ok() {
            "Installed fonts read again."
        } else {
            "The installed fonts could not be read."
        };
        self.report(status, cx);
    }

    fn preview_document(&mut self, cx: &mut Context<Self>) {
        let text = self.document.text(cx);
        self.apply(
            |preview| preview.apply_document(&text),
            "Settings Document previewed",
            cx,
        );
    }

    fn install_theme_family(&mut self, cx: &mut Context<Self>) {
        let text = self.document.text(cx);
        let status = match self.preview.install_theme_family(&text) {
            Ok(installed) => {
                format!("Installed {installed} themes into the preview. No theme was selected.")
            }
            Err(error) => error.message().to_owned(),
        };
        self.report(status, cx);
    }

    fn show_current_document(&mut self, cx: &mut Context<Self>) {
        match self.preview.export() {
            Some(text) => {
                self.document.replace(text, cx);
                self.report("Showing the current Settings Document.", cx);
            }
            None => self.report("The Settings Document could not be exported.", cx),
        }
    }

    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        match self.preview.reload() {
            Ok(()) => {
                self.show_current_document(cx);
                self.report("Settings file read again.", cx);
            }
            Err(error) => self.report(error.message(), cx),
        }
    }

    /// What the installed appearance resolved to, one fact per line.
    fn diagnostics(&self, cx: &App) -> Vec<String> {
        let current = appearance_runtime::current(cx);
        let settings = cx.global::<AppearanceRuntime>().settings.snapshot();
        vec![
            format!("Appearance generation: {}", current.generation.get()),
            format!("Settings revision: {}", settings.committed.revision),
            format!("Settings phase: {:?}", settings.phase),
            format!("Storage status: {:?}", settings.status),
            format!(
                "Failed save held for recovery: {}",
                if settings.recoverable_candidate.is_some() {
                    "yes"
                } else {
                    "no"
                }
            ),
            format!("Chrome appearance: {:?}", current.chrome.appearance),
            format!(
                "Terminal theme: requested {}, effective {}",
                current.terminal.requested_theme, current.terminal.effective_theme
            ),
            format!("Appearance diagnostics: {:?}", current.diagnostics),
        ]
    }
}

/// Lays out a single-line field frame as product fields are: Body text centered in the field's
/// height.
fn single_line_field<E: Styled>(frame: E, appearance: &ChromeAppearance) -> E {
    frame
        .h(appearance.height(28.0, 13.0))
        .flex()
        .items_center()
        .px(appearance.spacing(8.0))
        .chrome_text(appearance.typography.style(TextRole::Body))
}

fn appearance_mode_label(mode: AppearanceMode) -> &'static str {
    match mode {
        AppearanceMode::Light => "Light",
        AppearanceMode::Dark => "Dark",
        AppearanceMode::Auto => "Auto",
    }
}

fn accessibility_enabled(fact: AccessibilityPreviewFact, cx: &App) -> bool {
    let capabilities = appearance_runtime::current(cx)
        .chrome
        .composition
        .capabilities;
    match fact {
        AccessibilityPreviewFact::ReduceTransparency => capabilities.reduce_transparency,
        AccessibilityPreviewFact::IncreaseContrast => capabilities.increase_contrast,
        AccessibilityPreviewFact::ShowBorders => capabilities.show_borders,
        AccessibilityPreviewFact::ReduceMotion => capabilities.reduce_motion,
        AccessibilityPreviewFact::DifferentiateWithoutColor => {
            capabilities.differentiate_without_color
        }
    }
}

/// Results with icons, secondary text, and a disabled row, so every row state has a fixture.
fn palette_items() -> Vec<CommandPaletteItem<u8>> {
    let icon = |name: IconName| move |color, size| Icon::new(name, size, color).into_any_element();
    vec![
        CommandPaletteItem::new(PALETTE_OPEN_WORKSPACE, "Open Workspace")
            .description("Selected rows keep their secondary text legible")
            .leading_icon(icon(IconName::Folder)),
        CommandPaletteItem::new(PALETTE_OPEN_REMOTE_WORKSPACE, "Open Remote Workspace")
            .description("Hover this row to compare it with the selection")
            .leading_icon(icon(IconName::Globe)),
        CommandPaletteItem::new(PALETTE_UNAVAILABLE, "Unavailable Workspace").disabled(true),
    ]
}

impl DeveloperWorkbench {
    fn scroll_metrics(&self) -> Option<ScrollMetrics<f32>> {
        ScrollMetrics::for_pixels(
            0.0,
            f32::from(self.scroll.bounds().size.height),
            f32::from(self.scroll.max_offset().y),
            -f32::from(self.scroll.offset().y),
        )
    }

    fn sync_scrollbar(&self, cx: &mut App) {
        let metrics = self.scroll_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn reveal_scrollbar(&self, cx: &mut App) {
        let metrics = self.scroll_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }
}

impl Render for DeveloperWorkbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = if self.simulate_inactive {
            ControlWindowActivity::Inactive
        } else {
            super::appearance::window_activity(window)
        };
        super::sidebar_window::render_scoped(activity, || self.render_chrome(window, cx))
    }
}

impl DeveloperWorkbench {
    fn render_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let surface = crate::ui::appearance::settings::shared(cx);
        let appearance = &surface.chrome;
        self.sync_scrollbar(cx);
        let root = div()
            .debug_selector(|| "workbench-window-surface".to_owned())
            .key_context(WORKBENCH_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close));
        let content = super::sidebar_window::tab_traversal(root, cx)
            .size_full()
            .flex()
            .flex_row()
            .text_color(gpui_color(appearance.colors.text))
            .chrome_text(appearance.typography.style(TextRole::Body))
            .child(self.render_sidebar(&surface, window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_detail(&surface, window, cx))
                    .child(self.render_footer(&surface, cx)),
            );
        ModalLayer::new(content)
            .transient(self.palette.clone())
            .into_any_element()
    }

    fn render_sidebar(
        &mut self,
        surface: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entries = WorkbenchSection::ALL
            .into_iter()
            .map(|section| NavigationEntry {
                section,
                title: section.title(),
                selector: section.selector(),
                icon: section.icon(),
                available: true,
            })
            .collect();
        let movement = self.window_movement.clone();
        Sidebar::new(PREFIX, entries, &movement).render(self, surface, window, cx)
    }

    /// Window-wide controls: the previewed Appearance Mode and simulated system settings.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.preview.document().preferences.mode;
        let owner = cx.weak_entity();
        let modes = SegmentedControl::new(
            "workbench-appearance-mode",
            "Appearance Mode",
            &mode,
            [
                AppearanceMode::Light,
                AppearanceMode::Dark,
                AppearanceMode::Auto,
            ]
            .into_iter()
            .map(|mode| {
                let label = appearance_mode_label(mode);
                SegmentedOption::new(mode, label).debug_selector(format!(
                    "workbench-appearance-mode-{}",
                    label.to_ascii_lowercase()
                ))
            })
            .collect(),
        )
        .expect("three appearance modes are within the bounded option set")
        .debug_selector("workbench-appearance-mode")
        .on_change(move |change, _, cx| {
            let mode = *change.requested();
            let _ = owner.update(cx, |workbench, cx| workbench.set_mode(mode, cx));
        });
        let accessibility = ACCESSIBILITY_SIMULATIONS
            .into_iter()
            .map(|(fact, label)| {
                MenuEntry::checkbox(
                    label,
                    accessibility_enabled(fact, cx),
                    Simulation::Accessibility(fact),
                )
            })
            .collect();
        let owner = cx.weak_entity();
        let simulate = Menu::new(
            "workbench-simulate",
            "Simulate",
            vec![
                MenuEntry::checkbox(
                    "Inactive Window",
                    self.simulate_inactive,
                    Simulation::InactiveWindow,
                ),
                MenuEntry::section("Accessibility", accessibility),
                MenuEntry::separator(),
                MenuEntry::action("Use System Settings", Simulation::SystemSettings),
            ],
        )
        .debug_selector("workbench-simulate")
        .on_activate(move |activation, _, cx| {
            let simulation = *activation.action();
            let _ = owner.update(cx, |workbench, cx| workbench.simulate(simulation, cx));
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(modes)
            .child(simulate)
            .into_any_element()
    }

    fn render_detail(
        &mut self,
        surface: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let appearance = &surface.chrome;
        let section = self.active_section;
        let groups = match section {
            WorkbenchSection::Appearance => appearance::render(self, surface, window, cx),
            WorkbenchSection::Controls => self.controls.render(&self.palette, surface, window, cx),
            WorkbenchSection::FloatingSurfaces => {
                self.surfaces.render(&self.palette, surface, window, cx)
            }
            WorkbenchSection::Modals => self.modals.render(surface, window, cx),
            WorkbenchSection::Terminal => terminal::render(surface, window, cx),
            WorkbenchSection::Document => self.document.render(surface, window, cx),
        };
        let scrolled = self.scroll.max_offset().y > px(0.0) && self.scroll.offset().y < px(-0.5);
        let closing = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = closing.update(cx, |workbench, cx| {
                workbench.close(&CloseDeveloperWorkbench, window, cx);
            });
        });
        let heading = DetailHeading::new(
            PREFIX,
            section_heading(
                section.selector(),
                section.title(),
                section.description(),
                appearance,
            ),
            &self.window_movement,
            close,
        )
        .toolbar(self.render_toolbar(cx))
        .scrolled(scrolled)
        .render(surface);
        let revealing = cx.weak_entity();
        div()
            .debug_selector(|| "workbench-canvas".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(gpui_color(
                surface.surface(SettingsSurfaceRole::Canvas).paint,
            ))
            .child(heading)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("workbench-detail")
                            .debug_selector(|| "workbench-detail".to_owned())
                            .track_scroll(&self.scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .px(card_gutter(appearance))
                            .pt(appearance.spacing(8.0))
                            .pb(appearance.spacing(18.0))
                            .gap(group_spacing(appearance))
                            .on_scroll_wheel(move |_, _, cx| {
                                let _ = revealing.update(cx, |workbench, cx| {
                                    workbench.reveal_scrollbar(cx);
                                });
                            })
                            .children(groups),
                    )
                    .child(self.scrollbar.clone()),
            )
            .into_any_element()
    }

    /// The preview's state and the two operations that end it.
    fn render_footer(&self, surface: &SettingsAppearance, cx: &mut Context<Self>) -> AnyElement {
        let appearance = &surface.chrome;
        let open = self.preview.is_open();
        let button = |selector: &'static str,
                      label: &'static str,
                      operation: fn(&mut Self, &mut Context<Self>)| {
            let owner = cx.weak_entity();
            action_button(selector, label, open, move |_, cx| {
                let _ = owner.update(cx, operation);
            })
        };
        super::sidebar_window::render_footer(
            PREFIX,
            surface,
            div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .gap(appearance.spacing(8.0))
                .child(
                    div()
                        .debug_selector(|| "workbench-status".to_owned())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(appearance.colors.text_muted))
                        .child(self.status.clone()),
                )
                .child(button("workbench-cancel", "Cancel", Self::cancel))
                .child(button("workbench-commit", "Commit", Self::commit)),
        )
    }
}
