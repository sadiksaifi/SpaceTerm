//! Server-qualified sidebar interaction over the existing source-list components.

use std::collections::BTreeMap;

use super::server_group::{
    ServerConnection, ServerHeaderKey, ServerSection, ServerSectionId, ServerSectionModel,
};
use super::*;
use crate::repository_status::presentation::SidebarBadge;
use crate::ui::chrome_typography::ChromeTextStyleExt;
use crate::ui::pane_presentation::DisplayName;
use crate::ui::server_desktop_contracts::{ServerId, WorkspaceId as QualifiedWorkspaceId};
use spaceterm_core::identity::WorktreeId as QualifiedWorktreeId;
use spaceterm_ui::ContextMenuTarget;

#[derive(Clone)]
pub(crate) struct ServerWorkspaceRow {
    pub(crate) workspace: QualifiedWorkspaceId,
    pub(crate) name: DisplayName,
    pub(crate) pinned: bool,
    pub(crate) active: bool,
    pub(crate) paused: bool,
    pub(crate) repository: Option<SidebarBadge>,
    pub(crate) worktrees: Vec<ServerWorktreeRow>,
    pub(crate) creates_worktrees: bool,
}

#[derive(Clone)]
pub(crate) struct ServerWorktreeRow {
    pub(crate) worktree: QualifiedWorktreeId,
    pub(crate) name: SharedString,
    pub(crate) path: SharedString,
    pub(crate) repository: Option<SidebarBadge>,
    pub(crate) active: bool,
    pub(crate) has_tabs: bool,
    pub(crate) removable: bool,
}

