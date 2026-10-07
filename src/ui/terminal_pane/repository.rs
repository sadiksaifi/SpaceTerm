//! A Pane's Repository Status Interest: what it reports to the store, what it presents, and the
//! popover that shows it in full.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, KeyDownEvent, Pixels, Subscription, Window,
    anchored, deferred, div,
};
use spaceterm_ui::{Button, ButtonSize, ButtonVariant, FloatingLayer, FloatingRole};

use super::{PaneTerminalState, TerminalFailure, TerminalPane, TerminalPaneEvent};
use crate::repository_status::presentation::{PopoverPullRequest, RepositoryPopover};
use crate::repository_status::scheduler::{Interest, InterestId, SourceDirectory};
use crate::repository_status::{RemoteMachineKey, RepositoryMachine, RepositoryView};
use crate::terminal::metadata::{CurrentDirectory, MetadataFreshness, TerminalMetadataContext};
use crate::ui::ShowRepositoryStatus;
use crate::ui::appearance::gpui_color;
use crate::ui::chrome_typography::{ChromeTextStyleExt as _, TextRole};
use crate::ui::repository_status_store::{
    InstalledRepositoryStatus, RepositoryStatusStore, RepositoryViewsChanged,
};

/// The popover's width before it narrows to fit its Pane.
const POPOVER_WIDTH: f32 = 320.0;
/// The width of the popover's row labels.
const POPOVER_LABEL_WIDTH: f32 = 72.0;
/// The distance between the popover and the Pane's top and trailing edges, and between the
/// popover and the window's edges.
const POPOVER_INSET: f32 = 8.0;
/// The height the Terminal Find bar takes above the popover while it is open.
const FIND_BAR_CLEARANCE: f32 = 40.0;

#[derive(Default)]
pub(super) struct PaneRepositoryStatus {
    interest: Option<RegisteredInterest>,
    view: RepositoryView,
    popover: Option<OpenPopover>,
}

/// The open Repository Status popover, which holds focus until it closes.
struct OpenPopover {
    focus: FocusHandle,
    _focus_out: Subscription,
}

struct RegisteredInterest {
    store: Entity<RepositoryStatusStore>,
    id: InterestId,
    reported: PaneRepositoryFacts,
    _changes: Subscription,
}

/// Everything a Pane tells the store about its Interest.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PaneRepositoryFacts {
    machine: RepositoryMachine,
    directory: SourceDirectory,
    finished_commands: u64,
    visible: bool,
    available: bool,
    stopped: bool,
}

impl TerminalPane {
    /// The machine and Repository Source Directory this Pane reads, once it knows a directory.
    pub(crate) fn repository_source(&self) -> Option<(RepositoryMachine, SourceDirectory)> {
        self.repository_facts()
            .map(|facts| (facts.machine, facts.directory))
    }

    /// What this Pane's Repository Status presents.
    pub(crate) fn repository_view(&self) -> &RepositoryView {
        &self.repository_status.view
    }

    /// Reports any changed fact to the store, registering on the first known directory.
    pub(super) fn sync_repository_status(&mut self, cx: &mut Context<Self>) {
        let Some(facts) = self.repository_facts() else {
            return;
        };
        let Some(interest) = &mut self.repository_status.interest else {
            self.register_repository_interest(facts, cx);
            return;
        };
        if interest.reported.machine != facts.machine {
            self.release_repository_status(cx);
            self.register_repository_interest(facts, cx);
            return;
        }
        let previous = std::mem::replace(&mut interest.reported, facts.clone());
        let id = interest.id;
        interest.store.update(cx, |store, cx| {
            // Availability and stopping first, so a new directory never reads over a down
            // connection or for an exited Pane.
            if previous.available != facts.available {
                store.set_available(id, facts.available, cx);
            }
            if previous.stopped != facts.stopped {
                store.set_stopped(id, facts.stopped, cx);
            }
            if previous.directory != facts.directory {
                store.set_source(id, facts.directory.clone(), cx);
            }
            if previous.finished_commands != facts.finished_commands {
                store.command_finished(id, facts.finished_commands, cx);
            }
            if previous.visible != facts.visible {
                store.set_visible(id, facts.visible, cx);
            }
        });
    }

