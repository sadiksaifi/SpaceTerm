//! The Setup Guide: a panel docked on System Settings' window that offers the bundle holding the
//! server's grants to drag into a privacy list. It never activates SpaceTerm or takes keyboard
//! focus.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, CursorStyle, DisplayId,
    ExternalDragPayload, FileDragIcon, FileDragPaths, FontWeight, HighlightStyle, Pixels, Point,
    SharedString, StyledText, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, accesskit, canvas, div, img, px,
};
use spaceterm_ui::{
    Button, ButtonActivation, ButtonRole, ButtonSize, ButtonVariant, ControlHost, ControlMotion,
    ControlWindowActivity, Icon, IconButton, IconName,
};

use super::{GuidePresentation, PermissionSetup, permission_copy};
use crate::platform::permission_access::SystemPermission;
use crate::platform::setup_guide_host::{ApplicationBundle, ApplicationRowImage, SetupGuideHost};
use crate::ui::appearance::{ChromeAppearance, gpui_color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::IconRole;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};

/// The guide's height while it offers a bundle to drag: the instruction line, the bundle's row,
/// and one caption line. Its width follows System Settings' content column.
const DRAGGING_HEIGHT: Pixels = px(114.0);
/// The guide's height otherwise: the instruction or result line above one detail row.
const DETAIL_HEIGHT: Pixels = px(92.0);

const PADDING: f32 = 12.0;
const LINE_HEIGHT: f32 = 20.0;
const CAPTION_HEIGHT: f32 = 16.0;
/// The space between the bundle's row and the caption line below it.
const CAPTION_GAP: f32 = 6.0;
/// The detail and bundle rows' height, close to a row of the System Settings list.
const ROW_HEIGHT: f32 = 40.0;
const ROW_ICON_SIZE: f32 = 24.0;
/// The bundle row's tint of the text color, at rest and under the pointer.
const ROW_TINT: u8 = 0x14;
const ROW_HOVER_TINT: u8 = 0x24;
/// The icon the system drags when the host cannot draw the row.
const DRAG_ICON_SIZE: f32 = 32.0;
/// The opacity of the row's copy under the pointer, which lets the list show through it.
const DRAG_ROW_OPACITY: u8 = 0xD9;
/// How long the arrow takes to point at the list again after a click that did not drag.
const NUDGE_DURATION: Duration = Duration::from_millis(450);
/// How far the arrow rises while it points.
const NUDGE_DISTANCE: f32 = 4.0;

/// The guide's height for what it shows. `dragging` says whether it offers a bundle to drag.
pub(super) fn height(presentation: GuidePresentation, dragging: bool) -> Pixels {
    if dragging && !presentation.granted {
        DRAGGING_HEIGHT
    } else {
        DETAIL_HEIGHT
    }
}

/// Opens the guide at `bounds` on `display` without activating SpaceTerm. `holder` names the
/// entry System Settings lists the grant under, and `bundle` is that entry's bundle to drag, when
/// the guide can offer it.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one independent fact the new window needs"
)]
pub(super) fn open(
    display: DisplayId,
    bounds: Bounds<Pixels>,
    presentation: GuidePresentation,
    holder: String,
    bundle: Option<ApplicationBundle>,
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
                bundle,
                setup,
                host,
                glass,
                nudges: 0,
                row_bounds: Rc::default(),
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
    /// The entry's bundle to drag into the list, when the guide can offer it.
    bundle: Option<ApplicationBundle>,
    setup: WeakEntity<PermissionSetup>,
    host: Arc<dyn SetupGuideHost>,
    /// Whether the host's glass material lies behind the guide. Without it the guide paints its
    /// own surface.
    glass: bool,
    /// Counts clicks on the bundle's row that did not drag it. Each one points the arrow at the
    /// list again.
    nudges: usize,
    /// Where the bundle's row was last laid out, so its copy matches it.
    row_bounds: Rc<Cell<Bounds<Pixels>>>,
}

