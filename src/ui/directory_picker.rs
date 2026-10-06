//! The Directory Picker: one machine's directories browsed and pinned through the Command
//! Palette, with the machine reached through a [`DirectorySource`].
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::{cmp::Ordering, fmt};

use gpui::prelude::*;
use gpui::{Action, App, SharedString};
use gpui::{Context, Entity, EventEmitter, Render, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertOutcome, CommandPalette, CommandPaletteActivationPolicy, CommandPaletteCloseReason,
    CommandPaletteEmpty, CommandPaletteEvent, CommandPaletteItem, CommandPaletteLifecycleEvent,
    CommandPaletteMatching, CommandPalettePrimaryAction, CommandPaletteReplacementFocus,
    FuzzyTarget, Icon, IconName, ModalAction, ModalActionRole, ModalId, ModalPresentationHandle,
    fuzzy_filter,
};

use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateTab, FindNext, FindPrevious,
    FocusNextPane, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, FocusPreviousPane,
    MoveTabLeft, MoveTabRight, NewWorkspace, NextTab, OpenTerminalFind, PreviousTab, SplitDown,
    SplitRight, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
};

use crate::domain::PinnedDirectory;

mod local_source;
mod remote_source;

pub(crate) use local_source::LocalDirectorySource;
pub(crate) use remote_source::{
    RemoteDirectoryProvider, RemoteDirectoryProviderError, RemoteDirectorySource,
};

const HOME_DISPLAY: &str = "~/";
const CREATE_ALERT_ID: &str = "directory-picker-create-directory";
/// Identifies the search line's action that opens the exact path, creating it when missing.
const CONFIRM_ACTION: &str = "directory-picker-confirm";
/// Trails a missing exact path in the search line, since opening it creates it.
const NEW_DIRECTORY_NOTE: &str = "New directory";
/// Identifies the confirm action's menu item that hands off to System Directory Selection.
const SYSTEM_SELECTION_ACTION: &str = "directory-picker-system-selection";
/// Identifies the row that opens the enclosing directory.
const ENCLOSING_ROW: &str = "directory-picker-enclosing";
const NO_SUBDIRECTORIES: &str = "No subdirectories";
const TRUNCATED_LISTING_NOTE: &str =
    "Showing the first 1024 directories. Type a path to open others.";
const UNSUPPORTED_LOGIN_SHELL_MESSAGE: &str =
    "The remote login shell does not support login mode. Choose another account or shell.";
pub(super) const MAXIMUM_DIRECTORY_ROWS: usize = 1024;
const CONNECTION_LOST_NOTICE: EmptyNotice =
    EmptyNotice::new(IconName::TriangleAlert, "SSH connection lost");
const SESSION_UNAVAILABLE_NOTICE: EmptyNotice =
    EmptyNotice::new(IconName::TriangleAlert, "SSH server refused a new session")
        .description("Close a Pane on this connection or edit the path to try again.");
const UNSUPPORTED_LOGIN_SHELL_NOTICE: EmptyNotice =
    EmptyNotice::new(IconName::TriangleAlert, "Unsupported login shell")
        .description(UNSUPPORTED_LOGIN_SHELL_MESSAGE);

/// The content-unavailable view the list area presents in place of child directories.
#[derive(Clone, Copy)]
struct EmptyNotice {
    icon: IconName,
    title: &'static str,
    description: Option<&'static str>,
}

impl EmptyNotice {
    const fn new(icon: IconName, title: &'static str) -> Self {
        Self {
            icon,
            title,
            description: None,
        }
    }

    const fn description(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }
}

/// Why a [`DirectorySource`] could not read or change a directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectorySourceError {
    ConnectionLost,
    /// The machine refused a new session on a connection that is still live.
    SessionUnavailable,
    Missing,
    NotDirectory,
    PermissionDenied,
    UnsupportedLoginShell,
    Other,
}

pub(crate) type SourceFuture<T> = Pin<Box<dyn Future<Output = Result<T, DirectorySourceError>>>>;

/// The boundary through which the Directory Picker reads and changes one machine's directories.
///
/// Each source converts [`PickerPath`] spellings into its own machine's path type.
pub(crate) trait DirectorySource {
    /// Names the machine when it is not the local one.
    fn machine_name(&self) -> Option<&str>;

    /// Resolves the absolute home directory that `~/` names.
    fn discover_home(&self) -> SourceFuture<PickerPath>;

    fn list_directories(&self, directory: PickerPath) -> SourceFuture<DirectoryListing>;

    fn probe_exact_path(&self, directory: PickerPath) -> SourceFuture<ExactPathState>;

    fn create_directory_recursively(&self, directory: PickerPath) -> SourceFuture<()>;

    /// Validates the directory's physical identity as a Pinned Directory.
    fn pin(&self, directory: PickerPath) -> SourceFuture<PinnedDirectory>;
}

/// A directory path as spelled in the Directory Picker: absolute, or relative to the home
/// directory through `~`.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct PickerPath(String);

impl fmt::Debug for PickerPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PickerPath(<redacted>)")
    }
}

