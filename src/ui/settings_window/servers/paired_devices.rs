//! Bounded authorized-device pages and exact-pairing revocation in Servers Settings.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, accesskit, div};
use spaceterm_client::management_cli::confirmation::{ManagementAction, ManagementAssessment};
use spaceterm_core::{identity::PairingId, pairing::PairingTarget};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant};

use super::model::{PairedDevice, PairedDevices, ServerRequest, ServerSummary};
use super::{BoundServerRequest, Confirmation, SettingsWindow};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::pane_presentation::DisplayName;

#[derive(Default)]
pub(super) struct DeviceInventoryView {
    review: Option<DeviceReview>,
    notice: Option<SharedString>,
    refresh_focus: Option<gpui::FocusHandle>,
    action_focus: HashMap<PairingId, gpui::FocusHandle>,
}

impl DeviceInventoryView {
    pub(super) fn selection_changed(&mut self) {
        self.review = None;
        self.notice = None;
    }
}

struct DeviceReview {
    request: BoundServerRequest,
    page: PairedDevices,
    device: PairedDevice,
    previous_assessment: Option<ManagementAssessment>,
    focus: gpui::FocusHandle,
}

impl SettingsWindow {
    pub(super) fn render_paired_devices(
        &mut self,
        server: &ServerSummary,
        appearance: &ChromeAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.advance_device_review(server, window, cx);
        let page = server
            .paired_devices
            .as_ref()
            .filter(|page| page.valid_for(&server.pairing));
        self.servers.devices.action_focus.retain(|pairing, _| {
            page.is_some_and(|page| {
                page.devices
                    .iter()
                    .any(|device| device.target.parts().pairing == *pairing)
            })
        });
        let owner = cx.weak_entity();
        let key = server.key.clone();
        let connected = server.connected();
        let server_name = display_name(&server.name, "Server");
        let refresh_focus = self
            .servers
            .devices
            .refresh_focus
            .get_or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        // GPUI retains button state by element identity. Scope the entire inventory to the
        // canonical server so equal local Pairing IDs cannot reuse another server's focus.
        let server_scope = server
            .pairing
            .parts()
            .server
            .bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut content = div()
            .id(SharedString::from(format!(
                "settings-paired-devices-{server_scope}"
            )))
            .debug_selector(|| "settings-paired-devices".to_owned())
            .role(accesskit::Role::Group)
            .aria_label(format!("Paired devices on {server_name}"))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(appearance.spacing(8.0));
        let status: SharedString = match page {
            Some(page) if !connected => format!("Last read: {} of {} paired devices. Connect to refresh this inventory.", page.devices.len(), page.total).into(),
            Some(page) if page.total == 0 => "No paired devices are listed.".into(),
            Some(page) => format!("Showing {} of {} paired devices", page.devices.len(), page.total).into(),
            None if connected => "Read the server's paired devices to see who can use it.".into(),
            None => "Connect to this server to read its paired devices. You can still unpair this device below.".into(),
        };
        content = content.child(device_status(
            "settings-paired-devices-status",
            status,
            appearance,
        ));
        if let Some(page) = page {
            let identity = page
                .this_device
                .bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            content = content.child(device_text(
                "settings-paired-devices-this-identity",
                format!("This device ID: {identity}").into(),
                appearance,
            ));
            for device in &page.devices {
                let this = device.target.parts().device == page.this_device;
                let device_name = display_name(&device.name, "Unnamed device");
                let name: SharedString = if this {
                    format!("{device_name} (this device)").into()
                } else {
                    device_name.clone()
                };
                let selector = device_selector(&device.target);
                let reviewing = self
                    .servers
                    .devices
                    .review
                    .as_ref()
                    .is_some_and(|review| review.device.target == device.target);
                let focus = self
                    .servers
                    .devices
                    .action_focus
                    .entry(device.target.parts().pairing)
                    .or_insert_with(|| cx.focus_handle().tab_stop(true))
                    .clone();
                let captured = device.clone();
                let captured_request = BoundServerRequest {
                    target: super::model::ServerRequestTarget::Server {
                        pairing: server.pairing.clone(),
                        runtime_epoch: server.runtime_epoch,
                        route: server.route.clone(),
                        inventory_version: Some(page.version),
                        assessment: None,
                        management_comparison: None,
                        remote_policy: None,
                    },
                    request: ServerRequest::AssessUnpairDevice {
                        server: server.key.clone(),
                        target: device.target.clone(),
                        version: page.version,
                    },
                };
                let action_owner = cx.weak_entity();
                let action_label: SharedString = if this {
                    "Unpair this device…".into()
                } else {
                    "Unpair Device…".into()
                };
                content = content.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .role(accesskit::Role::Group)
                        .aria_label(name.clone())
                        .aria_description(if connected {
                            format!("Authorized to use {server_name}")
                        } else {
                            format!("Listed in the last read inventory for {server_name}")
                        })
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(appearance.spacing(4.0))
                        .child(device_text(&format!("{selector}-name"), name, appearance))
                        .child(
                            div().flex().min_w_0().child(
                                Button::new(SharedString::from(selector.clone()), action_label)
                                    .variant(ButtonVariant::Outline)
                                    .size(ButtonSize::Regular)
                                    .tab_stop(true)
                                    .disabled(!connected || reviewing)
                                    .focus_handle(focus.clone())
                                    .accessibility_description(format!("Review removal of {device_name} from {server_name}. Terminal Sessions keep running."))
                                    .debug_selector(selector)
                                    .on_activate(move |_, _, cx| {
                                        let _ = action_owner.update(cx, |settings, cx| {
                                            settings.review_device(&captured_request, &captured, focus.clone(), cx);
                                        });
                                    }),
                            ),
                        ),
                );
            }
        }
        let mut controls = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(appearance.spacing(8.0));
        controls = controls.child(
            Button::new(
                "settings-paired-devices-refresh",
                if page.is_some() {
                    "Refresh devices"
                } else {
                    "Read devices"
                },
            )
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Regular)
            .tab_stop(true)
            .disabled(!connected)
            .focus_handle(refresh_focus.clone())
            .debug_selector("settings-paired-devices-refresh")
            .on_activate(move |_, _, cx| {
                let _ = owner.update(cx, |settings, cx| {
                    settings.servers.devices.review = None;
                    settings.servers.devices.notice = None;
                    settings.send_server_request(
                        ServerRequest::ReadPairedDevices {
                            server: key.clone(),
                            after: None,
                            version: None,
                        },
                        cx,
                    );
                    cx.notify();
                });
            }),
        );
        if let Some(page) = page
            && let Some(after) = page.next
        {
            let key = server.key.clone();
            let version = page.version;
            let owner = cx.weak_entity();
            let after_focus = refresh_focus.clone();
            controls = controls.child(
                Button::new("settings-paired-devices-next", "Next page")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .disabled(!connected)
                    .accessibility_description(
                        "Read the next page from this exact inventory revision.",
                    )
                    .debug_selector("settings-paired-devices-next")
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            after_focus.focus(window, cx);
                            settings.servers.devices.review = None;
                            settings.send_server_request(
                                ServerRequest::ReadPairedDevices {
                                    server: key.clone(),
                                    after: Some(after),
                                    version: Some(version),
                                },
                                cx,
                            );
                            cx.notify();
                        });
                    }),
            );
        }
        if self.servers.devices.review.is_some() {
            content = content.child(device_status(
                "settings-paired-devices-review-status",
                "Reading the selected device's current pairing before confirmation…".into(),
                appearance,
            ));
            let owner = cx.weak_entity();
            controls = controls.child(
                Button::new("settings-paired-devices-cancel-review", "Cancel review")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Regular)
                    .tab_stop(true)
                    .debug_selector("settings-paired-devices-cancel-review")
                    .on_activate(move |_, window, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            refresh_focus.focus(window, cx);
                            settings.servers.devices.review = None;
                            cx.notify();
                        });
                    }),
            );
        }
        if let Some(notice) = self.servers.devices.notice.clone() {
            content = content.child(device_status(
                "settings-paired-devices-notice",
                notice,
                appearance,
            ));
        }
        content.child(controls).into_any_element()
    }

    fn review_device(
        &mut self,
        captured: &BoundServerRequest,
        device: &PairedDevice,
        focus: gpui::FocusHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = captured.request.server() else {
            return;
        };
        let Some(server) = self.servers.find(key) else {
            return;
        };
        let Some(page) = server
            .paired_devices
            .as_ref()
            .filter(|page| page.valid_for(&server.pairing))
        else {
            return;
        };
        if !server.connected() || !captured.still_matches(&self.servers.snapshot) {
            return;
        }
        let bound = captured.clone();
        self.servers.devices.review = Some(DeviceReview {
            request: bound.clone(),
            page: page.clone(),
            device: device.clone(),
            previous_assessment: server.unpair_assessment.clone(),
            focus,
        });
        self.servers.devices.notice = None;
        self.send_bound_server_request(bound, cx);
        cx.notify();
    }

    fn advance_device_review(
        &mut self,
        server: &ServerSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.servers.devices.review.as_ref() else {
            return;
        };
        if review.request.request.server() != Some(&server.key)
            || !server.connected()
            || !review.request.still_matches(&self.servers.snapshot)
            || server.paired_devices.as_ref() != Some(&review.page)
        {
            self.servers.devices.review = None;
            self.servers.devices.notice = Some(
                "The server or device inventory changed. Review the device again before unpairing."
                    .into(),
            );
            return;
        }
        let Some(assessment) = server.unpair_assessment.as_ref().filter(|assessment| {
            Some(*assessment) != review.previous_assessment.as_ref()
                && matches!(assessment.action(), ManagementAction::Unpair { target } if target == &review.device.target)
        }).cloned() else { return; };
        let Some(review) = self.servers.devices.review.take() else {
            return;
        };
        let captured_server = server.clone();
        cx.defer_in(window, move |settings, window, cx| {
            if !review.request.still_matches(&settings.servers.snapshot)
                || !settings
                    .servers
                    .find(&captured_server.key)
                    .is_some_and(|server| {
                        server.connected()
                            && server.paired_devices.as_ref() == Some(&review.page)
                            && server.unpair_assessment.as_ref() == Some(&assessment)
                    })
            {
                return;
            }
            let request = ServerRequest::UnpairDevice {
                server: captured_server.key.clone(),
                target: review.device.target.clone(),
                assessment: Box::new(assessment.clone()),
            };
            let confirmation = device_confirmation(&captured_server, &review.page, &review.device);
            review.focus.focus(window, cx);
            settings.present_confirmation(
                confirmation,
                request,
                move |server| {
                    server.connected()
                        && server.paired_devices.as_ref() == Some(&review.page)
                        && server.unpair_assessment.as_ref() == Some(&assessment)
                },
                window,
                cx,
            );
        });
    }
}

