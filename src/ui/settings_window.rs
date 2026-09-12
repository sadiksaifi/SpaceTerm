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
mod schemes;

#[cfg(test)]
#[path = "settings_window/tests.rs"]
mod tests;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Entity, FocusHandle, Global, ScrollHandle, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions, div,
    px, size,
};
use spaceterm_ui::{
    Alert, AlertIntent, ComboBox, ComboBoxItem, Icon, IconButton, IconName, ModalAction,
    ModalActionEmphasis, ModalActionIntent, ModalActionRole, ModalId, ModalLayer, OverlayScrollbar,
    OverlayScrollbarEvent, ScrollMetrics, SegmentedControl, SegmentedOption, SegmentedSize, Switch,
    TextInput, TextInputEscapeBehavior, TextInputEvent, TextInputReturnBehavior, TextInputVariant,
    ToggleSize, TooltipLayer,
};

use crate::appearance::{
    Appearance, AppearanceDocument, ChromeDensity, ChromeFontFamily, Color, FontClass, ResetTarget,
    SchemeId, SchemeKind, SchemeSelection, TerminalFontFamily, builtin_fallback_scheme,
};
use crate::ui::appearance::ChromeAppearance;

use catalog::{ROWS, SettingsRowId, SettingsSectionId};
use controls::{
    SettingsRow, SettingsRowLayout, Stepper, action_button, gpui_color, reset_button,
    section_header,
};
use editor::{SaveStatus, SettingsEditor};

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

/// The one Settings Window, so a second request activates the existing window.
struct OpenSettingsWindow(WindowHandle<SettingsWindow>);
impl Global for OpenSettingsWindow {}

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
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT))),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: false,
                traffic_light_position: None,
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
            let settings = cx.new(|cx| SettingsWindow::new(window, cx));
            let closing = settings.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                // A change made moments ago has not been written yet. Writing it here is the only
                // point at which it can still reach the retained document.
                let _ = closing.update(cx, |settings, cx| settings.editor.flush(cx));
                let _ = window;
                true
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

/// Registers the application-scoped Settings actions.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| open_or_activate(cx));
}

/// The Light, Dark, or Auto choice governing which Color Scheme slot applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppearanceMode {
    Light,
    Dark,
    Auto,
}

impl AppearanceMode {
    fn of(selection: &SchemeSelection) -> Self {
        match selection {
            SchemeSelection::Fixed {
                appearance: Appearance::Light,
                ..
            } => Self::Light,
            SchemeSelection::Fixed {
                appearance: Appearance::Dark,
                ..
            } => Self::Dark,
            SchemeSelection::System { .. } => Self::Auto,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::Auto => "Auto",
        }
    }
}

/// The scheme a domain last used in each appearance slot.
///
/// The document stores one selection, so switching from Dark to Light and back would otherwise
/// forget the dark scheme. These live only as long as the window, which is long enough to make
/// switching non-destructive.
#[derive(Clone)]
struct RememberedSchemes {
    light: SchemeId,
    dark: SchemeId,
}

impl RememberedSchemes {
    fn capture(selection: &SchemeSelection, kind: SchemeKind) -> Self {
        let fallback = |appearance| builtin_fallback_scheme(kind, appearance);
        match selection {
            SchemeSelection::System { light, dark } => Self {
                light: light.clone(),
                dark: dark.clone(),
            },
            SchemeSelection::Fixed {
                id,
                appearance: Appearance::Light,
            } => Self {
                light: id.clone(),
                dark: fallback(Appearance::Dark),
            },
            SchemeSelection::Fixed {
                id,
                appearance: Appearance::Dark,
            } => Self {
                light: fallback(Appearance::Light),
                dark: id.clone(),
            },
        }
    }

