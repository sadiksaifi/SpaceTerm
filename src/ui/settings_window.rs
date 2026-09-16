//! The Settings Window: a separate, modeless Operating-System Window for SpaceTerm Settings.
//!
//! Settings is its own window rather than a Workspace panel because it is application scoped: it
//! outlives any one Workspace and must stay reachable when no Workspace window exists. It follows
//! the desktop convention for a settings window while remaining entirely GPUI-rendered, so nothing
//! here depends on a host settings surface and the layout stays portable.

mod catalog;
mod controls;
mod editor;
mod import;
mod microphone;
mod schemes;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod control_tests;

#[cfg(test)]
mod microphone_tests;

#[cfg(test)]
#[path = "settings_window/tests.rs"]
mod tests;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Edges, Entity, FocusHandle, Global, Pixels,
    ScrollHandle, SharedString, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, actions, div, px, size,
};
use spaceterm_ui::{
    Alert, AlertIntent, ComboBox, ComboBoxItem, Icon, IconButton, IconName, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer, OverlayScrollbar,
    OverlayScrollbarEvent, ScrollMetrics, SegmentedControl, SegmentedOption, SegmentedSize, Switch,
    TextInput, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior, TextInputVariant,
    ToggleSize, TooltipLayer, WindowDragRegion, WindowDragRegionEvent, WindowDragRegionResponse,
};

use crate::appearance::{
    Appearance, AppearanceMode, ChromeDensity, ChromeFontFamily, Color, FontClass, SchemeId,
    SchemeKind, SettingsDocument, TerminalFontFamily,
};
use crate::platform::microphone_access::MicrophoneAccess;
#[cfg(test)]
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform, WindowMovementFactory,
};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};

use catalog::{ROWS, SettingsRowId, SettingsSectionId};
use controls::{
    CARD_RADIUS, ROW_INSET, SettingsGroup, SettingsRow, SettingsRowLayout, Stepper, action_button,
    gpui_color, reset_button, section_heading, text,
};
use editor::{SaveStatus, SettingsEditor};
use microphone::MicrophoneAccessRow;

actions!(
    spaceterm,
    [
        OpenSettings,
        CloseSettingsWindow,
        FocusSettingsSearch,
        ClearSettingsSearch
    ]
);

/// The key context the Settings Window publishes, so its shortcuts override Workspace shortcuts.
pub(crate) const SETTINGS_KEY_CONTEXT: &str = "Settings";

/// Fixed window geometry. Settings does not resize, so content scrolls inside a stable frame.
const WINDOW_WIDTH: f32 = 880.0;
const WINDOW_HEIGHT: f32 = 640.0;
const SIDEBAR_WIDTH: f32 = 196.0;
const SIDEBAR_INSET: f32 = 10.0;
/// The strip under the window carrying the save status and the one application-wide action.
const FOOTER_HEIGHT: f32 = 40.0;
/// The height of one navigation entry and of the search field above it, so the sidebar runs on one
/// rhythm from its first row to its last.
const NAVIGATION_ROW_HEIGHT: f32 = 28.0;
/// The radius of the navigation chip and of the search field, and the air a focus ring keeps
/// outside that chip.
const NAVIGATION_CHIP_RADIUS: f32 = crate::ui::selection_chip::CHIP_RADIUS;
const NAVIGATION_CHIP_RING_GAP: f32 = 2.0;

/// The chip a navigation entry rests its hover and its current-section state on.
///
/// It fills the entry rather than insetting further: the sidebar's own padding and the space
/// between entries are already the air around it, and a second inset would narrow the chip against
/// the search field it sits under.
fn navigation_chip(
    selected: bool,
    available: bool,
    appearance: &ChromeAppearance,
) -> SelectionChip {
    SelectionChip::new(
        ChipShape::symmetric(px(0.0), px(0.0), appearance.spacing(NAVIGATION_CHIP_RADIUS)),
        navigation_chip_paint(selected, available, &appearance.colors).raised(appearance),
    )
}

fn navigation_chip_paint(
    selected: bool,
    available: bool,
    colors: &crate::appearance::ChromeColors,
) -> ChipPaint {
    // Hover changes the fill. The selected rim stays neutral, while keyboard focus has its own
    // outset ring; a hover rim at the chip edge would look like persistent keyboard focus.
    if selected {
        ChipPaint {
            fill: Some(colors.row_selected_background),
            rim: Some(colors.row_selected_border),
            hover_fill: Some(colors.row_selected_hover_background),
            hover_rim: None,
        }
    } else {
        ChipPaint {
            // A resting row the same color as the sidebar paints nothing, so the base is not
            // composited twice beneath a translucent window.
            fill: (colors.row_background != colors.panel_background)
                .then_some(colors.row_background),
            rim: None,
            // A section the query emptied cannot be chosen, so nothing lights under the pointer.
            hover_fill: available.then_some(colors.row_hover_background),
            hover_rim: None,
        }
    }
}

/// The one Settings Window, so a second request activates the existing window.
struct OpenSettingsWindow(WindowHandle<SettingsWindow>);
impl Global for OpenSettingsWindow {}

/// Host-owned capabilities needed by the Settings window's app-drawn titlebar and Privacy section.
///
/// Keeping the native movement adapter behind the same factory used by Workspace windows leaves
/// Settings portable and gives each opened window one independent pointer-interaction owner. A
/// host without microphone authorization composes none, and the Privacy section says so.
struct SettingsWindowComposition {
    window_movement: Rc<dyn WindowMovementFactory>,
    microphone_access: Option<Rc<dyn MicrophoneAccess>>,
}
impl Global for SettingsWindowComposition {}

pub(crate) fn configure_window_chrome(
    window_movement: Rc<dyn WindowMovementFactory>,
    microphone_access: Option<Rc<dyn MicrophoneAccess>>,
    cx: &mut App,
) {
    cx.set_global(SettingsWindowComposition {
        window_movement,
        microphone_access,
    });
}

/// Opens Settings, or activates it when it is already open.
pub(crate) fn open_or_activate(cx: &mut App) {
    if !cx.has_global::<crate::ui::appearance_runtime::AppearanceRuntime>() {
        // Settings edits the retained document through the appearance runtime. Without it there is
        // nothing to present, so declining is the honest outcome rather than an empty window.
        eprintln!("SpaceTerm Settings is unavailable because appearance is not installed");
        return;
    }
    if let Some(existing) = cx
        .try_global::<OpenSettingsWindow>()
        .map(|global| global.0)
        .filter(|handle| handle.read(cx).is_ok())
    {
        cx.defer(move |cx| {
            let _ = existing.update(cx, |_, window, _| window.activate_window());
        });
        return;
    }
    let Some(composition) = cx.try_global::<SettingsWindowComposition>() else {
        eprintln!("SpaceTerm Settings is unavailable because window chrome is not installed");
        return;
    };
    let window_drag = composition.window_movement.create();
    let microphone_access = composition.microphone_access.clone();
    let titlebar_height = crate::ui::appearance::chrome(cx).top_height();
    let traffic_light_position = cx
        .try_global::<crate::platform::window_frame::WindowFrameGeometry>()
        .and_then(|geometry| geometry.settings_traffic_light_position(titlebar_height));
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_background: crate::ui::appearance_runtime::window_background(cx),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT))),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                // Retain the native title for the Window menu and accessibility while drawing the
                // visible section title in the client surface.
                appears_transparent: true,
                traffic_light_position,
            }),
            // A floating window stays ordinary and modeless: the normal window level and the
            // ordinary window class, with only tabbing, resizing, and minimizing withheld. It is
            // deliberately not a popup and never orders itself above other applications.
            kind: WindowKind::Floating,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            tabbing_identifier: None,
            ..WindowOptions::default()
        },
        |window, cx| {
            let settings = cx.new(|cx| {
                SettingsWindow::new_with_capabilities(
                    Rc::clone(&window_drag),
                    microphone_access.clone(),
                    window,
                    cx,
                )
            });
            let closing = settings.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                let _ = closing.update(cx, |settings, cx| {
                    settings.request_close(CloseIntent::Window(window.window_handle()), cx);
                });
                false
            });
            settings
        },
    );
    match opened {
        Ok(handle) => {
            cx.set_global(OpenSettingsWindow(handle));
            cx.activate(true);
        }
        Err(error) => eprintln!("failed to open the SpaceTerm Settings window: {error}"),
    }
}

