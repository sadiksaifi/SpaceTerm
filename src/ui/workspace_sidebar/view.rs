use super::*;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::drag_and_drop::{MarkerSide, drag_release_observer, insertion_marker};
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};
use gpui::accesskit;
use spaceterm_ui::{ContextMenuTarget, HoverFade};

/// Where the pointer stands over one row.
pub(super) struct RowHover {
    /// How far the row has eased toward its hovered paint.
    pub(super) level: f32,
    /// What follows the pointer over the row; the lifted copy has none.
    pub(super) tracker: Option<HoverFade>,
}

/// A row's hover as read at the start of a frame.
pub(super) struct RowFade {
    fade: HoverFade,
    level: f32,
    /// Each disclosed Worktree row's hover, in list order.
    worktrees: Vec<RowFade>,
}

/// The width of the trailing column that holds a git Workspace's disclosure chevron.
const DISCLOSURE_WIDTH: f32 = 16.0;
const ICON_COLUMN_WIDTH: f32 = 18.0;
const ROW_CONTENT_GAP: f32 = 10.0;
/// The minimum height of a row that holds one line.
const SINGLE_LINE_ROW_HEIGHT: f32 = 32.0;
/// How far a Worktree row's content starts inside its Workspace's.
const WORKTREE_LEVEL_INDENT: f32 = 14.0;
const FORMER_REPOSITORY_ROW_HEIGHT: f32 = 22.0;

/// The chip carrying a Workspace row's hover and persistent selection. An emphasized selection
/// marks a sidebar with keyboard focus.
fn row_chip(
    selected: bool,
    emphasized: bool,
    appearance: &crate::ui::appearance::ChromeAppearance,
    colors: &crate::appearance::ChromeColors,
    selection_colors: &crate::appearance::ChromeColors,
    cx: &App,
) -> SelectionChip {
    let paint = if selected {
        ChipPaint {
            fill: Some(selection_colors.row_selected_background),
            rim: Some(selection_colors.row_selected_border),
            hover_fill: appearance
                .active
                .then_some(selection_colors.row_selected_hover_background),
            hover_rim: appearance
                .active
                .then_some(selection_colors.row_selected_hover_border),
        }
    } else {
        ChipPaint {
            fill: None,
            rim: None,
            hover_fill: appearance.active.then_some(colors.row_hover_background),
            hover_rim: appearance.active.then_some(colors.row_hover_border),
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
    let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    SelectionChip::new(
        // A row's chip keeps the frame's one visible gap on both sides: to the window edge on one,
        // and to the Pane beside the sidebar on the other.
        ChipShape {
            inset_leading: frame.sidebar_chip_leading_inset(),
            inset_trailing: frame.sidebar_chip_trailing_inset(),
            inset_y: appearance.spacing(SIDEBAR_ROW_SELECTION_INSET_Y),
            radius: frame.chip_radius(),
        },
        paint,
    )
}

/// The leading and trailing row padding that keeps a row's content balanced inside its chip.
fn row_padding(appearance: &crate::ui::appearance::ChromeAppearance, cx: &App) -> (Pixels, Pixels) {
    let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    let air = appearance.spacing(SIDEBAR_ROW_CHIP_PADDING);
    (
        frame.sidebar_chip_leading_inset() + air,
        frame.sidebar_chip_trailing_inset() + air,
    )
}

/// A row's chip and the Panel-host row colors its text and icons use, promoted for a selected row
/// and eased toward hover paint.
fn row_paint(
    selected: bool,
    emphasized: bool,
    hover: f32,
    appearance: &crate::ui::appearance::ChromeAppearance,
    cx: &App,
) -> (SelectionChip, crate::appearance::ChromeColors) {
    let mut row_colors = appearance
        .host_colors(spaceterm_ui::ControlHost::Panel)
        .clone();
    // The chip's paints are read before the selected colors are promoted below, because that
    // promotion is what the row's text and icons consume. A keyboard-focused sidebar emphasizes
    // its selection in the accent color, like an AppKit source list, and draws no focus ring.
    let selection_colors = if emphasized {
        crate::ui::selection_chip::emphasized_selection_colors(&row_colors)
    } else if appearance.active {
        appearance
            .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel)
            .clone()
    } else {
        row_colors.clone()
    };
    let chip = row_chip(
        selected,
        emphasized,
        appearance,
        &row_colors,
        &selection_colors,
        cx,
    );
    if selected {
        row_colors.row_selected_background = selection_colors.row_selected_background;
        row_colors.row_selected_hover_background = selection_colors.row_selected_hover_background;
        row_colors.row_foreground = selection_colors.row_selected_foreground;
        row_colors.row_secondary = selection_colors.row_selected_secondary;
        row_colors.row_icon = selection_colors.row_selected_icon;
        row_colors.row_hover_foreground = selection_colors.row_selected_hover_foreground;
        row_colors.row_hover_secondary = selection_colors.row_selected_hover_secondary;
        row_colors.row_hover_icon = selection_colors.row_selected_hover_icon;
    }
    // Text and icons follow the chip's hover paint.
    let level = f64::from(hover);
    row_colors.row_foreground = row_colors
        .row_foreground
        .fade(row_colors.row_hover_foreground, level);
    row_colors.row_secondary = row_colors
        .row_secondary
        .fade(row_colors.row_hover_secondary, level);
    row_colors.row_icon = row_colors.row_icon.fade(row_colors.row_hover_icon, level);
    (chip, row_colors)
}

/// Where a Worktree row's icon starts: one level inside its Workspace's icon.
fn worktree_indent(appearance: &crate::ui::appearance::ChromeAppearance, cx: &App) -> Pixels {
    row_padding(appearance, cx).0 + appearance.spacing(WORKTREE_LEVEL_INDENT)
}

/// Where a Worktree row's text starts, after its icon.
fn worktree_text_indent(appearance: &crate::ui::appearance::ChromeAppearance, cx: &App) -> Pixels {
    worktree_indent(appearance, cx) + appearance.spacing(ICON_COLUMN_WIDTH + ROW_CONTENT_GAP)
}

/// The height of a row that holds one line: an expanded Workspace's.
fn single_line_row_height(appearance: &crate::ui::appearance::ChromeAppearance) -> Pixels {
    appearance.spacing(SINGLE_LINE_ROW_HEIGHT).max(
        appearance
            .typography
            .style(TextRole::Navigation)
            .line_height
            + appearance
                .spacing(SIDEBAR_ROW_TITLE_LINE_PADDING + 2.0 * SIDEBAR_ROW_SELECTION_INSET_Y),
    )
}

fn row_height(appearance: &crate::ui::appearance::ChromeAppearance) -> Pixels {
    let content_height = appearance
        .typography
        .style(TextRole::Navigation)
        .line_height
        + appearance.typography.style(TextRole::Secondary).line_height
        + appearance.spacing(
            SIDEBAR_ROW_TITLE_LINE_PADDING + SIDEBAR_ROW_DETAIL_LINE_PADDING + SIDEBAR_ROW_LINE_GAP,
        );
    appearance.spacing(SIDEBAR_ROW_HEIGHT).max(content_height)
}

/// Resolves the rename frame after the row enters its Panel control host.
#[derive(IntoElement)]
struct WorkspaceRenameField {
    workspace_id: WorkspaceId,
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
    appearance: crate::ui::appearance::ChromeAppearance,
}

impl gpui::RenderOnce for WorkspaceRenameField {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus_on_click = self.focus_handle.clone();
        let text_style = self.appearance.typography.style(TextRole::Navigation);
        let field_height = self
            .appearance
            .spacing(22.0)
            .max(text_style.line_height + self.appearance.spacing(6.0));
        spaceterm_ui::field_frame(
            ("workspace-rename-input", self.workspace_id.get()),
            &self.focus_handle,
            spaceterm_ui::FieldState::default(),
            RadiusRole::ControlSmall.pixels(),
            cx,
        )
        .debug_selector(move || format!("workspace-rename-input-{}", self.workspace_id.get()))
        .h(field_height)
        .w_full()
        .px(self.appearance.spacing(5.0))
        .flex()
        .items_center()
        .chrome_text(text_style)
        .text_color(gpui_color(self.appearance.colors.text))
        .on_click(move |_, window, cx| {
            focus_on_click.focus(window, cx);
            cx.stop_propagation();
        })
        .child(self.input)
    }
}

