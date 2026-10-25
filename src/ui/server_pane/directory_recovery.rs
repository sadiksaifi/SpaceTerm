//! One cancellable server-backed picker, shared by both desktop Pane presentations.
use crate::{
    server_connection::ServerConnection,
    ui::directory_picker::{
        DirectoryPicker, DirectoryPickerEvent, DirectorySelection, ServerDirectorySource,
    },
};
use gpui::{App, Context, Entity, Render, Window, div, prelude::*};
use spaceterm_server::pane::directories::Target;
use std::{path::PathBuf, rc::Rc};

type RestoreFocus = dyn Fn(&mut Window, &mut App);
pub(crate) struct RecoveryActions {
    pub(crate) current: Rc<dyn Fn(&App) -> bool>,
    pub(crate) online: Rc<dyn Fn() -> bool>,
    pub(crate) confirm: Rc<dyn Fn(PathBuf)>,
    pub(crate) restore_focus: Rc<RestoreFocus>,
}
pub(crate) struct DirectoryRecovery {
    picker: Entity<DirectoryPicker>,
    open: bool,
    _watch: gpui::Task<()>,
}
impl DirectoryRecovery {
    pub(crate) fn new(
        server: ServerConnection,
        target: Target,
        actions: RecoveryActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let RecoveryActions {
            current,
            online,
            confirm,
            restore_focus,
        } = actions;
        let source = Rc::new(ServerDirectorySource::new(
            server,
            target.original.clone(),
            Rc::clone(&online),
        ));
        let picker = cx.new(|cx| DirectoryPicker::new(source, "Choose Pane Directory", window, cx));
        let valid = Rc::clone(&current);
        let restore = Rc::clone(&restore_focus);
        cx.subscribe_in(
            &picker,
            window,
            move |recovery, picker, event, window, cx| {
                match event {
                    DirectoryPickerEvent::Confirmed(DirectorySelection::Server(directory))
                        if recovery.open && valid(cx) && online() =>
                    {
                        recovery.open = false;
                        picker.update(cx, |picker, cx| picker.complete_activation(window, cx));
                        confirm(directory.clone());
                        restore(window, cx);
                    }
                    DirectoryPickerEvent::Confirmed(_) => {
                        picker.update(cx, |picker, cx| picker.activation_failed(window, cx));
                    }
                    DirectoryPickerEvent::Dismissed => {
                        recovery.open = false;
                        restore(window, cx);
                    }
                    _ => {}
                }
                cx.notify();
            },
        )
        .detach();
        // The picker owns cancellation of its reads and rejects late validation results.
        let watch = cx.spawn_in(window, async move |recovery, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                let Ok(keep) = recovery.update_in(cx, |recovery, window, cx| {
                    if recovery.open && !current(cx) {
                        recovery.open = false;
                        recovery
                            .picker
                            .update(cx, |picker, cx| picker.cancel(window, cx));
                        cx.notify();
                    }
                    recovery.open
                }) else {
                    break;
                };
                if !keep {
                    break;
                }
            }
        });
        picker.update(cx, |picker, cx| {
            picker.open(window, cx);
        });
        Self {
            picker,
            open: true,
            _watch: watch,
        }
    }
    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }
}
impl Render for DirectoryRecovery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.picker.clone())
    }
}
