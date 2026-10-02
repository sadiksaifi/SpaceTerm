//! The Setup Guide: a panel docked beside System Settings that offers SpaceTerm to drag into a
//! privacy list.
//!
//! The guide floats above System Settings without activating SpaceTerm, so System Settings stays
//! the application a person works in. It never takes keyboard focus; every action is a click.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, Bounds, CursorStyle, DisplayId, ExternalDragPayload, FileDragIcon, FileDragPaths, Pixels,
    Size, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, div, img, px,
};
use spaceterm_ui::{
    Button, ButtonActivation, ButtonRole, ButtonSize, ButtonVariant, ControlHost, ControlWindowActivity, Icon,
    IconName,
};

use super::{GuidePresentation, PermissionSetup, permission_copy};
use crate::platform::setup_guide_host::ApplicationBundle;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

/// The guide's fixed frame. It fits the longest copy at every supported text size.
pub(super) const GUIDE_SIZE: Size<Pixels> = Size {
    width: px(380.0),
    height: px(172.0),
};

/// The icon size of SpaceTerm while a person drags it.
const DRAG_ICON_SIZE: f32 = 64.0;

/// Opens the guide at `bounds` on `display` without activating SpaceTerm.
pub(super) fn open(
    display: DisplayId,
    bounds: Bounds<Pixels>,
    presentation: GuidePresentation,
    bundle: Option<ApplicationBundle>,
    setup: WeakEntity<PermissionSetup>,
    cx: &mut App,
) -> Option<WindowHandle<SetupGuide>> {
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: Some(display),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            tabbing_identifier: None,
            ..WindowOptions::default()
        },
        |window, cx| {
            window.set_window_title("Setup Guide");
            cx.new(|_| SetupGuide {
                presentation,
                bundle,
                setup,
            })
        },
    );
    match opened {
        Ok(handle) => Some(handle),
        Err(_) => {
            eprintln!("failed to open the SpaceTerm Setup Guide");
            None
        }
    }
}

pub(crate) struct SetupGuide {
    presentation: GuidePresentation,
    bundle: Option<ApplicationBundle>,
    setup: WeakEntity<PermissionSetup>,
}

/// The running application while a person drags it out of the guide.
struct ApplicationDrag(PathBuf);

/// What follows the pointer until the drag leaves the guide and the system draws the icon.
struct ApplicationDragPreview(Arc<gpui::Image>);

impl Render for ApplicationDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        img(self.0.clone()).size(px(DRAG_ICON_SIZE))
    }
}

impl SetupGuide {
    pub(super) fn present(&mut self, presentation: GuidePresentation, cx: &mut Context<Self>) {
        if self.presentation != presentation {
            self.presentation = presentation;
            cx.notify();
        }
    }

    /// Runs a setup operation after the current event, so the setup may close this window.
    fn deferred(
        &self,
        operation: fn(&mut PermissionSetup, &mut Context<PermissionSetup>),
    ) -> impl Fn(&ButtonActivation, &mut Window, &mut App) + 'static {
        let setup = self.setup.clone();
        move |_, _, cx| {
            let setup = setup.clone();
            cx.defer(move |cx| {
                let _ = setup.update(cx, operation);
            });
        }
    }

    fn render_application(
        &self,
        appearance: &crate::ui::appearance::ChromeAppearance,
    ) -> Option<impl IntoElement> {
        let bundle = self.bundle.clone()?;
        let colors = &appearance.floating_colors;
        let icon = bundle.icon.clone();
        let name = crate::application_identity::ApplicationIdentity::current().display_name();
        Some(
            div()
                .id("setup-guide-application")
                .debug_selector(|| "setup-guide-application".to_owned())
                .flex_1()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .px(appearance.spacing(8.0))
                .py(appearance.spacing(6.0))
                .rounded(RadiusRole::Control.pixels())
                .border_1()
                .border_color(gpui_color(colors.border_variant))
                .bg(gpui_color(colors.element_background))
                .hover(|style| style.bg(gpui_color(colors.element_hover)))
                .cursor(CursorStyle::OpenHand)
                .child(img(bundle.icon.clone()).size(px(32.0)).flex_none())
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                                .child(name),
                        )
                        .child(
                            div()
                                .truncate()
                                .chrome_text(appearance.typography.style(TextRole::Caption))
                                .text_color(gpui_color(colors.text_secondary))
                                .child("Drag to the list"),
                        ),
                )
                .on_drag(ApplicationDrag(bundle.path), move |_, _, _, cx| {
                    cx.new(|_| ApplicationDragPreview(icon.clone()))
                })
                .external_drag_payload(|drag: &ApplicationDrag, _, _| {
                    Some(ExternalDragPayload::Files(
                        FileDragPaths::new([(drag.0.clone(), true)]).with_icon(
                            FileDragIcon::File {
                                size: px(DRAG_ICON_SIZE),
                            },
                        ),
                    ))
                }),
        )
    }
}

