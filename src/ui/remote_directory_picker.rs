use std::{cmp::Ordering, fmt, sync::Arc};

use gpui::prelude::*;
use gpui::{Action, App};
use gpui::{Context, Entity, EventEmitter, Render, Task, Window, div};
use spaceterm_ui::{
    Alert, AlertOutcome, CommandPalette, CommandPaletteActivationPolicy, CommandPaletteCloseReason,
    CommandPaletteConfirm, CommandPaletteEvent, CommandPaletteHint, CommandPaletteItem,
    CommandPaletteLifecycleEvent, CommandPaletteMatching, CommandPaletteReplacementFocus, Icon,
    IconName, ModalAction, ModalActionRole, ModalId, ModalPresentationHandle,
};

use super::{
    ActivateTab1, ActivateTab2, ActivateTab3, ActivateTab4, ActivateTab5, ActivateTab6,
    ActivateTab7, ActivateTab8, ActivateTab9, ActivateWorkspace1, ActivateWorkspace2,
    ActivateWorkspace3, ActivateWorkspace4, ActivateWorkspace5, ActivateWorkspace6,
    ActivateWorkspace7, ActivateWorkspace8, ActivateWorkspace9, ClosePane, CloseTab,
    CloseTerminalFind, CloseWorkspace, CopySelection, CreateTab, FindNext, FindPrevious,
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, NewWorkspace, OpenTerminalFind,
    SplitDown, SplitRight, SwitchWorkspace, TogglePaneZoom, ToggleSidebar, ToggleSidebarFocus,
};

use crate::domain::{RemoteDirectory, RemoteDirectoryIdentity, RemoteWorkspaceValueError};
use crate::ssh::remote_account::RemoteWorkspaceAccount;

const HOME_DISPLAY: &str = "~/";
const CREATE_ALERT_ID: &str = "remote-workspace-create-directory";
const UNSUPPORTED_LOGIN_SHELL_MESSAGE: &str =
    "The remote login shell does not support login mode. Choose another account or shell.";
pub(super) const MAXIMUM_REMOTE_DIRECTORY_ROWS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDirectoryProviderError {
    ConnectionLost,
    Missing,
    NotDirectory,
    PermissionDenied,
    UnsupportedLoginShell,
    InvalidResponse,
    Other,
}

/// The connected-SSH boundary used by the picker. Every path crossing it is a remote string type.
pub(crate) trait RemoteDirectoryProvider: Send + Sync {
    fn discover_account(
        &self,
    ) -> Task<Result<RemoteWorkspaceAccount, RemoteDirectoryProviderError>>;