/// Retains Settings until its latest edit is saved, then completes an authorized application quit.
pub(crate) fn quit_when_saved(cx: &mut App, completion: crate::app::ApplicationQuitAfterSave) {
    if let Some(settings) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>())
    {
        let _ = settings.update(cx, |settings, window, cx| {
            window.activate_window();
            settings.request_close(CloseIntent::Application(completion), cx);
        });
    } else {
        completion.complete(cx);
    }
}

#[derive(Clone)]
enum CloseIntent {
    Window(AnyWindowHandle),
    Application(crate::app::ApplicationQuitAfterSave),
}

/// Registers the application-scoped Settings actions.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| open_or_activate(cx));
}

fn appearance_mode_label(mode: AppearanceMode) -> &'static str {
    match mode {
        AppearanceMode::Light => "Light",
        AppearanceMode::Dark => "Dark",
        AppearanceMode::Auto => "Auto",
    }
}

pub(crate) struct SettingsWindow {
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
    editor: SettingsEditor,
    close_after_save: Option<CloseIntent>,
    search: Entity<TextInput>,
    query: SharedString,
    scroll: ScrollHandle,
    /// The scroll affordance for the detail pane, so a long section is visibly scrollable.
    scrollbar: Entity<OverlayScrollbar<f32>>,
    /// The section the detail pane presents. Navigation selects one view at a time.
    active_section: SettingsSectionId,
    /// The row Settings Search revealed, highlighted so the eye lands on it.
    revealed: Option<SettingsRowId>,
    interchange_status: Option<SharedString>,
    focus_handle: FocusHandle,
    /// One keyboard stop for section navigation. Pointer selection leaves focus on the window root.
    navigation_focus: FocusHandle,
    /// Keyboard traversal enables the ring; pointer selection withdraws keyboard focus and the ring.
    navigation_focus_visible: bool,
    operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    microphone_access: MicrophoneAccessRow,
}

impl SettingsWindow {
    #[cfg(test)]
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            None,
            window,
            cx,
        )
    }

    fn new_with_capabilities(
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        microphone_access: Option<Rc<dyn MicrophoneAccess>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Transparent native chrome hides this visually while retaining a stable Operating-System
        // window identity for the Window menu and accessibility clients.
        window.set_window_title("Settings");
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        let settings = cx
            .global::<crate::ui::appearance_runtime::AppearanceRuntime>()
            .settings
            .clone();
        let editor = SettingsEditor::new(settings);
        // The window takes focus so its own shortcuts and Tab traversal resolve from the moment it
        // opens, rather than only after something inside it is clicked.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let navigation_focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&navigation_focus, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&navigation_focus, window, |_, _, cx| cx.notify())
            .detach();
        let search = cx.new(|cx| {
            TextInput::new(
                "settings-search",
                "Search settings",
                String::new(),
                window,
                cx,
            )
            .placeholder("Search")
            .variant(TextInputVariant::Bare)
            .return_behavior(TextInputReturnBehavior::Propagate)
            .escape_behavior(TextInputEscapeBehavior::Propagate)
            .tab_behavior(spaceterm_ui::TextInputTabBehavior::Propagate)
            .input_length_limit(Some(128))
            .emit_programmatic_changes(true)
            .debug_selector("settings-search")
        });
        cx.subscribe_in(
            &search,
            window,
            |settings, search, event: &TextInputEvent, window, cx| {
                if matches!(
                    event,
                    TextInputEvent::TabForwardRequested | TextInputEvent::TabBackwardRequested
                ) {
                    settings.navigation_focus_visible = true;
                    if matches!(event, TextInputEvent::TabForwardRequested) {
                        window.focus_next();
                    } else {
                        window.focus_prev();
                    }
                    cx.notify();
                }
                if matches!(event, TextInputEvent::ValueChanged(_)) {
                    settings.query = SharedString::from(search.read(cx).value().to_owned());
                    settings.revealed = catalog::matching_rows(&settings.query).first().copied();
                    // Each section is its own view, so a query that the visible one cannot answer
                    // moves to the first section that can.
                    if settings.rows_for(settings.active_section).is_empty()
                        && let Some(section) = settings.section_for_query()
                    {
                        settings.active_section = section;
                        settings.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                    }
                    cx.notify();
                }
            },
        )
        .detach();
        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("settings-scrollbar"));
        cx.subscribe(
            &scrollbar,
            |settings, _, event: &OverlayScrollbarEvent<f32>, cx| {
                if let OverlayScrollbarEvent::OffsetRequested(offset) = event {
                    let current = settings.scroll.offset();
                    settings
                        .scroll
                        .set_offset(gpui::point(current.x, px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();
        // Another surface may commit while this window is open, and a preview repaints everything.
        cx.observe_global_in::<crate::ui::appearance_runtime::InstalledAppearance>(
            window,
            |settings, window, cx| {
                settings.window_appearance.apply(window, cx);
                settings.editor.synchronize();
                cx.notify();
            },
        )
        .detach();
        // Authorization can change in the system's settings while this window is in the background,
        // most often right after the Denied recovery sent the person there.
        cx.observe_window_activation(window, |settings, window, cx| {
            if window.is_window_active() {
                settings.refresh_microphone_access(cx);
            }
        })
        .detach();
        cx.on_app_quit(|settings, cx| {
            if !settings.editor.flush_for_shutdown(cx) {
                eprintln!("SpaceTerm Settings could not be saved during shutdown");
            }
            async {}
        })
        .detach();
        Self {
            window_appearance,
            editor,
            close_after_save: None,
            search,
            query: SharedString::default(),
            scroll: ScrollHandle::new(),
            scrollbar,
            active_section: SettingsSectionId::Appearance,
            revealed: None,
            interchange_status: None,
            focus_handle,
            navigation_focus,
            navigation_focus_visible: true,
            operating_system_window_drag_platform,
            microphone_access: MicrophoneAccessRow::new(microphone_access),
        }
    }

    fn close(&mut self, _: &CloseSettingsWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close(CloseIntent::Window(window.window_handle()), cx);
    }

    fn request_close(&mut self, intent: CloseIntent, cx: &mut Context<Self>) {
        if !matches!(
            self.close_after_save.as_ref(),
            Some(CloseIntent::Application(_))
        ) {
            self.close_after_save = Some(intent);
        }
        self.finish_close(cx);
    }

    fn finish_close(&mut self, cx: &mut Context<Self>) {
        let Some(intent) = self.close_after_save.clone() else {
            return;
        };
        if self.editor.flush(cx) {
            self.close_after_save = None;
            match intent {
                CloseIntent::Window(handle) => cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                }),
                CloseIntent::Application(completion) => completion.complete(cx),
            }
        } else if !self.editor.is_writing() {
            // Keep the draft and the existing Retry/Reload feedback instead of discarding a failed
            // save. A competing preview is reported once rather than spinning during close.
            self.close_after_save = None;
        }
    }

    fn focus_search(
        &mut self,
        _: &FocusSettingsSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = self.search.update(cx, |search, cx| {
            search.select_all(cx);
            search.focus_handle()
        });
        handle.focus(window);
        cx.notify();
    }

    fn clear_search(
        &mut self,
        _: &ClearSettingsSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query.is_empty() {
            return;
        }
        self.search.update(cx, |search, cx| {
            search.set_value(String::new(), cx);
        });
        self.query = SharedString::default();
        self.revealed = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    /// Presents one section. Each section is its own view, so the detail pane starts at its top.
    fn reveal_section(&mut self, section: SettingsSectionId, cx: &mut Context<Self>) {
        self.active_section = section;
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// The first section answering the current query, so search never lands on an empty view.
    fn section_for_query(&self) -> Option<SettingsSectionId> {
        SettingsSectionId::ALL
            .into_iter()
            .find(|section| !self.rows_for(*section).is_empty())
    }

    fn rows_for(&self, section: SettingsSectionId) -> Vec<SettingsRowId> {
        let matching = catalog::matching_rows(&self.query);
        ROWS.iter()
            .filter(|row| row.section == section && matching.contains(&row.id))
            .map(|row| row.id)
            .filter(|row| self.row_applies(*row))
            .collect()
    }

    /// Both scheme engines use the shared mode while retaining their own light and dark slots.
    fn row_applies(&self, row: SettingsRowId) -> bool {
        let auto = self.editor.document().preferences.mode == AppearanceMode::Auto;
        match row {
            SettingsRowId::ChromeScheme | SettingsRowId::TerminalScheme => !auto,
            SettingsRowId::ChromeLightScheme
            | SettingsRowId::ChromeDarkScheme
            | SettingsRowId::TerminalLightScheme
            | SettingsRowId::TerminalDarkScheme => auto,
            _ => true,
        }
    }

    fn fixed_appearance(&self) -> Appearance {
        self.editor
            .document()
            .preferences
            .mode
            .resolve(Appearance::Dark)
    }

    fn edit(&mut self, edit: impl FnOnce(&mut SettingsDocument), cx: &mut Context<Self>) {
        self.editor.edit(edit, cx);
    }

    /// Whether this row differs from its default, which is when a reset is worth offering.
    fn differs_from_default(&self, row: SettingsRowId) -> bool {
        let Some(target) = row.reset_target(self.fixed_appearance()) else {
            return false;
        };
        // Every resettable row asks this on every frame, so only preferences are copied. Cloning
        // the document would copy the whole installed scheme catalog to answer a question about
        // one field.
        let current = &self.editor.document().preferences;
        let mut reset = current.clone();
        reset.reset(target);
        reset != *current
    }

    fn row_reset(&self, row: SettingsRowId, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.differs_from_default(row) || !self.editor.editable() {
            return None;
        }
        let target = row.reset_target(self.fixed_appearance())?;
        let owner = cx.weak_entity();
        Some(
            reset_button(
                format!("{}-reset", row.descriptor().selector),
                row.descriptor().label,
                true,
                move |_, cx| {
                    let target = target.clone();
                    let _ = owner.update(cx, |settings, cx| settings.editor.reset(target, cx));
                },
            )
            .into_any_element(),
        )
    }
}

impl SettingsWindow {
    fn scroll_metrics(&self) -> Option<ScrollMetrics<f32>> {
        ScrollMetrics::for_pixels(
            0.0,
            f32::from(self.scroll.bounds().size.height),
            f32::from(self.scroll.max_offset().height),
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

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = crate::ui::appearance::chrome(cx).clone();
        self.sync_scrollbar(cx);
        let content = div()
            .debug_selector(|| "settings-window-surface".to_owned())
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::clear_search))
            // Record unbound Tab before child key handlers. Search delegates its bound traversal
            // action explicitly. Focus notifications must never decide input modality.
            .capture_key_down(cx.listener(|settings, event: &gpui::KeyDownEvent, _, cx| {
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key == "tab"
                    && !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.function
                    && !settings.navigation_focus_visible
                {
                    settings.navigation_focus_visible = true;
                    cx.notify();
                }
            }))
            .on_key_down(|event: &gpui::KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if event.keystroke.key != "tab"
                    || modifiers.control
                    || modifiers.alt
                    || modifiers.platform
                    || modifiers.function
                {
                    return;
                }
                // Inputs and popups handle their own traversal first. Other Settings controls
                // delegate an unhandled Tab to the window's registered focus order.
                if modifiers.shift {
                    window.focus_prev();
                } else {
                    window.focus_next();
                }
                window.prevent_default();
                cx.stop_propagation();
            })
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui_color(appearance.surface(
                crate::appearance::SurfaceRole::Sheet,
                appearance.colors.background,
            )))
            .text_color(gpui_color(appearance.colors.text))
            .text_size(appearance.text_size(text::BODY))
            .font(appearance.regular.clone())
            // Both columns run to the window's top edge beneath the transparent native titlebar.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&appearance, window, cx))
                    .child(self.render_detail(&appearance, window, cx)),
            )
            .child(self.render_footer(&appearance, cx));
        ModalLayer::new(TooltipLayer::new(content))
    }
}