impl Render for SetupGuide {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The guide accompanies the active System Settings window, so it always presents as active.
        let activity = ControlWindowActivity::Active;
        let content = activity.with_scope(|| self.render_panel(cx));
        activity.mount(ControlHost::Floating.mount(content))
    }
}

impl SetupGuide {
    fn render_panel(&self, cx: &App) -> gpui::AnyElement {
        let appearance = crate::ui::appearance::chrome(cx);
        let colors = &appearance.floating_colors;
        let presentation = self.presentation;
        let copy = permission_copy(presentation.permission);
        let application = crate::application_identity::ApplicationIdentity::current().display_name();
        let secondary = gpui_color(colors.text_secondary);
        let reveal = crate::desktop_profile::DesktopPresentation::get(cx)
            .wording()
            .reveal_file;

        let title = if presentation.granted {
            format!("{} Allowed", copy.name)
        } else {
            format!("Allow {}", copy.name)
        };
        let progress = (presentation.total > 1)
            .then(|| format!("{} of {}", presentation.position, presentation.total));
        let message = if presentation.granted {
            let restart = "Restart any tool that was already running.";
            match presentation.permission {
                crate::platform::computer_use_access::ComputerUsePermission::ScreenRecording => {
                    format!(
                        "Tools you start now can {}. {restart} If System Settings offers to quit \
                         and reopen {application}, choose Later to keep your terminal sessions.",
                        copy.purpose
                    )
                }
                crate::platform::computer_use_access::ComputerUsePermission::Accessibility => {
                    format!("Tools you start now can {}. {restart}", copy.purpose)
                }
            }
        } else if self.bundle.is_some() {
            format!(
                "Drag {application} into the {} list. If {application} is already there, turn it \
                 on.",
                copy.pane
            )
        } else {
            format!(
                "Add {application} to the {} list with the add button, then turn it on.",
                copy.pane
            )
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .children(presentation.granted.then(|| {
                Icon::new(
                    IconName::Check,
                    appearance.icons.metrics(IconRole::Status).glyph_size,
                    gpui_color(colors.success),
                )
            }))
            .child(
                div()
                    .debug_selector(|| "setup-guide-title".to_owned())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                    .child(title),
            )
            .children(progress.map(|progress| {
                div()
                    .debug_selector(|| "setup-guide-progress".to_owned())
                    .flex_none()
                    .chrome_text(appearance.typography.style(TextRole::Caption))
                    .text_color(secondary)
                    .child(progress)
            }));

        let actions: Vec<gpui::AnyElement> = if presentation.granted {
            match presentation.next {
                Some(_) => vec![
                    Button::new("setup-guide-not-now", "Not Now")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Small)
                        .role(ButtonRole::Cancel)
                        .debug_selector("setup-guide-not-now")
                        .on_activate(self.deferred(|setup, cx| setup.cancel(cx)))
                        .into_any_element(),
                    Button::new("setup-guide-continue", "Continue")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Small)
                        .debug_selector("setup-guide-continue")
                        .on_activate(self.deferred(|setup, cx| setup.continue_setup(cx)))
                        .into_any_element(),
                ],
                None => vec![
                    Button::new("setup-guide-done", "Done")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Small)
                        .debug_selector("setup-guide-done")
                        .on_activate(self.deferred(|setup, cx| setup.done(cx)))
                        .into_any_element(),
                ],
            }
        } else {
            let mut actions = Vec::new();
            if self.bundle.is_some() {
                actions.push(
                    Button::new("setup-guide-reveal", reveal)
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Small)
                        .debug_selector("setup-guide-reveal")
                        .on_activate(self.deferred(|setup, _| setup.reveal_application()))
                        .into_any_element(),
                );
            }
            actions.push(
                Button::new("setup-guide-cancel", "Cancel")
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Small)
                    .role(ButtonRole::Cancel)
                    .debug_selector("setup-guide-cancel")
                    .on_activate(self.deferred(|setup, cx| setup.cancel(cx)))
                    .into_any_element(),
            );
            actions
        };
        let application = (!presentation.granted)
            .then(|| self.render_application(appearance))
            .flatten();
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .children(application)
            .when(presentation.granted || self.bundle.is_none(), |footer| {
                footer.justify_end()
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .gap(appearance.spacing(8.0))
                    .children(actions),
            );

        div()
            .debug_selector(|| "setup-guide".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .justify_between()
            .gap(appearance.spacing(10.0))
            .p(appearance.spacing(14.0))
            .rounded(RadiusRole::SurfaceLarge.pixels())
            .border_1()
            .border_color(gpui_color(colors.border_variant))
            .bg(gpui_color(
                appearance.floating_surface(colors.elevated_surface_background),
            ))
            .text_color(gpui_color(colors.text))
            .chrome_text(appearance.typography.style(TextRole::Body))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(4.0))
                    .child(header)
                    .child(
                        div()
                            .debug_selector(|| "setup-guide-message".to_owned())
                            .whitespace_normal()
                            .text_color(secondary)
                            .child(message),
                    ),
            )
            .child(footer)
            .into_any_element()
    }
}
