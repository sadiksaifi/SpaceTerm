use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    Action, App, Context, Entity, EventEmitter, PromptButton, PromptLevel, Render, SharedString,
    Window, div, px,
};
use spaceterm_ui::{
    CommandPalette, CommandPaletteActivationPolicy, CommandPaletteCloseReason,
    CommandPaletteConfirm, CommandPaletteEvent, CommandPaletteHint, CommandPaletteItem,
    CommandPaletteLifecycleEvent, CommandPaletteMatching, Icon, IconName, MenuEntry,
};

use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateScratchWorkspace, CreateTab, FindNext,
    FindPrevious, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, OpenTerminalFind,
    SearchWorkspaces, SplitDown, SplitRight, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
};
use crate::domain::ValidatedWorkspaceDirectory;
use crate::platform::local_filesystem::picker::{
    WorkspacePickerDirectoryEntry, WorkspacePickerExactPathProbe, WorkspacePickerFilesystem,
    WorkspacePickerFilesystemError,
};
use crate::platform::permission_recovery::PermissionRecoveryOpener;

use crate::local_path::{
    LocalPathSemantics, ParsedWorkspacePath, WorkspacePathFormatError,
    display_workspace_directory_with_style, parse_workspace_path,
};
const ROW_ICON_SIZE: f32 = 14.0;
const DIRECTORY_SELECTION_ACTION: &str = "workspace-picker-directory-selection";
const RETRY_ACTION: &str = "workspace-picker-retry";
const SYSTEM_SETTINGS_ACTION: &str = "workspace-picker-open-system-settings";

/// Returns the directories the typed leaf selects, in stable presentation order.
///
/// The picker owns this filter because its query is a path rather than a search term: only a
/// case-insensitive prefix of the final segment matches, and no parent entry is produced. Moving
/// up a level is editing the path.
pub(super) fn filter_workspace_picker_rows(
    parsed: &ParsedWorkspacePath,
    entries: &[WorkspacePickerDirectoryEntry],
) -> Vec<WorkspacePickerDirectoryEntry> {
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
    directories
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum WorkspacePickerEvent {
    StateChanged,
    /// Escape closed the picker. Only Escape steps back to whatever presented it; an outside
    /// press or a focus loss dismisses the whole flow.
    Escaped,
    DirectorySelectionRequested,
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
    DirectorySelection,
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
    DirectorySelection,
    Creation,
}

struct ValidationCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    kind: ValidationKind,
    expected_input_path: Option<PathBuf>,
    result: Result<ValidatedWorkspaceDirectory, WorkspacePickerFilesystemError>,
}

/// The Workspace Picker presents live one-level directory reads through the application's Command
/// Palette, so it shares that transient's chrome, focus restoration, and dismissal exactly.
///
/// It owns only what is specific to choosing a Project Root: typed path spelling, the guarded
/// filesystem reads, folder creation, validation, and the System Directory Selection.
pub(super) struct WorkspacePicker {
    home: PathBuf,
    paths: LocalPathSemantics,
    filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync>,
    system_settings: Option<Rc<dyn PermissionRecoveryOpener>>,
    palette: Entity<CommandPalette<PathBuf>>,
    open: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    directory_selection_generation: u64,
    parsed: Option<ParsedWorkspacePath>,
    snapshot: Option<LoadedDirectorySnapshot>,
    rows: Vec<WorkspacePickerDirectoryEntry>,
    status: WorkspacePickerStatus,
    busy: Option<WorkspacePickerBusy>,
}

impl EventEmitter<WorkspacePickerEvent> for WorkspacePicker {}

impl WorkspacePicker {
    pub(super) fn new(
        home: PathBuf,
        filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync>,
        system_settings: Option<Rc<dyn PermissionRecoveryOpener>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Open Local Project", Vec::new(), window, cx);
            palette.set_hints(vec![CommandPaletteHint::new("Enter folder", "↵")], cx);
            // The query is an address, not a search term, so the picker filters and orders its own
            // rows, and activating one descends instead of completing the operation.
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |picker, _, event: &CommandPaletteEvent<PathBuf>, window, cx| {
                picker.reduce_palette_event(event, window, cx);
            },
        )
        .detach();

        Self {
            home,
            paths: filesystem.path_semantics(),
            filesystem,
            system_settings,
            palette,
            open: false,
            lifecycle_generation: 0,
            operation_generation: 0,
            directory_selection_generation: 0,
            parsed: None,
            snapshot: None,
            rows: Vec::new(),
            status: WorkspacePickerStatus::Loading,
            busy: None,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.open {
            self.refocus_path(window, cx);
            return false;
        }
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.busy = None;
        self.snapshot = None;
        self.parsed = None;
        self.rows.clear();
        self.status = WorkspacePickerStatus::Loading;
        self.palette.update(cx, |palette, cx| {
            palette.set_query_editable(true, cx);
            palette.set_dismissible(true, cx);
            palette.open(window, cx);
        });
        true
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) const fn blocks_terminal_input(&self) -> bool {
        self.open
    }

