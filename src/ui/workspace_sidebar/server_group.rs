//! A server's section in the Workspace sidebar. Its header names the server and states its
//! connection with a glyph and text, never color alone. Expanded, the section shows the Workspaces
//! this Client attached from the server, or an empty state that offers to attach one.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Workspace sidebar groups its Workspaces by server once it lists server Workspaces"
    )
)]

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, ElementId, Keystroke, Pixels, SharedString, Window, accesskit, div};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, ControlHost, FrameSpinner, HoverFade, Icon, IconName,
    ProgressSize,
};

use super::{
    SIDEBAR_ROW_CHIP_PADDING, SIDEBAR_ROW_SELECTION_INSET_Y, SIDEBAR_ROW_TITLE_LINE_PADDING,
    workspace_row_status_paint,
};
use crate::appearance::{ChromeColors, Color};
use crate::ui::appearance::{ChromeAppearance, gpui_color, shared_chrome};
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::pane_presentation::DisplayName;
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};
use crate::ui::workspace_frame::WorkspaceFrame;

const HEADER_HEIGHT: f32 = 32.0;
const GLYPH_COLUMN_WIDTH: f32 = 18.0;
const DISCLOSURE_WIDTH: f32 = 16.0;
const HEADER_CONTENT_GAP: f32 = 10.0;
const HEADER_STATUS_GAP: f32 = 6.0;
const BODY_LINE_GAP: f32 = 2.0;
const BODY_PADDING_Y: f32 = 4.0;
const EMPTY_MESSAGE: &str = "No Workspaces attached";

/// Identifies one server's section among the sidebar's sections.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(in crate::ui) struct ServerSectionId(u128);

impl ServerSectionId {
    pub(in crate::ui) const fn new(id: u128) -> Self {
        Self(id)
    }

    /// The debug selector of one part of the section.
    pub(in crate::ui) fn selector(self, part: &str) -> String {
        format!("server-section-{:032x}-{part}", self.0)
    }

    fn element(self, part: &str) -> ElementId {
        ElementId::Name(self.selector(part).into())
    }
}

/// Which side of a connection needs an update before this Client and a server can talk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum UpdateRequired {
    ThisComputer,
    Server,
}

/// What this Client knows about its connection to a server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum ServerConnection {
    Connected,
    Connecting,
    /// The server can't be reached. Its Workspaces stay listed and their Panes freeze.
    Offline,
    /// The server and this Client share no protocol version.
    Incompatible(UpdateRequired),
    /// The server was stopped on purpose and admits no Client until it starts again.
    Stopped,
}

impl ServerConnection {
    #[cfg(test)]
    pub(in crate::ui) const ALL: [Self; 6] = [
        Self::Connected,
        Self::Connecting,
        Self::Offline,
        Self::Incompatible(UpdateRequired::ThisComputer),
        Self::Incompatible(UpdateRequired::Server),
        Self::Stopped,
    ];

    pub(in crate::ui) const fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Connecting => "Connecting…",
            Self::Offline => "Offline",
            Self::Incompatible(_) => "Update required",
            Self::Stopped => "Stopped",
        }
    }

    /// The glyph that states the connection beside its label. A connecting server shows progress.
    const fn glyph(self) -> Option<IconName> {
        match self {
            Self::Connected => Some(IconName::Server),
            Self::Connecting => None,
            Self::Offline => Some(IconName::ServerOff),
            Self::Incompatible(_) => Some(IconName::TriangleAlert),
            Self::Stopped => Some(IconName::CircleStop),
        }
    }

    /// The status color the glyph and label propose, or none for the row's own colors.
    const fn tone(self, colors: &ChromeColors) -> Option<Color> {
        match self {
            Self::Connected => None,
            Self::Connecting => Some(colors.info),
            Self::Offline | Self::Stopped => Some(colors.icon_muted),
            Self::Incompatible(_) => Some(colors.warning),
        }
    }

    /// What the person can do about the connection, shown in the section and read with its
    /// header. `server` is the server's name.
    pub(in crate::ui) fn explanation(self, server: &DisplayName) -> Option<SharedString> {
        let application =
            crate::application_identity::ApplicationIdentity::current().display_name();
        match self {
            Self::Incompatible(UpdateRequired::ThisComputer) => {
                Some(format!("Update {application} on this computer to connect.").into())
            }
            Self::Incompatible(UpdateRequired::Server) => Some(
                match server.get() {
                    Some(name) => format!("Update {application} on {name} to connect."),
                    None => format!("Update {application} on the server to connect."),
                }
                .into(),
            ),
            Self::Connected | Self::Connecting | Self::Offline | Self::Stopped => None,
        }
    }

    const fn attaches(self) -> bool {
        matches!(self, Self::Connected)
    }
}

