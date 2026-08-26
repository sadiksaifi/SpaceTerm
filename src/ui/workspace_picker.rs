use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    Action, App, Bounds, Context, Corner, Entity, EventEmitter, KeyBinding, KeyDownEvent,
    ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    PromptButton, PromptLevel, Render, ScrollWheelEvent, WeakFocusHandle, Window, actions,
    anchored, canvas, div, list, px,
};
use gpui_symbols::{Icon, SymbolWeight};
use spaceterm_ui::{
    Button, ButtonSize, ButtonVariant, IconButton, OverlayScrollbar, OverlayScrollbarEvent,
    ScrollMetrics, TextInput, TextInputEvent, TextInputTabBehavior, TextInputVariant,
};

use super::{
    ActivateWindow1, ActivateWindow2, ActivateWindow3, ActivateWindow4, ActivateWindow5,
    ActivateWindow6, ActivateWindow7, ActivateWindow8, ActivateWindow9, ActivateWorkspace1,
    ActivateWorkspace2, ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5,
    ActivateWorkspace6, ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane,
    CloseTerminalFind, CloseWindow, CloseWorkspace, CopySelection, CreateWindow, CreateWorkspace,
    FindNext, FindPrevious, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp,
    OpenTerminalFind, SearchWorkspaces, SplitDown, SplitRight, TogglePaneZoom, ToggleSidebar,
    ToggleSidebarFocus,
};
use crate::domain::ValidatedWorkspaceDirectory;
use crate::platform::macos_system_settings::SystemSettingsOpener;
use crate::platform::workspace_picker_filesystem::{
    WorkspacePickerDirectoryEntry, WorkspacePickerExactPathProbe, WorkspacePickerFilesystem,
    WorkspacePickerFilesystemError,
};
use crate::theme::{ACTIVE_THEME, Color};

const KEY_CONTEXT: &str = "WorkspacePicker";
const PANEL_MAX_WIDTH: f32 = 680.0;
const PANEL_MAX_HEIGHT: f32 = 500.0;
const WINDOW_INSET: f32 = 16.0;
const ROW_HEIGHT: f32 = 34.0;

actions!(
    workspace_picker,
    [
        PickerMoveUp,
        PickerMoveDown,
        PickerConfirmTyped,
        PickerDismiss,
        PickerFocusNext,
        PickerFocusPrevious
    ]
);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PickerMoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-p", PickerMoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", PickerMoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", PickerMoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-enter", PickerConfirmTyped, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", PickerDismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("tab", PickerFocusNext, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", PickerFocusPrevious, Some(KEY_CONTEXT)),
    ]);
}

fn block_parent_action<A: Action>(_: &A, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspacePathFormatError {
    Relative,
    BareTilde,
    UnsupportedTilde,
}

impl WorkspacePathFormatError {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::Relative => "Enter an absolute path beginning with / or ~/.",
            Self::BareTilde => "Use ~/ to open your home folder.",
            Self::UnsupportedTilde => "Only ~/ is supported for home-relative paths.",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedWorkspacePath {
    display: String,
    exact_path: PathBuf,
    enumeration_directory: PathBuf,
    leaf_filter: String,
    trailing_separator: bool,
}

impl ParsedWorkspacePath {
    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) fn exact_path(&self) -> &Path {
        &self.exact_path
    }

    pub(super) fn enumeration_directory(&self) -> &Path {
        &self.enumeration_directory
    }

    pub(super) fn leaf_filter(&self) -> &str {
        &self.leaf_filter
    }

    #[cfg(test)]
    pub(super) const fn trailing_separator(&self) -> bool {
        self.trailing_separator
    }

    pub(super) fn reveals_dot_directories(&self) -> bool {
        self.leaf_filter.starts_with('.')
    }
}

pub(super) fn parse_workspace_path(
    input: &str,
    home: &Path,
) -> Result<ParsedWorkspacePath, WorkspacePathFormatError> {
    if input == "~" {
        return Err(WorkspacePathFormatError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with("~/") {
        return Err(WorkspacePathFormatError::UnsupportedTilde);
    }
    if !input.starts_with('/') && !input.starts_with("~/") {
        return Err(WorkspacePathFormatError::Relative);
    }

    let exact_path = expand_workspace_path(input, home);
    let trailing_separator = input.ends_with('/');
    let (enumeration_directory, leaf_filter) = if trailing_separator {
        (exact_path.clone(), String::new())
    } else {
        let separator = input.rfind('/').ok_or(WorkspacePathFormatError::Relative)?;
        let display_directory = &input[..=separator];
        (
            expand_workspace_path(display_directory, home),
            input[separator + 1..].to_owned(),
        )
    };

    Ok(ParsedWorkspacePath {
        display: input.to_owned(),
        exact_path,
        enumeration_directory,
        leaf_filter,
        trailing_separator,
    })
}

fn expand_workspace_path(display: &str, home: &Path) -> PathBuf {
    match display.strip_prefix("~/") {
        Some("") => home.to_path_buf(),
        Some(remainder) => home.join(remainder.trim_start_matches('/')),
        None => PathBuf::from(display),
    }
}

fn display_workspace_directory_with_style(
    path: &Path,
    home: &Path,
    prefer_tilde: bool,
) -> Option<String> {
    if prefer_tilde && path == home {
        return Some("~/".to_owned());
    }
    if prefer_tilde && let Ok(relative) = path.strip_prefix(home) {
        let relative = relative.to_str()?;
        return Some(format!("~/{relative}/"));
    }
    let path = path.to_str()?;
    Some(if path == "/" {
        "/".to_owned()
    } else {
        format!("{}/", path.trim_end_matches('/'))
    })
}

pub(super) fn parent_workspace_directory(
    parsed: &ParsedWorkspacePath,
    home: &Path,
) -> Option<(PathBuf, String)> {
    let parent = parsed.exact_path.parent()?.to_path_buf();
    let display =
        display_workspace_directory_with_style(&parent, home, parsed.display.starts_with("~/"))?;
    Some((parent, display))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkspacePickerRow {
    Parent { path: PathBuf },
    Directory(WorkspacePickerDirectoryEntry),
}

impl WorkspacePickerRow {
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::Parent { path } => path,
            Self::Directory(entry) => entry.path(),
        }
    }

    pub(super) fn name(&self) -> &str {
        match self {
            Self::Parent { .. } => "..",
            Self::Directory(entry) => entry.name(),
        }
    }
}

pub(super) fn filter_workspace_picker_rows(
    parsed: &ParsedWorkspacePath,
    entries: &[WorkspacePickerDirectoryEntry],
) -> Vec<WorkspacePickerRow> {
    let folded_filter = parsed.leaf_filter.to_lowercase();
    let mut directories = entries
        .iter()
        .filter(|entry| entry.name().to_lowercase().starts_with(&folded_filter))
        .cloned()
        .collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        let folded = left.name().to_lowercase().cmp(&right.name().to_lowercase());
        if folded == Ordering::Equal {
            left.name().cmp(right.name())
        } else {
            folded
        }
    });

    let mut rows = Vec::with_capacity(directories.len().saturating_add(1));
    if let Some(parent) = parsed.enumeration_directory.parent() {
        rows.push(WorkspacePickerRow::Parent {
            path: parent.to_path_buf(),
        });
    }
    rows.extend(directories.into_iter().map(WorkspacePickerRow::Directory));
    rows
}

