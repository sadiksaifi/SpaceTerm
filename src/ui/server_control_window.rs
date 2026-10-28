//! The server control window a status item opens for one server operation. It asks before
//! an operation that ends Terminal Sessions, explains startup authorization, shows progress, and
//! reports a refusal, without opening a Workspace Window.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Edges, FocusHandle, FontWeight, KeyBinding, LineFragment, Pixels,
    SharedString, Size, Task, Text, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, actions, div, px, size,
};
use spaceterm_server::{
    ServerError,
    service::{
        LingerAuthorization, ServiceStatus,
        control::{Confirmation, Controller},
    },
};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, ProgressRing, ProgressState};

use crate::platform::window_movement::WindowMovementFactory;
use crate::ui::appearance::gpui_color;
use crate::ui::appearance::settings::SettingsSurfaceRole;
use crate::ui::chrome_typography::{ChromeTextStyle, ChromeTextStyleExt as _, TextRole};
use crate::ui::settings_window::{AuthorizationPrompt, AuthorizedRequest, ServiceAction};
use crate::ui::sidebar_window::WindowMovement;

actions!(spaceterm, [DismissServerControl]);

#[cfg(test)]
mod tests;

const KEY_CONTEXT: &str = "ServerControl";
const WINDOW_WIDTH: f32 = 420.0;
const INSET: f32 = 24.0;
const HEADING_TO_BODY: f32 = 8.0;
const BODY_TO_BUTTONS: f32 = 20.0;
const BUTTON_GAP: f32 = 12.0;
/// The ring's extent, which stands in for the buttons while an operation runs.
const PROGRESS_SIZE: f32 = 24.0;

/// The operation the status item asked this window to perform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Operation {
    Start,
    Stop,
    Restart,
}

impl Operation {
    const fn service_action(self) -> ServiceAction {
        match self {
            Self::Start => ServiceAction::Start,
            Self::Stop => ServiceAction::Stop,
            Self::Restart => ServiceAction::Restart,
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// What the window presents. Each step names everything the person needs to decide.
pub(crate) enum Step {
    /// Asks before Stop or Restart, with the shared confirmation every control surface shows.
    Ask(Confirmation),
    /// Explains startup authorization before the operating system asks for a password.
    Authorize,
    Working(Operation),
    Failed(ServerError),
}

/// The key bindings the window installs once.
struct Bindings;
impl gpui::Global for Bindings {}

/// Opens one window for one operation. `closed` runs when the window goes away, whatever the
/// person chose.
pub(crate) fn open(
    controller: Controller,
    operation: Operation,
    step: Step,
    window_movement: Rc<dyn WindowMovementFactory>,
    closed: Box<dyn FnOnce(&mut App)>,
    cx: &mut App,
) -> gpui::Result<()> {
    if !cx.has_global::<Bindings>() {
        cx.bind_keys([
            KeyBinding::new("escape", DismissServerControl, Some(KEY_CONTEXT)),
            // Return chooses Cancel, so it never ends Terminal Sessions by accident.
            KeyBinding::new("enter", DismissServerControl, Some(KEY_CONTEXT)),
        ]);
        cx.set_global(Bindings);
    }
    let name = controller.display_name().to_owned();
    let content = Content::of(&name, operation, &step);
    let window_size = reachable_size(&name, operation, &content, cx);
    let options = cx
        .global::<crate::platform::window_chrome::WindowChrome>()
        .options(
            crate::platform::window_chrome::WindowRole::Panel,
            WindowOptions {
                window_background: crate::ui::appearance_runtime::window_background(cx),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    window_size,
                    cx,
                ))),
                window_min_size: Some(window_size),
                titlebar: Some(TitlebarOptions {
                    title: Some(content.title.clone()),
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
        );
    let drag = window_movement.create();
    cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            cx.on_release(move |_, cx| closed(cx)).detach();
            ServerControlWindow::new(controller, name, operation, step, drag, window, cx)
        })
    })?;
    cx.activate(true);
    Ok(())
}

/// Closes a server control window the way its own close request does: a window that is only
/// asking cancels, and one whose operation runs stays until the operation finishes. Reports
/// whether `window` is a server control window.
pub(crate) fn request_window_close(window: gpui::AnyWindowHandle, cx: &mut App) -> bool {
    let Some(control) = window.downcast::<ServerControlWindow>() else {
        return false;
    };
    let _ = control.update(cx, |this, window, cx| {
        // A window that stays keeps the client windowed with it.
        if matches!(this.step, Some(Step::Working(_))) {
            crate::app::dismissal_cancelled(cx);
        }
        this.respond(Response::Cancel, window, cx);
    });
    true
}