impl PickerPath {
    pub(crate) fn new(value: String) -> Result<Self, PickerPathError> {
        if !value.starts_with('/') && value != "~" && !value.starts_with("~/") {
            return Err(PickerPathError::Relative);
        }
        if value.chars().any(char::is_control) {
            return Err(PickerPathError::InvalidControlCharacter);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PickerPathError {
    Relative,
    BareTilde,
    UnsupportedTilde,
    InvalidControlCharacter,
    DotSegment,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct ParsedPickerPath {
    display: String,
    exact_directory: PickerPath,
    enumeration_directory: PickerPath,
    descend_prefix: String,
    leaf_filter: String,
    trailing_separator: bool,
}

impl fmt::Debug for ParsedPickerPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ParsedPickerPath(<redacted>)")
    }
}

impl ParsedPickerPath {
    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) const fn exact_directory(&self) -> &PickerPath {
        &self.exact_directory
    }

    pub(super) const fn enumeration_directory(&self) -> &PickerPath {
        &self.enumeration_directory
    }

    #[cfg(test)]
    pub(super) fn leaf_filter(&self) -> &str {
        &self.leaf_filter
    }

    #[cfg(test)]
    pub(super) const fn trailing_separator(&self) -> bool {
        self.trailing_separator
    }

    pub(super) fn reveals_hidden_directories(&self) -> bool {
        self.leaf_filter.starts_with('.')
    }

    /// Whether the exact path names a directory by its own name. A `.` or `..` leaf only filters
    /// hidden directories, since opening it would pin a relative spelling.
    pub(super) fn names_openable_directory(&self) -> bool {
        !matches!(self.leaf_filter.as_str(), "." | "..")
    }
}

pub(super) fn parse_picker_path(input: &str) -> Result<ParsedPickerPath, PickerPathError> {
    if input == "~" {
        return Err(PickerPathError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with("~/") {
        return Err(PickerPathError::UnsupportedTilde);
    }
    if !input.starts_with('/') && !input.starts_with("~/") {
        return Err(PickerPathError::Relative);
    }

    let exact_directory = PickerPath::new(input.to_owned())?;
    let trailing_separator = input.ends_with('/');
    let (enumeration_spelling, descend_prefix, leaf_filter) = if trailing_separator {
        (input, input.to_owned(), String::new())
    } else {
        let separator = input.rfind('/').ok_or(PickerPathError::Relative)?;
        let directory_with_separator = &input[..=separator];
        let enumeration_spelling =
            if directory_with_separator == "/" || directory_with_separator == "~/" {
                directory_with_separator
            } else {
                &directory_with_separator[..directory_with_separator.len() - 1]
            };
        (
            enumeration_spelling,
            directory_with_separator.to_owned(),
            input[separator + 1..].to_owned(),
        )
    };
    if descend_prefix
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(PickerPathError::DotSegment);
    }
    let enumeration_directory = PickerPath::new(enumeration_spelling.to_owned())?;

    Ok(ParsedPickerPath {
        display: input.to_owned(),
        exact_directory,
        enumeration_directory,
        descend_prefix,
        leaf_filter,
        trailing_separator,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryRowError {
    InvalidName,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct DirectoryRow {
    name: String,
}

/// A defensively bounded one-level directory result from a source.
impl fmt::Debug for DirectoryRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DirectoryRow(<redacted>)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct DirectoryListing {
    rows: Vec<DirectoryRow>,
    truncated: bool,
}

impl fmt::Debug for DirectoryListing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DirectoryListing(<redacted>)")
    }
}

impl DirectoryListing {
    #[cfg(test)]
    pub(crate) fn new(rows: Vec<DirectoryRow>) -> Self {
        Self::bounded(rows, false)
    }

    pub(crate) fn bounded(mut rows: Vec<DirectoryRow>, source_truncated: bool) -> Self {
        let truncated = source_truncated || rows.len() > MAXIMUM_DIRECTORY_ROWS;
        rows.truncate(MAXIMUM_DIRECTORY_ROWS);
        Self { rows, truncated }
    }

    pub(crate) fn rows(&self) -> &[DirectoryRow] {
        &self.rows
    }

    pub(crate) const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

impl DirectoryRow {
    pub(crate) fn new(name: String) -> Result<Self, DirectoryRowError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.chars().any(char::is_control)
        {
            return Err(DirectoryRowError::InvalidName);
        }
        Ok(Self { name })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone)]
struct DirectoryRowMatch {
    row: DirectoryRow,
    matched_indices: Vec<usize>,
}

fn match_directory_rows(
    parsed: &ParsedPickerPath,
    entries: &[DirectoryRow],
) -> Vec<DirectoryRowMatch> {
    let reveal_hidden = parsed.reveals_hidden_directories();
    let mut visible = entries
        .iter()
        .filter(|entry| reveal_hidden || !entry.name.starts_with('.'))
        .collect::<Vec<_>>();
    visible.sort_by(|left, right| {
        let folded = left.name.to_lowercase().cmp(&right.name.to_lowercase());
        if folded == Ordering::Equal {
            left.name.cmp(&right.name)
        } else {
            folded
        }
    });
    fuzzy_filter(&visible, &parsed.leaf_filter, |entry| {
        FuzzyTarget::new(entry.name())
    })
    .into_iter()
    .map(|matched| DirectoryRowMatch {
        row: visible[matched.item_index()].clone(),
        matched_indices: matched.field_highlight_indices(0),
    })
    .collect()
}

pub(super) fn descend_query(
    parsed: &ParsedPickerPath,
    row: &DirectoryRow,
) -> Result<PickerPath, PickerPathError> {
    PickerPath::new(format!("{}{}/", parsed.descend_prefix, row.name()))
}

/// Returns the path that lists the directory enclosing the one `parsed` lists.
fn enclosing_directory_query(parsed: &ParsedPickerPath, home: &PickerPath) -> Option<String> {
    let listed = parsed
        .enumeration_directory()
        .as_str()
        .trim_end_matches('/');
    let listed = if listed == "~" {
        home.as_str().trim_end_matches('/')
    } else {
        listed
    };
    let separator = listed.rfind('/')?;
    Some(format!("{}/", listed[..separator].trim_end_matches('/')))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactPathState {
    ReadableDirectory,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DirectoryPickerEvent {
    StateChanged,
    Dismissed,
    Confirmed(PinnedDirectory),
    /// The picker closed so System Directory Selection can choose the directory instead.
    SystemSelectionRequested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryPickerStatus {
    DiscoveringAccount,
    Loading,
    Readable,
    Missing,
    NotDirectory,
    PermissionDenied,
    ConnectionLost,
    SessionUnavailable,
    UnsupportedLoginShell,
    Other,
    Invalid(PickerPathError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryPickerBusy {
    CreationAlert,
    Creating,
    Validating,
    AwaitingActivation,
}

/// Identifies one presented row within one operation generation.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DirectoryPickerItemId {
    /// The row that opens the directory enclosing the listed one.
    Enclosing { operation_generation: u64 },
    /// One listed child directory.
    Child {
        row: DirectoryRow,
        directory: PickerPath,
        operation_generation: u64,
    },
}

#[derive(Clone)]
struct LoadedDirectorySnapshot {
    directory: PickerPath,
    listing: DirectoryListing,
}

struct RefreshCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    parsed: ParsedPickerPath,
    listing: Option<Result<DirectoryListing, DirectorySourceError>>,
    probe: Result<ExactPathState, DirectorySourceError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryValidationKind {
    Existing,
    Creation,
}

struct ValidationCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    directory: PickerPath,
    result: Result<PinnedDirectory, DirectorySourceError>,
}

/// One machine's directory chooser built on the reusable Command Palette.
pub(super) struct DirectoryPicker {
    source: Rc<dyn DirectorySource>,
    /// Labels the confirm action's menu item that hands off to System Directory Selection.
    system_selection: Option<SharedString>,
    palette: Entity<CommandPalette<DirectoryPickerItemId>>,
    opening: bool,
    open: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    home: Option<PickerPath>,
    parsed: Option<ParsedPickerPath>,
    snapshot: Option<LoadedDirectorySnapshot>,
    rows: Vec<DirectoryRowMatch>,
    rows_directory: Option<PickerPath>,
    listing_error: Option<DirectorySourceError>,
    listing_truncated: bool,
    status: DirectoryPickerStatus,
    busy: Option<DirectoryPickerBusy>,
    creation_alert: Option<ModalPresentationHandle>,
    home_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    validation_task: Option<Task<()>>,
}

impl EventEmitter<DirectoryPickerEvent> for DirectoryPicker {}

impl DirectoryPicker {
    /// Creates a closed picker for the machine `source` reaches, whose search line shows
    /// `placeholder` until a path is typed.
    pub(super) fn new(
        source: Rc<dyn DirectorySource>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A remote machine leads the path in scp form, so paths on two machines never look alike.
        let prefix = source
            .machine_name()
            .map(|machine| SharedString::from(format!("{machine}:")));
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new(placeholder, Vec::new(), window, cx);
            palette.set_query_prefix(prefix, cx);
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |picker, _, event: &CommandPaletteEvent<DirectoryPickerItemId>, window, cx| {
                picker.reduce_palette_event(event, window, cx);
            },
        )
        .detach();
        Self {
            source,
            system_selection: None,
            palette,
            opening: false,
            open: false,
            lifecycle_generation: 0,
            operation_generation: 0,
            home: None,
            parsed: None,
            snapshot: None,
            rows: Vec::new(),
            rows_directory: None,
            listing_error: None,
            listing_truncated: false,
            status: DirectoryPickerStatus::DiscoveringAccount,
            busy: None,
            creation_alert: None,
            home_task: None,
            refresh_task: None,
            validation_task: None,
        }
    }

    pub(super) fn with_system_selection(mut self, label: SharedString) -> Self {
        self.system_selection = Some(label);
        self
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    fn open_with_replacement(
        &mut self,
        replacement: Option<CommandPaletteReplacementFocus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.open || self.opening {
            self.refocus_path(window, cx);
            return false;
        }
        self.opening = true;
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        self.home = None;
        self.parsed = None;
        self.snapshot = None;
        self.clear_rows();
        self.listing_error = None;
        self.listing_truncated = false;
        self.status = DirectoryPickerStatus::DiscoveringAccount;
        self.busy = None;
        self.creation_alert = None;
        self.home_task.take();
        self.refresh_task.take();
        self.validation_task.take();
        self.palette.update(cx, |palette, cx| {
            palette.set_query_editable(true, cx);
            palette.set_dismissible(true, cx);
            palette.set_escape_cancellable(false, cx);
            if let Some(replacement) = replacement {
                palette.open_replacing(replacement, window, cx);
            } else {
                palette.open(window, cx);
            }
        });
        true
    }

    #[cfg(test)]
    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    pub(super) const fn blocks_terminal_input(&self) -> bool {
        self.open || self.opening
    }

    pub(super) fn refocus_path(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.palette
                .update(cx, |palette, cx| palette.focus_editor(window, cx));
        }
    }

    #[cfg(test)]
    pub(super) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.blocks_terminal_input() || self.busy.is_some() {
            return false;
        }
        let pending_open = self.opening && !self.open;
        let dismissed = self
            .palette
            .update(cx, |palette, cx| palette.dismiss(window, cx));
        if dismissed && pending_open {
            self.finish_close(CommandPaletteCloseReason::Programmatic, cx);
        }
        dismissed
    }

    /// Cancels this picker and its exact pending requests, including a busy selection.
    pub(super) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let alert = self.creation_alert.take();
        self.finish_close(CommandPaletteCloseReason::Programmatic, cx);
        if let Some(alert) = alert {
            let _ = alert.dismiss(window, cx);
        }
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx);
        });
    }

    pub(super) fn complete_activation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open || self.busy != Some(DirectoryPickerBusy::AwaitingActivation) {
            return false;
        }
        self.busy = None;
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx)
        })
    }

    pub(super) fn activation_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(DirectoryPickerBusy::AwaitingActivation) {
            self.busy = None;
            self.status = DirectoryPickerStatus::Other;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<DirectoryPickerItemId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.opening = false;
                self.open = true;
                self.palette
                    .update(cx, |palette, cx| palette.set_query(HOME_DISPLAY, cx));
                self.start_home_discovery(window, cx);
                self.publish(cx);
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(reason)) => {
                self.finish_close(*reason, cx);
            }
            CommandPaletteEvent::QueryChanged(query) => {
                self.refresh_for_input(query.clone(), window, cx);
            }
            CommandPaletteEvent::Activated(activation) => match activation.item_id() {
                DirectoryPickerItemId::Enclosing {
                    operation_generation,
                } => {
                    if *operation_generation == self.operation_generation {
                        self.open_enclosing_directory(window, cx);
                    }
                }
                DirectoryPickerItemId::Child {
                    row,
                    directory,
                    operation_generation,
                } => {
                    self.descend_to(row, directory, *operation_generation, window, cx);
                }
            },
            CommandPaletteEvent::HeaderAction(action) if action == CONFIRM_ACTION => {
                self.confirm_current(window, cx);
            }
            CommandPaletteEvent::HeaderAction(action) if action == SYSTEM_SELECTION_ACTION => {
                self.request_system_selection(window, cx);
            }
            _ => {}
        }
    }

    /// Closes the picker and hands the choice to System Directory Selection.
    fn request_system_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() || self.system_selection.is_none() {
            return;
        }
        self.end_session(Some(DirectoryPickerEvent::SystemSelectionRequested), cx);
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx);
        });
    }

    fn finish_close(&mut self, reason: CommandPaletteCloseReason, cx: &mut Context<Self>) {
        let outcome = match reason {
            CommandPaletteCloseReason::Completed => None,
            _ => Some(DirectoryPickerEvent::Dismissed),
        };
        self.end_session(outcome, cx);
    }

    /// Clears the session's state and requests, then reports how it ended.
    fn end_session(&mut self, outcome: Option<DirectoryPickerEvent>, cx: &mut Context<Self>) {
        if !self.open && !self.opening {
            return;
        }
        self.opening = false;
        self.open = false;
        self.home = None;
        self.parsed = None;
        self.snapshot = None;
        self.clear_rows();
        self.listing_error = None;
        self.listing_truncated = false;
        self.busy = None;
        self.creation_alert = None;
        self.home_task.take();
        self.refresh_task.take();
        self.validation_task.take();
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        if let Some(outcome) = outcome {
            cx.emit(outcome);
        }
        cx.emit(DirectoryPickerEvent::StateChanged);
        cx.notify();
    }

    fn start_home_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let task = self.source.discover_home();
        self.home_task.take();
        self.home_task = Some(cx.spawn_in(window, async move |picker, cx| {
            let result = task.await;
            let _ = picker.update_in(cx, |picker, window, cx| {
                if !picker.open
                    || picker.lifecycle_generation != lifecycle_generation
                    || picker.operation_generation != operation_generation
                {
                    return;
                }
                match result {
                    Ok(home) => {
                        picker.home = Some(home);
                        let query = picker.palette.read(cx).query().to_owned();
                        picker.refresh_for_input(query, window, cx);
                    }
                    Err(error) => {
                        picker.status = status_for_source_error(error);
                        picker.clear_rows();
                        picker.publish(cx);
                    }
                }
            });
        }));
    }

    fn refresh_for_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() || self.home.is_none() {
            return;
        }
        let parsed = match parse_picker_path(&value) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.refresh_task.take();
                self.parsed = None;
                self.status = DirectoryPickerStatus::Invalid(error);
                self.listing_error = None;
                self.listing_truncated = false;
                self.operation_generation = self.operation_generation.wrapping_add(1);
                self.clear_rows();
                self.publish(cx);
                return;
            }
        };
        let listing_needed = !self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.directory == *parsed.enumeration_directory());
        self.parsed = Some(parsed.clone());
        self.status = DirectoryPickerStatus::Loading;
        self.operation_generation = self.operation_generation.wrapping_add(1);
        if listing_needed {
            self.listing_error = None;
            self.listing_truncated = false;
            self.clear_rows();
        } else {
            self.rebuild_rows();
        }
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let listing = listing_needed.then(|| {
            self.source
                .list_directories(parsed.enumeration_directory().clone())
        });
        let probe = self
            .source
            .probe_exact_path(parsed.exact_directory().clone());
        self.refresh_task.take();
        self.refresh_task = Some(cx.spawn_in(window, async move |picker, cx| {
            let listing = match listing {
                Some(task) => Some(task.await),
                None => None,
            };
            let completion = RefreshCompletion {
                lifecycle_generation,
                operation_generation,
                parsed,
                listing,
                probe: probe.await,
            };
            let _ = picker.update_in(cx, |picker, _, cx| {
                picker.finish_refresh(completion, cx);
            });
        }));
        self.publish(cx);
    }

    fn finish_refresh(&mut self, completion: RefreshCompletion, cx: &mut Context<Self>) {
        let Some(current) = self.parsed.as_ref() else {
            return;
        };
        if !self.open
            || self.lifecycle_generation != completion.lifecycle_generation
            || self.operation_generation != completion.operation_generation
            || current.exact_directory() != completion.parsed.exact_directory()
            || current.enumeration_directory() != completion.parsed.enumeration_directory()
        {
            return;
        }
        match completion.listing {
            Some(Ok(listing)) => {
                self.listing_error = None;
                self.listing_truncated = listing.is_truncated();
                self.snapshot = Some(LoadedDirectorySnapshot {
                    directory: completion.parsed.enumeration_directory().clone(),
                    listing,
                });
                self.rebuild_rows();
            }
            Some(Err(error)) => {
                self.snapshot = None;
                self.listing_error = Some(error);
                self.listing_truncated = false;
                self.clear_rows();
            }
            None => {}
        }
        self.status = match completion.probe {
            Ok(ExactPathState::ReadableDirectory) => DirectoryPickerStatus::Readable,
            Ok(ExactPathState::Missing) => DirectoryPickerStatus::Missing,
            Err(error) => status_for_source_error(error),
        };
        self.publish(cx);
    }

    fn rebuild_rows(&mut self) {
        let (Some(parsed), Some(snapshot)) = (self.parsed.as_ref(), self.snapshot.as_ref()) else {
            return;
        };
        if snapshot.directory != *parsed.enumeration_directory() {
            return;
        }
        self.rows = match_directory_rows(parsed, snapshot.listing.rows());
        self.rows_directory = Some(snapshot.directory.clone());
    }

    fn clear_rows(&mut self) {
        self.rows.clear();
        self.rows_directory = None;
    }

    fn descend_to(
        &mut self,
        row: &DirectoryRow,
        directory: &PickerPath,
        operation_generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some()
            || operation_generation != self.operation_generation
            || self
                .parsed
                .as_ref()
                .map(ParsedPickerPath::enumeration_directory)
                != Some(directory)
        {
            return;
        }
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let Ok(directory) = descend_query(parsed, row) else {
            self.status = DirectoryPickerStatus::Other;
            self.publish(cx);
            return;
        };
        let query = directory.as_str().to_owned();
        if !self.palette.read(cx).can_set_query_exactly(&query, cx) {
            self.status = DirectoryPickerStatus::Other;
            self.publish(cx);
            return;
        }
        self.palette
            .update(cx, |palette, cx| palette.set_query(query, cx));
        self.refocus_path(window, cx);
    }

    fn enclosing_directory(&self) -> Option<String> {
        enclosing_directory_query(self.parsed.as_ref()?, self.home.as_ref()?)
    }

    fn open_enclosing_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(query) = self.enclosing_directory() else {
            return;
        };
        if !self.palette.read(cx).can_set_query_exactly(&query, cx) {
            self.status = DirectoryPickerStatus::Other;
            self.publish(cx);
            return;
        }
        self.palette
            .update(cx, |palette, cx| palette.set_query(query, cx));
        self.refocus_path(window, cx);
    }

    fn confirm_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm() {
            return;
        }
        let Some(parsed) = self.parsed.clone() else {
            return;
        };
        match self.status {
            DirectoryPickerStatus::Readable => {
                self.start_validation(
                    parsed.exact_directory().clone(),
                    DirectoryValidationKind::Existing,
                    window,
                    cx,
                );
            }
            DirectoryPickerStatus::Missing => self.present_creation_alert(parsed, window, cx),
            _ => {}
        }
    }

    fn present_creation_alert(
        &mut self,
        parsed: ParsedPickerPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(DirectoryPickerBusy::CreationAlert);
        self.publish(cx);
        let directory = parsed.exact_directory().clone();
        let expected = directory.clone();
        let picker = cx.weak_entity();
        let window_handle = window.window_handle();
        let alert = Alert::new(
            ModalId::new(CREATE_ALERT_ID),
            "Create directory",
            "Create Directory?",
            format!(
                "Create {}? Missing parent directories will also be created.",
                parsed.display()
            ),
            vec![
                ModalAction::new(
                    true,
                    "Create Directory",
                    ModalActionRole::Affirmative,
                    "directory-picker-create",
                )
                .default_action(true),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "directory-picker-create-cancel",
                ),
            ],
        )
        .present(window, cx, move |outcome, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = picker.update(cx, |picker, cx| {
                    picker.creation_alert = None;
                    if !picker.open
                        || picker.busy != Some(DirectoryPickerBusy::CreationAlert)
                        || picker
                            .parsed
                            .as_ref()
                            .map(|parsed| parsed.exact_directory())
                            != Some(&expected)
                    {
                        return;
                    }
                    if matches!(
                        outcome,
                        AlertOutcome::Activated {
                            action_id: true,
                            ..
                        }
                    ) {
                        picker.start_validation(
                            directory,
                            DirectoryValidationKind::Creation,
                            window,
                            cx,
                        );
                    } else {
                        picker.busy = None;
                        picker.publish(cx);
                        picker.refocus_path(window, cx);
                    }
                });
            });
        });
        match alert {
            Ok(handle) => self.creation_alert = Some(handle),
            Err(_) => {
                self.busy = None;
                self.status = DirectoryPickerStatus::Other;
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn start_validation(
        &mut self,
        directory: PickerPath,
        kind: DirectoryValidationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        self.busy = Some(match kind {
            DirectoryValidationKind::Existing => DirectoryPickerBusy::Validating,
            DirectoryValidationKind::Creation => DirectoryPickerBusy::Creating,
        });
        let source = Rc::clone(&self.source);
        let request = directory.clone();
        self.validation_task.take();
        self.validation_task = Some(cx.spawn_in(window, async move |picker, cx| {
            let result = match kind {
                DirectoryValidationKind::Creation => {
                    match source.create_directory_recursively(request.clone()).await {
                        Ok(()) => source.pin(request).await,
                        Err(error) => Err(error),
                    }
                }
                DirectoryValidationKind::Existing => source.pin(request).await,
            };
            let completion = ValidationCompletion {
                lifecycle_generation,
                operation_generation,
                directory,
                result,
            };
            let _ = picker.update_in(cx, |picker, window, cx| {
                picker.finish_validation(completion, window, cx);
            });
        }));
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
            || self.parsed.as_ref().map(|parsed| parsed.exact_directory())
                != Some(&completion.directory)
        {
            return;
        }
        match completion.result {
            Ok(pinned) => {
                if self.home.is_none() {
                    return;
                }
                self.busy = Some(DirectoryPickerBusy::AwaitingActivation);
                self.sync_palette(cx);
                cx.emit(DirectoryPickerEvent::Confirmed(pinned));
                cx.notify();
            }
            Err(error) => {
                self.busy = None;
                self.status = status_for_source_error(error);
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn can_confirm(&self) -> bool {
        self.busy.is_none()
            && matches!(
                self.status,
                DirectoryPickerStatus::Readable | DirectoryPickerStatus::Missing
            )
            && self
                .parsed
                .as_ref()
                .is_some_and(ParsedPickerPath::names_openable_directory)
    }

    /// Returns the search line's action that opens the exact path, creating it when missing, with
    /// System Directory Selection in its menu when offered.
    fn confirm_action(&self, cx: &App) -> CommandPalettePrimaryAction {
        let action = CommandPalettePrimaryAction::new(CONFIRM_ACTION, "Open");
        let action = match crate::desktop_profile::DesktopPresentation::get(cx)
            .shortcut(&spaceterm_ui::CommandPaletteConfirm)
        {
            Some(shortcut) => action.shortcut(shortcut),
            None => action,
        }
        .disabled(!self.can_confirm())
        .menu_disabled(self.busy.is_some())
        .debug_selector(CONFIRM_ACTION);
        match &self.system_selection {
            Some(label) => action.menu_item(SYSTEM_SELECTION_ACTION, label.clone()),
            None => action,
        }
    }

    /// Returns the listed rows: the enclosing directory, then the matching children.
    fn palette_items(&self) -> Vec<CommandPaletteItem<DirectoryPickerItemId>> {
        let Some(directory) = self.rows_directory.as_ref() else {
            return Vec::new();
        };
        let enclosing = (self.enclosing_directory().is_some()
            && (!self.rows.is_empty() || self.status == DirectoryPickerStatus::Readable))
            .then(|| enclosing_directory_item(self.operation_generation));
        enclosing
            .into_iter()
            .chain(self.rows.iter().cloned().map(|matched| {
                child_directory_item(matched.row, directory.clone(), self.operation_generation)
                    .matched_indices(matched.matched_indices)
            }))
            .collect()
    }

    /// Returns the caption after the last row: the listing omits directories, or a readable
    /// directory has none beneath the Go Back row.
    fn results_note(&self) -> Option<&'static str> {
        if self.listing_truncated {
            Some(TRUNCATED_LISTING_NOTE)
        } else if self.rows.is_empty() && self.status == DirectoryPickerStatus::Readable {
            Some(NO_SUBDIRECTORIES)
        } else {
            None
        }
    }

    /// Explains the list area whenever it presents no child directory.
    fn empty_state(&self) -> CommandPaletteEmpty {
        let notice = self.empty_notice();
        let icon = notice.icon;
        let empty = CommandPaletteEmpty::new(notice.title)
            .icon(move |tint, size| Icon::new(icon, size, tint).into_any_element());
        match notice.description {
            Some(description) => empty.description(description),
            None => empty,
        }
    }

    /// An unusable exact path outranks the listing, and a missing path offers creation.
    fn empty_notice(&self) -> EmptyNotice {
        match self.status {
            DirectoryPickerStatus::NotDirectory => {
                return EmptyNotice::new(IconName::File, "Not a directory")
                    .description("Only a directory can be a Pinned Directory.");
            }
            DirectoryPickerStatus::PermissionDenied => {
                return EmptyNotice::new(IconName::Lock, "Permission denied")
                    .description("Your account can\u{2019}t open this directory.");
            }
            DirectoryPickerStatus::ConnectionLost => return CONNECTION_LOST_NOTICE,
            DirectoryPickerStatus::SessionUnavailable => return SESSION_UNAVAILABLE_NOTICE,
            DirectoryPickerStatus::UnsupportedLoginShell => {
                return UNSUPPORTED_LOGIN_SHELL_NOTICE;
            }
            DirectoryPickerStatus::Other => {
                return EmptyNotice::new(IconName::CircleAlert, "Can\u{2019}t read this directory");
            }
            DirectoryPickerStatus::Invalid(error) => {
                return EmptyNotice::new(IconName::CircleAlert, "Invalid path")
                    .description(error.message());
            }
            DirectoryPickerStatus::Missing => {
                return EmptyNotice::new(IconName::FolderPlus, "Directory doesn\u{2019}t exist")
                    .description("Open creates it and pins this Workspace to it.");
            }
            DirectoryPickerStatus::DiscoveringAccount
            | DirectoryPickerStatus::Loading
            | DirectoryPickerStatus::Readable => {}
        }
        match self.listing_error {
            None => EmptyNotice::new(IconName::Folder, NO_SUBDIRECTORIES),
            Some(DirectorySourceError::ConnectionLost) => CONNECTION_LOST_NOTICE,
            Some(DirectorySourceError::SessionUnavailable) => SESSION_UNAVAILABLE_NOTICE,
            Some(DirectorySourceError::UnsupportedLoginShell) => UNSUPPORTED_LOGIN_SHELL_NOTICE,
            Some(DirectorySourceError::PermissionDenied) => {
                EmptyNotice::new(IconName::Lock, "Permission denied")
                    .description("Your account can\u{2019}t list this directory.")
            }
            Some(DirectorySourceError::Missing) => EmptyNotice::new(
                IconName::CircleAlert,
                "Enclosing directory doesn\u{2019}t exist",
            ),
            Some(DirectorySourceError::NotDirectory) => EmptyNotice::new(
                IconName::CircleAlert,
                "Enclosing path isn\u{2019}t a directory",
            ),
            Some(DirectorySourceError::Other) => {
                EmptyNotice::new(IconName::CircleAlert, "Can\u{2019}t list this directory")
            }
        }
    }

    /// Results are pending until the home and the exact path settle, unless rows already show.
    fn awaiting_results(&self) -> bool {
        self.busy.is_some()
            || (self.rows.is_empty()
                && matches!(
                    self.status,
                    DirectoryPickerStatus::DiscoveringAccount | DirectoryPickerStatus::Loading
                ))
    }

    fn sync_palette(&self, cx: &mut Context<Self>) {
        let busy = self.busy.is_some();
        let awaiting_results = self.awaiting_results();
        let empty = self.empty_state();
        let results_note = self.results_note().map(SharedString::from);
        let items = self.palette_items();
        let first_child = items
            .iter()
            .find(|item| matches!(item.id(), DirectoryPickerItemId::Child { .. }))
            .map(|item| item.id().clone());
        let confirm_action = self.confirm_action(cx);
        let query_note = (self.status == DirectoryPickerStatus::Missing)
            .then(|| SharedString::from(NEW_DIRECTORY_NOTE));
        self.palette.update(cx, |palette, cx| {
            // A stable selection survives republishing; otherwise Return descends into the first
            // child rather than leaving, and the confirm key pins regardless of selection.
            let retained = palette
                .selected_item_id()
                .filter(|selected| items.iter().any(|item| item.id() == *selected))
                .cloned();
            palette.set_preferred_item(retained.or(first_child), cx);
            palette.set_items(items, cx);
            palette.set_primary_action(Some(confirm_action), cx);
            palette.set_query_note(query_note, cx);
            palette.set_empty(empty, cx);
            palette.set_results_note(results_note, cx);
            palette.set_loading(awaiting_results, cx);
            palette.set_query_editable(!busy, cx);
            palette.set_dismissible(!busy, cx);
            palette.set_escape_cancellable(
                matches!(self.busy, Some(DirectoryPickerBusy::Validating)),
                cx,
            );
        });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.sync_palette(cx);
        cx.emit(DirectoryPickerEvent::StateChanged);
        cx.notify();
    }

    #[cfg(test)]
    fn row_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|matched| matched.row.name().to_owned())
            .collect()
    }
}

impl PickerPathError {
    const fn message(self) -> &'static str {
        match self {
            Self::Relative => "Enter an absolute path beginning with / or ~/.",
            Self::BareTilde => "Use ~/ to open your home directory.",
            Self::UnsupportedTilde => "Only ~/ is supported for home-relative paths.",
            Self::InvalidControlCharacter => "Paths cannot contain control characters.",
            Self::DotSegment => "Use Go Back to open an enclosing directory.",
        }
    }
}

