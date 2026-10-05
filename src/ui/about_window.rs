//! The About Operating-System Window presents SpaceTerm's application information.
//!
//! It reproduces AppKit's standard About panel: a small, fixed, modeless window presenting the
//! application icon, the name in bold, the version in secondary text, the description, and the
//! copyright, centered one above the other. The whole surface moves the window, as a panel
//! without controls of its own should, and only the close control is offered.

mod application_icon;

use std::{rc::Rc, sync::Arc};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Edges, FocusHandle, FontWeight, Global, LineFragment, Pixels,
    SharedString, Size, Text, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, actions, div, img, px, size,
};
use spaceterm_ui::{ClientWindowControls, WindowControlSide};

use crate::about::About;
use crate::platform::window_movement::WindowMovementFactory;
use crate::ui::appearance::gpui_color;
use crate::ui::appearance::settings::SettingsSurfaceRole;
use crate::ui::chrome_typography::{ChromeTextStyle, ChromeTextStyleExt as _, TextRole};
use crate::ui::sidebar_window::WindowMovement;

actions!(spaceterm, [CloseAboutWindow]);

/// The key context the About window publishes, so its close shortcuts apply only inside it.
pub(crate) const ABOUT_KEY_CONTEXT: &str = "About";

/// The panel's fixed width. Its height follows from the content it presents.
const WINDOW_WIDTH: f32 = 284.0;
/// The space between the window's side edges and the text.
const TEXT_INSET: f32 = 24.0;
/// The icon's drawn size, which includes the icon grid's own transparent margin.
const ICON_SIZE: f32 = 64.0;
/// Vertical rhythm from the top edge to the bottom edge, in content order.
const TOP_INSET: f32 = 20.0;
const ICON_TO_NAME: f32 = 8.0;
const NAME_TO_VERSION: f32 = 2.0;
const VERSION_TO_DESCRIPTION: f32 = 12.0;
const DESCRIPTION_TO_COPYRIGHT: f32 = 12.0;
const BOTTOM_INSET: f32 = 20.0;

/// The one About window, so a second request activates it.
struct OpenAboutWindow(WindowHandle<AboutWindow>);
impl Global for OpenAboutWindow {}

/// Host-owned window movement for the About window's client surface.
struct AboutWindowComposition {
    window_movement: Rc<dyn WindowMovementFactory>,
}
impl Global for AboutWindowComposition {}

pub(crate) fn configure_window_chrome(
    window_movement: Rc<dyn WindowMovementFactory>,
    cx: &mut App,
) {
    cx.set_global(AboutWindowComposition { window_movement });
}

/// The open About window. Membership, not a root-view read, decides this: the root view is leased
/// while an action dispatches inside the window.
fn open_about_window(cx: &App) -> Option<WindowHandle<AboutWindow>> {
    let handle = cx.try_global::<OpenAboutWindow>()?.0;
    cx.windows()
        .iter()
        .any(|window| window.window_id() == handle.window_id())
        .then_some(handle)
}

/// Opens the About window, or activates it when it is already open.
pub(crate) fn open_or_activate(cx: &mut App) {
    if let Some(existing) = open_about_window(cx) {
        cx.defer(move |cx| {
            let _ = existing.update(cx, |_, window, _| window.activate_window());
        });
        return;
    }
    if !cx.has_global::<crate::ui::appearance_runtime::AppearanceRuntime>() {
        eprintln!("SpaceTerm About is unavailable because appearance is not installed");
        return;
    }
    let Some(composition) = cx.try_global::<AboutWindowComposition>() else {
        eprintln!("SpaceTerm About is unavailable because window chrome is not installed");
        return;
    };
    let window_drag = composition.window_movement.create();
    let about = About::current();
    let title = SharedString::from(format!("About {}", about.name()));
    let opened = cx.open_window(window_options(title.clone(), about, cx), |window, cx| {
        cx.new(|cx| AboutWindow::new(about, title, window_drag, window, cx))
    });
    match opened {
        Ok(handle) => {
            cx.set_global(OpenAboutWindow(handle));
            cx.activate(true);
        }
        Err(_) => eprintln!("failed to open the SpaceTerm About window"),
    }
}

fn window_options(title: SharedString, about: About, cx: &App) -> WindowOptions {
    let size = window_size(about, cx);
    cx.global::<crate::platform::window_chrome::WindowChrome>()
        .options(
            crate::platform::window_chrome::WindowRole::Panel,
            WindowOptions {
                window_background: crate::ui::appearance_runtime::window_background(cx),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
                window_min_size: Some(size),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                kind: WindowKind::Normal,
                is_movable: true,
                is_resizable: false,
                is_minimizable: false,
                tabbing_identifier: None,
                ..WindowOptions::default()
            },
            cx,
        )
}

/// The text styles of the panel's four lines.
struct Typography {
    name: ChromeTextStyle,
    version: ChromeTextStyle,
    description: ChromeTextStyle,
    copyright: ChromeTextStyle,
}

impl Typography {
    fn of(appearance: &crate::ui::appearance::ChromeAppearance) -> Self {
        let mut name = appearance.typography.style(TextRole::Section).clone();
        name.font.weight = FontWeight::BOLD;
        Self {
            name,
            version: appearance.typography.style(TextRole::Secondary).clone(),
            description: appearance.typography.style(TextRole::Secondary).clone(),
            copyright: appearance.typography.style(TextRole::Caption).clone(),
        }
    }
}