impl SettingsWindow {
    fn handle_window_drag_event(
        &mut self,
        event: WindowDragRegionEvent,
        window: &mut Window,
    ) -> WindowDragRegionResponse {
        match event {
            WindowDragRegionEvent::InteractionStarted { .. } => {
                if let Err(error) = self
                    .operating_system_window_drag_platform
                    .interaction_started()
                {
                    Self::report_window_drag_error("begin", error);
                }
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::MoveRequested { .. } => {
                match self
                    .operating_system_window_drag_platform
                    .start_window_move(window)
                {
                    Ok(()) => WindowDragRegionResponse::OperatingSystemWindowMoveStarted,
                    Err(error) => {
                        Self::report_window_drag_error("start", error);
                        WindowDragRegionResponse::Continue
                    }
                }
            }
            WindowDragRegionEvent::DoubleActivationRequested => {
                window.titlebar_double_click();
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::InteractionFinished { .. } => {
                self.operating_system_window_drag_platform
                    .interaction_finished();
                WindowDragRegionResponse::Continue
            }
        }
    }

    fn report_window_drag_error(operation: &str, error: OperatingSystemWindowDragError) {
        eprintln!("failed to {operation} Settings Window drag: {error}");
    }

    /// Wraps client chrome in a native window-movement region.
    ///
    /// The sidebar's traffic-light strip and the content column's heading are separate regions so
    /// Search and every other control stay outside drag ownership, while the uncovered space in
    /// both behaves as the titlebar, including double-click.
    fn window_drag_region(
        &self,
        id: &'static str,
        content: impl IntoElement,
        pointer_insets: Edges<Pixels>,
        cx: &mut Context<Self>,
    ) -> WindowDragRegion {
        let owner = cx.weak_entity();
        WindowDragRegion::new(
            id,
            "Move Operating-System Window from Settings chrome",
            content,
        )
        .pointer_insets(pointer_insets)
        .debug_selector(id)
        .on_event(move |event, window, cx| {
            let event = *event;
            owner
                .update(cx, |settings, _| {
                    settings.handle_window_drag_event(event, window)
                })
                .unwrap_or_default()
        })
    }

    /// The sidebar material beneath the native traffic lights, reserved for window movement.
    fn render_sidebar_titlebar(
        &self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let region = self.window_drag_region(
            "settings-sidebar-drag-region",
            div().size_full(),
            Edges {
                left: px(super::workspace_chrome::TRAFFIC_LIGHT_CLEARANCE),
                ..Edges::default()
            },
            cx,
        );
        div()
            .debug_selector(|| "settings-sidebar-titlebar".to_owned())
            .flex_none()
            .w_full()
            .h(appearance.top_height())
            .child(region)
            .into_any_element()
    }

    /// The active section's large title and description at the head of the content surface.
    ///
    /// The heading stays fixed while rows scroll beneath it, the way a native Settings pane keeps
    /// its identity in view. Its top edge shares the traffic-light row, and the whole heading is
    /// window-movement space; a hairline appears only once content has scrolled under it.
    fn render_detail_heading(
        &self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section = self.active_section;
        let scrolled =
            self.scroll.max_offset().height > px(0.0) && self.scroll.offset().y < px(-0.5);
        let heading = div()
            .size_full()
            .px(appearance.spacing(CONTENT_GUTTER))
            .pt(appearance.spacing(HEADING_TOP_INSET))
            .pb(appearance.spacing(14.0))
            .child(section_heading(
                section.selector(),
                section.title(),
                section.description(),
                appearance,
            ));
        let region =
            self.window_drag_region("settings-detail-drag-region", heading, Edges::default(), cx);
        div()
            .debug_selector(|| "settings-detail-heading".to_owned())
            .relative()
            .flex_none()
            .w_full()
            .child(region)
            .when(scrolled, |heading| {
                heading.child(
                    div()
                        .debug_selector(|| "settings-detail-heading-divider".to_owned())
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(appearance.spacing(super::resize_handle_theme::VISIBLE_THICKNESS))
                        .bg(gpui_color(appearance.colors.border)),
                )
            })
            .into_any_element()
    }

    fn navigation_has_visible_focus(&self, window: &Window) -> bool {
        self.navigation_focus.is_focused(window) && self.navigation_focus_visible
    }

    /// Moves the navigation selection with the keyboard, skipping what the query emptied.
    ///
    /// The list activates as it moves, the way the Workspace sidebar does: each section is a view
    /// rather than a destination to confirm, so a separate commit step would say nothing.
    fn navigate_sections(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.navigation_focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let available = self.navigable_sections();
        let Some(current) = available
            .iter()
            .position(|section| *section == self.active_section)
        else {
            return;
        };
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(available.len() - 1),
            "home" => 0,
            "end" => available.len() - 1,
            _ => return,
        };
        window.prevent_default();
        cx.stop_propagation();
        self.navigation_focus_visible = true;
        if next != current {
            self.reveal_section(available[next], cx);
        }
        cx.notify();
    }

    /// The sections the current query left something to present.
    fn navigable_sections(&self) -> Vec<SettingsSectionId> {
        let matching = catalog::matching_rows(&self.query);
        SettingsSectionId::ALL
            .into_iter()
            .filter(|section| {
                ROWS.iter()
                    .any(|row| row.section == *section && matching.contains(&row.id))
            })
            .collect()
    }

    fn render_sidebar(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = self.navigable_sections();
        let list_focused = self.navigation_has_visible_focus(window);
        let entries = SettingsSectionId::ALL
            .iter()
            .map(|section| {
                let section = *section;
                let has_matches = available.contains(&section);
                let selected = self.active_section == section && has_matches;
                let owner = cx.weak_entity();
                let row_group = format!("settings-row-state-{}", section.selector());
                let colors = &appearance.colors;
                let (foreground, icon, hover_foreground, hover_icon) = if selected {
                    (
                        colors.row_selected_foreground,
                        colors.row_selected_icon,
                        colors.row_selected_hover_foreground,
                        colors.row_selected_hover_icon,
                    )
                } else {
                    (
                        colors.row_foreground,
                        colors.row_icon,
                        colors.row_hover_foreground,
                        colors.row_hover_icon,
                    )
                };
                // The same chip the Workspace sidebar rests its current row on, so the two
                // navigation surfaces read as one material rather than as two conventions.
                let chip = navigation_chip(selected, has_matches, appearance);
                let chip_selector = format!("settings-navigation-chip-{}", section.selector());
                div()
                    .id(SharedString::from(format!(
                        "settings-navigation-{}",
                        section.navigation_title()
                    )))
                    .debug_selector(move || format!("settings-navigation-{}", section.selector()))
                    .relative()
                    .group(row_group.clone())
                    .text_color(gpui_color(foreground))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(7.0))
                    .w_full()
                    .h(appearance.height(NAVIGATION_ROW_HEIGHT, text::BODY))
                    .px(appearance.spacing(8.0))
                    .cursor_default()
                    .when(selected, |entry| entry.font(appearance.emphasis.clone()))
                    .child(chip.render(chip_selector, &row_group))
                    .when(has_matches, |entry| {
                        entry
                            .hover(move |entry| entry.text_color(gpui_color(hover_foreground)))
                            .on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |settings, cx| {
                                    // A completed pointer selection is authoritative even if
                                    // native focus moved between the press and release.
                                    settings.navigation_focus_visible = false;
                                    settings.focus_handle.focus(window);
                                    settings.reveal_section(section, cx);
                                });
                            })
                    })
                    .when(!has_matches, |entry| {
                        entry.text_color(gpui_color(appearance.colors.text_disabled))
                    })
                    .when(selected && list_focused, |entry| {
                        entry.child(chip.ring(
                            appearance.spacing(NAVIGATION_CHIP_RING_GAP),
                            appearance.colors.sidebar_focus,
                            "settings-navigation-focus-indicator",
                        ))
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_color(gpui_color(if has_matches {
                                icon
                            } else {
                                colors.icon_disabled
                            }))
                            .when(has_matches, |icon| {
                                icon.group_hover(row_group, |style| {
                                    style.text_color(gpui_color(hover_icon))
                                })
                            })
                            .child(Icon::inherited(
                                match section {
                                    SettingsSectionId::Appearance => IconName::SunMoon,
                                    SettingsSectionId::Interface => IconName::AppWindow,
                                    SettingsSectionId::Terminal => IconName::Terminal,
                                    SettingsSectionId::ColorSchemes => IconName::Palette,
                                    SettingsSectionId::Privacy => IconName::Shield,
                                },
                                appearance.text_size(13.0),
                            )),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(appearance.text_size(text::BODY))
                            .child(section.navigation_title()),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .debug_selector(|| "settings-sidebar".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(appearance.text_size(SIDEBAR_WIDTH))
            .h_full()
            .bg(gpui_color(appearance.control_colors.panel_background))
            .child(self.render_sidebar_titlebar(appearance, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .px(appearance.spacing(SIDEBAR_INSET))
                    .pt(appearance.spacing(SIDEBAR_INSET))
                    .child(self.render_search_field(appearance, cx))
                    .child(
                        div()
                            .id("settings-navigation")
                            .debug_selector(|| "settings-navigation".to_owned())
                            .when(!available.is_empty(), |navigation| {
                                navigation.track_focus(&self.navigation_focus)
                            })
                            // GPUI track_focus automatically focuses on mouse-down. Suppress that before
                            // its bubble listener runs: pointer selection does not enter keyboard navigation.
                            .capture_any_mouse_down(cx.listener(
                                |settings, event: &gpui::MouseDownEvent, window, cx| {
                                    if event.button != gpui::MouseButton::Left {
                                        return;
                                    }
                                    window.prevent_default();
                                    settings.navigation_focus_visible = false;
                                    settings.focus_handle.focus(window);
                                    cx.notify();
                                },
                            ))
                            .on_key_down(cx.listener(|settings, event, window, cx| {
                                settings.navigate_sections(event, window, cx);
                            }))
                            .flex()
                            .flex_col()
                            .w_full()
                            .gap(appearance.spacing(2.0))
                            .children(entries),
                    ),
            )
            .into_any_element()
    }

    fn render_search_field(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let search_focus = self.search.read(cx).focus_handle();
        let owner = cx.weak_entity();
        spaceterm_ui::field_frame(
            "settings-search-frame",
            &search_focus,
            spaceterm_ui::FieldState::default(),
            cx,
        )
        .debug_selector(|| "settings-search-frame".to_owned())
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(appearance.spacing(7.0))
        .px(appearance.spacing(8.0))
        .h(appearance.height(NAVIGATION_ROW_HEIGHT, text::BODY))
        // The search field belongs to the window, not to the navigation list under it, so the
        // break between them is wider than the spacing inside the list.
        .mb(appearance.spacing(12.0))
        .rounded(appearance.spacing(NAVIGATION_CHIP_RADIUS))
        // The same glyph size the navigation icons take, so one icon column and one text
        // column run the height of the sidebar.
        .child(div().flex_none().child(Icon::new(
            IconName::Search,
            appearance.text_size(13.0),
            gpui_color(appearance.colors.input_placeholder),
        )))
        .child(div().min_w_0().flex_1().child(self.search.clone()))
        .when(!self.query.is_empty(), |field| {
            field.child(
                IconButton::new("settings-search-clear", "Clear search", |foreground| {
                    Icon::new(IconName::X, px(10.0), foreground).into_any_element()
                })
                .variant(spaceterm_ui::ButtonVariant::Ghost)
                .contextual_style(
                    controls::field_action_style(&appearance.colors),
                    gpui_color(appearance.colors.input_focused_border),
                )
                .size(spaceterm_ui::ButtonSize::Compact)
                .tab_stop(true)
                .debug_selector("settings-search-clear")
                .on_activate(move |_, window, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.clear_search(&ClearSettingsSearch, window, cx);
                    });
                }),
            )
        })
        .into_any_element()
    }