/// Keeps a hierarchy shortcut from mutating Workspaces, Tabs, or Panes behind the open picker. The
/// application's hierarchy actions are registered above the Command Palette.
fn block_parent_action<A: Action>(_: &A, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

impl Render for DirectoryPicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .when(self.blocks_terminal_input(), |picker| {
                picker
                    .capture_action(block_parent_action::<NewWorkspace>)
                    .capture_action(block_parent_action::<super::NewRemoteWorkspace>)
                    .capture_action(block_parent_action::<super::OpenLocalDirectory>)
                    .capture_action(block_parent_action::<super::OpenRemoteDirectory>)
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
                    .capture_action(block_parent_action::<NextTab>)
                    .capture_action(block_parent_action::<PreviousTab>)
                    .capture_action(block_parent_action::<MoveTabRight>)
                    .capture_action(block_parent_action::<MoveTabLeft>)
                    .capture_action(block_parent_action::<ClosePane>)
                    .capture_action(block_parent_action::<CloseTab>)
                    .capture_action(block_parent_action::<SplitRight>)
                    .capture_action(block_parent_action::<SplitDown>)
                    .capture_action(block_parent_action::<FocusPaneLeft>)
                    .capture_action(block_parent_action::<FocusPaneRight>)
                    .capture_action(block_parent_action::<FocusPaneUp>)
                    .capture_action(block_parent_action::<FocusPaneDown>)
                    .capture_action(block_parent_action::<FocusPreviousPane>)
                    .capture_action(block_parent_action::<FocusNextPane>)
                    .capture_action(block_parent_action::<TogglePaneZoom>)
                    .capture_action(block_parent_action::<OpenTerminalFind>)
                    .capture_action(block_parent_action::<FindNext>)
                    .capture_action(block_parent_action::<FindPrevious>)
                    .capture_action(block_parent_action::<CloseTerminalFind>)
            })
            .child(self.palette.clone())
    }
}

