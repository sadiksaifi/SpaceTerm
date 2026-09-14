use super::*;
use crate::ui::selection_chip::{ChipPaint, ChipShape, SelectionChip};

/// The chip carrying a Workspace row's hover and persistent selection.
///
/// The geometry and the paints are read together so the fill, the hover state, and the keyboard
/// focus ring cannot drift apart.
fn row_chip(selected: bool, appearance: &crate::ui::appearance::ChromeAppearance) -> SelectionChip {
    let colors = &appearance.colors;
    let paint = if selected {
        ChipPaint {
            fill: Some(colors.row_selected_background),
            rim: Some(colors.row_selected_border),
            hover_fill: Some(colors.row_selected_hover_background),
            hover_rim: Some(colors.row_selected_hover_border),
        }
    } else {
        ChipPaint {
            fill: None,
            rim: None,
            hover_fill: Some(colors.row_hover_background),
            hover_rim: Some(colors.row_hover_border),
        }
    };
    SelectionChip::new(
        ChipShape {
            inset_x: appearance.spacing(SIDEBAR_ROW_SELECTION_INSET_X),
            inset_y: appearance.spacing(SIDEBAR_ROW_SELECTION_INSET_Y),
            radius: appearance.spacing(SIDEBAR_ROW_SELECTION_RADIUS),
        },
        paint,
    )
}

struct WorkspaceRowLayout {
    row: WorkspaceRowViewModel,
    divider: bool,
}