pub(super) fn row_background(
    appearance: &crate::ui::appearance::ChromeAppearance,
) -> Option<Color> {
    let colors = appearance.host_colors(spaceterm_ui::ControlHost::Panel);
    let base = crate::ui::workspace_frame::base_surface(&appearance.colors);
    (colors.row_background != base).then(|| {
        appearance.materials.paint(
            crate::appearance::SurfaceRole::Surface,
            base,
            colors.row_background,
        )
    })
}

/// Whether a row face is the row in the Workspace list or its lifted copy following the pointer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowRole {
    InList,
    Lifted,
}

impl WorkspaceSidebar {
    /// The accent lines on a row that mark the slot a dragged row would land in.
    fn row_insertion_markers(
        &self,
        workspace_id: WorkspaceId,
        role: RowRole,
        appearance: &crate::ui::appearance::ChromeAppearance,
        cx: &App,
    ) -> Vec<AnyElement> {
        let len = self.rows.len();
        let (Some(slot), Some(index), RowRole::InList) = (
            self.workspace_reorder.insertion(len),
            self.row_position(workspace_id),
            role,
        ) else {
            return Vec::new();
        };
        let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
        let inset = (
            frame.sidebar_chip_leading_inset(),
            frame.sidebar_chip_trailing_inset(),
        );
        let side = if slot == index {
            MarkerSide::Leading
        } else if slot == len && index + 1 == len {
            MarkerSide::Trailing
        } else {
            return Vec::new();
        };
        vec![insertion_marker(
            gpui::Axis::Vertical,
            side,
            slot == 0 || slot == len,
            inset,
            format!("workspace-insertion-marker-{slot}"),
            appearance,
        )]
    }
}