fn enclosing_directory_item(
    operation_generation: u64,
) -> CommandPaletteItem<DirectoryPickerItemId> {
    CommandPaletteItem::new(
        DirectoryPickerItemId::Enclosing {
            operation_generation,
        },
        "Go Back",
    )
    .outside_default_selection()
    .leading_icon(|foreground, size| {
        Icon::new(IconName::FolderOutput, size, foreground).into_any_element()
    })
    .debug_selector(ENCLOSING_ROW)
}

fn child_directory_item(
    row: DirectoryRow,
    directory: PickerPath,
    operation_generation: u64,
) -> CommandPaletteItem<DirectoryPickerItemId> {
    let selector = format!("directory-picker-row-{}", row.name());
    let label = row.name().to_owned();
    let id = DirectoryPickerItemId::Child {
        row,
        directory,
        operation_generation,
    };
    CommandPaletteItem::new(id, label)
        .leading_icon(move |foreground, size| {
            Icon::new(IconName::Folder, size, foreground).into_any_element()
        })
        .debug_selector(selector)
}

fn status_for_source_error(error: DirectorySourceError) -> DirectoryPickerStatus {
    match error {
        DirectorySourceError::ConnectionLost => DirectoryPickerStatus::ConnectionLost,
        DirectorySourceError::SessionUnavailable => DirectoryPickerStatus::SessionUnavailable,
        DirectorySourceError::Missing => DirectoryPickerStatus::Missing,
        DirectorySourceError::NotDirectory => DirectoryPickerStatus::NotDirectory,
        DirectorySourceError::PermissionDenied => DirectoryPickerStatus::PermissionDenied,
        DirectorySourceError::UnsupportedLoginShell => DirectoryPickerStatus::UnsupportedLoginShell,
        DirectorySourceError::Other => DirectoryPickerStatus::Other,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::future::pending;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use gpui::{
        BackgroundExecutor, Keystroke, Modifiers, Task, TestAppContext, VisualTestContext, div,
    };

    use super::*;
    use crate::domain::{RemoteDirectory, RemoteDirectoryIdentity};
    use crate::ssh::fake_remote_utility_server::FakeRemoteUtilityServer;
    use crate::ssh::remote_account::RemoteWorkspaceAccount;

    #[test]
    fn remote_picker_wrapper_debug_should_redact_account_paths_and_rows() {
        let parsed = parse_picker_path("/sensitive/project").unwrap();
        let row = DirectoryRow::new("sensitive-child".to_owned()).unwrap();
        let listing = DirectoryListing::new(vec![row.clone()]);
        let account = RemoteWorkspaceAccount::from_validated_login_shell(
            "sensitive-user".to_owned(),
            RemoteDirectoryIdentity::new("/sensitive/home".to_owned()).unwrap(),
            crate::ssh::command::ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
        )
        .unwrap();
        let pinned = PinnedDirectory::Remote {
            directory: RemoteDirectory::new("/sensitive/project".to_owned()).unwrap(),
            identity: RemoteDirectoryIdentity::new("/sensitive/project".to_owned()).unwrap(),
        };
        let event = DirectoryPickerEvent::Confirmed(pinned);

        for debug in [
            format!("{parsed:?}"),
            format!("{row:?}"),
            format!("{listing:?}"),
            format!("{account:?}"),
            format!("{event:?}"),
        ] {
            assert!(!debug.contains("sensitive"));
        }
    }

    #[derive(Default)]
    struct ScriptedRemoteDirectoryProviderState {
        accounts: VecDeque<Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>>>,
        listings: VecDeque<Task<Result<DirectoryListing, RemoteDirectoryProviderError>>>,
        probes: VecDeque<Task<Result<ExactPathState, RemoteDirectoryProviderError>>>,
        creations: VecDeque<Task<Result<(), RemoteDirectoryProviderError>>>,
        validations: VecDeque<Task<Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>>>,
        listed_directories: Vec<RemoteDirectory>,
        created_directories: Vec<RemoteDirectory>,
        validated_directories: Vec<RemoteDirectory>,
    }

    #[derive(Clone, Default)]
    struct ScriptedRemoteDirectoryProvider {
        state: Arc<Mutex<ScriptedRemoteDirectoryProviderState>>,
    }

    impl RemoteDirectoryProvider for ScriptedRemoteDirectoryProvider {
        fn discover_account(
            &self,
        ) -> Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>> {
            self.state
                .lock()
                .unwrap()
                .accounts
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteDirectoryProviderError::Other)))
        }

        fn list_directories(
            &self,
            directory: RemoteDirectory,
        ) -> Task<Result<DirectoryListing, RemoteDirectoryProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.listed_directories.push(directory);
            state
                .listings
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteDirectoryProviderError::Other)))
        }

        fn probe_exact_path(
            &self,
            _directory: RemoteDirectory,
        ) -> Task<Result<ExactPathState, RemoteDirectoryProviderError>> {
            self.state
                .lock()
                .unwrap()
                .probes
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteDirectoryProviderError::Other)))
        }

        fn create_directory_recursively(
            &self,
            directory: RemoteDirectory,
        ) -> Task<Result<(), RemoteDirectoryProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.created_directories.push(directory);
            state
                .creations
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteDirectoryProviderError::Other)))
        }

        fn validate_physical_identity(
            &self,
            directory: RemoteDirectory,
        ) -> Task<Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.validated_directories.push(directory);
            state
                .validations
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteDirectoryProviderError::Other)))
        }
    }

    struct PendingOperationDrop(Arc<AtomicUsize>);

    impl Drop for PendingOperationDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

    struct CancellationTrackingRemoteDirectoryProvider {
        executor: BackgroundExecutor,
        dropped_operations: Arc<AtomicUsize>,
    }

    impl CancellationTrackingRemoteDirectoryProvider {
        fn pending<T: Send + 'static>(&self) -> Task<Result<T, RemoteDirectoryProviderError>> {
            let dropped_operations = Arc::clone(&self.dropped_operations);
            self.executor.spawn(async move {
                let _drop = PendingOperationDrop(dropped_operations);
                pending().await
            })
        }
    }

    impl RemoteDirectoryProvider for CancellationTrackingRemoteDirectoryProvider {
        fn discover_account(
            &self,
        ) -> Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>> {
            Task::ready(Ok(remote_account()))
        }

        fn list_directories(
            &self,
            _: RemoteDirectory,
        ) -> Task<Result<DirectoryListing, RemoteDirectoryProviderError>> {
            self.pending()
        }

        fn probe_exact_path(
            &self,
            _: RemoteDirectory,
        ) -> Task<Result<ExactPathState, RemoteDirectoryProviderError>> {
            self.pending()
        }

        fn create_directory_recursively(
            &self,
            _: RemoteDirectory,
        ) -> Task<Result<(), RemoteDirectoryProviderError>> {
            self.pending()
        }

        fn validate_physical_identity(
            &self,
            _: RemoteDirectory,
        ) -> Task<Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>> {
            self.pending()
        }
    }

    struct DirectoryPickerHarness {
        picker: gpui::Entity<DirectoryPicker>,
    }

    impl gpui::Render for DirectoryPickerHarness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            spaceterm_ui::ModalLayer::new(div().size_full().child(self.picker.clone()))
        }
    }

    fn remote_account() -> RemoteWorkspaceAccount {
        RemoteWorkspaceAccount::from_validated_login_shell(
            "tester".to_owned(),
            RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
            crate::ssh::command::ValidatedRemoteLoginShell::new("/bin/zsh".to_owned()).unwrap(),
        )
        .unwrap()
    }

    fn scripted_provider(
        listings: impl IntoIterator<Item = Result<Vec<DirectoryRow>, RemoteDirectoryProviderError>>,
        probes: impl IntoIterator<Item = Result<ExactPathState, RemoteDirectoryProviderError>>,
        creations: impl IntoIterator<Item = Result<(), RemoteDirectoryProviderError>>,
        validations: impl IntoIterator<
            Item = Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>,
        >,
    ) -> Arc<ScriptedRemoteDirectoryProvider> {
        Arc::new(ScriptedRemoteDirectoryProvider {
            state: Arc::new(Mutex::new(ScriptedRemoteDirectoryProviderState {
                accounts: [Task::ready(Ok(remote_account()))].into(),
                listings: listings
                    .into_iter()
                    .map(|result| result.map(DirectoryListing::new))
                    .map(Task::ready)
                    .collect(),
                probes: probes.into_iter().map(Task::ready).collect(),
                creations: creations.into_iter().map(Task::ready).collect(),
                validations: validations.into_iter().map(Task::ready).collect(),
                ..ScriptedRemoteDirectoryProviderState::default()
            })),
        })
    }

    fn directory_picker(
        provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<DirectoryPicker>,
        Rc<RefCell<Vec<DirectoryPickerEvent>>>,
        &mut VisualTestContext,
    ) {
        directory_picker_offering(provider, None, cx)
    }

    fn directory_picker_offering<'a>(
        injected: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
        system_selection: Option<&'static str>,
        cx: &'a mut TestAppContext,
    ) -> (
        gpui::Entity<DirectoryPicker>,
        Rc<RefCell<Vec<DirectoryPickerEvent>>>,
        &'a mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                let picker = DirectoryPicker::new(
                    Rc::new(RemoteDirectorySource::new(injected, "orb")),
                    "Pin to Directory",
                    window,
                    cx,
                );
                match system_selection {
                    Some(label) => picker.with_system_selection(label.into()),
                    None => picker,
                }
            });
            cx.subscribe(&picker, move |_, _, event: &DirectoryPickerEvent, _| {
                recorded_events.borrow_mut().push(event.clone());
            })
            .detach();
            DirectoryPickerHarness { picker }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            picker.update(cx, |picker, cx| assert!(picker.open(window, cx)));
        });
        cx.run_until_parked();
        (picker, events, cx)
    }

    #[gpui::test]
    fn remote_picker_lists_home_and_descends_without_closing(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [
                Ok(remote_rows(["Projects", ".ssh", "alpha"])),
                Ok(remote_rows(["SpaceTerm"])),
            ],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider.clone(), cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.row_names()),
            vec!["alpha", "Projects"]
        );
        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
        });
        cx.run_until_parked();

        assert_eq!(
            picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned()),
            "~/alpha/"
        );
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[gpui::test]
    fn directories_publish_a_list_that_descends_and_opens_through_accessibility(
        cx: &mut TestAppContext,
    ) {
        use gpui::accesskit::Action;
        use spaceterm_ui::a11y_testing::{A11yTree, perform};

        let identity = RemoteDirectoryIdentity::new("/home/tester/alpha".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(remote_rows(["alpha", "Projects"])), Ok(Vec::new())],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
            ],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = directory_picker(provider, cx);
        let tree = A11yTree::read(cx);
        let dialogs = tree.with_role("Dialog");
        assert_eq!(dialogs.len(), 1);
        assert_eq!(dialogs[0]["aria"]["label"], "Pin to Directory");
        assert_eq!(dialogs[0]["aria"]["modal"], true);
        let options = |tree: &A11yTree| {
            tree.with_role("ListBoxOption")
                .iter()
                .map(|option| option["aria"]["label"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(options(&tree), ["Go Back", "alpha", "Projects"]);

        perform(cx, tree.node("alpha"), Action::Click);
        let tree = A11yTree::read(cx);
        assert_eq!(
            picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned()),
            "~/alpha/"
        );
        assert_eq!(options(&tree), ["Go Back"]);

        perform(cx, tree.node("Open"), Action::Click);
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, DirectoryPickerEvent::Confirmed(_)))
        );
    }

    #[gpui::test]
    fn missing_remote_path_should_note_that_open_creates_it(cx: &mut TestAppContext) {
        let provider = scripted_provider([Ok(Vec::new())], [Ok(ExactPathState::Missing)], [], []);
        let (picker, _, cx) = directory_picker(provider, cx);

        assert!(picker.read_with(cx, |picker, _| picker.can_confirm()));
        assert!(cx.debug_bounds(CONFIRM_ACTION).is_some());
        assert!(
            cx.debug_bounds("command-palette-query-note").is_some(),
            "the search line did not note that Open creates the directory"
        );
        assert!(cx.debug_bounds(ENCLOSING_ROW).is_none());
        assert!(cx.debug_bounds("command-palette-empty").is_some());
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.empty_state().title().to_owned()),
            "Directory doesn\u{2019}t exist"
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("modal-action-directory-picker-create")
                .is_some()
        );
    }

    #[gpui::test]
    fn the_confirm_key_should_pin_the_current_directory_while_a_child_is_selected(
        cx: &mut TestAppContext,
    ) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(remote_rows(["Projects"]))],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [Ok(identity.clone())],
        );
        let (picker, events, cx) = directory_picker(provider.clone(), cx);
        assert!(matches!(
            picker.read_with(cx, |picker, cx| picker
                .palette
                .read(cx)
                .selected_item_id()
                .cloned()),
            Some(DirectoryPickerItemId::Child { .. })
        ));

        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert_eq!(
            provider.state.lock().unwrap().validated_directories,
            vec![remote_directory(HOME_DISPLAY)]
        );
        assert!(events.borrow().iter().any(|event| matches!(
            event,
            DirectoryPickerEvent::Confirmed(PinnedDirectory::Remote { identity: pinned, .. })
                if pinned == &identity
        )));
    }

    #[gpui::test]
    fn return_in_an_empty_directory_should_open_it_rather_than_go_back(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = directory_picker(provider.clone(), cx);
        assert!(cx.debug_bounds(ENCLOSING_ROW).is_some());
        assert!(
            cx.debug_bounds("command-palette-results-note").is_some(),
            "an empty directory did not say it has no subdirectories"
        );
        assert_eq!(
            picker.read_with(cx, |picker, cx| picker
                .palette
                .read(cx)
                .selected_item_id()
                .cloned()),
            None
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            provider.state.lock().unwrap().validated_directories,
            vec![remote_directory(HOME_DISPLAY)]
        );
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, DirectoryPickerEvent::Confirmed(_)))
        );
    }

    #[gpui::test]
    fn an_unusable_path_should_disable_the_confirm_action_and_ignore_the_confirm_key(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(remote_rows(["Projects"]))],
            [Err(RemoteDirectoryProviderError::PermissionDenied)],
            [],
            [],
        );
        let (picker, events, cx) = directory_picker(provider.clone(), cx);
        assert!(cx.debug_bounds(CONFIRM_ACTION).is_some());
        assert!(!picker.read_with(cx, |picker, _| picker.can_confirm()));

        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();

        assert!(
            provider
                .state
                .lock()
                .unwrap()
                .validated_directories
                .is_empty()
        );
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, DirectoryPickerEvent::Confirmed(_)))
        );
    }

    #[gpui::test]
    fn create_confirmation_uses_alert_then_emits_validated_remote_selection(
        cx: &mut TestAppContext,
    ) {
        let identity =
            RemoteDirectoryIdentity::new("/home/tester/Projects/new".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new()), Ok(Vec::new())],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::Missing),
            ],
            [Ok(())],
            [Ok(identity.clone())],
        );
        let (picker, events, cx) = directory_picker(provider.clone(), cx);
        set_remote_input(&picker, "~/Projects/new", cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
        });
        cx.run_until_parked();
        let create = cx
            .debug_bounds("modal-action-directory-picker-create")
            .expect("creation Alert should expose its typed affirmative action");
        cx.simulate_click(create.center(), Modifiers::none());
        cx.run_until_parked();

        let expected = remote_directory("~/Projects/new");
        let records = provider.state.lock().unwrap();
        assert_eq!(records.created_directories, vec![expected.clone()]);
        assert_eq!(records.validated_directories, vec![expected.clone()]);
        drop(records);
        let pinned = events.borrow().iter().find_map(|event| match event {
            DirectoryPickerEvent::Confirmed(pinned) => Some(pinned.clone()),
            _ => None,
        });
        assert_eq!(
            pinned,
            Some(PinnedDirectory::Remote {
                directory: expected,
                identity,
            })
        );
    }

    #[gpui::test]
    fn the_enclosing_row_should_lead_the_children_and_open_the_enclosing_directory(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [
                Ok(remote_rows(["Projects"])),
                Ok(remote_rows(["SpaceTerm"])),
                Ok(remote_rows(["Projects"])),
                Ok(remote_rows(["tester"])),
            ],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider, cx);
        set_remote_input(&picker, "~/Projects/", cx);

        let enclosing = cx
            .debug_bounds(ENCLOSING_ROW)
            .expect("the enclosing row should be rendered");
        let child = cx
            .debug_bounds("directory-picker-row-SpaceTerm")
            .expect("the child row should be rendered");
        assert_eq!(
            (enclosing.bottom(), enclosing.size.height),
            (child.top(), child.size.height),
            "the enclosing row should be an ordinary row directly above the children"
        );
        assert!(matches!(
            picker.read_with(cx, |picker, cx| picker
                .palette
                .read(cx)
                .selected_item_id()
                .cloned()),
            Some(DirectoryPickerItemId::Child { .. })
        ));

        let query = |picker: &gpui::Entity<DirectoryPicker>, cx: &mut VisualTestContext| {
            picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned())
        };
        cx.simulate_click(enclosing.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(query(&picker, cx), HOME_DISPLAY);

        let enclosing = cx.debug_bounds(ENCLOSING_ROW).unwrap();
        cx.simulate_click(enclosing.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(query(&picker, cx), "/home/");
        assert!(picker.read_with(cx, |picker, _| picker.is_open()));
    }

    #[test]
    fn the_enclosing_directory_should_contain_the_listed_directory() {
        let home = PickerPath::new("/home/tester".to_owned()).unwrap();
        let enclosing =
            |input: &str| enclosing_directory_query(&parse_picker_path(input).unwrap(), &home);

        assert_eq!(enclosing("~/Projects/"), Some(HOME_DISPLAY.to_owned()));
        assert_eq!(enclosing("~/Projects/Space"), Some(HOME_DISPLAY.to_owned()));
        assert_eq!(
            enclosing("~/Projects//SpaceTerm/"),
            Some("~/Projects/".to_owned())
        );
        assert_eq!(enclosing("~/"), Some("/home/".to_owned()));
        assert_eq!(enclosing("~/Proj"), Some("/home/".to_owned()));
        assert_eq!(enclosing("/usr/local/"), Some("/usr/".to_owned()));
        assert_eq!(enclosing("/usr/"), Some("/".to_owned()));
        assert_eq!(enclosing("/usr"), None);
        assert_eq!(enclosing("/"), None);
    }

    #[test]
    fn a_root_home_should_have_no_enclosing_directory() {
        let home = PickerPath::new("/".to_owned()).unwrap();

        assert_eq!(
            enclosing_directory_query(&parse_picker_path("~/").unwrap(), &home),
            None
        );
    }

    #[gpui::test]
    fn unsupported_login_shell_should_have_an_actionable_account_status(cx: &mut TestAppContext) {
        assert_eq!(
            status_for_source_error(DirectorySourceError::UnsupportedLoginShell),
            DirectoryPickerStatus::UnsupportedLoginShell
        );
        let provider = scripted_provider([], [], [], []);
        provider.state.lock().unwrap().accounts = [Task::ready(Err(
            RemoteDirectoryProviderError::UnsupportedLoginShell,
        ))]
        .into();
        let (picker, _, cx) = directory_picker(provider, cx);
        let empty = picker.read_with(cx, |picker, _| picker.empty_state());
        assert_eq!(empty.title(), "Unsupported login shell");
        assert_eq!(
            empty.description_text(),
            Some(UNSUPPORTED_LOGIN_SHELL_MESSAGE)
        );
        assert!(cx.debug_bounds("command-palette-empty").is_some());
        assert_eq!(
            UNSUPPORTED_LOGIN_SHELL_MESSAGE,
            "The remote login shell does not support login mode. Choose another account or shell."
        );
    }

    #[gpui::test]
    fn stale_remote_refresh_cannot_replace_the_current_readable_state(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider, cx);
        let (lifecycle_generation, operation_generation, parsed) =
            picker.read_with(cx, |picker, _| {
                (
                    picker.lifecycle_generation,
                    picker.operation_generation,
                    picker.parsed.clone().unwrap(),
                )
            });

        picker.update(cx, |picker, cx| {
            picker.finish_refresh(
                RefreshCompletion {
                    lifecycle_generation,
                    operation_generation: operation_generation.wrapping_sub(1),
                    parsed,
                    listing: Some(Err(DirectorySourceError::PermissionDenied)),
                    probe: Err(DirectorySourceError::ConnectionLost),
                },
                cx,
            );
        });

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.status),
            DirectoryPickerStatus::Readable
        );
    }

    #[gpui::test]
    fn remote_listing_errors_are_specific_and_never_keep_unread_rows(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Err(RemoteDirectoryProviderError::PermissionDenied)],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| {
                let empty = picker.empty_state();
                (
                    picker.status,
                    picker.can_confirm(),
                    picker.listing_error,
                    empty.title().to_owned(),
                    empty.description_text().map(str::to_owned),
                    picker.row_names(),
                )
            }),
            (
                DirectoryPickerStatus::Readable,
                true,
                Some(DirectorySourceError::PermissionDenied),
                "Permission denied".to_owned(),
                Some("Your account can\u{2019}t list this directory.".to_owned()),
                Vec::<String>::new(),
            )
        );
    }

    #[gpui::test]
    fn changing_directory_clears_rows_and_stale_activation_is_inert(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(remote_rows(["Projects"])), Ok(remote_rows(["Current"]))],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider, cx);
        let Some(DirectoryPickerItemId::Child {
            row,
            directory,
            operation_generation,
        }) = picker.read_with(cx, |picker, cx| {
            picker.palette.read(cx).selected_item_id().cloned()
        })
        else {
            panic!("the first child directory should be selected");
        };

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.refresh_for_input("~/Elsewhere/".to_owned(), window, cx);
                assert!(picker.row_names().is_empty());
            });
        });
        cx.run_until_parked();
        let current_query =
            picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned());
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.descend_to(&row, &directory, operation_generation, window, cx);
            });
        });

        assert_eq!(
            picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned()),
            current_query
        );
    }

    #[gpui::test]
    fn superseded_remote_refresh_should_cancel_listing_and_probe_tasks(cx: &mut TestAppContext) {
        let dropped_operations = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(CancellationTrackingRemoteDirectoryProvider {
            executor: cx.executor(),
            dropped_operations: Arc::clone(&dropped_operations),
        });
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected: Arc<dyn RemoteDirectoryProvider + Send + Sync> = provider;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                DirectoryPicker::new(
                    Rc::new(RemoteDirectorySource::new(injected, "orb")),
                    "Pin to Directory",
                    window,
                    cx,
                )
            });
            DirectoryPickerHarness { picker }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            picker.update(cx, |picker, cx| assert!(picker.open(window, cx)));
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.refresh_for_input("~/second/".to_owned(), window, cx);
            });
        });
        cx.run_until_parked();

        assert_eq!(dropped_operations.load(AtomicOrdering::SeqCst), 2);
        picker.update(cx, |picker, cx| {
            picker.finish_close(CommandPaletteCloseReason::Programmatic, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn rapid_remote_path_input_should_settle_within_the_connection_session_limit(
        cx: &mut TestAppContext,
    ) {
        let server = FakeRemoteUtilityServer::new(cx.executor(), 10);
        server.open_terminal_session_channels(8);
        let (picker, _, cx) = directory_picker(Arc::new(server.provider()), cx);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();

        let path = "~/Projects/SpaceTerm/crates/";
        for end in 1..=path.len() {
            cx.update(|_, cx| {
                picker.update(cx, |picker, cx| {
                    picker
                        .palette
                        .update(cx, |palette, cx| palette.set_query(&path[..end], cx));
                });
            });
            cx.executor().advance_clock(Duration::from_millis(2));
            cx.run_until_parked();
            picker.read_with(cx, |picker, _| {
                assert_ne!(picker.status, DirectoryPickerStatus::ConnectionLost);
                assert_ne!(
                    picker.listing_error,
                    Some(DirectorySourceError::ConnectionLost)
                );
            });
        }
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();

        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.status, DirectoryPickerStatus::Readable);
            assert!(picker.can_confirm());
            assert_eq!(picker.row_names(), ["Documents", "Projects", "srv"]);
        });
        assert_eq!(server.refused_sessions(), 0);
        assert!(server.peak_utility_sessions() <= 2);
    }

    #[gpui::test]
    fn a_server_that_keeps_refusing_sessions_should_not_report_a_lost_connection(
        cx: &mut TestAppContext,
    ) {
        let server = FakeRemoteUtilityServer::new(cx.executor(), 10);
        let (picker, _, cx) = directory_picker(Arc::new(server.provider()), cx);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        server.open_terminal_session_channels(10);

        set_remote_input(&picker, "~/Projects/", cx);
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();

        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.status, DirectoryPickerStatus::SessionUnavailable);
            assert_eq!(
                picker.empty_notice().title,
                SESSION_UNAVAILABLE_NOTICE.title
            );
            assert!(!picker.can_confirm());
        });
        assert!(server.refused_sessions() > 2);
    }

    #[gpui::test]
    fn a_pending_listing_should_present_loading_instead_of_an_empty_directory(
        cx: &mut TestAppContext,
    ) {
        let provider = Arc::new(CancellationTrackingRemoteDirectoryProvider {
            executor: cx.executor(),
            dropped_operations: Arc::new(AtomicUsize::new(0)),
        });
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected: Arc<dyn RemoteDirectoryProvider + Send + Sync> = provider;
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| {
                DirectoryPicker::new(
                    Rc::new(RemoteDirectorySource::new(injected, "orb")),
                    "Pin to Directory",
                    window,
                    cx,
                )
            });
            DirectoryPickerHarness { picker }
        });
        let picker = harness.read_with(cx, |harness, _| harness.picker.clone());
        cx.update(|window, cx| {
            window.activate_window();
            picker.update(cx, |picker, cx| assert!(picker.open(window, cx)));
        });
        cx.run_until_parked();
        // Past the palette's loading grace period.
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();

        assert!(cx.debug_bounds("command-palette-loading").is_some());
        assert!(cx.debug_bounds("command-palette-empty").is_none());
        picker.update(cx, |picker, cx| {
            picker.finish_close(CommandPaletteCloseReason::Programmatic, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn oversized_remote_listing_is_bounded_and_exposes_exact_path_guidance(
        cx: &mut TestAppContext,
    ) {
        let rows = (0..MAXIMUM_DIRECTORY_ROWS + 7)
            .map(|index| DirectoryRow::new(format!("directory-{index:04}")).unwrap())
            .collect::<Vec<_>>();
        let provider = scripted_provider(
            [Ok(rows)],
            [
                Ok(ExactPathState::ReadableDirectory),
                Ok(ExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = directory_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.row_names().len()),
            MAXIMUM_DIRECTORY_ROWS
        );
        assert!(picker.read_with(cx, |picker, _| picker.listing_truncated));
        assert!(picker.read_with(cx, |picker, _| picker.can_confirm()));
        assert_eq!(
            picker.read_with(cx, |picker, cx| picker
                .palette
                .read(cx)
                .results_note()
                .cloned()),
            Some(SharedString::from(
                "Showing the first 1024 directories. Type a path to open others."
            ))
        );
    }

    #[gpui::test]
    fn cancelling_a_modal_pending_open_unblocks_input_and_emits_dismissed_once(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = directory_picker(provider, cx);
        assert!(
            cx.update(|window, cx| { picker.update(cx, |picker, cx| picker.dismiss(window, cx)) })
        );
        cx.run_until_parked();
        events.borrow_mut().clear();
        let modal = cx.update(|window, cx| {
            picker.update(cx, |_, cx| {
                Alert::new(
                    ModalId::new("remote-picker-pending-open-test"),
                    "Pending remote picker",
                    "Continue?",
                    "The picker must wait for this alert.",
                    vec![ModalAction::new(
                        true,
                        "OK",
                        ModalActionRole::Affirmative,
                        "remote-picker-pending-ok",
                    )],
                )
                .present(window, cx, |_, _| {})
                .unwrap()
            })
        });
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| assert!(picker.open(window, cx)));
        });
        assert!(picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));

        assert!(
            cx.update(|window, cx| { picker.update(cx, |picker, cx| picker.dismiss(window, cx)) })
        );
        cx.run_until_parked();
        cx.update(|window, cx| modal.dismiss(window, cx).unwrap());
        cx.run_until_parked();

        assert!(!picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| **event == DirectoryPickerEvent::Dismissed)
                .count(),
            1
        );
    }

    #[gpui::test]
    fn escape_dismisses_directory_selection(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = directory_picker(provider, cx);
        assert!(picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));

        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        cx.run_until_parked();

        assert!(!picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert!(events.borrow().contains(&DirectoryPickerEvent::Dismissed));
    }

    #[gpui::test]
    fn choosing_system_selection_should_close_and_hand_off_instead_of_dismissing(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) =
            directory_picker_offering(provider, Some("Choose Directory…"), cx);

        let menu = cx
            .debug_bounds("directory-picker-confirm-menu")
            .expect("the confirm action did not offer System Directory Selection");
        cx.simulate_click(menu.center(), Modifiers::none());
        cx.run_until_parked();
        let item = cx
            .debug_bounds("command-palette-primary-menu-directory-picker-system-selection")
            .expect("the menu did not open");
        cx.simulate_click(item.center(), Modifiers::none());
        cx.run_until_parked();

        assert!(!picker.read_with(cx, |picker, _| picker.is_open()));
        let events = events.borrow();
        assert!(events.contains(&DirectoryPickerEvent::SystemSelectionRequested));
        assert!(!events.contains(&DirectoryPickerEvent::Dismissed));
    }

    #[gpui::test]
    fn a_picker_without_system_selection_should_offer_no_menu(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (_, _, cx) = directory_picker(provider, cx);

        assert!(cx.debug_bounds(CONFIRM_ACTION).is_some());
        assert!(cx.debug_bounds("directory-picker-confirm-menu").is_none());
    }

    #[gpui::test]
    fn the_open_action_should_expose_a_shortcut_element(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (_, _, cx) = directory_picker(provider, cx);

        let shortcut = cx.update(|_, cx| {
            crate::desktop_profile::DesktopPresentation::get(cx)
                .shortcut(&spaceterm_ui::CommandPaletteConfirm)
        });
        assert!(
            shortcut.is_some(),
            "the Confirm key has no displayed Shortcut"
        );
        assert!(
            cx.debug_bounds("directory-picker-confirm-shortcut")
                .is_some(),
            "the Open action did not show its Shortcut"
        );
    }

    #[gpui::test]
    fn programmatic_dismissal_emits_dismissed(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = directory_picker(provider, cx);

        assert!(
            cx.update(|window, cx| { picker.update(cx, |picker, cx| picker.dismiss(window, cx)) })
        );
        cx.run_until_parked();

        assert!(events.borrow().contains(&DirectoryPickerEvent::Dismissed));
    }

    #[gpui::test]
    fn validation_makes_the_path_read_only_until_the_parent_advances(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = directory_picker(provider, cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
            for key in ["cmd-a", "x"] {
                window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx);
            }
        });
        let query = picker.read_with(cx, |picker, cx| picker.palette.read(cx).query().to_owned());
        let dismissed =
            cx.update(|window, cx| picker.update(cx, |picker, cx| picker.dismiss(window, cx)));
        cx.run_until_parked();

        assert_eq!(query, HOME_DISPLAY);
        assert!(!dismissed);
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| { matches!(event, DirectoryPickerEvent::Confirmed(_)) })
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.busy),
            Some(DirectoryPickerBusy::AwaitingActivation)
        );
    }

    #[gpui::test]
    fn forced_cancel_should_drop_busy_validation_and_reject_late_selection(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let dropped = Arc::new(AtomicUsize::new(0));
        let tracking = Arc::clone(&dropped);
        let validation = cx.executor().spawn(async move {
            let _guard = PendingOperationDrop(tracking);
            pending().await
        });
        provider
            .state
            .lock()
            .unwrap()
            .validations
            .push_back(validation);
        let (picker, events, cx) = directory_picker(provider, cx);
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.confirm_current(window, cx)));
        cx.run_until_parked();
        assert!(picker.read_with(cx, |picker, _| picker.busy.is_some()));
        let (lifecycle_generation, operation_generation, directory) =
            picker.read_with(cx, |picker, _| {
                (
                    picker.lifecycle_generation,
                    picker.operation_generation,
                    picker.parsed.as_ref().unwrap().exact_directory().clone(),
                )
            });
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.cancel(window, cx)));
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.finish_validation(
                    ValidationCompletion {
                        lifecycle_generation,
                        operation_generation,
                        directory,
                        result: Ok(PinnedDirectory::Remote {
                            directory: crate::domain::RemoteDirectory::new("~/".to_owned())
                                .unwrap(),
                            identity: RemoteDirectoryIdentity::new("/home/tester".to_owned())
                                .unwrap(),
                        }),
                    },
                    window,
                    cx,
                );
            })
        });
        cx.run_until_parked();
        assert_eq!(dropped.load(AtomicOrdering::SeqCst), 1);
        assert!(!picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, DirectoryPickerEvent::Confirmed(_)))
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| **event == DirectoryPickerEvent::Dismissed)
                .count(),
            1
        );
    }

    #[gpui::test]
    fn escape_during_validation_dismisses_without_accepting_late_success(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(ExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = directory_picker(provider, cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        cx.run_until_parked();
        assert!(events.borrow().contains(&DirectoryPickerEvent::Dismissed));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, DirectoryPickerEvent::Confirmed(_)))
        );
    }

    fn set_remote_input(
        picker: &gpui::Entity<DirectoryPicker>,
        value: &str,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|_, cx| {
            picker.update(cx, |picker, cx| {
                picker
                    .palette
                    .update(cx, |palette, cx| palette.set_query(value, cx));
            });
        });
        cx.run_until_parked();
    }

    fn remote_directory(value: &str) -> RemoteDirectory {
        RemoteDirectory::new(value.to_owned()).unwrap()
    }

    #[test]
    fn remote_path_parser_should_accept_root_without_rewriting_it() {
        let parsed = parse_picker_path("/").unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
                parsed.trailing_separator(),
            ),
            ("/", "/", "/", "", true)
        );
    }

    #[test]
    fn remote_path_parser_should_preserve_home_relative_spelling() {
        let parsed = parse_picker_path("~/Projects/SpaceTerm").unwrap();

        assert_eq!(
            (
                parsed.display(),
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
            ),
            (
                "~/Projects/SpaceTerm",
                "~/Projects/SpaceTerm",
                "~/Projects",
                "SpaceTerm",
            )
        );
    }

    #[test]
    fn remote_path_parser_should_preserve_repeated_separators() {
        let home_relative = parse_picker_path("~//Projects//SpaceTerm").unwrap();
        let absolute = parse_picker_path("//srv///projects//SpaceTerm").unwrap();

        assert_eq!(
            (
                home_relative.exact_directory().as_str(),
                home_relative.enumeration_directory().as_str(),
                absolute.exact_directory().as_str(),
                absolute.enumeration_directory().as_str(),
            ),
            (
                "~//Projects//SpaceTerm",
                "~//Projects/",
                "//srv///projects//SpaceTerm",
                "//srv///projects/",
            )
        );
    }

    #[test]
    fn trailing_separator_should_enumerate_the_exact_remote_directory() {
        let parsed = parse_picker_path("~/Projects//SpaceTerm/").unwrap();

        assert_eq!(
            (
                parsed.exact_directory().as_str(),
                parsed.enumeration_directory().as_str(),
                parsed.leaf_filter(),
            ),
            ("~/Projects//SpaceTerm/", "~/Projects//SpaceTerm/", "",)
        );
    }

    #[test]
    fn remote_path_parser_should_reject_relative_and_unsupported_tilde_forms() {
        assert_eq!(
            parse_picker_path("Projects"),
            Err(PickerPathError::Relative)
        );
        assert_eq!(parse_picker_path("~"), Err(PickerPathError::BareTilde));
        assert_eq!(
            parse_picker_path("~other/Projects"),
            Err(PickerPathError::UnsupportedTilde)
        );
    }

    #[test]
    fn dot_segments_should_be_rejected_before_the_leaf_and_never_opened() {
        for input in ["~/Projects/../", "~/../SpaceTerm", "/usr/./local", "~/./"] {
            assert_eq!(
                parse_picker_path(input),
                Err(PickerPathError::DotSegment),
                "{input} was accepted"
            );
        }
        for leaf in ["~/Projects/.", "~/Projects/.."] {
            let parsed = parse_picker_path(leaf).unwrap();
            assert!(!parsed.names_openable_directory(), "{leaf} could be opened");
        }
        assert!(
            parse_picker_path("~/Projects/.config")
                .unwrap()
                .names_openable_directory()
        );
    }

    #[test]
    fn hidden_directories_should_be_revealed_only_from_a_dot_leaf() {
        let ordinary = parse_picker_path("~/Projects/").unwrap();
        let dotted = parse_picker_path("~/Projects/.").unwrap();
        let entries = remote_rows([".config", "SpaceTerm", ".ssh"]);

        assert_eq!(
            row_names(match_directory_rows(&ordinary, &entries)),
            vec!["SpaceTerm"]
        );
        assert_eq!(
            row_names(match_directory_rows(&dotted, &entries)),
            vec![".config", ".ssh"]
        );
    }

    #[test]
    fn rows_should_fuzzy_match_and_preserve_deterministic_name_order_for_ties() {
        let parsed = parse_picker_path("~/Projects/sp").unwrap();
        let entries = remote_rows(["spaceTerm", "Spatial", "SpaceTerm", "tools"]);

        assert_eq!(
            row_names(match_directory_rows(&parsed, &entries)),
            vec!["SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn rows_should_rank_contiguous_matches_and_report_indices() {
        let parsed = parse_picker_path("~/Projects/ro").unwrap();
        let entries = remote_rows(["random", "projects"]);

        let matches = match_directory_rows(&parsed, &entries);

        assert_eq!(matches[0].row.name(), "projects");
        assert_eq!(matches[0].matched_indices, vec![1, 2]);
    }

    #[test]
    fn an_empty_leaf_should_preserve_deterministic_name_order() {
        let parsed = parse_picker_path("~/Projects/").unwrap();
        let entries = remote_rows(["Zulu", "Alpha"]);

        assert_eq!(
            row_names(match_directory_rows(&parsed, &entries)),
            vec!["Alpha", "Zulu"]
        );
    }

    #[test]
    fn directory_rows_should_reject_non_one_level_names() {
        assert!(DirectoryRow::new("nested/project".to_owned()).is_err());
        assert!(DirectoryRow::new("project\nname".to_owned()).is_err());
        assert!(DirectoryRow::new(String::new()).is_err());
    }

    #[test]
    fn activating_a_row_should_rewrite_the_query_to_descend() {
        let parsed = parse_picker_path("~//Projects//sp").unwrap();
        let row = DirectoryRow::new("SpaceTerm".to_owned()).unwrap();

        assert_eq!(
            descend_query(&parsed, &row).unwrap().as_str(),
            "~//Projects//SpaceTerm/"
        );
    }

    fn remote_rows<const N: usize>(names: [&str; N]) -> Vec<DirectoryRow> {
        names
            .into_iter()
            .map(|name| DirectoryRow::new(name.to_owned()).unwrap())
            .collect()
    }

    fn row_names(rows: Vec<DirectoryRowMatch>) -> Vec<String> {
        rows.into_iter()
            .map(|matched| matched.row.name().to_owned())
            .collect()
    }
}