    fn render_detail(
        &mut self,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // One section at a time: navigation selects a view rather than a scroll destination, so
        // nothing from a neighbouring section can scroll into this one.
        let section = self.render_section(self.active_section, appearance, window, cx);
        let empty = self.rows_for(self.active_section).is_empty();
        let revealing = cx.weak_entity();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(gpui_color(appearance.surface(
                crate::appearance::SurfaceRole::Base,
                appearance.colors.background,
            )))
            .child(self.render_detail_heading(appearance, cx))
            .children(self.render_banner(appearance, cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("settings-detail")
                            .debug_selector(|| "settings-detail".to_owned())
                            .track_scroll(&self.scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .px(appearance.spacing(CARD_GUTTER))
                            .pt(appearance.spacing(8.0))
                            .pb(appearance.spacing(18.0))
                            .on_scroll_wheel(move |_, _, cx| {
                                let _ = revealing.update(cx, |settings, cx| {
                                    settings.reveal_scrollbar(cx);
                                });
                            })
                            .child(section)
                            .when(empty, |detail| {
                                detail.child(
                                    div()
                                        .debug_selector(|| "settings-no-results".to_owned())
                                        .px(appearance.spacing(ROW_INSET))
                                        .text_color(gpui_color(appearance.colors.text_muted))
                                        .child(SharedString::from(format!(
                                            "No settings match “{}”.",
                                            self.query
                                        ))),
                                )
                            }),
                    )
                    .child(self.scrollbar.clone()),
            )
            .into_any_element()
    }

    fn render_section(
        &mut self,
        section: SettingsSectionId,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.rows_for(section);
        if rows.is_empty() {
            return div()
                .debug_selector(move || format!("{}-empty", section.selector()))
                .into_any_element();
        }
        // Rows keep catalog order, so one run of neighbouring rows sharing a group title is one
        // card. A filtered view groups whatever survived the filter the same way.
        let mut groups: Vec<(&'static str, Vec<AnyElement>)> = Vec::new();
        for row in rows {
            let title = row.descriptor().group;
            let rendered = self.render_row(row, appearance, window, cx);
            match groups.last_mut() {
                Some((current, members)) if *current == title => members.push(rendered),
                _ => groups.push((title, vec![rendered])),
            }
        }
        let rendered = groups
            .into_iter()
            .map(|(title, members)| {
                SettingsGroup::new(group_selector(section, title), title, members)
                    .render(appearance)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let notice = (section == SettingsSectionId::ColorSchemes)
            .then(|| self.render_diagnostics_notice(appearance, cx))
            .flatten();
        div()
            .debug_selector(move || section.selector().to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(26.0))
            .children(notice)
            .children(rendered)
            .into_any_element()
    }
}

/// The space between the detail pane's edge and the text inside it.
///
/// Rows carry part of it themselves so a card's own edge clears the text it holds, and the column
/// gives back the rest. The two together are what a reader sees as the content's left edge.
const CONTENT_GUTTER: f32 = 26.0;

/// The space between the detail pane's edge and the cards standing in it.
///
/// It is the content gutter less the inset a row carries, so a card's edge stands outside the text
/// it holds by exactly that inset and a row's own fill can reach that edge. A card is a container
/// rather than something to read, so this line belongs to the cards alone: everything a reader
/// tracks down the page stays on [`CONTENT_GUTTER`].
const CARD_GUTTER: f32 = CONTENT_GUTTER - ROW_INSET;

/// The heading's distance from the window's top edge, which it shares with the traffic lights.
///
/// The title's line box begins just under the controls' top edge, so the large title reads as the
/// window's own name without crowding the native controls in the neighbouring column.
const HEADING_TOP_INSET: f32 = 20.0;

/// The weight choices a settings surface offers, rather than every value the document accepts.
const WEIGHTS: [(u16, &str); 6] = [
    (300, "Light (300)"),
    (400, "Regular (400)"),
    (500, "Medium (500)"),
    (600, "Semibold (600)"),
    (700, "Bold (700)"),
    (800, "Extrabold (800)"),
];

impl SettingsWindow {
    fn render_row(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let descriptor = row.descriptor();
        let highlighted = !self.query.is_empty() && self.revealed == Some(row);
        // App-owned copy inside a full-width row shares the row's surface. Reusable controls
        // retain their own complete paints through the installed control catalog.
        let mut highlighted_appearance;
        let content_appearance = if highlighted {
            highlighted_appearance = appearance.clone();
            highlighted_appearance.colors.text = appearance.colors.row_selected_foreground;
            highlighted_appearance.colors.text_secondary = appearance.colors.row_selected_secondary;
            highlighted_appearance.colors.text_muted = appearance.colors.row_selected_secondary;
            &highlighted_appearance
        } else {
            appearance
        };
        let control = self.render_control(row, content_appearance, cx);
        let mut rendered = SettingsRow::new(descriptor.selector, descriptor.label, control)
            .layout(row_layout(row))
            .reset(self.row_reset(row, cx))
            .highlighted(highlighted);
        if let Some(description) = self.row_description(row) {
            rendered = rendered.description(description);
        }
        rendered.render(appearance, window, cx).into_any_element()
    }

    fn render_control(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRowId::AppearanceMode => self.render_appearance_mode(cx),
            SettingsRowId::Transparency => self.render_transparency(appearance, cx),
            SettingsRowId::BackgroundBlur => self.render_background_blur(cx),
            SettingsRowId::ChromeScheme => {
                self.render_scheme_picker(row, SchemeKind::Chrome, None, appearance, cx)
            }
            SettingsRowId::ChromeLightScheme => self.render_scheme_picker(
                row,
                SchemeKind::Chrome,
                Some(Appearance::Light),
                appearance,
                cx,
            ),
            SettingsRowId::ChromeDarkScheme => self.render_scheme_picker(
                row,
                SchemeKind::Chrome,
                Some(Appearance::Dark),
                appearance,
                cx,
            ),
            SettingsRowId::TerminalScheme => {
                self.render_scheme_picker(row, SchemeKind::Terminal, None, appearance, cx)
            }
            SettingsRowId::TerminalLightScheme => self.render_scheme_picker(
                row,
                SchemeKind::Terminal,
                Some(Appearance::Light),
                appearance,
                cx,
            ),
            SettingsRowId::TerminalDarkScheme => self.render_scheme_picker(
                row,
                SchemeKind::Terminal,
                Some(Appearance::Dark),
                appearance,
                cx,
            ),
            SettingsRowId::ChromeDensity => self.render_density(appearance, cx),
            SettingsRowId::ChromeFontFamily => self.render_chrome_font(appearance, cx),
            SettingsRowId::TerminalFontFamily => self.render_terminal_font(appearance, cx),
            SettingsRowId::ChromeBaseSize => self.render_chrome_size(appearance, cx),
            SettingsRowId::TerminalBaseSize => self.render_terminal_size(appearance, cx),
            SettingsRowId::TerminalLineHeight => self.render_line_height(appearance, cx),
            SettingsRowId::ChromeRegularWeight
            | SettingsRowId::ChromeEmphasisWeight
            | SettingsRowId::ChromeHeadingWeight
            | SettingsRowId::TerminalRegularWeight
            | SettingsRowId::TerminalBoldWeight => self.render_weight(row, appearance, cx),
            SettingsRowId::TerminalItalic => self.render_italic(cx),
            SettingsRowId::TerminalBoldAsBright => self.render_bold_as_bright(cx),
            SettingsRowId::InterfaceSchemes => {
                self.render_installed_schemes(SchemeKind::Chrome, appearance, cx)
            }
            SettingsRowId::TerminalSchemes => {
                self.render_installed_schemes(SchemeKind::Terminal, appearance, cx)
            }
            SettingsRowId::SchemeInterchange => self.render_scheme_interchange(appearance, cx),
            SettingsRowId::MicrophoneAccess => self.render_microphone_access(appearance, cx),
        }
    }

    /// One mode chooses the light or dark slot for both independent scheme engines.
    fn render_appearance_mode(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().preferences.mode;
        let selector = "settings-appearance-mode";
        let edge = crate::ui::appearance::chrome(cx).colors.border;
        let owner = cx.weak_entity();
        let options = [
            AppearanceMode::Light,
            AppearanceMode::Dark,
            AppearanceMode::Auto,
        ]
        .into_iter()
        .map(|mode| {
            let palettes = self.mode_preview_palettes(mode);
            let label = appearance_mode_label(mode);
            SegmentedOption::new(mode, label)
                .debug_selector(format!("{selector}-{}", label.to_ascii_lowercase()))
                .preview(move |_, extent| mode_preview(&palettes, edge, extent).into_any_element())
        })
        .collect::<Vec<_>>();
        SegmentedControl::new(selector, "Appearance", &current, options)
            .expect("three appearance modes are within the bounded option set")
            .size(SegmentedSize::Card)
            .disabled(!self.editor.editable())
            .debug_selector(selector)
            .on_change(move |change, _, cx| {
                let mode = *change.requested();
                let _ = owner.update(cx, |settings, cx| settings.set_appearance_mode(mode, cx));
            })
            .into_any_element()
    }

    /// The miniature window previews the Interface scheme in each persistent slot.
    ///
    /// Auto shows both slots side by side, light leading, so it never reads as a second Light.
    fn mode_preview_palettes(&self, mode: AppearanceMode) -> Vec<Vec<Color>> {
        let slots = &self.editor.document().preferences.chrome.schemes;
        let summaries = self.editor.scheme_summaries(SchemeKind::Chrome).ok();
        let pick = |appearance| {
            summaries
                .as_ref()
                .and_then(|summaries| {
                    summaries
                        .iter()
                        .find(|summary| summary.id == *slots.get(appearance))
                })
                .map(|summary| summary.swatches.clone())
                .unwrap_or_default()
        };
        match mode {
            AppearanceMode::Light => vec![pick(Appearance::Light)],
            AppearanceMode::Dark => vec![pick(Appearance::Dark)],
            AppearanceMode::Auto => vec![pick(Appearance::Light), pick(Appearance::Dark)],
        }
    }

    fn set_appearance_mode(&mut self, mode: AppearanceMode, cx: &mut Context<Self>) {
        self.edit(move |draft| draft.preferences.mode = mode, cx);
    }

    fn render_scheme_picker(
        &mut self,
        row: SettingsRowId,
        kind: SchemeKind,
        slot: Option<Appearance>,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preferences = &self.editor.document().preferences;
        let schemes = match kind {
            SchemeKind::Chrome => &preferences.chrome.schemes,
            SchemeKind::Terminal => &preferences.terminal.schemes,
        };
        let restrict = slot.unwrap_or_else(|| self.fixed_appearance());
        let current = schemes.get(restrict).clone();
        let summaries = self
            .editor
            .scheme_summaries(kind)
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| summary.appearance == restrict)
            .collect::<Vec<_>>();
        // The control carries its own selector so it never collides with its row's.
        let selector = control_selector(row);
        let mut items = summaries
            .iter()
            .map(|summary| {
                ComboBoxItem::new(summary.id.clone(), SharedString::from(summary.name.clone()))
                    .debug_selector(format!("{selector}-{}", summary.id.as_str()))
            })
            .collect::<Vec<_>>();
        retain_selected_item(
            &mut items,
            ComboBoxItem::new(
                current.clone(),
                SharedString::from(format!("{current} (Unavailable)")),
            )
            .debug_selector(format!("{selector}-{}", current.as_str())),
        );
        let owner = cx.weak_entity();
        settings_selector(
            selector,
            format!("{} color scheme", row.descriptor().label),
            Some(current),
            "Choose a color scheme",
            items,
            appearance,
        )
        .disabled(!self.editor.editable())
        .on_accept(move |acceptance, _, cx| {
            let Some(id) = Some(acceptance.item_id().clone()) else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| {
                settings.set_scheme(kind, restrict, id, cx);
            });
        })
        .into_any_element()
    }

    fn set_scheme(
        &mut self,
        kind: SchemeKind,
        slot: Appearance,
        id: SchemeId,
        cx: &mut Context<Self>,
    ) {
        self.edit(
            move |draft| {
                let schemes = match kind {
                    SchemeKind::Chrome => &mut draft.preferences.chrome.schemes,
                    SchemeKind::Terminal => &mut draft.preferences.terminal.schemes,
                };
                schemes.set(slot, id);
            },
            cx,
        );
    }

    fn render_density(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = appearance;
        let current = self.editor.document().preferences.chrome.density;
        let owner = cx.weak_entity();
        SegmentedControl::new(
            "settings-chrome-density",
            "Density",
            &current,
            vec![
                SegmentedOption::new(ChromeDensity::Compact, "Compact")
                    .debug_selector("settings-chrome-density-compact"),
                SegmentedOption::new(ChromeDensity::Comfortable, "Comfortable")
                    .debug_selector("settings-chrome-density-comfortable"),
            ],
        )
        .expect("two densities are within the bounded option set")
        .disabled(!self.editor.editable())
        .debug_selector("settings-chrome-density")
        .on_change(move |change, _, cx| {
            let density = *change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.preferences.chrome.density = density, cx);
            });
        })
        .into_any_element()
    }

    fn render_chrome_font(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fonts = crate::ui::appearance_runtime::available_fonts(cx);
        let current = match &self.editor.document().preferences.chrome.typography.family {
            ChromeFontFamily::SystemUi => None,
            ChromeFontFamily::Named { family } => Some(family.clone()),
        };
        let mut items =
            vec![ComboBoxItem::new(None, "System").debug_selector("settings-chrome-font-system")];
        items.extend(fonts.installed.iter().map(|font| {
            ComboBoxItem::new(
                Some(font.family.clone()),
                SharedString::from(font.family.clone()),
            )
        }));
        if let Some(family) = &current {
            retain_selected_item(
                &mut items,
                ComboBoxItem::new(
                    current.clone(),
                    SharedString::from(format!("{family} (Unavailable)")),
                )
                .debug_selector("settings-chrome-font-unavailable"),
            );
        }
        let owner = cx.weak_entity();
        settings_selector(
            "settings-chrome-font-family".to_owned(),
            "Interface font",
            Some(current),
            "Choose an interface font",
            items,
            appearance,
        )
        .disabled(!self.editor.editable())
        .on_accept(move |acceptance, _, cx| {
            let Some(choice) = Some(acceptance.item_id().clone()) else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        draft.preferences.chrome.typography.family = match choice {
                            Some(family) => ChromeFontFamily::Named { family },
                            None => ChromeFontFamily::SystemUi,
                        };
                    },
                    cx,
                );
            });
        })
        .into_any_element()
    }

    fn render_terminal_font(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fonts = crate::ui::appearance_runtime::available_fonts(cx);
        let current = match &self
            .editor
            .document()
            .preferences
            .terminal
            .typography
            .family
        {
            TerminalFontFamily::DefaultMonospace => None,
            TerminalFontFamily::Named { family } => Some(family.clone()),
        };
        let mut items = vec![
            ComboBoxItem::new(None, "Default monospace")
                .debug_selector("settings-terminal-font-default"),
        ];
        items.extend(
            terminal_font_families(&fonts)
                .into_iter()
                .map(|family| ComboBoxItem::new(Some(family.clone()), SharedString::from(family))),
        );
        if let Some(family) = &current {
            retain_selected_item(
                &mut items,
                ComboBoxItem::new(
                    current.clone(),
                    SharedString::from(format!("{family} (Unavailable)")),
                )
                .debug_selector("settings-terminal-font-unavailable"),
            );
        }
        let owner = cx.weak_entity();
        settings_selector(
            "settings-terminal-font-family".to_owned(),
            "Terminal font",
            Some(current),
            "Choose a monospace font",
            items,
            appearance,
        )
        .disabled(!self.editor.editable())
        .on_accept(move |acceptance, _, cx| {
            let Some(choice) = Some(acceptance.item_id().clone()) else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        draft.preferences.terminal.typography.family = match choice {
                            Some(family) => TerminalFontFamily::Named { family },
                            None => TerminalFontFamily::DefaultMonospace,
                        };
                    },
                    cx,
                );
            });
        })
        .into_any_element()
    }

    fn render_transparency(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.editor.document().preferences.background.transparency;
        let owner = cx.weak_entity();
        Stepper::new(
            "settings-transparency",
            "background transparency",
            format!("{value:.2}"),
        )
        .bounds(value > 0.0, value < 1.0)
        .enabled(self.editor.editable())
        .on_step(move |delta, _, cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let value = &mut draft.preferences.background.transparency;
                        *value = (((*value * 20.0).round() + delta as f32) / 20.0).clamp(0.0, 1.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance, cx)
        .into_any_element()
    }

    fn render_background_blur(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let value = self.editor.document().preferences.background.blur;
        let owner = cx.weak_entity();
        Switch::new("settings-background-blur", "Blur background", value)
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .disabled(!self.editor.editable())
            .debug_selector("settings-background-blur")
            .on_change(move |change, _, cx| {
                let blur = change.requested();
                let _ = owner.update(cx, |settings, cx| {
                    settings.edit(move |draft| draft.preferences.background.blur = blur, cx);
                });
            })
            .into_any_element()
    }

    fn render_chrome_size(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self
            .editor
            .document()
            .preferences
            .chrome
            .typography
            .base_size;
        let owner = cx.weak_entity();
        Stepper::new(
            "settings-chrome-base-size",
            "interface font size",
            format!("{value:.0} pt"),
        )
        .bounds(value > 10.0, value < 24.0)
        .enabled(self.editor.editable())
        .on_step(move |delta, _, cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let size = &mut draft.preferences.chrome.typography.base_size;
                        *size = (*size + f32::from(delta as i16)).clamp(10.0, 24.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance, cx)
        .into_any_element()
    }

    fn render_terminal_size(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self
            .editor
            .document()
            .preferences
            .terminal
            .typography
            .base_size;
        let owner = cx.weak_entity();
        Stepper::new(
            "settings-terminal-base-size",
            "terminal font size",
            format!("{value:.0} pt"),
        )
        .bounds(value > 8.0, value < 32.0)
        .enabled(self.editor.editable())
        .on_step(move |delta, _, cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let size = &mut draft.preferences.terminal.typography.base_size;
                        *size = (*size + f32::from(delta as i16)).clamp(8.0, 32.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance, cx)
        .into_any_element()
    }

    fn render_line_height(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const STEP: f32 = 0.05;
        let value = self
            .editor
            .document()
            .preferences
            .terminal
            .typography
            .line_height;
        let owner = cx.weak_entity();
        Stepper::new(
            "settings-terminal-line-height",
            "line height",
            format!("{value:.2}×"),
        )
        .bounds(value > 1.0, value < 2.0)
        .enabled(self.editor.editable())
        .on_step(move |delta, _, cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let height = &mut draft.preferences.terminal.typography.line_height;
                        // Round to the step so repeated presses cannot drift off the grid.
                        let stepped = (*height / STEP).round() + f32::from(delta as i16);
                        *height = (stepped * STEP).clamp(1.0, 2.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance, cx)
        .into_any_element()
    }

    /// Renders one font-weight row.
    ///
    /// Weight uses the same selector family as the scheme and font rows. One dropdown family for
    /// every "choose one" row keeps a single form from presenting two different control shapes.
    fn render_weight(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let typography = &self.editor.document().preferences;
        let current = match row {
            SettingsRowId::ChromeRegularWeight => typography.chrome.typography.regular_weight,
            SettingsRowId::ChromeEmphasisWeight => typography.chrome.typography.emphasis_weight,
            SettingsRowId::ChromeHeadingWeight => typography.chrome.typography.heading_weight,
            SettingsRowId::TerminalRegularWeight => typography.terminal.typography.regular_weight,
            _ => typography.terminal.typography.bold_weight,
        };
        let selector = control_selector(row);
        let mut items = WEIGHTS
            .iter()
            .map(|(weight, label)| {
                ComboBoxItem::new(*weight, *label).debug_selector(format!("{selector}-{weight}"))
            })
            .collect::<Vec<_>>();
        retain_selected_item(
            &mut items,
            ComboBoxItem::new(current, SharedString::from(format!("Custom ({current})")))
                .debug_selector(format!("{selector}-{current}")),
        );
        let owner = cx.weak_entity();
        settings_selector(
            selector,
            row.descriptor().label,
            Some(current),
            "Choose a weight",
            items,
            appearance,
        )
        .disabled(!self.editor.editable())
        .on_accept(move |acceptance, _, cx| {
            let weight = *acceptance.item_id();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let preferences = &mut draft.preferences;
                        match row {
                            SettingsRowId::ChromeRegularWeight => {
                                preferences.chrome.typography.regular_weight = weight
                            }
                            SettingsRowId::ChromeEmphasisWeight => {
                                preferences.chrome.typography.emphasis_weight = weight
                            }
                            SettingsRowId::ChromeHeadingWeight => {
                                preferences.chrome.typography.heading_weight = weight
                            }
                            SettingsRowId::TerminalRegularWeight => {
                                preferences.terminal.typography.regular_weight = weight
                            }
                            _ => preferences.terminal.typography.bold_weight = weight,
                        }
                    },
                    cx,
                );
            });
        })
        .into_any_element()
    }

    fn render_italic(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let value = self
            .editor
            .document()
            .preferences
            .terminal
            .typography
            .italic;
        let owner = cx.weak_entity();
        Switch::new("settings-terminal-italic", "Italic text", value)
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .disabled(!self.editor.editable())
            .debug_selector("settings-terminal-italic")
            .on_change(move |change, _, cx| {
                let italic = change.requested();
                let _ = owner.update(cx, |settings, cx| {
                    settings.edit(
                        move |draft| draft.preferences.terminal.typography.italic = italic,
                        cx,
                    );
                });
            })
            .into_any_element()
    }

    fn render_bold_as_bright(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let value = self
            .editor
            .document()
            .preferences
            .terminal
            .rendering
            .bold_as_bright;
        let owner = cx.weak_entity();
        Switch::new(
            "settings-terminal-bold-as-bright",
            "Show bold text in bright colors",
            value,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!self.editor.editable())
        .debug_selector("settings-terminal-bold-as-bright")
        .on_change(move |change, _, cx| {
            let bold_as_bright = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        draft.preferences.terminal.rendering.bold_as_bright = bold_as_bright;
                    },
                    cx,
                );
            });
        })
        .into_any_element()
    }

    fn render_banner(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let status = self.editor.status();
        let explanation = status.explanation()?;
        let critical = matches!(status, SaveStatus::Unavailable(_));
        let owner = cx.weak_entity();
        let (label, action_selector) = if critical {
            ("Reload", "settings-banner-reload")
        } else {
            ("Retry", "settings-banner-retry")
        };
        Some(
            div()
                .debug_selector(|| "settings-banner".to_owned())
                .flex()
                .flex_row()
                .items_start()
                .gap(appearance.spacing(8.0))
                // The same inset notice the Color Schemes page carries, at the window's own scope
                // rather than the page's. A strip ruled off across the pane would be the one square
                // edge left on a surface made of cards.
                .mx(appearance.spacing(CARD_GUTTER))
                .mt(appearance.spacing(12.0))
                .p(appearance.spacing(10.0))
                .rounded(appearance.spacing(CARD_RADIUS))
                .bg(gpui_color(appearance.surface(
                    crate::appearance::SurfaceRole::Surface,
                    if critical {
                        appearance.colors.warning_background
                    } else {
                        appearance.colors.error_background
                    },
                )))
                .text_color(gpui_color(if critical {
                    appearance.colors.warning
                } else {
                    appearance.colors.error
                }))
                .border_1()
                .border_color(gpui_color(if critical {
                    appearance.colors.warning_border
                } else {
                    appearance.colors.error_border
                }))
                .child(div().flex_none().mt(px(1.0)).child(Icon::new(
                    IconName::TriangleAlert,
                    appearance.text_size(13.0),
                    gpui_color(if critical {
                        appearance.colors.warning
                    } else {
                        appearance.colors.error
                    }),
                )))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .gap(appearance.spacing(2.0))
                        .child(
                            div()
                                .font(appearance.emphasis.clone())
                                .child(status.message()),
                        )
                        .child(
                            div()
                                .text_size(appearance.text_size(text::SMALL))
                                .whitespace_normal()
                                .child(explanation),
                        ),
                )
                .child(action_button(action_selector, label, true, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        if critical {
                            settings.editor.reload(cx);
                        } else {
                            settings.editor.retry(cx);
                        }
                    });
                }))
                .into_any_element(),
        )
    }

    /// The window's own strip: what the last edit did, and the one action that undoes all of them.
    ///
    /// Reset All acts on the content column, not on navigation, so it sits in the content column
    /// opposite the save status. It takes the width of its label: the sidebar is the only column
    /// whose width means anything here, and a destructive action stretched to it reads as the
    /// heaviest thing in the window. One rule runs the full width, and the sidebar surface
    /// continues beneath it to the window's bottom edge.
    ///
    /// The strip is a status line rather than a form, and the only other thing on it is the save
    /// status, so the action rests at that status's weight: Bare paints no surface and carries the
    /// muted foreground, and the compact size sets it at the same step of the ramp the status
    /// takes. Pointer and keyboard lift it to full text, so the affordance arrives on approach
    /// rather than competing at rest with the settings it would undo.
    ///
    /// Both ends align to the content gutter, which is the line the title, the group headings, and
    /// every row label already sit on. A card's edge stands outside that line by the inset its rows
    /// carry, but a card is a container rather than something to read: the column a reader tracks
    /// down the page is the text, so the strip that closes the column joins the text.
    ///
    /// On the leading end that alignment is carried by the action's label, not its edge. A button
    /// insets its label by the padding it paints and by the border it reserves whether or not the
    /// variant paints one, and only the padding follows the density scale, so the strip removes
    /// each in its own scale. Bare paints no edge for the eye to align to, so the label is the only
    /// thing left on that line. The save status is plain text and needs no such correction.
    fn render_footer(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = self.editor.status();
        let owner = cx.weak_entity();
        div()
            .debug_selector(|| "settings-footer".to_owned())
            .flex()
            .flex_row()
            .w_full()
            .flex_none()
            .h(appearance.height(FOOTER_HEIGHT, text::BODY))
            .border_t_1()
            .border_color(gpui_color(appearance.colors.border))
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .w(appearance.text_size(SIDEBAR_WIDTH))
                    .bg(gpui_color(appearance.control_colors.panel_background)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(gpui_color(appearance.surface(
                        crate::appearance::SurfaceRole::Base,
                        appearance.colors.background,
                    )))
                    .items_center()
                    .justify_between()
                    .gap(appearance.spacing(CONTENT_GUTTER))
                    .pl(appearance
                        .spacing(CONTENT_GUTTER - super::button_theme::COMPACT_HORIZONTAL_PADDING)
                        - px(super::button_theme::CONTROL_BORDER_WIDTH))
                    .pr(appearance.spacing(CONTENT_GUTTER))
                    .child(
                        action_button(
                            "settings-reset-all",
                            "Reset All…",
                            self.editor.editable(),
                            move |window, cx| {
                                let _ = owner.update(cx, |settings, cx| {
                                    settings.confirm_reset_all(window, cx);
                                });
                            },
                        )
                        .variant(spaceterm_ui::ButtonVariant::Bare)
                        .size(spaceterm_ui::ButtonSize::Compact),
                    )
                    .child(
                        div()
                            .debug_selector(|| "settings-save-status".to_owned())
                            .min_w_0()
                            .truncate()
                            .text_size(appearance.text_size(text::SMALL))
                            // The recovery banner owns semantic emphasis on its paired surface.
                            .text_color(gpui_color(appearance.colors.text_muted))
                            .child(status.message()),
                    ),
            )
            .into_any_element()
    }

    fn confirm_reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-reset-all"),
            "Reset all settings",
            "Reset All Settings",
            "Every setting returns to its default, and the color schemes you imported are removed.",
            vec![
                ModalAction::new(
                    true,
                    "Reset",
                    ModalActionRole::Affirmative,
                    "settings-reset-all-confirm",
                )
                .with_intent(ModalActionIntent::Destructive)
                .with_emphasis(ModalActionEmphasis::Prominent),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "settings-reset-all-cancel",
                ),
            ],
        )
        .intent(AlertIntent::Critical)
        // An imported scheme is the one thing here the reset cannot give back, so the alert says
        // so and names the action that would keep it rather than leaving that to be discovered.
        .detail(
            "This cannot be undone. Export any imported scheme you want to keep first. \
             Microphone access is a system permission and is not affected.",
        )
        .present(window, cx, move |outcome, cx| {
            if !matches!(
                outcome,
                spaceterm_ui::AlertOutcome::Activated {
                    action_id: true,
                    ..
                }
            ) {
                return;
            }
            let _ = owner.update(cx, |settings, cx| {
                settings.editor.reset_all(cx);
                cx.notify();
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings reset confirmation");
        }
    }
}

/// A miniature scheme preview for one appearance-mode card.
/// A miniature SpaceTerm window for each palette, split evenly when there is more than one.
///
/// Each part clips one full-size miniature from its own side, so a split reads as one window
/// crossing from light to dark. A hairline edge keeps a light miniature visible on a light surface.
fn mode_preview(palettes: &[Vec<Color>], edge: Color, extent: gpui::Pixels) -> impl IntoElement {
    let width = extent * 1.5;
    let last = palettes.len().saturating_sub(1);
    div()
        .flex()
        .flex_row()
        .w(width)
        .h(extent)
        .rounded(px(4.0))
        .overflow_hidden()
        .border_1()
        .border_color(gpui_color(edge))
        .children(palettes.iter().enumerate().map(|(index, swatches)| {
            let miniature = mode_miniature(swatches, width, extent);
            div().relative().flex_1().h_full().overflow_hidden().child(
                if index == last && index > 0 {
                    miniature.absolute().top_0().right_0()
                } else {
                    miniature.absolute().top_0().left_0()
                },
            )
        }))
}

fn mode_miniature(swatches: &[Color], width: gpui::Pixels, extent: gpui::Pixels) -> gpui::Div {
    let background = swatches.first().copied().unwrap_or(Color::rgb(0x000000));
    let bar = swatches.get(1).copied().unwrap_or(Color::rgb(0xffffff));
    let accent = swatches.get(3).copied().unwrap_or(bar);
    div()
        .w(width)
        .h(extent)
        .bg(gpui_color(background))
        .flex()
        .flex_col()
        .child(
            div()
                .w_full()
                .h(extent * 0.22)
                .bg(gpui_color(bar))
                .opacity(0.35),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(2.0))
                .p(px(3.0))
                .child(div().w(extent * 0.8).h(px(2.0)).bg(gpui_color(bar)))
                .child(div().w(extent * 0.55).h(px(2.0)).bg(gpui_color(accent)))
                .child(div().w(extent * 0.65).h(px(2.0)).bg(gpui_color(bar))),
        )
}