impl WorkspaceSidebar {
    /// One Workspace row, or the lifted copy that follows the pointer during a drag. The copy
    /// claims no pointer input because it lies over every drop target.
    #[expect(
        clippy::too_many_arguments,
        reason = "one row render step needs its model, role, hover, owner, presentation, and host geometry"
    )]
    pub(super) fn render_workspace_row(
        &self,
        row: WorkspaceRowViewModel,
        role: RowRole,
        hover: RowHover,
        sidebar: WeakEntity<Self>,
        presentation: &crate::desktop_profile::DesktopPresentation,
        window: &Window,
        appearance: &crate::ui::appearance::ChromeAppearance,
        cx: &App,
    ) -> AnyElement {
        let WorkspaceRowViewModel {
            workspace_id,
            name,
            path,
            machine,
            tooltip,
            pinned,
            remote_connection_phase,
            available,
            repository,
            worktrees,
            creates_worktrees,
            active,
        } = row;
        let row_background = row_background(appearance);
        let emphasized = appearance.active && self.has_visible_focus(window);
        let lifted = role == RowRole::Lifted;
        let selected = if lifted {
            active
        } else {
            self.selected_key() == Some(SidebarRowKey::Workspace(workspace_id))
        };
        let RowHover {
            level: hover,
            tracker,
        } = hover;
        let hover = if appearance.active { hover } else { 0.0 };
        let level = f64::from(hover);
        let (chip, row_colors) = row_paint(selected, emphasized, hover, appearance, cx);
        let expanded = worktrees.as_ref().map(|section| section.expanded);
        // A disclosing row describes the Workspace: its pin moves to line 1, and while expanded
        // its one line leaves each directory and branch to the Worktree rows below it.
        let disclosing = expanded.is_some();
        let single_line = expanded == Some(true);
        // A collapsed row stands for its Active Worktree, so its menu removes that Worktree.
        let collapsed_worktree = worktrees
            .as_ref()
            .filter(|section| !section.expanded)
            .and_then(|section| section.rows().find(|worktree| worktree.active))
            .filter(|worktree| {
                !matches!(
                    worktree.removal,
                    WorktreeRemoval::Main | WorktreeRemoval::Unlisted
                )
            })
            .map(|worktree| {
                (
                    worktree.worktree_id,
                    worktree.name.clone(),
                    worktree.removal,
                )
            });
        // Text helpers consume no materials or surfaces. Give only those helpers the promoted
        // Panel-host row colors while all surface composition keeps the root appearance.
        let mut row_text_appearance = appearance.clone();
        row_text_appearance.colors = row_colors.clone();
        let click_sidebar = sidebar.clone();
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let remote_color =
            remote_connection_phase.map(|phase| remote_connection_color(phase, &row_colors));
        // An unavailable directory and the Remote connection status take line 2 from the
        // repository, as they take it from the directory.
        let repository = repository.filter(|_| available && remote_status.is_none());
        let pull_request_paint = appearance.colors.pull_request(if selected {
            appearance.colors.row_selected_background
        } else {
            crate::ui::workspace_frame::base_surface(&appearance.colors)
        });
        let (detail, detail_color, detail_selector) = if !available {
            (
                "Directory unavailable".into(),
                Some(row_colors.warning),
                Some(format!(
                    "workspace-row-directory-unavailable-{}",
                    workspace_id.get()
                )),
            )
        } else if let Some(status) = remote_status {
            (
                status.into(),
                remote_color,
                Some(format!(
                    "workspace-row-remote-status-{}",
                    workspace_id.get()
                )),
            )
        } else {
            (path, None, None)
        };
        let under_pointer = |paint: WorkspaceStatusPaint| WorkspaceStatusPaint {
            normal: paint.normal.fade(paint.hovered, level),
            ..paint
        };
        let detail_paint = detail_color.map(|color| {
            under_pointer(workspace_row_status_paint(
                color,
                selected,
                4.5,
                &row_colors,
            ))
        });
        // The row icon carries one status in the same precedence as the collapsed identity: an
        // unavailable directory first, then the Remote connection.
        let icon_paint = (!available)
            .then_some(row_colors.warning)
            .or(remote_color)
            .map(|color| {
                under_pointer(workspace_row_status_paint(
                    color,
                    selected,
                    3.0,
                    &row_colors,
                ))
            });
        let target_name = name.clone();
        let target_detail = if single_line {
            tooltip.clone()
        } else {
            detail.clone()
        };
        let accessibility_name = remote_status.map_or_else(
            || format!("Workspace actions for {name}"),
            |status| format!("Workspace actions for {name}, connection {status}"),
        );
        let rename = self
            .rename
            .as_ref()
            .filter(|rename| rename.workspace_id == workspace_id);
        let renaming = rename.is_some();
        let first_line = if let Some(rename) = rename {
            WorkspaceRenameField {
                workspace_id,
                input: rename.input.clone(),
                focus_handle: rename.focus_handle.clone(),
                appearance: row_text_appearance.clone(),
            }
            .into_any_element()
        } else {
            div()
                .id(("workspace-row-name", workspace_id.get()))
                .debug_selector(move || format!("workspace-row-name-{}", workspace_id.get()))
                .w_full()
                .truncate()
                .chrome_text(appearance.typography.style(TextRole::Navigation))
                .text_color(gpui_color(row_colors.row_foreground))
                .child(name.clone())
                .into_any_element()
        };

        let tooltip_text = remote_status
            .map(|status| format!("{tooltip}: {status}"))
            .unwrap_or_else(|| tooltip.to_string());
        let tooltip_text = format!("{name}\n{tooltip_text}");
        let tooltip_label = if !available {
            "Workspace unavailable"
        } else if remote_status.is_some() {
            "Remote Workspace connection"
        } else if disclosing {
            "Workspace"
        } else if pinned {
            "Pinned Directory"
        } else {
            "Workspace Directory"
        };

        let (row_padding_leading, row_padding_trailing) = row_padding(appearance, cx);
        let drag_sidebar = sidebar.clone();
        let owner = sidebar.entity_id();
        let markers = self.row_insertion_markers(workspace_id, role, appearance, cx);
        let element_name = if lifted {
            "workspace-row-preview"
        } else {
            "workspace-row"
        };
        // An expanded row's one line ends with the "+" and the chevron. A collapsed row centers
        // its chevron over both lines and keeps its place clear on line 1 only, so line 2 runs to
        // the row's end like a row without Worktrees. A collapsed row offers New Worktree from its
        // menu.
        let disclosure = expanded.map(|expanded| {
            self.render_disclosure(
                workspace_id,
                expanded,
                !lifted,
                row_colors.row_icon,
                sidebar.clone(),
                appearance,
            )
        });
        let (inline_trailing, collapsed_disclosure) = match (expanded, disclosure) {
            (Some(true), Some(disclosure)) => (
                Some(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .when(creates_worktrees && !lifted && !renaming, |trailing| {
                            trailing.child(new_worktree_button(
                                workspace_id,
                                // The button keeps its place while hidden, so the row never
                                // shifts.
                                if selected { 1.0 } else { hover },
                                sidebar.clone(),
                                appearance,
                            ))
                        })
                        .child(disclosure)
                        .into_any_element(),
                ),
                None,
            ),
            (_, disclosure) => (
                disclosure.as_ref().map(|_| {
                    div()
                        .w(appearance.spacing(DISCLOSURE_WIDTH))
                        .flex_shrink_0()
                        .into_any_element()
                }),
                disclosure,
            ),
        };
        let row_content = div()
            .id((element_name, workspace_id.get()))
            .debug_selector(move || {
                format!(
                    "{element_name}-{}-{}",
                    workspace_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .w_full()
            .h(if single_line {
                single_line_row_height(appearance)
            } else {
                row_height(appearance)
            })
            .flex_shrink_0()
            .pl(row_padding_leading)
            .pr(row_padding_trailing)
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(ROW_CONTENT_GAP))
            .when(!lifted, |row| row.block_mouse_except_scroll())
            .when_some(row_background, |row, background| {
                row.bg(gpui_color(background))
            })
            .child(chip.render(
                format!("{element_name}-selection-{}", workspace_id.get()),
                hover,
            ))
            .when(!lifted, |row| {
                row.on_click(move |_, _, cx| {
                    let _ = click_sidebar.update(cx, |sidebar, cx| {
                        sidebar.cursor = None;
                        cx.emit(SidebarEvent::Activate {
                            workspace_id,
                            focus_pane: true,
                        });
                    });
                    cx.stop_propagation();
                })
            })
            .when(!renaming && !lifted, |row| {
                row.on_drag(
                    DraggedWorkspace {
                        workspace_id,
                        owner,
                    },
                    move |_, _, window, cx| {
                        let preview = drag_sidebar
                            .update(cx, |sidebar, cx| {
                                sidebar.begin_workspace_drag(workspace_id, window, cx)
                            })
                            .unwrap_or_else(|_| DragPreview::empty());
                        cx.new(|_| preview)
                    },
                )
            })
            .child(
                div()
                    .w(appearance.spacing(ICON_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(gpui_color(
                                icon_paint.map_or(row_colors.row_icon, |paint| paint.normal),
                            ))
                            .child(Icon::inherited(
                                if remote_connection_phase.is_some() {
                                    IconName::Globe
                                } else {
                                    IconName::Terminal
                                },
                                appearance.icons.metrics(IconRole::Row).glyph_size,
                            )),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(SIDEBAR_ROW_LINE_GAP))
                    .child(
                        div()
                            .flex()
                            .gap(appearance.spacing(ROW_CONTENT_GAP))
                            .child(div().min_w_0().flex_1().child(text::title(
                                name,
                                first_line,
                                if renaming { None } else { machine },
                                pinned && disclosing && !renaming,
                                workspace_id.get(),
                                row_text_appearance.clone(),
                            )))
                            .children(inline_trailing),
                    )
                    .when(!single_line, |lines| {
                        lines.child(text::detail(
                            detail,
                            repository.map(|badge| text::RowRepository {
                                badge,
                                pull_request_paint,
                            }),
                            pinned && !disclosing && detail_color.is_none(),
                            detail_paint,
                            detail_selector,
                            workspace_id.get(),
                            row_text_appearance,
                        ))
                    }),
            )
            // Rows rest on the continuous base surface without separators. The hover and selection
            // chips alone give each Workspace its shape; collection focus never adds a row ring.
            .when_some(collapsed_disclosure, |row, disclosure| {
                // A square target, so the end of line 2 stays the row's own.
                row.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(row_padding_trailing)
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .h(appearance.spacing(DISCLOSURE_WIDTH))
                                .child(disclosure),
                        ),
                )
            })
            .children(markers)
            .when_some(tracker, |row, tracker| row.child(tracker.tracker()));
        if lifted {
            return row_content.into_any_element();
        }
        let row = Tooltip::new(("workspace-row-tooltip", workspace_id.get()), tooltip_label)
            .detail(tooltip_text)
            .debug_selector(format!("workspace-row-tooltip-{}", workspace_id.get()))
            .attach(row_content, TooltipTargetVisibility::Visible)
            .into_any_element();

        if renaming {
            return div()
                .id(("workspace-menu", workspace_id.get()))
                .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
                .w_full()
                .flex_shrink_0()
                .child(row)
                .into_any_element();
        }

        let press_sidebar = sidebar.clone();
        let target = ContextMenuTarget::new(accesskit::Role::TreeItem, target_name)
            .description(target_detail)
            .selected(selected)
            .level(1);
        let target = match expanded {
            Some(expanded) => target.expanded(expanded),
            None => target,
        };
        let target = target.on_press(move |_, cx| {
            let _ = press_sidebar.update(cx, |sidebar, cx| {
                sidebar.cursor = None;
                cx.emit(SidebarEvent::Activate {
                    workspace_id,
                    focus_pane: true,
                });
            });
        });
        div()
            .id(("workspace-menu", workspace_id.get()))
            .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
            .w_full()
            .flex_shrink_0()
            .child(self.row_menu(
                SidebarRowKey::Workspace(workspace_id),
                format!("workspace-menu-controls-{}", workspace_id.get()),
                accessibility_name,
                row,
                // An expanded Workspace's Tabs belong to its Worktrees, whose rows open them.
                workspace_menu_entries(
                    !single_line,
                    creates_worktrees,
                    collapsed_worktree,
                    pinned,
                    remote_connection_phase,
                    presentation,
                ),
                target,
                sidebar,
                window,
                move |sidebar, command, window, cx| {
                    sidebar.perform_menu_command(workspace_id, command, window, cx);
                },
            ))
            .into_any_element()
    }

    /// Decorates a row with its context menu, which the keyboard opens from the selected row.
    #[expect(
        clippy::too_many_arguments,
        reason = "one menu decoration needs its row, entries, target, owner, and command handler"
    )]
    fn row_menu<A: Copy + 'static>(
        &self,
        key: SidebarRowKey,
        selector: String,
        accessibility_name: String,
        row: AnyElement,
        entries: Vec<MenuEntry<A>>,
        target: ContextMenuTarget,
        sidebar: WeakEntity<Self>,
        window: &Window,
        perform: impl Fn(&mut Self, A, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let open_sidebar = sidebar.clone();
        let lifecycle_sidebar = sidebar.clone();
        let lifecycle_window = window.window_handle();
        let keyboard = self.selected_key() == Some(key);
        ContextMenu::new(
            SharedString::from(selector.clone()),
            accessibility_name,
            row,
            entries,
        )
        .size(MenuSize::Wide)
        .when(keyboard, |menu| menu.keyboard_trigger(&self.focus))
        .target(target)
        .debug_selector(selector)
        .on_open_request(move |_, window, cx| {
            open_sidebar
                .update(cx, |sidebar, cx| sidebar.request_menu(key, window, cx))
                .unwrap_or(false)
        })
        .on_lifecycle(move |event, cx| {
            let sidebar = lifecycle_sidebar.clone();
            let event = *event;
            // Menu lifecycle delivery can occur while its Window is borrowed.
            // Resolve ownership after that delivery, using the current Window facts.
            cx.defer(move |cx| {
                let _ = lifecycle_window.update(cx, |_, window, cx| {
                    let _ = sidebar.update(cx, |sidebar, cx| {
                        sidebar.handle_menu_lifecycle(key, event, window, cx);
                    });
                });
            });
        })
        .on_activate(move |activation, window, cx| {
            let command = *activation.action();
            let _ = sidebar.update(cx, |sidebar, cx| perform(sidebar, command, window, cx));
        })
        .into_any_element()
    }

    /// Each row's hover, in list order.
    pub(super) fn row_hovers(&self, window: &mut Window, cx: &mut App) -> Vec<RowFade> {
        self.rows
            .iter()
            .map(|row| {
                let fade =
                    HoverFade::new(("workspace-row-hover", row.workspace_id.get()), window, cx);
                let worktrees = row
                    .worktrees
                    .iter()
                    .filter(|section| section.expanded)
                    .flat_map(|section| section.rows())
                    .map(|worktree| {
                        let fade = HoverFade::new(
                            SharedString::from(format!(
                                "worktree-row-hover-{}-{}",
                                row.workspace_id.get(),
                                worktree.worktree_id
                            )),
                            window,
                            cx,
                        );
                        RowFade {
                            level: fade.level(window, cx),
                            fade,
                            worktrees: Vec::new(),
                        }
                    })
                    .collect();
                RowFade {
                    level: fade.level(window, cx),
                    fade,
                    worktrees,
                }
            })
            .collect()
    }

    pub(super) fn render_body(
        &self,
        hovers: Vec<RowFade>,
        sidebar: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = crate::ui::appearance::chrome(cx);
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let footer_icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let settings_shortcut = presentation.shortcut(&crate::ui::settings_window::OpenSettings);
        let scroll_sidebar = sidebar.clone();
        let reorder_sidebar = sidebar.clone();
        let owner = sidebar.entity_id();
        let mut rows = div()
            .id("workspace-list")
            .role(accesskit::Role::Tree)
            .aria_label("Workspaces")
            .debug_selector(|| "workspace-list".to_owned())
            .w_full()
            .min_h_0()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .on_scroll_wheel(move |_, _, cx| {
                let _ = scroll_sidebar.update(cx, |sidebar, cx| {
                    sidebar.reveal_scrollbar(cx);
                });
            })
            .on_drag_move::<DraggedWorkspace>(move |event, _, cx| {
                let dragged = event.drag(cx);
                if dragged.owner != owner {
                    return;
                }
                let workspace_id = dragged.workspace_id;
                let pointer = event.event.position;
                let _ = reorder_sidebar.update(cx, |sidebar, cx| {
                    sidebar.drag_workspace_to(workspace_id, pointer, cx);
                });
            })
            .occlude();
        // The observer stays outside the list, whose children are exactly its rows.
        let release_sidebar = sidebar.clone();
        let release_observer = drag_release_observer(move |window, cx| {
            let pointer = window.mouse_position();
            let _ = release_sidebar.update(cx, |sidebar, cx| {
                sidebar.finish_workspace_drag(pointer, cx);
            });
        });
        // Each Workspace and its Worktrees are one scroll child, so reordering, revealing, and
        // drag previews stay indexed by Workspace.
        for (row, fade) in self.rows.iter().zip(hovers) {
            let workspace_id = row.workspace_id;
            let disclosed = row.worktrees.as_ref().filter(|section| section.expanded);
            let parent = self.render_workspace_row(
                row.clone(),
                RowRole::InList,
                RowHover {
                    level: fade.level,
                    tracker: Some(fade.fade),
                },
                sidebar.clone(),
                presentation,
                window,
                appearance,
                cx,
            );
            let (worktree_keys, children): (Vec<_>, Vec<_>) = disclosed
                .map(|section| {
                    self.render_worktrees(
                        workspace_id,
                        section,
                        fade.worktrees,
                        sidebar.clone(),
                        window,
                        appearance,
                        cx,
                    )
                })
                .unwrap_or_default()
                .into_iter()
                .unzip();
            let keys: Vec<Option<SidebarRowKey>> =
                std::iter::once(Some(SidebarRowKey::Workspace(workspace_id)))
                    .chain(worktree_keys)
                    .collect();
            let measure_sidebar = sidebar.clone();
            rows = rows.child(
                div()
                    .on_children_prepainted(move |bounds, _, cx| {
                        let _ = measure_sidebar.update(cx, |sidebar, _| {
                            sidebar.record_row_spans(workspace_id, &keys, &bounds);
                        });
                    })
                    .id(("workspace-group", workspace_id.get()))
                    .debug_selector(move || format!("workspace-group-{}", workspace_id.get()))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .child(parent)
                    .children(children),
            );
        }

        let scrollbar = self.scrollbar.clone();
        let menu_sidebar = sidebar.clone();
        let lifecycle_sidebar = sidebar.clone();
        let remote_disabled = self.remote_unavailable.is_some();
        let new_workspace_tooltip = crate::ui::workspace_creation::new_workspace_trigger_tooltip(
            self.remote_unavailable.as_deref(),
        );
        let new_workspace_menu = Menu::new(
            "new-workspace-menu",
            "New Workspace",
            new_workspace_menu_entries(presentation, remote_disabled),
        )
        .size(MenuSize::Wide)
        .placement(AnchoredPlacementConfig::new(
            AnchoredPlacement::Top,
            AnchoredAlignment::End,
        ))
        .icon_trigger(move |foreground| {
            div()
                .debug_selector(|| "new-workspace-icon".to_owned())
                .flex()
                .child(Icon::new(IconName::Plus, footer_icon_size, foreground))
                .into_any_element()
        })
        .debug_selector("new-workspace-button")
        .on_lifecycle(move |event, cx| {
            let sidebar = lifecycle_sidebar.clone();
            let event = *event;
            // Menu lifecycle delivery can occur while its Window is borrowed.
            // Resolve ownership after that delivery, as row menus do.
            cx.defer(move |cx| {
                let _ = sidebar.update(cx, |sidebar, cx| {
                    sidebar.handle_new_workspace_menu_lifecycle(event, cx);
                });
            });
        })
        .on_activate(move |activation, window, cx| match *activation.action() {
            WorkspaceCreation::Local => {
                let _ = menu_sidebar.update(cx, |_, cx| {
                    cx.emit(SidebarEvent::Create(WorkspaceCreation::Local));
                });
            }
            creation => {
                let sidebar = menu_sidebar.clone();
                let _ = menu_sidebar.update(cx, |sidebar, cx| {
                    sidebar.dismiss_editing(window, cx);
                });
                // A chooser opens after this menu closes so it captures Terminal Input Focus, not
                // the menu, to restore on cancel.
                cx.defer(move |cx| {
                    let _ = sidebar.update(cx, |_, cx| {
                        cx.emit(SidebarEvent::Create(creation));
                    });
                });
            }
        });
        let new_workspace_menu = Tooltip::new("new-workspace-tooltip", new_workspace_tooltip)
            .debug_selector("new-workspace-tooltip")
            .attach(new_workspace_menu, TooltipTargetVisibility::Visible);
        let sidebar = div()
            .id("workspace-sidebar")
            .debug_selector(|| "workspace-sidebar".to_owned())
            .absolute()
            .top(
                crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx)
                    .top_chrome_height(appearance.top_height()),
            )
            .bottom_0()
            .left_0()
            .w(self.layout.width)
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_key_down({
                let sidebar = sidebar.clone();
                move |event, window, cx| {
                    let _ =
                        sidebar.update(cx, |sidebar, cx| sidebar.on_key_down(event, window, cx));
                }
            })
            .font(appearance.typography.style(TextRole::Body).font.clone())
            .bg(gpui_color(appearance.surface(
                crate::appearance::SurfaceRole::Base,
                crate::ui::workspace_frame::base_surface(&appearance.colors),
            )))
            .occlude()
            .child(rows)
            .child(release_observer)
            .child(
                div()
                    .id("workspace-sidebar-footer")
                    .debug_selector(|| "workspace-sidebar-footer".to_owned())
                    .relative()
                    .w_full()
                    .h(appearance.spacing(NEW_WORKSPACE_BUTTON_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(appearance.spacing(SIDEBAR_FOOTER_HORIZONTAL_PADDING))
                    .child(
                        IconButton::new("open-settings-button", "Settings", move |foreground| {
                            div()
                                .debug_selector(|| "open-settings-icon".to_owned())
                                .flex()
                                .child(Icon::new(IconName::Cog, footer_icon_size, foreground))
                                .into_any_element()
                        })
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Regular)
                        .preserve_ancestor_hover()
                        .debug_selector("open-settings-button")
                        .tooltip(
                            Tooltip::new("open-settings-tooltip", "Settings")
                                .shortcut(settings_shortcut.unwrap_or_default())
                                .debug_selector("open-settings-tooltip"),
                        )
                        // The same application action the menu item and the Shortcut
                        // carry, so every way in reaches the one Settings Window.
                        .on_activate(move |_, window, cx| {
                            window.dispatch_action(
                                Box::new(crate::ui::settings_window::OpenSettings),
                                cx,
                            );
                        }),
                    )
                    .child(new_workspace_menu),
            )
            .child(scrollbar)
            .into_any_element();
        spaceterm_ui::ControlHost::Panel
            .mount(sidebar)
            .into_any_element()
    }

    pub(in crate::ui) fn render_resize_handle(
        &self,
        sidebar: WeakEntity<Self>,
        handle_width: Pixels,
        top_chrome_height: Pixels,
    ) -> AnyElement {
        let pointer_guard = Self::pointer_guard(sidebar.clone());
        let selector = "workspace-sidebar-resize-handle";
        let current_width = f32::from(handle_width);
        let handle = ResizeHandle::new(
            selector,
            "Resize Workspace sidebar",
            ResizeAxis::Horizontal,
            current_width,
        )
        .tab_stop(true)
        .reset_on_double_click(true)
        // The sidebar and the content stage share one continuous base surface, so the edge keeps
        // its resize target and cursor while only keyboard focus reveals an indicator.
        .paint_divider(false)
        .target(ResizeHandleTarget::SpaciousLeading(top_chrome_height))
        .debug_selector(selector)
        .on_event_with_accepted_value(move |event, window, cx| {
            let event = *event;
            sidebar
                .update(cx, |sidebar, cx| {
                    sidebar.handle_resize_event(event, window, cx)
                })
                .ok()
                .flatten()
        });
        // A hidden sidebar is zero wide, although its handle stays at the collapsed chrome's edge.
        let handle = if self.layout.visible {
            handle
        } else {
            handle.accessibility_value(0.0)
        };
        let handle = SidebarResizeHandle {
            handle,
            divider_position: handle_width,
        };
        let wrapper = div()
            .absolute()
            .top_0()
            .left(handle_width - px(CHROME_DIVIDER_SIZE / 2.0))
            .w(px(CHROME_DIVIDER_SIZE))
            .child(pointer_guard);
        if self.layout.visible {
            wrapper.bottom_0().child(handle).into_any_element()
        } else {
            wrapper
                .h(top_chrome_height)
                .child(handle)
                .into_any_element()
        }
    }
}
/// The sidebar Resize Handle, given the range the window allows when it renders.
#[derive(IntoElement)]
struct SidebarResizeHandle {
    handle: ResizeHandle,
    divider_position: Pixels,
}

