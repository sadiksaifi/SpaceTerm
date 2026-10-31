//! The Settings Window: a separate, modeless Operating-System Window for SpaceTerm Settings.
//! It is application scoped so it stays reachable when no Workspace window exists.

mod add_server;
mod advanced;
mod background_image;
mod capture;
mod catalog;
mod clipboard;
mod command_line_tool;
mod direct_access;
mod direct_join;
mod direct_routes;
mod editor;
mod end_sessions;
mod git;
mod import;
mod keybindings;
pub(crate) mod local_operations;
mod notices;
mod permission_access;
pub(crate) mod remote_admin;
mod server_card;
mod server_facts;
mod server_list;
mod server_location;
mod server_selection;
pub(crate) mod server_update;
pub(crate) mod servers;
mod sidebar;
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
use crate::platform::capture_access::{CaptureAccess, CaptureDevice};
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
    CaptionTone, CardEnds, FormGroup, FormRow, FormRowLayout, Stepper, action_button, reset_button,
    row_horizontal_inset, section_heading,
};
use capture::CaptureAccessRow;
use catalog::SettingsRowId;
pub(crate) use catalog::SettingsSectionId;
pub(crate) use command_line_tool::configure_command_line_tool;
use editor::{SaveStatus, SettingsEditor};
use keybindings::ShortcutRows;
use permission_access::{PermissionAccessChanges, PermissionAccessRows};
pub(crate) use server_selection::configure_server_context;
pub(crate) use server_update::update_offered;
/// The server control window shares the service row's progress and authorization wording.
pub(crate) use servers::{AuthorizationPrompt, AuthorizedRequest, ServiceAction};
use theme_gallery::ThemeGallery;
use theme_store::ThemeStore;

