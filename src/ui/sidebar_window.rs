//! The sidebar window layout shared by the Settings Window and the Developer Workbench.
//! It owns geometry, navigation, and window movement; the owner keeps sections and policy.

pub(crate) mod form;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, accesskit, Bounds, Div, Edges, FocusHandle, Pixels, SharedString, Size, TitlebarOptions,
    Window, WindowBounds, WindowKind, WindowOptions, div, px,
};
use spaceterm_ui::{
    Button, ButtonMetrics, ButtonPaint, ButtonVariantStyle, ClientWindowControls, HoverFade, Icon,
    IconName, WindowCloseHandler, WindowDragRegion, WindowDragRegionEvent,
    WindowDragRegionResponse,
};

use crate::platform::window_movement::{
    OperatingSystemWindowDragError, OperatingSystemWindowDragPlatform,
};
use crate::ui::appearance::settings::{SettingsAppearance, SettingsSurfaceRole};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};

const SIDEBAR_WIDTH: f32 = 196.0;
const SIDEBAR_INSET: f32 = 10.0;
/// The strip under the content column carrying its status.
const FOOTER_HEIGHT: f32 = 40.0;
/// The height of one navigation entry and of a search field above the list, so the sidebar runs on
/// one rhythm from its first row to its last.
const NAVIGATION_ROW_HEIGHT: f32 = 28.0;

/// The space between the content column's edge and the text inside it.
pub(crate) const CONTENT_GUTTER: f32 = 26.0;

/// The space between consecutive groups in the content column.
const GROUP_SPACING: f32 = 26.0;

/// The row client-drawn Window Controls center in: the sidebar's first row with its inset above and
/// below, so the controls share a center line with Search, the way a desktop header bar holds both.
fn client_control_row_height(appearance: &ChromeAppearance) -> Pixels {
    appearance.spacing(SIDEBAR_INSET * 2.0 + NAVIGATION_ROW_HEIGHT)
}

/// The heading's distance from the window's top edge, which it shares with the traffic lights.
const HEADING_TOP_INSET: f32 = 20.0;

/// The space between the content column's edge and the cards standing in it: the content gutter
/// less the inset a row carries.
pub(crate) fn card_gutter(appearance: &ChromeAppearance) -> Pixels {
    appearance.spacing(CONTENT_GUTTER) - form::row_horizontal_inset(appearance)
}

pub(crate) fn group_spacing(appearance: &ChromeAppearance) -> Pixels {
    appearance.spacing(GROUP_SPACING)
}

/// The options every sidebar window opens with. It never orders itself above other applications, so
/// a system permission prompt a row raises appears above it.
pub(crate) fn window_options(title: &'static str, size: Size<Pixels>, cx: &App) -> WindowOptions {
    let titlebar_height = crate::ui::appearance::chrome(cx).top_height();
    let traffic_light_position = cx
        .try_global::<crate::platform::window_frame::WindowFrameGeometry>()
        .and_then(|geometry| geometry.sidebar_window_traffic_light_position(titlebar_height));
    let bounds = Bounds::centered(None, size, cx);
    cx.global::<crate::platform::window_chrome::WindowChrome>()
        .options(
            crate::platform::window_chrome::WindowRole::SidebarWindow,
            WindowOptions {
                app_id: crate::app::window_application_id(),
                window_background: crate::ui::appearance_runtime::window_background(cx),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    // Retain the native title for the Window menu and accessibility while drawing the
                    // visible section title in the client surface.
                    appears_transparent: true,
                    traffic_light_position,
                }),
                kind: WindowKind::Normal,
                is_movable: true,
                is_resizable: false,
                is_minimizable: false,
                tabbing_identifier: None,
                ..WindowOptions::default()
            },
            cx,
        )
}

/// Renders a sidebar window's surface inside its window-activity and control-theme scopes.
pub(crate) fn render_scoped(
    activity: spaceterm_ui::ControlWindowActivity,
    render: impl FnOnce() -> AnyElement,
) -> AnyElement {
    let scope = spaceterm_ui::ControlThemeScope::Settings;
    activity
        .mount(activity.with_scope(|| scope.mount(scope.with_scope(render))))
        .into_any_element()
}