impl RenderOnce for SidebarResizeHandle {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        self.handle.range(WorkspaceSidebar::resize_range(
            self.divider_position,
            window,
        ))
    }
}

impl WorkspaceSidebar {
    /// The trailing chevron that shows or hides a git Workspace's Worktrees. The lifted copy of
    /// a dragged row draws it without taking input.
    fn render_disclosure(
        &self,
        workspace_id: WorkspaceId,
        expanded: bool,
        interactive: bool,
        color: Color,
        sidebar: WeakEntity<Self>,
        appearance: &crate::ui::appearance::ChromeAppearance,
    ) -> AnyElement {
        let icon = div().text_color(gpui_color(color)).child(Icon::inherited(
            if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            },
            appearance.icons.metrics(IconRole::Caption).glyph_size,
        ));
        let column = div()
            .id(("workspace-disclosure", workspace_id.get()))
            .w(appearance.spacing(DISCLOSURE_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        if !interactive {
            return column.child(icon).into_any_element();
        }
        column
            .debug_selector(move || {
                format!(
                    "workspace-disclosure-{}-{}",
                    workspace_id.get(),
                    if expanded { "expanded" } else { "collapsed" }
                )
            })
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                let _ = sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_expanded(workspace_id, !expanded, window, cx);
                });
                cx.stop_propagation();
            })
            .child(icon)
            .into_any_element()
    }

    /// A git Workspace's disclosed Worktree rows, and a label above each former repository's.
    #[expect(
        clippy::too_many_arguments,
        reason = "one group render step needs its section, hovers, owner, and host geometry"
    )]
    fn render_worktrees(
        &self,
        workspace_id: WorkspaceId,
        section: &WorktreeSection,
        fades: Vec<RowFade>,
        sidebar: WeakEntity<Self>,
        window: &Window,
        appearance: &crate::ui::appearance::ChromeAppearance,
        cx: &App,
    ) -> Vec<(Option<SidebarRowKey>, AnyElement)> {
        let mut fades = fades.into_iter();
        let mut elements = Vec::new();
        let colors = appearance.host_colors(spaceterm_ui::ControlHost::Panel);
        for group in &section.groups {
            if let Some(name) = &group.former_repository {
                elements.push((
                    None,
                    div()
                        .debug_selector({
                            let name = name.clone();
                            move || format!("worktree-former-repository-{name}")
                        })
                        .relative()
                        .w_full()
                        .h(appearance.spacing(FORMER_REPOSITORY_ROW_HEIGHT))
                        .flex_shrink_0()
                        .flex()
                        .items_end()
                        .pl(worktree_text_indent(appearance, cx))
                        .pr(row_padding(appearance, cx).1)
                        .truncate()
                        .chrome_text(appearance.typography.style(TextRole::Secondary))
                        .text_color(gpui_color(colors.row_secondary))
                        .child(name.clone())
                        .into_any_element(),
                ));
            }
            let size = group.rows.len();
            for (index, worktree) in group.rows.iter().enumerate() {
                let Some(fade) = fades.next() else {
                    break;
                };
                elements.push((
                    Some(SidebarRowKey::Worktree(workspace_id, worktree.worktree_id)),
                    self.render_worktree_row(
                        workspace_id,
                        worktree,
                        (index + 1, size),
                        fade,
                        sidebar.clone(),
                        window,
                        appearance,
                        cx,
                    ),
                ));
            }
        }
        elements
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one row render step needs its model, set position, hover, owner, and host geometry"
    )]
    fn render_worktree_row(
        &self,
        workspace_id: WorkspaceId,
        worktree: &WorktreeRowViewModel,
        (position, size): (usize, usize),
        fade: RowFade,
        sidebar: WeakEntity<Self>,
        window: &Window,
        appearance: &crate::ui::appearance::ChromeAppearance,
        cx: &App,
    ) -> AnyElement {
        let worktree_id = worktree.worktree_id;
        let key = SidebarRowKey::Worktree(workspace_id, worktree_id);
        let selected = self.selected_key() == Some(key);
        let emphasized = appearance.active && self.has_visible_focus(window);
        let hover = if appearance.active { fade.level } else { 0.0 };
        let (chip, row_colors) = row_paint(selected, emphasized, hover, appearance, cx);
        let openable = worktree.openable();
        // A Worktree with no Tabs reads as secondary until it is opened.
        let title_color = if worktree.has_tabs && !worktree.missing {
            row_colors.row_foreground
        } else {
            row_colors.row_secondary
        };
        let mut text_appearance = appearance.clone();
        text_appearance.colors = row_colors.clone();
        let text_id = worktree_text_id(workspace_id, worktree_id);
        let selector = format!("worktree-row-{}-{worktree_id}", workspace_id.get());
        let head = if worktree.detached {
            format!("Detached at {}", worktree.label)
        } else {
            format!("Branch {}", worktree.label)
        };
        let mut states = Vec::new();
        if worktree.main {
            states.push("Main Worktree");
        }
        if worktree.missing {
            states.push("Missing");
        } else if worktree.locked {
            states.push("Locked");
        }
        if !worktree.has_tabs {
            states.push("No Tabs");
        }
        let description = std::iter::once(head.as_str())
            .chain(std::iter::once(worktree.directory_tooltip.as_ref()))
            .chain(states.iter().copied())
            .collect::<Vec<_>>()
            .join(", ");
        let tooltip_detail = std::iter::once(worktree.name.as_ref())
            .chain([head.as_str(), worktree.directory_tooltip.as_ref()])
            .chain(
                states
                    .iter()
                    .copied()
                    .filter(|state| *state != "Main Worktree"),
            )
            .collect::<Vec<_>>()
            .join("\n");
        // A Missing Worktree's directory gives way to its state, as an unavailable Workspace's does.
        let (detail, detail_paint, detail_selector) = if worktree.missing {
            (
                SharedString::from("Directory missing"),
                Some(workspace_row_status_paint(
                    row_colors.warning,
                    selected,
                    4.5,
                    &row_colors,
                )),
                format!("{selector}-missing"),
            )
        } else {
            (worktree.directory.clone(), None, format!("{selector}-path"))
        };
        let pull_request_paint = appearance.colors.pull_request(if selected {
            appearance.colors.row_selected_background
        } else {
            crate::ui::workspace_frame::base_surface(&appearance.colors)
        });
        let id = SharedString::from(selector.clone());
        let click_sidebar = sidebar.clone();
        let press_sidebar = sidebar.clone();
        let menu_sidebar = sidebar;
        let icon_size = appearance.icons.metrics(IconRole::Row).glyph_size;
        let row = div()
            .id(id)
            .debug_selector({
                let selector = selector.clone();
                move || {
                    format!(
                        "{selector}-{}",
                        if selected { "selected" } else { "unselected" }
                    )
                }
            })
            .relative()
            .w_full()
            .h(row_height(appearance))
            .flex_shrink_0()
            .pl(worktree_indent(appearance, cx))
            .pr(row_padding(appearance, cx).1)
            .flex()
            .items_center()
            .gap(appearance.spacing(ROW_CONTENT_GAP))
            .block_mouse_except_scroll()
            .when_some(row_background(appearance), |row, background| {
                row.bg(gpui_color(background))
            })
            .child(chip.render(format!("{selector}-selection"), hover))
            .when(openable, |row| {
                row.on_click(move |_, _, cx| {
                    let _ = click_sidebar.update(cx, |sidebar, cx| {
                        sidebar.cursor = None;
                        cx.emit(SidebarEvent::ActivateWorktree {
                            workspace_id,
                            worktree_id,
                            focus_pane: true,
                        });
                    });
                    cx.stop_propagation();
                })
            })
            .child(
                div()
                    .w(appearance.spacing(ICON_COLUMN_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(gpui_color(
                        detail_paint.map_or(row_colors.row_icon, |paint| paint.normal),
                    ))
                    .child(Icon::inherited(
                        if worktree.locked && !worktree.missing {
                            IconName::Lock
                        } else {
                            IconName::Folder
                        },
                        icon_size,
                    )),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(SIDEBAR_ROW_LINE_GAP))
                    .child(text::title(
                        worktree.name.clone(),
                        div()
                            .debug_selector({
                                let selector = selector.clone();
                                move || format!("{selector}-name")
                            })
                            .w_full()
                            .truncate()
                            .chrome_text(appearance.typography.style(TextRole::Navigation))
                            .text_color(gpui_color(title_color))
                            .child(worktree.name.clone())
                            .into_any_element(),
                        None,
                        false,
                        text_id,
                        text_appearance.clone(),
                    ))
                    .child(text::detail(
                        detail,
                        worktree
                            .repository
                            .clone()
                            .filter(|_| !worktree.missing)
                            .map(|badge| text::RowRepository {
                                badge,
                                pull_request_paint,
                            }),
                        false,
                        detail_paint,
                        Some(detail_selector),
                        text_id,
                        text_appearance,
                    )),
            )
            .child(fade.fade.tracker());
        let row = Tooltip::new(
            SharedString::from(format!("{selector}-tooltip")),
            if worktree.main {
                "Main Worktree"
            } else {
                "Worktree"
            },
        )
        .detail(tooltip_detail)
        .debug_selector(format!("{selector}-tooltip"))
        .attach(row, TooltipTargetVisibility::Visible)
        .into_any_element();
        let target = ContextMenuTarget::new(accesskit::Role::TreeItem, worktree.name.clone())
            .description(description)
            .selected(selected)
            .level(2)
            .set_position(position, size);
        // A Worktree that can't open keeps its menu, which still copies and removes it.
        let target = if openable {
            target.on_press(move |_, cx| {
                let _ = press_sidebar.update(cx, |sidebar, cx| {
                    sidebar.cursor = None;
                    cx.emit(SidebarEvent::ActivateWorktree {
                        workspace_id,
                        worktree_id,
                        focus_pane: true,
                    });
                });
            })
        } else {
            target
        };
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let mut entries = vec![
            new_tab_entry(
                WorktreeMenuCommand::NewTab,
                "worktree-menu-row-new-tab",
                presentation,
            )
            .disabled(!openable),
            MenuEntry::action("Copy Path", WorktreeMenuCommand::CopyPath)
                .icon(|foreground, size| {
                    Icon::new(IconName::Copy, size, foreground).into_any_element()
                })
                .debug_selector("worktree-menu-row-copy-path"),
            MenuEntry::action("Close Tabs", WorktreeMenuCommand::CloseTabs)
                .disabled(!worktree.has_tabs)
                .icon(|foreground, size| {
                    Icon::new(IconName::X, size, foreground).into_any_element()
                })
                .debug_selector("worktree-menu-row-close-tabs"),
        ];
        if let Some(remove) = remove_worktree_entry(
            WorktreeMenuCommand::Remove,
            worktree.removal,
            None,
            "worktree-menu-row-remove",
        ) {
            entries.extend([MenuEntry::separator(), remove]);
        }
        self.row_menu(
            key,
            format!("{selector}-menu"),
            format!("Worktree actions for {}", worktree.name),
            row,
            entries,
            target,
            menu_sidebar,
            window,
            move |_, command, _, cx| {
                cx.emit(SidebarEvent::WorktreeCommand {
                    workspace_id,
                    worktree_id,
                    command,
                });
            },
        )
    }
}