    pub(super) fn refocus_path(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.palette
                .update(cx, |palette, cx| palette.focus_editor(window, cx));
        }
    }

    #[cfg(test)]
    pub(super) fn path_input_is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.palette.read(cx).editor_is_focused(window, cx)
    }

    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.open || self.busy.is_some() {
            return false;
        }
        self.palette
            .update(cx, |palette, cx| palette.dismiss(window, cx))
    }

    pub(super) fn directory_selection_cancelled(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open && self.busy == Some(WorkspacePickerBusy::DirectorySelection) {
            self.busy = None;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    pub(super) fn directory_selection_failed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open && self.busy == Some(WorkspacePickerBusy::DirectorySelection) {
            self.busy = None;
            self.status = WorkspacePickerStatus::Other;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    pub(super) fn validate_system_selection(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open && self.busy == Some(WorkspacePickerBusy::DirectorySelection) {
            self.bind_system_selection_to_input(&path, cx);
            self.start_validation(path, ValidationKind::DirectorySelection, window, cx);
        }
    }

    fn bind_system_selection_to_input(&mut self, path: &Path, cx: &mut Context<Self>) {
        let prefer_tilde = self
            .parsed
            .as_ref()
            .is_some_and(|parsed| parsed.display().starts_with(self.paths.home_prefix()));
        let Some(display) = self.representable_directory_display(path, prefer_tilde, cx) else {
            return;
        };
        let Ok(parsed) = parse_workspace_path(self.paths, &display, &self.home) else {
            return;
        };
        self.palette
            .update(cx, |palette, cx| palette.set_query(display, cx));
        self.parsed = Some(parsed);
    }

    fn representable_directory_display(
        &self,
        path: &Path,
        prefer_tilde: bool,
        cx: &Context<Self>,
    ) -> Option<String> {
        let display =
            display_workspace_directory_with_style(self.paths, path, &self.home, prefer_tilde)?;
        self.palette
            .read(cx)
            .can_set_query_exactly(&display, cx)
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
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx)
        })
    }

    pub(super) fn activation_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(WorkspacePickerBusy::AwaitingActivation) {
            self.busy = None;
            self.status = WorkspacePickerStatus::Other;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.open = true;
                self.palette.update(cx, |palette, cx| {
                    palette.set_query(self.paths.home_prefix(), cx)
                });
                self.publish(cx);
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(reason)) => {
                self.open = false;
                self.busy = None;
                self.snapshot = None;
                self.parsed = None;
                self.rows.clear();
                self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
                self.operation_generation = self.operation_generation.wrapping_add(1);
                if matches!(reason, CommandPaletteCloseReason::Escape) {
                    cx.emit(WorkspacePickerEvent::Escaped);
                }
                cx.emit(WorkspacePickerEvent::StateChanged);
                cx.notify();
            }
            CommandPaletteEvent::QueryChanged(query) => {
                let value = query.text().to_owned();
                self.refresh_for_input(value, window, cx);
            }
            CommandPaletteEvent::Activated(activation) => {
                let path = activation.item_id().clone();
                self.descend_to(path, window, cx);
            }
            CommandPaletteEvent::Confirmed => self.confirm_typed_path(window, cx),
            CommandPaletteEvent::MenuAction(action) => match action.as_ref() {
                DIRECTORY_SELECTION_ACTION => self.request_directory_selection(cx),
                RETRY_ACTION => self.retry(window, cx),
                SYSTEM_SETTINGS_ACTION => self.open_system_settings(cx),
                _ => {}
            },
            CommandPaletteEvent::HeaderAction(_) => {}
        }
    }

    fn refresh_for_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() {
            return;
        }
        let parsed = match parse_workspace_path(self.paths, &value, &self.home) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.parsed = None;
                self.status = WorkspacePickerStatus::Invalid(error);
                self.operation_generation = self.operation_generation.wrapping_add(1);
                self.clear_rows(cx);
                self.publish(cx);
                return;
            }
        };
        let hide_dot_prefixed = !parsed.reveals_dot_directories();
        let listing_needed = !self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.directory == parsed.enumeration_directory()
                && snapshot.hide_dot_prefixed == hide_dot_prefixed
        });
        self.parsed = Some(parsed.clone());
        if !listing_needed {
            self.rebuild_rows(cx);
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
        self.publish(cx);
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
                let palette = self.palette.clone();
                entries.retain(|entry| palette.read(cx).can_set_query_exactly(entry.name(), cx));
                self.snapshot = Some(LoadedDirectorySnapshot {
                    directory: completion.parsed.enumeration_directory().to_owned(),
                    hide_dot_prefixed: completion.hide_dot_prefixed,
                    entries,
                });
                self.rebuild_rows(cx);
                None
            }
            Some(Err(error)) => Some(error),
            None => None,
        };
        // The status describes the exact typed path; only a failed read of the enumeration
        // directory may replace its rows. A missing leaf still filters the directory that holds
        // it, and missing ancestry keeps the last directory that did read.
        let unreadable_listing = matches!(
            listing_error,
            Some(
                WorkspacePickerFilesystemError::PermissionDenied
                    | WorkspacePickerFilesystemError::NotDirectory
                    | WorkspacePickerFilesystemError::Other
            )
        );
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
        if unreadable_listing {
            self.clear_rows(cx);
        }
        self.publish(cx);
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let (Some(parsed), Some(snapshot)) = (self.parsed.as_ref(), self.snapshot.as_ref()) else {
            return;
        };
        if snapshot.directory != parsed.enumeration_directory()
            || snapshot.hide_dot_prefixed == parsed.reveals_dot_directories()
        {
            return;
        }
        self.rows = filter_workspace_picker_rows(parsed, &snapshot.entries);
        let items = self
            .rows
            .iter()
            .cloned()
            .map(|entry| directory_palette_item(self.paths, entry))
            .collect();
        self.palette
            .update(cx, |palette, cx| palette.set_items(items, cx));
    }

    fn clear_rows(&mut self, cx: &mut Context<Self>) {
        self.rows.clear();
        self.palette
            .update(cx, |palette, cx| palette.set_items(Vec::new(), cx));
    }

    /// Descends into the row the palette has selected.
    #[cfg(test)]
    fn descend_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.palette.read(cx).selected_item_id().cloned() else {
            return;
        };
        self.descend_to(path, window, cx);
    }

    fn descend_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let prefer_tilde = self
            .parsed
            .as_ref()
            .is_some_and(|parsed| parsed.display().starts_with(self.paths.home_prefix()));
        let Some(display) = self.representable_directory_display(&path, prefer_tilde, cx) else {
            self.status = WorkspacePickerStatus::Other;
            self.publish(cx);
            return;
        };
        self.palette
            .update(cx, |palette, cx| palette.set_query(display, cx));
        self.refocus_path(window, cx);
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
                self.start_validation(
                    parsed.exact_path().to_owned(),
                    ValidationKind::Typed,
                    window,
                    cx,
                );
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
        self.publish(cx);
        let detail = format!(
            "Create {}? Missing parent folders will also be created.",
            parsed.display()
        );
        let response = window.prompt(
            PromptLevel::Info,
            "Create this folder?",
            Some(&detail),
            &[
                PromptButton::ok("Create & Open"),
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
                        parsed.exact_path().to_owned(),
                        ValidationKind::Creation,
                        window,
                        cx,
                    );
                } else {
                    picker.busy = None;
                    picker.publish(cx);
                    picker.refocus_path(window, cx);
                }
            });
        })
        .detach();
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
            ValidationKind::Typed | ValidationKind::DirectorySelection => {
                WorkspacePickerBusy::Validating
            }
        });
        let expected_input_path = match kind {
            ValidationKind::Typed | ValidationKind::Creation => Some(path.clone()),
            ValidationKind::DirectorySelection => self
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
            let _ = picker.update_in(cx, |picker, window, cx| {
                picker.finish_validation(completion, window, cx);
            });
        })
        .detach();
        self.publish(cx);
    }

    fn finish_validation(
        &mut self,
        completion: ValidationCompletion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                self.sync_palette(cx);
                cx.emit(WorkspacePickerEvent::Confirmed(directory));
                cx.notify();
            }
            Err(error) => {
                self.busy = None;
                self.status = if completion.kind == ValidationKind::DirectorySelection
                    && completion.expected_input_path.is_none()
                {
                    WorkspacePickerStatus::Other
                } else {
                    status_for_error(error)
                };
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn request_directory_selection(&mut self, cx: &mut Context<Self>) {
        if self.open && self.busy.is_none() {
            // The chooser must not retire a directory read that cancellation still needs.
            self.directory_selection_generation =
                self.directory_selection_generation.wrapping_add(1);
            self.busy = Some(WorkspacePickerBusy::DirectorySelection);
            self.publish(cx);
            cx.emit(WorkspacePickerEvent::DirectorySelectionRequested);
        }
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy.is_none() {
            let value = self.palette.read(cx).query().to_owned();
            self.snapshot = None;
            self.refresh_for_input(value, window, cx);
        }
    }

    fn open_system_settings(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || !self.open {
            return;
        }
        let Some(settings) = &self.system_settings else {
            return;
        };
        if settings.open().is_err() {
            self.status = WorkspacePickerStatus::Other;
            self.publish(cx);
        }
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
            "Create & Open"
        } else {
            "Open This Folder"
        }
    }

    /// The one line shown when the current path lists nothing.
    ///
    /// A missing folder needs no separate warning: the confirm control already reads
    /// `Create & Open`.
    fn empty_text(&self) -> &'static str {
        match self.status {
            WorkspacePickerStatus::Loading => "Reading\u{2026}",
            WorkspacePickerStatus::Missing => "No such folder",
            WorkspacePickerStatus::NotDirectory => "Not a folder",
            WorkspacePickerStatus::PermissionDenied => {
                "SpaceTerm needs permission to read this folder"
            }
            WorkspacePickerStatus::Other => "SpaceTerm couldn\u{2019}t read this folder",
            WorkspacePickerStatus::Invalid(error) => error.message(self.paths),
            WorkspacePickerStatus::Readable => "No folders here",
        }
    }

    fn actions_menu(&self, cx: &App) -> Vec<MenuEntry<SharedString>> {
        let mut entries = vec![
            MenuEntry::action(
                crate::desktop_profile::DesktopPresentation::get(cx)
                    .wording()
                    .directory_selection,
                DIRECTORY_SELECTION_ACTION.into(),
            )
            .disabled(self.busy.is_some())
            .debug_selector(DIRECTORY_SELECTION_ACTION),
        ];
        if self.status == WorkspacePickerStatus::PermissionDenied {
            entries.push(MenuEntry::separator());
            entries.push(
                MenuEntry::action("Retry", RETRY_ACTION.into())
                    .disabled(self.busy.is_some())
                    .debug_selector(RETRY_ACTION),
            );
            if let Some(settings) = &self.system_settings {
                entries.push(
                    MenuEntry::action(settings.label(), SYSTEM_SETTINGS_ACTION.into())
                        .debug_selector(SYSTEM_SETTINGS_ACTION),
                );
            }
        }
        entries
    }

    fn sync_palette(&self, cx: &mut Context<Self>) {
        let confirm = CommandPaletteConfirm::new(
            self.confirmation_label(),
            cx.global::<crate::desktop_profile::DesktopPresentation>()
                .command_palette_confirm_shortcut(),
        )
        .disabled(!self.can_confirm())
        .debug_selector("workspace-picker-confirm");
        let empty_text = self.empty_text();
        let actions = self.actions_menu(cx);
        let loading = self.busy.is_some();
        self.palette.update(cx, |palette, cx| {
            palette.set_confirm(Some(confirm), cx);
            palette.set_no_results_text(empty_text, cx);
            palette.set_actions_menu(actions, cx);
            palette.set_loading(loading, cx);
            palette.set_query_editable(!loading, cx);
            palette.set_dismissible(!loading, cx);
        });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.sync_palette(cx);
        cx.emit(WorkspacePickerEvent::StateChanged);
        cx.notify();
    }
}