/// The section model a sidebar window's owner provides to the shared navigation behavior.
pub(crate) trait SidebarOwner: Sized + 'static {
    type Section: Copy + PartialEq + 'static;

    fn navigation(&mut self) -> &mut SidebarNavigation;

    fn active_section(&self) -> Self::Section;

    /// The sections that currently have something to present, in navigation order.
    fn navigable_sections(&self) -> Vec<Self::Section>;

    /// Presents one section. Each section is its own view rather than a scroll destination.
    fn select_section(&mut self, section: Self::Section, cx: &mut Context<Self>);
}

/// Keyboard state of the sidebar's navigation list. Pointer selection withdraws the focus emphasis
/// so a click leaves no focus-like state behind.
pub(crate) struct SidebarNavigation {
    list_focus: FocusHandle,
    window_focus: FocusHandle,
    focus_visible: bool,
}

impl SidebarNavigation {
    pub(crate) fn new<T: 'static>(
        window_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let list_focus = cx.focus_handle().tab_stop(true);
        cx.on_focus(&list_focus, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&list_focus, window, |_, _, cx| cx.notify())
            .detach();
        Self {
            list_focus,
            window_focus,
            focus_visible: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn list_focus(&self) -> &FocusHandle {
        &self.list_focus
    }

    #[cfg(test)]
    pub(crate) fn focus_visible(&self) -> bool {
        self.focus_visible
    }

    pub(crate) fn has_visible_focus(&self, window: &Window) -> bool {
        self.list_focus.is_focused(window) && self.focus_visible
    }

    /// Records keyboard traversal, which is the only evidence that focus emphasis belongs on the
    /// list. Neither a focus nor a blur notification says what caused it.
    pub(crate) fn show_focus(&mut self) {
        self.focus_visible = true;
    }

    /// Records pointer selection: the window takes focus back and the list shows no emphasis.
    pub(crate) fn release_to_window(&mut self, window: &mut Window, cx: &mut App) {
        self.focus_visible = false;
        self.window_focus.focus(window, cx);
    }
}

/// One navigation entry.
pub(crate) struct NavigationEntry<S> {
    pub(crate) section: S,
    pub(crate) title: &'static str,
    /// The section's stable selector, such as `settings-section-font`.
    pub(crate) selector: &'static str,
    pub(crate) icon: IconName,
    /// Whether the section has anything to present. An unavailable entry stays listed but cannot
    /// be chosen.
    pub(crate) available: bool,
}

/// Moves the navigation selection with the keyboard, skipping unavailable sections. The list
/// activates as it moves.
fn navigate<T: SidebarOwner>(
    owner: &mut T,
    event: &gpui::KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    if !owner.navigation().list_focus.is_focused(window) || event.keystroke.modifiers.modified() {
        return;
    }
    let available = owner.navigable_sections();
    let active = owner.active_section();
    let Some(current) = available.iter().position(|section| *section == active) else {
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
    owner.navigation().show_focus();
    if next != current {
        owner.select_section(available[next], cx);
    }
    cx.notify();
}

/// Gives a sidebar window's root surface its Tab traversal. Unbound Tab is recorded before child
/// key handlers so keyboard modality comes from the key itself.
pub(crate) fn tab_traversal<T: SidebarOwner>(surface: Div, cx: &mut Context<T>) -> Div {
    surface
        .capture_key_down(
            cx.listener(|owner: &mut T, event: &gpui::KeyDownEvent, _, cx| {
                if is_plain_tab(event) && !owner.navigation().focus_visible {
                    owner.navigation().show_focus();
                    cx.notify();
                }
            }),
        )
        .on_key_down(|event: &gpui::KeyDownEvent, window, cx| {
            if !is_plain_tab(event) {
                return;
            }
            if event.keystroke.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            window.prevent_default();
            cx.stop_propagation();
        })
}

fn is_plain_tab(event: &gpui::KeyDownEvent) -> bool {
    let modifiers = event.keystroke.modifiers;
    event.keystroke.key == "tab"
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.platform
        && !modifiers.function
}

/// Native window movement from a sidebar window's client chrome. Search, toolbars, and other
/// controls stay outside drag ownership.
#[derive(Clone)]
pub(crate) struct WindowMovement {
    platform: Rc<dyn OperatingSystemWindowDragPlatform>,
    window_focus: FocusHandle,
    window_name: &'static str,
}

impl WindowMovement {
    pub(crate) fn new(
        platform: Rc<dyn OperatingSystemWindowDragPlatform>,
        window_focus: FocusHandle,
        window_name: &'static str,
    ) -> Self {
        Self {
            platform,
            window_focus,
            window_name,
        }
    }

    /// Wraps `content` as window-movement space whose uncovered area behaves as the titlebar.
    pub(super) fn region(
        &self,
        id: String,
        content: impl IntoElement,
        pointer_insets: Edges<Pixels>,
        window: &Window,
    ) -> WindowDragRegion {
        let movement = self.clone();
        WindowDragRegion::new(
            SharedString::from(id.clone()),
            format!(
                "Move Operating-System Window from {} chrome",
                self.window_name
            ),
            content,
        )
        .middle_activation(matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        ))
        .pointer_insets(pointer_insets)
        .debug_selector(id)
        .on_event(move |event, window, cx| movement.handle(*event, window, cx))
    }

    fn handle(
        &self,
        event: WindowDragRegionEvent,
        window: &mut Window,
        cx: &mut App,
    ) -> WindowDragRegionResponse {
        match event {
            WindowDragRegionEvent::InteractionStarted { .. } => {
                if !spaceterm_ui::window_modal_is_open(window, cx) {
                    self.window_focus.focus(window, cx);
                }
                if let Err(error) = self.platform.interaction_started() {
                    self.report("begin", error);
                }
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::MoveRequested { .. } => {
                match self.platform.start_window_move(window) {
                    Ok(()) => WindowDragRegionResponse::OperatingSystemWindowMoveStarted,
                    Err(error) => {
                        self.report("start", error);
                        WindowDragRegionResponse::Continue
                    }
                }
            }
            WindowDragRegionEvent::MiddleActivationRequested => {
                window.titlebar_middle_click();
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::DoubleActivationRequested => {
                window.titlebar_double_click();
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::SecondaryActivationRequested { position } => {
                self.platform.show_window_menu(window, position);
                WindowDragRegionResponse::Continue
            }
            WindowDragRegionEvent::InteractionFinished { .. } => {
                self.platform.interaction_finished();
                WindowDragRegionResponse::Continue
            }
        }
    }

    fn report(&self, operation: &str, error: OperatingSystemWindowDragError) {
        eprintln!(
            "failed to {operation} {} Window drag: {error}",
            self.window_name
        );
    }
}

/// The chip a navigation entry rests its hover and its current-section state on.
fn navigation_chip(
    selected: bool,
    available: bool,
    emphasized: bool,
    appearance: &ChromeAppearance,
    selection_colors: &crate::appearance::ChromeColors,
) -> SelectionChip {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Panel);
    let paint_colors = if selected { selection_colors } else { colors };
    let paint = navigation_chip_paint(selected, available, paint_colors);
    let paint = if selected && emphasized {
        // The accent is opaque, so the window's material does not thin it.
        paint
    } else if selected {
        paint.selected_on(appearance, colors.panel_background)
    } else {
        paint.raised_on(appearance, colors.panel_background)
    };
    SelectionChip::new(
        ChipShape::symmetric(px(0.0), px(0.0), RadiusRole::Control.pixels()),
        paint,
    )
}

pub(crate) fn navigation_chip_paint(
    selected: bool,
    available: bool,
    colors: &crate::appearance::ChromeColors,
) -> ChipPaint {
    // Hover changes the fill. Keyboard focus changes the selection colors rather than adding a
    // rim, so pointer selection does not leave a focus-like edge behind it.
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
            // An unavailable section cannot be chosen, so nothing lights under the pointer.
            hover_fill: available.then_some(colors.row_hover_background),
            hover_rim: None,
        }
    }
}

/// The sidebar column: the traffic-light strip, an optional header such as a search field, the
/// navigation list, and an optional footer at the bottom edge.
pub(crate) struct Sidebar<'a, T: SidebarOwner> {
    prefix: &'static str,
    entries: Vec<NavigationEntry<T::Section>>,
    header: Option<AnyElement>,
    footer: Option<AnyElement>,
    movement: &'a WindowMovement,
    close: Option<WindowCloseHandler>,
}

