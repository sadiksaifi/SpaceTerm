//! The Workspace Window's content while this Client has no attached Workspace. It offers to attach
//! a Workspace from a server or to create one, and it creates no server state of its own.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the Workspace Window shows this view once it can hold zero attached Workspaces"
    )
)]

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, ElementId, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Render,
    SharedString, Window, div,
};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, IconName};

use super::pane_presentation::ContentUnavailable;

/// What the empty Workspace Window asks its owner to begin. Neither request changes any server
/// until the person completes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmptyWorkspaceAction {
    /// Opens the Workspace Switcher to attach a Workspace a server already has.
    AttachWorkspace,
    /// Begins New Workspace.
    NewWorkspace,
}

impl EmptyWorkspaceAction {
    const fn label(self) -> &'static str {
        match self {
            Self::AttachWorkspace => "Attach Workspace…",
            Self::NewWorkspace => "New Workspace",
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::AttachWorkspace => "empty-workspace-attach",
            Self::NewWorkspace => "empty-workspace-new",
        }
    }
}

type ActionHandler = Rc<dyn Fn(EmptyWorkspaceAction, &mut Window, &mut App)>;

/// The empty Workspace Window's message and actions. The primary action is the one most likely to
/// help: attaching when a server has a Workspace to attach, and creating one otherwise.
#[derive(IntoElement)]
pub(crate) struct EmptyWorkspaceContent {
    id: ElementId,
    attachable: bool,
    on_action: Option<ActionHandler>,
}

impl EmptyWorkspaceContent {
    pub(crate) fn new(id: impl Into<ElementId>, attachable: bool) -> Self {
        Self {
            id: id.into(),
            attachable,
            on_action: None,
        }
    }

    pub(crate) fn on_action(
        mut self,
        handler: impl Fn(EmptyWorkspaceAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }

    /// The actions in presentation order, with the primary action last.
    fn actions(attachable: bool) -> [EmptyWorkspaceAction; 2] {
        if attachable {
            [
                EmptyWorkspaceAction::NewWorkspace,
                EmptyWorkspaceAction::AttachWorkspace,
            ]
        } else {
            [
                EmptyWorkspaceAction::AttachWorkspace,
                EmptyWorkspaceAction::NewWorkspace,
            ]
        }
    }
}

impl RenderOnce for EmptyWorkspaceContent {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let detail: SharedString = if self.attachable {
            "Attach a Workspace from one of your servers, or create a new one.".into()
        } else {
            "Create a Workspace, or attach one when a server has one to share.".into()
        };
        let [secondary, primary] = Self::actions(self.attachable);
        let buttons = [
            (secondary, ButtonVariant::Secondary),
            (primary, ButtonVariant::Primary),
        ]
        .into_iter()
        .map(|(action, variant)| {
            let on_action = self.on_action.clone();
            Button::new(action.selector(), action.label())
                .variant(variant)
                .size(ButtonSize::Large)
                .tab_stop(true)
                .debug_selector(action.selector())
                .on_activate(move |_, window, cx| {
                    if let Some(on_action) = &on_action {
                        on_action(action, window, cx);
                    }
                })
                .into_any_element()
        })
        .collect();
        ContentUnavailable::new(
            self.id,
            "empty-workspace",
            IconName::AppWindow,
            "No Workspaces",
            detail,
        )
        .prominent()
        .actions(buttons)
    }
}

/// The view a Workspace Window hosts while it has no attached Workspace. It takes keyboard focus
/// with the window and moves it among its actions with Tab.
pub(crate) struct EmptyWorkspaceWindow {
    focus_handle: FocusHandle,
    attachable: bool,
}

impl EventEmitter<EmptyWorkspaceAction> for EmptyWorkspaceWindow {}

impl EmptyWorkspaceWindow {
    /// `attachable` reports whether a connected server has a Workspace this Client could attach.
    pub(crate) fn new(attachable: bool, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            attachable,
        }
    }

    pub(crate) fn set_attachable(&mut self, attachable: bool, cx: &mut Context<Self>) {
        if self.attachable != attachable {
            self.attachable = attachable;
            cx.notify();
        }
    }
}