/// Keeps a hierarchy shortcut from mutating Workspaces, Tabs, or Panes behind the open picker.
///
/// The Command Palette owns focus, pointer, and dismissal isolation, but the application's
/// hierarchy actions are registered above it and would otherwise still fire.
fn block_parent_action<A: Action>(_: &A, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

impl Render for WorkspacePicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .when(self.open, |picker| {
                picker
                    .capture_action(block_parent_action::<CreateScratchWorkspace>)
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
                    .capture_action(block_parent_action::<CreateTab>)
                    .capture_action(block_parent_action::<ActivateTab1>)
                    .capture_action(block_parent_action::<ActivateTab2>)
                    .capture_action(block_parent_action::<ActivateTab3>)
                    .capture_action(block_parent_action::<ActivateTab4>)
                    .capture_action(block_parent_action::<ActivateTab5>)
                    .capture_action(block_parent_action::<ActivateTab6>)
                    .capture_action(block_parent_action::<ActivateTab7>)
                    .capture_action(block_parent_action::<ActivateTab8>)
                    .capture_action(block_parent_action::<ActivateTab9>)
                    .capture_action(block_parent_action::<ClosePane>)
                    .capture_action(block_parent_action::<CloseTab>)
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
            })
            .child(self.palette.clone())
    }
}