impl<'a, T: SidebarOwner> Sidebar<'a, T> {
    /// `prefix` names the window in every selector, such as `settings-sidebar`.
    pub(crate) fn new(
        prefix: &'static str,
        entries: Vec<NavigationEntry<T::Section>>,
        movement: &'a WindowMovement,
    ) -> Self {
        Self {
            prefix,
            entries,
            header: None,
            footer: None,
            movement,
            close: None,
        }
    }

    pub(crate) fn window_controls(mut self, close: WindowCloseHandler) -> Self {
        self.close = Some(close);
        self
    }

    pub(crate) fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(header.into_any_element());
        self
    }

    /// Quiet items that sit apart from the sections, at the bottom of the column.
    pub(crate) fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    pub(crate) fn render(
        self,
        owner: &mut T,
        surface: &SettingsAppearance,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> AnyElement {
        let appearance = &surface.chrome;
        let prefix = self.prefix;
        // Traffic lights need their own strip. Client-drawn controls need one only when they lead;
        // otherwise the strip shrinks to the inset above Search, which stays window-movement space.
        let client = matches!(
            window.window_decorations(),
            gpui::Decorations::Client { .. }
        );
        let leading_controls = self.close.is_some()
            && ClientWindowControls::width(spaceterm_ui::WindowControlSide::Left, window, cx)
                > px(0.0);
        let titlebar_height = match (client, leading_controls) {
            (false, _) => appearance.top_height(),
            (true, true) => client_control_row_height(appearance),
            (true, false) => appearance.spacing(SIDEBAR_INSET),
        };
        let titlebar = div()
            .debug_selector(move || format!("{prefix}-sidebar-titlebar"))
            .flex_none()
            .w_full()
            .h(titlebar_height)
            .child(
                self.movement.region(
                    format!("{prefix}-sidebar-drag-region"),
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .pl(px(spaceterm_ui::DesktopWindowStyle::current(cx)
                            .control_metrics()
                            .edge_margin))
                        .when_some(self.close, |titlebar, close| {
                            titlebar.child(
                                ClientWindowControls::new(close)
                                    .side(spaceterm_ui::WindowControlSide::Left)
                                    .surface_color(gpui_color(
                                        appearance.colors.title_bar_background,
                                    )),
                            )
                        }),
                    Edges {
                        left: cx
                            .try_global::<crate::platform::window_frame::WindowFrameGeometry>()
                            .and_then(|geometry| geometry.sidebar_window_titlebar_clearance())
                            .unwrap_or(px(0.0)),
                        ..Edges::default()
                    },
                    window,
                ),
            );
        let list = render_navigation_list(prefix, self.entries, owner, appearance, window, cx);
        let sidebar = div()
            .debug_selector(move || format!("{prefix}-sidebar"))
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(appearance.spacing(SIDEBAR_WIDTH))
            .h_full()
            .bg(gpui_color(
                surface.surface(SettingsSurfaceRole::Sidebar).paint,
            ))
            // Paint the boundary inside the sidebar without changing column widths.
            .when_some(surface.sidebar_edge(), |sidebar, edge| {
                sidebar.child(
                    div()
                        .debug_selector(move || format!("{prefix}-sidebar-divider"))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(px(super::control_theme::resize_handle::VISIBLE_THICKNESS))
                        .bg(gpui_color(edge)),
                )
            })
            .child(titlebar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .px(appearance.spacing(SIDEBAR_INSET))
                    .when(!client || leading_controls, |column| {
                        column.pt(appearance.spacing(SIDEBAR_INSET))
                    })
                    .children(self.header.map(|header| {
                        // The header belongs to the window, not to the list under it, so the break
                        // between them is wider than the spacing inside the list.
                        div().w_full().mb(appearance.spacing(12.0)).child(header)
                    }))
                    .child(list)
                    .children(self.footer.map(|footer| {
                        div()
                            .debug_selector(move || format!("{prefix}-sidebar-footer"))
                            .flex()
                            .items_center()
                            .flex_none()
                            .w_full()
                            .h(footer_height(appearance))
                            .mt_auto()
                            .child(footer)
                    })),
            );
        spaceterm_ui::ControlHost::Panel
            .mount(sidebar)
            .into_any_element()
    }
}