/// The bundle while a person drags it out of the guide.
struct BundleDrag {
    path: PathBuf,
    /// The row's copy, recorded when the drag begins. The system draws it once the drag leaves
    /// the guide.
    row: Rc<RefCell<Option<HeldRow>>>,
}

/// The row's copy and where the pointer holds it.
struct HeldRow {
    image: ApplicationRowImage,
    cursor_offset: Point<Pixels>,
}

/// The row's copy that follows the pointer until the drag leaves the guide, at the place the
/// pointer took hold of the row.
struct BundleDragPreview {
    icon: Arc<gpui::Image>,
    row: ApplicationRowImage,
}

impl Render for BundleDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.row;
        let appearance = crate::ui::appearance::chrome(cx);
        div()
            .w(row.size.width)
            .h(row.size.height)
            .flex()
            .flex_row()
            .items_center()
            .gap(row.gap)
            .px(row.padding)
            .rounded(row.corner_radius)
            .border_1()
            .border_color(row.border)
            .bg(row.fill)
            .cursor(CursorStyle::ClosedHand)
            .child(img(self.icon.clone()).size(row.icon_size).flex_none())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                    .text_color(row.name_color)
                    .child(row.name.clone()),
            )
    }
}

/// The bundle row's copy for a drag: the guide's surface under the row's contents, so it stays
/// legible over the list.
fn row_copy(
    name: &str,
    bounds: Bounds<Pixels>,
    appearance: &ChromeAppearance,
    window: &Window,
) -> ApplicationRowImage {
    let colors = &appearance.floating_colors;
    let style = appearance.typography.style(TextRole::BodyEmphasis);
    ApplicationRowImage {
        size: bounds.size,
        scale: window.scale_factor(),
        corner_radius: RadiusRole::Control.pixels(),
        fill: gpui_color(
            colors
                .elevated_surface_background
                .with_alpha(DRAG_ROW_OPACITY),
        ),
        border: gpui_color(colors.border_variant),
        padding: appearance.spacing(8.0),
        gap: appearance.spacing(8.0),
        icon_size: px(ROW_ICON_SIZE),
        name: name.to_owned(),
        name_color: gpui_color(colors.text),
        font_size: style.size,
        font_weight: style.font.weight.0,
    }
}

impl SetupGuide {
    pub(super) fn present(&mut self, presentation: GuidePresentation, cx: &mut Context<Self>) {
        if self.presentation != presentation {
            self.presentation = presentation;
            cx.notify();
        }
    }