/// The families the terminal font list offers.
///
/// Only monospaced families are offered: a proportional terminal font resolves to a fallback and
/// reports a diagnostic, so it is not a choice worth presenting in the first place.
fn terminal_font_families(fonts: &crate::appearance::AvailableFonts) -> Vec<String> {
    fonts
        .installed
        .iter()
        .filter(|font| font.class == FontClass::Monospace)
        .map(|font| font.family.clone())
        .collect()
}

/// Keeps a retained preference visible even when the available choices no longer include it.
fn retain_selected_item<I: Eq>(items: &mut Vec<ComboBoxItem<I>>, current: ComboBoxItem<I>) {
    if !items.iter().any(|item| item.id() == current.id()) {
        items.insert(0, current);
    }
}

/// One settings selector, decorated the same way wherever the form offers a choice.
///
/// Every selector filters a list, so its popup carries the same search glyph as the window's own
/// search field, and the control marks the current value in its list.
fn settings_selector<I: Clone + Eq + 'static>(
    selector: String,
    accessibility_name: impl Into<SharedString>,
    selected: Option<I>,
    prompt: &'static str,
    items: Vec<ComboBoxItem<I>>,
    appearance: &ChromeAppearance,
) -> ComboBox<I> {
    let glyph = gpui_color(appearance.colors.icon_muted);
    ComboBox::new(
        SharedString::from(selector.clone()),
        accessibility_name,
        selected,
        prompt,
        items,
    )
    // The trigger takes the width of the value it shows, so the value and its chevron stay
    // together at the row's right edge, and it carries a bezel so it ends where the steppers and
    // segmented controls beside it end rather than optically short of them.
    .hug(true)
    .bezel(true)
    .input_leading(move |size| Icon::new(IconName::Search, size, glyph).into_any_element())
    .debug_selector(selector)
}