    fn slot(&self, appearance: Appearance) -> &SchemeId {
        match appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
        }
    }

    fn remember(&mut self, appearance: Appearance, id: SchemeId) {
        match appearance {
            Appearance::Light => self.light = id,
            Appearance::Dark => self.dark = id,
        }
    }

    /// The selection a mode produces, preserving the other slot's choice.
    fn selection(&self, mode: AppearanceMode) -> SchemeSelection {
        match mode {
            AppearanceMode::Light => SchemeSelection::Fixed {
                id: self.light.clone(),
                appearance: Appearance::Light,
            },
            AppearanceMode::Dark => SchemeSelection::Fixed {
                id: self.dark.clone(),
                appearance: Appearance::Dark,
            },
            AppearanceMode::Auto => SchemeSelection::System {
                light: self.light.clone(),
                dark: self.dark.clone(),
            },
        }
    }
}

pub(crate) struct SettingsWindow {
    editor: SettingsEditor,
    search: Entity<TextInput>,
    query: SharedString,
    scroll: ScrollHandle,
    /// The scroll affordance for the detail pane, so a long section is visibly scrollable.
    scrollbar: Entity<OverlayScrollbar<f32>>,
    /// The section the detail pane presents. Navigation selects one view at a time.
    active_section: SettingsSectionId,
    /// The row Settings Search revealed, highlighted so the eye lands on it.
    revealed: Option<SettingsRowId>,
    chrome_schemes: RememberedSchemes,
    terminal_schemes: RememberedSchemes,
    interchange_status: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx
            .global::<crate::ui::appearance_runtime::AppearanceRuntime>()
            .settings
            .clone();
        let editor = SettingsEditor::new(settings);
        let preferences = &editor.document().preferences;
        let chrome_schemes =
            RememberedSchemes::capture(&preferences.chrome.scheme, SchemeKind::Chrome);
        let terminal_schemes =
            RememberedSchemes::capture(&preferences.terminal.scheme, SchemeKind::Terminal);
        // The window takes focus so its own shortcuts and Tab traversal resolve from the moment it
        // opens, rather than only after something inside it is clicked.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
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
            .input_length_limit(Some(128))
            .emit_programmatic_changes(true)
            .debug_selector("settings-search")
        });
        cx.subscribe(&search, |settings, search, event: &TextInputEvent, cx| {
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
        })
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
        cx.observe_global::<crate::ui::appearance_runtime::InstalledAppearance>(|settings, cx| {
            settings.editor.synchronize();
            cx.notify();
        })
        .detach();
        Self {
            editor,
            search,
            query: SharedString::default(),
            scroll: ScrollHandle::new(),
            scrollbar,
            active_section: SettingsSectionId::Appearance,
            revealed: None,
            chrome_schemes,
            terminal_schemes,
            interchange_status: None,
            focus_handle,
        }
    }

    fn close(&mut self, _: &CloseSettingsWindow, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.flush(cx);
        window.remove_window();
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

    /// Whether a row is meaningful for the current document.
    ///
    /// A fixed appearance selects one scheme; Auto selects one per slot. Presenting all three at
    /// once would imply choices the document cannot hold.
    fn row_applies(&self, row: SettingsRowId) -> bool {
        let preferences = &self.editor.document().preferences;
        let chrome_auto = matches!(preferences.chrome.scheme, SchemeSelection::System { .. });
        let terminal_auto = matches!(preferences.terminal.scheme, SchemeSelection::System { .. });
        match row {
            SettingsRowId::ChromeScheme => !chrome_auto,
            SettingsRowId::ChromeLightScheme | SettingsRowId::ChromeDarkScheme => chrome_auto,
            SettingsRowId::TerminalScheme => !terminal_auto,
            SettingsRowId::TerminalLightScheme | SettingsRowId::TerminalDarkScheme => terminal_auto,
            _ => true,
        }
    }

    fn edit(&mut self, edit: impl FnOnce(&mut AppearanceDocument), cx: &mut Context<Self>) {
        self.editor.edit(edit, cx);
    }

    /// Whether this row differs from its default, which is when a reset is worth offering.
    fn differs_from_default(&self, row: SettingsRowId) -> bool {
        let Some(target) = row.reset_target() else {
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
        let target = row.reset_target()?;
        let owner = cx.weak_entity();
        Some(
            reset_button(
                format!("{}-reset", row.descriptor().selector),
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
            .key_context(SETTINGS_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::clear_search))
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui_color(appearance.colors.background))
            .text_color(gpui_color(appearance.colors.text))
            .text_size(appearance.text_size(12.0))
            .font(appearance.regular.clone())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&appearance, cx))
                    .child(self.render_detail(&appearance, window, cx)),
            )
            .child(self.render_footer(&appearance, cx));
        ModalLayer::new(TooltipLayer::new(content))
    }
}