    fn list_directories(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>>;

    fn probe_exact_path(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>>;

    fn create_directory_recursively(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<(), RemoteDirectoryProviderError>>;

    fn validate_physical_identity(
        &self,
        directory: RemoteDirectory,
    ) -> Task<Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteDirectoryFormatError {
    Relative,
    BareTilde,
    UnsupportedTilde,
    InvalidControlCharacter,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct ParsedRemoteDirectory {
    display: String,
    exact_directory: RemoteDirectory,
    enumeration_directory: RemoteDirectory,
    descend_prefix: String,
    leaf_filter: String,
    trailing_separator: bool,
}

impl fmt::Debug for ParsedRemoteDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ParsedRemoteDirectory(<redacted>)")
    }
}

impl ParsedRemoteDirectory {
    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) const fn exact_directory(&self) -> &RemoteDirectory {
        &self.exact_directory
    }

    pub(super) const fn enumeration_directory(&self) -> &RemoteDirectory {
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
}

pub(super) fn parse_remote_directory(
    input: &str,
) -> Result<ParsedRemoteDirectory, RemoteDirectoryFormatError> {
    if input == "~" {
        return Err(RemoteDirectoryFormatError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with("~/") {
        return Err(RemoteDirectoryFormatError::UnsupportedTilde);
    }
    if !input.starts_with('/') && !input.starts_with("~/") {
        return Err(RemoteDirectoryFormatError::Relative);
    }

    let exact_directory = RemoteDirectory::new(input.to_owned())
        .map_err(|_| RemoteDirectoryFormatError::InvalidControlCharacter)?;
    let trailing_separator = input.ends_with('/');
    let (enumeration_spelling, descend_prefix, leaf_filter) = if trailing_separator {
        (input, input.to_owned(), String::new())
    } else {
        let separator = input
            .rfind('/')
            .ok_or(RemoteDirectoryFormatError::Relative)?;
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
    let enumeration_directory = RemoteDirectory::new(enumeration_spelling.to_owned())
        .map_err(|_| RemoteDirectoryFormatError::InvalidControlCharacter)?;

    Ok(ParsedRemoteDirectory {
        display: input.to_owned(),
        exact_directory,
        enumeration_directory,
        descend_prefix,
        leaf_filter,
        trailing_separator,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDirectoryRowError {
    InvalidName,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RemoteDirectoryRow {
    name: String,
}

/// A defensively bounded one-level directory result from a remote provider.
impl fmt::Debug for RemoteDirectoryRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectoryRow(<redacted>)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RemoteDirectoryListing {
    rows: Vec<RemoteDirectoryRow>,
    truncated: bool,
}

impl fmt::Debug for RemoteDirectoryListing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectoryListing(<redacted>)")
    }
}

impl RemoteDirectoryListing {
    #[cfg(test)]
    pub(crate) fn new(rows: Vec<RemoteDirectoryRow>) -> Self {
        Self::from_remote(rows, false)
    }

    pub(crate) fn from_remote(mut rows: Vec<RemoteDirectoryRow>, remotely_truncated: bool) -> Self {
        let truncated = remotely_truncated || rows.len() > MAXIMUM_REMOTE_DIRECTORY_ROWS;
        rows.truncate(MAXIMUM_REMOTE_DIRECTORY_ROWS);
        Self { rows, truncated }
    }

    pub(crate) fn rows(&self) -> &[RemoteDirectoryRow] {
        &self.rows
    }

    pub(crate) const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

impl RemoteDirectoryRow {
    pub(crate) fn new(name: String) -> Result<Self, RemoteDirectoryRowError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.chars().any(char::is_control)
        {
            return Err(RemoteDirectoryRowError::InvalidName);
        }
        Ok(Self { name })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

pub(super) fn filter_remote_workspace_rows(
    parsed: &ParsedRemoteDirectory,
    entries: &[RemoteDirectoryRow],
) -> Vec<RemoteDirectoryRow> {
    let folded_filter = parsed.leaf_filter.to_lowercase();
    let reveal_hidden = parsed.reveals_hidden_directories();
    let mut rows = entries
        .iter()
        .filter(|entry| reveal_hidden || !entry.name.starts_with('.'))
        .filter(|entry| entry.name.to_lowercase().starts_with(&folded_filter))
        .cloned()
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let folded = left.name.to_lowercase().cmp(&right.name.to_lowercase());
        if folded == Ordering::Equal {
            left.name.cmp(&right.name)
        } else {
            folded
        }
    });
    rows
}

pub(super) fn descend_remote_workspace_query(
    parsed: &ParsedRemoteDirectory,
    row: &RemoteDirectoryRow,
) -> Result<RemoteDirectory, RemoteWorkspaceValueError> {
    RemoteDirectory::new(format!("{}{}/", parsed.descend_prefix, row.name()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDirectoryExactPathState {
    ReadableDirectory,
    Missing,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct RemoteDirectorySelection {
    directory: RemoteDirectory,
    physical_directory: RemoteDirectoryIdentity,
}

impl fmt::Debug for RemoteDirectorySelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectorySelection(<redacted>)")
    }
}

impl RemoteDirectorySelection {
    #[cfg(test)]
    pub(super) fn new(
        directory: RemoteDirectory,
        physical_directory: RemoteDirectoryIdentity,
    ) -> Self {
        Self {
            directory,
            physical_directory,
        }
    }

    pub(super) const fn directory(&self) -> &RemoteDirectory {
        &self.directory
    }

    pub(super) const fn physical_directory(&self) -> &RemoteDirectoryIdentity {
        &self.physical_directory
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RemoteDirectoryPickerEvent {
    StateChanged,
    Dismissed,
    Confirmed(RemoteDirectorySelection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDirectoryPickerStatus {
    DiscoveringAccount,
    Loading,
    Readable,
    Missing,
    NotDirectory,
    PermissionDenied,
    ConnectionLost,
    UnsupportedLoginShell,
    Other,
    Invalid(RemoteDirectoryFormatError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDirectoryPickerBusy {
    CreationAlert,
    Creating,
    Validating,
    AwaitingActivation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteDirectoryPickerItemId {
    row: RemoteDirectoryRow,
    directory: RemoteDirectory,
    operation_generation: u64,
}

#[derive(Clone)]
struct LoadedRemoteDirectorySnapshot {
    directory: RemoteDirectory,
    listing: RemoteDirectoryListing,
}

struct RefreshCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    parsed: ParsedRemoteDirectory,
    listing: Option<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>>,
    probe: Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDirectoryValidationKind {
    Existing,
    Creation,
}

struct ValidationCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    directory: RemoteDirectory,
    result: Result<RemoteDirectoryIdentity, RemoteDirectoryProviderError>,
}

/// One connected-destination directory chooser built on the reusable Command Palette.
pub(super) struct RemoteDirectoryPicker {
    provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
    palette: Entity<CommandPalette<RemoteDirectoryPickerItemId>>,
    opening: bool,
    open: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    account: Option<RemoteWorkspaceAccount>,
    parsed: Option<ParsedRemoteDirectory>,
    snapshot: Option<LoadedRemoteDirectorySnapshot>,
    rows: Vec<RemoteDirectoryRow>,
    listing_error: Option<RemoteDirectoryProviderError>,
    listing_truncated: bool,
    status: RemoteDirectoryPickerStatus,
    busy: Option<RemoteDirectoryPickerBusy>,
    creation_alert: Option<ModalPresentationHandle>,
    account_task: Option<Task<()>>,
    refresh_task: Option<Task<()>>,
    validation_task: Option<Task<()>>,
}

impl EventEmitter<RemoteDirectoryPickerEvent> for RemoteDirectoryPicker {}

impl RemoteDirectoryPicker {
    pub(super) fn new(
        provider: Arc<dyn RemoteDirectoryProvider + Send + Sync>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Pin to Directory", Vec::new(), window, cx);
            palette.set_hints(vec![CommandPaletteHint::new("Enter directory", "↵")], cx);
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |picker, _, event: &CommandPaletteEvent<RemoteDirectoryPickerItemId>, window, cx| {
                picker.reduce_palette_event(event, window, cx);
            },
        )
        .detach();
        Self {
            provider,
            palette,
            opening: false,
            open: false,
            lifecycle_generation: 0,
            operation_generation: 0,
            account: None,
            parsed: None,
            snapshot: None,
            rows: Vec::new(),
            listing_error: None,
            listing_truncated: false,
            status: RemoteDirectoryPickerStatus::DiscoveringAccount,
            busy: None,
            creation_alert: None,
            account_task: None,
            refresh_task: None,
            validation_task: None,
        }
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
        self.account = None;
        self.parsed = None;
        self.snapshot = None;
        self.rows.clear();
        self.listing_error = None;
        self.listing_truncated = false;
        self.status = RemoteDirectoryPickerStatus::DiscoveringAccount;
        self.busy = None;
        self.creation_alert = None;
        self.account_task.take();
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

    #[cfg(test)]
    pub(super) fn path_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.palette.read(cx).editor_is_focused(window, cx)
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
        if !self.open || self.busy != Some(RemoteDirectoryPickerBusy::AwaitingActivation) {
            return false;
        }
        self.busy = None;
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx)
        })
    }

    pub(super) fn activation_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(RemoteDirectoryPickerBusy::AwaitingActivation) {
            self.busy = None;
            self.status = RemoteDirectoryPickerStatus::Other;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<RemoteDirectoryPickerItemId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Opened) => {
                self.opening = false;
                self.open = true;
                self.palette
                    .update(cx, |palette, cx| palette.set_query(HOME_DISPLAY, cx));
                self.start_account_discovery(window, cx);
                self.publish(cx);
            }
            CommandPaletteEvent::Lifecycle(CommandPaletteLifecycleEvent::Closed(reason)) => {
                self.finish_close(*reason, cx);
            }
            CommandPaletteEvent::QueryChanged(query) => {
                self.refresh_for_input(query.text().to_owned(), window, cx);
            }
            CommandPaletteEvent::Activated(activation) => {
                self.descend_to(activation.item_id().clone(), window, cx);
            }
            CommandPaletteEvent::Confirmed => self.confirm_current(window, cx),
            CommandPaletteEvent::HeaderAction(_) | CommandPaletteEvent::MenuAction(_) => {}
        }
    }

    fn finish_close(&mut self, reason: CommandPaletteCloseReason, cx: &mut Context<Self>) {
        if !self.open && !self.opening {
            return;
        }
        self.opening = false;
        self.open = false;
        self.account = None;
        self.parsed = None;
        self.snapshot = None;
        self.rows.clear();
        self.listing_error = None;
        self.listing_truncated = false;
        self.busy = None;
        self.creation_alert = None;
        self.account_task.take();
        self.refresh_task.take();
        self.validation_task.take();
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        match reason {
            CommandPaletteCloseReason::Completed => {}
            _ => cx.emit(RemoteDirectoryPickerEvent::Dismissed),
        }
        cx.emit(RemoteDirectoryPickerEvent::StateChanged);
        cx.notify();
    }

    fn start_account_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let task = self.provider.discover_account();
        self.account_task.take();
        self.account_task = Some(cx.spawn_in(window, async move |picker, cx| {
            let result = task.await;
            let _ = picker.update_in(cx, |picker, window, cx| {
                if !picker.open
                    || picker.lifecycle_generation != lifecycle_generation
                    || picker.operation_generation != operation_generation
                {
                    return;
                }
                match result {
                    Ok(account) => {
                        picker.account = Some(account);
                        let query = picker.palette.read(cx).query().to_owned();
                        picker.refresh_for_input(query, window, cx);
                    }
                    Err(error) => {
                        picker.status = status_for_provider_error(error);
                        picker.clear_rows(cx);
                        picker.publish(cx);
                    }
                }
            });
        }));
    }

    fn refresh_for_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() || self.account.is_none() {
            return;
        }
        let parsed = match parse_remote_directory(&value) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.refresh_task.take();
                self.parsed = None;
                self.status = RemoteDirectoryPickerStatus::Invalid(error);
                self.listing_error = None;
                self.listing_truncated = false;
                self.operation_generation = self.operation_generation.wrapping_add(1);
                self.clear_rows(cx);
                self.publish(cx);
                return;
            }
        };
        let listing_needed = !self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.directory == *parsed.enumeration_directory());
        self.parsed = Some(parsed.clone());
        self.status = RemoteDirectoryPickerStatus::Loading;
        self.operation_generation = self.operation_generation.wrapping_add(1);
        if listing_needed {
            self.listing_error = None;
            self.listing_truncated = false;
            self.clear_rows(cx);
        } else {
            self.rebuild_rows(cx);
        }
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let listing = listing_needed.then(|| {
            self.provider
                .list_directories(parsed.enumeration_directory().clone())
        });
        let probe = self
            .provider
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
                self.snapshot = Some(LoadedRemoteDirectorySnapshot {
                    directory: completion.parsed.enumeration_directory().clone(),
                    listing,
                });
                self.rebuild_rows(cx);
            }
            Some(Err(error)) => {
                self.snapshot = None;
                self.listing_error = Some(error);
                self.listing_truncated = false;
                self.clear_rows(cx);
            }
            None => {}
        }
        self.status = match completion.probe {
            Ok(RemoteDirectoryExactPathState::ReadableDirectory) => {
                RemoteDirectoryPickerStatus::Readable
            }
            Ok(RemoteDirectoryExactPathState::Missing) => RemoteDirectoryPickerStatus::Missing,
            Err(error) => status_for_provider_error(error),
        };
        self.publish(cx);
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let (Some(parsed), Some(snapshot)) = (self.parsed.as_ref(), self.snapshot.as_ref()) else {
            return;
        };
        if snapshot.directory != *parsed.enumeration_directory() {
            return;
        }
        self.rows = filter_remote_workspace_rows(parsed, snapshot.listing.rows());
        let directory = snapshot.directory.clone();
        let operation_generation = self.operation_generation;
        let truncated = self.listing_truncated;
        let items = self
            .rows
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, row)| {
                remote_directory_palette_item(
                    RemoteDirectoryPickerItemId {
                        row,
                        directory: directory.clone(),
                        operation_generation,
                    },
                    truncated && index == 0,
                )
            })
            .collect();
        self.palette
            .update(cx, |palette, cx| palette.set_items(items, cx));
    }

    fn clear_rows(&mut self, cx: &mut Context<Self>) {
        self.rows.clear();
        self.palette
            .update(cx, |palette, cx| palette.set_items(Vec::new(), cx));
    }

    fn descend_to(
        &mut self,
        item: RemoteDirectoryPickerItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some()
            || item.operation_generation != self.operation_generation
            || self
                .parsed
                .as_ref()
                .map(ParsedRemoteDirectory::enumeration_directory)
                != Some(&item.directory)
        {
            return;
        }
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let Ok(directory) = descend_remote_workspace_query(parsed, &item.row) else {
            self.status = RemoteDirectoryPickerStatus::Other;
            self.publish(cx);
            return;
        };
        let query = directory.as_str().to_owned();
        if !self.palette.read(cx).can_set_query_exactly(&query, cx) {
            self.status = RemoteDirectoryPickerStatus::Other;
            self.publish(cx);
            return;
        }
        self.palette
            .update(cx, |palette, cx| palette.set_query(query, cx));
        self.refocus_path(window, cx);
    }

    fn confirm_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let Some(parsed) = self.parsed.clone() else {
            return;
        };
        match self.status {
            RemoteDirectoryPickerStatus::Readable => {
                self.start_validation(
                    parsed.exact_directory().clone(),
                    RemoteDirectoryValidationKind::Existing,
                    window,
                    cx,
                );
            }
            RemoteDirectoryPickerStatus::Missing => self.present_creation_alert(parsed, window, cx),
            _ => {}
        }
    }