    /// Opens the Repository Status popover when the Pane presents Repository Status.
    pub(crate) fn show_repository_status(
        &mut self,
        _: &ShowRepositoryStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(popover) = &self.repository_status.popover {
            popover.focus.focus(window, cx);
            return;
        }
        if RepositoryPopover::from_view(&self.presented_repository_view(cx), Instant::now())
            .is_none()
        {
            return;
        }
        let focus = cx.focus_handle();
        // Focus that moves anywhere else, such as another Pane, closes the popover and stays
        // where it went. Another window taking the keyboard, such as Settings, leaves the popover
        // open and focused for when this window returns.
        let focus_out = cx.on_focus_out(&focus, window, |pane, _, window, cx| {
            if !window.is_window_active() {
                return;
            }
            if pane.repository_status.popover.take().is_some() {
                cx.notify();
            }
        });
        focus.focus(window, cx);
        self.repository_status.popover = Some(OpenPopover {
            focus,
            _focus_out: focus_out,
        });
        self.advance_native_service_focus_epoch();
        let _ = self.sync_terminal_input_focus(window, cx);
        cx.notify();
    }

    /// Closes the popover and returns Terminal Input Focus to the Pane when the popover held it.
    pub(super) fn close_repository_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popover) = self.repository_status.popover.take() else {
            return;
        };
        // A popover that already left the frame is no longer an ancestor of anything, so its own
        // focus is checked directly.
        if popover.focus.is_focused(window) || popover.focus.contains_focused(window, cx) {
            self.focus_handle.focus(window, cx);
        }
        self.advance_native_service_focus_epoch();
        let _ = self.sync_terminal_input_focus(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn present_repository_view(&mut self, view: RepositoryView, cx: &mut Context<Self>) {
        self.repository_status.view = view;
        cx.notify();
    }

    /// The view the popover presents: the Pane's own, or the Developer Workbench fixture.
    fn presented_repository_view(&self, cx: &App) -> RepositoryView {
        #[cfg(feature = "developer-tools")]
        if let Some(view) = crate::ui::developer_workbench::repository_view_fixture(cx) {
            return view;
        }
        let _ = cx;
        self.repository_status.view.clone()
    }

    #[cfg(test)]
    pub(super) fn repository_popover_open(&self) -> bool {
        self.repository_status.popover.is_some()
    }

    pub(super) fn render_repository_popover(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.repository_status.popover.as_ref()?;
        let Some(popover) =
            RepositoryPopover::from_view(&self.presented_repository_view(cx), Instant::now())
        else {
            // The Pane left its repository while the popover was open.
            cx.defer_in(window, |pane, window, cx| {
                pane.close_repository_status(window, cx)
            });
            return None;
        };
        let root = super::compact_home_directory(&popover.root, self.metadata().context.home());
        Some(render_popover(
            popover,
            root,
            open.focus.clone(),
            self.find_input.is_some(),
            window.viewport_size(),
            cx,
        ))
    }

    pub(super) fn release_repository_status(&mut self, cx: &mut App) {
        if let Some(interest) = self.repository_status.interest.take() {
            interest
                .store
                .update(cx, |store, cx| store.unregister(interest.id, cx));
        }
    }

    fn register_repository_interest(&mut self, facts: PaneRepositoryFacts, cx: &mut Context<Self>) {
        let Some(store) = InstalledRepositoryStatus::store(cx) else {
            return;
        };
        let id = store.update(cx, |store, cx| {
            let id = store.register(
                Interest {
                    machine: facts.machine.clone(),
                    directory: facts.directory.clone(),
                    visible: facts.visible,
                    available: facts.available,
                    finished_commands: facts.finished_commands,
                },
                cx,
            );
            if facts.stopped {
                store.set_stopped(id, true, cx);
            }
            id
        });
        let changes = cx.subscribe(&store, |pane, _, event: &RepositoryViewsChanged, cx| {
            if pane
                .repository_status
                .interest
                .as_ref()
                .is_some_and(|interest| event.contains(interest.id))
            {
                pane.refresh_repository_view(cx);
            }
        });
        self.repository_status.interest = Some(RegisteredInterest {
            store,
            id,
            reported: facts,
            _changes: changes,
        });
        self.refresh_repository_view(cx);
    }

    fn refresh_repository_view(&mut self, cx: &mut Context<Self>) {
        let view = self
            .repository_status
            .interest
            .as_ref()
            .map(|interest| interest.store.read(cx).view(interest.id))
            .unwrap_or_default();
        if view != self.repository_status.view {
            self.repository_status.view = view;
            cx.emit(TerminalPaneEvent::CaptionChanged);
            cx.notify();
        }
    }

    /// The facts to report, or `None` before the Pane knows a directory.
    fn repository_facts(&self) -> Option<PaneRepositoryFacts> {
        let metadata = self.terminal_session.metadata.as_deref()?;
        let stale = metadata.freshness == MetadataFreshness::Stale;
        // Stale facts keep the last directory: the Interest stops and presents it as last known.
        let directory = metadata
            .context
            .current_directory(&metadata.repository_directory)?;
        let (machine, directory) = match (&metadata.context, directory) {
            (TerminalMetadataContext::Local { .. }, CurrentDirectory::Local(path)) => {
                (RepositoryMachine::Local, SourceDirectory::Local(path))
            }
            (TerminalMetadataContext::Remote(remote), CurrentDirectory::Remote(directory)) => (
                RepositoryMachine::Remote(RemoteMachineKey::new(remote.destination().as_str())),
                SourceDirectory::Remote(directory.as_str().into()),
            ),
            _ => return None,
        };
        let focus = self.product_focus;
        let exited = matches!(self.pane_state, PaneTerminalState::Exited(_))
            || self
                .pane_state
                .failure()
                .is_some_and(TerminalFailure::is_fatal);
        Some(PaneRepositoryFacts {
            machine,
            directory,
            finished_commands: metadata.finished_commands,
            visible: focus.active_workspace && focus.active_tab && focus.pane_visible,
            available: !self.terminal_session.remote_input_blocked,
            stopped: stale || exited,
        })
    }
}

