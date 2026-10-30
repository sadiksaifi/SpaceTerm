//! The Setup Guide: a panel docked on System Settings' window that names the entry to turn on in a
//! privacy list. It never activates SpaceTerm or takes keyboard focus.

use std::ops::Range;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, DisplayId, FontWeight, HighlightStyle, Pixels, SharedString,
    StyledText, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, accesskit, div, px,
};
use spaceterm_ui::{
    Button, ButtonActivation, ButtonRole, ButtonSize, ButtonVariant, ControlHost,
    ControlWindowActivity, Icon, IconButton, IconName,
};

use super::{GuidePresentation, PermissionSetup, permission_copy};
use crate::platform::permission_access::SystemPermission;
use crate::platform::setup_guide_host::SetupGuideHost;
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

/// The guide's height: the instruction line above one detail row. Its width follows System
/// Settings' content column.
pub(super) const HEIGHT: Pixels = px(92.0);

const PADDING: f32 = 12.0;
const LINE_HEIGHT: f32 = 20.0;
/// The detail row's height, close to a row of the System Settings list.
const ROW_HEIGHT: f32 = 40.0;

/// Opens the guide at `bounds` on `display` without activating SpaceTerm. `holder` names the
/// entry System Settings lists the grant under.
pub(super) fn open(
    display: DisplayId,
    bounds: Bounds<Pixels>,
    presentation: GuidePresentation,
    holder: String,
    host: Arc<dyn SetupGuideHost>,
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
            let glass = host.configure_window(window, RadiusRole::SurfaceLarge.pixels());
            cx.new(|_| SetupGuide {
                presentation,
                holder,
                setup,
                glass,
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
    /// The entry System Settings lists the grant under.
    holder: String,
    setup: WeakEntity<PermissionSetup>,
    /// Whether the host's glass material lies behind the guide. Without it the guide paints its
    /// own surface.
    glass: bool,
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

    /// The one control besides Continue. Once every permission is granted it finishes the setup;
    /// before that it cancels the rest.
    fn render_close(&self) -> AnyElement {
        let presentation = self.presentation;
        let operation: fn(&mut PermissionSetup, &mut Context<PermissionSetup>) =
            if presentation.granted && presentation.next.is_none() {
                |setup, cx| setup.done(cx)
            } else {
                |setup, cx| setup.cancel(cx)
            };
        IconButton::new("setup-guide-close", "Close", |foreground| {
            Icon::new(IconName::X, px(12.0), foreground).into_any_element()
        })
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::Small)
        .role(ButtonRole::Cancel)
        .debug_selector("setup-guide-close")
        .on_activate(self.deferred(operation))
        .into_any_element()
    }

    /// The arrow that points at the list.
    fn render_arrow(&self, glyph: Pixels, appearance: &ChromeAppearance) -> AnyElement {
        Icon::new(
            IconName::ArrowUp,
            glyph,
            gpui_color(appearance.floating_colors.text_accent),
        )
        .into_any_element()
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

/// One sentence as assistive technology reads it, without its emphasis.
fn plain(parts: &[(&str, bool)]) -> String {
    parts.iter().map(|(part, _)| *part).collect()
}

/// One sentence whose `true` parts are semibold, the way the guide names the entry and what it
/// allows.
fn emphasized(parts: &[(&str, bool)]) -> StyledText {
    let mut text = String::new();
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for (part, strong) in parts {
        let start = text.len();
        text.push_str(part);
        if *strong {
            highlights.push((
                start..text.len(),
                HighlightStyle {
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

impl SetupGuide {
    fn render_panel(&self, cx: &Context<Self>) -> AnyElement {
        let appearance = crate::ui::appearance::chrome(cx);
        let colors = &appearance.floating_colors;
        let presentation = self.presentation;
        let name = permission_copy(presentation.permission, presentation.naming).name;
        let holder = self.holder.as_str();
        let glyph = appearance.icons.metrics(IconRole::Status).glyph_size;
        // Lines below the header start under its text, past the symbol.
        let indent = glyph + appearance.spacing(6.0);

        let (symbol, message) = if presentation.granted {
            (
                Icon::new(IconName::Check, glyph, gpui_color(colors.success)).into_any_element(),
                vec![(name, true), (" is allowed.", false)],
            )
        } else {
            (
                self.render_arrow(glyph, appearance),
                vec![
                    ("Turn on ", false),
                    (holder, true),
                    (" to allow ", false),
                    (name, true),
                    (".", false),
                ],
            )
        };
        let header = div()
            .h(px(LINE_HEIGHT))
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(6.0))
            .child(div().flex_none().child(symbol))
            .child(
                div()
                    .id("setup-guide-message")
                    .role(accesskit::Role::Label)
                    .aria_value(plain(&message))
                    .debug_selector(|| "setup-guide-message".to_owned())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(emphasized(&message)),
            )
            .child(div().flex_none().child(self.render_close()));

        let detail = |text: String| {
            div()
                .id("setup-guide-detail")
                .role(accesskit::Role::Label)
                .aria_value(text.clone())
                .debug_selector(|| "setup-guide-detail".to_owned())
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .chrome_text(appearance.typography.style(TextRole::Caption))
                .text_color(gpui_color(colors.text_secondary))
                .child(text)
        };
        let body = if presentation.granted {
            let advice = match presentation.permission {
                SystemPermission::ScreenRecording => {
                    format!("If offered to quit {holder}, choose Later.")
                }
                SystemPermission::Accessibility => "Restart any tool already running.".to_owned(),
            };
            div()
                .h(px(ROW_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(appearance.spacing(8.0))
                .pl(indent)
                .child(detail(advice))
                .children(presentation.next.map(|next| {
                    Button::new(
                        "setup-guide-continue",
                        format!("Allow {}", permission_copy(next, presentation.naming).name),
                    )
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Small)
                    .debug_selector("setup-guide-continue")
                    .on_activate(self.deferred(|setup, cx| setup.continue_setup(cx)))
                }))
                .into_any_element()
        } else {
            let advice = if presentation.cleared {
                format!(
                    "SpaceTerm removed an entry an earlier {holder} left, then listed it again."
                )
            } else {
                format!(
                    "If {holder} isn\u{2019}t listed, close System Settings and set it up again."
                )
            };
            div()
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .pl(indent)
                .child(detail(advice))
                .into_any_element()
        };

        div()
            .debug_selector(|| "setup-guide".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .gap(appearance.spacing(8.0))
            .p(px(PADDING))
            .rounded(RadiusRole::SurfaceLarge.pixels())
            // Without glass the guide has no blur, so its surface is opaque.
            .when(!self.glass, |panel| {
                let base = colors.elevated_surface_background;
                let surface = appearance
                    .floating_surface(base)
                    .source_over(base.with_alpha(255));
                panel
                    .border_1()
                    .border_color(gpui_color(colors.border_variant))
                    .bg(gpui_color(surface))
            })
            .text_color(gpui_color(colors.text))
            .chrome_text(appearance.typography.style(TextRole::Body))
            .child(header)
            .child(body)
            .into_any_element()
    }
}
