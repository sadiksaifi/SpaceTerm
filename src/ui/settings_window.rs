//! The Settings Window: a separate, modeless Operating-System Window for SpaceTerm Settings.
//! It is application scoped so it stays reachable when no Workspace window exists.

mod advanced;
mod catalog;
mod clipboard;
mod editor;
mod import;
mod keybindings;
mod microphone;
mod permission_access;
mod theme_gallery;
mod theme_store;
mod themes;
mod updates;

#[cfg(test)]
mod control_tests;

#[cfg(test)]
mod updates_tests;

#[cfg(test)]
#[path = "settings_window/tests.rs"]
mod tests;

use crate::ui::appearance::gpui_color;
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Entity, FocusHandle, Global, Pixels, ScrollHandle,
    SharedString, Window, WindowHandle, actions, div, px, size,
};
use spaceterm_ui::{
    Alert, AlertIntent, ComboBox, ComboBoxItem, Icon, IconName, ModalAction, ModalActionEmphasis,
    ModalActionIntent, ModalActionRole, ModalId, ModalLayer, OverlayScrollbar,
    OverlayScrollbarEvent, ScrollMetrics, SearchField, SegmentedControl, SegmentedOption,
    SegmentedSize, Switch, TextInput, TextInputEscapeBehavior, TextInputEvent,
    TextInputReturnBehavior, TextInputVariant, ToggleSize,
};

use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AvailableFonts, ChromeDensity, Color,
    FontClass, ResetTarget, SystemAppearance, TerminalFontFamily, ThemeCatalog, ThemeId,
    UnavailableWindowEffect, WindowBackgroundChoices,
};
use crate::desktop_profile::HostFeature;
use crate::platform::microphone_access::MicrophoneAccess;
use crate::platform::permission_access::{PermissionAccess, SystemPermission};
#[cfg(test)]
use crate::platform::window_movement::RecordingOperatingSystemWindowDragPlatform;
use crate::platform::window_movement::{OperatingSystemWindowDragPlatform, WindowMovementFactory};
use crate::settings::SettingsDocument;
use crate::theme_registry::ZedThemeRegistry;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::appearance::settings::{SettingsAppearance, SettingsSurfaceRole};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::{
    DetailHeading, NavigationEntry, Sidebar, SidebarNavigation, SidebarOwner, WindowMovement,
    card_gutter, group_spacing,
};

use crate::ui::sidebar_window::form::{
    FormGroup, FormRow, FormRowLayout, Stepper, action_button, reset_button, row_horizontal_inset,
    section_heading,
};
use catalog::{SettingsRowId, SettingsSectionId};
use editor::{SaveStatus, SettingsEditor};
use keybindings::ShortcutRows;
use microphone::MicrophoneAccessRow;
use permission_access::{PermissionAccessChanges, PermissionAccessRows};
use theme_gallery::ThemeGallery;
use theme_store::ThemeStore;