/// A quiet command at the foot of the sidebar, such as About. It dispatches its action instead of
/// selecting a section.
pub(crate) fn render_footer_action(
    selector: String,
    label: SharedString,
    icon: IconName,
    action: Box<dyn gpui::Action>,
    appearance: &ChromeAppearance,
) -> AnyElement {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Panel);
    // The chip a section row rests on, so the action lights exactly as a row does.
    let chip =
        navigation_chip_paint(false, true, colors).raised_on(appearance, colors.panel_background);
    let fill = |fill: Option<crate::appearance::Color>| fill.map_or(gpui::rgba(0), gpui_color);
    let paint = |background, foreground, icon| {
        ButtonPaint::new(background, gpui_color(foreground), gpui::rgba(0))
            .icon_foreground(gpui_color(icon))
    };
    let normal = paint(fill(chip.fill), colors.text_secondary, colors.row_icon);
    // An inactive window does not light under the pointer.
    let hovered = if appearance.active {
        paint(
            fill(chip.hover_fill),
            colors.row_hover_foreground,
            colors.row_hover_icon,
        )
    } else {
        normal
    };
    let text = appearance.typography.style(TextRole::Secondary);
    let line_height = f32::from(text.line_height) / f32::from(text.size);
    let metrics = ButtonMetrics::new(navigation_row_height(appearance))
        .horizontal_padding(appearance.spacing(8.0))
        .gap(appearance.spacing(7.0))
        .corner_radius(RadiusRole::Control.pixels())
        .border_width(px(0.0))
        .font_size(text.size)
        .line_heights(line_height, line_height);
    let glyph_size = appearance.icons.metrics(IconRole::Row).glyph_size;
    let activated = action.boxed_clone();
    // The shared button publishes no accessibility node of its own, so the row names it, the way
    // client-drawn Window Controls do.
    div()
        .id(SharedString::from(format!("{selector}-accessible")))
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, cx| {
            window.dispatch_action(action.boxed_clone(), cx)
        })
        .w_full()
        .child(
            Button::new(SharedString::from(selector.clone()), label)
                .contextual_style(
                    ButtonVariantStyle::new(normal, hovered, hovered, normal),
                    gpui_color(colors.focus_ring),
                )
                .contextual_metrics(metrics)
                .full_width(true)
                .align_start()
                .tab_stop(true)
                .debug_selector(selector)
                .leading(move |color| Icon::new(icon, glyph_size, color).into_any_element())
                .on_activate(move |_, window, cx| {
                    window.dispatch_action(activated.boxed_clone(), cx)
                }),
        )
        .into_any_element()
}