impl SettingsWindow {
    fn render_sidebar(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let matching = catalog::matching_rows(&self.query);
        let entries = SettingsSectionId::ALL
            .iter()
            .map(|section| {
                let section = *section;
                let has_matches = ROWS
                    .iter()
                    .any(|row| row.section == section && matching.contains(&row.id));
                let selected = self.active_section == section && has_matches;
                let owner = cx.weak_entity();
                div()
                    .id(SharedString::from(format!(
                        "settings-navigation-{}",
                        section.navigation_title()
                    )))
                    .debug_selector(move || format!("settings-navigation-{}", section.selector()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(appearance.spacing(7.0))
                    .w_full()
                    .h(appearance.height(28.0, 12.0))
                    .px(appearance.spacing(8.0))
                    .rounded(px(6.0))
                    .cursor_default()
                    // A navigation row is a chrome list row: the selected row takes the ghost
                    // selected fill and hover keeps precedence over it, as every other list in
                    // SpaceTerm does. The accent role stays reserved for accents such as the
                    // active Tab underline, which no scheme promises to be legible behind text.
                    .when(selected, |entry| {
                        entry
                            .bg(gpui_color(appearance.colors.ghost_element_selected))
                            .font(appearance.emphasis.clone())
                            .text_color(gpui_color(
                                appearance.colors.ghost_element_selected_foreground,
                            ))
                    })
                    .when(!selected && has_matches, |entry| {
                        entry.text_color(gpui_color(appearance.colors.text_secondary))
                    })
                    .when(has_matches, |entry| {
                        entry
                            .hover(|entry| {
                                entry
                                    .bg(gpui_color(appearance.colors.ghost_element_hover))
                                    .text_color(gpui_color(
                                        appearance.colors.ghost_element_hover_foreground,
                                    ))
                            })
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |settings, cx| {
                                    settings.reveal_section(section, cx)
                                });
                            })
                    })
                    .when(!has_matches, |entry| {
                        entry.text_color(gpui_color(appearance.colors.text_disabled))
                    })
                    .child(div().flex_none().child(Icon::new(
                        match section {
                            SettingsSectionId::Appearance => IconName::Palette,
                            SettingsSectionId::Terminal => IconName::Terminal,
                            SettingsSectionId::ColorSchemes => IconName::SunMoon,
                        },
                        appearance.text_size(13.0),
                        gpui_color(if !has_matches {
                            appearance.colors.icon_disabled
                        } else if selected {
                            appearance.colors.icon_accent
                        } else {
                            appearance.colors.icon_muted
                        }),
                    )))
                    .child(div().min_w_0().flex_1().child(section.navigation_title()))
            })
            .collect::<Vec<_>>();
        let owner = cx.weak_entity();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(appearance.text_size(SIDEBAR_WIDTH))
            .h_full()
            .gap(appearance.spacing(4.0))
            .p(appearance.spacing(10.0))
            .bg(gpui_color(appearance.colors.panel_background))
            .border_r_1()
            .border_color(gpui_color(appearance.colors.border))
            .child(self.render_search_field(appearance, cx))
            .children(entries)
            .child(div().flex_1())
            .child(action_button(
                "settings-reset-all",
                "Reset All…",
                self.editor.editable(),
                {
                    move |window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.confirm_reset_all(window, cx);
                        });
                    }
                },
            ))
            .into_any_element()
    }

    fn render_search_field(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.search.read(cx).is_focused();
        let owner = cx.weak_entity();
        div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(appearance.spacing(5.0))
            .px(appearance.spacing(7.0))
            .h(appearance.height(28.0, 12.0))
            // The search field belongs to the window, not to the navigation list under it, so the
            // break between them is wider than the spacing inside the list.
            .mb(appearance.spacing(12.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(gpui_color(if focused {
                appearance.colors.input_focused_border
            } else {
                appearance.colors.input_border
            }))
            .bg(gpui_color(appearance.colors.input_background))
            .child(div().flex_none().child(Icon::new(
                IconName::Search,
                appearance.text_size(11.0),
                gpui_color(appearance.colors.icon_muted),
            )))
            .child(div().min_w_0().flex_1().child(self.search.clone()))
            .when(!self.query.is_empty(), |field| {
                field.child(
                    IconButton::new("settings-search-clear", "Clear search", |foreground| {
                        Icon::new(IconName::X, px(10.0), foreground).into_any_element()
                    })
                    .variant(spaceterm_ui::ButtonVariant::Ghost)
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
                            .track_scroll(&self.scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .p(appearance.spacing(16.0))
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
        let rendered = rows
            .into_iter()
            .map(|row| self.render_row(row, appearance, window, cx))
            .collect::<Vec<_>>();
        div()
            .debug_selector(move || section.selector().to_owned())
            .flex()
            .flex_col()
            .w_full()
            .gap(appearance.spacing(2.0))
            .child(section_header(
                section.selector(),
                section.title(),
                section.description(),
                appearance,
            ))
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
        let _ = window;
        let descriptor = row.descriptor();
        let control = self.render_control(row, appearance, cx);
        let mut rendered = SettingsRow::new(descriptor.selector, descriptor.label, control)
            .layout(row_layout(row))
            .reset(self.row_reset(row, cx))
            .highlighted(!self.query.is_empty() && self.revealed == Some(row));
        if let Some(description) = row_description(row) {
            rendered = rendered.description(description);
        }
        rendered.render(appearance).into_any_element()
    }

    fn render_control(
        &mut self,
        row: SettingsRowId,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            SettingsRowId::ChromeAppearanceMode => {
                self.render_appearance_mode(SchemeKind::Chrome, appearance, cx)
            }
            SettingsRowId::TerminalAppearanceMode => {
                self.render_appearance_mode(SchemeKind::Terminal, appearance, cx)
            }
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
            SettingsRowId::InstalledSchemes => self.render_installed_schemes(appearance, cx),
            SettingsRowId::SchemeInterchange => self.render_scheme_interchange(appearance, cx),
            SettingsRowId::AppearanceDiagnostics => {
                self.render_appearance_diagnostics(appearance, cx)
            }
        }
    }

    fn render_appearance_mode(
        &mut self,
        kind: SchemeKind,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preferences = &self.editor.document().preferences;
        let selection = match kind {
            SchemeKind::Chrome => &preferences.chrome.scheme,
            SchemeKind::Terminal => &preferences.terminal.scheme,
        };
        let current = AppearanceMode::of(selection);
        let selector = match kind {
            SchemeKind::Chrome => "settings-chrome-appearance-mode",
            SchemeKind::Terminal => "settings-terminal-appearance-mode",
        };
        let owner = cx.weak_entity();
        let options = [
            AppearanceMode::Light,
            AppearanceMode::Dark,
            AppearanceMode::Auto,
        ]
        .into_iter()
        .map(|mode| {
            let swatches = self.mode_preview_colors(kind, mode, cx);
            SegmentedOption::new(mode, mode.label())
                .debug_selector(format!("{selector}-{}", mode.label().to_ascii_lowercase()))
                .preview(move |_, extent| mode_preview(&swatches, extent).into_any_element())
        })
        .collect::<Vec<_>>();
        let control = SegmentedControl::new(selector, "Appearance", &current, options)
            .expect("three appearance modes are within the bounded option set")
            .size(SegmentedSize::Card)
            .disabled(!self.editor.editable())
            .debug_selector(selector)
            .on_change(move |change, _, cx| {
                let mode = *change.requested();
                let _ = owner.update(cx, |settings, cx| {
                    settings.set_appearance_mode(kind, mode, cx)
                });
            });
        let _ = appearance;
        control.into_any_element()
    }

    /// Representative colors for one mode's preview card.
    fn mode_preview_colors(
        &self,
        kind: SchemeKind,
        mode: AppearanceMode,
        cx: &mut Context<Self>,
    ) -> Vec<Color> {
        let _ = cx;
        let remembered = self.remembered(kind);
        let pick = |appearance| {
            self.editor
                .scheme_summaries(kind)
                .ok()
                .and_then(|summaries| {
                    summaries
                        .into_iter()
                        .find(|summary| summary.id == *remembered.slot(appearance))
                })
                .map(|summary| summary.swatches)
                .unwrap_or_default()
        };
        match mode {
            AppearanceMode::Light => pick(Appearance::Light),
            AppearanceMode::Dark => pick(Appearance::Dark),
            AppearanceMode::Auto => {
                let light = pick(Appearance::Light);
                let dark = pick(Appearance::Dark);
                let half = light.len().div_ceil(2);
                light.into_iter().take(half).chain(dark).collect()
            }
        }
    }

    fn remembered(&self, kind: SchemeKind) -> &RememberedSchemes {
        match kind {
            SchemeKind::Chrome => &self.chrome_schemes,
            SchemeKind::Terminal => &self.terminal_schemes,
        }
    }

    fn set_appearance_mode(
        &mut self,
        kind: SchemeKind,
        mode: AppearanceMode,
        cx: &mut Context<Self>,
    ) {
        let selection = self.remembered(kind).selection(mode);
        self.edit(
            move |draft| match kind {
                SchemeKind::Chrome => draft.preferences.chrome.scheme = selection,
                SchemeKind::Terminal => draft.preferences.terminal.scheme = selection,
            },
            cx,
        );
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
        let selection = match kind {
            SchemeKind::Chrome => &preferences.chrome.scheme,
            SchemeKind::Terminal => &preferences.terminal.scheme,
        };
        // A fixed selection restricts the list to its own appearance so a chosen scheme always
        // matches the mode; Auto restricts each slot to that slot's appearance.
        let (current, restrict) = match (selection, slot) {
            (SchemeSelection::Fixed { id, appearance }, _) => (id.clone(), *appearance),
            (SchemeSelection::System { light, .. }, Some(Appearance::Light)) => {
                (light.clone(), Appearance::Light)
            }
            (SchemeSelection::System { dark, .. }, _) => (dark.clone(), Appearance::Dark),
        };
        let summaries = self
            .editor
            .scheme_summaries(kind)
            .unwrap_or_default()
            .into_iter()
            .filter(|summary| summary.appearance == restrict)
            .collect::<Vec<_>>();
        // The control carries its own selector so it never collides with its row's.
        let selector = control_selector(row);
        let items = summaries
            .iter()
            .map(|summary| {
                ComboBoxItem::new(summary.id.clone(), SharedString::from(summary.name.clone()))
                    .debug_selector(format!("{selector}-{}", summary.id.as_str()))
            })
            .collect::<Vec<_>>();
        let owner = cx.weak_entity();
        settings_selector(
            selector,
            row.descriptor().label,
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
        match kind {
            SchemeKind::Chrome => self.chrome_schemes.remember(slot, id.clone()),
            SchemeKind::Terminal => self.terminal_schemes.remember(slot, id.clone()),
        }
        self.edit(
            move |draft| {
                let selection = match kind {
                    SchemeKind::Chrome => &mut draft.preferences.chrome.scheme,
                    SchemeKind::Terminal => &mut draft.preferences.terminal.scheme,
                };
                match selection {
                    SchemeSelection::Fixed { id: current, .. } => *current = id,
                    SchemeSelection::System { light, dark } => match slot {
                        Appearance::Light => *light = id,
                        Appearance::Dark => *dark = id,
                    },
                }
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
        .render(appearance)
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
        .render(appearance)
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
        let items = WEIGHTS
            .iter()
            .map(|(weight, label)| {
                ComboBoxItem::new(*weight, *label).debug_selector(format!("{selector}-{weight}"))
            })
            .collect::<Vec<_>>();
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
                .w_full()
                .gap(appearance.spacing(8.0))
                .p(appearance.spacing(10.0))
                .bg(gpui_color(if critical {
                    appearance.colors.warning_background
                } else {
                    appearance.colors.error_background
                }))
                .border_b_1()
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
                                .text_size(appearance.text_size(11.0))
                                .text_color(gpui_color(appearance.colors.text_secondary))
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

    fn render_footer(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = cx;
        let status = self.editor.status();
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .w_full()
            .flex_none()
            .h(appearance.height(28.0, 11.0))
            .px(appearance.spacing(14.0))
            .bg(gpui_color(appearance.colors.panel_background))
            .border_t_1()
            .border_color(gpui_color(appearance.colors.border))
            .child(
                div()
                    .debug_selector(|| "settings-save-status".to_owned())
                    .text_size(appearance.text_size(11.0))
                    .text_color(gpui_color(match status {
                        SaveStatus::Saved | SaveStatus::Saving => appearance.colors.text_muted,
                        SaveStatus::Failed(_) => appearance.colors.error,
                        SaveStatus::Unavailable(_) => appearance.colors.warning,
                    }))
                    .child(status.message()),
            )
            .into_any_element()
    }

    fn confirm_reset_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new("settings-reset-all"),
            "Reset all appearance settings",
            "Reset All Appearance Settings",
            "Every appearance choice returns to its default. Color schemes you imported are kept.",
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
                settings.editor.reset(ResetTarget::AllAppearance, cx);
                let preferences = &settings.editor.document().preferences;
                settings.chrome_schemes =
                    RememberedSchemes::capture(&preferences.chrome.scheme, SchemeKind::Chrome);
                settings.terminal_schemes =
                    RememberedSchemes::capture(&preferences.terminal.scheme, SchemeKind::Terminal);
                cx.notify();
            });
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm settings reset confirmation");
        }
    }
}

/// A miniature scheme preview for one appearance-mode card.
fn mode_preview(swatches: &[Color], extent: gpui::Pixels) -> impl IntoElement {
    let background = swatches.first().copied().unwrap_or(Color::rgb(0x000000));
    let bar = swatches.get(1).copied().unwrap_or(Color::rgb(0xffffff));
    let accent = swatches.get(3).copied().unwrap_or(bar);
    div()
        .w(extent * 1.5)
        .h(extent)
        .rounded(px(4.0))
        .overflow_hidden()
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
/// The Color Schemes rows present lists and button groups rather than one control, so they take
/// the whole row and carry their label above it.
fn row_layout(row: SettingsRowId) -> SettingsRowLayout {
    match row {
        SettingsRowId::InstalledSchemes
        | SettingsRowId::SchemeInterchange
        | SettingsRowId::AppearanceDiagnostics => SettingsRowLayout::Above,
        _ => SettingsRowLayout::Beside,
    }
}

/// One line of guidance for the rows that warrant it.
fn row_description(row: SettingsRowId) -> Option<&'static str> {
    match row {
        SettingsRowId::ChromeAppearanceMode => {
            Some("Auto follows the system light and dark setting.")
        }
        SettingsRowId::TerminalAppearanceMode => {
            Some("The terminal follows its own setting, not the application's.")
        }
        SettingsRowId::TerminalFontFamily => Some("Only monospaced families are listed."),
        SettingsRowId::InstalledSchemes => {
            Some("Built-in schemes are always available. Imported schemes can be removed.")
        }
        _ => None,
    }
}