/// One server's section as the sidebar lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui) struct ServerSectionModel {
    pub(in crate::ui) id: ServerSectionId,
    pub(in crate::ui) name: DisplayName,
    pub(in crate::ui) connection: ServerConnection,
    /// Whether the section shows its Workspaces.
    pub(in crate::ui) expanded: bool,
}

/// What a key does on a server's header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum ServerHeaderKey {
    /// Shows or hides the section's Workspaces.
    SetExpanded(bool),
    /// Moves the keyboard to the section's first Workspace, when it has one.
    EnterSection,
}

impl ServerSectionModel {
    /// The name the header shows, or a placeholder for a server that reported none.
    pub(in crate::ui) fn title(&self) -> SharedString {
        self.name
            .get()
            .cloned()
            .unwrap_or(SharedString::new_static("Unnamed Server"))
    }

    /// What a key does on the section's header, as a source list's group row answers it: Right
    /// expands and then enters the section, Left collapses, and Return and Space toggle. The
    /// sidebar moves between rows on every key this leaves to it.
    pub(in crate::ui) fn header_key(&self, keystroke: &Keystroke) -> Option<ServerHeaderKey> {
        if keystroke.modifiers.modified() {
            return None;
        }
        match keystroke.key.as_str() {
            "right" if self.expanded => Some(ServerHeaderKey::EnterSection),
            "right" => Some(ServerHeaderKey::SetExpanded(true)),
            "left" if self.expanded => Some(ServerHeaderKey::SetExpanded(false)),
            "enter" | "space" => Some(ServerHeaderKey::SetExpanded(!self.expanded)),
            _ => None,
        }
    }
}

type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type AttachHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A server's section: its header, then its Workspaces or its empty state while expanded.
///
/// The sidebar owns selection and the keyboard. It passes whether the keyboard stands on the
/// header, and answers keys on it with [`ServerSectionModel::header_key`].
#[derive(IntoElement)]
pub(in crate::ui) struct ServerSection {
    model: ServerSectionModel,
    selected: bool,
    emphasized: bool,
    set_position: Option<(usize, usize)>,
    workspaces: Vec<AnyElement>,
    on_toggle: Option<ToggleHandler>,
    on_attach: Option<AttachHandler>,
}

impl ServerSection {
    pub(in crate::ui) fn new(model: ServerSectionModel) -> Self {
        Self {
            model,
            selected: false,
            emphasized: false,
            set_position: None,
            workspaces: Vec::new(),
            on_toggle: None,
            on_attach: None,
        }
    }

    /// Whether the keyboard stands on the header.
    pub(in crate::ui) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Whether the sidebar has keyboard focus, so a selected header is painted in the accent.
    pub(in crate::ui) fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    /// The section's 1-based position among `size` server sections.
    pub(in crate::ui) fn set_position(mut self, position: usize, size: usize) -> Self {
        self.set_position = Some((position, size));
        self
    }

    /// The rows of the Workspaces this Client attached from the server, in sidebar order.
    pub(in crate::ui) fn workspaces(mut self, workspaces: Vec<AnyElement>) -> Self {
        self.workspaces = workspaces;
        self
    }