actions!(
    spaceterm,
    [
        OpenSettings,
        OpenKeyboardShortcuts,
        CloseSettingsWindow,
        FocusSettingsSearch,
        ClearSettingsSearch,
        AddServer
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
    /// The microphone and camera, which programs in a Terminal Session use.
    pub(crate) capture: Option<Rc<dyn CaptureAccess>>,
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

/// Presents a notice over Settings when Settings is the active window, and reports whether it did.
///
/// A Workspace Window outcome can arrive while Settings is the active window. Some desktops
/// refuse to raise the Workspace Window, which would leave its alert hidden behind Settings.
/// Attaching the notice to the window the person is using keeps the outcome visible without
/// raising anything.
///
/// The notice is presented once the current update finishes, because the request usually comes
/// from a Settings control while Settings is still being updated.
pub(crate) fn present_over_active_settings(
    cx: &mut App,
    present: impl FnOnce(&mut Window, &mut Context<SettingsWindow>) + 'static,
) -> bool {
    let Some(settings) = cx
        .active_window()
        .and_then(|window| window.downcast::<SettingsWindow>())
    else {
        return false;
    };
    cx.defer(move |cx| {
        let _ = settings.update(cx, |_, window, cx| present(window, cx));
    });
    true
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
    let local_server = servers::offered_local_server(command_line_tool::offered(cx));
    // Settings returns where it last closed, on the Settings Section it showed, unless the
    // request names one.
    let remembered = super::remembered_view::settings_window(cx);
    let section = section.or(remembered.map(|remembered| remembered.section));
    let mut options = super::sidebar_window::window_options(
        "Settings",
        size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        cx,
    );
    if let Some(remembered) = remembered {
        remembered.placement.apply(&mut options, cx);
    }
    let opened = cx.open_window(
        options,
        |window, cx| {
            let settings = cx.new(|cx| {
                let mut settings = SettingsWindow::new_with_capabilities(
                    Rc::clone(&window_drag),
                    permissions.clone(),
                    theme_registry.clone(),
                    local_server.clone(),
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

/// Scrolls `scroll` the least distance that shows all of `bounds`, or its top when it is taller
/// than the viewport. Returns whether the offset changed.
fn reveal(scroll: &ScrollHandle, bounds: Bounds<Pixels>) -> bool {
    let viewport = scroll.bounds();
    let shift = if bounds.top() < viewport.top() || bounds.size.height > viewport.size.height {
        viewport.top() - bounds.top()
    } else if bounds.bottom() > viewport.bottom() {
        viewport.bottom() - bounds.bottom()
    } else {
        return false;
    };
    let offset = scroll.offset();
    let lowest = -scroll.max_offset().y;
    scroll.set_offset(gpui::point(
        offset.x,
        (offset.y + shift).clamp(lowest, px(0.0)),
    ));
    true
}

/// Opens Settings on Servers with `target`'s server selected. Selecting reads nothing from that
/// server.
pub(crate) fn open_server_settings(target: crate::ui::ServerTarget, cx: &mut App) {
    open_or_activate(Some(SettingsSectionId::Servers), cx);
    // An existing window selects the section in a deferred update, so the selection follows it.
    cx.defer(move |cx| {
        if let Some(settings) = open_settings_window(cx) {
            let _ = settings.update(cx, |settings, _, cx| {
                settings.select_server(
                    match target {
                        crate::ui::ServerTarget::Local => server_list::ServerChoice::Local,
                        crate::ui::ServerTarget::Direct(route) => {
                            server_list::ServerChoice::Direct(route)
                        }
                    },
                    cx,
                );
            });
        }
    });
}

/// Opens Settings on Servers and starts the one Update Server journey there, for a surface that
/// offers it elsewhere, such as the incompatible-server screen or the status item. The journey
/// reads the server first and asks before it changes anything.
pub(crate) fn open_server_update(cx: &mut App) {
    open_or_activate(Some(SettingsSectionId::Servers), cx);
    // An existing window selects the section in a deferred update, so the request follows it.
    cx.defer(|cx| {
        if let Some(settings) = open_settings_window(cx) {
            let _ = settings.update(cx, |settings, _, cx| {
                // The journey updates the local server, so its detail is the one presented.
                settings.select_server(server_list::ServerChoice::Local, cx);
                settings.request_server_update(cx);
            });
        }
    });
}

/// Opens Settings on Servers and starts adding a server there. `invitation` fills in an invitation
/// link, such as one the operating system delivered, and `host` fills in an SSH host. Neither
/// sets up nor joins: the person reviews the value and confirms first. An invitation wins when
/// both are given, because the person followed that link most recently.
pub(crate) fn open_add_server(invitation: Option<String>, host: Option<String>, cx: &mut App) {
    open_or_activate(Some(SettingsSectionId::Servers), cx);
    // An existing window selects the section in a deferred update, so the request follows it.
    cx.defer(move |cx| {
        if let Some(settings) = open_settings_window(cx) {
            let _ = settings.update(cx, |settings, window, cx| {
                let request = match (invitation, host) {
                    (Some(link), _) => AddServerRequest::Invitation(Some(link.into())),
                    (None, host) => AddServerRequest::Ssh(host),
                };
                settings.begin_add_server(request, window, cx);
            });
        }
    });
}

/// Where Add Server starts, and the value it fills in.
pub(crate) enum AddServerRequest {
    Ssh(Option<String>),
    /// The invitation text is wiped when the request drops.
    Invitation(Option<zeroize::Zeroizing<String>>),
}

/// Retains Settings until its latest edit is saved, then completes an authorized application quit.
/// Closes the Settings window the way its own close button does: it saves first and stays open
/// when saving fails. Reports whether `window` is the Settings window.
pub(crate) fn request_window_close(window: AnyWindowHandle, cx: &mut App) -> bool {
    let Some(settings) = window.downcast::<SettingsWindow>() else {
        return false;
    };
    let _ = settings.update(cx, |settings, _, cx| {
        settings.request_close(CloseIntent::Window(window), cx);
    });
    true
}

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
    cx.on_action(|_: &AddServer, cx| open_add_server(None, None, cx));
    super::application_commands::init(cx);
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
    /// A row to scroll into view again once the next frame lays it out, for a row that grew.
    reveal_after_layout: Rc<std::cell::Cell<Option<SettingsRowId>>>,
    focus_handle: FocusHandle,
    navigation: SidebarNavigation,
    window_movement: WindowMovement,
    microphone_access: CaptureAccessRow,
    camera_access: CaptureAccessRow,
    /// The sections and rows this desktop presents. A desktop omits only the surfaces of features
    /// it has no equivalent for; an unavailable feature keeps its rows to explain why.
    available_sections: Vec<SettingsSectionId>,
    omitted_rows: Vec<SettingsRowId>,
    /// Counts Background Image choices, so only the latest one applies.
    background_image_choices: u64,
    /// Keeps the copy the draft names until the Settings name it too.
    background_image_lease: Option<super::background_image_runtime::BackgroundImageLease>,
    permission_access: PermissionAccessRows,
    _permission_changes: Option<PermissionAccessChanges>,
    theme_gallery: ThemeGallery,
    /// The Get More Themes sheet, kept for the window's life so the registry is listed once.
    theme_store: Entity<ThemeStore>,
    /// One shortcut recorder per Command, kept so a recording survives re-rendering.
    shortcuts: ShortcutRows,
    /// The read-only settings file in the Advanced section.
    settings_file: advanced::SettingsFileView,
    server_location: server_location::ServerLocation,
    end_all_sessions: end_sessions::EndAllSessions,
    /// The selected server's direct network access and paired devices.
    direct_access: direct_access::DirectAccess,
    /// The local server this window presents. `None` omits the Servers section.
    local_server: Option<servers::LocalServerRow>,
    local_operations: Entity<local_operations::LocalServerOperations>,
    /// The service that runs the local server. `None` omits its row.
    server_service: Option<servers::ServerServiceRow>,
    /// The server version and its update journey. `None` omits its row.
    server_update: Option<server_update::ServerUpdateRow>,
    /// This computer's command for the local server's identity. `None` omits its row.
    command_line_tool: Option<command_line_tool::CommandLineToolRow>,
    /// The desktop's direct network connections. `None` omits their rows.
    direct_routes: Option<direct_routes::DirectRouteRows>,
    /// The server the person selected in the Servers list. A server no longer listed gives way
    /// to the local server or the first saved one.
    selected_server: Option<server_list::ServerChoice>,
    /// The server the current search shows, which stands in for the person's choice only while
    /// the query lasts.
    search_selected_server: Option<server_list::ServerChoice>,
    /// The All Servers list's one keyboard stop. The arrow keys move its selection.
    server_list_focus: FocusHandle,
    /// The keyboard stop of All Servers' Add Server row.
    add_server_focus: FocusHandle,
    /// The servers last reported to the retained selection, so only a change reports again.
    reconciled_servers: Option<Vec<crate::server_context::ServerKey>>,
    /// The Servers notices the person hid in this window.
    hidden_notices: notices::HiddenNotices,
    /// Follows the presented server's facts, so a server SpaceTerm later disconnects from shows
    /// them.
    server_facts: server_facts::FactsFollower,
    /// Check Now underway on the Server row, for the server it was asked for.
    version_check: Option<server_card::VersionCheck>,
    /// A read of the selected server, which the Server row shows as Checking once it runs long.
    check_progress: server_card::DelayedProgress,
    /// An operation changing the selected server, which the Server row names once it runs long.
    operation_progress: server_card::DelayedProgress,
    /// The status last observed for each listed server, which a server SpaceTerm isn't connected
    /// to presents. It grants nothing: what each server offers follows its live connection.
    retained_status: std::collections::BTreeMap<
        crate::server_context::ServerKey,
        crate::server_context::ObservedServerStatus,
    >,
    /// Each paired server's Status, Stop, Restart and Update work, and its last outcome.
    remote_admin: remote_admin::RemoteAdministration,
    /// How Servers names the server for this account on this computer.
    this_computer: &'static str,
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
        let previous = std::mem::replace(&mut self.active_section, section);
        self.section_presented(previous, cx);
        self.shortcuts.dismiss_notice();
        self.shortcuts.end_search_capture(cx);
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }
}

/// What a Settings search that matches nothing says, in the shared empty state. The
/// Keybindings and installed-themes searches use it too.
pub(super) fn no_results(query: &str, cx: &App) -> spaceterm_ui::EmptyState {
    crate::ui::empty_state::empty_state(
        format!("No Results for \u{201C}{}\u{201D}", query.trim()),
        cx,
    )
    .icon(IconName::Search)
    .detail(SEARCH_DETAIL)
    .debug_selector("settings-no-results")
}

/// What every search that finds nothing suggests.
pub(super) const SEARCH_DETAIL: &str = "Check the spelling or try a different term.";

fn section_feature(section: SettingsSectionId) -> Option<HostFeature> {
    (section == SettingsSectionId::Updates).then_some(HostFeature::Updates)
}

/// The host feature one row presents, if any.
fn row_feature(row: SettingsRowId) -> Option<HostFeature> {
    if let Some(feature) = section_feature(row.descriptor().section) {
        Some(feature)
    } else if matches!(
        row,
        SettingsRowId::MicrophoneAccess | SettingsRowId::CameraAccess
    ) {
        Some(HostFeature::CaptureAccess)
    } else {
        permission_access::row_permission(row).map(|_| HostFeature::SystemPermissions)
    }
}

/// How one row returns to its default.
#[derive(Clone, Debug)]
enum RowReset {
    ServerLocation,
    Appearance(ResetTarget),
    Update,
    Clipboard,
    Git,
    Sidebar,
    Shortcut(crate::keybindings::Command),
}

impl SettingsWindow {
    #[cfg(test)]
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_capabilities(
            Rc::new(RecordingOperatingSystemWindowDragPlatform::default()),
            PermissionCapabilities::default(),
            None,
            None,
            window,
            cx,
        )
    }

    fn new_with_capabilities(
        operating_system_window_drag_platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        permissions: PermissionCapabilities,
        theme_registry: Option<ZedThemeRegistry>,
        local_server: servers::OfferedLocalServer,
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
                    let previous = settings.active_section;
                    settings.synchronize_search_results();
                    settings.section_presented(previous, cx);
                    cx.notify();
                }
            },
        )
        .detach();
        let scrollbar = cx.new(|_| {
            OverlayScrollbar::<f32>::new("settings-scrollbar")
                .accessibility_name("Settings content")
        });
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
        // Tool discovery and the GitHub CLI login check finish after this window opens.
        cx.observe_global::<crate::ui::repository_status_store::RepositoryTools>(|_, cx| {
            cx.notify()
        })
        .detach();
        // Authorization can change in System Settings while this window is in the background.
        // Leaving the window also ends searching by Shortcut, so chords pressed on return are not
        // captured.
        cx.observe_window_activation(window, |settings, window, cx| {
            if window.is_window_active() {
                settings.refresh_capture_access(cx);
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
            // The direct registry ends with the application, so this window lets go of it and
            // stops following it.
            settings.direct_routes = None;
            async {}
        })
        .detach();
        let theme_gallery = ThemeGallery::new(window, cx);
        let owner = cx.weak_entity();
        let theme_store = cx.new(|cx| ThemeStore::new(owner, theme_registry, window, cx));
        let shortcuts = ShortcutRows::new(window, cx);
        let settings_file = advanced::SettingsFileView::new(window, cx);
        let server_location = server_location::ServerLocation::new(window, cx);
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
        let direct_routes = crate::ui::direct_servers::installed(cx)
            .map(|connections| direct_routes::DirectRouteRows::new(connections, cx));
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let this_computer = presentation.wording().this_computer;
        let available_sections = SettingsSectionId::ALL
            .into_iter()
            .filter(|section| {
                section_feature(*section).is_none_or(|feature| presentation.has_feature(feature))
                    && (*section != SettingsSectionId::Servers
                        || local_server.is_some()
                        || direct_routes.is_some())
            })
            .collect();
        // Only a desktop whose Workspace windows can present a Background Image keeps its copies.
        let presents_background_image = super::background_image_runtime::store(cx).is_some();
        let omitted_rows = catalog::rows()
            .map(|row| row.id)
            .filter(|row| {
                row_feature(*row).is_some_and(|feature| !presentation.has_feature(feature))
                    || (*row == SettingsRowId::BackgroundImage && !presents_background_image)
                    // Only this computer's service starts a server on its own.
                    || (*row == SettingsRowId::ServerAutoStart
                        && local_server
                            .as_ref()
                            .is_none_or(|offered| offered.service.is_none()))
                    || (*row == SettingsRowId::CommandLineTool
                        && local_server
                            .as_ref()
                            .is_none_or(|offered| offered.command_line_tool.is_none()))
            })
            .collect();
        let local_operations = local_operations::LocalServerOperations::get(cx);
        cx.subscribe(&local_operations, |settings, _, operation, cx| {
            settings.synchronize_local_operations(cx);
            settings.check_local_server(cx);
            // The operation may have moved the launcher, or failed to.
            settings.reread_command_line_tool(cx);
            match operation {
                local_operations::CompletedOperation::Service => settings.check_server_update(cx),
                local_operations::CompletedOperation::Update => settings.check_server_service(cx),
                local_operations::CompletedOperation::Elsewhere => {
                    settings.reread_server_service(cx);
                    settings.reread_server_update(cx);
                }
            }
            settings.continue_requested_update(cx);
            cx.notify();
        })
        .detach();
        let mut settings = Self {
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
            reveal_after_layout: Rc::default(),
            focus_handle,
            navigation,
            window_movement,
            available_sections,
            omitted_rows,
            background_image_choices: 0,
            background_image_lease: None,
            microphone_access: CaptureAccessRow::new(
                CaptureDevice::Microphone,
                permissions.capture.clone(),
            ),
            camera_access: CaptureAccessRow::new(CaptureDevice::Camera, permissions.capture),
            permission_access: PermissionAccessRows::new(
                permissions.system_permissions,
                permission_setup,
                cx,
            ),
            _permission_changes: permission_changes,
            theme_gallery,
            theme_store,
            shortcuts,
            settings_file,
            server_location,
            end_all_sessions: end_sessions::EndAllSessions::new(),
            direct_access: direct_access::DirectAccess::new(),
            local_operations,
            server_service: local_server
                .as_ref()
                .and_then(|offered| offered.service.clone())
                .map(servers::ServerServiceRow::new),
            server_update: local_server
                .as_ref()
                .and_then(|offered| offered.update.clone())
                .map(|control| {
                    server_update::ServerUpdateRow::new(control, window.window_handle())
                }),
            command_line_tool: local_server
                .as_ref()
                .and_then(|offered| offered.command_line_tool.clone())
                .map(command_line_tool::CommandLineToolRow::new),
            local_server: local_server
                .map(|offered| servers::LocalServerRow::new(offered.connection)),
            direct_routes,
            selected_server: None,
            search_selected_server: None,
            server_list_focus: cx.focus_handle().tab_stop(true),
            add_server_focus: cx.focus_handle().tab_stop(true),
            reconciled_servers: None,
            hidden_notices: notices::HiddenNotices::default(),
            server_facts: server_facts::FactsFollower::default(),
            version_check: None,
            check_progress: server_card::DelayedProgress::default(),
            operation_progress: server_card::DelayedProgress::default(),
            retained_status: std::collections::BTreeMap::new(),
            remote_admin: remote_admin::RemoteAdministration::default(),
            this_computer,
        };
        // Servers and Git present the retained selection, which loads after the window opens.
        cx.observe_global::<server_selection::ServerSelection>(|settings, cx| {
            settings.follow_server_selection(cx);
        })
        .detach();
        settings.follow_server_selection(cx);
        settings.reconcile_servers(cx);
        settings.synchronize_local_operations(cx);
        settings
    }

    /// Checks the local server when its section replaces another in the detail pane, so the
    /// status is current without opening Settings asking the server anything.
    fn section_presented(&mut self, previous: SettingsSectionId, cx: &mut Context<Self>) {
        // Servers and Git both show the server picker, whose statuses come from these reads.
        let picker =
            |section| matches!(section, SettingsSectionId::Servers | SettingsSectionId::Git);
        if picker(self.active_section) && !picker(previous) {
            self.check_local_server(cx);
            self.check_server_service(cx);
            self.check_server_update(cx);
        }
        if self.active_section == SettingsSectionId::Servers
            && previous != SettingsSectionId::Servers
        {
            self.check_command_line_tool(cx);
        }
        self.sync_server_location(cx);
    }

    fn close(&mut self, _: &CloseSettingsWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.request_close(CloseIntent::Window(window.window_handle()), cx);
    }

    fn request_close(&mut self, intent: CloseIntent, cx: &mut Context<Self>) {
        // Closing the window is the person's response to what it shows, if it shows the
        // failure. Quitting is not.
        if matches!(intent, CloseIntent::Window(_)) {
            self.server_location.answered();
        }
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
                CloseIntent::Window(handle) => {
                    let section = self.active_section;
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| {
                            super::remembered_view::remember_settings_window(section, window, cx);
                            window.remove_window();
                        });
                    });
                }
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
        if reveal(&self.scroll, bounds) {
            cx.notify();
        }
    }

    /// Scrolls `row` into view now and again after the next layout, for a row that is about to
    /// grow, such as one that starts showing an error.
    pub(in crate::ui::settings_window) fn reveal_growing_row(
        &mut self,
        row: SettingsRowId,
        cx: &mut Context<Self>,
    ) {
        self.scroll_row_into_view(row, cx);
        self.reveal_after_layout.set(Some(row));
        cx.notify();
    }

    /// The rows this desktop can present that answer the query, best first, without regard to
    /// the server selected in the Servers list.
    fn reachable_rows(&self) -> Vec<SettingsRowId> {
        catalog::matching_rows(&self.query, self.permission_access.naming())
            .into_iter()
            .filter(|row| {
                self.available_sections.contains(&row.descriptor().section)
                    && !self.omitted_rows.contains(row)
            })
            .collect()
    }

    /// The rows answering the query for the selected server, best first.
    fn search_matches(&self) -> Vec<SettingsRowId> {
        self.server_search_matches(self.reachable_rows())
    }

    fn matching_rows(&self) -> Vec<SettingsRowId> {
        self.search_matches()
    }

    fn rows_for(&self, section: SettingsSectionId) -> Vec<SettingsRowId> {
        self.matching_rows()
            .into_iter()
            .filter(|row| row.descriptor().section == section)
            .collect()
    }

    fn synchronize_search_results(&mut self) {
        let reachable = self.reachable_rows();
        self.select_server_for_query(&reachable);
        let matching = self.search_matches();
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
        if row == SettingsRowId::ServerWorktreeLocation {
            return self
                .server_location_differs()
                .then_some(RowReset::ServerLocation);
        }
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
        if let Some(differs) = self.git_preference_differs(row) {
            return differs.then_some(RowReset::Git);
        }
        if let Some(differs) = self.sidebar_preference_differs(row) {
            return differs.then_some(RowReset::Sidebar);
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
                        RowReset::ServerLocation => settings.reset_server_location(cx),
                        RowReset::Appearance(target) => settings.editor.reset(target, cx),
                        RowReset::Update => settings.reset_update_preference(row, cx),
                        RowReset::Clipboard => settings.reset_clipboard_preference(row, cx),
                        RowReset::Git => settings.reset_git_preference(row, cx),
                        RowReset::Sidebar => settings.reset_sidebar_preference(cx),
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
        self.hold_background_image(cx);
        let activity = super::appearance::window_activity(window);
        super::sidebar_window::render_scoped(activity, || self.render_chrome(window, cx))
    }
}

impl SettingsWindow {
    fn render_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let settings = crate::ui::appearance::settings::shared(cx);
        let appearance = &settings.chrome;
        self.sync_scrollbar(cx);
        // Rendering the Location row marks it shown again.
        self.server_location.render_begins();
        let surface = div()
            .debug_selector(|| "settings-window-surface".to_owned())
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::clear_search));
        // The surface holds the window's focus, so it publishes a node for focus to rest on.
        let content = super::sidebar_window::tab_traversal(surface, cx)
            .id("settings-window-surface")
            .role(gpui::accesskit::Role::Group)
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
        // A failure the person can no longer see goes back to the Workspace Window.
        self.server_location.release_unless_rendered();
        ModalLayer::new(super::window_shell::render(content, window, cx)).into_any_element()
    }
}