/// The selector of the control inside one row.
///
/// A row and the control it holds are separate elements, so they carry separate selectors and a
/// test can address either one.
fn control_selector(row: SettingsRowId) -> String {
    format!("{}-control", row.descriptor().selector)
}

/// Where a row's label sits.
///
/// The Color Schemes rows present a list and a button group rather than one control, and each is
/// the only row in its box, so the group's own title names them and the content spans the row.
fn row_layout(row: SettingsRowId) -> SettingsRowLayout {
    match row {
        SettingsRowId::InterfaceSchemes
        | SettingsRowId::TerminalSchemes
        | SettingsRowId::SchemeInterchange => SettingsRowLayout::Full,
        _ => SettingsRowLayout::Beside,
    }
}

impl SettingsWindow {
    /// One line of guidance for the rows that warrant it.
    ///
    /// Microphone access explains its current status, so the guidance follows the system.
    fn row_description(&self, row: SettingsRowId) -> Option<&'static str> {
        match row {
            SettingsRowId::AppearanceMode => Some("Auto matches the system light or dark setting."),
            SettingsRowId::Transparency => Some("0 is opaque. 1 is maximum transparency."),
            SettingsRowId::BackgroundBlur => {
                Some("Soften the desktop behind transparent backgrounds.")
            }
            SettingsRowId::TerminalFontFamily => Some("Only monospaced families are listed."),
            SettingsRowId::MicrophoneAccess => {
                Some(self.microphone_access.presentation().explanation)
            }
            _ => None,
        }
    }
}

/// The selector of one titled box, so a test can address a group rather than only its rows.
fn group_selector(section: SettingsSectionId, title: &str) -> String {
    let slug = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("{}-group-{slug}", section.selector())
}