pub(super) fn repair_workspace_picker_selection(
    previous: Option<&Path>,
    rows: &[WorkspacePickerRow],
) -> Option<PathBuf> {
    if let Some(previous) = previous
        && rows.iter().any(|row| row.path() == previous)
    {
        return Some(previous.to_path_buf());
    }
    rows.iter().find_map(|row| match row {
        WorkspacePickerRow::Parent { .. } => None,
        WorkspacePickerRow::Directory(entry) => Some(entry.path().to_path_buf()),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkspacePickerEvent {
    StateChanged,
    ScrimDismissed,
    FinderRequested,
    Confirmed(ValidatedWorkspaceDirectory),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspacePickerStatus {
    Loading,
    Readable,
    Missing,
    NotDirectory,
    PermissionDenied,
    Other,
    Invalid(WorkspacePathFormatError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspacePickerBusy {
    Finder,
    CreationPrompt,
    Creating,
    Validating,
    AwaitingActivation,
}

#[derive(Clone)]
struct LoadedDirectorySnapshot {
    directory: PathBuf,
    hide_dot_prefixed: bool,
    entries: Vec<WorkspacePickerDirectoryEntry>,
}

struct RefreshCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    parsed: ParsedWorkspacePath,
    hide_dot_prefixed: bool,
    listing: Option<Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError>>,
    probe: WorkspacePickerExactPathProbe,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ValidationKind {
    Typed,
    Finder,
    Creation,
}

struct ValidationCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    kind: ValidationKind,
    expected_input_path: Option<PathBuf>,
    result: Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError>,
}

pub(super) struct WorkspacePicker {
    home: PathBuf,
    filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync>,
    system_settings: Rc<dyn SystemSettingsOpener>,
    input: Entity<TextInput>,
    focus_scope: gpui::FocusHandle,
    list: ListState,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    open: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    parsed: Option<ParsedWorkspacePath>,
    snapshot: Option<LoadedDirectorySnapshot>,
    rows: Vec<WorkspacePickerRow>,
    selected_path: Option<PathBuf>,
    status: WorkspacePickerStatus,
    busy: Option<WorkspacePickerBusy>,
    restore_focus: Option<WeakFocusHandle>,
}

impl EventEmitter<WorkspacePickerEvent> for WorkspacePicker {}

impl WorkspacePicker {
    pub(super) fn new(
        home: PathBuf,
        filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync>,
        system_settings: Rc<dyn SystemSettingsOpener>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new("workspace-picker-path", "Workspace path", "~/", window, cx)
                .variant(TextInputVariant::Bare)
                .tab_behavior(TextInputTabBehavior::Propagate)
                .debug_selector("workspace-picker-path-input")
        });
        cx.subscribe_in(
            &input,
            window,
            |picker, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    let value = input.read(cx).value().to_owned();
                    picker.refresh_for_input(value, window, cx);
                }
                TextInputEvent::Submitted => picker.submit(window, cx),
                TextInputEvent::Cancelled => {
                    picker.dismiss(window, cx);
                }
                TextInputEvent::TabForwardRequested => picker.descend_selected(window, cx),
                TextInputEvent::TabBackwardRequested => window.focus_prev(),
                _ => {}
            },
        )
        .detach();

        let scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("workspace-picker-scrollbar"));
        cx.subscribe_in(
            &scrollbar,
            window,
            |picker, _, event: &OverlayScrollbarEvent<f32>, _, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => picker.list.scrollbar_drag_started(),
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    picker
                        .list
                        .set_offset_from_scrollbar(gpui::point(px(0.0), px(-*offset)));
                    cx.notify();
                }
            },
        )
        .detach();

        cx.observe_window_activation(window, |picker, window, cx| {
            if picker.open
                && window.is_window_active()
                && !matches!(
                    picker.busy,
                    Some(WorkspacePickerBusy::Finder | WorkspacePickerBusy::CreationPrompt)
                )
            {
                picker.input.read(cx).focus_handle().focus(window);
            }
        })
        .detach();

        Self {
            home,
            filesystem,
            system_settings,
            input,
            focus_scope: cx.focus_handle(),
            list: ListState::new(0, ListAlignment::Top, px(0.0)).measure_all(),
            scrollbar,
            open: false,
            lifecycle_generation: 0,
            operation_generation: 0,
            parsed: None,
            snapshot: None,
            rows: Vec::new(),
            selected_path: None,
            status: WorkspacePickerStatus::Loading,
            busy: None,
            restore_focus: None,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.open {
            self.refocus_path(window, cx);
            return false;
        }
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.restore_focus = window.focused(cx).map(|focus| focus.downgrade());
        self.open = true;
        self.busy = None;
        self.snapshot = None;
        self.rows.clear();
        self.selected_path = None;
        self.list.reset(0);
        self.input.update(cx, |input, cx| {
            input.set_editable(true, cx);
            input.set_value("~/", cx)
        });
        self.input.read(cx).focus_handle().focus(window);
        self.refresh_for_input("~/".to_owned(), window, cx);
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
        true
    }

    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    pub(super) const fn blocks_terminal_input(&self) -> bool {
        self.open
    }

    pub(super) fn refocus_path(&self, window: &mut Window, cx: &App) {
        if self.open {
            self.input.read(cx).focus_handle().focus(window);
        }
    }

    #[cfg(test)]
    pub(super) fn path_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle().is_focused(window)
    }

    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.open || self.busy.is_some() {
            return false;
        }
        self.close_and_restore_focus(window, cx)
    }

    fn dismiss_from_scrim(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.dismiss(window, cx) {
            return false;
        }
        cx.emit(WorkspacePickerEvent::ScrimDismissed);
        true
    }

    pub(super) fn finder_cancelled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(WorkspacePickerBusy::Finder) {
            self.busy = None;
            self.set_input_editable_deferred(true, window, cx);
            self.refocus_path(window, cx);
            cx.emit(WorkspacePickerEvent::StateChanged);
            cx.notify();
        }
    }

    pub(super) fn finder_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(WorkspacePickerBusy::Finder) {
            self.busy = None;
            self.status = WorkspacePickerStatus::Other;
            self.set_input_editable_deferred(true, window, cx);
            self.refocus_path(window, cx);
            cx.emit(WorkspacePickerEvent::StateChanged);
            cx.notify();
        }
    }

    pub(super) fn validate_finder_selection(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open && self.busy == Some(WorkspacePickerBusy::Finder) {
            self.bind_finder_selection_to_input(&path, cx);
            self.start_validation(path, ValidationKind::Finder, window, cx);
        }
    }

    fn bind_finder_selection_to_input(&mut self, path: &Path, cx: &mut Context<Self>) {
        let prefer_tilde = self
            .parsed
            .as_ref()
            .is_some_and(|parsed| parsed.display().starts_with("~/"));
        let Some(display) = self.representable_directory_display(path, prefer_tilde, cx) else {
            return;
        };
        let Ok(parsed) = parse_workspace_path(&display, &self.home) else {
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value(display, cx));
        self.parsed = Some(parsed);
    }

    fn representable_directory_display(
        &self,
        path: &Path,
        prefer_tilde: bool,
        cx: &App,
    ) -> Option<String> {
        let display = display_workspace_directory_with_style(path, &self.home, prefer_tilde)?;
        self.input
            .read(cx)
            .can_set_value_exactly(&display)
            .then_some(display)
    }

    pub(super) fn complete_activation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open || self.busy != Some(WorkspacePickerBusy::AwaitingActivation) {
            return false;
        }
        self.busy = None;
        self.restore_focus = None;
        self.close_and_restore_focus(window, cx)
    }

    pub(super) fn activation_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(WorkspacePickerBusy::AwaitingActivation) {
            self.busy = None;
            self.status = WorkspacePickerStatus::Other;
            self.set_input_editable_deferred(true, window, cx);
            self.refocus_path(window, cx);
            cx.emit(WorkspacePickerEvent::StateChanged);
            cx.notify();
        }
    }

    fn close_and_restore_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open = false;
        self.set_input_editable_deferred(true, window, cx);
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.busy = None;
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        if let Some(focus) = self.restore_focus.take().and_then(|focus| focus.upgrade()) {
            focus.focus(window);
        }
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
        true
    }

    fn refresh_for_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() {
            return;
        }
        let parsed = match parse_workspace_path(&value, &self.home) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.parsed = None;
                self.status = WorkspacePickerStatus::Invalid(error);
                self.operation_generation = self.operation_generation.wrapping_add(1);
                cx.emit(WorkspacePickerEvent::StateChanged);
                cx.notify();
                return;
            }
        };
        let hide_dot_prefixed = !parsed.reveals_dot_directories();
        let listing_needed = !self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.directory == parsed.enumeration_directory
                && snapshot.hide_dot_prefixed == hide_dot_prefixed
        });
        self.parsed = Some(parsed.clone());
        if !listing_needed {
            self.rebuild_rows();
        }
        self.status = WorkspacePickerStatus::Loading;
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let filesystem = Arc::clone(&self.filesystem);
        let request = parsed.clone();
        let background = cx.background_spawn(async move {
            let listing = listing_needed.then(|| {
                filesystem.list_directories(request.enumeration_directory(), hide_dot_prefixed)
            });
            let probe = filesystem.probe_exact_path(request.exact_path());
            RefreshCompletion {
                lifecycle_generation,
                operation_generation,
                parsed: request,
                hide_dot_prefixed,
                listing,
                probe,
            }
        });
        cx.spawn_in(window, async move |picker, cx| {
            let completion = background.await;
            let _ = picker.update_in(cx, |picker, _, cx| {
                picker.finish_refresh(completion, cx);
            });
        })
        .detach();
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    fn finish_refresh(&mut self, completion: RefreshCompletion, cx: &mut Context<Self>) {
        let Some(current) = self.parsed.as_ref() else {
            return;
        };
        if !self.open
            || self.lifecycle_generation != completion.lifecycle_generation
            || self.operation_generation != completion.operation_generation
            || current.enumeration_directory() != completion.parsed.enumeration_directory()
            || current.exact_path() != completion.parsed.exact_path()
        {
            return;
        }

        let listing_error = match completion.listing {
            Some(Ok(mut entries)) => {
                entries.retain(|entry| self.input.read(cx).can_set_value_exactly(entry.name()));
                self.snapshot = Some(LoadedDirectorySnapshot {
                    directory: completion.parsed.enumeration_directory.clone(),
                    hide_dot_prefixed: completion.hide_dot_prefixed,
                    entries,
                });
                self.rebuild_rows();
                None
            }
            Some(Err(error)) => Some(error),
            None => None,
        };
        self.status = match listing_error {
            Some(WorkspacePickerFilesystemError::PermissionDenied) => {
                WorkspacePickerStatus::PermissionDenied
            }
            Some(WorkspacePickerFilesystemError::NotDirectory) => {
                WorkspacePickerStatus::NotDirectory
            }
            Some(WorkspacePickerFilesystemError::Other) => WorkspacePickerStatus::Other,
            Some(WorkspacePickerFilesystemError::Missing) | None => match completion.probe {
                WorkspacePickerExactPathProbe::ReadableDirectory => WorkspacePickerStatus::Readable,
                WorkspacePickerExactPathProbe::Unavailable(error) => status_for_error(error),
            },
        };
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let (Some(parsed), Some(snapshot)) = (self.parsed.as_ref(), self.snapshot.as_ref()) else {
            return;
        };
        if snapshot.directory != parsed.enumeration_directory
            || snapshot.hide_dot_prefixed == parsed.reveals_dot_directories()
        {
            return;
        }
        self.rows = filter_workspace_picker_rows(parsed, &snapshot.entries);
        self.selected_path =
            repair_workspace_picker_selection(self.selected_path.as_deref(), &self.rows);
        self.list.reset(self.rows.len());
        self.reveal_selected();
    }

    fn reveal_selected(&mut self) {
        if let Some(index) = self.selected_index() {
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected_path.as_ref()?;
        self.rows.iter().position(|row| row.path() == selected)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.selected_index() {
            Some(index) => index.saturating_add_signed(delta).min(self.rows.len() - 1),
            None if delta < 0 => self.rows.len() - 1,
            None => 0,
        };
        self.selected_path = Some(self.rows[next].path().to_path_buf());
        self.list.scroll_to_reveal_item(next);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_path.is_some() {
            self.descend_selected(window, cx);
        } else {
            self.confirm_typed_path(window, cx);
        }
    }

    fn descend_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected_path.clone() else {
            return;
        };
        self.descend_to(path, window, cx);
    }

    fn descend_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let prefer_tilde = self
            .parsed
            .as_ref()
            .is_some_and(|parsed| parsed.display().starts_with("~/"));
        let Some(display) = self.representable_directory_display(&path, prefer_tilde, cx) else {
            self.status = WorkspacePickerStatus::Other;
            cx.notify();
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value(display.clone(), cx));
        self.refresh_for_input(display, window, cx);
        self.refocus_path(window, cx);
    }

    fn navigate_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(parsed) = self.parsed.as_ref() else {
            return false;
        };
        let Some((path, _)) = parent_workspace_directory(parsed, &self.home) else {
            return false;
        };
        self.descend_to(path, window, cx);
        true
    }

    fn confirm_typed_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(parsed) = self.parsed.clone() else {
            return;
        };
        match self.status {
            WorkspacePickerStatus::Readable => {
                self.start_validation(parsed.exact_path, ValidationKind::Typed, window, cx);
            }
            WorkspacePickerStatus::Missing => {
                self.prompt_for_creation(parsed, window, cx);
            }
            _ => {}
        }
    }

    fn prompt_for_creation(
        &mut self,
        parsed: ParsedWorkspacePath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(WorkspacePickerBusy::CreationPrompt);
        self.set_input_editable_deferred(false, window, cx);
        let detail = format!(
            "Create {}? Missing parent folders will also be created.",
            parsed.display()
        );
        let response = window.prompt(
            PromptLevel::Info,
            "Create this folder?",
            Some(&detail),
            &[
                PromptButton::ok("Create & Add"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |picker, cx| {
            let answer = response.await.ok();
            let _ = picker.update_in(cx, |picker, window, cx| {
                if !picker.open
                    || picker.busy != Some(WorkspacePickerBusy::CreationPrompt)
                    || picker.parsed.as_ref().map(ParsedWorkspacePath::exact_path)
                        != Some(parsed.exact_path())
                {
                    return;
                }
                if answer == Some(0) {
                    picker.start_validation(
                        parsed.exact_path,
                        ValidationKind::Creation,
                        window,
                        cx,
                    );
                } else {
                    picker.busy = None;
                    picker.set_input_editable_deferred(true, window, cx);
                    picker.refocus_path(window, cx);
                    cx.emit(WorkspacePickerEvent::StateChanged);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    fn start_validation(
        &mut self,
        path: PathBuf,
        kind: ValidationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        self.busy = Some(match kind {
            ValidationKind::Creation => WorkspacePickerBusy::Creating,
            ValidationKind::Typed | ValidationKind::Finder => WorkspacePickerBusy::Validating,
        });
        self.set_input_editable_deferred(false, window, cx);
        let expected_input_path = match kind {
            ValidationKind::Typed | ValidationKind::Creation => Some(path.clone()),
            ValidationKind::Finder => self
                .parsed
                .as_ref()
                .filter(|parsed| parsed.exact_path() == path)
                .map(|_| path.clone()),
        };
        let filesystem = Arc::clone(&self.filesystem);
        let background = cx.background_spawn(async move {
            let result = if matches!(kind, ValidationKind::Creation) {
                filesystem
                    .create_dir_all(&path)
                    .and_then(|()| filesystem.validate_workspace_directory(&path))
            } else {
                filesystem.validate_workspace_directory(&path)
            };
            ValidationCompletion {
                lifecycle_generation,
                operation_generation,
                kind,
                expected_input_path,
                result,
            }
        });
        cx.spawn_in(window, async move |picker, cx| {
            let completion = background.await;
            let _ = picker.update_in(cx, |picker, _, cx| {
                picker.finish_validation(completion, cx);
            });
        })
        .detach();
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    fn finish_validation(&mut self, completion: ValidationCompletion, cx: &mut Context<Self>) {
        if !self.open
            || self.lifecycle_generation != completion.lifecycle_generation
            || self.operation_generation != completion.operation_generation
            || completion
                .expected_input_path
                .as_deref()
                .is_some_and(|expected| {
                    self.parsed.as_ref().map(ParsedWorkspacePath::exact_path) != Some(expected)
                })
        {
            return;
        }
        match completion.result {
            Ok(directory) => {
                self.busy = Some(WorkspacePickerBusy::AwaitingActivation);
                cx.emit(WorkspacePickerEvent::Confirmed(directory));
            }
            Err(error) => {
                self.busy = None;
                self.status = if completion.kind == ValidationKind::Finder
                    && completion.expected_input_path.is_none()
                {
                    WorkspacePickerStatus::Other
                } else {
                    status_for_error(error)
                };
                self.input
                    .update(cx, |input, cx| input.set_editable(true, cx));
                cx.emit(WorkspacePickerEvent::StateChanged);
            }
        }
        cx.notify();
    }

    fn request_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy.is_none() {
            self.busy = Some(WorkspacePickerBusy::Finder);
            self.set_input_editable_deferred(false, window, cx);
            cx.emit(WorkspacePickerEvent::FinderRequested);
            cx.emit(WorkspacePickerEvent::StateChanged);
            cx.notify();
        }
    }

    fn set_input_editable_deferred(
        &self,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.input.clone();
        window.defer(cx, move |_, cx| {
            input.update(cx, |input, cx| input.set_editable(editable, cx));
        });
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy.is_none() {
            let value = self.input.read(cx).value().to_owned();
            self.snapshot = None;
            self.refresh_for_input(value, window, cx);
        }
    }

    fn open_system_settings(&mut self, cx: &mut Context<Self>) {
        if self.system_settings.open_files_and_folders().is_err() {
            self.status = WorkspacePickerStatus::Other;
            cx.notify();
        }
    }

    fn boundary_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.input.read(cx).focus_handle().is_focused(window)
            || event.keystroke.modifiers.modified()
        {
            return false;
        }
        let (selection, value_len) = {
            let input = self.input.read(cx);
            (input.selection(), input.value().len())
        };
        let should_navigate = if event.keystroke.key == "left" {
            selection.is_empty() && selection.caret() == 0
        } else if event.keystroke.key == "backspace" {
            selection.is_empty()
                && selection.caret() == value_len
                && self
                    .parsed
                    .as_ref()
                    .is_some_and(|parsed| parsed.leaf_filter().is_empty())
        } else {
            false
        };
        should_navigate && self.navigate_parent(window, cx)
    }

    fn can_confirm(&self) -> bool {
        self.busy.is_none()
            && matches!(
                self.status,
                WorkspacePickerStatus::Readable | WorkspacePickerStatus::Missing
            )
    }

    fn confirmation_label(&self) -> &'static str {
        if self.status == WorkspacePickerStatus::Missing {
            "Create & Add"
        } else {
            "Add"
        }
    }

    fn inline_message(&self) -> Option<(&'static str, Option<&'static str>)> {
        match self.status {
            WorkspacePickerStatus::Missing => Some((
                "No such folder.",
                Some("Missing parent folders will also be created."),
            )),
            WorkspacePickerStatus::NotDirectory => Some(("Not a folder.", None)),
            WorkspacePickerStatus::Other => None,
            WorkspacePickerStatus::Invalid(error) => Some((error.message(), None)),
            _ => None,
        }
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height = f32::from(self.list.viewport_bounds().size.height);
        let maximum_offset = f32::from(self.list.max_offset_for_scrollbar().height);
        let offset = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        ScrollMetrics::for_pixels(0.0, track_height, maximum_offset, offset)
    }

    fn render_add_button(
        &self,
        id: &'static str,
        selector: &'static str,
        picker: gpui::WeakEntity<Self>,
    ) -> Button {
        Button::new(id, self.confirmation_label())
            .variant(ButtonVariant::Primary)
            .size(ButtonSize::Small)
            .disabled(!self.can_confirm())
            .tab_stop(true)
            .debug_selector(selector)
            .trailing(|foreground| {
                div()
                    .text_size(px(10.0))
                    .text_color(foreground)
                    .child("⌘↵")
                    .into_any_element()
            })
            .on_activate(move |_, window, cx| {
                let _ = picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
            })
    }

    fn render_rows(&self, height: Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows: Rc<[WorkspacePickerRow]> = self.rows.clone().into();
        let selected = self.selected_path.clone();
        let picker = cx.entity().downgrade();
        div()
            .relative()
            .size_full()
            .child(
                list(self.list.clone(), move |index, _, _| {
                    let Some(row) = rows.get(index).cloned() else {
                        return div().into_any_element();
                    };
                    let path = row.path().to_path_buf();
                    let selected = selected.as_deref() == Some(path.as_path());
                    let click_picker = picker.clone();
                    div()
                        .id(("workspace-picker-row", index))
                        .debug_selector(move || format!("workspace-picker-row-{index}"))
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(5.0))
                        .cursor_default()
                        .when(selected, |row| {
                            row.bg(gpui_color(ACTIVE_THEME.element_selected))
                        })
                        .child(
                            Icon::new(if matches!(row, WorkspacePickerRow::Parent { .. }) {
                                "arrow.up"
                            } else {
                                "folder"
                            })
                            .size(px(13.0))
                            .color(gpui_color(ACTIVE_THEME.icon_muted)),
                        )
                        .child(format!(
                            "{}{}",
                            row.name(),
                            if matches!(row, WorkspacePickerRow::Directory(_)) {
                                "/"
                            } else {
                                ""
                            }
                        ))
                        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                            window.prevent_default();
                            let path = path.clone();
                            let _ = click_picker.update(cx, |picker, cx| {
                                picker.selected_path = Some(path.clone());
                                picker.reveal_selected();
                                if event.click_count >= 2 {
                                    picker.descend_to(path, window, cx);
                                } else {
                                    cx.notify();
                                }
                            });
                            cx.stop_propagation();
                        })
                        .into_any_element()
                })
                .h(height)
                .w_full(),
            )
            .child(self.scrollbar.clone())
            .into_any_element()
    }

    fn render_read_failure_body(&self) -> gpui::AnyElement {
        div()
            .debug_selector(|| "workspace-picker-read-failure".to_owned())
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("SpaceTerm couldn’t read this folder")
            .into_any_element()
    }

    fn render_permission_body(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let retry_picker = cx.entity().downgrade();
        let settings_picker = retry_picker.clone();
        div()
            .debug_selector(|| "workspace-picker-permission".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child("SpaceTerm needs permission to read this folder")
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("workspace-picker-retry", "Retry")
                            .tab_stop(true)
                            .on_activate(move |_, window, cx| {
                                let _ =
                                    retry_picker.update(cx, |picker, cx| picker.retry(window, cx));
                            }),
                    )
                    .child(
                        Button::new(
                            "workspace-picker-open-system-settings",
                            "Open System Settings",
                        )
                        .tab_stop(true)
                        .on_activate(move |_, _, cx| {
                            let _ = settings_picker
                                .update(cx, |picker, cx| picker.open_system_settings(cx));
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_panel(
        &self,
        width: Pixels,
        height: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let picker = cx.entity().downgrade();
        let back_picker = picker.clone();
        let finder_picker = picker.clone();
        let capture_picker = picker.clone();
        let list_height = (height - px(166.0)).max(px(0.0));
        let inline_message = self.inline_message();
        let body = match self.status {
            WorkspacePickerStatus::PermissionDenied => self.render_permission_body(cx),
            WorkspacePickerStatus::Other => self.render_read_failure_body(),
            _ => self.render_rows(list_height, cx),
        };
        let back_disabled = self
            .parsed
            .as_ref()
            .and_then(|parsed| parent_workspace_directory(parsed, &self.home))
            .is_none();

        div()
            .debug_selector(|| "workspace-picker-panel".to_owned())
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(gpui_color(ACTIVE_THEME.border_variant))
            .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
            .text_color(gpui_color(ACTIVE_THEME.text))
            .text_size(px(13.0))
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_b_1()
                    .border_color(gpui_color(ACTIVE_THEME.border_variant))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_2()
                            .capture_key_down(move |event, window, cx| {
                                let consumed = capture_picker
                                    .update(cx, |picker, cx| {
                                        picker.boundary_key_down(event, window, cx)
                                    })
                                    .unwrap_or(false);
                                if consumed {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                }
                            })
                            .child(
                                IconButton::new(
                                    "workspace-picker-back",
                                    "Parent folder",
                                    |foreground| {
                                        Icon::new("chevron.left")
                                            .weight(SymbolWeight::Semibold)
                                            .size(px(14.0))
                                            .color(foreground)
                                            .into_any_element()
                                    },
                                )
                                .disabled(back_disabled)
                                .tab_stop(true)
                                .debug_selector("workspace-picker-back")
                                .on_activate(
                                    move |_, window, cx| {
                                        let _ = back_picker.update(cx, |picker, cx| {
                                            picker.navigate_parent(window, cx)
                                        });
                                    },
                                ),
                            )
                            .child(div().min_w_0().flex_1().child(self.input.clone()))
                            .child(self.render_add_button(
                                "workspace-picker-header-add",
                                "workspace-picker-header-add",
                                picker.clone(),
                            )),
                    )
                    .when_some(inline_message, |header, (message, detail)| {
                        header.child(
                            div()
                                .pl(px(34.0))
                                .text_size(px(11.0))
                                .text_color(gpui_color(ACTIVE_THEME.warning))
                                .child(message)
                                .when_some(detail, |line, detail| line.child(format!(" {detail}"))),
                        )
                    }),
            )
            .child(
                div()
                    .min_h_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(30.0))
                            .flex_shrink_0()
                            .px_4()
                            .flex()
                            .items_center()
                            .text_size(px(10.0))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child("DIRECTORIES"),
                    )
                    .child(body),
            )
            .child(
                div()
                    .w_full()
                    .h(px(66.0))
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .border_t_1()
                    .border_color(gpui_color(ACTIVE_THEME.border_variant))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(gpui_color(ACTIVE_THEME.text_muted))
                            .child("↑↓ navigate   Tab complete   ↵ open   ⌘↵ add   Esc close"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("workspace-picker-finder", "Choose with Finder…")
                                    .tab_stop(true)
                                    .disabled(self.busy.is_some())
                                    .debug_selector("workspace-picker-finder")
                                    .on_activate(move |_, window, cx| {
                                        let _ = finder_picker.update(cx, |picker, cx| {
                                            picker.request_finder(window, cx)
                                        });
                                    }),
                            )
                            .child(self.render_add_button(
                                "workspace-picker-footer-add",
                                "workspace-picker-footer-add",
                                picker,
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_outside_tracker(
        &self,
        panel_bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let picker = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || panel_bounds.contains(&event.position)
                    {
                        return;
                    }
                    window.prevent_default();
                    let _ = picker.update(cx, |picker, cx| picker.dismiss_from_scrim(window, cx));
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

impl Render for WorkspacePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        self.scrollbar.update(cx, |scrollbar, cx| {
            scrollbar.sync(self.scrollbar_metrics(), cx)
        });
        let viewport = window.viewport_size();
        let width = px(PANEL_MAX_WIDTH).min((viewport.width - px(WINDOW_INSET * 2.0)).max(px(0.0)));
        let height =
            px(PANEL_MAX_HEIGHT).min((viewport.height - px(WINDOW_INSET * 2.0)).max(px(0.0)));
        let left = ((viewport.width - width) / 2.0).max(px(0.0));
        let top = ((viewport.height - height) / 2.0).max(px(0.0));
        let bounds = Bounds::new(gpui::point(left, top), gpui::size(width, height));
        let outside = self.render_outside_tracker(bounds, cx);
        let panel = self.render_panel(width, height, cx);

        anchored()
            .anchor(Corner::TopLeft)
            .position(gpui::point(px(0.0), px(0.0)))
            .snap_to_window()
            .child(
                div()
                    .debug_selector(|| "workspace-picker-overlay".to_owned())
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .key_context(KEY_CONTEXT)
                    .track_focus(&self.focus_scope)
                    .tab_group()
                    .bg(gpui_color(ACTIVE_THEME.overlay_scrim))
                    .occlude()
                    .child(outside)
                    .child(div().absolute().left(left).top(top).child(panel))
                    .capture_action(block_parent_action::<CreateWorkspace>)
                    .capture_action(block_parent_action::<SearchWorkspaces>)
                    .capture_action(block_parent_action::<CloseWorkspace>)
                    .capture_action(block_parent_action::<ActivateWorkspace1>)
                    .capture_action(block_parent_action::<ActivateWorkspace2>)
                    .capture_action(block_parent_action::<ActivateWorkspace3>)
                    .capture_action(block_parent_action::<ActivateWorkspace4>)
                    .capture_action(block_parent_action::<ActivateWorkspace5>)
                    .capture_action(block_parent_action::<ActivateWorkspace6>)
                    .capture_action(block_parent_action::<ActivateWorkspace7>)
                    .capture_action(block_parent_action::<ActivateWorkspace8>)
                    .capture_action(block_parent_action::<ActivateWorkspace9>)
                    .capture_action(block_parent_action::<ToggleSidebar>)
                    .capture_action(block_parent_action::<ToggleSidebarFocus>)
                    .capture_action(block_parent_action::<CopySelection>)
                    .capture_action(block_parent_action::<CreateWindow>)
                    .capture_action(block_parent_action::<ActivateWindow1>)
                    .capture_action(block_parent_action::<ActivateWindow2>)
                    .capture_action(block_parent_action::<ActivateWindow3>)
                    .capture_action(block_parent_action::<ActivateWindow4>)
                    .capture_action(block_parent_action::<ActivateWindow5>)
                    .capture_action(block_parent_action::<ActivateWindow6>)
                    .capture_action(block_parent_action::<ActivateWindow7>)
                    .capture_action(block_parent_action::<ActivateWindow8>)
                    .capture_action(block_parent_action::<ActivateWindow9>)
                    .capture_action(block_parent_action::<ClosePane>)
                    .capture_action(block_parent_action::<CloseWindow>)
                    .capture_action(block_parent_action::<SplitRight>)
                    .capture_action(block_parent_action::<SplitDown>)
                    .capture_action(block_parent_action::<FocusPaneLeft>)
                    .capture_action(block_parent_action::<FocusPaneRight>)
                    .capture_action(block_parent_action::<FocusPaneUp>)
                    .capture_action(block_parent_action::<FocusPaneDown>)
                    .capture_action(block_parent_action::<TogglePaneZoom>)
                    .capture_action(block_parent_action::<OpenTerminalFind>)
                    .capture_action(block_parent_action::<FindNext>)
                    .capture_action(block_parent_action::<FindPrevious>)
                    .capture_action(block_parent_action::<CloseTerminalFind>)
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_down(MouseButton::Right, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_down(MouseButton::Middle, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_move(|_: &MouseMoveEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_up(MouseButton::Left, |_: &MouseUpEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_up(MouseButton::Right, |_: &MouseUpEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_mouse_up(MouseButton::Middle, |_: &MouseUpEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_scroll_wheel(|_: &ScrollWheelEvent, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_action(cx.listener(|picker, _: &PickerMoveUp, _, cx| {
                        picker.move_selection(-1, cx);
                        cx.stop_propagation();
                    }))
                    .on_action(cx.listener(|picker, _: &PickerMoveDown, _, cx| {
                        picker.move_selection(1, cx);
                        cx.stop_propagation();
                    }))
                    .on_action(cx.listener(|picker, _: &PickerConfirmTyped, window, cx| {
                        picker.confirm_typed_path(window, cx);
                        cx.stop_propagation();
                    }))
                    .on_action(cx.listener(|picker, _: &PickerDismiss, window, cx| {
                        if picker.dismiss(window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_action(cx.listener(|_, _: &PickerFocusNext, window, cx| {
                        window.focus_next();
                        cx.stop_propagation();
                    }))
                    .on_action(cx.listener(|_, _: &PickerFocusPrevious, window, cx| {
                        window.focus_prev();
                        cx.stop_propagation();
                    })),
            )
            .into_any_element()
    }
}

fn status_for_error(error: WorkspacePickerFilesystemError) -> WorkspacePickerStatus {
    match error {
        WorkspacePickerFilesystemError::PermissionDenied => WorkspacePickerStatus::PermissionDenied,
        WorkspacePickerFilesystemError::Missing => WorkspacePickerStatus::Missing,
        WorkspacePickerFilesystemError::NotDirectory => WorkspacePickerStatus::NotDirectory,
        WorkspacePickerFilesystemError::Other => WorkspacePickerStatus::Other,
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::domain::WorkspaceDirectoryIdentity;
    use crate::platform::macos_system_settings::SystemSettingsOpenError;

    #[derive(Default)]
    struct ScriptedWorkspacePickerFilesystemState {
        readable_paths: Vec<PathBuf>,
        listed_entries: Vec<WorkspacePickerDirectoryEntry>,
        probed_paths: Vec<PathBuf>,
        created_paths: Vec<PathBuf>,
        validated_paths: Vec<PathBuf>,
        validation_results:
            VecDeque<Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError>>,
    }

    #[derive(Clone, Default)]
    struct ScriptedWorkspacePickerFilesystem {
        state: Arc<Mutex<ScriptedWorkspacePickerFilesystemState>>,
    }

    impl ScriptedWorkspacePickerFilesystem {
        fn new(
            readable_paths: impl IntoIterator<Item = PathBuf>,
            validation_results: impl IntoIterator<
                Item = Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError>,
            >,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(ScriptedWorkspacePickerFilesystemState {
                    readable_paths: readable_paths.into_iter().collect(),
                    validation_results: validation_results.into_iter().collect(),
                    ..ScriptedWorkspacePickerFilesystemState::default()
                })),
            }
        }

        fn set_listed_entries(
            &self,
            entries: impl IntoIterator<Item = WorkspacePickerDirectoryEntry>,
        ) {
            self.state.lock().unwrap().listed_entries = entries.into_iter().collect();
        }

        fn clear_records(&self) {
            let mut state = self.state.lock().unwrap();
            state.probed_paths.clear();
            state.created_paths.clear();
            state.validated_paths.clear();
        }

        fn records(&self) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
            let state = self.state.lock().unwrap();
            (
                state.probed_paths.clone(),
                state.created_paths.clone(),
                state.validated_paths.clone(),
            )
        }
    }

    impl WorkspacePickerFilesystem for ScriptedWorkspacePickerFilesystem {
        fn list_directories(
            &self,
            _directory: &Path,
            _hide_dot_prefixed: bool,
        ) -> Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError> {
            Ok(self.state.lock().unwrap().listed_entries.clone())
        }

        fn probe_exact_path(&self, path: &Path) -> WorkspacePickerExactPathProbe {
            let mut state = self.state.lock().unwrap();
            state.probed_paths.push(path.to_path_buf());
            if state.readable_paths.iter().any(|readable| readable == path) {
                WorkspacePickerExactPathProbe::ReadableDirectory
            } else {
                WorkspacePickerExactPathProbe::Unavailable(WorkspacePickerFilesystemError::Missing)
            }
        }

        fn create_dir_all(&self, path: &Path) -> Result<(), WorkspacePickerFilesystemError> {
            self.state
                .lock()
                .unwrap()
                .created_paths
                .push(path.to_path_buf());
            Ok(())
        }

        fn validate_workspace_directory(
            &self,
            path: &Path,
        ) -> Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError> {
            let mut state = self.state.lock().unwrap();
            state.validated_paths.push(path.to_path_buf());
            state
                .validation_results
                .pop_front()
                .unwrap_or(Err(WorkspacePickerFilesystemError::Other))
        }
    }

    struct TestSystemSettingsOpener;

    impl SystemSettingsOpener for TestSystemSettingsOpener {
        fn open_files_and_folders(&self) -> Result<(), SystemSettingsOpenError> {
            Ok(())
        }
    }

    struct WorkspacePickerHarness {
        picker: Entity<WorkspacePicker>,
    }

    impl Render for WorkspacePickerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.picker.clone())
        }
    }

    #[derive(Default)]
    struct UnderlayPointerEvents {
        moves: Cell<usize>,
        releases: Cell<usize>,
    }

    struct PointerIsolationHarness {
        picker: Entity<WorkspacePicker>,
        underlay_events: Rc<UnderlayPointerEvents>,
    }

    impl Render for PointerIsolationHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let move_events = Rc::clone(&self.underlay_events);
            let release_events = Rc::clone(&self.underlay_events);
            div()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .on_mouse_move(move |_, _, _| {
                            move_events.moves.set(move_events.moves.get() + 1);
                        })
                        .on_mouse_up(MouseButton::Left, move |_, _, _| {
                            release_events
                                .releases
                                .set(release_events.releases.get() + 1);
                        }),
                )
                .child(self.picker.clone())
        }
    }

    fn home() -> PathBuf {
        PathBuf::from("/Users/tester")
    }

    fn workspace_picker(
        filesystem: Arc<ScriptedWorkspacePickerFilesystem>,
        cx: &mut TestAppContext,
    ) -> (Entity<WorkspacePicker>, &mut VisualTestContext) {
        cx.update(crate::ui::init);
        let injected_filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync> = filesystem;
        let system_settings: Rc<dyn SystemSettingsOpener> = Rc::new(TestSystemSettingsOpener);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                WorkspacePicker::new(home(), injected_filesystem, system_settings, window, cx)
            });
            WorkspacePickerHarness { picker }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            picker.update(cx, |picker, cx| {
                picker.open(window, cx);
            });
        });
        cx.run_until_parked();
        (picker, cx)
    }

    fn set_input(picker: &Entity<WorkspacePicker>, value: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker
                    .input
                    .update(cx, |input, cx| input.set_value(value, cx));
                picker.refresh_for_input(value.to_owned(), window, cx);
            });
        });
        cx.run_until_parked();
    }

    #[test]
    fn workspace_picker_path_parser_accepts_root() {
        let parsed = parse_workspace_path("/", &home()).unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_path(),
                parsed.enumeration_directory(),
                parsed.leaf_filter(),
                parsed.trailing_separator(),
            ),
            ("/", Path::new("/"), Path::new("/"), "", true)
        );
    }

    #[test]
    fn workspace_picker_path_parser_expands_home_only_for_filesystem_operations() {
        let parsed = parse_workspace_path("~/Projects/SpaceTerm", &home()).unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_path(),
                parsed.enumeration_directory(),
                parsed.leaf_filter(),
            ),
            (
                "~/Projects/SpaceTerm",
                Path::new("/Users/tester/Projects/SpaceTerm"),
                Path::new("/Users/tester/Projects"),
                "SpaceTerm",
            )
        );
    }

    #[test]
    fn workspace_picker_path_parser_keeps_repeated_slashes_after_tilde_home_relative() {
        let parsed = parse_workspace_path("~//tmp/project", &home()).unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_path(),
                parsed.enumeration_directory(),
                parsed.leaf_filter(),
            ),
            (
                "~//tmp/project",
                Path::new("/Users/tester/tmp/project"),
                Path::new("/Users/tester/tmp"),
                "project",
            )
        );
    }

    #[test]
    fn workspace_picker_path_parser_uses_trailing_separator_as_directory_enumeration() {
        let parsed = parse_workspace_path("~/Projects/SpaceTerm/", &home()).unwrap();

        assert_eq!(
            (
                parsed.exact_path(),
                parsed.enumeration_directory(),
                parsed.leaf_filter(),
            ),
            (
                Path::new("/Users/tester/Projects/SpaceTerm"),
                Path::new("/Users/tester/Projects/SpaceTerm"),
                "",
            )
        );
    }

    #[test]
    fn workspace_picker_path_parser_preserves_typed_spelling() {
        let input = "~/Projects/symlink/../SpaceTerm";
        let parsed = parse_workspace_path(input, &home()).unwrap();

        assert_eq!(parsed.display(), input);
        assert_eq!(
            parsed.exact_path(),
            Path::new("/Users/tester/Projects/symlink/../SpaceTerm")
        );
    }

    #[test]
    fn workspace_picker_path_parser_rejects_invalid_relative_and_tilde_forms() {
        assert_eq!(
            parse_workspace_path("Projects", &home()),
            Err(WorkspacePathFormatError::Relative)
        );
        assert_eq!(
            parse_workspace_path("~", &home()),
            Err(WorkspacePathFormatError::BareTilde)
        );
        assert_eq!(
            parse_workspace_path("~other/Projects", &home()),
            Err(WorkspacePathFormatError::UnsupportedTilde)
        );
    }

    #[test]
    fn workspace_picker_parent_navigation_preserves_home_display_until_leaving_home() {
        let nested = parse_workspace_path("~/Projects/SpaceTerm/", &home()).unwrap();
        let home_path = parse_workspace_path("~/", &home()).unwrap();

        assert_eq!(
            parent_workspace_directory(&nested, &home()),
            Some((
                PathBuf::from("/Users/tester/Projects"),
                "~/Projects/".to_owned(),
            ))
        );
        assert_eq!(
            parent_workspace_directory(&home_path, &home()),
            Some((PathBuf::from("/Users"), "/Users/".to_owned()))
        );
    }

    #[test]
    fn workspace_picker_parent_navigation_preserves_absolute_style_inside_home() {
        let parsed = parse_workspace_path("/Users/tester/Projects/SpaceTerm", &home()).unwrap();

        assert_eq!(
            parent_workspace_directory(&parsed, &home()),
            Some((
                PathBuf::from("/Users/tester/Projects"),
                "/Users/tester/Projects/".to_owned(),
            ))
        );
    }

    #[test]
    fn workspace_picker_root_has_no_parent_navigation() {
        let root = parse_workspace_path("/", &home()).unwrap();

        assert_eq!(parent_workspace_directory(&root, &home()), None);
    }

    #[test]
    fn workspace_picker_dot_directories_are_revealed_only_by_dot_leaf() {
        assert!(
            parse_workspace_path("~/.", &home())
                .unwrap()
                .reveals_dot_directories()
        );
        assert!(
            !parse_workspace_path("~/config", &home())
                .unwrap()
                .reveals_dot_directories()
        );
    }

    #[test]
    fn workspace_picker_rows_filter_case_insensitive_prefixes_and_sort_deterministically() {
        let parsed = parse_workspace_path("~/Projects/sp", &home()).unwrap();
        let entries = [
            WorkspacePickerDirectoryEntry::new(
                "spaceTerm".to_owned(),
                home().join("Projects/spaceTerm"),
            ),
            WorkspacePickerDirectoryEntry::new(
                "Spatial".to_owned(),
                home().join("Projects/Spatial"),
            ),
            WorkspacePickerDirectoryEntry::new(
                "SpaceTerm".to_owned(),
                home().join("Projects/SpaceTerm"),
            ),
            WorkspacePickerDirectoryEntry::new("tools".to_owned(), home().join("Projects/tools")),
        ];

        let rows = filter_workspace_picker_rows(&parsed, &entries);

        assert_eq!(
            rows.iter()
                .map(WorkspacePickerRow::name)
                .collect::<Vec<_>>(),
            vec!["..", "SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn workspace_picker_parent_row_is_unfiltered_and_root_has_no_parent_row() {
        let filtered = parse_workspace_path("~/Projects/no-match", &home()).unwrap();
        let root = parse_workspace_path("/", &home()).unwrap();
        let entries = [WorkspacePickerDirectoryEntry::new(
            "SpaceTerm".to_owned(),
            home().join("Projects/SpaceTerm"),
        )];

        assert_eq!(
            filter_workspace_picker_rows(&filtered, &entries),
            vec![WorkspacePickerRow::Parent { path: home() }]
        );
        assert!(filter_workspace_picker_rows(&root, &[]).is_empty());
    }

    #[test]
    fn workspace_picker_selection_preserves_exact_path_or_uses_first_folder() {
        let parsed = parse_workspace_path("~/Projects/s", &home()).unwrap();
        let first = home().join("Projects/SpaceTerm");
        let second = home().join("Projects/spatial");
        let rows = filter_workspace_picker_rows(
            &parsed,
            &[
                WorkspacePickerDirectoryEntry::new("spatial".to_owned(), second.clone()),
                WorkspacePickerDirectoryEntry::new("SpaceTerm".to_owned(), first.clone()),
            ],
        );

        assert_eq!(
            repair_workspace_picker_selection(Some(&second), &rows),
            Some(second)
        );
        assert_eq!(repair_workspace_picker_selection(None, &rows), Some(first));
    }

    #[test]
    fn workspace_picker_selection_is_empty_when_only_parent_row_remains() {
        let parsed = parse_workspace_path("~/Projects/no-match", &home()).unwrap();
        let rows = filter_workspace_picker_rows(&parsed, &[]);

        assert_eq!(repair_workspace_picker_selection(None, &rows), None);
    }

    #[gpui::test]
    fn workspace_picker_omits_unrepresentable_sibling_before_navigation_and_validation(
        cx: &mut TestAppContext,
    ) {
        let representable_path = home().join("project x");
        let unrepresentable_path = home().join("project\nx");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [
                home(),
                representable_path.clone(),
                unrepresentable_path.clone(),
            ],
            [Ok(ValidatedWorkspaceDirectory::new(
                representable_path.clone(),
                WorkspaceDirectoryIdentity::new(7, 11),
            ))],
        ));
        filesystem.set_listed_entries([
            WorkspacePickerDirectoryEntry::new("project x".to_owned(), representable_path.clone()),
            WorkspacePickerDirectoryEntry::new("project\nx".to_owned(), unrepresentable_path),
        ]);
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        let initial_rows = picker.read_with(cx, |picker, _| {
            picker
                .rows
                .iter()
                .map(WorkspacePickerRow::name)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.descend_selected(window, cx));
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(
            picker.read_with(cx, |picker, cx| {
                (
                    initial_rows,
                    picker.input.read(cx).value().to_owned(),
                    picker
                        .parsed
                        .as_ref()
                        .map(|parsed| parsed.exact_path().to_path_buf()),
                    filesystem.records(),
                )
            }),
            (
                vec!["..".to_owned(), "project x".to_owned()],
                "~/project x/".to_owned(),
                Some(representable_path.clone()),
                (
                    vec![representable_path.clone()],
                    Vec::new(),
                    vec![representable_path],
                ),
            )
        );
    }

    #[gpui::test]
    fn finder_validation_accepts_unrepresentable_path_without_rebinding_the_path_bar(
        cx: &mut TestAppContext,
    ) {
        let finder_path = home().join("project\nx");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home()],
            [Ok(ValidatedWorkspaceDirectory::new(
                finder_path.clone(),
                WorkspaceDirectoryIdentity::new(7, 11),
            ))],
        ));
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.request_finder(window, cx);
                picker.validate_finder_selection(finder_path.clone(), window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            picker.read_with(cx, |picker, cx| {
                (
                    picker.input.read(cx).value().to_owned(),
                    picker
                        .parsed
                        .as_ref()
                        .map(|parsed| parsed.exact_path().to_path_buf()),
                    picker.busy,
                    filesystem.records(),
                )
            }),
            (
                "~/".to_owned(),
                Some(home()),
                Some(WorkspacePickerBusy::AwaitingActivation),
                (Vec::new(), Vec::new(), vec![finder_path]),
            )
        );
    }

    #[gpui::test]
    fn workspace_picker_shift_tab_leaves_the_path_and_tab_returns_from_a_button(
        cx: &mut TestAppContext,
    ) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::default());
        let (picker, cx) = workspace_picker(filesystem, cx);
        assert!(cx.update(|window, cx| { picker.read(cx).path_input_is_focused(window, cx) }));

        cx.simulate_keystrokes("shift-tab");
        assert!(!cx.update(|window, cx| { picker.read(cx).path_input_is_focused(window, cx) }));

        cx.simulate_keystrokes("tab");
        assert!(cx.update(|window, cx| { picker.read(cx).path_input_is_focused(window, cx) }));
    }

    #[gpui::test]
    fn workspace_picker_overlay_occludes_underlay_pointer_movement_and_release(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::ui::init);
        let filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync> =
            Arc::new(ScriptedWorkspacePickerFilesystem::default());
        let system_settings: Rc<dyn SystemSettingsOpener> = Rc::new(TestSystemSettingsOpener);
        let underlay_events = Rc::new(UnderlayPointerEvents::default());
        let harness_events = Rc::clone(&underlay_events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker =
                cx.new(|cx| WorkspacePicker::new(home(), filesystem, system_settings, window, cx));
            PointerIsolationHarness {
                picker,
                underlay_events: harness_events,
            }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            picker.update(cx, |picker, cx| {
                picker.open(window, cx);
            });
        });
        cx.run_until_parked();

        let scrim = gpui::point(px(4.0), px(4.0));
        cx.simulate_mouse_move(scrim, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(scrim, MouseButton::Left, Modifiers::none());

        assert_eq!(
            (underlay_events.moves.get(), underlay_events.releases.get()),
            (0, 0)
        );
    }

    #[gpui::test]
    fn stale_finder_validation_completion_does_not_overwrite_newer_typed_path(
        cx: &mut TestAppContext,
    ) {
        let current_path = PathBuf::from("/current-typed-path");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), current_path.clone()],
            [],
        ));
        let (picker, cx) = workspace_picker(filesystem, cx);
        let (lifecycle_generation, stale_operation_generation) = picker
            .read_with(cx, |picker, _| {
                (picker.lifecycle_generation, picker.operation_generation)
            });
        set_input(&picker, "/current-typed-path/", cx);

        picker.update(cx, |picker, cx| {
            picker.finish_validation(
                ValidationCompletion {
                    lifecycle_generation,
                    operation_generation: stale_operation_generation,
                    kind: ValidationKind::Finder,
                    expected_input_path: Some(PathBuf::from("/stale-finder-path")),
                    result: Err(WorkspacePickerFilesystemError::Missing),
                },
                cx,
            );
        });

        assert_eq!(
            picker.read_with(cx, |picker, cx| {
                (
                    picker.input.read(cx).value().to_owned(),
                    picker
                        .parsed
                        .as_ref()
                        .map(|parsed| parsed.exact_path().to_path_buf()),
                    picker.status,
                    picker.busy,
                )
            }),
            (
                "/current-typed-path/".to_owned(),
                Some(current_path),
                WorkspacePickerStatus::Readable,
                None,
            )
        );
    }

    #[gpui::test]
    fn finder_validation_failure_keeps_retry_and_creation_bound_to_selected_path(
        cx: &mut TestAppContext,
    ) {
        let typed_path = PathBuf::from("/typed-before-finder");
        let finder_path = PathBuf::from("/selected-with-finder");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), typed_path.clone()],
            [
                Err(WorkspacePickerFilesystemError::Missing),
                Ok(ValidatedWorkspaceDirectory::new(
                    finder_path.clone(),
                    WorkspaceDirectoryIdentity::new(7, 11),
                )),
            ],
        ));
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        set_input(&picker, "/typed-before-finder/", cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.request_finder(window, cx);
                picker.validate_finder_selection(finder_path.clone(), window, cx);
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.retry(window, cx));
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Create & Add");
        cx.run_until_parked();

        let input = picker.read_with(cx, |picker, cx| picker.input.read(cx).value().to_owned());
        assert_eq!(
            (input, filesystem.records()),
            (
                "/selected-with-finder/".to_owned(),
                (
                    vec![finder_path.clone()],
                    vec![finder_path.clone()],
                    vec![finder_path.clone(), finder_path],
                ),
            )
        );
    }
}