    fn present_creation_alert(
        &mut self,
        parsed: ParsedRemoteDirectory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(RemoteDirectoryPickerBusy::CreationAlert);
        self.publish(cx);
        let directory = parsed.exact_directory().clone();
        let expected = directory.clone();
        let picker = cx.weak_entity();
        let window_handle = window.window_handle();
        let alert = Alert::new(
            ModalId::new(CREATE_ALERT_ID),
            "Create remote directory",
            "Create Remote Directory?",
            format!(
                "Create {}? Missing parent directorys will also be created.",
                parsed.display()
            ),
            vec![
                ModalAction::new(
                    true,
                    "Create Directory",
                    ModalActionRole::Affirmative,
                    "remote-workspace-create",
                )
                .default_action(true),
                ModalAction::new(
                    false,
                    "Cancel",
                    ModalActionRole::Cancel,
                    "remote-workspace-create-cancel",
                ),
            ],
        )
        .present(window, cx, move |outcome, cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = picker.update(cx, |picker, cx| {
                    picker.creation_alert = None;
                    if !picker.open
                        || picker.busy != Some(RemoteDirectoryPickerBusy::CreationAlert)
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
                            RemoteDirectoryValidationKind::Creation,
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
                self.status = RemoteDirectoryPickerStatus::Other;
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn start_validation(
        &mut self,
        directory: RemoteDirectory,
        kind: RemoteDirectoryValidationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        self.busy = Some(match kind {
            RemoteDirectoryValidationKind::Existing => RemoteDirectoryPickerBusy::Validating,
            RemoteDirectoryValidationKind::Creation => RemoteDirectoryPickerBusy::Creating,
        });
        let provider = Arc::clone(&self.provider);
        let request = directory.clone();
        self.validation_task.take();
        self.validation_task = Some(cx.spawn_in(window, async move |picker, cx| {
            let result = match kind {
                RemoteDirectoryValidationKind::Creation => {
                    match provider.create_directory_recursively(request.clone()).await {
                        Ok(()) => provider.validate_physical_identity(request).await,
                        Err(error) => Err(error),
                    }
                }
                RemoteDirectoryValidationKind::Existing => {
                    provider.validate_physical_identity(request).await
                }
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
            Ok(physical_directory) => {
                if self.account.is_none() {
                    return;
                }
                self.busy = Some(RemoteDirectoryPickerBusy::AwaitingActivation);
                self.sync_palette(cx);
                cx.emit(RemoteDirectoryPickerEvent::Confirmed(
                    RemoteDirectorySelection {
                        directory: completion.directory,
                        physical_directory,
                    },
                ));
                cx.notify();
            }
            Err(error) => {
                self.busy = None;
                self.status = status_for_provider_error(error);
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn can_confirm(&self) -> bool {
        self.busy.is_none()
            && matches!(
                self.status,
                RemoteDirectoryPickerStatus::Readable | RemoteDirectoryPickerStatus::Missing
            )
    }

    fn confirmation_label(&self) -> &'static str {
        if self.status == RemoteDirectoryPickerStatus::Missing {
            "Create Directory"
        } else {
            "Pin to This Directory"
        }
    }

    fn empty_text(&self) -> &'static str {
        if self.listing_truncated {
            return "Only the first 1024 directorys are shown; type an exact path to continue";
        }
        if let Some(error) = self.listing_error {
            return listing_error_text(error);
        }
        match self.status {
            RemoteDirectoryPickerStatus::DiscoveringAccount => "Discovering remote home\u{2026}",
            RemoteDirectoryPickerStatus::Loading => "Reading remote directory\u{2026}",
            RemoteDirectoryPickerStatus::Readable => "No directorys here",
            RemoteDirectoryPickerStatus::Missing => "No such remote directory",
            RemoteDirectoryPickerStatus::NotDirectory => "Not a remote directory",
            RemoteDirectoryPickerStatus::PermissionDenied => {
                "Permission denied for this remote directory"
            }
            RemoteDirectoryPickerStatus::ConnectionLost => "SSH connection was lost",
            RemoteDirectoryPickerStatus::UnsupportedLoginShell => UNSUPPORTED_LOGIN_SHELL_MESSAGE,
            RemoteDirectoryPickerStatus::Other => {
                "SpaceTerm couldn\u{2019}t read this remote directory"
            }
            RemoteDirectoryPickerStatus::Invalid(error) => error.message(),
        }
    }

    fn sync_palette(&self, cx: &mut Context<Self>) {
        let loading = self.busy.is_some();
        let confirm = CommandPaletteConfirm::new(
            self.confirmation_label(),
            cx.global::<crate::desktop_profile::DesktopPresentation>()
                .command_palette_confirm_shortcut(),
        )
        .disabled(!self.can_confirm())
        .debug_selector("remote-directory-picker-confirm");
        self.palette.update(cx, |palette, cx| {
            palette.set_confirm(Some(confirm), cx);
            palette.set_no_results_text(self.empty_text(), cx);
            palette.set_loading(loading, cx);
            palette.set_query_editable(!loading, cx);
            palette.set_dismissible(!loading, cx);
            palette.set_escape_cancellable(
                matches!(self.busy, Some(RemoteDirectoryPickerBusy::Validating)),
                cx,
            );
        });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.sync_palette(cx);
        cx.emit(RemoteDirectoryPickerEvent::StateChanged);
        cx.notify();
    }

    #[cfg(test)]
    fn row_names(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.name().to_owned()).collect()
    }
}

impl RemoteDirectoryFormatError {
    const fn message(self) -> &'static str {
        match self {
            Self::Relative => "Enter an absolute path beginning with / or ~/.",
            Self::BareTilde => "Use ~/ to open your remote home directory.",
            Self::UnsupportedTilde => "Only ~/ is supported for home-relative remote paths.",
            Self::InvalidControlCharacter => "Remote paths cannot contain control characters.",
        }
    }
}

/// Keeps a hierarchy shortcut from mutating Workspaces, Tabs, or Panes behind the open picker.
///
/// The Command Palette owns focus, pointer, and dismissal isolation, but the application's
/// hierarchy actions are registered above it and would otherwise still fire.
fn block_parent_action<A: Action>(_: &A, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

impl Render for RemoteDirectoryPicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .when(self.blocks_terminal_input(), |picker| {
                picker
                    .capture_action(block_parent_action::<NewWorkspace>)
                    .capture_action(block_parent_action::<super::NewRemoteWorkspace>)
                    .capture_action(block_parent_action::<SwitchWorkspace>)
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

fn remote_directory_palette_item(
    item: RemoteDirectoryPickerItemId,
    show_truncation_notice: bool,
) -> CommandPaletteItem<RemoteDirectoryPickerItemId> {
    let selector = format!("remote-directory-picker-row-{}", item.row.name());
    let label = format!("{}/", item.row.name());
    let palette_item = CommandPaletteItem::new(item, label)
        .leading_icon(move |foreground, size| {
            Icon::new(IconName::Folder, size, foreground).into_any_element()
        })
        .debug_selector(selector);
    if show_truncation_notice {
        palette_item.section("First 1024 directorys shown; type an exact path for others")
    } else {
        palette_item
    }
}

fn listing_error_text(error: RemoteDirectoryProviderError) -> &'static str {
    match error {
        RemoteDirectoryProviderError::ConnectionLost => "SSH connection was lost",
        RemoteDirectoryProviderError::Missing => "Remote parent directory no longer exists",
        RemoteDirectoryProviderError::NotDirectory => "Remote parent path is not a directory",
        RemoteDirectoryProviderError::PermissionDenied => {
            "Permission denied while listing this remote directory"
        }
        RemoteDirectoryProviderError::UnsupportedLoginShell => UNSUPPORTED_LOGIN_SHELL_MESSAGE,
        RemoteDirectoryProviderError::InvalidResponse | RemoteDirectoryProviderError::Other => {
            "SpaceTerm couldn\u{2019}t list this remote directory"
        }
    }
}

fn status_for_provider_error(error: RemoteDirectoryProviderError) -> RemoteDirectoryPickerStatus {
    match error {
        RemoteDirectoryProviderError::ConnectionLost => RemoteDirectoryPickerStatus::ConnectionLost,
        RemoteDirectoryProviderError::Missing => RemoteDirectoryPickerStatus::Missing,
        RemoteDirectoryProviderError::NotDirectory => RemoteDirectoryPickerStatus::NotDirectory,
        RemoteDirectoryProviderError::PermissionDenied => {
            RemoteDirectoryPickerStatus::PermissionDenied
        }
        RemoteDirectoryProviderError::UnsupportedLoginShell => {
            RemoteDirectoryPickerStatus::UnsupportedLoginShell
        }
        RemoteDirectoryProviderError::InvalidResponse | RemoteDirectoryProviderError::Other => {
            RemoteDirectoryPickerStatus::Other
        }
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

    use gpui::{
        BackgroundExecutor, Keystroke, Modifiers, Task, TestAppContext, VisualTestContext, div,
    };

    use super::*;

    #[test]
    fn remote_picker_wrapper_debug_should_redact_account_paths_and_rows() {
        let parsed = parse_remote_directory("/sensitive/project").unwrap();
        let row = RemoteDirectoryRow::new("sensitive-child".to_owned()).unwrap();
        let listing = RemoteDirectoryListing::new(vec![row.clone()]);
        let account = RemoteWorkspaceAccount::new(
            "sensitive-user".to_owned(),
            RemoteDirectoryIdentity::new("/sensitive/home".to_owned()).unwrap(),
            "/bin/zsh".to_owned(),
        )
        .unwrap();
        let selection = RemoteDirectorySelection::new(
            RemoteDirectory::new("/sensitive/project".to_owned()).unwrap(),
            RemoteDirectoryIdentity::new("/sensitive/project".to_owned()).unwrap(),
        );
        let event = RemoteDirectoryPickerEvent::Confirmed(selection);

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
        listings: VecDeque<Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>>>,
        probes: VecDeque<Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>>>,
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
        ) -> Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>> {
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
        ) -> Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>> {
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
        ) -> Task<Result<RemoteDirectoryListing, RemoteDirectoryProviderError>> {
            self.pending()
        }

        fn probe_exact_path(
            &self,
            _: RemoteDirectory,
        ) -> Task<Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>> {
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

    struct RemoteDirectoryPickerHarness {
        picker: gpui::Entity<RemoteDirectoryPicker>,
    }

    impl gpui::Render for RemoteDirectoryPickerHarness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            spaceterm_ui::ModalLayer::new(div().size_full().child(self.picker.clone()))
        }
    }

    fn remote_account() -> RemoteWorkspaceAccount {
        RemoteWorkspaceAccount::new(
            "tester".to_owned(),
            RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap(),
            "/bin/zsh".to_owned(),
        )
        .unwrap()
    }

    fn scripted_provider(
        listings: impl IntoIterator<
            Item = Result<Vec<RemoteDirectoryRow>, RemoteDirectoryProviderError>,
        >,
        probes: impl IntoIterator<
            Item = Result<RemoteDirectoryExactPathState, RemoteDirectoryProviderError>,
        >,
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
                    .map(|result| result.map(RemoteDirectoryListing::new))
                    .map(Task::ready)
                    .collect(),
                probes: probes.into_iter().map(Task::ready).collect(),
                creations: creations.into_iter().map(Task::ready).collect(),
                validations: validations.into_iter().map(Task::ready).collect(),
                ..ScriptedRemoteDirectoryProviderState::default()
            })),
        })
    }

    fn remote_directory_picker(
        provider: Arc<ScriptedRemoteDirectoryProvider>,
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<RemoteDirectoryPicker>,
        Rc<RefCell<Vec<RemoteDirectoryPickerEvent>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init)
            .expect("UI initialization should succeed");
        let injected: Arc<dyn RemoteDirectoryProvider + Send + Sync> = provider;
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| RemoteDirectoryPicker::new(injected, window, cx));
            cx.subscribe(
                &picker,
                move |_, _, event: &RemoteDirectoryPickerEvent, _| {
                    recorded_events.borrow_mut().push(event.clone());
                },
            )
            .detach();
            RemoteDirectoryPickerHarness { picker }
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
                Ok(RemoteDirectoryExactPathState::ReadableDirectory),
                Ok(RemoteDirectoryExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(Arc::clone(&provider), cx);

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
    fn missing_remote_path_changes_the_sole_confirmation_to_create(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::Missing)],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| (
                picker.confirmation_label(),
                picker.can_confirm()
            )),
            ("Create Directory", true)
        );
        assert!(cx.debug_bounds("remote-directory-picker-confirm").is_some());
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
                Ok(RemoteDirectoryExactPathState::ReadableDirectory),
                Ok(RemoteDirectoryExactPathState::Missing),
            ],
            [Ok(())],
            [Ok(identity.clone())],
        );
        let (picker, events, cx) = remote_directory_picker(Arc::clone(&provider), cx);
        set_remote_input(&picker, "~/Projects/new", cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
        });
        cx.run_until_parked();
        let create = cx
            .debug_bounds("modal-action-remote-workspace-create")
            .expect("creation Alert should expose its typed affirmative action");
        cx.simulate_click(create.center(), Modifiers::none());
        cx.run_until_parked();

        let expected = remote_directory("~/Projects/new");
        let records = provider.state.lock().unwrap();
        assert_eq!(records.created_directories, vec![expected.clone()]);
        assert_eq!(records.validated_directories, vec![expected.clone()]);
        drop(records);
        let selection = events.borrow().iter().find_map(|event| match event {
            RemoteDirectoryPickerEvent::Confirmed(selection) => Some(selection.clone()),
            _ => None,
        });
        let selection = selection.expect("validated selection should be emitted");
        assert_eq!(selection.directory(), &expected);
        assert_eq!(selection.physical_directory(), &identity);
    }

    #[test]
    fn unsupported_login_shell_should_have_an_actionable_account_status() {
        assert_eq!(
            status_for_provider_error(RemoteDirectoryProviderError::UnsupportedLoginShell),
            RemoteDirectoryPickerStatus::UnsupportedLoginShell
        );
        assert_eq!(
            UNSUPPORTED_LOGIN_SHELL_MESSAGE,
            "The remote login shell does not support login mode. Choose another account or shell."
        );
    }

    #[gpui::test]
    fn stale_remote_refresh_cannot_replace_the_current_readable_state(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(provider, cx);
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
                    listing: Some(Err(RemoteDirectoryProviderError::PermissionDenied)),
                    probe: Err(RemoteDirectoryProviderError::ConnectionLost),
                },
                cx,
            );
        });

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.status),
            RemoteDirectoryPickerStatus::Readable
        );
    }

    #[gpui::test]
    fn remote_listing_errors_are_specific_and_never_keep_unread_rows(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Err(RemoteDirectoryProviderError::PermissionDenied)],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| {
                (
                    picker.status,
                    picker.can_confirm(),
                    picker.listing_error,
                    picker.empty_text(),
                    picker.row_names(),
                )
            }),
            (
                RemoteDirectoryPickerStatus::Readable,
                true,
                Some(RemoteDirectoryProviderError::PermissionDenied),
                "Permission denied while listing this remote directory",
                Vec::<String>::new(),
            )
        );
    }

    #[gpui::test]
    fn changing_directory_clears_rows_and_stale_activation_is_inert(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(remote_rows(["Projects"])), Ok(remote_rows(["Current"]))],
            [
                Ok(RemoteDirectoryExactPathState::ReadableDirectory),
                Ok(RemoteDirectoryExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(provider, cx);
        let stale_item = picker.read_with(cx, |picker, cx| {
            picker.palette.read(cx).selected_item_id().cloned().unwrap()
        });

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
            picker.update(cx, |picker, cx| picker.descend_to(stale_item, window, cx));
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
            let picker = cx.new(|cx| RemoteDirectoryPicker::new(injected, window, cx));
            RemoteDirectoryPickerHarness { picker }
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
    fn oversized_remote_listing_is_bounded_and_exposes_exact_path_guidance(
        cx: &mut TestAppContext,
    ) {
        let rows = (0..MAXIMUM_REMOTE_DIRECTORY_ROWS + 7)
            .map(|index| RemoteDirectoryRow::new(format!("directory-{index:04}")).unwrap())
            .collect::<Vec<_>>();
        let provider = scripted_provider(
            [Ok(rows)],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_directory_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.row_names().len()),
            MAXIMUM_REMOTE_DIRECTORY_ROWS
        );
        assert!(picker.read_with(cx, |picker, _| picker.listing_truncated));
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.empty_text()),
            "Only the first 1024 directorys are shown; type an exact path to continue"
        );
    }

    #[gpui::test]
    fn cancelling_a_modal_pending_open_unblocks_input_and_emits_dismissed_once(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_directory_picker(provider, cx);
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
                .filter(|event| **event == RemoteDirectoryPickerEvent::Dismissed)
                .count(),
            1
        );
    }

    #[gpui::test]
    fn escape_dismisses_directory_selection(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_directory_picker(provider, cx);
        assert!(picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert!(cx.update(|window, cx| picker.read(cx).path_input_is_focused(window, cx)));

        cx.update(|window, cx| {
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        cx.run_until_parked();

        assert!(!picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert!(
            events
                .borrow()
                .contains(&RemoteDirectoryPickerEvent::Dismissed)
        );
    }

    #[gpui::test]
    fn programmatic_dismissal_emits_dismissed(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_directory_picker(provider, cx);

        assert!(
            cx.update(|window, cx| { picker.update(cx, |picker, cx| picker.dismiss(window, cx)) })
        );
        cx.run_until_parked();

        assert!(
            events
                .borrow()
                .contains(&RemoteDirectoryPickerEvent::Dismissed)
        );
    }

    #[gpui::test]
    fn validation_makes_the_path_read_only_until_the_parent_advances(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = remote_directory_picker(provider, cx);

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
                .any(|event| { matches!(event, RemoteDirectoryPickerEvent::Confirmed(_)) })
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.busy),
            Some(RemoteDirectoryPickerBusy::AwaitingActivation)
        );
    }

    #[gpui::test]
    fn forced_cancel_should_drop_busy_validation_and_reject_late_selection(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
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
        let (picker, events, cx) = remote_directory_picker(provider, cx);
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.confirm_current(window, cx)));
        cx.run_until_parked();
        assert!(picker.read_with(cx, |picker, _| picker.busy.is_some()));
        cx.update(|window, cx| picker.update(cx, |picker, cx| picker.cancel(window, cx)));
        cx.run_until_parked();
        assert_eq!(dropped.load(AtomicOrdering::SeqCst), 1);
        assert!(!picker.read_with(cx, |picker, _| picker.blocks_terminal_input()));
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, RemoteDirectoryPickerEvent::Confirmed(_)))
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| **event == RemoteDirectoryPickerEvent::Dismissed)
                .count(),
            1
        );
    }

    #[gpui::test]
    fn escape_during_validation_dismisses_without_accepting_late_success(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteDirectoryExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = remote_directory_picker(provider, cx);
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
        });
        cx.run_until_parked();
        assert!(
            events
                .borrow()
                .contains(&RemoteDirectoryPickerEvent::Dismissed)
        );
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, RemoteDirectoryPickerEvent::Confirmed(_)))
        );
    }

    fn set_remote_input(
        picker: &gpui::Entity<RemoteDirectoryPicker>,
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
        let parsed = parse_remote_directory("/").unwrap();

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
        let parsed = parse_remote_directory("~/Projects/SpaceTerm").unwrap();

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
        let home_relative = parse_remote_directory("~//Projects//SpaceTerm").unwrap();
        let absolute = parse_remote_directory("//srv///projects//SpaceTerm").unwrap();

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
        let parsed = parse_remote_directory("~/Projects//SpaceTerm/").unwrap();

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
            parse_remote_directory("Projects"),
            Err(RemoteDirectoryFormatError::Relative)
        );
        assert_eq!(
            parse_remote_directory("~"),
            Err(RemoteDirectoryFormatError::BareTilde)
        );
        assert_eq!(
            parse_remote_directory("~other/Projects"),
            Err(RemoteDirectoryFormatError::UnsupportedTilde)
        );
    }

    #[test]
    fn hidden_directories_should_be_revealed_only_from_a_dot_leaf() {
        let ordinary = parse_remote_directory("~/Projects/").unwrap();
        let dotted = parse_remote_directory("~/Projects/.").unwrap();
        let entries = remote_rows([".config", "SpaceTerm", ".ssh"]);

        assert_eq!(
            row_names(filter_remote_workspace_rows(&ordinary, &entries)),
            vec!["SpaceTerm"]
        );
        assert_eq!(
            row_names(filter_remote_workspace_rows(&dotted, &entries)),
            vec![".config", ".ssh"]
        );
    }

    #[test]
    fn rows_should_filter_case_insensitive_prefixes_and_sort_deterministically() {
        let parsed = parse_remote_directory("~/Projects/sp").unwrap();
        let entries = remote_rows(["spaceTerm", "Spatial", "SpaceTerm", "tools"]);

        assert_eq!(
            row_names(filter_remote_workspace_rows(&parsed, &entries)),
            vec!["SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn directory_rows_should_reject_non_one_level_names() {
        assert!(RemoteDirectoryRow::new("nested/project".to_owned()).is_err());
        assert!(RemoteDirectoryRow::new("project\nname".to_owned()).is_err());
        assert!(RemoteDirectoryRow::new(String::new()).is_err());
    }

    #[test]
    fn activating_a_row_should_rewrite_the_query_to_descend() {
        let parsed = parse_remote_directory("~//Projects//sp").unwrap();
        let row = RemoteDirectoryRow::new("SpaceTerm".to_owned()).unwrap();

        assert_eq!(
            descend_remote_workspace_query(&parsed, &row)
                .unwrap()
                .as_str(),
            "~//Projects//SpaceTerm/"
        );
    }

    fn remote_rows<const N: usize>(names: [&str; N]) -> Vec<RemoteDirectoryRow> {
        names
            .into_iter()
            .map(|name| RemoteDirectoryRow::new(name.to_owned()).unwrap())
            .collect()
    }

    fn row_names(rows: Vec<RemoteDirectoryRow>) -> Vec<String> {
        rows.into_iter().map(|row| row.name().to_owned()).collect()
    }
}