fn directory_palette_item(
    paths: LocalPathSemantics,
    entry: WorkspacePickerDirectoryEntry,
) -> CommandPaletteItem<PathBuf> {
    let selector = format!("workspace-picker-row-{}", entry.name());
    CommandPaletteItem::new(
        entry.path().to_path_buf(),
        format!("{}{}", entry.name(), paths.separator()),
    )
    .leading_icon(move |foreground| {
        Icon::new(IconName::Folder, px(ROW_ICON_SIZE), foreground).into_any_element()
    })
    .debug_selector(selector)
}

fn status_for_error(error: WorkspacePickerFilesystemError) -> WorkspacePickerStatus {
    match error {
        WorkspacePickerFilesystemError::Missing => WorkspacePickerStatus::Missing,
        WorkspacePickerFilesystemError::NotDirectory => WorkspacePickerStatus::NotDirectory,
        WorkspacePickerFilesystemError::PermissionDenied => WorkspacePickerStatus::PermissionDenied,
        WorkspacePickerFilesystemError::Other => WorkspacePickerStatus::Other,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DirectorySelectionRequestIdentity {
    lifecycle: u64,
    request: u64,
}
impl WorkspacePicker {
    pub(super) fn directory_selection_request_identity(&self) -> DirectorySelectionRequestIdentity {
        DirectorySelectionRequestIdentity {
            lifecycle: self.lifecycle_generation,
            request: self.directory_selection_generation,
        }
    }
    pub(super) fn complete_directory_selection_request(
        &mut self,
        identity: DirectorySelectionRequestIdentity,
        result: Result<Option<PathBuf>, crate::directory_selection::DirectoryChooserError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if identity != self.directory_selection_request_identity() {
            return;
        }
        match result {
            Ok(Some(path)) => self.validate_system_selection(path, window, cx),
            Ok(None) => self.directory_selection_cancelled(window, cx),
            Err(_) => self.directory_selection_failed(window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use gpui::{Keystroke, TestAppContext, VisualTestContext, div};

    use super::*;
    use crate::domain::WorkspaceDirectoryIdentity;
    use crate::platform::permission_recovery::PermissionRecoveryError;

    #[derive(Default)]
    struct ScriptedWorkspacePickerFilesystemState {
        readable_paths: Vec<PathBuf>,
        listed_entries: Vec<WorkspacePickerDirectoryEntry>,
        listing_error: Option<WorkspacePickerFilesystemError>,
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

        fn set_listing_error(&self, error: WorkspacePickerFilesystemError) {
            self.state.lock().unwrap().listing_error = Some(error);
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
        fn path_semantics(&self) -> LocalPathSemantics {
            LocalPathSemantics::Posix
        }
        fn list_directories(
            &self,
            _directory: &Path,
            _hide_dot_prefixed: bool,
        ) -> Result<Vec<WorkspacePickerDirectoryEntry>, WorkspacePickerFilesystemError> {
            let state = self.state.lock().unwrap();
            state
                .listing_error
                .map_or_else(|| Ok(state.listed_entries.clone()), Err)
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

    struct TestPermissionRecoveryOpener;

    impl PermissionRecoveryOpener for TestPermissionRecoveryOpener {
        fn label(&self) -> &'static str {
            "Open System Settings"
        }
        fn open(&self) -> Result<(), PermissionRecoveryError> {
            Ok(())
        }
    }

    struct WorkspacePickerHarness {
        picker: Entity<WorkspacePicker>,
        modal: Option<spaceterm_ui::ModalPresentationHandle>,
    }

    impl WorkspacePickerHarness {
        fn present_modal(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.modal = Some(
                spaceterm_ui::Alert::new(
                    spaceterm_ui::ModalId::new("workspace-picker-test-modal"),
                    "Blocking alert",
                    "Blocking Alert",
                    "The Workspace Picker request should wait behind this modal.",
                    vec![
                        spaceterm_ui::ModalAction::new(
                            "ok",
                            "OK",
                            spaceterm_ui::ModalActionRole::Affirmative,
                            "workspace-picker-test-ok",
                        )
                        .default_action(true),
                    ],
                )
                .present(window, cx, |_, _| {})
                .expect("test modal should present"),
            );
        }
    }

    impl Render for WorkspacePickerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            spaceterm_ui::ModalLayer::new(div().size_full().child(self.picker.clone()))
        }
    }

    fn home() -> PathBuf {
        PathBuf::from("/Users/tester")
    }

    fn workspace_picker(
        filesystem: Arc<ScriptedWorkspacePickerFilesystem>,
        cx: &mut TestAppContext,
    ) -> (Entity<WorkspacePicker>, &mut VisualTestContext) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected_filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync> = filesystem;
        let system_settings: Option<Rc<dyn PermissionRecoveryOpener>> =
            Some(Rc::new(TestPermissionRecoveryOpener));
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                WorkspacePicker::new(home(), injected_filesystem, system_settings, window, cx)
            });
            WorkspacePickerHarness {
                picker,
                modal: None,
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
        (picker, cx)
    }

    #[gpui::test]
    fn first_picker_request_deferred_by_modal_still_starts_at_home(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected_filesystem: Arc<dyn WorkspacePickerFilesystem + Send + Sync> = filesystem;
        let system_settings: Option<Rc<dyn PermissionRecoveryOpener>> =
            Some(Rc::new(TestPermissionRecoveryOpener));
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                WorkspacePicker::new(home(), injected_filesystem, system_settings, window, cx)
            });
            WorkspacePickerHarness {
                picker,
                modal: None,
            }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            harness.update(cx, |harness, cx| harness.present_modal(window, cx));
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                assert!(picker.open(window, cx));
            });
        });
        cx.run_until_parked();
        let modal = harness
            .read_with(cx, |harness, _| harness.modal.clone())
            .expect("modal handle should be retained");
        cx.update(|window, cx| modal.dismiss(window, cx).expect("modal should dismiss"));
        cx.run_until_parked();

        assert_eq!(
            path_bar(&picker, cx),
            LocalPathSemantics::Posix.home_prefix()
        );
        assert!(picker.read_with(cx, |picker, cx| {
            picker.is_open() && picker.palette.read(cx).is_open()
        }));
    }

    fn set_input(picker: &Entity<WorkspacePicker>, value: &str, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            picker.update(cx, |picker, cx| {
                picker
                    .palette
                    .update(cx, |palette, cx| palette.set_query(value, cx));
            });
        });
        cx.run_until_parked();
    }

    fn path_bar(picker: &Entity<WorkspacePicker>, cx: &mut VisualTestContext) -> String {
        picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned())
    }

    fn row_names(picker: &Entity<WorkspacePicker>, cx: &mut VisualTestContext) -> Vec<String> {
        picker.read_with(cx, |picker, _| {
            picker
                .rows
                .iter()
                .map(|entry| entry.name().to_owned())
                .collect()
        })
    }

    #[test]
    fn workspace_picker_path_parser_accepts_root() {
        let parsed = parse_workspace_path(LocalPathSemantics::Posix, "/", &home()).unwrap();

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
        let parsed =
            parse_workspace_path(LocalPathSemantics::Posix, "~/Projects/SpaceTerm", &home())
                .unwrap();

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
        let parsed =
            parse_workspace_path(LocalPathSemantics::Posix, "~//tmp/project", &home()).unwrap();

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
        let parsed =
            parse_workspace_path(LocalPathSemantics::Posix, "~/Projects/SpaceTerm/", &home())
                .unwrap();

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
        let parsed = parse_workspace_path(LocalPathSemantics::Posix, input, &home()).unwrap();

        assert_eq!(parsed.display(), input);
        assert_eq!(
            parsed.exact_path(),
            Path::new("/Users/tester/Projects/symlink/../SpaceTerm")
        );
    }

    #[test]
    fn workspace_picker_path_parser_rejects_invalid_relative_and_tilde_forms() {
        assert_eq!(
            parse_workspace_path(LocalPathSemantics::Posix, "Projects", &home()),
            Err(WorkspacePathFormatError::Relative)
        );
        assert_eq!(
            parse_workspace_path(LocalPathSemantics::Posix, "~", &home()),
            Err(WorkspacePathFormatError::BareTilde)
        );
        assert_eq!(
            parse_workspace_path(LocalPathSemantics::Posix, "~other/Projects", &home()),
            Err(WorkspacePathFormatError::UnsupportedTilde)
        );
    }

    #[test]
    fn workspace_picker_display_keeps_home_relative_spelling_inside_home() {
        assert_eq!(
            display_workspace_directory_with_style(
                LocalPathSemantics::Posix,
                &home().join("Projects/SpaceTerm"),
                &home(),
                true
            ),
            Some("~/Projects/SpaceTerm/".to_owned())
        );
        assert_eq!(
            display_workspace_directory_with_style(
                LocalPathSemantics::Posix,
                &home(),
                &home(),
                true
            ),
            Some("~/".to_owned())
        );
    }

    #[test]
    fn workspace_picker_display_keeps_absolute_spelling_even_inside_home() {
        assert_eq!(
            display_workspace_directory_with_style(
                LocalPathSemantics::Posix,
                &home().join("Projects/SpaceTerm"),
                &home(),
                false
            ),
            Some("/Users/tester/Projects/SpaceTerm/".to_owned())
        );
        assert_eq!(
            display_workspace_directory_with_style(
                LocalPathSemantics::Posix,
                &PathBuf::from("/"),
                &home(),
                true
            ),
            Some("/".to_owned())
        );
    }

    #[test]
    fn workspace_picker_dot_directories_are_revealed_only_by_dot_leaf() {
        assert!(
            parse_workspace_path(LocalPathSemantics::Posix, "~/.", &home())
                .unwrap()
                .reveals_dot_directories()
        );
        assert!(
            !parse_workspace_path(LocalPathSemantics::Posix, "~/config", &home())
                .unwrap()
                .reveals_dot_directories()
        );
    }

    #[test]
    fn workspace_picker_rows_filter_case_insensitive_prefixes_and_sort_deterministically() {
        let parsed =
            parse_workspace_path(LocalPathSemantics::Posix, "~/Projects/sp", &home()).unwrap();
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
                .map(WorkspacePickerDirectoryEntry::name)
                .collect::<Vec<_>>(),
            vec!["SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn workspace_picker_rows_never_include_a_parent_entry() {
        let filtered =
            parse_workspace_path(LocalPathSemantics::Posix, "~/Projects/no-match", &home())
                .unwrap();
        let nested =
            parse_workspace_path(LocalPathSemantics::Posix, "~/Projects/", &home()).unwrap();
        let entries = [WorkspacePickerDirectoryEntry::new(
            "SpaceTerm".to_owned(),
            home().join("Projects/SpaceTerm"),
        )];

        assert!(
            filter_workspace_picker_rows(&filtered, &entries).is_empty(),
            "a leaf matching nothing still produced a row"
        );
        assert_eq!(
            filter_workspace_picker_rows(&nested, &entries)
                .iter()
                .map(WorkspacePickerDirectoryEntry::name)
                .collect::<Vec<_>>(),
            vec!["SpaceTerm"],
            "a nested directory listing gained an entry it did not read"
        );
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
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        filesystem.set_listed_entries([
            WorkspacePickerDirectoryEntry::new("project x".to_owned(), representable_path.clone()),
            WorkspacePickerDirectoryEntry::new("project\nx".to_owned(), unrepresentable_path),
        ]);
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        let initial_rows = row_names(&picker, cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.descend_selected(window, cx));
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        cx.run_until_parked();

        let path_bar = path_bar(&picker, cx);
        assert_eq!(
            picker.read_with(cx, |picker, _| {
                (
                    initial_rows,
                    path_bar,
                    picker
                        .parsed
                        .as_ref()
                        .map(|parsed| parsed.exact_path().to_path_buf()),
                    filesystem.records(),
                )
            }),
            (
                vec!["project x".to_owned()],
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
    fn directory_selection_validation_accepts_unrepresentable_path_without_rebinding_the_path_bar(
        cx: &mut TestAppContext,
    ) {
        let directory_selection_path = home().join("project\nx");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home()],
            [Ok(ValidatedWorkspaceDirectory::new(
                directory_selection_path.clone(),
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.request_directory_selection(cx);
                picker.validate_system_selection(directory_selection_path.clone(), window, cx);
            });
        });
        cx.run_until_parked();

        let path_bar = path_bar(&picker, cx);
        assert_eq!(
            picker.read_with(cx, |picker, _| {
                (
                    path_bar,
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
                (Vec::new(), Vec::new(), vec![directory_selection_path]),
            )
        );
    }

    #[gpui::test]
    fn workspace_picker_should_present_exactly_one_confirm_control(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);

        assert!(
            cx.debug_bounds("workspace-picker-confirm").is_some(),
            "the picker did not render its confirm control"
        );
        // The confirm control and the System Directory Selection are the only footer actions; the old panel
        // repeated the confirm in its header as well.
        assert!(
            cx.debug_bounds("workspace-picker-header-add").is_none(),
            "the picker rendered a second confirm control in its header"
        );
        assert!(
            cx.debug_bounds("workspace-picker-panel").is_none(),
            "the picker rendered its own panel instead of the Command Palette"
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.confirmation_label()),
            "Open This Folder"
        );
    }

    #[gpui::test]
    fn a_missing_folder_should_be_expressed_only_by_the_confirm_label(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);

        set_input(&picker, "~/jkasdf", cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| (
                picker.status,
                picker.confirmation_label(),
                picker.can_confirm(),
                picker.empty_text(),
            )),
            (
                WorkspacePickerStatus::Missing,
                "Create & Open",
                true,
                "No such folder",
            )
        );
    }

    #[gpui::test]
    fn a_partial_leaf_should_still_list_the_directory_that_holds_it(cx: &mut TestAppContext) {
        let documents = home().join("Documents");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), documents.clone()],
            [],
        ));
        filesystem.set_listed_entries([WorkspacePickerDirectoryEntry::new(
            "Documents".to_owned(),
            documents,
        )]);
        let (picker, cx) = workspace_picker(filesystem, cx);

        set_input(&picker, "~/Doc", cx);

        // `~/Doc` names no folder, but it filters `~/`, so the match it selects must stay listed.
        assert_eq!(row_names(&picker, cx), vec!["Documents".to_owned()]);
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.status),
            WorkspacePickerStatus::Missing
        );
    }

    #[gpui::test]
    fn an_unreadable_directory_should_replace_its_rows(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        filesystem.set_listed_entries([WorkspacePickerDirectoryEntry::new(
            "Documents".to_owned(),
            home().join("Documents"),
        )]);
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        assert_eq!(row_names(&picker, cx), vec!["Documents".to_owned()]);

        filesystem.set_listing_error(WorkspacePickerFilesystemError::PermissionDenied);
        set_input(&picker, "/locked/", cx);

        assert!(
            row_names(&picker, cx).is_empty(),
            "an unreadable directory kept rows it did not read"
        );
    }

    #[gpui::test]
    fn the_directory_selection_fallback_should_be_one_click_from_the_footer(
        cx: &mut TestAppContext,
    ) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);

        assert_eq!(
            picker.read_with(cx, |picker, cx| picker.actions_menu(cx).len()),
            1
        );
        assert!(
            cx.debug_bounds("workspace-picker-directory-selection")
                .is_some(),
            "the System Directory Selection was not offered directly"
        );
        assert!(
            cx.debug_bounds("command-palette-actions-menu").is_none(),
            "the lone System Directory Selection was hidden behind a disclosure"
        );
    }

    #[gpui::test]
    fn an_unreadable_folder_should_offer_recovery_from_the_actions_menu(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        filesystem.set_listing_error(WorkspacePickerFilesystemError::PermissionDenied);
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);

        set_input(&picker, "/locked/", cx);

        let (status, empty_text, actions) = picker.read_with(cx, |picker, cx| {
            (
                picker.status,
                picker.empty_text(),
                picker.actions_menu(cx).len(),
            )
        });
        assert_eq!(
            (status, empty_text),
            (
                WorkspacePickerStatus::PermissionDenied,
                "SpaceTerm needs permission to read this folder",
            )
        );
        // Choose with System, a separator, Retry, and Open System Settings; only a readable folder
        // leaves the System Directory Selection alone in the footer.
        assert_eq!(actions, 4);
        assert!(
            cx.debug_bounds("workspace-picker-permission").is_none(),
            "the picker rendered a centred permission body instead of using its empty state"
        );
    }

    #[gpui::test]
    fn unavailable_permission_recovery_is_omitted_and_cannot_be_invoked(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        filesystem.set_listing_error(WorkspacePickerFilesystemError::PermissionDenied);
        let (picker, cx) = workspace_picker(filesystem, cx);
        cx.update(|_, cx| {
            picker.update(cx, |picker, cx| {
                picker.system_settings = None;
                picker.open_system_settings(cx);
                assert_eq!(picker.status, WorkspacePickerStatus::PermissionDenied);
                assert_eq!(picker.actions_menu(cx).len(), 3);
            })
        });
    }

    #[gpui::test]
    fn directory_selection_cancellation_preserves_a_pending_directory_refresh(
        cx: &mut TestAppContext,
    ) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        filesystem.set_listed_entries([WorkspacePickerDirectoryEntry::new(
            "Projects".to_owned(),
            home().join("Projects"),
        )]);
        let (picker, cx) = workspace_picker(filesystem, cx);

        // Exercise both possible orders without parking the background executor until DirectorySelection
        // is already open. The real refresh task must still publish its result in either order.
        for cancel_before_refresh in [true, false] {
            let identity = cx.update(|window, cx| {
                picker.update(cx, |picker, cx| {
                    picker.retry(window, cx);
                    assert_eq!(picker.status, WorkspacePickerStatus::Loading);
                    picker.request_directory_selection(cx);
                    let identity = picker.directory_selection_request_identity();
                    if cancel_before_refresh {
                        picker.complete_directory_selection_request(identity, Ok(None), window, cx);
                    }
                    identity
                })
            });
            cx.run_until_parked();
            cx.update(|window, cx| {
                picker.update(cx, |picker, cx| {
                    assert_eq!(picker.status, WorkspacePickerStatus::Readable);
                    if !cancel_before_refresh {
                        picker.complete_directory_selection_request(identity, Ok(None), window, cx);
                    }
                    assert!(picker.can_confirm());
                    assert!(picker.path_input_is_focused(window, cx));
                });
            });
            assert_eq!(
                path_bar(&picker, cx),
                LocalPathSemantics::Posix.home_prefix()
            );
            assert_eq!(row_names(&picker, cx), vec!["Projects"]);
        }
    }

    #[gpui::test]
    fn stale_chooser_completion_cannot_settle_a_later_request(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.request_directory_selection(cx);
                let previous = picker.directory_selection_request_identity();
                picker.complete_directory_selection_request(previous, Ok(None), window, cx);
                picker.request_directory_selection(cx);
                let current = picker.directory_selection_request_identity();
                assert_ne!(previous, current);
                picker.complete_directory_selection_request(previous, Ok(Some(home())), window, cx);
                assert_eq!(picker.busy, Some(WorkspacePickerBusy::DirectorySelection));
                picker.complete_directory_selection_request(current, Ok(None), window, cx);
                assert_eq!(picker.busy, None);
            })
        });
    }

    #[gpui::test]
    fn activating_a_row_should_descend_without_closing_the_picker(cx: &mut TestAppContext) {
        let nested = home().join("Projects");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), nested.clone()],
            [],
        ));
        filesystem.set_listed_entries([WorkspacePickerDirectoryEntry::new(
            "Projects".to_owned(),
            nested.clone(),
        )]);
        let (picker, cx) = workspace_picker(filesystem, cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.descend_selected(window, cx));
        });
        cx.run_until_parked();

        assert_eq!(path_bar(&picker, cx), "~/Projects/".to_owned());
        assert!(
            picker.read_with(cx, |picker, _| picker.is_open()),
            "descending into a folder closed the picker"
        );
    }

    #[gpui::test]
    fn directory_selection_cancellation_should_survive_key_window_transitions(
        cx: &mut TestAppContext,
    ) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);
        let original_query = path_bar(&picker, cx);

        picker.update(cx, |picker, cx| picker.request_directory_selection(cx));
        cx.deactivate_window();
        cx.run_until_parked();
        let retained_while_inactive =
            picker.read_with(cx, |picker, _| (picker.is_open(), picker.busy));

        cx.update(|window, _| window.activate_window());
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.directory_selection_cancelled(window, cx)
            });
        });
        cx.run_until_parked();

        assert_eq!(
            retained_while_inactive,
            (true, Some(WorkspacePickerBusy::DirectorySelection))
        );
        assert_eq!(path_bar(&picker, cx), original_query);
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
        assert!(cx.update(|window, cx| picker.read(cx).path_input_is_focused(window, cx)));
    }

    #[gpui::test]
    fn directory_selection_completion_should_survive_key_window_transitions(
        cx: &mut TestAppContext,
    ) {
        let selected = home().join("Projects");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), selected.clone()],
            [Ok(ValidatedWorkspaceDirectory::new(
                selected.clone(),
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        let (picker, cx) = workspace_picker(filesystem, cx);

        picker.update(cx, |picker, cx| picker.request_directory_selection(cx));
        cx.deactivate_window();
        cx.run_until_parked();
        cx.update(|window, _| window.activate_window());
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.validate_system_selection(selected, window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(
            picker.read_with(cx, |picker, _| (picker.is_open(), picker.busy)),
            (true, Some(WorkspacePickerBusy::AwaitingActivation))
        );
    }

    #[gpui::test]
    fn creation_prompt_cancellation_should_survive_key_window_transitions(cx: &mut TestAppContext) {
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new([home()], []));
        let (picker, cx) = workspace_picker(filesystem, cx);
        set_input(&picker, "~/new-project", cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        assert!(cx.has_pending_prompt());
        cx.deactivate_window();
        cx.run_until_parked();
        cx.update(|window, _| window.activate_window());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();

        assert_eq!(path_bar(&picker, cx), "~/new-project");
        assert_eq!(
            picker.read_with(cx, |picker, _| (picker.is_open(), picker.busy)),
            (true, None)
        );
        assert!(cx.update(|window, cx| picker.read(cx).path_input_is_focused(window, cx)));
    }

    #[gpui::test]
    fn creation_prompt_completion_should_survive_key_window_transitions(cx: &mut TestAppContext) {
        let path = home().join("new-project");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home()],
            [Ok(ValidatedWorkspaceDirectory::new(
                path.clone(),
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        set_input(&picker, "~/new-project", cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        assert!(cx.has_pending_prompt());
        cx.deactivate_window();
        cx.run_until_parked();
        cx.update(|window, _| window.activate_window());
        cx.simulate_prompt_answer("Create & Open");
        cx.run_until_parked();

        assert_eq!(
            (
                picker.read_with(cx, |picker, _| (picker.is_open(), picker.busy)),
                filesystem.records(),
            ),
            (
                (true, Some(WorkspacePickerBusy::AwaitingActivation)),
                (Vec::new(), vec![path.clone()], vec![path]),
            )
        );
    }

    #[gpui::test]
    fn pending_validation_should_reject_query_mutation_and_dismissal(cx: &mut TestAppContext) {
        let path = home().join("Projects");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), path.clone()],
            [Ok(ValidatedWorkspaceDirectory::new(
                path.clone(),
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        let (picker, cx) = workspace_picker(filesystem, cx);
        set_input(&picker, "~/Projects/", cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
            for key in ["cmd-a", "x", "escape"] {
                window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx);
            }
        });
        let programmatic_dismissed =
            cx.update(|window, cx| picker.update(cx, |picker, cx| picker.dismiss(window, cx)));
        let retained = picker.read_with(cx, |picker, _| {
            (
                picker.is_open(),
                picker.busy,
                picker
                    .parsed
                    .as_ref()
                    .map(|parsed| parsed.exact_path().to_path_buf()),
            )
        });
        let query = path_bar(&picker, cx);
        cx.run_until_parked();

        assert!(!programmatic_dismissed);
        assert_eq!(
            (query, retained),
            (
                "~/Projects/".to_owned(),
                (true, Some(WorkspacePickerBusy::Validating), Some(path)),
            )
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.busy),
            Some(WorkspacePickerBusy::AwaitingActivation)
        );
    }

    #[gpui::test]
    fn background_creation_should_survive_dismissal_until_confirmation(cx: &mut TestAppContext) {
        let path = home().join("new-project");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home()],
            [Ok(ValidatedWorkspaceDirectory::new(
                path,
                WorkspaceDirectoryIdentity::for_test(7011),
            ))],
        ));
        let (picker, cx) = workspace_picker(filesystem, cx);
        set_input(&picker, "~/new-project", cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_typed_path(window, cx));
        });
        cx.simulate_prompt_answer("Create & Open");
        assert!(cx.executor().tick(), "prompt completion did not run");
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        let programmatic_dismissed =
            cx.update(|window, cx| picker.update(cx, |picker, cx| picker.dismiss(window, cx)));
        let retained = picker.read_with(cx, |picker, _| (picker.is_open(), picker.busy));
        cx.run_until_parked();

        assert!(!programmatic_dismissed);
        assert_eq!(retained, (true, Some(WorkspacePickerBusy::Creating)));
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.busy),
            Some(WorkspacePickerBusy::AwaitingActivation)
        );
    }

    #[gpui::test]
    fn stale_directory_selection_validation_completion_does_not_overwrite_newer_typed_path(
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

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.finish_validation(
                    ValidationCompletion {
                        lifecycle_generation,
                        operation_generation: stale_operation_generation,
                        kind: ValidationKind::DirectorySelection,
                        expected_input_path: Some(PathBuf::from("/stale-directory_selection-path")),
                        result: Err(WorkspacePickerFilesystemError::Missing),
                    },
                    window,
                    cx,
                );
            });
        });

        let path_bar = path_bar(&picker, cx);
        assert_eq!(
            picker.read_with(cx, |picker, _| {
                (
                    path_bar,
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
    fn directory_selection_validation_failure_keeps_retry_and_creation_bound_to_selected_path(
        cx: &mut TestAppContext,
    ) {
        let typed_path = PathBuf::from("/typed-before-directory_selection");
        let directory_selection_path = PathBuf::from("/selected-with-directory_selection");
        let filesystem = Arc::new(ScriptedWorkspacePickerFilesystem::new(
            [home(), typed_path.clone()],
            [
                Err(WorkspacePickerFilesystemError::Missing),
                Ok(ValidatedWorkspaceDirectory::new(
                    directory_selection_path.clone(),
                    WorkspaceDirectoryIdentity::for_test(7011),
                )),
            ],
        ));
        let (picker, cx) = workspace_picker(Arc::clone(&filesystem), cx);
        set_input(&picker, "/typed-before-directory_selection/", cx);
        filesystem.clear_records();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.request_directory_selection(cx);
                picker.validate_system_selection(directory_selection_path.clone(), window, cx);
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
        cx.simulate_prompt_answer("Create & Open");
        cx.run_until_parked();

        let input = path_bar(&picker, cx);
        assert_eq!(
            (input, filesystem.records()),
            (
                "/selected-with-directory_selection/".to_owned(),
                (
                    vec![directory_selection_path.clone()],
                    vec![directory_selection_path.clone()],
                    vec![directory_selection_path.clone(), directory_selection_path],
                ),
            )
        );
    }
}