/// The Repository Status popover: identity, then branch facts, then changes, then freshness. It
/// has no git actions; the Pull Request number is its only control.
fn render_popover(
    popover: RepositoryPopover,
    root: String,
    focus: FocusHandle,
    below_find_bar: bool,
    viewport: gpui::Size<Pixels>,
    cx: &mut Context<TerminalPane>,
) -> AnyElement {
    let appearance = crate::ui::appearance::shared_chrome(cx);
    let shell = spaceterm_ui::floating_surface_theme(cx).shell(FloatingRole::Popover);
    let colors = &appearance.floating_colors;
    let text = if popover.dimmed {
        colors.text_muted
    } else {
        colors.text
    };
    let muted = colors.text_muted;
    let spacing = |value: f32| appearance.spacing(value);
    let divider = || {
        div()
            .flex_shrink_0()
            .my(spacing(6.0))
            .h(shell.hairline())
            .bg(shell.divider())
    };
    let row = |label: &'static str, value: AnyElement| {
        div()
            .flex()
            .items_baseline()
            .min_w_0()
            .child(
                div()
                    .flex_shrink_0()
                    .w(spacing(POPOVER_LABEL_WIDTH))
                    .text_color(gpui_color(muted))
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .min_w_0()
                    .gap(spacing(6.0))
                    .child(value),
            )
    };
    let value = |value: String| div().min_w_0().truncate().child(value).into_any_element();

    let mut facts = div()
        .flex()
        .flex_col()
        .gap(spacing(2.0))
        .child(row("Branch", value(popover.branch)));
    if let Some(operation) = popover.operation {
        facts = facts.child(row("State", value(operation)));
    }
    if let Some(pull_request) = popover.pull_request {
        facts = facts.child(row("PR", pull_request_value(pull_request, &appearance)));
    }
    if let Some(upstream) = popover.upstream {
        let mut upstream_value = div()
            .flex()
            .items_baseline()
            .min_w_0()
            .gap(spacing(6.0))
            .child(div().min_w_0().truncate().child(upstream.name));
        if let Some(detail) = upstream.detail {
            upstream_value = upstream_value.child(div().flex_shrink_0().child(detail));
        }
        facts = facts.child(row("Upstream", upstream_value.into_any_element()));
    }
    if let Some(commit) = popover.commit {
        facts = facts.child(row("Commit", value(commit)));
    }

    let mut surface = div()
        .id("repository-status-popover")
        .debug_selector(|| "repository-status-popover".to_owned())
        .role(gpui::accesskit::Role::Dialog)
        .aria_label("Repository Status")
        .track_focus(&focus)
        .w(spacing(POPOVER_WIDTH))
        .max_w(viewport.width - spacing(POPOVER_INSET) * 2.0)
        .max_h(viewport.height - spacing(POPOVER_INSET) * 2.0)
        .p(spacing(10.0))
        .flex()
        .flex_col()
        .chrome_text(appearance.typography.style(TextRole::Body))
        .text_color(gpui_color(text))
        .block_mouse_except_scroll()
        // The popover scrolls its own body; the terminal or a Pane beneath it never scrolls too.
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .on_key_down(cx.listener(|pane, event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                pane.close_repository_status(window, cx);
                cx.stop_propagation();
            }
        }))
        .on_mouse_down_out(cx.listener(|pane, _, window, cx| {
            pane.close_repository_status(window, cx);
        }));
    // Everything above the footer scrolls as one, so a short Pane keeps every fact reachable and
    // the freshness footer in view.
    let mut body = div()
        .id("repository-status-body")
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_shrink_0()
                .chrome_text(appearance.typography.style(TextRole::BodyEmphasis))
                .truncate()
                .child(popover.name),
        )
        .child(
            div()
                .flex_shrink_0()
                .chrome_text(appearance.typography.style(TextRole::Secondary))
                .text_color(gpui_color(muted))
                .truncate()
                .child(root),
        )
        .child(divider())
        .child(facts.flex_shrink_0());
    if let Some(changes) = popover.changes {
        let mut list = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(spacing(2.0))
            .child(row("Changes", value(changes.summary)));
        for (index, change) in changes.entries.into_iter().enumerate() {
            list = list.child(
                div()
                    .id(("repository-status-change", index))
                    .flex()
                    .items_baseline()
                    .min_w_0()
                    .pl(spacing(12.0))
                    .aria_label(gpui::SharedString::from(format!(
                        "{}: {}",
                        change.kind, change.path
                    )))
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(spacing(18.0))
                            .text_color(gpui_color(muted))
                            .child(change.letter),
                    )
                    .child(div().min_w_0().truncate().child(change.path)),
            );
        }
        if let Some(more) = changes.more {
            list = list.child(
                div()
                    .pl(spacing(12.0))
                    .text_color(gpui_color(muted))
                    .child(more),
            );
        }
        body = body.child(divider()).child(list);
    }
    surface = surface.child(body).child(divider()).child(
        div()
            .flex_shrink_0()
            .debug_selector(|| "repository-status-popover-footer".to_owned())
            .chrome_text(appearance.typography.style(TextRole::Secondary))
            .text_color(gpui_color(muted))
            .child(popover.footer),
    );
    // The popover hangs from the Pane's top trailing corner and may extend past a short or narrow
    // Pane; the window's edges bound it instead.
    let placement = anchored()
        .anchor(gpui::Anchor::TopRight)
        .snap_to_window_with_margin(spacing(POPOVER_INSET))
        .child(shell.mount(surface));
    div()
        .absolute()
        .top(spacing(
            POPOVER_INSET
                + if below_find_bar {
                    FIND_BAR_CLEARANCE
                } else {
                    0.0
                },
        ))
        .right(spacing(POPOVER_INSET))
        .size_0()
        .child(match shell.layer(false) {
            FloatingLayer::Deferred(priority) => deferred(placement)
                .with_priority(priority)
                .into_any_element(),
            FloatingLayer::Normal => placement.into_any_element(),
        })
        .into_any_element()
}

