#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Remote Workspace Picker is wired into Workspace Manager in the next slice"
    )
)]

use std::{cmp::Ordering, sync::Arc};

use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, Render, Task, Window, div, px};
use gpui_symbols::Icon;
use spaceterm_ui::{
    Alert, AlertOutcome, CommandPalette, CommandPaletteActivationPolicy, CommandPaletteCloseReason,
    CommandPaletteConfirm, CommandPaletteEvent, CommandPaletteItem, CommandPaletteLifecycleEvent,
    CommandPaletteMatching, CommandPaletteReplacementFocus, ModalAction, ModalActionRole, ModalId,
    ModalPresentationHandle,
};

use crate::domain::{RemoteDirectoryIdentity, RemoteWorkspaceDirectory, RemoteWorkspaceValueError};
use crate::theme::{ACTIVE_THEME, Color};

const HOME_DISPLAY: &str = "~/";
const ROW_ICON_SIZE: f32 = 14.0;
const CREATE_ALERT_ID: &str = "remote-workspace-create-folder";
pub(super) const MAXIMUM_REMOTE_WORKSPACE_DIRECTORY_ROWS: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceAccountError {
    InvalidUser,
    InvalidLoginShell,
}

/// Account facts discovered from the connected destination before remote path navigation begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteWorkspaceAccount {
    user: String,
    home_identity: RemoteDirectoryIdentity,
    login_shell: String,
}

impl RemoteWorkspaceAccount {
    pub(crate) fn new(
        user: String,
        home_identity: RemoteDirectoryIdentity,
        login_shell: String,
    ) -> Result<Self, RemoteWorkspaceAccountError> {
        if user.is_empty()
            || user
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(RemoteWorkspaceAccountError::InvalidUser);
        }
        if login_shell.is_empty() || login_shell.chars().any(char::is_control) {
            return Err(RemoteWorkspaceAccountError::InvalidLoginShell);
        }
        Ok(Self {
            user,
            home_identity,
            login_shell,
        })
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    pub(crate) const fn home_identity(&self) -> &RemoteDirectoryIdentity {
        &self.home_identity
    }