/// The element id a Worktree row's text uses. Workspace rows use their Workspace id, so a Worktree
/// row takes the upper half of the id space.
fn worktree_text_id(workspace_id: WorkspaceId, worktree_id: WorktreeId) -> u64 {
    (1 << 63) | ((workspace_id.get() & 0x7fff_ffff) << 32) | (worktree_id.get() & 0xffff_ffff)
}

/// "New Tab", with the Shortcut that creates a Tab.
fn new_tab_entry<A: Clone>(
    command: A,
    selector: &'static str,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> MenuEntry<A> {
    let entry = MenuEntry::action("New Tab", command);
    match presentation.shortcut(&crate::ui::CreateTab) {
        Some(shortcut) => entry.shortcut(shortcut),
        None => entry,
    }
    .icon(|foreground, size| Icon::new(IconName::SquarePlus, size, foreground).into_any_element())
    .debug_selector(selector)
}

/// "Remove Worktree…", named when a Workspace row stands for the Worktree. A Worktree that can't
/// be removed says why in place of the action; one git no longer lists offers none.
fn remove_worktree_entry<A: Clone>(
    command: A,
    removal: WorktreeRemoval,
    name: Option<&str>,
    selector: &'static str,
) -> Option<MenuEntry<A>> {
    let reason = match removal {
        WorktreeRemoval::Allowed => None,
        WorktreeRemoval::Main => Some("Main Worktree Can\u{2019}t Be Removed"),
        WorktreeRemoval::Locked => Some("Locked Worktree Can\u{2019}t Be Removed"),
        WorktreeRemoval::HoldsPinnedDirectory => Some("Unpin the Directory to Remove"),
        WorktreeRemoval::Unlisted => return None,
    };
    let label = match (reason, name) {
        (Some(reason), _) => SharedString::from(reason),
        (None, Some(name)) => format!("Remove Worktree \u{201c}{name}\u{201d}\u{2026}").into(),
        (None, None) => SharedString::from("Remove Worktree\u{2026}"),
    };
    Some(
        MenuEntry::action(label, command)
            .disabled(reason.is_some())
            .destructive(reason.is_none())
            .icon(|foreground, size| {
                Icon::new(IconName::Trash2, size, foreground).into_any_element()
            })
            .debug_selector(selector),
    )
}

