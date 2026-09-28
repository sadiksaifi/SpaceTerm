//! The launch view: a small window shown while a fresh launch waits on an update.
//!
//! The service owns the launch gate and releases it on success, failure, or timeout. This view
//! only reports progress. It offers no action and constructs no Workspace, and it closes itself
//! once the gate opens.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Entity, Global, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, px, size,
};
use spaceterm_ui::{DeterminateProgress, ProgressBar, ProgressSize, ProgressState};

use super::{CURRENT_VERSION, service};
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::updates::{ApplicationUpdates, LaunchState, UpdateState};

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 168.0;
/// A quick launch check finishes before a window would be worth showing.
const CHECK_REVEAL_DELAY: Duration = Duration::from_millis(600);

struct OpenLaunchWindow(WindowHandle<LaunchView>);

impl Global for OpenLaunchWindow {}

/// Presents the launch view for the gate the service holds, and closes it once the gate opens.
///
/// A check alone is shown only if it outlasts a short delay. If the window cannot be shown, the
/// gate is released so a launch never waits on a view nobody can see.
pub(crate) fn show_launch(cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    match updates.read(cx).launch_state() {
        LaunchState::Open => {}
        LaunchState::Checking => cx
            .spawn(async move |cx| {
                cx.background_executor().timer(CHECK_REVEAL_DELAY).await;
                cx.update(open_window);
            })
            .detach(),
        LaunchState::Required | LaunchState::Installing => open_window(cx),
    }
}

fn open_window(cx: &mut App) {
    let Some(updates) = service(cx) else { return };
    let already_open = cx
        .try_global::<OpenLaunchWindow>()
        .is_some_and(|open| open.0.read(cx).is_ok());
    if already_open || updates.read(cx).launch_state() == LaunchState::Open {
        return;
    }
    if !cx.has_global::<crate::ui::appearance_runtime::AppearanceRuntime>() {
        eprintln!("SpaceTerm could not show update progress because appearance is not installed");
        ApplicationUpdates::release_launch(cx);
        return;
    }
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_background: crate::ui::appearance_runtime::window_background(cx),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("SpaceTerm".into()),
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
        |window, cx| {
            // The service decides when the launch continues. Closing the view would only hide
            // that progress, so the window stays until the gate opens. Quit remains available.
            window.on_window_should_close(cx, |_, _| false);
            cx.new(|cx| LaunchView::new(updates.clone(), window, cx))
        },
    );
    match opened {
        Ok(handle) => {
            cx.set_global(OpenLaunchWindow(handle));
            cx.activate(true);
        }
        Err(_) => {
            eprintln!("failed to open the SpaceTerm update window");
            ApplicationUpdates::release_launch(cx);
        }
    }
}

/// What the launch view says for one gate and service state.
#[derive(Clone, Debug, PartialEq)]
struct LaunchPresentation {
    title: &'static str,
    message: Option<SharedString>,
    status: SharedString,
    /// Trailing detail under the bar, such as the downloaded size.
    detail: Option<SharedString>,
    /// Completion, or `None` while the work has no knowable completion.
    progress: Option<f32>,
}

impl LaunchPresentation {
    fn resolve(launch: LaunchState, state: &UpdateState) -> Option<Self> {
        let (title, message) = match launch {
            LaunchState::Open => return None,
            LaunchState::Checking => {
                return Some(Self {
                    title: "Checking for Updates",
                    message: None,
                    status: "SpaceTerm opens in a moment.".into(),
                    detail: None,
                    progress: None,
                });
            }
            LaunchState::Required => (
                "Updating SpaceTerm",
                format!(
                    "SpaceTerm {CURRENT_VERSION} is out of date. The update installs before \
                     SpaceTerm opens."
                ),
            ),
            LaunchState::Installing => (
                "Installing Update",
                "SpaceTerm is finishing the update it downloaded earlier.".to_owned(),
            ),
        };
        let reopens = || Some(SharedString::from("SpaceTerm reopens when it’s done."));
        let (status, detail, progress) = match state {
            UpdateState::Downloading {
                version,
                received,
                total,
            } if *total > 0 => (
                format!("Downloading SpaceTerm {version}…"),
                Some(format!("{} of {}", size_label(*received), size_label(*total)).into()),
                Some((*received as f64 / *total as f64).clamp(0.0, 1.0) as f32),
            ),
            UpdateState::Downloading { version, .. } => {
                (format!("Downloading SpaceTerm {version}…"), None, None)
            }
            UpdateState::Available { version }
            | UpdateState::Verifying { version }
            | UpdateState::Ready { version } => {
                (format!("Preparing SpaceTerm {version}…"), None, None)
            }
            UpdateState::Installing { version } => {
                (format!("Installing SpaceTerm {version}…"), reopens(), None)
            }
            _ => ("Preparing the update…".to_owned(), None, None),
        };
        Some(Self {
            title,
            message: Some(message.into()),
            status: status.into(),
            detail,
            progress,
        })
    }
}

/// A download size in the decimal units the system uses for files.
fn size_label(bytes: u64) -> String {
    const MEGABYTE: f64 = 1_000_000.0;
    if bytes < 1_000_000 {
        format!("{} KB", bytes.div_ceil(1_000))
    } else {
        format!("{:.1} MB", bytes as f64 / MEGABYTE)
    }
}

struct LaunchView {
    updates: Entity<ApplicationUpdates>,
    window_appearance: crate::ui::appearance_runtime::WindowAppearanceOwner,
}