    /// Runs with the expansion the header asks for.
    pub(in crate::ui) fn on_toggle(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    /// Runs when the empty state's Attach Workspace is chosen.
    pub(in crate::ui) fn on_attach(
        mut self,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_attach = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ServerSection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = shared_chrome(cx);
        let id = self.model.id;
        let hover = HoverFade::new(id.element("hover"), window, cx);
        let level = if appearance.active {
            hover.level(window, cx)
        } else {
            0.0
        };
        let (chip, colors) = header_paint(self.selected, self.emphasized, level, &appearance, cx);
        let frame = WorkspaceFrame::for_appearance(&appearance, cx);
        let leading =
            frame.sidebar_chip_leading_inset() + appearance.spacing(SIDEBAR_ROW_CHIP_PADDING);
        let trailing =
            frame.sidebar_chip_trailing_inset() + appearance.spacing(SIDEBAR_ROW_CHIP_PADDING);
        let text_indent = leading + appearance.spacing(GLYPH_COLUMN_WIDTH + HEADER_CONTENT_GAP);
        let explanation = self.model.connection.explanation(&self.model.name);
        let header = render_header(
            &self.model,
            Header {
                selected: self.selected,
                level,
                chip,
                set_position: self.set_position,
                explanation: explanation.clone(),
                leading,
                trailing,
                hover,
                on_toggle: self.on_toggle,
            },
            &colors,
            &appearance,
        );
        let expanded = self.model.expanded;
        let empty = self.workspaces.is_empty();
        div()
            .id(id.element("section"))
            .debug_selector(move || id.selector("section"))
            .w_full()
            .flex()
            .flex_col()
            .child(header)
            .when(expanded, |section| {
                section
                    .when_some(explanation, |section, explanation| {
                        section.child(
                            body_line(&appearance, text_indent, trailing)
                                .id(id.element("explanation"))
                                .role(accesskit::Role::Label)
                                .aria_value(explanation.clone())
                                .debug_selector(move || id.selector("explanation"))
                                .text_color(gpui_color(colors.row_secondary))
                                .child(explanation),
                        )
                    })
                    .when(empty, |section| {
                        section.child(render_empty_state(
                            id,
                            self.model.connection.attaches(),
                            self.on_attach,
                            text_indent,
                            trailing,
                            &colors,
                            &appearance,
                        ))
                    })
                    .children(self.workspaces)
            })
    }
}

/// What the header needs beyond the section's model.
struct Header {
    selected: bool,
    level: f32,
    chip: SelectionChip,
    set_position: Option<(usize, usize)>,
    explanation: Option<SharedString>,
    leading: Pixels,
    trailing: Pixels,
    hover: HoverFade,
    on_toggle: Option<ToggleHandler>,
}

fn render_header(
    model: &ServerSectionModel,
    header: Header,
    colors: &ChromeColors,
    appearance: &ChromeAppearance,
) -> AnyElement {
    let id = model.id;
    let expanded = model.expanded;
    let connection = model.connection;
    let title = model.title();
    let label = connection.label();
    let description: SharedString = match &header.explanation {
        Some(explanation) => format!("{label}. {explanation}").into(),
        None => label.into(),
    };
    // The glyph and label carry the connection; the status color only reinforces them.
    let tone = connection.tone(colors).map(|tone| {
        let paint = |minimum_contrast| {
            let paint = workspace_row_status_paint(tone, header.selected, minimum_contrast, colors);
            paint.normal.fade(paint.hovered, f64::from(header.level))
        };
        (paint(3.0), paint(4.5))
    });
    let (glyph_color, status_color) = tone.unwrap_or((colors.row_icon, colors.row_secondary));
    let glyph_size = appearance.icons.metrics(IconRole::Row).glyph_size;
    let glyph = match connection.glyph() {
        Some(icon) => Icon::inherited(icon, glyph_size).into_any_element(),
        None => FrameSpinner::new(id.element("progress"), connection.label())
            .size(ProgressSize::Compact)
            .into_any_element(),
    };
    let toggle = |expanded: bool| {
        let on_toggle = header.on_toggle.clone();
        move |window: &mut Window, cx: &mut App| {
            if let Some(on_toggle) = &on_toggle {
                on_toggle(expanded, window, cx);
            }
        }
    };
    let click = toggle(!expanded);
    let press = toggle(!expanded);
    let expand = toggle(true);
    let collapse = toggle(false);
    div()
        .id(id.element("header"))
        .debug_selector(move || {
            id.selector(if expanded {
                "header-expanded"
            } else {
                "header-collapsed"
            })
        })
        .role(accesskit::Role::TreeItem)
        .aria_label(title.clone())
        .aria_description(description)
        .aria_expanded(expanded)
        .aria_level(1)
        .aria_selected(header.selected)
        .when_some(header.set_position, |row, (position, size)| {
            row.aria_position_in_set(position).aria_size_of_set(size)
        })
        .on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
            press(window, cx)
        })
        .when(!expanded, |row| {
            row.on_a11y_action(accesskit::Action::Expand, move |_, window, cx| {
                expand(window, cx)
            })
        })
        .when(expanded, |row| {
            row.on_a11y_action(accesskit::Action::Collapse, move |_, window, cx| {
                collapse(window, cx)
            })
        })
        .relative()
        .w_full()
        .h(header_height(appearance))
        .flex_shrink_0()
        .flex()
        .items_center()
        .pl(header.leading)
        .pr(header.trailing)
        .gap(appearance.spacing(HEADER_CONTENT_GAP))
        .block_mouse_except_scroll()
        .when_some(
            super::view::row_background(appearance),
            |row, background| row.bg(gpui_color(background)),
        )
        .on_click(move |_, window, cx| {
            click(window, cx);
            cx.stop_propagation();
        })
        .child(header.chip.render(id.selector("selection"), header.level))
        .child(
            div()
                .debug_selector(move || id.selector("glyph"))
                .w(appearance.spacing(GLYPH_COLUMN_WIDTH))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui_color(glyph_color))
                .child(glyph),
        )
        .child(
            div()
                .debug_selector(move || id.selector("name"))
                .min_w_0()
                .flex_1()
                .truncate()
                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                .text_color(gpui_color(colors.row_foreground))
                .child(title),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(appearance.spacing(HEADER_STATUS_GAP))
                .child(
                    div()
                        .debug_selector(move || id.selector("status"))
                        .whitespace_nowrap()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(status_color))
                        .child(label),
                )
                .child(
                    div()
                        .debug_selector(move || id.selector("disclosure"))
                        .w(appearance.spacing(DISCLOSURE_WIDTH))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(gpui_color(colors.row_icon))
                        .child(Icon::inherited(
                            if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            },
                            appearance.icons.metrics(IconRole::Caption).glyph_size,
                        )),
                ),
        )
        .child(header.hover.tracker())
        .into_any_element()
}