    pub(crate) fn login_shell(&self) -> &str {
        &self.login_shell
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceProviderError {
    ConnectionLost,
    Missing,
    NotDirectory,
    PermissionDenied,
    InvalidResponse,
    Other,
}

/// The connected-SSH boundary used by the picker. Every path crossing it is a remote string type.
pub(crate) trait RemoteWorkspaceProvider: Send + Sync {
    fn discover_account(
        &self,
    ) -> Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>>;

    fn list_directories(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>>;

    fn probe_exact_path(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>>;

    fn create_directory_recursively(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<(), RemoteWorkspaceProviderError>>;

    fn validate_physical_identity(
        &self,
        directory: RemoteWorkspaceDirectory,
    ) -> Task<Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspacePathFormatError {
    Relative,
    BareTilde,
    UnsupportedTilde,
    InvalidControlCharacter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedRemoteWorkspacePath {
    display: String,
    exact_directory: RemoteWorkspaceDirectory,
    enumeration_directory: RemoteWorkspaceDirectory,
    descend_prefix: String,
    leaf_filter: String,
    trailing_separator: bool,
}

impl ParsedRemoteWorkspacePath {
    pub(super) fn display(&self) -> &str {
        &self.display
    }

    pub(super) const fn exact_directory(&self) -> &RemoteWorkspaceDirectory {
        &self.exact_directory
    }

    pub(super) const fn enumeration_directory(&self) -> &RemoteWorkspaceDirectory {
        &self.enumeration_directory
    }

    pub(super) fn leaf_filter(&self) -> &str {
        &self.leaf_filter
    }

    pub(super) const fn trailing_separator(&self) -> bool {
        self.trailing_separator
    }

    pub(super) fn reveals_hidden_directories(&self) -> bool {
        self.leaf_filter.starts_with('.')
    }
}

pub(super) fn parse_remote_workspace_path(
    input: &str,
) -> Result<ParsedRemoteWorkspacePath, RemoteWorkspacePathFormatError> {
    if input == "~" {
        return Err(RemoteWorkspacePathFormatError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with("~/") {
        return Err(RemoteWorkspacePathFormatError::UnsupportedTilde);
    }
    if !input.starts_with('/') && !input.starts_with("~/") {
        return Err(RemoteWorkspacePathFormatError::Relative);
    }

    let exact_directory = RemoteWorkspaceDirectory::new(input.to_owned())
        .map_err(|_| RemoteWorkspacePathFormatError::InvalidControlCharacter)?;
    let trailing_separator = input.ends_with('/');
    let (enumeration_spelling, descend_prefix, leaf_filter) = if trailing_separator {
        (input, input.to_owned(), String::new())
    } else {
        let separator = input
            .rfind('/')
            .ok_or(RemoteWorkspacePathFormatError::Relative)?;
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
    let enumeration_directory = RemoteWorkspaceDirectory::new(enumeration_spelling.to_owned())
        .map_err(|_| RemoteWorkspacePathFormatError::InvalidControlCharacter)?;

    Ok(ParsedRemoteWorkspacePath {
        display: input.to_owned(),
        exact_directory,
        enumeration_directory,
        descend_prefix,
        leaf_filter,
        trailing_separator,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceDirectoryRowError {
    InvalidName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteWorkspaceDirectoryRow {
    name: String,
}

/// A defensively bounded one-level directory result from a remote provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteWorkspaceDirectoryListing {
    rows: Vec<RemoteWorkspaceDirectoryRow>,
    truncated: bool,
}

impl RemoteWorkspaceDirectoryListing {
    pub(crate) fn new(rows: Vec<RemoteWorkspaceDirectoryRow>) -> Self {
        Self::from_remote(rows, false)
    }

    pub(crate) fn from_remote(
        mut rows: Vec<RemoteWorkspaceDirectoryRow>,
        remotely_truncated: bool,
    ) -> Self {
        let truncated = remotely_truncated || rows.len() > MAXIMUM_REMOTE_WORKSPACE_DIRECTORY_ROWS;
        rows.truncate(MAXIMUM_REMOTE_WORKSPACE_DIRECTORY_ROWS);
        Self { rows, truncated }
    }

    pub(crate) fn rows(&self) -> &[RemoteWorkspaceDirectoryRow] {
        &self.rows
    }

    pub(crate) const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

impl RemoteWorkspaceDirectoryRow {
    pub(crate) fn new(name: String) -> Result<Self, RemoteWorkspaceDirectoryRowError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.chars().any(char::is_control)
        {
            return Err(RemoteWorkspaceDirectoryRowError::InvalidName);
        }
        Ok(Self { name })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

pub(super) fn filter_remote_workspace_rows(
    parsed: &ParsedRemoteWorkspacePath,
    entries: &[RemoteWorkspaceDirectoryRow],
) -> Vec<RemoteWorkspaceDirectoryRow> {
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
    parsed: &ParsedRemoteWorkspacePath,
    row: &RemoteWorkspaceDirectoryRow,
) -> Result<RemoteWorkspaceDirectory, RemoteWorkspaceValueError> {
    RemoteWorkspaceDirectory::new(format!("{}{}/", parsed.descend_prefix, row.name()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteWorkspaceExactPathState {
    ReadableDirectory,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspaceConfirmation {
    OpenRemoteProject(RemoteWorkspaceDirectory),
    CreateFolder(RemoteWorkspaceDirectory),
}

pub(super) fn remote_workspace_confirmation(
    parsed: &ParsedRemoteWorkspacePath,
    state: RemoteWorkspaceExactPathState,
) -> RemoteWorkspaceConfirmation {
    match state {
        RemoteWorkspaceExactPathState::ReadableDirectory => {
            RemoteWorkspaceConfirmation::OpenRemoteProject(parsed.exact_directory.clone())
        }
        RemoteWorkspaceExactPathState::Missing => {
            RemoteWorkspaceConfirmation::CreateFolder(parsed.exact_directory.clone())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RemoteWorkspaceSelection {
    directory: RemoteWorkspaceDirectory,
    physical_directory: RemoteDirectoryIdentity,
    account: RemoteWorkspaceAccount,
}

impl RemoteWorkspaceSelection {
    #[cfg(test)]
    pub(super) fn new(
        directory: RemoteWorkspaceDirectory,
        physical_directory: RemoteDirectoryIdentity,
        account: RemoteWorkspaceAccount,
    ) -> Self {
        Self {
            directory,
            physical_directory,
            account,
        }
    }

    pub(super) const fn directory(&self) -> &RemoteWorkspaceDirectory {
        &self.directory
    }

    pub(super) const fn physical_directory(&self) -> &RemoteDirectoryIdentity {
        &self.physical_directory
    }

    pub(super) const fn account(&self) -> &RemoteWorkspaceAccount {
        &self.account
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RemoteWorkspacePickerEvent {
    StateChanged,
    BackToHost,
    Dismissed,
    Confirmed(RemoteWorkspaceSelection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteWorkspacePickerStatus {
    DiscoveringAccount,
    Loading,
    Readable,
    Missing,
    NotDirectory,
    PermissionDenied,
    ConnectionLost,
    Other,
    Invalid(RemoteWorkspacePathFormatError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteWorkspacePickerBusy {
    CreationAlert,
    Creating,
    Validating,
    AwaitingActivation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteWorkspacePickerItemId {
    row: RemoteWorkspaceDirectoryRow,
    directory: RemoteWorkspaceDirectory,
    operation_generation: u64,
}

#[derive(Clone)]
struct LoadedRemoteDirectorySnapshot {
    directory: RemoteWorkspaceDirectory,
    listing: RemoteWorkspaceDirectoryListing,
}

struct RefreshCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    parsed: ParsedRemoteWorkspacePath,
    listing: Option<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>>,
    probe: Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteWorkspaceValidationKind {
    Existing,
    Creation,
}

struct ValidationCompletion {
    lifecycle_generation: u64,
    operation_generation: u64,
    directory: RemoteWorkspaceDirectory,
    result: Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>,
}

/// One connected-destination directory chooser built on the reusable Command Palette.
pub(super) struct RemoteWorkspacePicker {
    provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
    palette: Entity<CommandPalette<RemoteWorkspacePickerItemId>>,
    opening: bool,
    open: bool,
    lifecycle_generation: u64,
    operation_generation: u64,
    account: Option<RemoteWorkspaceAccount>,
    parsed: Option<ParsedRemoteWorkspacePath>,
    snapshot: Option<LoadedRemoteDirectorySnapshot>,
    rows: Vec<RemoteWorkspaceDirectoryRow>,
    listing_error: Option<RemoteWorkspaceProviderError>,
    listing_truncated: bool,
    status: RemoteWorkspacePickerStatus,
    busy: Option<RemoteWorkspacePickerBusy>,
    creation_alert: Option<ModalPresentationHandle>,
}

impl EventEmitter<RemoteWorkspacePickerEvent> for RemoteWorkspacePicker {}

impl RemoteWorkspacePicker {
    pub(super) fn new(
        provider: Arc<dyn RemoteWorkspaceProvider + Send + Sync>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| {
            let mut palette = CommandPalette::new("Remote workspace path", Vec::new(), window, cx);
            palette.set_matching(CommandPaletteMatching::Caller, cx);
            palette.set_activation(CommandPaletteActivationPolicy::Continue, cx);
            palette
        });
        cx.subscribe_in(
            &palette,
            window,
            |picker, _, event: &CommandPaletteEvent<RemoteWorkspacePickerItemId>, window, cx| {
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
            status: RemoteWorkspacePickerStatus::DiscoveringAccount,
            busy: None,
            creation_alert: None,
        }
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    pub(super) fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.open_with_replacement(Some(replacement), window, cx)
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
        self.status = RemoteWorkspacePickerStatus::DiscoveringAccount;
        self.busy = None;
        self.creation_alert = None;
        self.palette.update(cx, |palette, cx| {
            palette.set_query_editable(true, cx);
            palette.set_dismissible(true, cx);
            if let Some(replacement) = replacement {
                palette.open_replacing(replacement, window, cx);
            } else {
                palette.open(window, cx);
            }
        });
        true
    }

    pub(super) const fn is_open(&self) -> bool {
        self.open
    }

    pub(super) const fn blocks_terminal_input(&self) -> bool {
        self.open || self.opening
    }

    pub(super) fn path_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.palette.read(cx).editor_is_focused(window, cx)
    }

    pub(super) fn refocus_path(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.palette
                .update(cx, |palette, cx| palette.focus_editor(window, cx));
        }
    }

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

    pub(super) fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<CommandPaletteReplacementFocus> {
        if !self.open || self.busy.is_some() {
            return None;
        }
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_for_replacement(window, cx)
        })
    }

    pub(super) fn complete_activation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open || self.busy != Some(RemoteWorkspacePickerBusy::AwaitingActivation) {
            return false;
        }
        self.busy = None;
        self.palette.update(cx, |palette, cx| {
            palette.dismiss_without_restoring_focus(window, cx)
        })
    }

    pub(super) fn activation_failed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.busy == Some(RemoteWorkspacePickerBusy::AwaitingActivation) {
            self.busy = None;
            self.status = RemoteWorkspacePickerStatus::Other;
            self.publish(cx);
            self.refocus_path(window, cx);
        }
    }

    fn reduce_palette_event(
        &mut self,
        event: &CommandPaletteEvent<RemoteWorkspacePickerItemId>,
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
        self.lifecycle_generation = self.lifecycle_generation.wrapping_add(1);
        self.operation_generation = self.operation_generation.wrapping_add(1);
        match reason {
            CommandPaletteCloseReason::Escape => cx.emit(RemoteWorkspacePickerEvent::BackToHost),
            CommandPaletteCloseReason::Completed => {}
            _ => cx.emit(RemoteWorkspacePickerEvent::Dismissed),
        }
        cx.emit(RemoteWorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    fn start_account_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        let task = self.provider.discover_account();
        cx.spawn_in(window, async move |picker, cx| {
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
        })
        .detach();
    }

    fn refresh_for_input(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.busy.is_some() || self.account.is_none() {
            return;
        }
        let parsed = match parse_remote_workspace_path(&value) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.parsed = None;
                self.status = RemoteWorkspacePickerStatus::Invalid(error);
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
        self.status = RemoteWorkspacePickerStatus::Loading;
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
        cx.spawn_in(window, async move |picker, cx| {
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
            Ok(RemoteWorkspaceExactPathState::ReadableDirectory) => {
                RemoteWorkspacePickerStatus::Readable
            }
            Ok(RemoteWorkspaceExactPathState::Missing) => RemoteWorkspacePickerStatus::Missing,
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
                    RemoteWorkspacePickerItemId {
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
        item: RemoteWorkspacePickerItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some()
            || item.operation_generation != self.operation_generation
            || self
                .parsed
                .as_ref()
                .map(ParsedRemoteWorkspacePath::enumeration_directory)
                != Some(&item.directory)
        {
            return;
        }
        let Some(parsed) = self.parsed.as_ref() else {
            return;
        };
        let Ok(directory) = descend_remote_workspace_query(parsed, &item.row) else {
            self.status = RemoteWorkspacePickerStatus::Other;
            self.publish(cx);
            return;
        };
        let query = directory.as_str().to_owned();
        if !self.palette.read(cx).can_set_query_exactly(&query, cx) {
            self.status = RemoteWorkspacePickerStatus::Other;
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
            RemoteWorkspacePickerStatus::Readable => {
                self.start_validation(
                    parsed.exact_directory().clone(),
                    RemoteWorkspaceValidationKind::Existing,
                    window,
                    cx,
                );
            }
            RemoteWorkspacePickerStatus::Missing => self.present_creation_alert(parsed, window, cx),
            _ => {}
        }
    }

    fn present_creation_alert(
        &mut self,
        parsed: ParsedRemoteWorkspacePath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(RemoteWorkspacePickerBusy::CreationAlert);
        self.publish(cx);
        let directory = parsed.exact_directory().clone();
        let expected = directory.clone();
        let picker = cx.weak_entity();
        let window_handle = window.window_handle();
        let alert = Alert::new(
            ModalId::new(CREATE_ALERT_ID),
            "Create remote folder",
            "Create Remote Folder?",
            format!(
                "Create {}? Missing parent folders will also be created.",
                parsed.display()
            ),
            vec![
                ModalAction::new(
                    true,
                    "Create Folder",
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
                        || picker.busy != Some(RemoteWorkspacePickerBusy::CreationAlert)
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
                            RemoteWorkspaceValidationKind::Creation,
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
                self.status = RemoteWorkspacePickerStatus::Other;
                self.publish(cx);
                self.refocus_path(window, cx);
            }
        }
    }

    fn start_validation(
        &mut self,
        directory: RemoteWorkspaceDirectory,
        kind: RemoteWorkspaceValidationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.operation_generation = self.operation_generation.wrapping_add(1);
        let operation_generation = self.operation_generation;
        let lifecycle_generation = self.lifecycle_generation;
        self.busy = Some(match kind {
            RemoteWorkspaceValidationKind::Existing => RemoteWorkspacePickerBusy::Validating,
            RemoteWorkspaceValidationKind::Creation => RemoteWorkspacePickerBusy::Creating,
        });
        let provider = Arc::clone(&self.provider);
        let request = directory.clone();
        cx.spawn_in(window, async move |picker, cx| {
            let result = match kind {
                RemoteWorkspaceValidationKind::Creation => {
                    match provider.create_directory_recursively(request.clone()).await {
                        Ok(()) => provider.validate_physical_identity(request).await,
                        Err(error) => Err(error),
                    }
                }
                RemoteWorkspaceValidationKind::Existing => {
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
            || self.parsed.as_ref().map(|parsed| parsed.exact_directory())
                != Some(&completion.directory)
        {
            return;
        }
        match completion.result {
            Ok(physical_directory) => {
                let Some(account) = self.account.clone() else {
                    return;
                };
                self.busy = Some(RemoteWorkspacePickerBusy::AwaitingActivation);
                self.sync_palette(cx);
                cx.emit(RemoteWorkspacePickerEvent::Confirmed(
                    RemoteWorkspaceSelection {
                        directory: completion.directory,
                        physical_directory,
                        account,
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
                RemoteWorkspacePickerStatus::Readable | RemoteWorkspacePickerStatus::Missing
            )
    }

    fn confirmation_label(&self) -> &'static str {
        if self.status == RemoteWorkspacePickerStatus::Missing {
            "Create Folder"
        } else {
            "Open Remote Project"
        }
    }

    fn empty_text(&self) -> &'static str {
        if self.listing_truncated {
            return "Only the first 1024 folders are shown; type an exact path to continue";
        }
        if let Some(error) = self.listing_error {
            return listing_error_text(error);
        }
        match self.status {
            RemoteWorkspacePickerStatus::DiscoveringAccount => "Discovering remote home\u{2026}",
            RemoteWorkspacePickerStatus::Loading => "Reading remote folder\u{2026}",
            RemoteWorkspacePickerStatus::Readable => "No folders here",
            RemoteWorkspacePickerStatus::Missing => "No such remote folder",
            RemoteWorkspacePickerStatus::NotDirectory => "Not a remote folder",
            RemoteWorkspacePickerStatus::PermissionDenied => {
                "Permission denied for this remote folder"
            }
            RemoteWorkspacePickerStatus::ConnectionLost => "SSH connection was lost",
            RemoteWorkspacePickerStatus::Other => {
                "SpaceTerm couldn\u{2019}t read this remote folder"
            }
            RemoteWorkspacePickerStatus::Invalid(error) => error.message(),
        }
    }

    fn sync_palette(&self, cx: &mut Context<Self>) {
        let loading = self.busy.is_some();
        let confirm = CommandPaletteConfirm::new(self.confirmation_label())
            .disabled(!self.can_confirm())
            .debug_selector("remote-workspace-picker-confirm");
        self.palette.update(cx, |palette, cx| {
            palette.set_confirm(Some(confirm), cx);
            palette.set_no_results_text(self.empty_text(), cx);
            palette.set_loading(loading, cx);
            palette.set_query_editable(!loading, cx);
            palette.set_dismissible(!loading, cx);
        });
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.sync_palette(cx);
        cx.emit(RemoteWorkspacePickerEvent::StateChanged);
        cx.notify();
    }

    #[cfg(test)]
    fn row_names(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.name().to_owned()).collect()
    }
}

impl RemoteWorkspacePathFormatError {
    const fn message(self) -> &'static str {
        match self {
            Self::Relative => "Enter an absolute path beginning with / or ~/.",
            Self::BareTilde => "Use ~/ to open your remote home folder.",
            Self::UnsupportedTilde => "Only ~/ is supported for home-relative remote paths.",
            Self::InvalidControlCharacter => "Remote paths cannot contain control characters.",
        }
    }
}

impl Render for RemoteWorkspacePicker {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.palette.clone())
    }
}

fn remote_directory_palette_item(
    item: RemoteWorkspacePickerItemId,
    show_truncation_notice: bool,
) -> CommandPaletteItem<RemoteWorkspacePickerItemId> {
    let icon_color = gpui_color(ACTIVE_THEME.icon_muted);
    let selector = format!("remote-workspace-picker-row-{}", item.row.name());
    let label = format!("{}/", item.row.name());
    let palette_item = CommandPaletteItem::new(item, label)
        .leading_icon(move |_| {
            Icon::new("folder")
                .size(px(ROW_ICON_SIZE))
                .color(icon_color)
                .into_any_element()
        })
        .debug_selector(selector);
    if show_truncation_notice {
        palette_item.section("First 1024 folders shown; type an exact path for others")
    } else {
        palette_item
    }
}

fn listing_error_text(error: RemoteWorkspaceProviderError) -> &'static str {
    match error {
        RemoteWorkspaceProviderError::ConnectionLost => "SSH connection was lost",
        RemoteWorkspaceProviderError::Missing => "Remote parent folder no longer exists",
        RemoteWorkspaceProviderError::NotDirectory => "Remote parent path is not a folder",
        RemoteWorkspaceProviderError::PermissionDenied => {
            "Permission denied while listing this remote folder"
        }
        RemoteWorkspaceProviderError::InvalidResponse | RemoteWorkspaceProviderError::Other => {
            "SpaceTerm couldn\u{2019}t list this remote folder"
        }
    }
}

fn status_for_provider_error(error: RemoteWorkspaceProviderError) -> RemoteWorkspacePickerStatus {
    match error {
        RemoteWorkspaceProviderError::ConnectionLost => RemoteWorkspacePickerStatus::ConnectionLost,
        RemoteWorkspaceProviderError::Missing => RemoteWorkspacePickerStatus::Missing,
        RemoteWorkspaceProviderError::NotDirectory => RemoteWorkspacePickerStatus::NotDirectory,
        RemoteWorkspaceProviderError::PermissionDenied => {
            RemoteWorkspacePickerStatus::PermissionDenied
        }
        RemoteWorkspaceProviderError::InvalidResponse | RemoteWorkspaceProviderError::Other => {
            RemoteWorkspacePickerStatus::Other
        }
    }
}

fn gpui_color(color: Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use gpui::{Keystroke, Modifiers, Task, TestAppContext, VisualTestContext, div};

    use super::*;

    #[derive(Default)]
    struct ScriptedRemoteWorkspaceProviderState {
        accounts: VecDeque<Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>>>,
        listings:
            VecDeque<Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>>>,
        probes: VecDeque<Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>>>,
        creations: VecDeque<Task<Result<(), RemoteWorkspaceProviderError>>>,
        validations: VecDeque<Task<Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>>>,
        listed_directories: Vec<RemoteWorkspaceDirectory>,
        created_directories: Vec<RemoteWorkspaceDirectory>,
        validated_directories: Vec<RemoteWorkspaceDirectory>,
    }

    #[derive(Clone, Default)]
    struct ScriptedRemoteWorkspaceProvider {
        state: Arc<Mutex<ScriptedRemoteWorkspaceProviderState>>,
    }

    impl RemoteWorkspaceProvider for ScriptedRemoteWorkspaceProvider {
        fn discover_account(
            &self,
        ) -> Task<Result<RemoteWorkspaceAccount, RemoteWorkspaceProviderError>> {
            self.state
                .lock()
                .unwrap()
                .accounts
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }

        fn list_directories(
            &self,
            directory: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteWorkspaceDirectoryListing, RemoteWorkspaceProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.listed_directories.push(directory);
            state
                .listings
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }

        fn probe_exact_path(
            &self,
            _directory: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>> {
            self.state
                .lock()
                .unwrap()
                .probes
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }

        fn create_directory_recursively(
            &self,
            directory: RemoteWorkspaceDirectory,
        ) -> Task<Result<(), RemoteWorkspaceProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.created_directories.push(directory);
            state
                .creations
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }

        fn validate_physical_identity(
            &self,
            directory: RemoteWorkspaceDirectory,
        ) -> Task<Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>> {
            let mut state = self.state.lock().unwrap();
            state.validated_directories.push(directory);
            state
                .validations
                .pop_front()
                .unwrap_or_else(|| Task::ready(Err(RemoteWorkspaceProviderError::Other)))
        }
    }

    struct RemoteWorkspacePickerHarness {
        picker: gpui::Entity<RemoteWorkspacePicker>,
    }

    impl gpui::Render for RemoteWorkspacePickerHarness {
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
            Item = Result<Vec<RemoteWorkspaceDirectoryRow>, RemoteWorkspaceProviderError>,
        >,
        probes: impl IntoIterator<
            Item = Result<RemoteWorkspaceExactPathState, RemoteWorkspaceProviderError>,
        >,
        creations: impl IntoIterator<Item = Result<(), RemoteWorkspaceProviderError>>,
        validations: impl IntoIterator<
            Item = Result<RemoteDirectoryIdentity, RemoteWorkspaceProviderError>,
        >,
    ) -> Arc<ScriptedRemoteWorkspaceProvider> {
        Arc::new(ScriptedRemoteWorkspaceProvider {
            state: Arc::new(Mutex::new(ScriptedRemoteWorkspaceProviderState {
                accounts: [Task::ready(Ok(remote_account()))].into(),
                listings: listings
                    .into_iter()
                    .map(|result| result.map(RemoteWorkspaceDirectoryListing::new))
                    .map(Task::ready)
                    .collect(),
                probes: probes.into_iter().map(Task::ready).collect(),
                creations: creations.into_iter().map(Task::ready).collect(),
                validations: validations.into_iter().map(Task::ready).collect(),
                ..ScriptedRemoteWorkspaceProviderState::default()
            })),
        })
    }

    fn remote_workspace_picker(
        provider: Arc<ScriptedRemoteWorkspaceProvider>,
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<RemoteWorkspacePicker>,
        Rc<RefCell<Vec<RemoteWorkspacePickerEvent>>>,
        &mut VisualTestContext,
    ) {
        cx.update(crate::ui::init);
        let injected: Arc<dyn RemoteWorkspaceProvider + Send + Sync> = provider;
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = Rc::clone(&events);
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let picker = cx.new(|cx| RemoteWorkspacePicker::new(injected, window, cx));
            cx.subscribe(
                &picker,
                move |_, _, event: &RemoteWorkspacePickerEvent, _| {
                    recorded_events.borrow_mut().push(event.clone());
                },
            )
            .detach();
            RemoteWorkspacePickerHarness { picker }
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
                Ok(RemoteWorkspaceExactPathState::ReadableDirectory),
                Ok(RemoteWorkspaceExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(Arc::clone(&provider), cx);

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
            [Ok(RemoteWorkspaceExactPathState::Missing)],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| (
                picker.confirmation_label(),
                picker.can_confirm()
            )),
            ("Create Folder", true)
        );
        assert!(cx.debug_bounds("remote-workspace-picker-confirm").is_some());
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
                Ok(RemoteWorkspaceExactPathState::ReadableDirectory),
                Ok(RemoteWorkspaceExactPathState::Missing),
            ],
            [Ok(())],
            [Ok(identity.clone())],
        );
        let (picker, events, cx) = remote_workspace_picker(Arc::clone(&provider), cx);
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
            RemoteWorkspacePickerEvent::Confirmed(selection) => Some(selection.clone()),
            _ => None,
        });
        let selection = selection.expect("validated selection should be emitted");
        assert_eq!(selection.directory(), &expected);
        assert_eq!(selection.physical_directory(), &identity);
        assert_eq!(selection.account().user(), "tester");
        assert_eq!(selection.account().home_identity().as_str(), "/home/tester");
        assert_eq!(selection.account().login_shell(), "/bin/zsh");
    }

    #[gpui::test]
    fn stale_remote_refresh_cannot_replace_the_current_readable_state(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(provider, cx);
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
                    listing: Some(Err(RemoteWorkspaceProviderError::PermissionDenied)),
                    probe: Err(RemoteWorkspaceProviderError::ConnectionLost),
                },
                cx,
            );
        });

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.status),
            RemoteWorkspacePickerStatus::Readable
        );
    }

    #[gpui::test]
    fn remote_listing_errors_are_specific_and_never_keep_unread_rows(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Err(RemoteWorkspaceProviderError::PermissionDenied)],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(provider, cx);

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
                RemoteWorkspacePickerStatus::Readable,
                true,
                Some(RemoteWorkspaceProviderError::PermissionDenied),
                "Permission denied while listing this remote folder",
                Vec::<String>::new(),
            )
        );
    }

    #[gpui::test]
    fn changing_directory_clears_rows_and_stale_activation_is_inert(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(remote_rows(["Projects"])), Ok(remote_rows(["Current"]))],
            [
                Ok(RemoteWorkspaceExactPathState::ReadableDirectory),
                Ok(RemoteWorkspaceExactPathState::ReadableDirectory),
            ],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(provider, cx);
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
    fn oversized_remote_listing_is_bounded_and_exposes_exact_path_guidance(
        cx: &mut TestAppContext,
    ) {
        let rows = (0..MAXIMUM_REMOTE_WORKSPACE_DIRECTORY_ROWS + 7)
            .map(|index| RemoteWorkspaceDirectoryRow::new(format!("folder-{index:04}")).unwrap())
            .collect::<Vec<_>>();
        let provider = scripted_provider(
            [Ok(rows)],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, _, cx) = remote_workspace_picker(provider, cx);

        assert_eq!(
            picker.read_with(cx, |picker, _| picker.row_names().len()),
            MAXIMUM_REMOTE_WORKSPACE_DIRECTORY_ROWS
        );
        assert!(picker.read_with(cx, |picker, _| picker.listing_truncated));
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.empty_text()),
            "Only the first 1024 folders are shown; type an exact path to continue"
        );
    }

    #[gpui::test]
    fn cancelling_a_modal_pending_open_unblocks_input_and_emits_dismissed_once(
        cx: &mut TestAppContext,
    ) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_workspace_picker(provider, cx);
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
                .filter(|event| **event == RemoteWorkspacePickerEvent::Dismissed)
                .count(),
            1
        );
    }

    #[gpui::test]
    fn escape_steps_back_but_programmatic_dismissal_ends_the_flow(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_workspace_picker(provider, cx);
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
                .contains(&RemoteWorkspacePickerEvent::BackToHost)
        );
        assert!(
            !events
                .borrow()
                .contains(&RemoteWorkspacePickerEvent::Dismissed)
        );
    }

    #[gpui::test]
    fn non_back_dismissal_emits_dismissed(cx: &mut TestAppContext) {
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [],
        );
        let (picker, events, cx) = remote_workspace_picker(provider, cx);

        assert!(
            cx.update(|window, cx| { picker.update(cx, |picker, cx| picker.dismiss(window, cx)) })
        );
        cx.run_until_parked();

        assert!(
            events
                .borrow()
                .contains(&RemoteWorkspacePickerEvent::Dismissed)
        );
        assert!(
            !events
                .borrow()
                .contains(&RemoteWorkspacePickerEvent::BackToHost)
        );
    }

    #[gpui::test]
    fn validation_makes_the_path_read_only_until_the_parent_advances(cx: &mut TestAppContext) {
        let identity = RemoteDirectoryIdentity::new("/home/tester".to_owned()).unwrap();
        let provider = scripted_provider(
            [Ok(Vec::new())],
            [Ok(RemoteWorkspaceExactPathState::ReadableDirectory)],
            [],
            [Ok(identity)],
        );
        let (picker, events, cx) = remote_workspace_picker(provider, cx);

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.confirm_current(window, cx));
            for key in ["cmd-a", "x", "escape"] {
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
                .any(|event| { matches!(event, RemoteWorkspacePickerEvent::Confirmed(_)) })
        );
        assert_eq!(
            picker.read_with(cx, |picker, _| picker.busy),
            Some(RemoteWorkspacePickerBusy::AwaitingActivation)
        );
    }

    fn set_remote_input(
        picker: &gpui::Entity<RemoteWorkspacePicker>,
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

    fn remote_directory(value: &str) -> RemoteWorkspaceDirectory {
        RemoteWorkspaceDirectory::new(value.to_owned()).unwrap()
    }

    #[test]
    fn remote_path_parser_should_accept_root_without_rewriting_it() {
        let parsed = parse_remote_workspace_path("/").unwrap();

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
        let parsed = parse_remote_workspace_path("~/Projects/SpaceTerm").unwrap();

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
        let home_relative = parse_remote_workspace_path("~//Projects//SpaceTerm").unwrap();
        let absolute = parse_remote_workspace_path("//srv///projects//SpaceTerm").unwrap();

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
        let parsed = parse_remote_workspace_path("~/Projects//SpaceTerm/").unwrap();

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
            parse_remote_workspace_path("Projects"),
            Err(RemoteWorkspacePathFormatError::Relative)
        );
        assert_eq!(
            parse_remote_workspace_path("~"),
            Err(RemoteWorkspacePathFormatError::BareTilde)
        );
        assert_eq!(
            parse_remote_workspace_path("~other/Projects"),
            Err(RemoteWorkspacePathFormatError::UnsupportedTilde)
        );
    }

    #[test]
    fn hidden_directories_should_be_revealed_only_from_a_dot_leaf() {
        let ordinary = parse_remote_workspace_path("~/Projects/").unwrap();
        let dotted = parse_remote_workspace_path("~/Projects/.").unwrap();
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
        let parsed = parse_remote_workspace_path("~/Projects/sp").unwrap();
        let entries = remote_rows(["spaceTerm", "Spatial", "SpaceTerm", "tools"]);

        assert_eq!(
            row_names(filter_remote_workspace_rows(&parsed, &entries)),
            vec!["SpaceTerm", "spaceTerm", "Spatial"]
        );
    }

    #[test]
    fn directory_rows_should_reject_non_one_level_names() {
        assert!(RemoteWorkspaceDirectoryRow::new("nested/project".to_owned()).is_err());
        assert!(RemoteWorkspaceDirectoryRow::new("project\nname".to_owned()).is_err());
        assert!(RemoteWorkspaceDirectoryRow::new(String::new()).is_err());
    }

    #[test]
    fn activating_a_row_should_rewrite_the_query_to_descend() {
        let parsed = parse_remote_workspace_path("~//Projects//sp").unwrap();
        let row = RemoteWorkspaceDirectoryRow::new("SpaceTerm".to_owned()).unwrap();

        assert_eq!(
            descend_remote_workspace_query(&parsed, &row)
                .unwrap()
                .as_str(),
            "~//Projects//SpaceTerm/"
        );
    }

    #[test]
    fn exact_path_state_should_choose_open_or_create_without_rewriting_the_path() {
        let parsed = parse_remote_workspace_path("~/Projects//SpaceTerm").unwrap();

        assert_eq!(
            remote_workspace_confirmation(
                &parsed,
                RemoteWorkspaceExactPathState::ReadableDirectory
            ),
            RemoteWorkspaceConfirmation::OpenRemoteProject(
                RemoteWorkspaceDirectory::new("~/Projects//SpaceTerm".to_owned()).unwrap()
            )
        );
        assert_eq!(
            remote_workspace_confirmation(&parsed, RemoteWorkspaceExactPathState::Missing),
            RemoteWorkspaceConfirmation::CreateFolder(
                RemoteWorkspaceDirectory::new("~/Projects//SpaceTerm".to_owned()).unwrap()
            )
        );
    }

    fn remote_rows<const N: usize>(names: [&str; N]) -> Vec<RemoteWorkspaceDirectoryRow> {
        names
            .into_iter()
            .map(|name| RemoteWorkspaceDirectoryRow::new(name.to_owned()).unwrap())
            .collect()
    }

    fn row_names(rows: Vec<RemoteWorkspaceDirectoryRow>) -> Vec<String> {
        rows.into_iter().map(|row| row.name().to_owned()).collect()
    }
}