actions!(
    spaceterm,
    [
        OpenSettings,
        OpenKeyboardShortcuts,
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

/// The one Settings Window, so a second request activates the existing window.
struct OpenSettingsWindow(WindowHandle<SettingsWindow>);
impl Global for OpenSettingsWindow {}

/// The system permissions the Privacy section reads and recovers.
#[derive(Clone, Default)]
pub(crate) struct PermissionCapabilities {
    pub(crate) microphone: Option<Rc<dyn MicrophoneAccess>>,
    pub(crate) system_permissions: Option<Rc<dyn PermissionAccess>>,
    /// The application's Permission Setup, shared with every Pane.
    pub(crate) permission_setup: Option<Entity<super::permission_setup::PermissionSetup>>,
}

/// Host-owned capabilities needed by the Settings window's app-drawn titlebar and Privacy section.
struct SettingsWindowComposition {
    window_movement: Rc<dyn WindowMovementFactory>,
    permissions: PermissionCapabilities,
    theme_registry: Option<ZedThemeRegistry>,
}
impl Global for SettingsWindowComposition {}

pub(crate) fn configure_window_chrome(
    window_movement: Rc<dyn WindowMovementFactory>,
    permissions: PermissionCapabilities,
    theme_registry: Option<ZedThemeRegistry>,
    cx: &mut App,
) {
    cx.set_global(SettingsWindowComposition {
        window_movement,
        permissions,
        theme_registry,
    });
}

/// The open Settings Window. Membership, not a root-view read, decides this: the root view is
/// leased while an action dispatches inside Settings, so a read would miss the open window.
fn open_settings_window(cx: &App) -> Option<WindowHandle<SettingsWindow>> {
    let handle = cx.try_global::<OpenSettingsWindow>()?.0;
    cx.windows()
        .iter()
        .any(|window| window.window_id() == handle.window_id())
        .then_some(handle)
}

/// Opens Settings, or activates it when it is already open, showing `section` when one is given.
pub(crate) fn open_or_activate(section: Option<SettingsSectionId>, cx: &mut App) {
    if !cx.has_global::<crate::ui::appearance_runtime::AppearanceRuntime>() {
        // Settings edits the retained document through the appearance runtime. Without it there is
        // nothing to present, so declining is the honest outcome rather than an empty window.
        eprintln!("SpaceTerm Settings is unavailable because appearance is not installed");
        return;
    }
    if let Some(existing) = open_settings_window(cx) {
        cx.defer(move |cx| {
            let _ = existing.update(cx, |settings, window, cx| {
                if let Some(section) = section {
                    settings.select_section(section, cx);
                }
                window.activate_window();
            });
        });
        return;
    }
    let Some(composition) = cx.try_global::<SettingsWindowComposition>() else {
        eprintln!("SpaceTerm Settings is unavailable because window chrome is not installed");
        return;
    };
    let window_drag = composition.window_movement.create();
    let permissions = composition.permissions.clone();
    let theme_registry = composition.theme_registry.clone();
    let opened = cx.open_window(
        super::sidebar_window::window_options(
            "Settings",
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            cx,
        ),
        |window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new_with_capabilities(
                    Rc::clone(&window_drag),
                    permissions.clone(),
                    theme_registry.clone(),
                    window,
                    cx,
                );
                if let Some(section) = section {
                    settings.select_section(section, cx);
                }
                settings
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
        Err(_) => eprintln!("failed to open the SpaceTerm Settings window"),
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
        completion.saved(cx);
    }
}

#[derive(Clone)]
enum CloseIntent {
    Window(AnyWindowHandle),
    Application(crate::app::ApplicationQuitAfterSave),
}

/// Registers the application-scoped Settings actions.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| open_or_activate(None, cx));
    cx.on_action(|_: &OpenKeyboardShortcuts, cx| {
        open_or_activate(Some(SettingsSectionId::Keybindings), cx);
    });
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
    window_traffic_lights: super::appearance_runtime::WindowTrafficLightOwner,
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
    /// Where each row of the presented section was laid out in the latest frame, so keyboard focus
    /// reaching a row outside the viewport can scroll it into view.
    row_bounds: Rc<RefCell<HashMap<SettingsRowId, Bounds<Pixels>>>>,
    focus_handle: FocusHandle,
    navigation: SidebarNavigation,
    window_movement: WindowMovement,
    microphone_access: MicrophoneAccessRow,
    /// The sections and rows this desktop presents. A desktop omits only the surfaces of features
    /// it has no equivalent for; an unavailable feature keeps its rows to explain why.
    available_sections: Vec<SettingsSectionId>,
    omitted_rows: Vec<SettingsRowId>,
    permission_access: PermissionAccessRows,
    _permission_changes: Option<PermissionAccessChanges>,
    theme_gallery: ThemeGallery,
    /// The Get More Themes sheet, kept for the window's life so the registry is listed once.
    theme_store: Entity<ThemeStore>,
    /// One shortcut recorder per Command, kept so a recording survives re-rendering.
    shortcuts: ShortcutRows,
    /// The read-only settings file in the Advanced section.
    settings_file: advanced::SettingsFileView,
}

impl SidebarOwner for SettingsWindow {
    type Section = SettingsSectionId;

    fn navigation(&mut self) -> &mut SidebarNavigation {
        &mut self.navigation
    }

    fn active_section(&self) -> SettingsSectionId {
        self.active_section
    }

    /// The sections the current query left something to present.
    fn navigable_sections(&self) -> Vec<SettingsSectionId> {
        let matching = self.matching_rows();
        SettingsSectionId::ALL
            .into_iter()
            .filter(|section| {
                catalog::rows().any(|row| row.section == *section && matching.contains(&row.id))
            })
            .collect()
    }

    /// Each section is its own view, so the detail pane starts at its top.
    fn select_section(&mut self, section: SettingsSectionId, cx: &mut Context<Self>) {
        self.active_section = section;
        self.shortcuts.dismiss_notice();
        self.shortcuts.end_search_capture(cx);
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }
}

fn section_feature(section: SettingsSectionId) -> Option<HostFeature> {
    (section == SettingsSectionId::Updates).then_some(HostFeature::Updates)
}

/// The host feature one row presents, if any.
fn row_feature(row: SettingsRowId) -> Option<HostFeature> {
    if let Some(feature) = section_feature(row.descriptor().section) {
        Some(feature)
    } else if row == SettingsRowId::MicrophoneAccess {
        Some(HostFeature::MicrophoneAccess)
    } else {
        permission_access::row_permission(row).map(|_| HostFeature::SystemPermissions)
    }
}

/// How one row returns to its default.
#[derive(Clone, Debug)]
enum RowReset {
    Appearance(ResetTarget),
    Update,
    Clipboard,
    Shortcut(crate::keybindings::Command),
}

impl SettingsWindow {
    #[cfg(test)]
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            PermissionCapabilities::default(),
            None,
            window,
            cx,
        )
    }

    fn new_with_capabilities(
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        permissions: PermissionCapabilities,
        theme_registry: Option<ZedThemeRegistry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        super::appearance_runtime::complete_font_catalog(cx);
        // Transparent native chrome hides this visually while retaining a stable Operating-System
        // window identity for the Window menu and accessibility clients.
        window.set_window_title("Settings");
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        let mut window_traffic_lights =
            super::appearance_runtime::WindowTrafficLightOwner::sidebar_window();
        window_traffic_lights.apply(window, cx);
        let settings = cx
            .global::<crate::ui::appearance_runtime::AppearanceRuntime>()
            .settings
            .clone();
        // Settings Recovery can run from the launch prompt while this window is open, and a reset
        // to defaults may leave the installed appearance unchanged, so follow the owner directly.
        let changes = settings.subscribe();
        cx.spawn(async move |settings, cx| {
            while changes.recv().await.is_ok() {
                let followed = settings.update(cx, |settings, cx| {
                    settings.editor.synchronize();
                    cx.notify();
                });
                if followed.is_err() {
                    break;
                }
            }
        })
        .detach();
        let editor = SettingsEditor::new(settings);
        // The window takes focus so its own shortcuts and Tab traversal resolve from the moment it
        // opens, rather than only after something inside it is clicked.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let navigation = SidebarNavigation::new(focus_handle.clone(), window, cx);
        let window_movement = WindowMovement::new(
            operating_system_window_drag_platform,
            focus_handle.clone(),
            "Settings",
        );
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
                    settings.navigation.show_focus();
                    if matches!(event, TextInputEvent::TabForwardRequested) {
                        window.focus_next(cx);
                    } else {
                        window.focus_prev(cx);
                    }
                    cx.notify();
                }
                if matches!(event, TextInputEvent::ValueChanged(_)) {
                    settings.query = SharedString::from(search.read(cx).value().to_owned());
                    settings.synchronize_search_results();
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
                settings.window_traffic_lights.apply(window, cx);
                settings.editor.synchronize();
                cx.notify();
            },
        )
        .detach();
        // Authorization can change in System Settings while this window is in the background.
        // Leaving the window also ends searching by Shortcut, so chords pressed on return are not
        // captured.
        cx.observe_window_activation(window, |settings, window, cx| {
            if window.is_window_active() {
                settings.refresh_microphone_access(cx);
                settings.refresh_permission_access(cx);
            } else {
                settings.shortcuts.end_search_capture(cx);
            }
            cx.notify();
        })
        .detach();
        // The Updates section renders the application's update state as it changes.
        if let Some(service) = cx.try_global::<crate::updates::UpdateService>() {
            cx.observe(&service.0.clone(), |_, _, cx| cx.notify())
                .detach();
        }
        cx.on_app_quit(|settings, cx| {
            if !settings.editor.flush_for_shutdown(cx) {
                eprintln!("SpaceTerm Settings could not be saved during shutdown");
            }
            async {}
        })
        .detach();
        let theme_gallery = ThemeGallery::new(window, cx);
        let owner = cx.weak_entity();
        let theme_store = cx.new(|cx| ThemeStore::new(owner, theme_registry, window, cx));
        let shortcuts = ShortcutRows::new(window, cx);
        let settings_file = advanced::SettingsFileView::new(window, cx);
        let permission_changes =
            PermissionAccessChanges::observe(permissions.system_permissions.as_ref(), cx);
        // A Permission Setup reports its progress and the grant it finds while this window waits
        // in the background.
        let permission_setup = permissions.permission_setup.clone();
        if let Some(setup) = &permission_setup {
            cx.observe(setup, |settings, _, cx| {
                settings.permission_access.synchronize_setup(cx);
                cx.notify();
            })
            .detach();
        }
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let available_sections = SettingsSectionId::ALL
            .into_iter()
            .filter(|section| {
                section_feature(*section).is_none_or(|feature| presentation.has_feature(feature))
            })
            .collect();
        let omitted_rows = catalog::rows()
            .map(|row| row.id)
            .filter(|row| {
                row_feature(*row).is_some_and(|feature| !presentation.has_feature(feature))
            })
            .collect();
        Self {
            window_appearance,
            window_traffic_lights,
            editor,
            close_after_save: None,
            search,
            query: SharedString::default(),
            scroll: ScrollHandle::new(),
            scrollbar,
            active_section: SettingsSectionId::Interface,
            revealed: None,
            row_bounds: Rc::default(),
            focus_handle,
            navigation,
            window_movement,
            available_sections,
            omitted_rows,
            microphone_access: MicrophoneAccessRow::new(permissions.microphone),
            permission_access: PermissionAccessRows::new(
                permissions.system_permissions,
                permission_setup,
                crate::application_identity::ApplicationIdentity::current().display_name(),
                cx,
            ),
            _permission_changes: permission_changes,
            theme_gallery,
            theme_store,
            shortcuts,
            settings_file,
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
                CloseIntent::Application(completion) => completion.saved(cx),
            }
        } else if !self.editor.is_writing() {
            // Keep the draft and the existing Retry/Reload feedback instead of discarding a failed
            // save. A competing preview is reported once rather than spinning during close.
            if let Some(CloseIntent::Application(completion)) = self.close_after_save.take() {
                completion.failed(cx);
            } else {
                self.close_after_save = None;
            }
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
        handle.focus(window, cx);
        cx.notify();
    }

    fn clear_search(
        &mut self,
        _: &ClearSettingsSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query.is_empty() {
            if self.search.read(cx).is_focused() {
                self.focus_handle.focus(window, cx);
            }
            return;
        }
        self.search.update(cx, |search, cx| {
            search.set_value(String::new(), cx);
        });
        self.query = SharedString::default();
        self.revealed = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Scrolls the detail pane the least distance that shows the whole row, for keyboard focus that
    /// reached a row outside the viewport. A row taller than the viewport shows its top.
    fn scroll_row_into_view(&mut self, row: SettingsRowId, cx: &mut Context<Self>) {
        let Some(bounds) = self.row_bounds.borrow().get(&row).copied() else {
            return;
        };
        let viewport = self.scroll.bounds();
        let shift = if bounds.top() < viewport.top() || bounds.size.height > viewport.size.height {
            viewport.top() - bounds.top()
        } else if bounds.bottom() > viewport.bottom() {
            viewport.bottom() - bounds.bottom()
        } else {
            return;
        };
        let offset = self.scroll.offset();
        let lowest = -self.scroll.max_offset().y;
        self.scroll.set_offset(gpui::point(
            offset.x,
            (offset.y + shift).clamp(lowest, px(0.0)),
        ));
        cx.notify();
    }

    fn matching_rows(&self) -> Vec<SettingsRowId> {
        catalog::matching_rows(&self.query, self.permission_access.naming())
            .into_iter()
            .filter(|row| {
                self.available_sections.contains(&row.descriptor().section)
                    && !self.omitted_rows.contains(row)
            })
            .collect()
    }

    fn rows_for(&self, section: SettingsSectionId) -> Vec<SettingsRowId> {
        self.matching_rows()
            .into_iter()
            .filter(|row| row.descriptor().section == section)
            .collect()
    }

    fn synchronize_search_results(&mut self) {
        let matching = self.matching_rows();
        self.revealed = if self.query.trim().is_empty() {
            None
        } else {
            matching.first().copied()
        };
        // Each section is its own view, so a query that the visible one cannot answer moves to the
        // first section that can.
        if !matching
            .iter()
            .any(|row| row.descriptor().section == self.active_section)
            && let Some(row) = matching.first()
        {
            self.active_section = row.descriptor().section;
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        }
    }

    fn fixed_appearance(&self) -> Appearance {
        self.editor
            .document()
            .appearance
            .mode
            .resolve(Appearance::Dark)
    }

    fn edit(&mut self, edit: impl FnOnce(&mut SettingsDocument), cx: &mut Context<Self>) {
        self.editor.edit(edit, cx);
    }

    /// How this row returns to its default, when it differs from it.
    fn pending_reset(&self, row: SettingsRowId, cx: &App) -> Option<RowReset> {
        if let SettingsRowId::Shortcut(command) = row {
            return self
                .shortcut_differs(command, cx)
                .then_some(RowReset::Shortcut(command));
        }
        if let Some(differs) = self.update_preference_differs(row) {
            return differs.then_some(RowReset::Update);
        }
        if let Some(differs) = self.clipboard_preference_differs(row) {
            return differs.then_some(RowReset::Clipboard);
        }
        if matches!(row, SettingsRowId::Opacity | SettingsRowId::Blur)
            && !self.window_background(cx).adjustable()
        {
            // The row already shows its default and cannot change, so there is nothing to offer.
            return None;
        }
        let target = row.reset_target(self.fixed_appearance())?;
        // Only preferences are copied, because cloning the document every frame would copy the
        // installed theme catalog.
        let current = &self.editor.document().appearance;
        let mut reset = current.clone();
        reset.reset(target.clone());
        (reset != *current).then_some(RowReset::Appearance(target))
    }

    fn row_reset(
        &self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.editor.editable() {
            return None;
        }
        let reset = self.pending_reset(row, cx)?;
        let owner = cx.weak_entity();
        Some(
            reset_button(
                format!("{}-reset", row.descriptor().selector),
                row.descriptor().label(self.permission_access.naming()),
                appearance
                    .icons
                    .mark_metrics(crate::ui::chrome_icons::MarkRole::Reset)
                    .glyph_size,
                true,
                move |_, cx| {
                    let reset = reset.clone();
                    let _ = owner.update(cx, |settings, cx| match reset {
                        RowReset::Appearance(target) => settings.editor.reset(target, cx),
                        RowReset::Update => settings.reset_update_preference(row, cx),
                        RowReset::Clipboard => settings.reset_clipboard_preference(row, cx),
                        RowReset::Shortcut(command) => settings.reset_shortcut(command, cx),
                    });
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

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = super::appearance::window_activity(window);
        super::sidebar_window::render_scoped(activity, || self.render_chrome(window, cx))
    }
}

impl SettingsWindow {
    fn render_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let settings = crate::ui::appearance::settings::shared(cx);
        let appearance = &settings.chrome;
        self.sync_scrollbar(cx);
        let surface = div()
            .debug_selector(|| "settings-window-surface".to_owned())
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::clear_search));
        let content = super::sidebar_window::tab_traversal(surface, cx)
            .size_full()
            .flex()
            .flex_row()
            .text_color(gpui_color(appearance.colors.text))
            .chrome_text(appearance.typography.style(TextRole::Body))
            // Both columns run to the window's top edge beneath the transparent native titlebar,
            // and the sidebar runs on to the bottom edge: the footer belongs to the content column.
            .child(self.render_sidebar(&settings, window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_detail(&settings, window, cx))
                    .child(self.render_footer(&settings)),
            );
        ModalLayer::new(super::window_shell::render(content, window, cx)).into_any_element()
    }
}