/// An expanded section with no attached Workspace. Attaching needs a connected server; any other
/// connection keeps the action visible but unavailable, and the header says why.
fn render_empty_state(
    id: ServerSectionId,
    attaches: bool,
    on_attach: Option<AttachHandler>,
    text_indent: Pixels,
    trailing: Pixels,
    colors: &ChromeColors,
    appearance: &ChromeAppearance,
) -> AnyElement {
    let attach_selector = id.selector("attach");
    body_line(appearance, text_indent, trailing)
        .debug_selector(move || id.selector("empty"))
        .flex()
        .flex_col()
        .items_start()
        .gap(appearance.spacing(BODY_LINE_GAP))
        .child(
            div()
                .id(id.element("empty-message"))
                .role(accesskit::Role::Label)
                .aria_value(EMPTY_MESSAGE)
                .text_color(gpui_color(colors.row_secondary))
                .child(EMPTY_MESSAGE),
        )
        .child(
            Button::new(id.element("attach"), "Attach Workspace…")
                .variant(ButtonVariant::Link)
                .size(ButtonSize::Small)
                .disabled(!attaches)
                .tab_stop(true)
                .debug_selector(attach_selector)
                .on_activate(move |_, window, cx| {
                    if let Some(on_attach) = &on_attach {
                        on_attach(window, cx);
                    }
                }),
        )
        .into_any_element()
}

/// A line of secondary text inside an expanded section, aligned with the header's name.
fn body_line(appearance: &ChromeAppearance, text_indent: Pixels, trailing: Pixels) -> gpui::Div {
    div()
        .w_full()
        .pl(text_indent)
        .pr(trailing)
        .py(appearance.spacing(BODY_PADDING_Y))
        .whitespace_normal()
        .chrome_text(appearance.typography.style(TextRole::Secondary))
}