    /// Names the entry and offers its bundle as the server last reported them.
    pub(super) fn offer(
        &mut self,
        holder: String,
        bundle: Option<ApplicationBundle>,
        cx: &mut Context<Self>,
    ) {
        let path = |bundle: &Option<ApplicationBundle>| bundle.as_ref().map(|b| b.path.clone());
        if self.holder != holder || path(&self.bundle) != path(&bundle) {
            self.holder = holder;
            self.bundle = bundle;
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

    /// The one control besides the drag and Continue. Once every permission is granted it
    /// finishes the setup; before that it cancels the rest.
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

    /// The arrow that points at the list. A click on the bundle's row that does not drag it
    /// raises the arrow once, unless motion is reduced.
    fn render_arrow(&self, glyph: Pixels, appearance: &ChromeAppearance, cx: &App) -> AnyElement {
        let arrow = Icon::new(
            IconName::ArrowUp,
            glyph,
            gpui_color(appearance.floating_colors.text_accent),
        );
        let reduced = cx.try_global::<ControlMotion>() == Some(&ControlMotion::Reduced);
        if self.nudges == 0 || reduced {
            return arrow.into_any_element();
        }
        div()
            .relative()
            .child(arrow)
            .with_animation(
                ("setup-guide-nudge", self.nudges),
                Animation::new(NUDGE_DURATION),
                |arrow, delta| {
                    let lift = (delta * std::f32::consts::PI).sin() * NUDGE_DISTANCE;
                    arrow.top(px(-lift))
                },
            )
            .into_any_element()
    }

    fn render_bundle(
        &self,
        bundle: &ApplicationBundle,
        appearance: &ChromeAppearance,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = &appearance.floating_colors;
        let icon = bundle.icon.clone();
        let name: SharedString = self.holder.clone().into();
        let row_bounds = self.row_bounds.clone();
        let host = self.host.clone();
        div()
            .id("setup-guide-bundle")
            .debug_selector(|| "setup-guide-bundle".to_owned())
            // A click only nudges the arrow, so the row publishes a group rather than a control.
            .role(accesskit::Role::Group)
            .aria_label(name.clone())
            .aria_description("Drag to the list above")
            .h(px(ROW_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(appearance.spacing(8.0))
            .px(appearance.spacing(8.0))
            .relative()
            .rounded(RadiusRole::Control.pixels())
            // A tint of the text color rather than a fill, so the row lets the material behind
            // the guide through in either appearance.
            .bg(gpui_color(colors.text.with_alpha(ROW_TINT)))
            .hover(|style| style.bg(gpui_color(colors.text.with_alpha(ROW_HOVER_TINT))))
            .cursor(CursorStyle::OpenHand)
            .child({
                let row_bounds = self.row_bounds.clone();
                canvas(move |bounds, _, _| row_bounds.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full()
            })
            .child(img(bundle.icon.clone()).size(px(ROW_ICON_SIZE)).flex_none())
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                    .child(name.clone()),
            )
            .on_click(cx.listener(|guide, _, _, cx| {
                guide.nudges += 1;
                cx.notify();
            }))
            .on_drag(
                BundleDrag {
                    path: bundle.path.clone(),
                    row: Rc::default(),
                },
                move |drag, cursor_offset, window, cx| {
                    let appearance = crate::ui::appearance::chrome(cx);
                    let row = row_copy(&name, row_bounds.get(), appearance, window);
                    *drag.row.borrow_mut() = Some(HeldRow {
                        image: row.clone(),
                        cursor_offset,
                    });
                    let icon = icon.clone();
                    cx.new(|_| BundleDragPreview { icon, row })
                },
            )
            .external_drag_payload(move |drag: &BundleDrag, _, _| {
                let drawn = drag.row.borrow().as_ref().and_then(|held| {
                    Some(FileDragIcon::Image {
                        image: host.draw_application_row(&drag.path, &held.image)?,
                        size: held.image.size,
                        cursor_offset: held.cursor_offset,
                    })
                });
                let icon = drawn.unwrap_or(FileDragIcon::File {
                    size: px(DRAG_ICON_SIZE),
                });
                Some(ExternalDragPayload::Files(
                    FileDragPaths::new([(drag.path.clone(), true)]).with_icon(icon),
                ))
            })
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
        } else if self.bundle.is_some() {
            (
                self.render_arrow(glyph, appearance, cx),
                vec![
                    ("Drag ", false),
                    (holder, true),
                    (" to the list above to allow ", false),
                    (name, true),
                    (".", false),
                ],
            )
        } else {
            (
                self.render_arrow(glyph, appearance, cx),
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
        let caption = |text: String| {
            div()
                .id("setup-guide-caption")
                .role(accesskit::Role::Label)
                .aria_value(text.clone())
                .debug_selector(|| "setup-guide-caption".to_owned())
                .h(px(CAPTION_HEIGHT))
                .flex()
                .items_center()
                .pl(indent)
                .min_w_0()
                .truncate()
                .chrome_text(appearance.typography.style(TextRole::Caption))
                .text_color(gpui_color(colors.text_secondary))
                .child(text)
        };
        let body = if let (Some(bundle), false) = (&self.bundle, presentation.granted) {
            let entry = if presentation.cleared {
                format!("SpaceTerm removed an earlier entry for {holder}.")
            } else {
                format!("If {holder} is listed, turn it on.")
            };
            div()
                .flex()
                .flex_col()
                .gap(px(CAPTION_GAP))
                .child(self.render_bundle(bundle, appearance, cx))
                .child(caption(entry))
                .into_any_element()
        } else if presentation.granted {
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
                    "SpaceTerm removed an earlier entry for {holder}. If {holder} isn\u{2019}t listed, close System Settings and set it up again."
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