/// The Pull Request number, which opens the Pull Request in the browser, and its state in words.
fn pull_request_value(
    pull_request: PopoverPullRequest,
    appearance: &crate::ui::appearance::ChromeAppearance,
) -> AnyElement {
    let paint = appearance
        .floating_colors
        .pull_request(appearance.floating_colors.background);
    let color = match pull_request.state {
        crate::repository_status::presentation::PullRequestState::Open => paint.open,
        crate::repository_status::presentation::PullRequestState::Draft => paint.draft,
    };
    let url = pull_request.url.clone();
    div()
        .flex()
        .items_baseline()
        .min_w_0()
        .gap(appearance.spacing(6.0))
        .child(
            Button::new(
                "repository-status-pull-request",
                pull_request.number.clone(),
            )
            .variant(ButtonVariant::Link)
            .size(ButtonSize::Compact)
            .debug_selector("repository-status-pull-request".to_owned())
            .accessibility_description(gpui::SharedString::from(format!(
                "Open Pull Request {}: {}",
                pull_request.number, pull_request.title
            )))
            .on_activate(move |_, _, cx| cx.open_url(&url)),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(gpui_color(color))
                .child(pull_request.state.label()),
        )
        .child(div().min_w_0().truncate().child(pull_request.title))
        .into_any_element()
}