/// The height of a navigation entry, which a footer action shares.
fn navigation_row_height(appearance: &ChromeAppearance) -> Pixels {
    appearance
        .typography
        .style(TextRole::Navigation)
        .line_height
        + appearance.spacing(NAVIGATION_ROW_HEIGHT - 16.0)
}

fn render_navigation_list<T: SidebarOwner>(
    prefix: &'static str,
    entries: Vec<NavigationEntry<T::Section>>,
    owner: &mut T,
    appearance: &ChromeAppearance,
    window: &mut Window,
    cx: &mut Context<T>,
) -> AnyElement {
    let active = owner.active_section();
    let navigation = owner.navigation();
    let list_focused = navigation.has_visible_focus(window);
    let list_focus = navigation.list_focus.clone();
    let list_holds_focus = list_focus.is_focused(window);
    let any_available = entries.iter().any(|entry| entry.available);
    let panel_colors = appearance.host_colors(spaceterm_ui::ControlHost::Panel);
    // AppKit source lists draw no focus ring. Keyboard focus emphasizes the selection in the
    // accent color instead, and a list without focus shows it in the unfocused colors.
    let emphasized = list_focused && appearance.active;
    let selection_colors = if emphasized {
        crate::ui::selection_chip::emphasized_selection_colors(panel_colors)
    } else if appearance.active {
        appearance
            .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel)
            .clone()
    } else {
        panel_colors.clone()
    };
    let rows = entries
        .into_iter()
        .map(|entry| {
            let NavigationEntry {
                section,
                title,
                selector,
                icon,
                available,
            } = entry;
            let selected = active == section && available;
            let fade = HoverFade::new(
                SharedString::from(format!("{prefix}-navigation-hover-{selector}")),
                window,
                cx,
            );
            let hover = if available && appearance.active {
                fade.level(window, cx)
            } else {
                0.0
            };
            let colors = panel_colors;
            let (foreground, icon_color, hover_foreground, hover_icon) = if selected {
                (
                    selection_colors.row_selected_foreground,
                    selection_colors.row_selected_icon,
                    selection_colors.row_selected_hover_foreground,
                    selection_colors.row_selected_hover_icon,
                )
            } else {
                (
                    colors.row_foreground,
                    colors.row_icon,
                    colors.row_hover_foreground,
                    colors.row_hover_icon,
                )
            };
            let chip = navigation_chip(
                selected,
                available,
                emphasized,
                appearance,
                &selection_colors,
            );
            // Text and icon follow the chip's hover paint.
            let foreground = foreground.fade(hover_foreground, f64::from(hover));
            let icon_color = icon_color.fade(hover_icon, f64::from(hover));
            let chip_selector = format!("{prefix}-navigation-chip-{selector}");
            let selecting = cx.weak_entity();
            let pressing = cx.weak_entity();
            div()
                .id(SharedString::from(format!("{prefix}-navigation-{title}")))
                .role(accesskit::Role::ListBoxOption)
                .aria_label(SharedString::from(title))
                .aria_selected(selected)
                .aria_disabled(!available)
                // The list holds keyboard focus, so its selection is the focused option.
                .when(selected && list_holds_focus, |row| row.aria_active_descendant())
                .when(available, |row| {
                    row.on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                        let _ = pressing.update(cx, |owner, cx| {
                            owner.navigation().release_to_window(window, cx);
                            owner.select_section(section, cx);
                        });
                    })
                })
                .debug_selector(move || format!("{prefix}-navigation-{selector}"))
                .relative()
                .text_color(gpui_color(foreground))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(7.0))
                .w_full()
                .h(navigation_row_height(appearance))
                .px(appearance.spacing(8.0))
                .cursor_default()
                .chrome_text(appearance.typography.style(TextRole::Navigation))
                .child(chip.render(chip_selector, hover))
                .when(available, |row| {
                    row.child(fade.tracker()).on_click(move |_, window, cx| {
                        let _ = selecting.update(cx, |owner, cx| {
                            // A completed pointer selection is authoritative even if native
                            // focus moved between the press and release.
                            owner.navigation().release_to_window(window, cx);
                            owner.select_section(section, cx);
                        });
                    })
                })
                .when(!available, |row| {
                    row.text_color(gpui_color(panel_colors.text_disabled))
                })
                .child(
                    div()
                        .flex_none()
                        .text_color(gpui_color(if available {
                            icon_color
                        } else {
                            colors.icon_disabled
                        }))
                        .child(Icon::inherited(
                            icon,
                            appearance.icons.metrics(IconRole::Row).glyph_size,
                        )),
                )
                .child(div().min_w_0().flex_1().child(title))
        })
        .collect::<Vec<_>>();
    div()
        .id(SharedString::from(format!("{prefix}-navigation")))
        .role(accesskit::Role::ListBox)
        .aria_label("Sections")
        .debug_selector(move || format!("{prefix}-navigation"))
        .when(any_available, |list| list.track_focus(&list_focus))
        // GPUI track_focus focuses on mouse-down. Suppress that before its bubble listener runs:
        // pointer selection does not enter keyboard navigation.
        .capture_any_mouse_down(cx.listener(
            |owner: &mut T, event: &gpui::MouseDownEvent, window, cx| {
                if event.button != gpui::MouseButton::Left {
                    return;
                }
                window.prevent_default();
                owner.navigation().release_to_window(window, cx);
                cx.notify();
            },
        ))
        .on_key_down(cx.listener(navigate::<T>))
        .flex()
        .flex_col()
        .w_full()
        .gap(appearance.spacing(2.0))
        .children(rows)
        .into_any_element()
}