/// "New Worktree…", which presents the New Worktree dialog.
fn new_worktree_entry<A: Clone>(
    command: A,
    selector: &'static str,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> MenuEntry<A> {
    let entry = MenuEntry::action(NEW_WORKTREE_LABEL, command);
    match presentation.shortcut(&crate::ui::NewWorktree) {
        Some(shortcut) => entry.shortcut(shortcut),
        None => entry,
    }
    .icon(|foreground, size| Icon::new(IconName::GitBranch, size, foreground).into_any_element())
    .debug_selector(selector)
}

const NEW_WORKTREE_LABEL: &str = "New Worktree\u{2026}";

/// The "+" an expanded git Workspace row shows under the pointer and while selected.
fn new_worktree_button(
    workspace_id: WorkspaceId,
    reveal: f32,
    sidebar: WeakEntity<WorkspaceSidebar>,
    appearance: &crate::ui::appearance::ChromeAppearance,
) -> AnyElement {
    let glyph_size = appearance.icons.metrics(IconRole::Caption).glyph_size;
    div()
        .flex_shrink_0()
        .opacity(reveal)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            IconButton::new(
                ("workspace-new-worktree", workspace_id.get()),
                NEW_WORKTREE_LABEL,
                move |foreground| {
                    Icon::new(IconName::Plus, glyph_size, foreground).into_any_element()
                },
            )
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Small)
            .tab_stop(false)
            .preserve_ancestor_hover()
            .debug_selector(format!("workspace-new-worktree-{}", workspace_id.get()))
            .tooltip(
                Tooltip::new(
                    ("workspace-new-worktree-tooltip", workspace_id.get()),
                    NEW_WORKTREE_LABEL,
                )
                .debug_selector(format!(
                    "workspace-new-worktree-tooltip-{}",
                    workspace_id.get()
                )),
            )
            .on_activate(move |_, _, cx| {
                let _ = sidebar.update(cx, |_, cx| {
                    cx.emit(SidebarEvent::Command {
                        workspace_id,
                        command: WorkspaceMenuCommand::NewWorktree,
                    });
                });
            }),
        )
        .into_any_element()
}