impl WorkspaceSidebar {
    fn render_workspace_row(
        &self,
        layout: WorkspaceRowLayout,
        sidebar: WeakEntity<Self>,
        presentation: &crate::desktop_profile::DesktopPresentation,
        window: &Window,
        appearance: &crate::ui::appearance::ChromeAppearance,
        cx: &App,
    ) -> AnyElement {
        let WorkspaceRowLayout { row, divider } = layout;
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
        let mut appearance = appearance.clone();
        let row_group = format!("workspace-row-state-{}", workspace_id.get());
        // Hover and selection are carried by an inset chip rather than by the row's own fill, so
        // the strip keeps the sidebar surface and the current Workspace reads as a resting shape
        // with air around it. The chip's paints are read before the selected roles are promoted
        // below, because that promotion is what the row's text and icons consume.
        let chip = row_chip(active, &appearance);
        if active {
            appearance.colors.row_foreground = appearance.colors.row_selected_foreground;
            appearance.colors.row_secondary = appearance.colors.row_selected_secondary;
            appearance.colors.row_icon = appearance.colors.row_selected_icon;
            appearance.colors.row_hover_foreground =
                appearance.colors.row_selected_hover_foreground;
            appearance.colors.row_hover_secondary = appearance.colors.row_selected_hover_secondary;
            appearance.colors.row_hover_icon = appearance.colors.row_selected_hover_icon;
        }
        let click_sidebar = sidebar.clone();
        let remote_status = remote_connection_phase.and_then(remote_connection_status);
        let remote_color =
            remote_connection_phase.map(|phase| remote_connection_color(phase, &appearance.colors));
        let (detail, detail_color, detail_selector) = if !available {
            (
                "Directory unavailable".into(),
                Some(appearance.colors.warning),
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
        let detail_paint = detail_color
            .map(|color| workspace_status_paint(color, active, 4.5, &appearance.colors));
        let remote_icon_paint = remote_color
            .map(|color| workspace_status_paint(color, active, 3.0, &appearance.colors));
        let unavailable_icon_paint = (!available).then(|| {
            workspace_status_paint(appearance.colors.warning, active, 3.0, &appearance.colors)
        });
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
            let input = rename.input.clone();
            let focus_handle = rename.focus_handle.clone();
            spaceterm_ui::field_frame(
                ("workspace-rename-input", workspace_id.get()),
                &focus_handle,
                spaceterm_ui::FieldState::default(),
                cx,
            )
            .debug_selector(move || format!("workspace-rename-input-{}", workspace_id.get()))
            .h(appearance.height(22.0, SIDEBAR_NAME_TEXT_SIZE))
            .w_full()
            .px(appearance.spacing(5.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(appearance.spacing(4.0))
            .font(appearance.regular.clone())
            .text_size(appearance.text_size(SIDEBAR_NAME_TEXT_SIZE))
            .text_color(gpui_color(appearance.colors.text))
            .on_click(move |_, window, cx| {
                focus_handle.focus(window);
                cx.stop_propagation();
            })
            .child(input)
            .into_any_element()
        } else {
            div()
                .id(("workspace-row-name", workspace_id.get()))
                .debug_selector(move || format!("workspace-row-name-{}", workspace_id.get()))
                .w_full()
                .truncate()
                .font(appearance.emphasis.clone())
                .text_size(appearance.text_size(SIDEBAR_NAME_TEXT_SIZE))
                .text_color(gpui_color(appearance.colors.row_foreground))
                .group_hover(row_group.clone(), |style| {
                    style.text_color(gpui_color(appearance.colors.row_hover_foreground))
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
            .h(appearance.height(SIDEBAR_ROW_HEIGHT, SIDEBAR_NAME_TEXT_SIZE))
            .flex_shrink_0()
            .px(appearance.spacing(SIDEBAR_ROW_HORIZONTAL_PADDING))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(10.0))
            .block_mouse_except_scroll()
            .group(row_group.clone())
            .bg(gpui_color(appearance.colors.row_background))
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
                            .relative()
                            .text_color(gpui_color(
                                remote_icon_paint
                                    .map_or(appearance.colors.row_icon, |paint| paint.normal),
                            ))
                            .group_hover(row_group.clone(), |style| {
                                style.text_color(gpui_color(
                                    remote_icon_paint
                                        .map_or(appearance.colors.row_hover_icon, |paint| {
                                            paint.hovered
                                        }),
                                ))
                            })
                            .child(Icon::inherited(
                                if remote_connection_phase.is_some() {
                                    IconName::Globe
                                } else {
                                    IconName::Terminal
                                },
                                appearance.spacing(SIDEBAR_ROW_ICON_SIZE),
                            ))
                            .when(!available, |icon| {
                                icon.child(
                                    div()
                                        .absolute()
                                        .right(px(-5.0))
                                        .bottom(px(-4.0))
                                        .text_color(gpui_color(
                                            unavailable_icon_paint
                                                .map_or(appearance.colors.warning, |paint| {
                                                    paint.normal
                                                }),
                                        ))
                                        .group_hover(row_group.clone(), |style| {
                                            style.text_color(gpui_color(
                                                unavailable_icon_paint
                                                    .map_or(appearance.colors.warning, |paint| {
                                                        paint.hovered
                                                    }),
                                            ))
                                        })
                                        .child(Icon::inherited(
                                            IconName::TriangleAlert,
                                            appearance.spacing(10.0),
                                        )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(2.0))
                    .child(text::title(
                        name,
                        first_line,
                        if renaming { None } else { machine },
                        workspace_id.get(),
                        appearance.clone(),
                    ))
                    .child(text::detail(
                        detail,
                        format!("{tab_count}T · {pane_count}P").into(),
                        pinned && detail_color.is_none(),
                        detail_paint,
                        detail_selector,
                        workspace_id.get(),
                        appearance.clone(),
                    )),
            )
            // A divider that runs up to a selected chip fences it in. Both seams that touch the
            // selection are left open instead, so the chip keeps the air that makes it read as one
            // resting shape rather than as a band cut out of the list.
            .when(divider, |row| {
                row.child(
                    div()
                        .id(("workspace-row-divider", workspace_id.get()))
                        .debug_selector(move || {
                            format!("workspace-row-divider-{}", workspace_id.get())
                        })
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(CHROME_DIVIDER_SIZE))
                        .bg(gpui_color(appearance.colors.border_variant)),
                )
            })
            // Keyboard focus rides just outside the chip it belongs to, with a hairline of the
            // sidebar surface between the two. A ring drawn on the row's own edges would box the
            // whole strip and say nothing about which shape the keyboard is pointing at.
            .when(active && self.focus.is_focused(window), |row| {
                row.child(chip.ring(
                    appearance.spacing(SIDEBAR_ROW_SELECTION_RING_GAP),
                    appearance.colors.sidebar_focus,
                    "workspace-sidebar-focus-indicator",
                ))
            });
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
        let footer_icon_size = appearance.spacing(SIDEBAR_FOOTER_ICON_SIZE);
        let shortcuts = presentation.shortcut(&crate::ui::NewWorkspace);
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
        for (index, row) in self.rows.iter().enumerate() {
            let next_selected = self.rows.get(index + 1).is_some_and(|next| next.active);
            rows = rows.child(self.render_workspace_row(
                WorkspaceRowLayout {
                    row: row.clone(),
                    divider: !row.active && !next_selected,
                },
                sidebar.clone(),
                presentation,
                window,
                appearance,
                cx,
            ));
        }

        let scrollbar = self.scrollbar.clone();
        let local_sidebar = sidebar.clone();
        let remote_tooltip = self
            .remote_unavailable
            .clone()
            .unwrap_or_else(|| "New Remote Workspace".to_owned());
        div()
            .id("workspace-sidebar")
            .debug_selector(|| "workspace-sidebar".to_owned())
            .absolute()
            .top(appearance.top_height())
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
            .font(appearance.regular.clone())
            .bg(gpui_color(appearance.colors.panel_background))
            .occlude()
            .child(rows)
            .child(
                div()
                    .id("workspace-sidebar-footer")
                    .debug_selector(|| "workspace-sidebar-footer".to_owned())
                    .relative()
                    .w_full()
                    .h(appearance.height(NEW_WORKSPACE_BUTTON_HEIGHT, SIDEBAR_NAME_TEXT_SIZE))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(appearance.spacing(SIDEBAR_FOOTER_HORIZONTAL_PADDING))
                    // Settings stands alone at the leading end: it is application scoped, while
                    // the two buttons opposite it both add a Workspace to this window. Putting the
                    // pair together and the odd one out apart says which is which without a label.
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(
                                IconButton::new(
                                    "new-remote-workspace-button",
                                    "New Remote Workspace",
                                    move |foreground| {
                                        div()
                                            .debug_selector(|| {
                                                "new-remote-workspace-icon".to_owned()
                                            })
                                            .flex()
                                            .child(Icon::custom(
                                                CustomIconName::GlobePlus,
                                                footer_icon_size,
                                                foreground,
                                            ))
                                            .into_any_element()
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Regular)
                                .disabled(self.remote_unavailable.is_some())
                                .preserve_ancestor_hover()
                                .debug_selector("new-remote-workspace-button")
                                .tooltip(
                                    Tooltip::new("new-remote-workspace-tooltip", remote_tooltip)
                                        .keyboard_equivalent(
                                            presentation.shortcut(&NewRemoteWorkspace),
                                        )
                                        .debug_selector("new-remote-workspace-tooltip"),
                                )
                                .on_activate(
                                    move |_, window, cx| {
                                        let _ = sidebar.update(cx, |sidebar, cx| {
                                            sidebar.dismiss_editing(window, cx);
                                            cx.emit(SidebarEvent::NewRemoteWorkspace);
                                        });
                                    },
                                ),
                            )
                            .child(
                                IconButton::new(
                                    "new-local-workspace-button",
                                    "New Local Workspace",
                                    move |foreground| {
                                        div()
                                            .debug_selector(|| {
                                                "new-local-workspace-icon".to_owned()
                                            })
                                            .flex()
                                            .child(Icon::custom(
                                                CustomIconName::RectangleStackBadgePlus,
                                                footer_icon_size,
                                                foreground,
                                            ))
                                            .into_any_element()
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Regular)
                                .preserve_ancestor_hover()
                                .debug_selector("new-local-workspace-button")
                                .tooltip(
                                    Tooltip::new(
                                        "new-local-workspace-tooltip",
                                        "New Local Workspace",
                                    )
                                    .debug_selector("new-local-workspace-tooltip")
                                    .keyboard_equivalent(shortcuts),
                                )
                                .on_activate(move |_, _, cx| {
                                    let _ = local_sidebar.update(cx, |_, cx| {
                                        cx.emit(SidebarEvent::NewLocalWorkspace)
                                    });
                                }),
                            ),
                    ),
            )
            .child(scrollbar)
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
        .icon(|foreground, size| {
            Icon::new(IconName::SquarePlus, size, foreground).into_any_element()
        })
        .debug_selector("workspace-menu-row-new-tab"),
        MenuEntry::action("Rename Workspace", RowMenuCommand::Rename)
            .icon(|foreground, size| {
                Icon::new(IconName::Pencil, size, foreground).into_any_element()
            })
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
                Icon::new(IconName::PinOff, size * 0.85, foreground).into_any_element()
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
    entries.extend([
        MenuEntry::separator(),
        MenuEntry::action(
            "Close Workspace",
            RowMenuCommand::Workspace(WorkspaceMenuCommand::Close),
        )
        .destructive(true)
        .icon(|foreground, size| Icon::new(IconName::X, size, foreground).into_any_element())
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
