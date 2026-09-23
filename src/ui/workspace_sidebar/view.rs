use super::*;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};

/// The chip carrying a Workspace row's hover and persistent selection.
///
/// The geometry and the paints are read together so the fill, the hover state, and the keyboard
/// focus ring cannot drift apart.
fn row_chip(
    selected: bool,
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
    let paint = if selected {
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
///
/// Each side's padding is the chip's margin on that side plus the air the content keeps inside the
/// chip.
fn row_padding(appearance: &crate::ui::appearance::ChromeAppearance, cx: &App) -> (Pixels, Pixels) {
    let frame = crate::ui::workspace_frame::WorkspaceFrame::for_appearance(appearance, cx);
    let air = appearance.spacing(SIDEBAR_ROW_CHIP_PADDING);
    (
        frame.sidebar_chip_leading_inset() + air,
        frame.sidebar_chip_trailing_inset() + air,
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
            cx,
        )
        .debug_selector(move || format!("workspace-rename-input-{}", self.workspace_id.get()))
        .h(field_height)
        .w_full()
        .px(self.appearance.spacing(5.0))
        .flex()
        .items_center()
        .rounded(RadiusRole::ControlSmall.pixels())
        .chrome_text(text_style)
        .text_color(gpui_color(self.appearance.colors.text))
        .on_click(move |_, window, cx| {
            focus_on_click.focus(window);
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

impl WorkspaceSidebar {
    fn render_workspace_row(
        &self,
        row: WorkspaceRowViewModel,
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
            tab_count,
            pane_count,
            active,
        } = row;
        let row_background = row_background(appearance);
        let mut row_colors = appearance
            .host_colors(spaceterm_ui::ControlHost::Panel)
            .clone();
        let row_group = format!("workspace-row-state-{}", workspace_id.get());
        // Hover and selection are carried by an inset chip rather than by the row's own fill, so
        // the strip keeps the sidebar surface and the current Workspace reads as a resting shape
        // with air around it. The chip's paints are read before the selected colors are promoted
        // below, because that promotion is what the row's text and icons consume.
        let selection_colors = if appearance.active && !self.focus.is_focused(window) {
            appearance
                .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel)
                .clone()
        } else {
            row_colors.clone()
        };
        let chip = row_chip(active, appearance, &row_colors, &selection_colors, cx);
        if active {
            row_colors.row_selected_background = selection_colors.row_selected_background;
            row_colors.row_selected_hover_background =
                selection_colors.row_selected_hover_background;
            row_colors.row_foreground = selection_colors.row_selected_foreground;
            row_colors.row_secondary = selection_colors.row_selected_secondary;
            row_colors.row_icon = selection_colors.row_selected_icon;
            row_colors.row_hover_foreground = selection_colors.row_selected_hover_foreground;
            row_colors.row_hover_secondary = selection_colors.row_selected_hover_secondary;
            row_colors.row_hover_icon = selection_colors.row_selected_hover_icon;
        }
        // Text helpers consume no materials or surfaces. Give only those helpers the promoted
        // Panel-host row colors while all surface composition keeps the root appearance.
        let mut row_text_appearance = appearance.clone();
        row_text_appearance.colors = row_colors.clone();
        let click_sidebar = sidebar.clone();
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let remote_color =
            remote_connection_phase.map(|phase| remote_connection_color(phase, &row_colors));
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
        let detail_paint =
            detail_color.map(|color| workspace_row_status_paint(color, active, 4.5, &row_colors));
        // The row icon carries one status in the same precedence as the collapsed identity: an
        // unavailable directory first, then the Remote connection.
        let icon_paint = (!available)
            .then_some(row_colors.warning)
            .or(remote_color)
            .map(|color| workspace_row_status_paint(color, active, 3.0, &row_colors));
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
                .when(appearance.active, |label| {
                    label.group_hover(row_group.clone(), |style| {
                        style.text_color(gpui_color(row_colors.row_hover_foreground))
                    })
                })
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
        } else if pinned {
            "Pinned Directory"
        } else {
            "Workspace Directory"
        };

        let (row_padding_leading, row_padding_trailing) = row_padding(appearance, cx);
        let row_content = div()
            .id(("workspace-row", workspace_id.get()))
            .debug_selector(move || {
                format!(
                    "workspace-row-{}-{}",
                    workspace_id.get(),
                    if active { "active" } else { "inactive" }
                )
            })
            .relative()
            .w_full()
            .h(row_height(appearance))
            .flex_shrink_0()
            .pl(row_padding_leading)
            .pr(row_padding_trailing)
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .block_mouse_except_scroll()
            .group(row_group.clone())
            .when_some(row_background, |row, background| {
                row.bg(gpui_color(background))
            })
            .child(chip.render(
                format!("workspace-row-selection-{}", workspace_id.get()),
                &row_group,
            ))
            .on_click(move |_, _, cx| {
                let _ = click_sidebar.update(cx, |_, cx| {
                    cx.emit(SidebarEvent::Activate {
                        workspace_id,
                        focus_pane: true,
                    });
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .w(appearance.spacing(18.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_color(gpui_color(
                                icon_paint.map_or(row_colors.row_icon, |paint| paint.normal),
                            ))
                            .when(appearance.active, |icon| {
                                icon.group_hover(row_group.clone(), |style| {
                                    style.text_color(gpui_color(
                                        icon_paint.map_or(row_colors.row_hover_icon, |paint| {
                                            paint.hovered
                                        }),
                                    ))
                                })
                            })
                            .child(Icon::inherited(
                                if remote_connection_phase.is_some() {
                                    IconName::Globe
                                } else {
                                    IconName::Terminal
                                },
                                appearance.icons.metrics(IconRole::Row).glyph_size,
                            ))
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(SIDEBAR_ROW_LINE_GAP))
                    .child(text::title(
                        name,
                        first_line,
                        if renaming { None } else { machine },
                        workspace_id.get(),
                        row_text_appearance.clone(),
                    ))
                    .child(text::detail(
                        detail,
                        format!("{tab_count}T · {pane_count}P").into(),
                        pinned && detail_color.is_none(),
                        detail_paint,
                        detail_selector,
                        workspace_id.get(),
                        row_text_appearance,
                    )),
            )
            // Rows rest on the continuous base surface without separators. The hover and selection
            // chips alone give each Workspace its shape; collection focus never adds a row ring.
            ;
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

        let open_sidebar = sidebar.clone();
        let lifecycle_sidebar = sidebar.clone();
        let lifecycle_window = window.window_handle();
        let activate_sidebar = sidebar;
        div()
            .id(("workspace-menu", workspace_id.get()))
            .debug_selector(move || format!("workspace-menu-{}", workspace_id.get()))
            .w_full()
            .flex_shrink_0()
            .child(
                ContextMenu::new(
                    ("workspace-menu-controls", workspace_id.get()),
                    accessibility_name,
                    row,
                    workspace_menu_entries(pinned, remote_connection_phase, presentation),
                )
                .size(MenuSize::Wide)
                .when(active, |menu| menu.keyboard_trigger(&self.focus))
                .debug_selector(format!("workspace-menu-controls-{}", workspace_id.get()))
                .on_open_request(move |_, window, cx| {
                    open_sidebar
                        .update(cx, |sidebar, cx| {
                            sidebar.request_menu(workspace_id, window, cx)
                        })
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
                                sidebar.handle_menu_lifecycle(workspace_id, event, window, cx);
                            });
                        });
                    });
                })
                .on_activate(move |activation, window, cx| {
                    let command = *activation.action();
                    let _ = activate_sidebar.update(cx, |sidebar, cx| {
                        sidebar.perform_menu_command(workspace_id, command, window, cx);
                    });
                }),
            )
            .into_any_element()
    }

    pub(super) fn render_body(
        &self,
        sidebar: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let appearance = crate::ui::appearance::chrome(cx);
        let presentation = crate::desktop_profile::DesktopPresentation::get(cx);
        let footer_icon_size = appearance.icons.metrics(IconRole::Control).glyph_size;
        let new_local_shortcut = presentation.shortcut(&crate::ui::NewWorkspace);
        let new_remote_shortcut = presentation.shortcut(&NewRemoteWorkspace);
        let settings_shortcut = presentation.shortcut(&crate::ui::settings_window::OpenSettings);
        let scroll_sidebar = sidebar.clone();
        let mut rows = div()
            .id("workspace-list")
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
            .occlude();
        for row in &self.rows {
            rows = rows.child(self.render_workspace_row(
                row.clone(),
                sidebar.clone(),
                presentation,
                window,
                appearance,
                cx,
            ));
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
            new_workspace_menu_entries(new_local_shortcut, new_remote_shortcut, remote_disabled),
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
            NewWorkspaceMenuCommand::Local => {
                let _ = menu_sidebar.update(cx, |_, cx| {
                    cx.emit(SidebarEvent::NewLocalWorkspace);
                });
            }
            NewWorkspaceMenuCommand::Remote => {
                let sidebar = menu_sidebar.clone();
                let _ = menu_sidebar.update(cx, |sidebar, cx| {
                    sidebar.dismiss_editing(window, cx);
                });
                // The flow opens after this menu closes so it captures Terminal Input
                // Focus (not the menu) to restore on cancel, matching the switcher's
                // creation rows.
                cx.defer(move |cx| {
                    let _ = sidebar.update(cx, |_, cx| {
                        cx.emit(SidebarEvent::NewRemoteWorkspace);
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
                    // Settings stands alone at the leading end: it is application scoped, while
                    // the menu opposite it adds a Workspace to this window. Keeping the odd one
                    // out apart says which is which without a label.
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
                                .keyboard_equivalent(settings_shortcut)
                                .debug_selector("open-settings-tooltip"),
                        )
                        // The same application action the menu item and the keyboard equivalent
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
        .on_event(move |event, window, cx| {
            let event = *event;
            let _ = sidebar.update(cx, |sidebar, cx| {
                sidebar.handle_resize_event(event, window, cx);
            });
        });
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
/// The footer creation menu's rows mirror the Workspace switcher's creation rows: the same
/// labels, leading icons, and trailing shortcuts, presented as a button-triggered menu.
/// Labels and icons come from the shared creation descriptors so the two surfaces cannot drift.
fn new_workspace_menu_entries(
    local_shortcut: impl Into<SharedString>,
    remote_shortcut: impl Into<SharedString>,
    remote_disabled: bool,
) -> Vec<MenuEntry<NewWorkspaceMenuCommand>> {
    use crate::ui::workspace_creation::{
        LOCAL_WORKSPACE_ICON, LOCAL_WORKSPACE_LABEL, REMOTE_WORKSPACE_ICON, REMOTE_WORKSPACE_LABEL,
    };
    vec![
        MenuEntry::action(LOCAL_WORKSPACE_LABEL, NewWorkspaceMenuCommand::Local)
            .shortcut(local_shortcut)
            .icon(|foreground, size| {
                Icon::custom(LOCAL_WORKSPACE_ICON, size, foreground).into_any_element()
            })
            .debug_selector("new-workspace-menu-create-local"),
        MenuEntry::action(REMOTE_WORKSPACE_LABEL, NewWorkspaceMenuCommand::Remote)
            .shortcut(remote_shortcut)
            .icon(|foreground, size| {
                Icon::custom(REMOTE_WORKSPACE_ICON, size, foreground).into_any_element()
            })
            .disabled(remote_disabled)
            .debug_selector("new-workspace-menu-create-remote"),
    ]
}

/// Management commands use labels and shortcuts. Creation commands retain Local/Remote symbols.
fn workspace_menu_entries(
    pinned: bool,
    remote_connection_phase: Option<RemoteConnectionPhase>,
    presentation: &crate::desktop_profile::DesktopPresentation,
) -> Vec<MenuEntry<RowMenuCommand>> {
    let shortcut = presentation.shortcut(&crate::ui::CreateTab);
    let mut entries = vec![
        MenuEntry::action(
            "New Tab",
            RowMenuCommand::Workspace(WorkspaceMenuCommand::NewTab),
        )
        .shortcut(shortcut)
        .debug_selector("workspace-menu-row-new-tab"),
        MenuEntry::action("Rename Workspace", RowMenuCommand::Rename)
            .debug_selector("workspace-menu-row-rename"),
    ];
    entries.push(
        MenuEntry::action(
            if pinned {
                "Change Pinned Directory"
            } else {
                "Pin workspace to a directory"
            },
            RowMenuCommand::Workspace(WorkspaceMenuCommand::PinDirectory),
        )
        .debug_selector("workspace-menu-row-pin-directory"),
    );
    if pinned {
        entries.push(
            MenuEntry::action(
                "Unpin Directory",
                RowMenuCommand::Workspace(WorkspaceMenuCommand::UnpinDirectory),
            )
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
            .debug_selector("workspace-menu-row-reconnect"),
        );
    }
    entries.extend([
        MenuEntry::separator(),
        MenuEntry::action(
            "Close Workspace",
            RowMenuCommand::Workspace(WorkspaceMenuCommand::Close),
        )
        .destructive(true)
        .debug_selector("workspace-menu-row-close"),
    ]);
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