fn display_name(name: &str, fallback: &'static str) -> SharedString {
    DisplayName::reported(name)
        .get()
        .cloned()
        .unwrap_or_else(|| SharedString::new_static(fallback))
}

fn device_text(id: &str, text: SharedString, appearance: &ChromeAppearance) -> AnyElement {
    div()
        .min_w_0()
        .whitespace_normal()
        .chrome_text(appearance.typography.style(TextRole::Secondary))
        .text_color(gpui_color(appearance.colors.text_secondary))
        .child(gpui::Text::new(
            SharedString::from(id.to_owned()).into(),
            text,
        ))
        .into_any_element()
}

fn device_status(id: &str, text: SharedString, appearance: &ChromeAppearance) -> AnyElement {
    let announcement = text.clone();
    div()
        .id(SharedString::from(id.to_owned()))
        .role(accesskit::Role::Group)
        .aria_label("Device inventory status")
        .a11y_synthetic_children(move |builder| {
            let mut status = accesskit::Node::new(accesskit::Role::Status);
            status.set_label(announcement.as_ref());
            status.set_live(accesskit::Live::Polite);
            builder.push_child(builder.synthetic_node_id("device-status"), status);
        })
        .child(device_text(id, text, appearance))
        .into_any_element()
}

fn device_selector(target: &PairingTarget) -> String {
    format!(
        "settings-paired-device-{}-unpair",
        target.parts().pairing.local()
    )
}

fn device_confirmation(
    server: &ServerSummary,
    page: &PairedDevices,
    device: &PairedDevice,
) -> Confirmation {
    let this = device.target.parts().device == page.this_device;
    let name = display_name(&server.name, "Server");
    let device_name = display_name(&device.name, "Unnamed device");
    let subject = if this {
        "this device"
    } else {
        device_name.as_ref()
    };
    let message = if this {
        format!(
            "This device stops using {name} now. {name}'s Workspaces leave this device's windows, and its saved connection is removed."
        )
    } else {
        format!(
            "{} loses access to {name} and disconnects. This device's connection and Workspaces stay attached.",
            device_name
        )
    };
    let message = format!("{message} {name} and its Terminal Sessions keep running.");
    let message = if page.total == 1 {
        format!(
            "{message} This is the server's last paired device. Pair a device again before using the server."
        )
    } else {
        format!("{message} Other paired devices keep their access.")
    };
    Confirmation {
        id: "settings-paired-device-unpair",
        title: format!("Unpair {subject} from {name}?").into(),
        message: message.into(),
        detail: None,
        confirm: "Unpair Device",
        destructive: true,
    }
}
