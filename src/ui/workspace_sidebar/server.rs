//! Server-qualified sidebar interaction over the existing source-list components.

use std::collections::BTreeMap;

use super::server_group::{
    ServerConnection, ServerHeaderKey, ServerSection, ServerSectionId, ServerSectionModel,
};
use super::*;
use crate::ui::chrome_typography::ChromeTextStyleExt;
use crate::ui::pane_presentation::DisplayName;
use crate::ui::server_desktop_contracts::{ServerId, WorkspaceId as QualifiedWorkspaceId};
use spaceterm_ui::ContextMenuTarget;

#[derive(Clone)]
pub(crate) struct ServerWorkspaceRow {
    pub(crate) workspace: QualifiedWorkspaceId,
    pub(crate) name: DisplayName,
    pub(crate) pinned: bool,
    pub(crate) active: bool,
    pub(crate) paused: bool,
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
    Detach(QualifiedWorkspaceId),
    Kill(QualifiedWorkspaceId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServerRowKey {
    Server(ServerId),
    Workspace(QualifiedWorkspaceId),
}

#[derive(Default)]
pub(crate) struct ServerSidebar {
    sections: Vec<ServerSidebarSection>,
    expanded: BTreeMap<ServerId, bool>,
    cursor: Option<ServerRowKey>,
    pub(super) menu: Option<QualifiedWorkspaceId>,
}

#[derive(Clone, Copy)]
enum ServerWorkspaceCommand {
    Detach,
    Kill,
}

impl ServerSidebar {
    pub(super) fn needs_root_focus(&self) -> bool {
        !matches!(self.selected(), Some(ServerRowKey::Workspace(_)))
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
                        .map(|row| ServerRowKey::Workspace(row.workspace)),
                )
            })
            .collect()
    }
    fn selected(&self) -> Option<ServerRowKey> {
        self.cursor.or_else(|| {
            self.sections
                .iter()
                .flat_map(|section| &section.rows)
                .find(|row| row.active)
                .map(|row| ServerRowKey::Workspace(row.workspace))
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
                ServerRowKey::Workspace(workspace) => keys
                    .iter()
                    .position(|key| *key == ServerRowKey::Server(workspace.server()))
                    .unwrap_or(current),
                _ => current,
            },
            "enter" | "space" => {
                if let ServerRowKey::Workspace(workspace) = key {
                    cx.emit(SidebarEvent::Server(ServerSidebarEvent::Activate(
                        workspace,
                    )));
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
        if next != current
            && let ServerRowKey::Workspace(workspace) = keys[next]
        {
            cx.emit(SidebarEvent::Server(ServerSidebarEvent::Activate(
                workspace,
            )));
        }
        self.focus_visible = true;
        self.scroll_handle.scroll_to_item(next);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
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
                            .child(div().min_w_0().flex_1().truncate().child(name.clone()))
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
                        ContextMenu::new(
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
                                ServerWorkspaceCommand::Detach => {
                                    ServerSidebarEvent::Detach(workspace)
                                }
                                ServerWorkspaceCommand::Kill => ServerSidebarEvent::Kill(workspace),
                            };
                            let _ =
                                activate.update(cx, |_, cx| cx.emit(SidebarEvent::Server(event)));
                        })
                        .debug_selector(workspace_selector(workspace, "menu"))
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