/// The text and buttons of one step.
struct Content {
    title: SharedString,
    heading: SharedString,
    body: Option<SharedString>,
    buttons: Vec<Choice>,
}

#[derive(Clone)]
struct Choice {
    label: SharedString,
    variant: ButtonVariant,
    response: Response,
    selector: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Response {
    Cancel,
    Confirm,
    Authorize,
}

impl Content {
    fn of(name: &str, operation: Operation, step: &Step) -> Self {
        let cancel = Choice {
            label: "Cancel".into(),
            variant: ButtonVariant::Secondary,
            response: Response::Cancel,
            selector: "server-control-cancel",
        };
        match step {
            Step::Ask(confirmation) => Self {
                title: confirmation.title().into(),
                heading: confirmation.message().into(),
                body: (!confirmation.detail().is_empty()).then(|| confirmation.detail().into()),
                buttons: vec![
                    cancel,
                    Choice {
                        label: confirmation.confirm_label().into(),
                        variant: ButtonVariant::Destructive,
                        response: Response::Confirm,
                        selector: "server-control-confirm",
                    },
                ],
            },
            Step::Authorize => {
                let prompt = AuthorizationPrompt::for_request(&AuthorizedRequest::Start, None);
                Self {
                    title: format!("{name} Server").into(),
                    heading: prompt.title.into(),
                    body: Some(format!("{} {}", prompt.message, prompt.detail).into()),
                    buttons: vec![
                        cancel,
                        Choice {
                            label: prompt.confirm.into(),
                            variant: ButtonVariant::Primary,
                            response: Response::Authorize,
                            selector: "server-control-authorize",
                        },
                    ],
                }
            }
            Step::Working(operation) => Self {
                title: format!("{name} Server").into(),
                heading: match operation {
                    Operation::Stop => "Stopping the server…",
                    Operation::Restart => "Restarting the server…",
                    Operation::Start => operation.service_action().progress(false).1,
                }
                .into(),
                body: None,
                buttons: Vec::new(),
            },
            Step::Failed(error) => Self {
                title: format!("{name} Server").into(),
                heading: format!("The {name} server couldn\u{2019}t {}.", operation.verb()).into(),
                body: Some(error.to_string().into()),
                buttons: vec![Choice {
                    label: "OK".into(),
                    variant: ButtonVariant::Primary,
                    response: Response::Cancel,
                    selector: "server-control-ok",
                }],
            },
        }
    }
}

struct Typography {
    heading: ChromeTextStyle,
    body: ChromeTextStyle,
}

impl Typography {
    fn of(appearance: &crate::ui::appearance::ChromeAppearance) -> Self {
        let mut heading = appearance.typography.style(TextRole::Section).clone();
        heading.font.weight = FontWeight::BOLD;
        Self {
            heading,
            body: appearance.typography.style(TextRole::Secondary).clone(),
        }
    }
}

/// The size that fits `content` and every later step its window can reach. A window that
/// cannot be resized keeps one size, because some compositors keep a fixed-size window at its
/// smaller size when it later asks to grow, clipping the step's text and buttons.
fn reachable_size(name: &str, operation: Operation, content: &Content, cx: &App) -> Size<Pixels> {
    let mut later = vec![Content::of(name, operation, &Step::Working(operation))];
    // Start and Restart can need startup authorization, then continue as Start, and the
    // operating system's prompt can be refused. Starting the server can fail under either
    // operation's heading.
    if operation != Operation::Stop {
        let start = Operation::Start;
        later.push(Content::of(name, start, &Step::Working(start)));
        later.push(Content::of(name, start, &Step::Authorize));
        later
            .extend([operation, start].map(|failed| {
                Content::of(name, failed, &Step::Failed(ServerError::StartupFailed))
            }));
    }
    std::iter::once(content)
        .chain(&later)
        .map(|content| window_size(content, cx))
        .fold(size(px(WINDOW_WIDTH), px(0.0)), |fits, needed| {
            size(fits.width, fits.height.max(needed.height))
        })
}

/// The fixed width, and the height the step's wrapped text and controls take.
fn window_size(content: &Content, cx: &App) -> Size<Pixels> {
    let surface = crate::ui::appearance::settings::shared(cx);
    let appearance = &surface.chrome;
    let typography = Typography::of(appearance);
    let text_width = px(WINDOW_WIDTH - 2.0 * INSET);
    let wrapped = |style: &ChromeTextStyle, text: &str| {
        let lines = 1 + cx
            .text_system()
            .line_wrapper(style.font.clone(), style.size)
            .wrap_line(&[LineFragment::text(text)], text_width)
            .count();
        style.line_height * lines as f32
    };
    let mut height = appearance.spacing(2.0 * INSET + BODY_TO_BUTTONS)
        + wrapped(&typography.heading, &content.heading);
    if let Some(body) = &content.body {
        height += appearance.spacing(HEADING_TO_BODY) + wrapped(&typography.body, body);
    }
    height += if content.buttons.is_empty() {
        px(PROGRESS_SIZE)
    } else {
        cx.global::<spaceterm_ui::ButtonTheme>()
            .control_height(ButtonSize::Regular)
    };
    size(px(WINDOW_WIDTH), height.ceil())
}

pub(crate) struct ServerControlWindow {
    controller: Controller,
    name: String,
    operation: Operation,
    step: Option<Step>,
    focus_handle: FocusHandle,
    window_movement: WindowMovement,
    window_appearance: super::appearance_runtime::WindowAppearanceOwner,
}

impl ServerControlWindow {
    fn new(
        controller: Controller,
        name: String,
        operation: Operation,
        step: Step,
        drag: Rc<dyn crate::platform::window_movement::OperatingSystemWindowDragPlatform>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window_appearance = super::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        cx.observe_global_in::<super::appearance_runtime::InstalledAppearance>(
            window,
            |this, window, cx| {
                this.window_appearance.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
        // Closing while an operation runs would abandon its result, so the window stays open.
        let owner = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            owner
                .read_with(cx, |this, _| !matches!(this.step, Some(Step::Working(_))))
                .unwrap_or(true)
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let window_movement = WindowMovement::new(drag, focus_handle.clone(), "Server Control");
        Self {
            controller,
            name,
            operation,
            step: Some(step),
            focus_handle,
            window_movement,
            window_appearance,
        }
    }

    fn respond(&mut self, response: Response, window: &mut Window, cx: &mut Context<Self>) {
        match (self.step.take(), response) {
            (Some(Step::Working(operation)), _) => self.step = Some(Step::Working(operation)),
            (Some(Step::Ask(confirmation)), Response::Confirm) => {
                let controller = self.controller.clone();
                self.run(
                    cx.background_spawn(async move { controller.confirm(confirmation) }),
                    window,
                    cx,
                );
            }
            (Some(Step::Authorize), Response::Authorize) => {
                let controller = self.controller.clone();
                self.run(
                    cx.background_spawn(async move {
                        controller
                            .enable_linger(LingerAuthorization::Desktop)
                            .and_then(|()| controller.start())
                    }),
                    window,
                    cx,
                );
            }
            _ => window.remove_window(),
        }
    }

    fn dismiss(&mut self, _: &DismissServerControl, window: &mut Window, cx: &mut Context<Self>) {
        self.respond(Response::Cancel, window, cx);
    }

    /// Runs the admitted operation off the event thread and closes when it succeeds.
    fn run(
        &mut self,
        work: Task<Result<ServiceStatus, ServerError>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(Step::Working(self.operation), window, cx);
        // Exiting the client waits for the admitted operation. Its result is collected by the
        // application rather than this window, so the wait always ends.
        crate::status_menu::begin_operation(cx);
        let control = window.window_handle().downcast::<Self>();
        App::spawn(cx, async move |cx| {
            let result = work.await;
            cx.update(crate::status_menu::end_operation);
            let Some(control) = control else {
                return;
            };
            if let Ok(status) = result {
                cx.update(|cx| crate::status_menu::operation_succeeded(cx, status));
            }
            let _ = control.update(cx, |this, window, cx| match result {
                Ok(_) => window.remove_window(),
                // The operating system's prompt was cancelled or refused. Nothing changed.
                Err(ServerError::AuthorizationRequired) if this.operation == Operation::Start => {
                    this.show(Step::Authorize, window, cx);
                }
                // The Restart stopped the server, then starting it needed authorization. Only
                // the start remains, so the window continues as Start and never restarts again.
                Err(ServerError::AuthorizationRequired) if this.operation == Operation::Restart => {
                    this.operation = Operation::Start;
                    this.show(Step::Authorize, window, cx);
                }
                Err(error) => this.show(Step::Failed(error), window, cx),
            });
        })
        .detach();
    }

    fn show(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let content = Content::of(&self.name, self.operation, &step);
        window.set_window_title(&content.title);
        // The window opened at a size every expected step fits. Only an unexpectedly long
        // refusal grows it, and nothing shrinks it.
        let needed = window_size(&content, cx);
        let current = window.bounds().size;
        if needed.height > current.height {
            window.resize(size(current.width, needed.height));
        }
        self.step = Some(step);
        cx.notify();
    }

    fn render_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let surface = crate::ui::appearance::settings::shared(cx);
        let appearance = &surface.chrome;
        let typography = Typography::of(appearance);
        let Some(step) = &self.step else {
            return div().into_any_element();
        };
        let content = Content::of(&self.name, self.operation, step);
        let text = gpui_color(appearance.colors.text);
        let secondary = gpui_color(appearance.colors.text_secondary);
        let line = |selector: &'static str, style: &ChromeTextStyle, color, value: SharedString| {
            div()
                .debug_selector(move || selector.to_owned())
                .w_full()
                .text_center()
                .chrome_text(style)
                .text_color(color)
                .child(Text::new(selector.into(), value))
        };
        let controls = if content.buttons.is_empty() {
            div()
                .flex()
                .justify_center()
                .child(ProgressRing::new(
                    "server-control-progress",
                    content.heading.clone(),
                    ProgressState::Indeterminate,
                ))
                .into_any_element()
        } else {
            let owner = cx.weak_entity();
            div()
                .flex()
                .flex_row()
                .gap(appearance.spacing(BUTTON_GAP))
                .children(content.buttons.iter().map(|choice| {
                    let owner = owner.clone();
                    let response = choice.response;
                    div()
                        .flex_1()
                        .child(
                            Button::new(choice.selector, choice.label.clone())
                                .variant(choice.variant)
                                .size(ButtonSize::Regular)
                                .full_width(true)
                                .tab_stop(true)
                                .debug_selector(choice.selector)
                                .on_activate(move |_, window, cx| {
                                    let _ = owner.update(cx, |this, cx| {
                                        this.respond(response, window, cx);
                                    });
                                }),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        };
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .p(appearance.spacing(INSET))
            .child(line(
                "server-control-heading",
                &typography.heading,
                text,
                content.heading.clone(),
            ))
            .children(content.body.clone().map(|body| {
                line("server-control-detail", &typography.body, secondary, body)
                    .mt(appearance.spacing(HEADING_TO_BODY))
            }))
            // A shorter step leaves its space above the buttons, which stay at the bottom.
            .child(div().flex_1().min_h(appearance.spacing(BODY_TO_BUTTONS)))
            .child(controls);
        let movement = self.window_movement.region(
            "server-control-drag-region".to_owned(),
            // Like an alert dialog, the window has no close control: Cancel, Escape, and the
            // window manager's close request close it, and none can overlap the heading.
            div().relative().size_full().child(body),
            Edges::default(),
            window,
        );
        let root = div()
            .id("server-control-surface")
            .role(gpui::accesskit::Role::AlertDialog)
            .aria_label(content.heading)
            .debug_selector(|| "server-control-surface".to_owned())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::dismiss))
            .on_key_down(traverse)
            .size_full()
            .bg(gpui_color(
                surface.surface(SettingsSurfaceRole::Canvas).paint,
            ))
            .text_color(text)
            .child(movement);
        super::window_shell::render(root, window, cx)
    }
}

/// Tab and Shift-Tab move focus between the buttons, so the keyboard reaches every choice.
fn traverse(event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut App) {
    let modifiers = event.keystroke.modifiers;
    if event.keystroke.key != "tab"
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
    window.prevent_default();
    cx.stop_propagation();
}

impl Render for ServerControlWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = super::appearance::window_activity(window);
        super::sidebar_window::render_scoped(activity, || self.render_surface(window, cx))
    }
}