/// The panel's size: its fixed width, and the height its content takes at the density it opens
/// with, every line wrapped as it will render. Like AppKit's panel, it keeps that size while open.
fn window_size(about: About, cx: &App) -> Size<Pixels> {
    let surface = crate::ui::appearance::settings::shared(cx);
    let appearance = &surface.chrome;
    let typography = Typography::of(appearance);
    let text_width = px(WINDOW_WIDTH - 2.0 * TEXT_INSET);
    let version = about.version_line();
    let text_height = [
        (&typography.name, about.name()),
        (&typography.version, version.as_str()),
        (&typography.description, about.description()),
        (&typography.copyright, about.copyright()),
    ]
    .into_iter()
    .map(|(style, text)| {
        let wrapped_lines = 1 + cx
            .text_system()
            .line_wrapper(style.font.clone(), style.size)
            .wrap_line(&[LineFragment::text(text)], text_width)
            .count();
        style.line_height * wrapped_lines as f32
    })
    .fold(px(0.0), |height, line| height + line);
    let height = appearance.spacing(
        TOP_INSET
            + ICON_TO_NAME
            + NAME_TO_VERSION
            + VERSION_TO_DESCRIPTION
            + DESCRIPTION_TO_COPYRIGHT
            + BOTTOM_INSET,
    ) + px(ICON_SIZE)
        + text_height;
    size(px(WINDOW_WIDTH), height.ceil())
}

pub(crate) struct AboutWindow {
    about: About,
    icon: Option<Arc<gpui::Image>>,
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
    focus_handle: FocusHandle,
    window_movement: WindowMovement,
}

impl AboutWindow {
    fn new(
        about: About,
        title: SharedString,
        window_drag: Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        window.set_window_title(&title);
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        cx.observe_global_in::<super::appearance_runtime::InstalledAppearance>(
            window,
            |about_window, window, cx| {
                about_window.window_appearance.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let window_movement = WindowMovement::new(window_drag, focus_handle.clone(), "About");
        let icon = match application_icon::svg(about.identity().icon(), ICON_SIZE) {
            Ok(svg) => Some(Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Svg,
                svg.into_bytes(),
            ))),
            Err(error) => {
                eprintln!("failed to draw the application icon: {error}");
                None
            }
        };
        Self {
            about,
            icon,
            window_appearance,
            focus_handle,
            window_movement,
        }
    }

    fn close(&mut self, _: &CloseAboutWindow, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn render_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let surface = crate::ui::appearance::settings::shared(cx);
        let appearance = &surface.chrome;
        let typography = Typography::of(appearance);
        let text = gpui_color(appearance.colors.text);
        let secondary = gpui_color(appearance.colors.text_secondary);
        // Each fact is its own identified text, so a screen reader reads it as a label.
        let line = |selector: &'static str, style: &ChromeTextStyle, color, value: SharedString| {
            div()
                .debug_selector(move || selector.to_owned())
                .w_full()
                .text_center()
                .chrome_text(style)
                .text_color(color)
                .child(Text::new(selector.into(), value))
        };
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(TEXT_INSET))
            .pt(appearance.spacing(TOP_INSET))
            .child(
                div()
                    .debug_selector(|| "about-icon".to_owned())
                    .flex_none()
                    .size(px(ICON_SIZE))
                    .children(self.icon.clone().map(|icon| img(icon).size_full())),
            )
            .child(
                line(
                    "about-name",
                    &typography.name,
                    text,
                    self.about.name().into(),
                )
                .mt(appearance.spacing(ICON_TO_NAME)),
            )
            .child(
                line(
                    "about-version",
                    &typography.version,
                    secondary,
                    self.about.version_line().into(),
                )
                .mt(appearance.spacing(NAME_TO_VERSION)),
            )
            .child(
                line(
                    "about-description",
                    &typography.description,
                    text,
                    self.about.description().into(),
                )
                .mt(appearance.spacing(VERSION_TO_DESCRIPTION)),
            )
            .child(
                line(
                    "about-copyright",
                    &typography.copyright,
                    secondary,
                    self.about.copyright().into(),
                )
                .mt(appearance.spacing(DESCRIPTION_TO_COPYRIGHT)),
            );
        let closing = cx.weak_entity();
        let close: spaceterm_ui::WindowCloseHandler = Rc::new(move |window, cx| {
            let _ = closing.update(cx, |about, cx| about.close(&CloseAboutWindow, window, cx));
        });
        let edge_margin = px(spaceterm_ui::DesktopWindowStyle::current(cx)
            .control_metrics()
            .edge_margin);
        let control_surface = gpui_color(appearance.colors.title_bar_background);
        let controls = div()
            .debug_selector(|| "about-window-controls".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .flex()
            .flex_row()
            .justify_between()
            .pt(edge_margin)
            .px(edge_margin)
            .child(
                ClientWindowControls::new(Rc::clone(&close))
                    .side(WindowControlSide::Left)
                    .surface_color(control_surface),
            )
            .child(
                ClientWindowControls::new(close)
                    .side(WindowControlSide::Right)
                    .surface_color(control_surface),
            );
        let movement = self.window_movement.region(
            "about-drag-region".to_owned(),
            div().relative().size_full().child(content).child(controls),
            Edges::default(),
            window,
        );
        let root = div()
            .debug_selector(|| "about-window-surface".to_owned())
            .key_context(ABOUT_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close))
            .size_full()
            .bg(gpui_color(
                surface.surface(SettingsSurfaceRole::Canvas).paint,
            ))
            .text_color(text)
            .child(movement);
        super::window_shell::render(root, window, cx)
    }
}

impl Render for AboutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = super::appearance::window_activity(window);
        super::sidebar_window::render_scoped(activity, || self.render_surface(window, cx))
    }
}

#[cfg(test)]
mod tests;