fn header_height(appearance: &ChromeAppearance) -> Pixels {
    appearance.spacing(HEADER_HEIGHT).max(
        appearance
            .typography
            .style(TextRole::BodyEmphasis)
            .line_height
            + appearance
                .spacing(SIDEBAR_ROW_TITLE_LINE_PADDING + 2.0 * SIDEBAR_ROW_SELECTION_INSET_Y),
    )
}

/// The header's chip and the Panel-host colors its text and glyphs use. They match a Workspace
/// row's, so the keyboard's selection reads the same on a header as on a row.
fn header_paint(
    selected: bool,
    emphasized: bool,
    hover: f32,
    appearance: &ChromeAppearance,
    cx: &App,
) -> (SelectionChip, ChromeColors) {
    let colors = appearance.host_colors(ControlHost::Panel);
    let selection = selection_colors(emphasized, appearance);
    let active = appearance.active;
    let paint = if selected {
        ChipPaint {
            fill: Some(selection.row_selected_background),
            rim: Some(selection.row_selected_border),
            hover_fill: active.then_some(selection.row_selected_hover_background),
            hover_rim: active.then_some(selection.row_selected_hover_border),
        }
    } else {
        ChipPaint {
            fill: None,
            rim: None,
            hover_fill: active.then_some(colors.row_hover_background),
            hover_rim: active.then_some(colors.row_hover_border),
        }
    };
    let paint = if selected && emphasized {
        // The accent is opaque, so the window's material does not thin it.
        paint
    } else if selected {
        paint.selected_on(appearance, colors.row_background)
    } else {
        paint.raised_on(appearance, colors.row_background)
    };
    let frame = WorkspaceFrame::for_appearance(appearance, cx);
    let chip = SelectionChip::new(
        ChipShape {
            inset_leading: frame.sidebar_chip_leading_inset(),
            inset_trailing: frame.sidebar_chip_trailing_inset(),
            inset_y: appearance.spacing(SIDEBAR_ROW_SELECTION_INSET_Y),
            radius: frame.chip_radius(),
        },
        paint,
    );
    (chip, header_colors(selected, emphasized, hover, appearance))
}

/// The selection paints of the sidebar: the accent while it has keyboard focus in an active
/// window, as a source list emphasizes its selection, and the quieter selection otherwise.
fn selection_colors(emphasized: bool, appearance: &ChromeAppearance) -> ChromeColors {
    let colors = appearance.host_colors(ControlHost::Panel);
    if emphasized {
        crate::ui::selection_chip::emphasized_selection_colors(colors)
    } else if appearance.active {
        appearance
            .unfocused_selection_colors(ControlHost::Panel)
            .clone()
    } else {
        colors.clone()
    }
}

/// The colors a header's text and glyphs use: promoted when selected and eased toward hover paint.
fn header_colors(
    selected: bool,
    emphasized: bool,
    hover: f32,
    appearance: &ChromeAppearance,
) -> ChromeColors {
    let mut colors = appearance.host_colors(ControlHost::Panel).clone();
    if selected {
        let selection = selection_colors(emphasized, appearance);
        colors.row_selected_background = selection.row_selected_background;
        colors.row_selected_hover_background = selection.row_selected_hover_background;
        colors.row_foreground = selection.row_selected_foreground;
        colors.row_secondary = selection.row_selected_secondary;
        colors.row_icon = selection.row_selected_icon;
        colors.row_hover_foreground = selection.row_selected_hover_foreground;
        colors.row_hover_secondary = selection.row_selected_hover_secondary;
        colors.row_hover_icon = selection.row_selected_hover_icon;
    }
    let level = f64::from(hover);
    colors.row_foreground = colors
        .row_foreground
        .fade(colors.row_hover_foreground, level);
    colors.row_secondary = colors.row_secondary.fade(colors.row_hover_secondary, level);
    colors.row_icon = colors.row_icon.fade(colors.row_hover_icon, level);
    colors
}

#[cfg(test)]
#[path = "server_group/tests.rs"]
mod tests;
