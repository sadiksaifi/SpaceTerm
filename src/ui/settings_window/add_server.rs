//! Add Server: the one journey that connects to an SSH host or joins with an invitation.
//!
//! Every entry point, such as the application menu, an invitation link the operating system
//! delivered, or Settings Search, starts here. Filling in a value connects and joins nothing; the
//! person confirms first.

use gpui::{Context, Window};

use super::{AddServerRequest, SettingsWindow};

impl SettingsWindow {
    /// Starts adding a server with `request`'s value filled in and keyboard focus on it.
    pub(super) fn begin_add_server(
        &mut self,
        request: AddServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = match request {
            AddServerRequest::Invitation(text) => {
                let Some(rows) = self.direct_routes.as_ref() else {
                    return;
                };
                if let Some(text) = text {
                    rows.invitation
                        .update(cx, |input, cx| input.set_value(text.as_str(), cx));
                }
                rows.invitation.clone()
            }
            AddServerRequest::Ssh(host) => {
                let Some(rows) = self.ssh_servers.as_ref() else {
                    return;
                };
                let field = rows.destination().clone();
                if let Some(host) = host {
                    field.update(cx, |input, cx| input.set_value(host, cx));
                }
                field
            }
        };
        let focus = field.read(cx).focus_handle();
        focus.focus(window, cx);
        cx.notify();
    }
}