impl SettingsWindow {
    /// The active section's large title and description at the head of the content surface.
    fn render_detail_heading(
        &mut self,
        settings: &SettingsAppearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section = self.active_section;
        // Servers and Git act on one server, which the same picker chooses on both.
        let picker =
            matches!(section, SettingsSectionId::Servers | SettingsSectionId::Git).then(|| {
                let flush = DetailHeading::toolbar_on_gutter(window, cx);
                self.render_server_picker(&settings.chrome, flush, cx)
            });
        let close_owner = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let handle = window.window_handle();
            let _ = close_owner.update(cx, |settings, cx| {
                settings.request_close(CloseIntent::Window(handle), cx)
            });
        });
        let scrolled = self.scroll.max_offset().y > px(0.0) && self.scroll.offset().y < px(-0.5);
        let heading = DetailHeading::new(
            "settings",
            section_heading(
                section.selector(),
                section.title(),
                self.section_description(section, cx),
                &settings.chrome,
            ),
            &self.window_movement,
            close,
        )
        .scrolled(scrolled);
        match picker {
            Some(picker) => heading.toolbar(picker),
            None => heading,
        }
        .render(settings, window, cx)
    }

    /// The heading band without a section: window movement and Window Controls only.
    fn render_bare_detail_heading(
        &mut self,
        settings: &SettingsAppearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let close_owner = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let handle = window.window_handle();
            let _ = close_owner.update(cx, |settings, cx| {
                settings.request_close(CloseIntent::Window(handle), cx)
            });
        });
        DetailHeading::new("settings", div(), &self.window_movement, close)
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
                    SettingsSectionId::Git => IconName::GitBranch,
                    SettingsSectionId::Servers => IconName::Terminal,
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
        // A search that matches nothing names no section: its result is the canvas's only
        // content, under a heading band that keeps only window movement and Window Controls.
        let (heading, bare_heading) = if empty {
            (
                None,
                Some(self.render_bare_detail_heading(settings, window, cx)),
            )
        } else {
            (Some(self.render_detail_heading(settings, window, cx)), None)
        };
        let revealing = cx.weak_entity();
        div()
            .debug_selector(|| "settings-canvas".to_owned())
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(gpui_color(
                settings.surface(SettingsSurfaceRole::Canvas).paint,
            ))
            .children(heading)
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
                                detail.size_full().child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .px(row_horizontal_inset(appearance))
                                        .child(no_results(&self.query, cx)),
                                )
                            }),
                    )
                    .child(self.scrollbar.clone()),
            )
            .children(
                bare_heading
                    .map(|heading| div().absolute().top_0().left_0().right_0().child(heading)),
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
        // Each group title is one card. A search orders rows by how well they match, which can
        // split a group's rows apart, so a later row joins the card its group already has.
        let mut groups: Vec<(&'static str, Vec<SettingsRowId>, Vec<AnyElement>)> = Vec::new();
        for &row in &rows {
            let title = row.descriptor().group;
            let ends = CardEnds::of(&rows, &row, |other| other.descriptor().group == title);
            let rendered = self.render_row(row, ends, appearance, window, cx);
            // Each paired device has a row of its own under Devices. They follow every catalog
            // row of their card, so the card's row bounds still pair with its catalog rows.
            let devices = if row == SettingsRowId::PairedDevices {
                self.render_device_rows(appearance, window, cx)
            } else {
                Vec::new()
            };
            match groups.iter_mut().find(|(current, _, _)| *current == title) {
                Some((_, ids, members)) => {
                    ids.push(row);
                    members.push(rendered);
                    members.extend(devices);
                }
                None => {
                    let mut members = vec![rendered];
                    members.extend(devices);
                    groups.push((title, vec![row], members));
                }
            }
        }
        let server_footnote = self.server_card_footnote(cx);
        let network_footnote = self.network_access_footnote(cx);
        let git_footnote = self.server_git_footnote();
        self.row_bounds.borrow_mut().clear();
        let rendered = groups
            .into_iter()
            .map(|(title, ids, members)| {
                // The gallery's title names the appearance it is showing, which changes with the
                // mode and, under Auto, with the slot chosen above it. Its selector stays fixed.
                let heading = if title == SettingsRowId::InstalledThemes.descriptor().group {
                    SharedString::new_static(self.installed_themes_title(cx))
                } else if matches!(section, SettingsSectionId::Servers | SettingsSectionId::Git) {
                    // A group acting on the selected server names it where the picker doesn't.
                    self.server_group_heading(title)
                } else {
                    SharedString::new_static(title)
                };
                // A card whose rows can't be used now says why, and what to do.
                let footnote = match (section, title) {
                    (SettingsSectionId::Servers, server_list::SERVER_GROUP) => {
                        server_footnote.clone()
                    }
                    (SettingsSectionId::Servers, server_list::NETWORK_GROUP) => {
                        network_footnote.clone()
                    }
                    (SettingsSectionId::Git, server_list::GIT_SERVER_GROUP) => git_footnote.clone(),
                    _ => None,
                };
                let row_bounds = Rc::clone(&self.row_bounds);
                let reveal_after_layout = Rc::clone(&self.reveal_after_layout);
                let scroll = self.scroll.clone();
                FormGroup::new(group_selector(section, title), heading, members)
                    .footnote(footnote)
                    .on_rows_prepainted(move |bounds, window, _| {
                        let revealed = reveal_after_layout
                            .get()
                            .and_then(|row| Some(bounds[ids.iter().position(|id| *id == row)?]));
                        row_bounds
                            .borrow_mut()
                            .extend(ids.iter().copied().zip(bounds));
                        if let Some(bounds) = revealed {
                            reveal_after_layout.set(None);
                            if reveal(&scroll, bounds) {
                                window.refresh();
                            }
                        }
                    })
                    .render(settings)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let notice = match section {
            SettingsSectionId::Themes => self.render_diagnostics_notice(appearance, cx),
            SettingsSectionId::Servers => self.render_server_notices(appearance, cx),
            _ => None,
        };
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
    /// Renders `row`, which touches the card edges `ends` names.
    fn render_row(
        &mut self,
        row: SettingsRowId,
        ends: CardEnds,
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
        if row == SettingsRowId::ServerStatus {
            self.track_server_progress(cx);
        }
        let control = self.render_control(row, ends, content_appearance, window, cx);
        let mut rendered = FormRow::new(
            descriptor.selector,
            descriptor.label(self.permission_access.naming()),
            control,
        )
        .layout(self.row_layout(row))
        .reset(self.row_reset(row, appearance, cx))
        .matched_indices(matched_indices)
        .highlighted(highlighted)
        .dimmed(self.row_dimmed(row));
        if let Some((description, message)) = self.server_row_text(row, cx) {
            rendered = rendered.description(description).message(message);
            if row == SettingsRowId::ServerStatus {
                rendered = rendered.status_line(self.server_progress_indicator(cx));
            }
        } else if row == SettingsRowId::UpdateStatus {
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
        ends: CardEnds,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRowId::AppearanceMode => self.render_appearance_mode(cx),
            SettingsRowId::Opacity => self.render_opacity(appearance, cx),
            SettingsRowId::Blur => self.render_blur(cx),
            SettingsRowId::BackgroundImage => self.render_background_image(cx),
            SettingsRowId::TerminalTheme => self.render_current_theme(appearance, cx),
            SettingsRowId::Density => self.render_density(cx),
            SettingsRowId::SidebarDefaultState => self.render_sidebar_default_state(cx),
            SettingsRowId::TerminalFontFamily => self.render_terminal_font(appearance, cx),
            SettingsRowId::TerminalBaseSize => self.render_terminal_size(appearance, cx),
            SettingsRowId::TerminalLineHeight => self.render_line_height(appearance, cx),
            SettingsRowId::TerminalRegularWeight | SettingsRowId::TerminalBoldWeight => {
                self.render_weight(row, appearance, cx)
            }
            SettingsRowId::TerminalItalic => self.render_italic(cx),
            SettingsRowId::TerminalBoldAsBright => self.render_bold_as_bright(cx),
            SettingsRowId::InstalledThemes => self.render_installed_themes(appearance, cx),
            SettingsRowId::MicrophoneAccess => {
                self.render_capture_access(CaptureDevice::Microphone, appearance, cx)
            }
            SettingsRowId::CameraAccess => {
                self.render_capture_access(CaptureDevice::Camera, appearance, cx)
            }
            SettingsRowId::ScreenRecordingAccess => {
                self.render_permission_access(SystemPermission::ScreenRecording, appearance, cx)
            }
            SettingsRowId::AccessibilityAccess => {
                self.render_permission_access(SystemPermission::Accessibility, appearance, cx)
            }
            SettingsRowId::ClipboardWrites | SettingsRowId::ClipboardReads => {
                self.render_clipboard_preference(row, cx)
            }
            SettingsRowId::ShowRepositoryStatus | SettingsRowId::ShowPullRequests => {
                self.render_git_preference(row, cx)
            }
            SettingsRowId::GitTool | SettingsRowId::GitHubCli => {
                self.render_git_tool(row, appearance, cx)
            }
            SettingsRowId::ServerWorktreeLocation => self.render_server_location(appearance, cx),
            SettingsRowId::ServerStatus => self.render_server_status(appearance, cx),
            SettingsRowId::ServerAutoStart => self.render_auto_start(appearance, cx),
            SettingsRowId::EndAllSessions => self.render_end_all_sessions(appearance, cx),
            SettingsRowId::NetworkAccess => self.render_network_access(appearance, cx),
            SettingsRowId::PairedDevices => self.render_paired_devices(appearance, cx),
            SettingsRowId::AllServers => self.render_all_servers(ends, appearance, window, cx),
            SettingsRowId::AddServer => self.render_add_server_row(ends, appearance, cx),
            SettingsRowId::CommandLineTool => self.render_command_line_tool(appearance, cx),
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
            ("Try Again", "settings-banner-retry")
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
        "This can\u{2019}t be undone. Server connections and system permissions are kept."
            .to_owned()
    }

    fn confirm_reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-reset-all"),
            "Reset all settings",
            "Reset All Settings?",
            "Settings and keyboard shortcuts return to their defaults, and installed terminal themes are removed.",
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
        SettingsRowId::AllServers | SettingsRowId::AddServer => FormRowLayout::Items,
        _ => FormRowLayout::Beside,
    }
}

impl SettingsWindow {
    /// How `row` arranges its label and content.
    fn row_layout(&self, row: SettingsRowId) -> FormRowLayout {
        row_layout(row)
    }

    /// Whether `row` can't be used now, because SpaceTerm isn't connected to the server it acts
    /// on. Its card's footnote says why.
    fn row_dimmed(&self, row: SettingsRowId) -> bool {
        match row {
            // This account's service changes automatic start without a connection. Terminal
            // Sessions need the server itself.
            SettingsRowId::EndAllSessions => !self.selected_server_live(),
            SettingsRowId::NetworkAccess | SettingsRowId::PairedDevices => {
                !self.network_access_live()
            }
            SettingsRowId::GitTool | SettingsRowId::ServerWorktreeLocation => {
                self.server_git_footnote().is_some()
            }
            _ => false,
        }
    }

    /// The one-line description of a Servers or Git row that acts on the selected server, and
    /// the error under its control, if any.
    fn server_row_text(
        &self,
        row: SettingsRowId,
        cx: &App,
    ) -> Option<(SharedString, Option<(SharedString, CaptionTone)>)> {
        let error = |message: Option<SharedString>| message.map(|text| (text, CaptionTone::Error));
        Some(match row {
            // The Server row keeps one line under its label; longer explanations go in the card's
            // footnote.
            SettingsRowId::ServerStatus => (self.server_subtitle(cx), None),
            SettingsRowId::ServerAutoStart => (
                self.auto_start_description(),
                error(self.auto_start_error()),
            ),
            SettingsRowId::EndAllSessions => (
                SharedString::new_static("Ending them keeps Workspaces."),
                self.end_all_sessions_caption(),
            ),
            SettingsRowId::NetworkAccess => (
                self.network_access_description(),
                error(self.network_access_error()),
            ),
            SettingsRowId::PairedDevices => (
                SharedString::new_static(
                    "A paired device can do anything this computer can on this server.",
                ),
                self.no_paired_devices_note()
                    .map(|text| (text, CaptionTone::Guidance)),
            ),
            SettingsRowId::ServerWorktreeLocation => (self.server_location_caption(cx).0, None),
            SettingsRowId::CommandLineTool => {
                let presentation = self.command_line_tool_presentation(cx)?;
                (presentation.explanation, presentation.caption)
            }
            _ => return None,
        })
    }

    /// One line of guidance for the rows that warrant it.
    fn row_description(&self, row: SettingsRowId, cx: &App) -> Option<&'static str> {
        match row {
            SettingsRowId::AppearanceMode => Some("Auto matches the system appearance."),
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
                            "Window transparency is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Opacity), false) => {
                            "Window transparency is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Opacity), true) if opacity => {
                            "Window transparency is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Opacity), true) => {
                            "Window transparency is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Blur), false) if opacity => {
                            "Desktop blur is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Blur), false) => {
                            "Desktop blur is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Blur), true) if opacity => {
                            "Desktop blur is unavailable on this system."
                        }
                        (Some(UnavailableWindowEffect::Blur), true) => {
                            "Desktop blur is unavailable on this system."
                        }
                        (None, _) if opacity && fully_opaque => {
                            "Decrease below 1 to see through windows."
                        }
                        (None, true) if opacity => "Accessibility settings keep windows opaque.",
                        (None, _) if opacity => "0 is transparent; 1 is opaque.",
                        (None, _) if fully_opaque => "Decrease Opacity below 1 to see blur.",
                        (None, true) => "Accessibility settings turn off blur.",
                        (None, false) => "Soften content behind windows.",
                    },
                )
            }
            SettingsRowId::BackgroundImage => Some(
                if super::appearance_runtime::current(cx)
                    .chrome
                    .composition
                    .effective
                    == crate::appearance::WindowBackgroundAppearance::Opaque
                {
                    "Shown behind windows. Decrease Opacity below 1 to see it."
                } else {
                    "Shown behind windows."
                },
            ),
            SettingsRowId::SidebarDefaultState => Some("The sidebar state in each new window."),
            SettingsRowId::TerminalFontFamily => Some("Only monospace fonts."),
            SettingsRowId::AutomaticUpdateDownloads
            | SettingsRowId::UpdateCheckInterval
            | SettingsRowId::UpdateReminderInterval => updates::update_row_description(row),
            SettingsRowId::MicrophoneAccess => {
                Some(self.microphone_access.presentation().explanation)
            }
            SettingsRowId::CameraAccess => Some(self.camera_access.presentation().explanation),
            SettingsRowId::ClipboardWrites => {
                Some("Programs in the Focused Pane can replace clipboard text, including over SSH.")
            }
            SettingsRowId::ClipboardReads => Some(
                "Programs in the Focused Pane can retrieve clipboard text, including remotely.",
            ),
            SettingsRowId::ShowRepositoryStatus => {
                Some("Show branch and changes in Pane Captions, and branch in sidebar rows.")
            }
            SettingsRowId::ShowPullRequests => {
                Some("Show open Pull Requests in the sidebar and Repository Status popover.")
            }
            SettingsRowId::GitTool | SettingsRowId::GitHubCli => {
                Some(self.git_tool_presentation(row, cx).explanation)
            }
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