impl SettingsWindow {
    /// The active section's large title and description at the head of the content surface.
    fn render_detail_heading(
        &self,
        settings: &SettingsAppearance,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let section = self.active_section;
        let close_owner = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let handle = window.window_handle();
            let _ = close_owner.update(cx, |settings, cx| {
                settings.request_close(CloseIntent::Window(handle), cx)
            });
        });
        let scrolled = self.scroll.max_offset().y > px(0.0) && self.scroll.offset().y < px(-0.5);
        DetailHeading::new(
            "settings",
            section_heading(
                section.selector(),
                section.title(),
                section.description(crate::desktop_profile::DesktopPresentation::get(cx)),
                &settings.chrome,
            ),
            &self.window_movement,
            close,
        )
        .scrolled(scrolled)
        .render(settings, window, cx)
    }

    fn render_sidebar(
        &mut self,
        settings: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let available = self.navigable_sections();
        let entries = self
            .available_sections
            .iter()
            .copied()
            .map(|section| NavigationEntry {
                section,
                title: section.title(),
                selector: section.selector(),
                icon: match section {
                    SettingsSectionId::Interface => IconName::AppWindow,
                    SettingsSectionId::Font => IconName::Type,
                    SettingsSectionId::Themes => IconName::Palette,
                    SettingsSectionId::Keybindings => IconName::Keyboard,
                    SettingsSectionId::Privacy => IconName::Shield,
                    SettingsSectionId::Updates => IconName::Download,
                    SettingsSectionId::Advanced => IconName::Cog,
                },
                available: available.contains(&section),
            })
            .collect();
        let movement = self.window_movement.clone();
        let closing = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let handle = window.window_handle();
            let _ = closing.update(cx, |settings, cx| {
                settings.request_close(CloseIntent::Window(handle), cx)
            });
        });
        Sidebar::new("settings", entries, &movement)
            .window_controls(close)
            .header(
                SearchField::new("settings-search-frame", self.search.clone())
                    .debug_selectors("settings-search-frame", "settings-search-clear"),
            )
            .footer(super::sidebar_window::render_footer_action(
                "settings-about".to_owned(),
                // The entry names the product as its Command does; the window names the build.
                crate::keybindings::Command::About.label().into(),
                IconName::Info,
                Box::new(crate::app::ShowAboutApplication),
                &settings.chrome,
            ))
            .render(self, settings, window, cx)
    }

    fn render_detail(
        &mut self,
        settings: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let appearance = &settings.chrome;
        // One section at a time: navigation selects a view rather than a scroll destination, so
        // nothing from a neighbouring section can scroll into this one.
        let section = self.render_section(self.active_section, settings, window, cx);
        let empty = self.rows_for(self.active_section).is_empty();
        let revealing = cx.weak_entity();
        div()
            .debug_selector(|| "settings-canvas".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(gpui_color(
                settings.surface(SettingsSurfaceRole::Canvas).paint,
            ))
            .child(self.render_detail_heading(settings, window, cx))
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
                            .px(card_gutter(appearance))
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
                                        .px(row_horizontal_inset(appearance))
                                        .text_color(gpui_color(appearance.colors.text_muted))
                                        .child(gpui::Text::new(
                                            "settings-no-results".into(),
                                            SharedString::from(format!(
                                                "No settings match “{}”.",
                                                self.query
                                            )),
                                        )),
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
        settings: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let appearance = &settings.chrome;
        let mut rows = self.rows_for(section);
        if rows.is_empty() {
            return div()
                .debug_selector(move || format!("{}-empty", section.selector()))
                .into_any_element();
        }
        // The Keybindings search narrows the rows Settings Search left, so it stays in place
        // above them even when it finds nothing.
        let (shortcut_search, no_shortcuts) = if section == SettingsSectionId::Keybindings {
            self.retain_found_shortcuts(&mut rows, cx);
            let empty = rows
                .is_empty()
                .then(|| self.render_no_shortcuts_found(appearance, cx))
                .flatten();
            (Some(self.render_shortcut_search(cx)), empty)
        } else {
            (None, None)
        };
        // Rows keep catalog order, so one run of neighbouring rows sharing a group title is one
        // card. A filtered view groups whatever survived the filter the same way.
        let mut groups: Vec<(&'static str, Vec<SettingsRowId>, Vec<AnyElement>)> = Vec::new();
        for row in rows {
            let title = row.descriptor().group;
            let rendered = self.render_row(row, appearance, window, cx);
            match groups.last_mut() {
                Some((current, ids, members)) if *current == title => {
                    ids.push(row);
                    members.push(rendered);
                }
                _ => groups.push((title, vec![row], vec![rendered])),
            }
        }
        self.row_bounds.borrow_mut().clear();
        let rendered = groups
            .into_iter()
            .map(|(title, ids, members)| {
                // The gallery's title names the appearance it is showing, which changes with the
                // mode and, under Auto, with the slot chosen above it. Its selector stays fixed.
                let heading = if title == SettingsRowId::InstalledThemes.descriptor().group {
                    self.installed_themes_title(cx)
                } else {
                    title
                };
                let row_bounds = Rc::clone(&self.row_bounds);
                FormGroup::new(group_selector(section, title), heading, members)
                    .on_rows_prepainted(move |bounds, _, _| {
                        row_bounds
                            .borrow_mut()
                            .extend(ids.iter().copied().zip(bounds));
                    })
                    .render(settings)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let notice = (section == SettingsSectionId::Themes)
            .then(|| self.render_diagnostics_notice(appearance, cx))
            .flatten();
        div()
            .debug_selector(move || section.selector().to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(group_spacing(appearance))
            .children(notice)
            .children(shortcut_search)
            .children(no_shortcuts)
            .children(rendered)
            .into_any_element()
    }
}

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
        let matched_indices = if self.query.trim().is_empty() {
            Vec::new()
        } else {
            catalog::matching_row_matches(&self.query, self.permission_access.naming())
                .into_iter()
                .find(|matched| matched.id == row)
                .map_or_else(Vec::new, |matched| matched.matched_indices)
        };
        // App-owned copy inside a full-width row shares the row's surface. Reusable controls
        // retain their own complete paints through the installed control catalog.
        let mut highlighted_appearance;
        let content_appearance = if highlighted {
            highlighted_appearance = appearance.clone();
            highlighted_appearance.colors.text = appearance.colors.row_selected_foreground;
            highlighted_appearance.colors.text_secondary = appearance.colors.row_selected_secondary;
            highlighted_appearance.colors.text_muted = appearance.colors.row_selected_secondary;
            let card = appearance.host_colors(spaceterm_ui::ControlHost::Card);
            highlighted_appearance.card_controls.reference.text = card.row_selected_foreground;
            highlighted_appearance
                .card_controls
                .reference
                .text_secondary = card.row_selected_secondary;
            highlighted_appearance.card_controls.reference.text_muted = card.row_selected_secondary;
            &highlighted_appearance
        } else {
            appearance
        };
        let control = self.render_control(row, content_appearance, window, cx);
        let mut rendered = FormRow::new(
            descriptor.selector,
            descriptor.label(self.permission_access.naming()),
            control,
        )
        .layout(row_layout(row))
        .reset(self.row_reset(row, appearance, cx))
        .matched_indices(matched_indices)
        .highlighted(highlighted);
        if row == SettingsRowId::UpdateStatus {
            rendered = rendered.description(self.update_status(cx).summary);
        } else if let Some(permission) = permission_access::row_permission(row) {
            rendered = rendered.description(
                self.permission_access
                    .row(permission)
                    .presentation()
                    .explanation,
            );
        } else if let SettingsRowId::Shortcut(command) = row {
            if let Some(description) = self.shortcut_description(command, cx) {
                rendered = rendered.caption(description.text, description.tone);
            }
        } else if let Some(description) = self.row_description(row, cx) {
            rendered = rendered.description(description);
        }
        rendered.render(appearance, window, cx).into_any_element()
    }

    fn render_control(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRowId::AppearanceMode => self.render_appearance_mode(cx),
            SettingsRowId::Opacity => self.render_opacity(appearance, cx),
            SettingsRowId::Blur => self.render_blur(cx),
            SettingsRowId::TerminalTheme => self.render_current_theme(appearance, window, cx),
            SettingsRowId::Density => self.render_density(cx),
            SettingsRowId::TerminalFontFamily => self.render_terminal_font(appearance, cx),
            SettingsRowId::TerminalBaseSize => self.render_terminal_size(appearance, cx),
            SettingsRowId::TerminalLineHeight => self.render_line_height(appearance, cx),
            SettingsRowId::TerminalRegularWeight | SettingsRowId::TerminalBoldWeight => {
                self.render_weight(row, appearance, cx)
            }
            SettingsRowId::TerminalItalic => self.render_italic(cx),
            SettingsRowId::TerminalBoldAsBright => self.render_bold_as_bright(cx),
            SettingsRowId::InstalledThemes => self.render_installed_themes(appearance, cx),
            SettingsRowId::MicrophoneAccess => self.render_microphone_access(appearance, cx),
            SettingsRowId::ScreenRecordingAccess => {
                self.render_permission_access(SystemPermission::ScreenRecording, appearance, cx)
            }
            SettingsRowId::AccessibilityAccess => {
                self.render_permission_access(SystemPermission::Accessibility, appearance, cx)
            }
            SettingsRowId::ClipboardWrites | SettingsRowId::ClipboardReads => {
                self.render_clipboard_preference(row, cx)
            }
            SettingsRowId::UpdateStatus => self.render_update_status(cx),
            SettingsRowId::AutomaticUpdateDownloads => self.render_automatic_update_downloads(cx),
            SettingsRowId::UpdateCheckInterval => self.render_update_check_interval(cx),
            SettingsRowId::UpdateReminderInterval => self.render_update_reminder_interval(cx),
            SettingsRowId::Shortcut(command) => self.render_shortcut(command, cx),
            SettingsRowId::SettingsFile => self.render_settings_file(appearance, cx),
            SettingsRowId::ExportSettings => {
                let owner = cx.weak_entity();
                action_button(
                    "settings-document-export",
                    "Export…",
                    true,
                    move |window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.begin_document_export(window, cx);
                        });
                    },
                )
                .into_any_element()
            }
            SettingsRowId::ImportSettings => {
                let owner = cx.weak_entity();
                action_button(
                    "settings-import",
                    "Import…",
                    self.editor.editable(),
                    move |window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.begin_settings_import(window, cx);
                        });
                    },
                )
                .into_any_element()
            }
            SettingsRowId::ResetAllSettings => {
                let owner = cx.weak_entity();
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
                .into_any_element()
            }
        }
    }

    /// One mode selects Chrome appearance and the matching Terminal slot.
    fn render_appearance_mode(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().appearance.mode;
        let selector = "settings-appearance-mode";
        let edge = crate::ui::appearance::chrome(cx)
            .host_colors(spaceterm_ui::ControlHost::Card)
            .border;
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

    /// Each miniature previews built-in Chrome and its matching Terminal slot. Auto shows both
    /// slots side by side, light leading.
    fn mode_preview_palettes(&self, mode: AppearanceMode) -> Vec<ModePreviewPalette> {
        let document = self.editor.document();
        let catalog =
            ThemeCatalog::from_terminal_themes(&document.terminal_themes).unwrap_or_default();
        let pick = |appearance: Appearance| {
            let mut preferences = document.appearance.clone();
            preferences.mode = appearance.into();
            let resolved = catalog
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable(),
                    &AvailableFonts::default(),
                )
                .ok()?;
            let chrome = &resolved.chrome.colors;
            let terminal = &resolved.terminal.colors;
            Some(ModePreviewPalette {
                root: chrome.background,
                title_bar: chrome.title_bar_background,
                terminal_background: terminal.background,
                terminal_foreground: terminal.foreground,
                terminal_accent: terminal.normal[4],
            })
        };
        let appearances = match mode {
            AppearanceMode::Light => &[Appearance::Light][..],
            AppearanceMode::Dark => &[Appearance::Dark][..],
            AppearanceMode::Auto => &[Appearance::Light, Appearance::Dark][..],
        };
        appearances.iter().copied().filter_map(pick).collect()
    }

    fn set_appearance_mode(&mut self, mode: AppearanceMode, cx: &mut Context<Self>) {
        self.edit(move |draft| draft.appearance.mode = mode, cx);
        self.synchronize_search_results();
    }

    fn set_theme(&mut self, slot: Appearance, id: ThemeId, cx: &mut Context<Self>) {
        self.edit(
            move |draft| {
                let themes = &mut draft.appearance.terminal.themes;
                themes.set(slot, id);
            },
            cx,
        );
    }

    fn render_density(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.editor.document().appearance.window.density;
        let owner = cx.weak_entity();
        SegmentedControl::new(
            "settings-density",
            "Density",
            &current,
            vec![
                SegmentedOption::new(ChromeDensity::Compact, "Compact")
                    .debug_selector("settings-density-compact"),
                SegmentedOption::new(ChromeDensity::Comfortable, "Comfortable")
                    .debug_selector("settings-density-comfortable"),
            ],
        )
        .expect("two densities are within the bounded option set")
        .disabled(!self.editor.editable())
        .debug_selector("settings-density")
        .on_change(move |change, _, cx| {
            let density = *change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(move |draft| draft.appearance.window.density = density, cx);
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
        let current = match &self.editor.document().appearance.terminal.typography.family {
            TerminalFontFamily::DefaultMonospace => None,
            TerminalFontFamily::Named { family } => Some(family.clone()),
        };
        let mut items = vec![
            ComboBoxItem::new(None, crate::bundled_font::LABEL)
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
            let choice = acceptance.item_id().clone();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        draft.appearance.terminal.typography.family = match choice {
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

    /// Opacity and Blur as the window presents them on this desktop. Where a window effect is
    /// unavailable, both rows show the rendering defaults and the retained choices wait in the
    /// document.
    fn window_background(&self, cx: &App) -> WindowBackgroundChoices {
        super::appearance_runtime::current(cx)
            .chrome
            .composition
            .capabilities
            .window_background(&self.editor.document().appearance.window)
    }

    fn render_opacity(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let background = self.window_background(cx);
        let value = background.opacity;
        let owner = cx.weak_entity();
        Stepper::new(
            "settings-opacity",
            "background opacity",
            format!("{value:.2}"),
        )
        .bounds(value > 0.0, value < 1.0)
        .enabled(self.editor.editable() && background.adjustable())
        .on_step(move |delta, _, cx| {
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| {
                        let value = &mut draft.appearance.window.opacity;
                        *value = (((*value * 20.0).round() + delta as f32) / 20.0).clamp(0.0, 1.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance)
        .into_any_element()
    }

    fn render_blur(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let background = self.window_background(cx);
        let owner = cx.weak_entity();
        Switch::new("settings-blur", "Blur background", background.blur)
            .size(ToggleSize::Regular)
            .label_hidden(true)
            .disabled(!self.editor.editable() || !background.adjustable())
            .debug_selector("settings-blur")
            .on_change(move |change, _, cx| {
                let blur = change.requested();
                let _ = owner.update(cx, |settings, cx| {
                    settings.edit(move |draft| draft.appearance.window.blur = blur, cx);
                });
            })
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
            .appearance
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
                        let size = &mut draft.appearance.terminal.typography.base_size;
                        *size = (*size + f32::from(delta as i16)).clamp(8.0, 32.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance)
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
            .appearance
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
                        let height = &mut draft.appearance.terminal.typography.line_height;
                        // Round to the step so repeated presses cannot drift off the grid.
                        let stepped = (*height / STEP).round() + f32::from(delta as i16);
                        *height = (stepped * STEP).clamp(1.0, 2.0);
                    },
                    cx,
                );
            });
        })
        .render(appearance)
        .into_any_element()
    }

    /// Renders one font-weight row.
    fn render_weight(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let typography = &self.editor.document().appearance;
        let current = match row {
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
            row.descriptor().label(self.permission_access.naming()),
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
                        let preferences = &mut draft.appearance;
                        match row {
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
        let value = self.editor.document().appearance.terminal.typography.italic;
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
                        move |draft| draft.appearance.terminal.typography.italic = italic,
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
            .appearance
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
                        draft.appearance.terminal.rendering.bold_as_bright = bold_as_bright;
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
        let pair = if critical {
            appearance.semantic_text_pairs.warning_status
        } else {
            appearance.semantic_text_pairs.error_status
        };
        Some(
            div()
                .debug_selector(|| "settings-banner".to_owned())
                .flex()
                .flex_row()
                .items_start()
                .gap(appearance.spacing(8.0))
                .mx(card_gutter(appearance))
                .mt(appearance.spacing(12.0))
                .p(appearance.spacing(10.0))
                .rounded(RadiusRole::Card.pixels())
                .bg(gpui_color(pair.background))
                .text_color(gpui_color(pair.primary))
                .border(px(HAIRLINE))
                .border_color(gpui_color(if critical {
                    appearance.colors.warning_border
                } else {
                    appearance.colors.error_border
                }))
                .child(div().flex_none().mt(px(1.0)).child(Icon::new(
                    IconName::TriangleAlert,
                    appearance.icons.metrics(IconRole::Status).glyph_size,
                    gpui_color(pair.primary),
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
                                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                                .child(gpui::Text::new(
                                    "settings-banner-message".into(),
                                    status.message().into(),
                                )),
                        )
                        .child(
                            div()
                                .chrome_text(appearance.typography.style(TextRole::Secondary))
                                .text_color(gpui_color(pair.secondary))
                                .whitespace_normal()
                                .child(gpui::Text::new(
                                    "settings-banner-explanation".into(),
                                    explanation.into(),
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_none()
                        .self_center()
                        .gap(appearance.spacing(6.0))
                        .when(status.recoverable(), |actions| {
                            let owner = owner.clone();
                            actions.child(action_button(
                                "settings-banner-reset-settings",
                                "Reset Settings…",
                                true,
                                move |window, cx| {
                                    let _ = owner.update(cx, |settings, cx| {
                                        settings.confirm_settings_recovery(window, cx);
                                    });
                                },
                            ))
                        })
                        .child(action_button(action_selector, label, true, move |_, cx| {
                            let _ = owner.update(cx, |settings, cx| {
                                if critical {
                                    settings.editor.reload(cx);
                                } else {
                                    settings.editor.retry(cx);
                                }
                            });
                        })),
                )
                .into_any_element(),
        )
    }

    /// The content column's closing strip: what the last edit did.
    fn render_footer(&self, settings: &SettingsAppearance) -> AnyElement {
        let appearance = &settings.chrome;
        super::sidebar_window::render_footer(
            "settings",
            settings,
            div()
                .debug_selector(|| "settings-save-status".to_owned())
                .min_w_0()
                .truncate()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                // The recovery banner owns semantic emphasis on its paired surface.
                .text_color(gpui_color(appearance.colors.text_muted))
                .child(gpui::Text::new(
                    "settings-save-status".into(),
                    self.editor.status().message().into(),
                )),
        )
    }

    /// Confirms Settings Recovery. The reset replaces the file SpaceTerm could not read, so the
    /// alert names the backup that keeps it.
    fn confirm_settings_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let result = super::settings_recovery::confirmation_alert().present(
            window,
            cx,
            move |outcome, cx| {
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
                    // A failure keeps the banner, whose status now names what stopped the reset.
                    if settings.editor.recover_by_reset(cx).is_err() {
                        eprintln!("SpaceTerm Settings could not be reset");
                    }
                });
            },
        );
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings recovery confirmation");
        }
    }

    fn reset_all_detail(&self) -> String {
        let mut detail = String::from(
            "This cannot be undone. Themes can be installed again from their Zed extension or file.",
        );
        let mut permissions = Vec::new();
        if !self.omitted_rows.contains(&SettingsRowId::MicrophoneAccess) {
            permissions.push("Microphone");
        }
        for (row, permission) in [
            (
                SettingsRowId::ScreenRecordingAccess,
                SystemPermission::ScreenRecording,
            ),
            (
                SettingsRowId::AccessibilityAccess,
                SystemPermission::Accessibility,
            ),
        ] {
            if !self.omitted_rows.contains(&row) {
                permissions.push(self.permission_access.row(permission).copy().name);
            }
        }
        match permissions.as_slice() {
            [] => {}
            [permission] => detail.push_str(&format!(" {permission} access is a system permission and is not affected.")),
            [first, second] => detail.push_str(&format!(" {first} and {second} access are system permissions and are not affected.")),
            [first, second, third] => detail.push_str(&format!(" {first}, {second}, and {third} access are system permissions and are not affected.")),
            _ => unreachable!("Settings presents at most three system permission rows"),
        }
        detail
    }

    fn confirm_reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-reset-all"),
            "Reset all settings",
            "Reset All Settings",
            "Every setting and keyboard shortcut returns to its default, and the terminal themes you installed are removed.",
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
        // Installed themes are the one thing here the reset cannot give back, so the alert says
        // so rather than leaving that to be discovered.
        .detail(self.reset_all_detail())
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
                settings.shortcuts.dismiss_notice();
                settings.editor.reset_all(cx);
                cx.notify();
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings reset confirmation");
        }
    }
}

/// The colors one appearance-mode miniature paints: built-in Chrome around its Terminal slot.
#[derive(Clone, Debug, PartialEq)]
struct ModePreviewPalette {
    root: Color,
    title_bar: Color,
    terminal_background: Color,
    terminal_foreground: Color,
    terminal_accent: Color,
}

/// A miniature SpaceTerm window for each palette, split evenly when there is more than one.
fn mode_preview(
    palettes: &[ModePreviewPalette],
    edge: Color,
    extent: gpui::Pixels,
) -> impl IntoElement {
    let width = extent * 1.5;
    let last = palettes.len().saturating_sub(1);
    div()
        .flex()
        .flex_row()
        .w(width)
        .h(extent)
        .rounded(RadiusRole::ControlSmall.pixels())
        .overflow_hidden()
        .border(px(HAIRLINE))
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

fn mode_miniature(
    palette: &ModePreviewPalette,
    width: gpui::Pixels,
    extent: gpui::Pixels,
) -> gpui::Div {
    div()
        .w(width)
        .h(extent)
        .bg(gpui_color(palette.root))
        .flex()
        .flex_col()
        .child(
            div()
                .w_full()
                .h(extent * 0.22)
                .bg(gpui_color(palette.title_bar))
                .opacity(0.35),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .bg(gpui_color(palette.terminal_background))
                .gap(px(2.0))
                .p(px(3.0))
                .child(
                    div()
                        .w(extent * 0.8)
                        .h(px(2.0))
                        .bg(gpui_color(palette.terminal_foreground)),
                )
                .child(
                    div()
                        .w(extent * 0.55)
                        .h(px(2.0))
                        .bg(gpui_color(palette.terminal_accent)),
                )
                .child(
                    div()
                        .w(extent * 0.65)
                        .h(px(2.0))
                        .bg(gpui_color(palette.terminal_foreground)),
                ),
        )
}

/// The families the terminal font list offers. Only monospaced families are offered, because a
/// proportional terminal font resolves to a fallback.
fn terminal_font_families(fonts: &crate::appearance::AvailableFonts) -> Vec<String> {
    fonts
        .installed
        .iter()
        .filter(|font| font.class == FontClass::Monospace)
        .filter(|font| font.family != crate::bundled_font::FAMILY)
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
fn settings_selector<I: Clone + Eq + 'static>(
    selector: String,
    accessibility_name: impl Into<SharedString>,
    selected: Option<I>,
    prompt: &'static str,
    items: Vec<ComboBoxItem<I>>,
    appearance: &ChromeAppearance,
) -> ComboBox<I> {
    let glyph = gpui_color(
        appearance
            .host_colors(spaceterm_ui::ControlHost::Card)
            .icon_muted,
    );
    ComboBox::new(
        SharedString::from(selector.clone()),
        accessibility_name,
        selected,
        prompt,
        items,
    )
    // The trigger takes the width of the value it shows, so the value and its chevron stay
    // together at the row's right edge. Its ordinary frame comes from the shared ComboBox theme.
    .hug(true)
    .input_leading(move |size| Icon::new(IconName::Search, size, glyph).into_any_element())
    .debug_selector(selector)
}

fn control_selector(row: SettingsRowId) -> String {
    format!("{}-control", row.descriptor().selector)
}

/// Where a row's label sits.
fn row_layout(row: SettingsRowId) -> FormRowLayout {
    match row {
        SettingsRowId::TerminalTheme
        | SettingsRowId::InstalledThemes
        | SettingsRowId::SettingsFile => FormRowLayout::Full,
        _ => FormRowLayout::Beside,
    }
}

impl SettingsWindow {
    /// One line of guidance for the rows that warrant it.
    fn row_description(&self, row: SettingsRowId, cx: &App) -> Option<&'static str> {
        match row {
            SettingsRowId::AppearanceMode => Some("Auto matches the system light or dark setting."),
            SettingsRowId::Opacity | SettingsRowId::Blur => {
                let background = self.window_background(cx);
                let fully_opaque = background.opacity == 1.0;
                let composition = super::appearance_runtime::current(cx).chrome.composition;
                let accessibility_forced_opaque =
                    !fully_opaque && composition.floating_materials.is_opaque();
                let opacity = row == SettingsRowId::Opacity;
                Some(
                    match (background.unavailable, accessibility_forced_opaque) {
                        (Some(UnavailableWindowEffect::Opacity), false) if opacity => {
                            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Floating surfaces use the default opacity."
                        }
                        (Some(UnavailableWindowEffect::Opacity), false) => {
                            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Floating surfaces use the default blur."
                        }
                        (Some(UnavailableWindowEffect::Opacity), true) if opacity => {
                            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Accessibility settings currently keep floating surfaces opaque."
                        }
                        (Some(UnavailableWindowEffect::Opacity), true) => {
                            "Window opacity adjustment is unavailable on this system, so the window stays opaque. Accessibility settings currently disable floating-surface blur."
                        }
                        (Some(UnavailableWindowEffect::Blur), false) if opacity => {
                            "Desktop blur is unavailable on this system, so the window stays opaque. Floating surfaces use the default opacity."
                        }
                        (Some(UnavailableWindowEffect::Blur), false) => {
                            "Desktop blur is unavailable on this system, so the window stays opaque. Floating surfaces use the default blur."
                        }
                        (Some(UnavailableWindowEffect::Blur), true) if opacity => {
                            "Desktop blur is unavailable on this system, so the window stays opaque. Accessibility settings currently keep floating surfaces opaque."
                        }
                        (Some(UnavailableWindowEffect::Blur), true) => {
                            "Desktop blur is unavailable on this system, so the window stays opaque. Accessibility settings currently disable floating-surface blur."
                        }
                        (None, _) if opacity && fully_opaque => {
                            "The window and floating surfaces are opaque at 1. Decrease this value to reveal the content behind them."
                        }
                        (None, true) if opacity => {
                            "Accessibility settings currently keep the window and floating surfaces opaque. Your opacity choice is kept."
                        }
                        (None, _) if opacity => {
                            "Adjust opacity for windows and floating surfaces. 0 is transparent; 1 is opaque."
                        }
                        (None, _) if fully_opaque => {
                            "Blur affects the desktop behind the window and content behind floating surfaces. Decrease Opacity below 1 to see it."
                        }
                        (None, true) => {
                            "Accessibility settings currently disable window and floating-surface blur. Your blur choice is kept."
                        }
                        (None, false) => {
                            "Soften the desktop behind the window and content behind floating surfaces."
                        }
                    },
                )
            }
            SettingsRowId::TerminalFontFamily => Some("Only monospaced families are listed."),
            SettingsRowId::AutomaticUpdateDownloads
            | SettingsRowId::UpdateCheckInterval
            | SettingsRowId::UpdateReminderInterval => updates::update_row_description(row),
            SettingsRowId::MicrophoneAccess => {
                Some(self.microphone_access.presentation().explanation)
            }
            SettingsRowId::ClipboardWrites => Some(
                "Programs in the focused pane can replace your clipboard text, including over SSH.",
            ),
            SettingsRowId::ClipboardReads => Some(
                "Programs in the focused pane can retrieve your clipboard text, including programs on remote machines.",
            ),
            SettingsRowId::ExportSettings => {
                Some("Save every setting, keyboard shortcut, and installed theme to a file.")
            }
            SettingsRowId::ImportSettings => Some(
                "Replace every setting, keyboard shortcut, and installed theme with an exported file.",
            ),
            SettingsRowId::ResetAllSettings => Some(
                "Return every setting and keyboard shortcut to its default and remove installed themes.",
            ),
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