#[derive(Clone)]
pub(crate) struct ServerSidebarSection {
    pub(crate) server: ServerId,
    pub(crate) name: DisplayName,
    pub(crate) connection: ServerConnection,
    pub(crate) rows: Vec<ServerWorkspaceRow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServerSidebarEvent {
    Activate(QualifiedWorkspaceId),
    Attach(ServerId),
    Settings(ServerId),
    StartingDirectory(QualifiedWorkspaceId),
    Detach(QualifiedWorkspaceId),
    Kill(QualifiedWorkspaceId),
    NewWorktree(QualifiedWorkspaceId),
    OpenWorktree(QualifiedWorkspaceId, QualifiedWorktreeId),
    RemoveWorktree(QualifiedWorktreeId),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ServerRowKey {
    Server(ServerId),
    Workspace(QualifiedWorkspaceId),
    Worktree(QualifiedWorkspaceId, QualifiedWorktreeId),
}

#[derive(Default)]
pub(crate) struct ServerSidebar {
    sections: Vec<ServerSidebarSection>,
    expanded: BTreeMap<ServerId, bool>,
    cursor: Option<ServerRowKey>,
    pub(super) menu: Option<QualifiedWorkspaceId>,
    /// Where each Workspace and Worktree row last painted, in list content coordinates. The
    /// list's scroll children are whole server sections, so keyboard reveal needs row spans.
    row_spans: std::collections::HashMap<ServerRowKey, (Pixels, Pixels)>,
}

#[derive(Clone, Copy)]
enum ServerWorkspaceCommand {
    StartingDirectory,
    Settings,
    Detach,
    Kill,
    NewWorktree,
}

impl ServerSidebar {
    pub(super) fn needs_root_focus(&self) -> bool {
        !matches!(
            self.selected(),
            Some(ServerRowKey::Workspace(_) | ServerRowKey::Worktree(_, _))
        )
    }
    fn keys(&self) -> Vec<ServerRowKey> {
        self.sections
            .iter()
            .flat_map(|section| {
                std::iter::once(ServerRowKey::Server(section.server)).chain(
                    section
                        .rows
                        .iter()
                        .filter(|_| self.expanded.get(&section.server).copied().unwrap_or(true))
                        .flat_map(|row| {
                            std::iter::once(ServerRowKey::Workspace(row.workspace)).chain(
                                row.worktrees.iter().map(|worktree| {
                                    ServerRowKey::Worktree(row.workspace, worktree.worktree)
                                }),
                            )
                        }),
                )
            })
            .collect()
    }
    /// The keyboard cursor, or else the active Worktree of the active Workspace, or else the
    /// active Workspace, as the local sidebar selects.
    fn selected(&self) -> Option<ServerRowKey> {
        self.cursor.or_else(|| {
            let row = self
                .sections
                .iter()
                .flat_map(|section| &section.rows)
                .find(|row| row.active)?;
            Some(
                row.worktrees
                    .iter()
                    .find(|worktree| worktree.active)
                    .map_or(ServerRowKey::Workspace(row.workspace), |worktree| {
                        ServerRowKey::Worktree(row.workspace, worktree.worktree)
                    }),
            )
        })
    }
}

impl WorkspaceSidebar {
    pub(crate) fn set_server_sections(
        &mut self,
        sections: Vec<ServerSidebarSection>,
        cx: &mut Context<Self>,
    ) {
        let server = self.server.get_or_insert_with(ServerSidebar::default);
        server.sections = sections;
        let keys = server.keys();
        server.cursor = server.cursor.filter(|cursor| keys.contains(cursor));
        server.menu = server
            .menu
            .filter(|workspace| keys.contains(&ServerRowKey::Workspace(*workspace)));
        self.rows.clear();
        cx.notify();
    }

    pub(crate) fn focus_server_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_visible = true;
        self.focus.focus(window, cx);
        cx.emit(SidebarEvent::FocusChanged);
        cx.notify();
    }

    pub(crate) fn server_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus.is_focused(window) || event.keystroke.modifiers.modified() {
            return;
        }
        let Some(server) = &mut self.server else {
            return;
        };
        let keys = server.keys();
        if keys.is_empty() {
            return;
        }
        let selected = server.selected();
        let current = selected
            .and_then(|selected| keys.iter().position(|key| *key == selected))
            .unwrap_or(0);
        let key = keys[current];
        if let ServerRowKey::Server(id) = key
            && let Some(section) = server.sections.iter().find(|section| section.server == id)
        {
            let model = ServerSectionModel {
                id: section_id(id),
                name: section.name.clone(),
                connection: section.connection,
                expanded: server.expanded.get(&id).copied().unwrap_or(true),
            };
            match model.header_key(&event.keystroke) {
                Some(ServerHeaderKey::SetExpanded(expanded)) => {
                    server.expanded.insert(id, expanded);
                    server.cursor = Some(key);
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                Some(ServerHeaderKey::EnterSection) => {
                    if let Some(ServerRowKey::Workspace(workspace)) = keys.get(current + 1) {
                        server.cursor = Some(ServerRowKey::Workspace(*workspace));
                        cx.emit(SidebarEvent::Server(ServerSidebarEvent::Activate(
                            *workspace,
                        )));
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                None => {}
            }
        }
        let next = match event.keystroke.key.as_str() {
            "up" => current.saturating_sub(1),
            "down" => (current + 1).min(keys.len() - 1),
            "home" => 0,
            "end" => keys.len() - 1,
            "left" => match key {
                ServerRowKey::Worktree(workspace, _) => keys
                    .iter()
                    .position(|key| *key == ServerRowKey::Workspace(workspace))
                    .unwrap_or(current),
                ServerRowKey::Workspace(workspace) => keys
                    .iter()
                    .position(|key| *key == ServerRowKey::Server(workspace.server()))
                    .unwrap_or(current),
                _ => current,
            },
            "enter" | "space" => {
                match key {
                    ServerRowKey::Workspace(workspace) => cx.emit(SidebarEvent::Server(
                        ServerSidebarEvent::Activate(workspace),
                    )),
                    ServerRowKey::Worktree(workspace, worktree) => cx.emit(SidebarEvent::Server(
                        ServerSidebarEvent::OpenWorktree(workspace, worktree),
                    )),
                    _ => {}
                }
                current
            }
            "escape" => {
                cx.emit(SidebarEvent::FocusPane);
                current
            }
            _ => return,
        };
        server.cursor = Some(keys[next]);
        if next != current {
            match keys[next] {
                ServerRowKey::Workspace(workspace) => cx.emit(SidebarEvent::Server(
                    ServerSidebarEvent::Activate(workspace),
                )),
                ServerRowKey::Worktree(workspace, worktree)
                    if server
                        .sections
                        .iter()
                        .flat_map(|section| &section.rows)
                        .flat_map(|row| &row.worktrees)
                        .any(|row| row.worktree == worktree && row.has_tabs) =>
                {
                    cx.emit(SidebarEvent::Server(ServerSidebarEvent::OpenWorktree(
                        workspace, worktree,
                    )))
                }
                _ => {}
            }
        }
        self.focus_visible = true;
        self.reveal_server_row(keys[next]);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    /// Scrolls the least distance that shows a row whole, or the top of a row taller than the
    /// list. Before a row has painted, the list shows its server section instead.
    fn reveal_server_row(&self, key: ServerRowKey) {
        let Some(server) = &self.server else {
            return;
        };
        let server_id = match key {
            ServerRowKey::Server(id) => id,
            ServerRowKey::Workspace(workspace) | ServerRowKey::Worktree(workspace, _) => {
                workspace.server()
            }
        };
        let Some(section) = server
            .sections
            .iter()
            .position(|section| section.server == server_id)
        else {
            return;
        };
        let span = match key {
            // A server header spans from its section's top to its first row.
            ServerRowKey::Server(_) => self.scroll_handle.bounds_for_item(section).map(|bounds| {
                let first = server.sections[section]
                    .rows
                    .first()
                    .filter(|_| server.expanded.get(&server_id).copied().unwrap_or(true))
                    .and_then(|row| {
                        server
                            .row_spans
                            .get(&ServerRowKey::Workspace(row.workspace))
                    })
                    .map_or(bounds.bottom(), |(top, _)| *top);
                (bounds.top(), first.min(bounds.bottom()))
            }),
            _ => server.row_spans.get(&key).copied(),
        };
        let Some((top, bottom)) = span else {
            self.scroll_handle.scroll_to_item(section);
            return;
        };
        let viewport = self.scroll_handle.bounds();
        let mut offset = self.scroll_handle.offset();
        if top + offset.y < viewport.top() || bottom - top > viewport.size.height {
            offset.y = viewport.top() - top;
        } else if bottom + offset.y > viewport.bottom() {
            offset.y = viewport.bottom() - bottom;
        } else {
            return;
        }
        self.scroll_handle.set_offset(offset);
    }

    /// Records where one Workspace group painted its rows. `keys` names each child in order.
    fn record_server_row_spans(
        &mut self,
        keys: &[ServerRowKey],
        children: &[gpui::Bounds<Pixels>],
    ) {
        // Painted bounds include the list's scroll offset; spans are kept without it.
        let offset = self.scroll_handle.offset().y;
        let Some(server) = &mut self.server else {
            return;
        };
        for (key, bounds) in keys.iter().zip(children) {
            server
                .row_spans
                .insert(*key, (bounds.top() - offset, bounds.bottom() - offset));
        }
    }

    pub(crate) fn render_server_sections(
        &self,
        owner: WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> Vec<AnyElement> {
        let Some(server) = &self.server else {
            return Vec::new();
        };
        let appearance = crate::ui::appearance::chrome(cx);
        server
            .sections
            .iter()
            .enumerate()
            .map(|(position, section)| {
                let id = section.server;
                let rows = section
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(row_position, row)| {
                        let workspace = row.workspace;
                        let name = row
                            .name
                            .get()
                            .cloned()
                            .unwrap_or_else(|| "Unnamed Workspace".into());
                        let selected =
                            server.selected() == Some(ServerRowKey::Workspace(workspace));
                        let (chip, colors) = super::view::row_paint(
                            selected,
                            self.focus_visible && self.focus.is_focused(window),
                            0.0,
                            appearance,
                            cx,
                        );
                        let (leading, trailing) = super::view::row_padding(appearance, cx);
                        let click = owner.clone();
                        let content = div()
                            .id(workspace_selector(workspace, "row"))
                            .on_click(move |_, _, cx| {
                                let _ = click.update(cx, |sidebar, cx| {
                                    if let Some(server) = &mut sidebar.server {
                                        server.cursor = Some(ServerRowKey::Workspace(workspace));
                                    }
                                    cx.emit(SidebarEvent::Server(ServerSidebarEvent::Activate(
                                        workspace,
                                    )));
                                    cx.notify();
                                });
                            })
                            .debug_selector(move || workspace_selector(workspace, "row"))
                            .w_full()
                            .relative()
                            .min_h(appearance.spacing(SIDEBAR_ROW_HEIGHT))
                            .pl(leading + appearance.spacing(14.0))
                            .pr(trailing)
                            .flex()
                            .items_center()
                            .chrome_text(
                                appearance
                                    .typography
                                    .style(crate::ui::chrome_typography::TextRole::Navigation),
                            )
                            .text_color(crate::ui::appearance::gpui_color(colors.row_foreground))
                            .child(chip.render(workspace_selector(workspace, "chip"), 0.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(div().truncate().child(name.clone()))
                                    .child(text::detail(
                                        SharedString::default(),
                                        row.repository.clone().map(|badge| text::RowRepository {
                                            badge,
                                            pull_request_paint: appearance.colors.pull_request(
                                                crate::ui::workspace_frame::base_surface(
                                                    &appearance.colors,
                                                ),
                                            ),
                                        }),
                                        false,
                                        None,
                                        None,
                                        workspace.local(),
                                        appearance.clone(),
                                    )),
                            )
                            .when(row.paused, |row| row.child("Paused"))
                            .into_any_element();
                        let press = owner.clone();
                        let target =
                            ContextMenuTarget::new(gpui::accesskit::Role::TreeItem, name.clone())
                                .level(2)
                                .set_position(row_position + 1, section.rows.len())
                                .selected(selected)
                                .description(if row.paused {
                                    "Paused"
                                } else if row.pinned {
                                    "Pinned Workspace"
                                } else {
                                    "Workspace"
                                })
                                .on_press(move |_, cx| {
                                    let _ = press.update(cx, |_, cx| {
                                        cx.emit(SidebarEvent::Server(ServerSidebarEvent::Activate(
                                            workspace,
                                        )))
                                    });
                                });
                        let activate = owner.clone();
                        let open = owner.clone();
                        let lifecycle = owner.clone();
                        let entries = vec![
                            MenuEntry::action(
                                "Starting Directory…",
                                ServerWorkspaceCommand::StartingDirectory,
                            )
                            .disabled(section.connection != ServerConnection::Connected)
                            .debug_selector("server-workspace-menu-starting-directory"),
                            MenuEntry::action("Server Settings…", ServerWorkspaceCommand::Settings)
                                .debug_selector("server-workspace-menu-settings"),
                            MenuEntry::separator(),
                            MenuEntry::action("New Worktree…", ServerWorkspaceCommand::NewWorktree)
                                .disabled(
                                    !row.creates_worktrees
                                        || section.connection != ServerConnection::Connected,
                                )
                                .debug_selector("server-workspace-menu-new-worktree"),
                            MenuEntry::separator(),
                            MenuEntry::action("Detach Workspace", ServerWorkspaceCommand::Detach)
                                .icon(|foreground, size| {
                                    Icon::new(IconName::Unlink, size, foreground).into_any_element()
                                })
                                .debug_selector("workspace-menu-row-detach"),
                            MenuEntry::separator(),
                            MenuEntry::action("Kill Workspace", ServerWorkspaceCommand::Kill)
                                .destructive(true)
                                .disabled(section.connection != ServerConnection::Connected)
                                .icon(|foreground, size| {
                                    Icon::new(IconName::Trash2, size, foreground).into_any_element()
                                })
                                .debug_selector("workspace-menu-row-close"),
                        ];
                        let menu = ContextMenu::new(
                            workspace_selector(workspace, "menu"),
                            format!("Workspace actions for {name}"),
                            content,
                            entries,
                        )
                        .size(MenuSize::Wide)
                        .target(target)
                        .when(selected, |menu| menu.keyboard_trigger(&self.focus))
                        .on_open_request(move |_, _, cx| {
                            open.update(cx, |sidebar, cx| {
                                if let Some(server) = &mut sidebar.server {
                                    server.menu = Some(workspace);
                                }
                                cx.notify();
                                true
                            })
                            .unwrap_or(false)
                        })
                        .on_lifecycle(move |event, cx| {
                            if matches!(event, MenuLifecycleEvent::Closed(_)) {
                                let _ = lifecycle.update(cx, |sidebar, cx| {
                                    if let Some(server) = &mut sidebar.server {
                                        server.menu = None;
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .on_activate(move |activation, _, cx| {
                            let event = match activation.action() {
                                ServerWorkspaceCommand::StartingDirectory => {
                                    ServerSidebarEvent::StartingDirectory(workspace)
                                }
                                ServerWorkspaceCommand::Settings => {
                                    ServerSidebarEvent::Settings(workspace.server())
                                }
                                ServerWorkspaceCommand::Detach => {
                                    ServerSidebarEvent::Detach(workspace)
                                }
                                ServerWorkspaceCommand::Kill => ServerSidebarEvent::Kill(workspace),
                                ServerWorkspaceCommand::NewWorktree => {
                                    ServerSidebarEvent::NewWorktree(workspace)
                                }
                            };
                            let _ =
                                activate.update(cx, |_, cx| cx.emit(SidebarEvent::Server(event)));
                        })
                        .debug_selector(workspace_selector(workspace, "menu"))
                        .into_any_element();
                        let keys = std::iter::once(ServerRowKey::Workspace(workspace))
                            .chain(row.worktrees.iter().map(|worktree| {
                                ServerRowKey::Worktree(workspace, worktree.worktree)
                            }))
                            .collect::<Vec<_>>();
                        let measure = owner.clone();
                        div()
                            .on_children_prepainted(move |bounds, _, cx| {
                                let _ = measure.update(cx, |sidebar, _| {
                                    sidebar.record_server_row_spans(&keys, &bounds);
                                });
                            })
                            .w_full()
                            .flex()
                            .flex_col()
                            .child(menu)
                            .children(row.worktrees.iter().map(|worktree| {
                                render_server_worktree(
                                    row.workspace,
                                    worktree,
                                    server.selected(),
                                    section.connection,
                                    owner.clone(),
                                    appearance,
                                    (
                                        &self.focus,
                                        self.focus_visible && self.focus.is_focused(window),
                                    ),
                                    cx,
                                )
                            }))
                            .into_any_element()
                    })
                    .collect();
                let toggle = owner.clone();
                let attach = owner.clone();
                ServerSection::new(ServerSectionModel {
                    id: section_id(id),
                    name: section.name.clone(),
                    connection: section.connection,
                    expanded: server.expanded.get(&id).copied().unwrap_or(true),
                })
                .selected(server.selected() == Some(ServerRowKey::Server(id)))
                .emphasized(self.focus_visible && self.focus.is_focused(window))
                .set_position(position + 1, server.sections.len())
                .workspaces(rows)
                .on_toggle(move |expanded, _, cx| {
                    let _ = toggle.update(cx, |sidebar, cx| {
                        if let Some(server) = &mut sidebar.server {
                            server.expanded.insert(id, expanded);
                            server.cursor = Some(ServerRowKey::Server(id));
                        }
                        cx.notify();
                    });
                })
                .on_attach(move |_, cx| {
                    let _ = attach.update(cx, |_, cx| {
                        cx.emit(SidebarEvent::Server(ServerSidebarEvent::Attach(id)))
                    });
                })
                .into_any_element()
            })
            .collect()
    }
}

pub(crate) fn section_id(server: ServerId) -> ServerSectionId {
    ServerSectionId::new(u128::from_be_bytes(server.bytes()))
}
pub(crate) fn workspace_selector(workspace: QualifiedWorkspaceId, part: &str) -> String {
    format!(
        "workspace-{:032x}-{}-{part}",
        u128::from_be_bytes(workspace.server().bytes()),
        workspace.local()
    )
}

#[derive(Clone, Copy)]
enum WorktreeCommand {
    Open,
    Remove,
}

#[expect(
    clippy::too_many_arguments,
    reason = "source-list rendering receives the owner, focus and resolved appearance separately"
)]
fn render_server_worktree(
    workspace: QualifiedWorkspaceId,
    row: &ServerWorktreeRow,
    selected: Option<ServerRowKey>,
    connection: ServerConnection,
    owner: WeakEntity<WorkspaceSidebar>,
    appearance: &crate::ui::appearance::ChromeAppearance,
    (focus, emphasized): (&FocusHandle, bool),
    cx: &App,
) -> AnyElement {
    let worktree = row.worktree;
    // Like Workspace rows, the selected row takes the accent while the sidebar has keyboard
    // focus. The active Worktree is the selection only until the cursor moves.
    let selected = selected == Some(ServerRowKey::Worktree(workspace, worktree));
    let (chip, colors) = super::view::row_paint(selected, emphasized, 0.0, appearance, cx);
    let activate = owner.clone();
    let id = format!(
        "{}-worktree-{}",
        workspace_selector(workspace, "group"),
        worktree.local()
    );
    let selector = id.clone();
    let content = div()
        .id(id.clone())
        .debug_selector(move || selector)
        .relative()
        .w_full()
        .pl(appearance.spacing(40.0))
        .pr(appearance.spacing(8.0))
        .py(appearance.spacing(4.0))
        .chrome_text(
            appearance
                .typography
                .style(crate::ui::chrome_typography::TextRole::Navigation),
        )
        .text_color(crate::ui::appearance::gpui_color(colors.row_foreground))
        .child(chip.render(format!("{id}-chip"), 0.0))
        .child(div().truncate().child(row.name.clone()))
        .child(text::detail(
            row.path.clone(),
            row.repository.clone().map(|badge| text::RowRepository {
                badge,
                pull_request_paint: appearance
                    .colors
                    .pull_request(crate::ui::workspace_frame::base_surface(&appearance.colors)),
            }),
            false,
            None,
            None,
            worktree.local(),
            appearance.clone(),
        ));
    let click = owner.clone();
    let content = content.on_click(move |_, _, cx| {
        let _ = click.update(cx, |sidebar, cx| {
            if let Some(server) = &mut sidebar.server {
                server.cursor = Some(ServerRowKey::Worktree(workspace, worktree));
            }
            cx.emit(SidebarEvent::Server(ServerSidebarEvent::OpenWorktree(
                workspace, worktree,
            )));
        });
    });
    let press = owner.clone();
    let target = ContextMenuTarget::new(gpui::accesskit::Role::TreeItem, row.name.clone())
        .level(3)
        .selected(selected)
        .description(row.path.clone())
        .on_press(move |_, cx| {
            let _ = press.update(cx, |_, cx| {
                cx.emit(SidebarEvent::Server(ServerSidebarEvent::OpenWorktree(
                    workspace, worktree,
                )))
            });
        });
    ContextMenu::new(
        format!("{id}-menu"),
        "Worktree actions",
        content,
        vec![
            MenuEntry::action("Open Worktree", WorktreeCommand::Open)
                .disabled(!row.has_tabs && connection != ServerConnection::Connected),
            MenuEntry::separator(),
            MenuEntry::action("Remove Worktree…", WorktreeCommand::Remove)
                .destructive(true)
                .disabled(!row.removable || connection != ServerConnection::Connected),
        ],
    )
    .target(target)
    .size(MenuSize::Wide)
    .when(selected, |menu| menu.keyboard_trigger(focus))
    .on_activate(move |activation, _, cx| {
        let event = match activation.action() {
            WorktreeCommand::Open => ServerSidebarEvent::OpenWorktree(workspace, worktree),
            WorktreeCommand::Remove => ServerSidebarEvent::RemoveWorktree(worktree),
        };
        let _ = activate.update(cx, |_, cx| cx.emit(SidebarEvent::Server(event)));
    })
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::server_view_testing::{A, server, worktree, ws};

    fn worktree_row(local: u64, active: bool) -> ServerWorktreeRow {
        ServerWorktreeRow {
            worktree: worktree(A, local),
            name: "feature".into(),
            path: "/server/feature".into(),
            repository: None,
            active,
            has_tabs: true,
            removable: true,
        }
    }

    fn sidebar(active_worktree: bool) -> ServerSidebar {
        ServerSidebar {
            sections: vec![ServerSidebarSection {
                server: server(A),
                name: DisplayName::reported("Local"),
                connection: ServerConnection::Connected,
                rows: vec![ServerWorkspaceRow {
                    workspace: ws(A, 1),
                    name: DisplayName::reported("work"),
                    pinned: false,
                    active: true,
                    paused: false,
                    repository: None,
                    worktrees: vec![worktree_row(1, false), worktree_row(2, active_worktree)],
                    creates_worktrees: true,
                }],
            }],
            ..ServerSidebar::default()
        }
    }

    #[test]
    fn server_sidebar_selects_one_worktree_row_at_a_time() {
        assert_eq!(
            sidebar(false).selected(),
            Some(ServerRowKey::Workspace(ws(A, 1)))
        );
        let mut sidebar = sidebar(true);
        assert_eq!(
            sidebar.selected(),
            Some(ServerRowKey::Worktree(ws(A, 1), worktree(A, 2))),
            "the active Worktree is the selection until the cursor moves"
        );
        sidebar.cursor = Some(ServerRowKey::Worktree(ws(A, 1), worktree(A, 1)));
        assert_eq!(
            sidebar.selected(),
            Some(ServerRowKey::Worktree(ws(A, 1), worktree(A, 1))),
            "the keyboard cursor, not the active Worktree, is the one selected row"
        );
    }
}