impl Focusable for EmptyWorkspaceWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EmptyWorkspaceWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        div()
            .id("empty-workspace-window")
            .debug_selector(|| "empty-workspace-window".to_owned())
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(super::appearance::chrome(cx).spacing(24.0))
            .on_key_down(|event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let modifiers = keystroke.modifiers;
                if keystroke.key != "tab"
                    || modifiers.control
                    || modifiers.alt
                    || modifiers.platform
                    || modifiers.function
                {
                    return;
                }
                if modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                cx.stop_propagation();
            })
            .child(
                EmptyWorkspaceContent::new("empty-workspace-content", self.attachable).on_action(
                    move |action, _, cx| {
                        let _ = owner.update(cx, |_, cx| cx.emit(action));
                    },
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use spaceterm_ui::a11y_testing::{A11yTree, perform};

    use super::*;
    use crate::ui::pane_presentation::tests::press;

    fn window(
        attachable: bool,
        cx: &mut TestAppContext,
    ) -> (
        Entity<EmptyWorkspaceWindow>,
        Rc<RefCell<Vec<EmptyWorkspaceAction>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init).unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = EmptyWorkspaceWindow::new(attachable, cx);
            view.focus_handle.focus(window, cx);
            view
        });
        let requests = Rc::new(RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let requests = Rc::clone(&requests);
            cx.subscribe(&view, move |_, action: &EmptyWorkspaceAction, _| {
                requests.borrow_mut().push(*action);
            })
            .detach();
        });
        cx.run_until_parked();
        (view, requests, cx)
    }

    #[gpui::test]
    fn the_empty_window_offers_attach_and_create_without_a_default_action(cx: &mut TestAppContext) {
        let (view, requests, cx) = window(false, cx);
        let tree = A11yTree::read(cx);
        let content = tree.node("No Workspaces");
        assert_eq!(content["aria"]["role"], "Group");
        assert_eq!(
            tree.with_role("Button")
                .iter()
                .map(|button| button["aria"]["label"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            ["Attach Workspace…", "New Workspace"]
        );

        // Return and Space with the window focused begin nothing.
        press(cx, "enter space");
        assert!(requests.borrow().is_empty());

        press(cx, "tab");
        assert_eq!(
            A11yTree::read(cx)
                .focused()
                .map(|node| node["aria"]["label"].clone()),
            Some("Attach Workspace…".into())
        );
        press(cx, "enter tab space");
        assert_eq!(
            *requests.borrow(),
            [
                EmptyWorkspaceAction::AttachWorkspace,
                EmptyWorkspaceAction::NewWorkspace
            ]
        );
        press(cx, "shift-tab");
        assert_eq!(
            A11yTree::read(cx)
                .focused()
                .map(|node| node["aria"]["label"].clone()),
            Some("Attach Workspace…".into())
        );

        requests.borrow_mut().clear();
        let attach = cx.debug_bounds("empty-workspace-attach").unwrap().center();
        cx.simulate_click(attach, Modifiers::none());
        let tree = A11yTree::read(cx);
        perform(
            cx,
            tree.node("New Workspace"),
            gpui::accesskit::Action::Click,
        );
        cx.run_until_parked();
        assert_eq!(
            *requests.borrow(),
            [
                EmptyWorkspaceAction::AttachWorkspace,
                EmptyWorkspaceAction::NewWorkspace
            ]
        );

        // Attaching becomes the primary action once a server has a Workspace to attach.
        view.update(cx, |view, cx| view.set_attachable(true, cx));
        cx.run_until_parked();
        assert_eq!(
            A11yTree::read(cx)
                .with_role("Button")
                .iter()
                .map(|button| button["aria"]["label"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            ["New Workspace", "Attach Workspace…"]
        );
    }
}