/// The footer creation menu's rows, built from the shared creation descriptors.
fn new_workspace_menu_entries(
    presentation: &crate::desktop_profile::DesktopPresentation,
    remote_disabled: bool,
) -> Vec<MenuEntry<WorkspaceCreation>> {
    let mut entries = Vec::new();
    for creation in WorkspaceCreation::ALL {
        if creation.starts_group() {
            entries.push(MenuEntry::separator());
        }
        let entry = MenuEntry::action(creation.label(), creation);
        let entry = match creation.shortcut(presentation) {
            Some(shortcut) => entry.shortcut(shortcut),
            None => entry,
        };
        entries.push(
            entry
                .icon(move |foreground, size| {
                    Icon::custom(creation.icon(), size, foreground).into_any_element()
                })
                .disabled(remote_disabled && creation.is_remote())
                .debug_selector(format!("new-workspace-menu-{}", creation.selector())),
        );
    }
    entries
}

/// Every Workspace command carries a symbol, so all labels share the icon column.
fn workspace_menu_entries(
    new_tab: bool,
    new_worktree: bool,
    active_worktree: Option<(WorktreeId, SharedString, WorktreeRemoval)>,
    pinned: bool,
    remote_connection_phase: Option<RemoteConnectionPhase>,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<MenuEntry<RowMenuCommand>> {
    let mut entries = Vec::new();
    if new_tab {
        entries.push(new_tab_entry(
            RowMenuCommand::Workspace(WorkspaceMenuCommand::NewTab),
            "workspace-menu-row-new-tab",
            presentation,
        ));
    }
    if new_worktree {
        entries.push(new_worktree_entry(
            RowMenuCommand::Workspace(WorkspaceMenuCommand::NewWorktree),
            "workspace-menu-row-new-worktree",
            presentation,
        ));
    }
    entries.push(
        MenuEntry::action("Rename Workspace", RowMenuCommand::Rename)
            .icon(|foreground, size| {
                Icon::new(IconName::Pencil, size, foreground).into_any_element()
            })
            .debug_selector("workspace-menu-row-rename"),
    );
    entries.push(
        MenuEntry::action(
            if pinned {
                "Change Pinned Directory"
            } else {
                "Pin workspace to a directory"
            },
            RowMenuCommand::Workspace(WorkspaceMenuCommand::PinDirectory),
        )
        .icon(|foreground, size| Icon::new(IconName::Pin, size, foreground).into_any_element())
        .debug_selector("workspace-menu-row-pin-directory"),
    );
    if pinned {
        entries.push(
            MenuEntry::action(
                "Unpin Directory",
                RowMenuCommand::Workspace(WorkspaceMenuCommand::UnpinDirectory),
            )
            .icon(|foreground, size| {
                Icon::new(IconName::PinOff, size, foreground).into_any_element()
            })
            .debug_selector("workspace-menu-row-unpin-directory"),
        );
    }
    if matches!(
        remote_connection_phase,
        Some(RemoteConnectionPhase::Disconnected | RemoteConnectionPhase::Failed)
    ) {
        entries.push(
            MenuEntry::action(
                "Reconnect",
                RowMenuCommand::Workspace(WorkspaceMenuCommand::Reconnect),
            )
            .icon(|foreground, size| {
                Icon::new(IconName::RotateCw, size, foreground).into_any_element()
            })
            .debug_selector("workspace-menu-row-reconnect"),
        );
    }
    // The destructive commands sit together below the divider.
    entries.push(MenuEntry::separator());
    if let Some((worktree_id, name, removal)) = active_worktree
        && let Some(entry) = remove_worktree_entry(
            RowMenuCommand::Worktree(worktree_id, WorktreeMenuCommand::Remove),
            removal,
            Some(&name),
            "workspace-menu-row-remove-worktree",
        )
    {
        entries.push(entry);
    }
    entries.push(
        MenuEntry::action(
            "Close Workspace",
            RowMenuCommand::Workspace(WorkspaceMenuCommand::Close),
        )
        .destructive(true)
        .icon(|foreground, size| Icon::new(IconName::X, size, foreground).into_any_element())
        .debug_selector("workspace-menu-row-close"),
    );
    entries
}

impl WorkspaceSidebar {
    fn pointer_guard(sidebar: WeakEntity<Self>) -> AnyElement {
        let suppressed_move_sidebar = sidebar.clone();
        let suppressed_up_sidebar = sidebar;
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let suppressed = suppressed_move_sidebar
                        .update(cx, |sidebar, cx| {
                            if !sidebar.suppress_pointer_until_release {
                                return false;
                            }
                            if event.pressed_button != Some(MouseButton::Left) {
                                sidebar.suppress_pointer_until_release = false;
                                cx.notify();
                            }
                            true
                        })
                        .unwrap_or(false);
                    if suppressed {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                        return;
                    }
                    let suppressed = suppressed_up_sidebar
                        .update(cx, |sidebar, cx| {
                            if !sidebar.suppress_pointer_until_release {
                                return false;
                            }
                            sidebar.suppress_pointer_until_release = false;
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if suppressed {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}