impl LaunchView {
    fn new(
        updates: Entity<ApplicationUpdates>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window_appearance = crate::ui::appearance_runtime::WindowAppearanceOwner::default();
        window_appearance.apply(window, cx);
        cx.observe_in(&updates, window, |_, updates, window, cx| {
            if updates.read(cx).launch_state() == LaunchState::Open {
                window.remove_window();
            } else {
                cx.notify();
            }
        })
        .detach();
        cx.observe_global_in::<crate::ui::appearance_runtime::InstalledAppearance>(
            window,
            |view, window, cx| {
                view.window_appearance.apply(window, cx);
                cx.notify();
            },
        )
        .detach();
        Self {
            updates,
            window_appearance,
        }
    }
}

impl Render for LaunchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let activity = crate::ui::appearance::window_activity(window);
        let updates = self.updates.read(cx);
        let presentation = LaunchPresentation::resolve(updates.launch_state(), updates.state());
        let appearance = crate::ui::appearance::chrome(cx);
        let Some(presentation) = presentation else {
            return activity.mount(div().into_any_element());
        };
        let secondary = gpui_color(appearance.colors.text_secondary);
        let progress = match presentation
            .progress
            .map(|fraction| DeterminateProgress::new(f64::from(fraction)))
        {
            Some(Ok(progress)) => ProgressState::Determinate(progress),
            _ => ProgressState::Indeterminate,
        };
        let content = div()
            .debug_selector(|| "update-launch".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap(appearance.spacing(14.0))
            .px(appearance.spacing(28.0))
            // Clear the transparent title bar so the content centers in what remains.
            .pt(appearance.top_height() / 2.0)
            .text_color(gpui_color(appearance.colors.text))
            .chrome_text(appearance.typography.style(TextRole::Body))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(4.0))
                    .child(
                        div()
                            .debug_selector(|| "update-launch-title".to_owned())
                            .chrome_text(appearance.typography.style(TextRole::Section))
                            .child(presentation.title),
                    )
                    .children(presentation.message.map(|message| {
                        div()
                            .debug_selector(|| "update-launch-message".to_owned())
                            .text_color(secondary)
                            .whitespace_normal()
                            .child(message)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(appearance.spacing(6.0))
                    .child(
                        ProgressBar::new("update-launch-progress", presentation.title, progress)
                            .size(ProgressSize::Regular)
                            .debug_selector("update-launch-progress"),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .gap(appearance.spacing(12.0))
                            .chrome_text(appearance.typography.style(TextRole::Secondary))
                            .text_color(secondary)
                            .child(
                                div()
                                    .debug_selector(|| "update-launch-status".to_owned())
                                    .min_w_0()
                                    .truncate()
                                    .child(presentation.status),
                            )
                            .children(
                                presentation
                                    .detail
                                    .map(|detail| div().flex_none().child(detail)),
                            ),
                    ),
            );
        activity.mount(content.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(launch: LaunchState, state: UpdateState) -> Option<LaunchPresentation> {
        LaunchPresentation::resolve(launch, &state)
    }

    #[test]
    fn open_gate_should_present_nothing() {
        assert_eq!(resolve(LaunchState::Open, UpdateState::Idle), None);
    }

    #[test]
    fn required_update_should_report_download_size_and_completion() {
        let presentation = resolve(
            LaunchState::Required,
            UpdateState::Downloading {
                version: "0.4.2".to_owned(),
                received: 12_300_000,
                total: 41_000_000,
            },
        )
        .expect("a held launch is presented");
        assert_eq!(presentation.title, "Updating SpaceTerm");
        assert_eq!(presentation.status.as_ref(), "Downloading SpaceTerm 0.4.2…");
        assert_eq!(presentation.detail.as_deref(), Some("12.3 MB of 41.0 MB"));
        assert_eq!(presentation.progress, Some(0.3));
    }

    #[test]
    fn work_without_known_completion_should_stay_indeterminate() {
        let version = || "0.4.2".to_owned();
        for (launch, state) in [
            (LaunchState::Checking, UpdateState::Checking),
            (
                LaunchState::Required,
                UpdateState::Downloading {
                    version: version(),
                    received: 10,
                    total: 0,
                },
            ),
            (
                LaunchState::Required,
                UpdateState::Verifying { version: version() },
            ),
            (
                LaunchState::Installing,
                UpdateState::Ready { version: version() },
            ),
            (
                LaunchState::Installing,
                UpdateState::Installing { version: version() },
            ),
        ] {
            let presentation = resolve(launch, state.clone()).expect("a held launch is presented");
            assert_eq!(presentation.progress, None, "{launch:?} {state:?}");
        }
    }

    #[test]
    fn launch_should_describe_steps_without_transport_or_signature_terms() {
        let version = || "0.4.2".to_owned();
        for state in [
            UpdateState::Available { version: version() },
            UpdateState::Verifying { version: version() },
            UpdateState::Ready { version: version() },
            UpdateState::Installing { version: version() },
            UpdateState::Checking,
        ] {
            let presentation =
                resolve(LaunchState::Required, state.clone()).expect("a held launch is presented");
            let text = format!(
                "{} {:?} {} {:?}",
                presentation.title, presentation.message, presentation.status, presentation.detail
            )
            .to_lowercase();
            for internal in ["signature", "sparkle", "verif", "appcast", "github", "feed"] {
                assert!(!text.contains(internal), "{state:?} mentions {internal}");
            }
        }
    }

    #[test]
    fn size_label_should_use_decimal_units() {
        assert_eq!(size_label(999), "1 KB");
        assert_eq!(size_label(12_345_678), "12.3 MB");
    }
}