/// The active section's large title and description at the head of the content column, with an
/// optional trailing toolbar for window-wide controls.
pub(crate) struct DetailHeading<'a> {
    prefix: &'static str,
    heading: AnyElement,
    toolbar: Option<AnyElement>,
    scrolled: bool,
    movement: &'a WindowMovement,
    close: WindowCloseHandler,
}

impl<'a> DetailHeading<'a> {
    pub(crate) fn new(
        prefix: &'static str,
        heading: impl IntoElement,
        movement: &'a WindowMovement,
        close: WindowCloseHandler,
    ) -> Self {
        Self {
            prefix,
            heading: heading.into_any_element(),
            toolbar: None,
            scrolled: false,
            movement,
            close,
        }
    }

    /// Window-wide controls beside the heading, outside window-movement ownership. Only the
    /// Developer Workbench has any.
    #[cfg(feature = "developer-tools")]
    pub(crate) fn toolbar(mut self, toolbar: impl IntoElement) -> Self {
        self.toolbar = Some(toolbar.into_any_element());
        self
    }

    pub(crate) fn scrolled(mut self, scrolled: bool) -> Self {
        self.scrolled = scrolled;
        self
    }

    pub(crate) fn render(
        self,
        surface: &SettingsAppearance,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = &surface.chrome;
        let prefix = self.prefix;
        let client_controls =
            ClientWindowControls::width(spaceterm_ui::WindowControlSide::Right, window, cx)
                > px(0.0);
        let heading = div()
            .size_full()
            .px(appearance.spacing(CONTENT_GUTTER))
            .pt(appearance.spacing(HEADING_TOP_INSET))
            .pb(appearance.spacing(14.0))
            .child(self.heading);
        let region = self.movement.region(
            format!("{prefix}-detail-drag-region"),
            heading,
            Edges::default(),
            window,
        );
        div()
            .debug_selector(move || format!("{prefix}-detail-heading"))
            .relative()
            .flex()
            .flex_row()
            .flex_none()
            .w_full()
            .child(div().flex_1().min_w_0().child(region))
            .children(self.toolbar.map(|toolbar| {
                div()
                    .debug_selector(move || format!("{prefix}-toolbar"))
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_start()
                    .gap(appearance.spacing(8.0))
                    .pt(appearance.spacing(HEADING_TOP_INSET))
                    .pr(appearance.spacing(if client_controls { 8.0 } else { CONTENT_GUTTER }))
                    .child(toolbar)
            }))
            .when(client_controls, |heading| {
                heading.child(
                    div()
                        .debug_selector(move || format!("{prefix}-window-controls"))
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(client_control_row_height(appearance))
                        .pr(px(spaceterm_ui::DesktopWindowStyle::current(cx)
                            .control_metrics()
                            .edge_margin))
                        .child(
                            ClientWindowControls::new(self.close)
                                .surface_color(gpui_color(appearance.colors.title_bar_background)),
                        ),
                )
            })
            .when(self.scrolled, |heading| {
                heading.child(
                    div()
                        .debug_selector(move || format!("{prefix}-detail-heading-divider"))
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(super::control_theme::resize_handle::VISIBLE_THICKNESS))
                        .bg(gpui_color(surface.separator(SettingsSurfaceRole::Canvas))),
                )
            })
            .into_any_element()
    }
}

/// The height of the content column's footer, which a sidebar footer shares so the two read as one
/// band across the window.
fn footer_height(appearance: &ChromeAppearance) -> Pixels {
    appearance.typography.style(TextRole::Secondary).line_height
        + appearance.spacing(FOOTER_HEIGHT - 15.0)
}

/// The content column's closing strip.
pub(crate) fn render_footer(
    prefix: &'static str,
    surface: &SettingsAppearance,
    content: impl IntoElement,
) -> AnyElement {
    let appearance = &surface.chrome;
    div()
        .debug_selector(move || format!("{prefix}-footer"))
        .flex()
        .flex_row()
        .flex_none()
        .w_full()
        .h(footer_height(appearance))
        .border_t_1()
        .border_color(gpui_color(surface.separator(SettingsSurfaceRole::Canvas)))
        .bg(gpui_color(
            surface.surface(SettingsSurfaceRole::Canvas).paint,
        ))
        .items_center()
        .justify_end()
        .gap(appearance.spacing(8.0))
        .px(appearance.spacing(CONTENT_GUTTER))
        .child(content)
        .into_any_element()
}
