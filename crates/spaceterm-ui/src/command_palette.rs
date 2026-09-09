use std::{cell::Cell, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, AppContext as _, BorrowAppContext as _, Corner, CursorStyle, Entity,
    EventEmitter, Global, HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding,
    ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, Rgba, ScrollWheelEvent, SharedString,
    Styled as _, Subscription, WeakEntity, WeakFocusHandle, Window, WindowId, actions, anchored,
    canvas, div, list, prelude::FluentBuilder as _, px,
};

use crate::{
    Icon, IconName, TextInput, TextInputEvent, TextInputTabBehavior, TextInputVariant,
    button::{Button, ButtonSize, ButtonVariant, IconButton},
    menu::{Menu, MenuActivation, MenuEntry, MenuSize},
    overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics},
};

const KEY_CONTEXT: &str = "SpaceTermCommandPalette";

/// Every footer control shares one size so their labels sit on one baseline and one inset.
const FOOTER_CONTROL_SIZE: ButtonSize = ButtonSize::Small;

actions!(
    spaceterm_command_palette,
    [
        MoveUp,
        MoveDown,
        MovePageUp,
        MovePageDown,
        Activate,
        Confirm,
        Dismiss,
        FocusNext,
        FocusPrevious
    ]
);

/// A platform-selected complete Command Palette keybinding set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteKeybindingProfile {
    /// The shipped macOS navigation, confirmation, dismissal, and focus bindings.
    MacOs,
}

/// Installs the platform-specific key equivalents for `profile`.
pub fn install_command_palette_keybindings(cx: &mut App, profile: CommandPaletteKeybindingProfile) {
    match profile {
        CommandPaletteKeybindingProfile::MacOs => cx.bind_keys([
            KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-p", MoveUp, Some(KEY_CONTEXT)),
            KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-n", MoveDown, Some(KEY_CONTEXT)),
            KeyBinding::new("pageup", MovePageUp, Some(KEY_CONTEXT)),
            KeyBinding::new("pagedown", MovePageDown, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-m", Activate, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-enter", Confirm, Some(KEY_CONTEXT)),
            KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
            KeyBinding::new("cmd-.", Dismiss, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-g", Dismiss, Some(KEY_CONTEXT)),
            KeyBinding::new("tab", FocusNext, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-tab", FocusPrevious, Some(KEY_CONTEXT)),
        ]),
    }
}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        cx.set_global(CommandPaletteCoordinator::default());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandPaletteRegistration(u64);

type SuspendPalette = Rc<dyn Fn(u64, &mut App) -> Option<WeakFocusHandle>>;
type ResumePalette = Rc<dyn Fn(u64, CommandPaletteRegistration, &mut App)>;
type ReplacePalette = Rc<dyn Fn(&mut App)>;
type ReplacePaletteNow = Rc<dyn Fn(&mut Window, &mut App) -> Option<WeakFocusHandle>>;

struct ErasedPaletteRegistration {
    token: CommandPaletteRegistration,
    suspend: SuspendPalette,
    resume: ResumePalette,
    replace: ReplacePalette,
    replace_now: ReplacePaletteNow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModalPaletteSuspension {
    generation: u64,
    registration: Option<CommandPaletteRegistration>,
}

#[derive(Default)]
struct CommandPaletteCoordinator {
    registrations: std::collections::HashMap<WindowId, ErasedPaletteRegistration>,
    modal_suspensions: std::collections::HashMap<WindowId, ModalPaletteSuspension>,
    next_registration: u64,
    next_suspension: u64,
}

impl Global for CommandPaletteCoordinator {}

fn command_palette_modal_generation(window_id: WindowId, cx: &App) -> Option<u64> {
    cx.has_global::<CommandPaletteCoordinator>()
        .then(|| {
            cx.global::<CommandPaletteCoordinator>()
                .modal_suspensions
                .get(&window_id)
                .map(|suspension| suspension.generation)
        })
        .flatten()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandPaletteSuspension {
    window_id: WindowId,
    generation: u64,
}

pub(crate) struct SuspendedCommandPalette {
    pub(crate) token: CommandPaletteSuspension,
    pub(crate) predecessor: Option<WeakFocusHandle>,
}

pub(crate) fn suspend_window_command_palette(
    window_id: WindowId,
    cx: &mut App,
) -> SuspendedCommandPalette {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        init(cx);
    }
    let (generation, suspend) =
        cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
            coordinator.next_suspension = coordinator.next_suspension.wrapping_add(1);
            let generation = coordinator.next_suspension;
            let registration = coordinator.registrations.get(&window_id);
            let suspension = ModalPaletteSuspension {
                generation,
                registration: registration.map(|registration| registration.token),
            };
            let suspend = registration.map(|registration| registration.suspend.clone());
            coordinator.modal_suspensions.insert(window_id, suspension);
            (generation, suspend)
        });
    let predecessor = suspend.and_then(|suspend| suspend(generation, cx));
    SuspendedCommandPalette {
        token: CommandPaletteSuspension {
            window_id,
            generation,
        },
        predecessor,
    }
}

pub(crate) fn resume_window_command_palette(
    suspension: CommandPaletteSuspension,
    cx: &mut App,
) -> bool {
    let Some((generation, registration, resume)) =
        modal_resume_operation(suspension.window_id, Some(suspension.generation), cx)
    else {
        return false;
    };
    resume(generation, registration, cx);
    true
}

pub(crate) fn retry_window_command_palette_modal_resume(window_id: WindowId, cx: &mut App) {
    let Some((generation, registration, resume)) = modal_resume_operation(window_id, None, cx)
    else {
        return;
    };
    resume(generation, registration, cx);
}

fn modal_resume_operation(
    window_id: WindowId,
    expected_generation: Option<u64>,
    cx: &mut App,
) -> Option<(u64, CommandPaletteRegistration, ResumePalette)> {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        return None;
    }
    cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
        let suspension = *coordinator.modal_suspensions.get(&window_id)?;
        if expected_generation.is_some_and(|expected| expected != suspension.generation) {
            return None;
        }
        let Some(registration) = suspension.registration else {
            coordinator.modal_suspensions.remove(&window_id);
            return None;
        };
        let resume = coordinator
            .registrations
            .get(&window_id)
            .filter(|candidate| candidate.token == registration)
            .map(|candidate| candidate.resume.clone());
        if resume.is_none() && coordinator.modal_suspensions.get(&window_id) == Some(&suspension) {
            coordinator.modal_suspensions.remove(&window_id);
        }
        resume.map(|resume| (suspension.generation, registration, resume))
    })
}

fn complete_window_command_palette_modal_resume(
    window_id: WindowId,
    generation: u64,
    registration: CommandPaletteRegistration,
    cx: &mut App,
) {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        return;
    }
    cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
        if coordinator
            .modal_suspensions
            .get(&window_id)
            .is_some_and(|suspension| {
                suspension.generation == generation && suspension.registration == Some(registration)
            })
        {
            coordinator.modal_suspensions.remove(&window_id);
        }
    });
}

pub(crate) fn discard_window_command_palette_suspension(
    suspension: CommandPaletteSuspension,
    cx: &mut App,
) {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        return;
    }
    cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
        if coordinator
            .modal_suspensions
            .get(&suspension.window_id)
            .is_some_and(|current| current.generation == suspension.generation)
        {
            coordinator.modal_suspensions.remove(&suspension.window_id);
        }
    });
}

fn register_open_palette<I: Clone + Eq + 'static>(
    owner: WeakEntity<CommandPalette<I>>,
    window: &Window,
    cx: &mut App,
) -> (CommandPaletteRegistration, Option<u64>) {
    let window_id = window.window_handle().window_id();
    let window_handle = window.window_handle();
    let suspend_window = window_handle;
    let suspend_owner = owner.clone();
    let suspend: SuspendPalette = Rc::new(move |generation, cx| {
        let predecessor = suspend_owner
            .update(cx, |palette, cx| palette.suspend_for_modal(generation, cx))
            .ok()
            .flatten();
        let _ = cx.update_window(suspend_window, |_, window, _| window.refresh());
        predecessor
    });
    let replace_owner = owner.clone();
    let replace_window = window_handle;
    let replace: ReplacePalette = Rc::new(move |cx| {
        let replace_owner = replace_owner.clone();
        cx.defer(move |cx| {
            let _ = cx.update_window(replace_window, |_, window, cx| {
                let _ = replace_owner.update(cx, |palette, cx| {
                    if !palette.cancel_pending_open(window, cx)
                        && palette.begin_close(CommandPaletteCloseReason::Replaced, window, cx)
                    {
                        palette.finish_close(CommandPaletteCloseReason::Replaced, cx);
                    }
                });
            });
        });
    });
    let replace_now_owner = owner.clone();
    let replace_now: ReplacePaletteNow = Rc::new(move |window, cx| {
        replace_now_owner
            .update(cx, |palette, cx| {
                palette
                    .dismiss_for_replacement(window, cx)
                    .and_then(|replacement| replacement.restore_focus)
            })
            .ok()
            .flatten()
    });
    let resume_window_id = window_id;
    let resume_retry_generation = Rc::new(Cell::new(None));
    let resume: ResumePalette = Rc::new(move |generation, registration, cx| {
        let may_resume = owner
            .read_with(cx, |palette, _| {
                (palette.open || palette.pending_open.is_some())
                    && palette.suspended_by_modal == Some(generation)
            })
            .unwrap_or(false);
        if !may_resume {
            return;
        }
        let owner = owner.clone();
        let retry_generation = resume_retry_generation.clone();
        cx.defer(move |cx| {
            let _ = cx.update_window(window_handle, |_, window, cx| {
                let resumed = owner
                    .update(cx, |palette, cx| {
                        palette.resume_from_modal(generation, window, cx)
                    })
                    .unwrap_or(false);
                if resumed {
                    retry_generation.set(None);
                    complete_window_command_palette_modal_resume(
                        resume_window_id,
                        generation,
                        registration,
                        cx,
                    );
                } else if retry_generation.get() != Some(generation) {
                    retry_generation.set(Some(generation));
                    window.on_next_frame(move |_, cx| {
                        retry_window_command_palette_modal_resume(resume_window_id, cx);
                    });
                }
            });
        });
    });
    let (token, modal_suspension, replaced) =
        cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
            coordinator.next_registration = coordinator.next_registration.wrapping_add(1);
            let token = CommandPaletteRegistration(coordinator.next_registration);
            let replaced = coordinator
                .registrations
                .insert(
                    window_id,
                    ErasedPaletteRegistration {
                        token,
                        suspend,
                        resume,
                        replace,
                        replace_now,
                    },
                )
                .map(|registration| registration.replace);
            let modal_suspension =
                coordinator
                    .modal_suspensions
                    .get_mut(&window_id)
                    .map(|suspension| {
                        suspension.registration = Some(token);
                        suspension.generation
                    });
            (token, modal_suspension, replaced)
        });
    if let Some(replaced) = replaced {
        replaced(cx);
    }
    (token, modal_suspension)
}

pub(crate) fn dismiss_active_command_palette_for_replacement(
    window: &mut Window,
    cx: &mut App,
) -> Option<WeakFocusHandle> {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        return None;
    }
    let replace = cx
        .global::<CommandPaletteCoordinator>()
        .registrations
        .get(&window.window_handle().window_id())
        .map(|registration| registration.replace_now.clone());
    replace.and_then(|replace| replace(window, cx))
}

fn unregister_palette(
    window_id: WindowId,
    token: CommandPaletteRegistration,
    suspended_generation: Option<u64>,
    cx: &mut App,
) {
    if !cx.has_global::<CommandPaletteCoordinator>() {
        return;
    }
    cx.update_global::<CommandPaletteCoordinator, _>(|coordinator, _| {
        if !coordinator
            .registrations
            .get(&window_id)
            .is_some_and(|registration| registration.token == token)
        {
            return;
        }
        coordinator.registrations.remove(&window_id);
        if let Some(suspension) = coordinator.modal_suspensions.get_mut(&window_id)
            && suspension.registration == Some(token)
            && suspended_generation == Some(suspension.generation)
        {
            suspension.registration = None;
        }
    });
}

/// The input path that activated a command-palette item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteActivationSource {
    /// Return activated the current item.
    Keyboard,
    /// A primary pointer press and release activated one row.
    Pointer,
}

/// A typed command-palette activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteActivation<I> {
    item_id: I,
    source: CommandPaletteActivationSource,
}

impl<I> CommandPaletteActivation<I> {
    /// Returns the caller-owned item identity.
    pub fn item_id(&self) -> &I {
        &self.item_id
    }

    /// Returns the input path that activated the item.
    pub fn source(&self) -> CommandPaletteActivationSource {
        self.source
    }

    /// Consumes the activation and returns its caller-owned item identity.
    pub fn into_item_id(self) -> I {
        self.item_id
    }
}

/// Why an open command palette closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteCloseReason {
    /// An enabled item was activated.
    Activated,
    /// Escape dismissed the palette.
    Escape,
    /// A pointer press outside the panel dismissed the palette.
    Outside,
    /// Focus moved away from the palette editor.
    FocusLost,
    /// The operating-system window deactivated.
    Deactivated,
    /// The owner explicitly dismissed the palette.
    Programmatic,
    /// The owner replaced this palette with another transient UI owner.
    Replaced,
    /// The owner completed its operation and takes focus itself.
    Completed,
}

impl CommandPaletteCloseReason {
    const fn restores_focus(self) -> bool {
        matches!(
            self,
            Self::Activated | Self::Escape | Self::Outside | Self::Programmatic
        )
    }

    const fn is_implicit_dismissal(self) -> bool {
        matches!(
            self,
            Self::Escape | Self::Outside | Self::FocusLost | Self::Deactivated
        )
    }
}

/// One exact command-palette lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPaletteLifecycleEvent {
    /// The palette became open.
    Opened,
    /// The palette became closed for the supplied reason.
    Closed(CommandPaletteCloseReason),
}

/// The original focus owner transferred through a command-palette replacement chain.
pub struct CommandPaletteReplacementFocus {
    restore_focus: Option<WeakFocusHandle>,
}

struct PendingCommandPaletteOpen {
    replacement: Option<CommandPaletteReplacementFocus>,
}

/// Monotonic identity for the current command-palette query.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct CommandPaletteGeneration(u64);

impl CommandPaletteGeneration {
    /// Returns the opaque generation as a diagnostic integer.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// A query snapshot that callers may use to feed asynchronous results back to the palette.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteQuery {
    text: String,
    generation: CommandPaletteGeneration,
}

impl CommandPaletteQuery {
    /// Returns the complete single-line query.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the generation that results must match.
    pub fn generation(&self) -> CommandPaletteGeneration {
        self.generation
    }
}

/// Typed events emitted by a [`CommandPalette`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPaletteEvent<I> {
    /// The palette opened or closed.
    Lifecycle(CommandPaletteLifecycleEvent),
    /// An enabled semantic item was activated.
    Activated(CommandPaletteActivation<I>),
    /// The query changed or a refresh was explicitly requested.
    QueryChanged(CommandPaletteQuery),
    /// A search-line control was activated.
    HeaderAction(SharedString),
    /// A footer actions-menu entry was activated.
    MenuAction(SharedString),
    /// The footer confirm control was activated by pointer or by its keyboard equivalent.
    Confirmed,
}

/// A standardized trailing row accessory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPaletteAccessory {
    /// Secondary explanatory text.
    Text(SharedString),
    /// A display-only keyboard shortcut.
    Shortcut(SharedString),
    /// A compact status label.
    Status(SharedString),
    /// A selected or completed checkmark.
    Checkmark,
}

type IconBuilder = Rc<dyn Fn(Rgba) -> AnyElement>;

/// One control rendered at the trailing edge of the command-palette search line.
///
/// The caller owns the icon and the identity it receives back through
/// [`CommandPaletteEvent::HeaderAction`]; the palette owns the control's size and paint.
#[derive(Clone)]
pub struct CommandPaletteAction {
    id: SharedString,
    accessibility_name: SharedString,
    icon: IconBuilder,
    disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPaletteAction {
    /// Creates an enabled search-line control with a mandatory logical accessibility name.
    pub fn new(
        id: impl Into<SharedString>,
        accessibility_name: impl Into<SharedString>,
        icon: impl Fn(Rgba) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            icon: Rc::new(icon),
            disabled: false,
            debug_selector: None,
        }
    }

    /// Controls whether the control can activate.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned identity reported on activation.
    pub fn id(&self) -> &SharedString {
        &self.id
    }
}

/// One footer hint pairing a key presentation with the action it performs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteHint {
    label: SharedString,
    key: SharedString,
}

impl CommandPaletteHint {
    /// Creates a hint such as `Open` paired with `\u{23ce}`.
    pub fn new(label: impl Into<SharedString>, key: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            key: key.into(),
        }
    }
}

/// Who decides which items a query presents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandPaletteMatching {
    /// The palette filters and ranks items with its own static semantic matcher.
    #[default]
    Semantic,
    /// The caller supplies exactly the items to present, already filtered and ordered.
    ///
    /// The palette presents every item in caller order and highlights nothing, because the query
    /// is not a substring of the labels it produces. Callers whose query is an address rather than
    /// a search term, such as a filesystem path, select this.
    Caller,
}

/// What activating an item does to the palette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandPaletteActivationPolicy {
    /// Activation closes the palette, because the item completed the caller's operation.
    #[default]
    Close,
    /// Activation keeps the palette open, because the item narrows the caller's next results.
    ///
    /// Drill-down callers, such as a filesystem navigator whose rows descend a level, select this
    /// and answer the activation by publishing a new query and new items.
    Continue,
}

/// The single primary footer control and its keyboard equivalent.
///
/// It is primary by placement and by owning the confirm key, not by weight: the palette renders it
/// as low-emphasis text so the result list stays the loudest thing on the surface.
///
/// The caller owns the label, displayed shortcut, enabled state, and operation identity; the
/// palette owns the control's size, paint, placement, and shortcut rendering. Activating it, by
/// pointer or by the palette's confirm key, emits [`CommandPaletteEvent::Confirmed`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteConfirm {
    label: SharedString,
    shortcut: SharedString,
    disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPaletteConfirm {
    /// Creates an enabled confirm control with caller-selected label and shortcut presentation.
    pub fn new(label: impl Into<SharedString>, shortcut: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            shortcut: shortcut.into(),
            disabled: false,
            debug_selector: None,
        }
    }

    /// Controls whether the control can activate. A disabled confirm ignores its keyboard
    /// equivalent as well as pointer activation.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns whether the control is disabled.
    pub const fn is_disabled(&self) -> bool {
        self.disabled
    }
}

/// One typed semantic command-palette item.
///
/// Item identities must remain stable. Later items with a duplicate identity are discarded so
/// selection and pointer ownership always refer to exactly one row.
#[derive(Clone)]
pub struct CommandPaletteItem<I> {
    id: I,
    label: SharedString,
    description: Option<SharedString>,
    section: Option<SharedString>,
    keywords: Vec<SharedString>,
    disabled: bool,
    leading_icon: Option<IconBuilder>,
    trailing: Option<CommandPaletteAccessory>,
    debug_selector: Option<String>,
}

/// A query-aware provider for the command palette's pinned fallback row.
///
/// The provider receives the exact editor text. Its typed item is appended after ordinary matches
/// without participating in filtering or scoring.
#[derive(Clone)]
pub struct CommandPaletteFallback<I>(CommandPaletteFallbackProvider<I>);

type CommandPaletteFallbackProvider<I> = Rc<dyn Fn(&str) -> CommandPaletteItem<I>>;

impl<I> CommandPaletteFallback<I> {
    /// Creates a provider whose row is rebuilt whenever the accepted query changes.
    pub fn new(provider: impl Fn(&str) -> CommandPaletteItem<I> + 'static) -> Self {
        Self(Rc::new(provider))
    }

    fn item(&self, query: &str) -> CommandPaletteItem<I> {
        (self.0)(query)
    }
}

impl<I> CommandPaletteItem<I> {
    /// Creates an enabled item. The label is also its logical accessibility name.
    pub fn new(id: I, label: impl Into<SharedString>) -> Self {
        Self {
            id,
            label: label.into(),
            description: None,
            section: None,
            keywords: Vec::new(),
            disabled: false,
            leading_icon: None,
            trailing: None,
            debug_selector: None,
        }
    }

    /// Adds one line of secondary descriptive text.
    pub fn description(mut self, value: impl Into<SharedString>) -> Self {
        self.description = Some(value.into());
        self
    }

    /// Groups the item under a heading shown when the section changes between adjacent rows.
    ///
    /// Items are grouped in provider order, so a caller that wants one heading per section must
    /// supply that section's items contiguously.
    pub fn section(mut self, value: impl Into<SharedString>) -> Self {
        self.section = Some(value.into());
        self
    }

    /// Replaces the non-presentational search keywords.
    pub fn keywords(mut self, values: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        self.keywords = values.into_iter().map(Into::into).collect();
        self
    }

    /// Controls whether navigation and activation may reach this item.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a bounded leading icon built with the resolved row foreground color.
    pub fn leading_icon(mut self, build: impl Fn(Rgba) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    /// Adds one standardized trailing accessory.
    pub fn trailing(mut self, accessory: CommandPaletteAccessory) -> Self {
        self.trailing = Some(accessory);
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Returns the caller-owned identity.
    pub fn id(&self) -> &I {
        &self.id
    }

    /// Returns the primary label.
    pub fn label(&self) -> &str {
        self.label.as_ref()
    }

    /// Returns the optional description.
    pub fn description_text(&self) -> Option<&str> {
        self.description.as_ref().map(AsRef::as_ref)
    }

    /// Returns the optional grouping section.
    pub fn section_text(&self) -> Option<&str> {
        self.section.as_ref().map(AsRef::as_ref)
    }

    /// Returns whether navigation and activation skip this item.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandPaletteMatch {
    item_index: usize,
    score: i64,
    label_highlights: Vec<Range<usize>>,
    description_highlights: Vec<Range<usize>>,
}

fn match_command_palette_items<I>(
    items: &[CommandPaletteItem<I>],
    query: &str,
    matching: CommandPaletteMatching,
) -> Vec<CommandPaletteMatch> {
    let tokens: Vec<Vec<char>> = match matching {
        CommandPaletteMatching::Caller => Vec::new(),
        CommandPaletteMatching::Semantic => query
            .split_whitespace()
            .map(lowercase_chars)
            .filter(|token| !token.is_empty())
            .collect(),
    };
    if tokens.is_empty() {
        return items
            .iter()
            .enumerate()
            .map(|(item_index, _)| CommandPaletteMatch {
                item_index,
                score: 0,
                label_highlights: Vec::new(),
                description_highlights: Vec::new(),
            })
            .collect();
    }

    let mut section_groups = vec![0usize; items.len()];
    for item_index in 1..items.len() {
        section_groups[item_index] = section_groups[item_index - 1]
            + usize::from(items[item_index].section != items[item_index - 1].section);
    }
    let mut matches = items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            match_item(item, &tokens).map(|(score, label_highlights, description_highlights)| {
                CommandPaletteMatch {
                    item_index,
                    score,
                    label_highlights,
                    description_highlights,
                }
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        section_groups[left.item_index]
            .cmp(&section_groups[right.item_index])
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.item_index.cmp(&right.item_index))
    });
    matches
}

fn lowercase_chars(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Clone)]
struct SearchUnit {
    character: char,
    source: Range<usize>,
}

fn search_units(text: &str) -> Vec<SearchUnit> {
    let mut units = Vec::new();
    for (start, character) in text.char_indices() {
        let source = start..start + character.len_utf8();
        units.extend(character.to_lowercase().map(|character| SearchUnit {
            character,
            source: source.clone(),
        }));
    }
    units
}

type ItemMatch = (i64, Vec<Range<usize>>, Vec<Range<usize>>);

fn match_item<I>(item: &CommandPaletteItem<I>, tokens: &[Vec<char>]) -> Option<ItemMatch> {
    let label_units = search_units(item.label.as_ref());
    let description_units = item.description.as_ref().map(|text| search_units(text));
    let keyword_units: Vec<_> = item
        .keywords
        .iter()
        .map(|keyword| search_units(keyword))
        .collect();
    let mut score = 0;
    let mut label_highlights = Vec::new();
    let mut description_highlights = Vec::new();

    for token in tokens {
        let mut best =
            fuzzy_match(&label_units, token).map(|matched| (matched.score + 20_000, 0, matched));
        if let Some(units) = &description_units
            && let Some(matched) = fuzzy_match(units, token)
        {
            let candidate = (matched.score + 400, 1, matched);
            if best.as_ref().is_none_or(|current| candidate.0 > current.0) {
                best = Some(candidate);
            }
        }
        for units in &keyword_units {
            if let Some(matched) = fuzzy_match(units, token) {
                let candidate = (matched.score + 200, 2, matched);
                if best.as_ref().is_none_or(|current| candidate.0 > current.0) {
                    best = Some(candidate);
                }
            }
        }
        let (token_score, field, matched) = best?;
        score += token_score;
        match field {
            0 => label_highlights.extend(matched.ranges),
            1 => description_highlights.extend(matched.ranges),
            _ => {}
        }
    }

    Some((
        score,
        merge_ranges(label_highlights),
        merge_ranges(description_highlights),
    ))
}

struct FuzzyMatch {
    score: i64,
    ranges: Vec<Range<usize>>,
}

fn fuzzy_match(target: &[SearchUnit], query: &[char]) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            ranges: Vec::new(),
        });
    }
    let mut best: Option<(i64, Vec<usize>)> = None;
    for start in target
        .iter()
        .enumerate()
        .filter_map(|(index, unit)| (unit.character == query[0]).then_some(index))
    {
        let mut indexes = vec![start];
        let mut cursor = start + 1;
        let mut complete = true;
        for query_character in &query[1..] {
            let Some(relative) = target[cursor..]
                .iter()
                .position(|unit| unit.character == *query_character)
            else {
                complete = false;
                break;
            };
            cursor += relative;
            indexes.push(cursor);
            cursor += 1;
        }
        if !complete {
            continue;
        }
        let end = indexes.last().copied().unwrap_or(start);
        let gaps = end + 1 - start - indexes.len();
        let contiguous_pairs = indexes
            .windows(2)
            .filter(|pair| pair[1] == pair[0] + 1)
            .count();
        let whole = indexes.len() == target.len() && start == 0;
        let prefix = start == 0;
        let rank = 1_000
            + i64::from(whole) * 8_000
            + i64::from(prefix) * 3_000
            + contiguous_pairs as i64 * 80
            - gaps as i64 * 25
            - start as i64 * 4;
        if best.as_ref().is_none_or(|current| rank > current.0) {
            best = Some((rank, indexes));
        }
    }
    let (score, indexes) = best?;
    let ranges = indexes
        .into_iter()
        .filter_map(|index| target.get(index).map(|unit| unit.source.clone()))
        .collect();
    Some(FuzzyMatch {
        score,
        ranges: merge_ranges(ranges),
    })
}

fn merge_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}

/// Application-owned command-palette paint values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPalettePaint {
    background: Rgba,
    border: Rgba,
    separator: Rgba,
    foreground: Rgba,
    muted: Rgba,
    disabled: Rgba,
    hover_background: Rgba,
    selected_background: Rgba,
    selected_foreground: Rgba,
    match_foreground: Rgba,
    icon_foreground: Rgba,
    disabled_icon_foreground: Rgba,
    section_foreground: Rgba,
    footer_foreground: Rgba,
    footer_key_foreground: Rgba,
}

impl CommandPalettePaint {
    /// Creates the core paint catalog.
    ///
    /// Separator, hover, section, and footer colors default to the closest core value so a caller
    /// only overrides what its theme distinguishes.
    #[expect(
        clippy::too_many_arguments,
        reason = "the bounded paint catalog is clearer than nested untyped color groups"
    )]
    pub fn new(
        background: Rgba,
        border: Rgba,
        foreground: Rgba,
        muted: Rgba,
        disabled: Rgba,
        selected_background: Rgba,
        selected_foreground: Rgba,
        match_foreground: Rgba,
    ) -> Self {
        Self {
            background,
            border,
            separator: border,
            foreground,
            muted,
            disabled,
            hover_background: selected_background,
            selected_background,
            selected_foreground,
            match_foreground,
            icon_foreground: foreground,
            disabled_icon_foreground: disabled,
            section_foreground: muted,
            footer_foreground: muted,
            footer_key_foreground: disabled,
        }
    }

    /// Sets normal and disabled icon colors independently of result text.
    pub fn icons(mut self, normal: Rgba, disabled: Rgba) -> Self {
        self.icon_foreground = normal;
        self.disabled_icon_foreground = disabled;
        self
    }

    /// Sets the hairline color used under the editor, above the footer, and between sections.
    pub fn separator(mut self, color: Rgba) -> Self {
        self.separator = color;
        self
    }

    /// Sets the pointer-hover row background, which stays distinct from the selected background.
    pub fn hover_background(mut self, color: Rgba) -> Self {
        self.hover_background = color;
        self
    }

    /// Sets the section heading foreground.
    pub fn section_foreground(mut self, color: Rgba) -> Self {
        self.section_foreground = color;
        self
    }

    /// Sets the footer hint label and key foregrounds.
    pub fn footer(mut self, foreground: Rgba, key_foreground: Rgba) -> Self {
        self.footer_foreground = foreground;
        self.footer_key_foreground = key_foreground;
        self
    }
}

/// Native desktop dimensions for the command-palette panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPaletteMetrics {
    panel_width: Pixels,
    maximum_height: Pixels,
    top_offset: Pixels,
    viewport_margin: Pixels,
    panel_padding: Pixels,
    input_height: Pixels,
    row_height: Pixels,
    single_line_row_height: Pixels,
    row_line_gap: Pixels,
    section_height: Pixels,
    separator_height: Pixels,
    footer_height: Pixels,
    footer_padding: Option<Pixels>,
    horizontal_padding: Pixels,
    leading_width: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    input_size: Pixels,
    label_size: Pixels,
    secondary_size: Pixels,
    accessory_padding: Pixels,
    accessory_line_padding: Pixels,
    accessory_radius: Pixels,
}

impl CommandPaletteMetrics {
    /// Creates compact native defaults around a panel width and row height.
    pub fn new(panel_width: Pixels, row_height: Pixels) -> Self {
        Self {
            panel_width,
            maximum_height: px(480.0),
            top_offset: px(52.0),
            viewport_margin: px(16.0),
            panel_padding: px(4.0),
            input_height: px(42.0),
            row_height,
            single_line_row_height: row_height,
            row_line_gap: px(2.0),
            section_height: px(22.0),
            separator_height: px(9.0),
            footer_height: px(30.0),
            footer_padding: None,
            horizontal_padding: px(12.0),
            leading_width: px(18.0),
            gap: px(10.0),
            corner_radius: px(8.0),
            border_width: px(1.0),
            input_size: px(14.0),
            label_size: px(13.0),
            secondary_size: px(11.0),
            accessory_padding: px(5.0),
            accessory_line_padding: px(2.0),
            accessory_radius: px(4.0),
        }
    }

    /// Sets the footer's horizontal padding, overriding the panel's content inset.
    ///
    /// Footer controls are text with their own padding, so their boxes sitting on the content
    /// edges puts their labels inside those edges. A caller that wants the labels to line up with
    /// the editor and the rows sets a smaller footer padding. Defaults to the content inset, which
    /// aligns the control boxes instead.
    pub fn footer_padding(mut self, padding: Pixels) -> Self {
        self.footer_padding = Some(padding);
        self
    }

    /// Sets the height of a row that carries no description.
    ///
    /// A row height sized for a label above a description leaves a single-line row mostly empty,
    /// so callers whose items are all one line give that row its own height. Defaults to the
    /// ordinary row height, which keeps mixed result sets uniform.
    pub fn single_line_row_height(mut self, height: Pixels) -> Self {
        self.single_line_row_height = height;
        self
    }

    /// Sets the maximum panel height and top offset.
    pub fn panel_geometry(mut self, maximum_height: Pixels, top_offset: Pixels) -> Self {
        self.maximum_height = maximum_height;
        self.top_offset = top_offset;
        self
    }

    /// Sets the minimum panel distance from viewport edges.
    pub fn viewport_margin(mut self, margin: Pixels) -> Self {
        self.viewport_margin = margin;
        self
    }

    /// Sets panel padding and the editor height.
    pub fn panel_spacing(mut self, padding: Pixels, input_height: Pixels) -> Self {
        self.panel_padding = padding;
        self.input_height = input_height;
        self
    }

    /// Sets row padding, leading-slot width, and column gap.
    pub fn row_spacing(
        mut self,
        horizontal_padding: Pixels,
        leading_width: Pixels,
        gap: Pixels,
    ) -> Self {
        self.horizontal_padding = horizontal_padding;
        self.leading_width = leading_width;
        self.gap = gap;
        self
    }

    /// Sets the gap between a row's label line and its description line.
    pub fn row_line_gap(mut self, gap: Pixels) -> Self {
        self.row_line_gap = gap;
        self
    }

    /// Sets section heading and section separator heights.
    pub fn section_spacing(mut self, section_height: Pixels, separator_height: Pixels) -> Self {
        self.section_height = section_height;
        self.separator_height = separator_height;
        self
    }

    /// Sets the hint and actions footer height.
    pub fn footer_height(mut self, height: Pixels) -> Self {
        self.footer_height = height;
        self
    }

    /// Sets panel corner radius and stable border width.
    pub fn panel_shape(mut self, corner_radius: Pixels, border_width: Pixels) -> Self {
        self.corner_radius = corner_radius;
        self.border_width = border_width;
        self
    }

    /// Sets editor, primary, and secondary font sizes.
    pub fn font_sizes(mut self, input: Pixels, label: Pixels, secondary: Pixels) -> Self {
        self.input_size = input;
        self.label_size = label;
        self.secondary_size = secondary;
        self
    }

    /// Sets the padded status accessory shape.
    pub fn accessory_shape(
        mut self,
        padding: Pixels,
        line_padding: Pixels,
        radius: Pixels,
    ) -> Self {
        self.accessory_padding = padding;
        self.accessory_line_padding = line_padding;
        self.accessory_radius = radius;
        self
    }

    /// Returns the shared left edge of the editor, headings, status text, and row content.
    fn content_leading_inset(&self) -> Pixels {
        self.panel_padding + self.horizontal_padding
    }

    fn footer_inset(&self) -> Pixels {
        self.footer_padding
            .unwrap_or_else(|| self.content_leading_inset())
    }

    /// Returns the concentric radius for an inset row inside the outer panel.
    fn row_corner_radius(&self) -> Pixels {
        (self.corner_radius - self.panel_padding).max(px(0.0))
    }
}

/// Application-owned presentation installed once for every command palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPaletteTheme {
    paint: CommandPalettePaint,
    metrics: CommandPaletteMetrics,
}

impl CommandPaletteTheme {
    /// Creates a complete command-palette theme.
    pub fn new(paint: CommandPalettePaint, metrics: CommandPaletteMetrics) -> Self {
        Self { paint, metrics }
    }
}

impl Global for CommandPaletteTheme {}

/// A reusable entity-backed command palette with typed semantic items.
///
/// Its [`TextInput`] supplies native editable-text semantics. GPUI 0.2.2 cannot yet publish
/// listbox and option roles for ordinary elements, so the API requires logical row labels and
/// keeps arbitrary row painting outside the accessibility seam.
pub struct CommandPalette<I: Clone + Eq + 'static> {
    no_results_text: SharedString,
    items: Rc<[CommandPaletteItem<I>]>,
    presented_items: Rc<[CommandPaletteItem<I>]>,
    fallback: Option<CommandPaletteFallback<I>>,
    fallback_item_id: Option<I>,
    ordinary_match_count: usize,
    matches: Rc<[CommandPaletteMatch]>,
    presented_results: Rc<PresentedResults>,
    leading_reserved: bool,
    header_actions: Vec<CommandPaletteAction>,
    hints: Vec<CommandPaletteHint>,
    actions_menu: Vec<MenuEntry<SharedString>>,
    actions_menu_label: SharedString,
    confirm: Option<CommandPaletteConfirm>,
    matching: CommandPaletteMatching,
    activation: CommandPaletteActivationPolicy,
    selected: Option<I>,
    preferred: Option<I>,
    query: String,
    generation: CommandPaletteGeneration,
    loading: bool,
    dismissible: bool,
    escape_cancellable: bool,
    open: bool,
    pending_open: Option<PendingCommandPaletteOpen>,
    suspended_by_modal: Option<u64>,
    coordinator_registration: Option<CommandPaletteRegistration>,
    input: Entity<TextInput>,
    focus_scope: gpui::FocusHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    restore_focus: Option<WeakFocusHandle>,
    restore_on_activation: Option<WeakFocusHandle>,
    pointer_press: Option<I>,
    pointer_suppressed: bool,
    hover_suppressed: bool,
    pointer_anchor: gpui::Point<Pixels>,
    list: ListState,
    scrollbar_reveal_pending: bool,
    selection_reveal_pending: bool,
    _input_subscription: Subscription,
    _focus_subscription: Subscription,
    _scrollbar_subscription: Subscription,
}

mod presented_results {
    use gpui::{Pixels, SharedString, px};

    use super::{CommandPaletteItem, CommandPaletteMatch, CommandPaletteMetrics};

    /// One presented list row. Section headings and separators are derived, never caller-painted.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(super) enum PaletteRow {
        Section(SharedString),
        Separator,
        Item { position: usize, single_line: bool },
    }

    impl PaletteRow {
        pub(super) fn height(&self, metrics: CommandPaletteMetrics) -> Pixels {
            match self {
                Self::Section(_) => metrics.section_height,
                Self::Separator => metrics.separator_height,
                Self::Item {
                    single_line: true, ..
                } => metrics.single_line_row_height,
                Self::Item { .. } => metrics.row_height,
            }
        }

        pub(super) const fn item_position(&self) -> Option<usize> {
            match self {
                Self::Item { position, .. } => Some(*position),
                Self::Section(_) | Self::Separator => None,
            }
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(super) struct PresentedResults {
        rows: Vec<PaletteRow>,
    }

    impl PresentedResults {
        pub(super) fn new<I>(
            items: &[CommandPaletteItem<I>],
            matches: &[CommandPaletteMatch],
        ) -> Self {
            let mut rows = Vec::with_capacity(matches.len());
            let mut current: Option<SharedString> = None;
            let mut started = false;
            for (position, matched) in matches.iter().enumerate() {
                let Some(item) = items.get(matched.item_index) else {
                    continue;
                };
                if !started || item.section != current {
                    if started {
                        rows.push(PaletteRow::Separator);
                    }
                    if let Some(section) = item.section.clone() {
                        rows.push(PaletteRow::Section(section));
                    }
                    current = item.section.clone();
                }
                started = true;
                rows.push(PaletteRow::Item {
                    position,
                    single_line: item.description.is_none(),
                });
            }
            Self { rows }
        }

        pub(super) fn len(&self) -> usize {
            self.rows.len()
        }

        #[cfg(test)]
        pub(super) fn rows(&self) -> &[PaletteRow] {
            &self.rows
        }

        pub(super) fn row(&self, index: usize) -> Option<&PaletteRow> {
            self.rows.get(index)
        }

        pub(super) fn total_height(&self, metrics: CommandPaletteMetrics) -> Pixels {
            self.rows
                .iter()
                .fold(px(0.0), |height, row| height + row.height(metrics))
        }

        pub(super) fn list_index_for_match(&self, position: usize) -> Option<usize> {
            self.rows
                .iter()
                .position(|row| row.item_position() == Some(position))
        }

        #[cfg(test)]
        pub(super) fn row_at_y(
            &self,
            content_y: Pixels,
            metrics: CommandPaletteMetrics,
        ) -> Option<(usize, &PaletteRow)> {
            if content_y < px(0.0) {
                return None;
            }
            let mut row_top = px(0.0);
            self.rows.iter().enumerate().find(|(_, row)| {
                let row_bottom = row_top + row.height(metrics);
                let contains = content_y >= row_top && content_y < row_bottom;
                row_top = row_bottom;
                contains
            })
        }

        #[cfg(test)]
        pub(super) fn item_at_y(
            &self,
            content_y: Pixels,
            metrics: CommandPaletteMetrics,
        ) -> Option<usize> {
            self.row_at_y(content_y, metrics)?.1.item_position()
        }

        pub(super) fn page_target(
            &self,
            current: Option<usize>,
            enabled: &[usize],
            viewport_height: Pixels,
            direction: isize,
            metrics: CommandPaletteMetrics,
        ) -> Option<usize> {
            let edge = if direction < 0 {
                *enabled.last()?
            } else {
                *enabled.first()?
            };
            let current = current
                .filter(|position| enabled.contains(position))
                .unwrap_or(edge);
            let current_enabled_index = enabled.iter().position(|position| *position == current)?;
            let current_top = self.item_top(current, metrics)?;
            let target_y = if direction < 0 {
                (current_top - viewport_height).max(px(0.0))
            } else {
                current_top + viewport_height.max(px(0.0))
            };

            if direction < 0 {
                let candidates = &enabled[..current_enabled_index];
                candidates
                    .iter()
                    .copied()
                    .find(|position| {
                        self.item_top(*position, metrics)
                            .is_some_and(|top| top >= target_y)
                    })
                    .or_else(|| candidates.last().copied())
                    .or(Some(current))
            } else {
                let candidates = &enabled[current_enabled_index + 1..];
                candidates
                    .iter()
                    .copied()
                    .take_while(|position| {
                        self.item_top(*position, metrics)
                            .is_some_and(|top| top <= target_y)
                    })
                    .last()
                    .or_else(|| candidates.first().copied())
                    .or(Some(current))
            }
        }

        fn item_top(&self, position: usize, metrics: CommandPaletteMetrics) -> Option<Pixels> {
            let mut top = px(0.0);
            for row in &self.rows {
                if row.item_position() == Some(position) {
                    return Some(top);
                }
                top += row.height(metrics);
            }
            None
        }
    }
}

use presented_results::{PaletteRow, PresentedResults};

impl<I: Clone + Eq + 'static> EventEmitter<CommandPaletteEvent<I>> for CommandPalette<I> {}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    /// Creates a closed palette with static items and a reusable [`TextInput`] editor.
    pub fn new(
        placeholder: impl Into<SharedString>,
        items: Vec<CommandPaletteItem<I>>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let placeholder = placeholder.into();
        let input_placeholder = placeholder.clone();
        let input = cx.new(|cx| {
            TextInput::new(
                "command-palette-input",
                "Command palette query",
                "",
                window,
                cx,
            )
            .placeholder(input_placeholder)
            .variant(TextInputVariant::Bare)
            .tab_behavior(TextInputTabBehavior::Propagate)
            .debug_selector("command-palette-input")
        });
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |palette, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::ValueChanged(_) => {
                    palette.update_query(input.read(cx).value().to_owned(), cx);
                }
                TextInputEvent::Submitted => {
                    palette.activate_selected(CommandPaletteActivationSource::Keyboard, window, cx)
                }
                TextInputEvent::Cancelled => {
                    palette.close(CommandPaletteCloseReason::Escape, window, cx);
                }
                TextInputEvent::TabForwardRequested => {
                    palette.focus_next_control(window, cx);
                }
                TextInputEvent::TabBackwardRequested => {
                    palette.focus_previous_control(window, cx);
                }
                _ => {}
            },
        );
        let focus_scope = cx.focus_handle();
        let focus_subscription = cx.on_focus_out(&focus_scope, window, |palette, _, window, cx| {
            if palette.open
                && palette.suspended_by_modal.is_none()
                && !crate::menu::window_menu_is_open(window, cx)
            {
                palette.close(CommandPaletteCloseReason::FocusLost, window, cx);
            }
        });
        let scrollbar =
            cx.new(|_| OverlayScrollbar::<f32>::new("command-palette-scrollbar").persistent());
        let scrollbar_subscription = cx.subscribe_in(
            &scrollbar,
            window,
            |palette, _, event: &OverlayScrollbarEvent<f32>, _, cx| match event {
                OverlayScrollbarEvent::InteractionStarted => palette.list.scrollbar_drag_started(),
                OverlayScrollbarEvent::OffsetRequested(offset) => {
                    palette
                        .list
                        .set_offset_from_scrollbar(gpui::point(px(0.0), px(-*offset)));
                    cx.notify();
                }
            },
        );
        cx.observe_window_activation(window, |palette, window, cx| {
            if (palette.open || palette.pending_open.is_some())
                && let Some(generation) = palette.suspended_by_modal
                && window.is_window_active()
                && !crate::modal::window_modal_is_open(window, cx)
            {
                if palette.resume_from_modal(generation, window, cx)
                    && let Some(registration) = palette.coordinator_registration
                {
                    complete_window_command_palette_modal_resume(
                        window.window_handle().window_id(),
                        generation,
                        registration,
                        cx,
                    );
                }
            } else if palette.open
                && palette.suspended_by_modal.is_none()
                && !window.is_window_active()
            {
                palette.close(CommandPaletteCloseReason::Deactivated, window, cx);
            } else if !palette.open
                && window.is_window_active()
                && let Some(focus) = palette
                    .restore_on_activation
                    .take()
                    .and_then(|focus| focus.upgrade())
            {
                focus.focus(window);
            }
        })
        .detach();
        let window_id = window.window_handle().window_id();
        cx.on_release(move |palette, cx| {
            if let Some(registration) = palette.coordinator_registration.take() {
                unregister_palette(window_id, registration, palette.suspended_by_modal, cx);
            }
            crate::tooltip::set_window_tooltip_suppression(
                window_id,
                crate::tooltip::TooltipSuppression::CommandPalette,
                false,
                cx,
            );
        })
        .detach();
        let items: Rc<[CommandPaletteItem<I>]> = unique_items(items).into();
        let matches: Rc<[CommandPaletteMatch]> =
            match_command_palette_items(&items, "", CommandPaletteMatching::Semantic).into();
        let selected = first_enabled_id(&items, &matches);
        let presented_results = Rc::new(PresentedResults::new(&items, &matches));
        let leading_reserved = items.iter().any(|item| item.leading_icon.is_some());
        let list =
            ListState::new(presented_results.len(), ListAlignment::Top, px(0.0)).measure_all();
        let mut palette = Self {
            no_results_text: "No matching items".into(),
            presented_items: Rc::clone(&items),
            items,
            fallback: None,
            fallback_item_id: None,
            ordinary_match_count: matches.len(),
            matches,
            presented_results,
            leading_reserved,
            header_actions: Vec::new(),
            hints: Vec::new(),
            actions_menu: Vec::new(),
            actions_menu_label: "Actions".into(),
            confirm: None,
            matching: CommandPaletteMatching::Semantic,
            activation: CommandPaletteActivationPolicy::Close,
            selected,
            preferred: None,
            query: String::new(),
            generation: CommandPaletteGeneration::default(),
            loading: false,
            dismissible: true,
            escape_cancellable: false,
            open: false,
            pending_open: None,
            suspended_by_modal: None,
            coordinator_registration: None,
            input,
            focus_scope,
            scrollbar,
            restore_focus: None,
            restore_on_activation: None,
            pointer_press: None,
            pointer_suppressed: false,
            hover_suppressed: false,
            pointer_anchor: gpui::point(px(0.0), px(0.0)),
            list,
            scrollbar_reveal_pending: false,
            selection_reveal_pending: false,
            _input_subscription: input_subscription,
            _focus_subscription: focus_subscription,
            _scrollbar_subscription: scrollbar_subscription,
        };
        palette.install_scroll_handler(cx);
        palette
    }

    fn install_scroll_handler(&mut self, cx: &mut gpui::Context<Self>) {
        let palette = cx.entity().downgrade();
        // GPUI dispatches this handler while it holds the list state's mutable borrow, so the
        // handler must not read that state. It only records the request; the next render reads
        // the scroll geometry and reveals the scrollbar.
        self.list.set_scroll_handler(move |_, _, cx| {
            let _ = palette.update(cx, |palette, cx| {
                palette.scrollbar_reveal_pending = true;
                cx.notify();
            });
        });
    }

    /// Sets the identity preferred when no stable enabled selection remains.
    pub fn set_preferred_item(&mut self, id: Option<I>, cx: &mut gpui::Context<Self>) {
        self.preferred = id;
        self.repair_selection();
        cx.notify();
    }

    /// Replaces the no-results message.
    pub fn set_no_results_text(
        &mut self,
        text: impl Into<SharedString>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.no_results_text = text.into();
        cx.notify();
    }

    /// Pins one query-aware item after all ordinary matches.
    ///
    /// The fallback remains present for every query. Ordinary matches retain initial-selection
    /// precedence; with no ordinary match, an enabled fallback becomes selected.
    pub fn set_fallback(
        &mut self,
        fallback: Option<CommandPaletteFallback<I>>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.fallback = fallback;
        self.recompute_matches();
        cx.notify();
    }

    /// Replaces the controls rendered at the trailing edge of the search line.
    ///
    /// Activating one emits [`CommandPaletteEvent::HeaderAction`] with the caller's identity.
    pub fn set_header_actions(
        &mut self,
        actions: Vec<CommandPaletteAction>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.header_actions = actions;
        cx.notify();
    }

    /// Selects who filters and orders items for the current query.
    ///
    /// Switching modes recomputes the presented results immediately and repairs selection by
    /// stable identity.
    pub fn set_matching(&mut self, matching: CommandPaletteMatching, cx: &mut gpui::Context<Self>) {
        if self.matching == matching {
            return;
        }
        self.matching = matching;
        self.recompute_matches();
        cx.notify();
    }

    /// Selects whether activating an item closes the palette or keeps it open.
    pub fn set_activation(
        &mut self,
        activation: CommandPaletteActivationPolicy,
        cx: &mut gpui::Context<Self>,
    ) {
        self.activation = activation;
        cx.notify();
    }

    /// Replaces the single primary footer control. `None` removes it.
    ///
    /// An installed confirm claims the palette's confirm key; without one that key is left for the
    /// surrounding application.
    pub fn set_confirm(
        &mut self,
        confirm: Option<CommandPaletteConfirm>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.confirm = confirm;
        cx.notify();
    }

    /// Replaces the footer hints. An empty list removes the footer unless an actions menu remains.
    pub fn set_hints(&mut self, hints: Vec<CommandPaletteHint>, cx: &mut gpui::Context<Self>) {
        self.hints = hints;
        cx.notify();
    }

    /// Replaces the footer actions menu. An empty list removes its trigger.
    ///
    /// Activating an entry emits [`CommandPaletteEvent::MenuAction`]. The palette stays open while
    /// the menu holds focus and closes only once the caller acts on the entry.
    pub fn set_actions_menu(
        &mut self,
        entries: Vec<MenuEntry<SharedString>>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.actions_menu = entries;
        cx.notify();
    }

    /// Replaces the footer actions-menu trigger label, which is also its accessibility name.
    pub fn set_actions_menu_label(
        &mut self,
        label: impl Into<SharedString>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.actions_menu_label = label.into();
        cx.notify();
    }

    /// Opens the palette, captures the exact prior focus owner, and focuses its editor.
    ///
    /// Returns `true` only for an actual closed-to-open transition.
    pub fn open(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> bool {
        self.open_with_replacement(None, window, cx)
    }

    /// Opens the palette as the next owner in a replacement chain.
    ///
    /// The transferred focus owner is restored when this palette later closes normally.
    pub fn open_replacing(
        &mut self,
        replacement: CommandPaletteReplacementFocus,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.open_with_replacement(Some(replacement), window, cx)
    }

    fn open_with_replacement(
        &mut self,
        replacement: Option<CommandPaletteReplacementFocus>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.open {
            if self.suspended_by_modal.is_none() && !crate::modal::window_modal_is_open(window, cx)
            {
                self.input.read(cx).focus_handle().focus(window);
            }
            return false;
        }
        if self.pending_open.is_some() {
            return false;
        }
        if command_palette_modal_generation(window.window_handle().window_id(), cx).is_some()
            || crate::modal::window_modal_is_open(window, cx)
        {
            let (registration, modal_suspension) =
                register_open_palette(cx.entity().downgrade(), window, cx);
            if let Some(generation) = modal_suspension {
                self.coordinator_registration = Some(registration);
                self.suspended_by_modal = Some(generation);
                self.pending_open = Some(PendingCommandPaletteOpen { replacement });
            } else {
                unregister_palette(window.window_handle().window_id(), registration, None, cx);
            }
            return false;
        }
        self.finish_open(replacement, window, cx)
    }

    fn finish_open(
        &mut self,
        replacement: Option<CommandPaletteReplacementFocus>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.restore_on_activation = None;
        let menu_replacement = crate::menu::dismiss_active_menu_for_replacement(window, cx);
        let combo_replacement =
            crate::combo_box::dismiss_active_combo_box_for_replacement(window, cx);
        let combo_focus = combo_replacement
            .as_ref()
            .and_then(|replacement| replacement.restore_focus.clone());
        if let Some(combo_replacement) = combo_replacement {
            combo_replacement.finish(cx);
        }
        self.restore_focus = match replacement {
            Some(replacement) => replacement.restore_focus,
            None => match menu_replacement {
                Some(crate::menu::MenuReplacementFocus(focus)) => focus,
                None => combo_focus.or_else(|| window.focused(cx).map(|focus| focus.downgrade())),
            },
        };
        self.open = true;
        if self.coordinator_registration.is_none() {
            let (registration, modal_suspension) =
                register_open_palette(cx.entity().downgrade(), window, cx);
            self.coordinator_registration = Some(registration);
            self.suspended_by_modal = modal_suspension;
        }
        crate::tooltip::set_window_tooltip_suppression(
            window.window_handle().window_id(),
            crate::tooltip::TooltipSuppression::CommandPalette,
            true,
            cx,
        );
        self.pointer_press = None;
        self.pointer_suppressed = true;
        self.hover_suppressed = true;
        self.pointer_anchor = window.mouse_position();
        self.selected = None;
        if !self.query.is_empty() {
            self.input.update(cx, |input, cx| input.set_value("", cx));
            self.query.clear();
            self.recompute_matches();
        }
        self.repair_selection();
        self.reveal_selected();
        self.selection_reveal_pending = true;
        if self.suspended_by_modal.is_none() && !crate::modal::window_modal_is_open(window, cx) {
            self.input.read(cx).focus_handle().focus(window);
        }
        cx.emit(CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Opened,
        ));
        self.request_refresh(cx);
        cx.notify();
        true
    }

    /// Dismisses an open palette programmatically and restores its exact prior focus owner.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> bool {
        self.cancel_pending_open(window, cx)
            || self.close(CommandPaletteCloseReason::Programmatic, window, cx)
    }

    /// Closes an open palette whose owner has completed its operation and moved focus itself.
    ///
    /// Unlike [`Self::dismiss`] this discards the captured prior focus owner, so the palette does
    /// not pull focus back from whatever the completed operation focused.
    pub fn dismiss_without_restoring_focus(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        self.restore_focus = None;
        self.restore_on_activation = None;
        self.cancel_pending_open(window, cx)
            || self.close(CommandPaletteCloseReason::Completed, window, cx)
    }

    /// Returns focus to the query editor of an open palette.
    pub fn focus_editor(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.open
            && self.suspended_by_modal.is_none()
            && !crate::modal::window_modal_is_open(window, cx)
        {
            self.input.read(cx).focus_handle().focus(window);
        }
    }

    /// Returns whether the query editor currently holds keyboard focus.
    pub fn editor_is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.input.read(cx).focus_handle().is_focused(window)
    }

    /// Returns whether [`Self::set_query`] would preserve `query` byte-for-byte.
    ///
    /// Callers whose query is an address rather than a search term use this to reject values the
    /// single-line editor would normalize.
    pub fn can_set_query_exactly(&self, query: &str, cx: &gpui::App) -> bool {
        self.input.read(cx).can_set_value_exactly(query)
    }

    /// Closes an open palette before another transient takes focus.
    pub fn dismiss_for_replacement(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<CommandPaletteReplacementFocus> {
        let replacement = CommandPaletteReplacementFocus {
            restore_focus: self.restore_focus.clone(),
        };
        self.close(CommandPaletteCloseReason::Replaced, window, cx)
            .then_some(replacement)
    }

    /// Returns whether the transient overlay is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Returns the current editor query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the current query generation.
    pub fn generation(&self) -> CommandPaletteGeneration {
        self.generation
    }

    /// Returns the selected enabled item identity, if any.
    pub fn selected_item_id(&self) -> Option<&I> {
        self.selected.as_ref()
    }

    /// Replaces items immediately and preserves selection by stable identity when possible.
    pub fn set_items(&mut self, items: Vec<CommandPaletteItem<I>>, cx: &mut gpui::Context<Self>) {
        self.items = unique_items(items).into();
        self.loading = false;
        self.recompute_matches();
        cx.notify();
    }

    /// Sets the current loading presentation without changing items or generation.
    pub fn set_loading(&mut self, loading: bool, cx: &mut gpui::Context<Self>) {
        if self.loading != loading {
            self.loading = loading;
            cx.notify();
        }
    }

    /// Allows explicit Escape cancellation while outside clicks and focus loss remain blocked.
    pub fn set_escape_cancellable(&mut self, enabled: bool, cx: &mut gpui::Context<Self>) {
        self.escape_cancellable = enabled;
        cx.notify();
    }

    /// Controls whether user, focus, or window transitions may dismiss the open palette.
    /// Explicit owner dismissal, replacement, completion, and item activation remain available.
    pub fn set_dismissible(&mut self, dismissible: bool, cx: &mut gpui::Context<Self>) {
        if self.dismissible != dismissible {
            self.dismissible = dismissible;
            cx.notify();
        }
    }

    /// Controls user editing without preventing owner-driven query updates.
    pub fn set_query_editable(&mut self, editable: bool, cx: &mut gpui::Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_editable(editable, cx));
    }

    /// Requests a refresh for the current query and returns its new generation.
    pub fn refresh(&mut self, cx: &mut gpui::Context<Self>) -> CommandPaletteGeneration {
        self.loading = true;
        let generation = self.request_refresh(cx);
        cx.notify();
        generation
    }

    /// Applies items only when `generation` still describes the current query.
    ///
    /// Returns `false` without changing state for a stale asynchronous result.
    pub fn apply_items(
        &mut self,
        generation: CommandPaletteGeneration,
        items: Vec<CommandPaletteItem<I>>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.set_items(items, cx);
        true
    }

    /// Sets loading only when `generation` still describes the current query.
    pub fn set_loading_for_generation(
        &mut self,
        generation: CommandPaletteGeneration,
        loading: bool,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.set_loading(loading, cx);
        true
    }

    /// Replaces the editor query and emits one generation-bearing query event.
    pub fn set_query(&mut self, query: impl Into<String>, cx: &mut gpui::Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(query.into(), cx));
        let accepted_query = self.input.read(cx).value().to_owned();
        self.update_query(accepted_query, cx);
    }

    fn update_query(&mut self, query: String, cx: &mut gpui::Context<Self>) {
        if self.query == query {
            return;
        }
        self.query = query;
        self.loading = false;
        self.recompute_matches();
        self.request_refresh(cx);
        cx.notify();
    }

    fn request_refresh(&mut self, cx: &mut gpui::Context<Self>) -> CommandPaletteGeneration {
        self.generation.0 = self.generation.0.wrapping_add(1);
        let generation = self.generation;
        cx.emit(CommandPaletteEvent::QueryChanged(CommandPaletteQuery {
            text: self.query.clone(),
            generation,
        }));
        generation
    }

    fn recompute_matches(&mut self) {
        self.pointer_press = None;
        let selected_was_fallback = self
            .fallback_item_id
            .as_ref()
            .is_some_and(|id| self.selected.as_ref() == Some(id));
        let mut presented = self.items.to_vec();
        let mut matches = match_command_palette_items(&self.items, &self.query, self.matching);
        self.ordinary_match_count = matches.len();
        if let Some(item) = self
            .fallback
            .as_ref()
            .map(|fallback| fallback.item(&self.query))
            && !presented.iter().any(|existing| existing.id == item.id)
        {
            let item_index = presented.len();
            self.fallback_item_id = Some(item.id.clone());
            presented.push(item);
            matches.push(CommandPaletteMatch {
                item_index,
                score: i64::MIN,
                label_highlights: Vec::new(),
                description_highlights: Vec::new(),
            });
        } else {
            self.fallback_item_id = None;
        }
        self.presented_items = presented.into();
        self.matches = matches.into();
        self.leading_reserved = self
            .presented_items
            .iter()
            .any(|item| item.leading_icon.is_some());
        if selected_was_fallback && self.ordinary_match_count > 0 {
            self.selected = None;
        }
        self.presented_results =
            Rc::new(PresentedResults::new(&self.presented_items, &self.matches));
        self.list.reset(self.presented_results.len());
        self.repair_selection();
        self.reveal_selected();
        self.selection_reveal_pending = true;
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height = f32::from(self.list.viewport_bounds().size.height);
        let maximum_offset = f32::from(self.list.max_offset_for_scrollbar().height);
        let offset = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        ScrollMetrics::for_pixels(0.0, track_height, maximum_offset, offset)
    }

    fn sync_scrollbar(&self, cx: &mut gpui::Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.sync(metrics, cx));
    }

    fn reveal_scrollbar(&self, cx: &mut gpui::Context<Self>) {
        let metrics = self.scrollbar_metrics();
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
    }

    fn repair_selection(&mut self) {
        let stable = self.selected.as_ref().is_some_and(|selected| {
            self.matches.iter().any(|matched| {
                self.presented_items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *selected)
            })
        });
        if stable {
            return;
        }
        self.selected = self.preferred.as_ref().and_then(|preferred| {
            self.matches.iter().find_map(|matched| {
                self.presented_items
                    .get(matched.item_index)
                    .and_then(|item| {
                        (!item.disabled && item.id == *preferred).then(|| item.id.clone())
                    })
            })
        });
        if self.selected.is_none() {
            self.selected = first_enabled_id(&self.presented_items, &self.matches);
        }
    }

    fn move_selection(&mut self, delta: isize, window: &Window, cx: &mut gpui::Context<Self>) {
        self.suppress_pointer(window.mouse_position(), cx);
        let enabled = self.enabled_match_positions();
        if enabled.is_empty() {
            return;
        }
        let current = self.selected_match_position();
        let next = if delta >= 0 {
            current
                .and_then(|current| enabled.iter().position(|position| *position == current))
                .map_or(0, |position| (position + 1) % enabled.len())
        } else {
            current
                .and_then(|current| enabled.iter().position(|position| *position == current))
                .map_or(enabled.len() - 1, |position| {
                    position.checked_sub(1).unwrap_or(enabled.len() - 1)
                })
        };
        self.select_match_position(enabled[next], cx);
    }

    fn move_page(&mut self, direction: isize, window: &Window, cx: &mut gpui::Context<Self>) {
        self.suppress_pointer(window.mouse_position(), cx);
        let enabled = self.enabled_match_positions();
        if enabled.is_empty() {
            return;
        }
        let metrics = cx.global::<CommandPaletteTheme>().metrics;
        let next = self.presented_results.page_target(
            self.selected_match_position(),
            &enabled,
            self.list.viewport_bounds().size.height,
            direction,
            metrics,
        );
        if let Some(next) = next {
            self.select_match_position(next, cx);
        }
    }

    fn reveal_selected(&mut self) {
        let Some(position) = self.selected_match_position() else {
            return;
        };
        let Some(row) = self.presented_results.list_index_for_match(position) else {
            return;
        };
        let item_is_visible = self.list.bounds_for_item(row).is_some_and(|item_bounds| {
            let viewport = self.list.viewport_bounds();
            item_bounds.top() >= viewport.top() && item_bounds.bottom() <= viewport.bottom()
        });
        if !item_is_visible {
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.0),
            });
        }
    }

    fn enabled_match_positions(&self) -> Vec<usize> {
        self.matches
            .iter()
            .enumerate()
            .filter_map(|(position, matched)| {
                self.presented_items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled)
                    .then_some(position)
            })
            .collect()
    }

    fn selected_match_position(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.matches.iter().position(|matched| {
            self.presented_items
                .get(matched.item_index)
                .is_some_and(|item| item.id == *selected)
        })
    }

    fn select_match_position(&mut self, position: usize, cx: &mut gpui::Context<Self>) {
        let next = self
            .matches
            .get(position)
            .and_then(|matched| self.presented_items.get(matched.item_index))
            .filter(|item| !item.disabled)
            .map(|item| item.id.clone());
        if next.is_some() && self.selected != next {
            self.selected = next;
            if let Some(row) = self.presented_results.list_index_for_match(position) {
                self.list.scroll_to_reveal_item(row);
            }
            cx.notify();
        }
    }

    fn suppress_pointer(&mut self, position: gpui::Point<Pixels>, cx: &mut gpui::Context<Self>) {
        self.pointer_anchor = position;
        if !self.pointer_suppressed || !self.hover_suppressed {
            self.pointer_suppressed = true;
            self.hover_suppressed = true;
            cx.notify();
        }
    }

    fn suppress_hover_for_scroll(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.pointer_anchor = position;
        if self.pointer_suppressed || !self.hover_suppressed {
            self.pointer_suppressed = false;
            self.hover_suppressed = true;
            cx.notify();
        }
    }

    fn resume_pointer_interaction(&mut self, cx: &mut gpui::Context<Self>) {
        if self.pointer_suppressed || self.hover_suppressed {
            self.pointer_suppressed = false;
            self.hover_suppressed = false;
            cx.notify();
        }
    }

    fn pointer_moved(
        &mut self,
        position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if (self.pointer_suppressed || self.hover_suppressed) && position == self.pointer_anchor {
            return false;
        }
        self.resume_pointer_interaction(cx);
        true
    }

    fn pointer_hover(
        &mut self,
        id: &I,
        pointer_position: gpui::Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self.pointer_moved(pointer_position, cx) {
            return;
        }
        let position = self.matches.iter().position(|matched| {
            self.presented_items
                .get(matched.item_index)
                .is_some_and(|item| !item.disabled && item.id == *id)
        });
        if let Some(position) = position {
            self.select_match_position(position, cx);
        }
    }

    fn pointer_down(&mut self, id: I, cx: &mut gpui::Context<Self>) {
        self.resume_pointer_interaction(cx);
        self.pointer_press = Some(id);
    }

    fn pointer_up(&mut self, id: &I, inside: bool) -> bool {
        if self.pointer_press.as_ref() != Some(id) {
            return false;
        }
        self.pointer_press = None;
        inside
            && self.matches.iter().any(|matched| {
                self.presented_items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *id)
            })
    }

    fn focus_next_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_next();
        if !self.focus_scope.contains_focused(window, cx) {
            self.input.read(cx).focus_handle().focus(window);
        }
        cx.stop_propagation();
    }

    fn focus_previous_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_prev();
        if self.focus_scope.contains_focused(window, cx) {
            cx.stop_propagation();
            return;
        }

        let input_focus = self.input.read(cx).focus_handle();
        input_focus.focus(window);
        let mut last_internal = input_focus;
        let maximum_steps = self.header_actions.len()
            + usize::from(!self.actions_menu.is_empty())
            + usize::from(self.confirm.is_some());
        for _ in 0..maximum_steps {
            window.focus_next();
            if !self.focus_scope.contains_focused(window, cx) {
                last_internal.focus(window);
                break;
            }
            if let Some(focused) = window.focused(cx) {
                last_internal = focused;
            }
        }
        cx.stop_propagation();
    }

    fn activate_selected(
        &mut self,
        source: CommandPaletteActivationSource,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.loading {
            return;
        }
        let Some(item_id) = self.selected.clone() else {
            return;
        };
        let enabled = self.matches.iter().any(|matched| {
            self.presented_items
                .get(matched.item_index)
                .is_some_and(|item| item.id == item_id && !item.disabled)
        });
        if !enabled {
            return;
        }
        if self.activation == CommandPaletteActivationPolicy::Continue {
            cx.emit(CommandPaletteEvent::Activated(CommandPaletteActivation {
                item_id,
                source,
            }));
            return;
        }
        if !self.begin_close(CommandPaletteCloseReason::Activated, window, cx) {
            return;
        }
        cx.emit(CommandPaletteEvent::Activated(CommandPaletteActivation {
            item_id,
            source,
        }));
        self.finish_close(CommandPaletteCloseReason::Activated, cx);
    }

    fn suspend_for_modal(
        &mut self,
        generation: u64,
        cx: &mut gpui::Context<Self>,
    ) -> Option<WeakFocusHandle> {
        if !self.open {
            return None;
        }
        self.suspended_by_modal = Some(generation);
        self.pointer_press = None;
        cx.notify();
        self.restore_focus.clone()
    }

    fn resume_from_modal(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let pending_replacement = self
            .pending_open
            .as_ref()
            .is_some_and(|pending| pending.replacement.is_some());
        let editor_retained_focus = self.input.read(cx).focus_handle().is_focused(window);
        let predecessor_restored = self
            .restore_focus
            .as_ref()
            .and_then(WeakFocusHandle::upgrade)
            .is_some_and(|focus| focus.contains_focused(window, cx));
        if self.suspended_by_modal != Some(generation)
            || crate::modal::window_modal_is_open(window, cx)
            || (!editor_retained_focus
                && !predecessor_restored
                && !pending_replacement
                && !crate::modal::focus_allows_transient_resume(window, cx))
        {
            return false;
        }
        self.suspended_by_modal = None;
        if let Some(pending) = self.pending_open.take() {
            return self.finish_open(pending.replacement, window, cx);
        }
        if !self.open {
            return false;
        }
        self.pointer_press = None;
        self.pointer_suppressed = true;
        self.hover_suppressed = true;
        self.pointer_anchor = window.mouse_position();
        self.input.read(cx).focus_handle().focus(window);
        cx.notify();
        true
    }

    fn cancel_pending_open(&mut self, window: &Window, cx: &mut gpui::Context<Self>) -> bool {
        if self.pending_open.take().is_none() {
            return false;
        }
        let suspended_generation = self.suspended_by_modal.take();
        if let Some(registration) = self.coordinator_registration.take() {
            unregister_palette(
                window.window_handle().window_id(),
                registration,
                suspended_generation,
                cx,
            );
        }
        true
    }

    fn close(
        &mut self,
        reason: CommandPaletteCloseReason,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.begin_close(reason, window, cx) {
            return false;
        }
        self.finish_close(reason, cx);
        true
    }

    fn begin_close(
        &mut self,
        reason: CommandPaletteCloseReason,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.open
            || (!self.dismissible
                && reason.is_implicit_dismissal()
                && !(self.escape_cancellable && reason == CommandPaletteCloseReason::Escape))
        {
            return false;
        }
        self.open = false;
        let suspended_generation = self.suspended_by_modal.take();
        if let Some(registration) = self.coordinator_registration.take() {
            unregister_palette(
                window.window_handle().window_id(),
                registration,
                suspended_generation,
                cx,
            );
        }
        crate::tooltip::set_window_tooltip_suppression(
            window.window_handle().window_id(),
            crate::tooltip::TooltipSuppression::CommandPalette,
            false,
            cx,
        );
        self.loading = false;
        self.generation.0 = self.generation.0.wrapping_add(1);
        self.pointer_press = None;
        self.pointer_suppressed = false;
        self.hover_suppressed = false;
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        let restore_focus = self.restore_focus.take();
        if reason == CommandPaletteCloseReason::Deactivated {
            self.restore_on_activation = restore_focus;
        } else {
            self.restore_on_activation = None;
            if reason.restores_focus()
                && let Some(focus) = restore_focus.and_then(|focus| focus.upgrade())
            {
                focus.focus(window);
            }
        }
        true
    }

    fn finish_close(&self, reason: CommandPaletteCloseReason, cx: &mut gpui::Context<Self>) {
        cx.emit(CommandPaletteEvent::Lifecycle(
            CommandPaletteLifecycleEvent::Closed(reason),
        ));
        cx.notify();
    }
}

fn unique_items<I: Clone + Eq>(items: Vec<CommandPaletteItem<I>>) -> Vec<CommandPaletteItem<I>> {
    let mut unique = Vec::with_capacity(items.len());
    for item in items {
        if !unique
            .iter()
            .any(|existing: &CommandPaletteItem<I>| existing.id == item.id)
        {
            unique.push(item);
        }
    }
    unique
}

fn first_enabled_id<I: Clone>(
    items: &[CommandPaletteItem<I>],
    matches: &[CommandPaletteMatch],
) -> Option<I> {
    matches.iter().find_map(|matched| {
        items
            .get(matched.item_index)
            .filter(|item| !item.disabled)
            .map(|item| item.id.clone())
    })
}

impl<I: Clone + Eq + 'static> Render for CommandPalette<I> {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        if !self.open
            || self.suspended_by_modal.is_some()
            || crate::modal::window_modal_is_open(window, cx)
        {
            return div().into_any_element();
        }
        let theme = *cx.global::<CommandPaletteTheme>();
        let metrics = theme.metrics;
        if std::mem::take(&mut self.scrollbar_reveal_pending) {
            self.reveal_scrollbar(cx);
        } else {
            self.sync_scrollbar(cx);
        }
        if std::mem::take(&mut self.selection_reveal_pending) {
            let palette = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = palette.update(cx, |palette, cx| {
                    palette.reveal_selected();
                    cx.notify();
                });
            });
        }
        let viewport = window.viewport_size();
        let available_width = (viewport.width - metrics.viewport_margin * 2.0).max(px(0.0));
        let panel_width = metrics.panel_width.min(available_width);
        let left = ((viewport.width - panel_width) / 2.0).max(px(0.0));
        let top = metrics
            .top_offset
            .min((viewport.height - metrics.viewport_margin).max(px(0.0)));

        let footer = self.has_footer();
        let content_height = if self.loading || self.matches.is_empty() {
            metrics.row_height
        } else {
            self.presented_results.total_height(metrics)
        };
        let chrome_height = chrome_height(metrics, footer);
        let available_height = (viewport.height - top - metrics.viewport_margin).max(px(0.0));
        let panel_height = (chrome_height + content_height)
            .min(metrics.maximum_height)
            .min(available_height);
        let list_height = (panel_height - chrome_height).max(px(0.0));
        let mut fitted = px(0.0);
        if !self.loading && !self.matches.is_empty() {
            for index in 0..self.presented_results.len() {
                let Some(row) = self.presented_results.row(index) else {
                    break;
                };
                let height = row.height(metrics);
                if fitted + height > list_height {
                    break;
                }
                fitted += height;
            }
        }
        let list_height = if fitted > px(0.0) {
            fitted
        } else {
            list_height
        };
        let panel_height = chrome_height + list_height;

        let panel_bounds = gpui::Bounds::new(
            gpui::point(left, top),
            gpui::size(panel_width, panel_height),
        );
        let outside = self.render_outside_tracker(panel_bounds, cx);
        let panel = self.render_panel(panel_width, panel_height, list_height, theme, cx);
        let overlay = div()
            .relative()
            .w(viewport.width)
            .h(viewport.height)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_scope)
            .tab_group()
            .child(outside)
            .child(div().absolute().left(left).top(top).child(panel))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .when(self.pointer_suppressed, |overlay| {
                overlay.child(
                    canvas(
                        |_, _, _| (),
                        |_, _, window, _| window.set_window_cursor_style(CursorStyle::None),
                    )
                    .absolute()
                    .inset_0(),
                )
            })
            .on_action(cx.listener(|palette, _: &MoveUp, window, cx| {
                palette.move_selection(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MoveDown, window, cx| {
                palette.move_selection(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MovePageUp, window, cx| {
                palette.move_page(-1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &MovePageDown, window, cx| {
                palette.move_page(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &Activate, window, cx| {
                palette.activate_selected(CommandPaletteActivationSource::Keyboard, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|palette, _: &Dismiss, window, cx| {
                palette.close(CommandPaletteCloseReason::Escape, window, cx);
                cx.stop_propagation();
            }))
            .when(self.confirm.is_some(), |overlay| {
                overlay.on_action(cx.listener(|palette, _: &Confirm, _, cx| {
                    palette.confirm_activated(cx);
                    cx.stop_propagation();
                }))
            })
            .on_action(cx.listener(|palette, _: &FocusNext, window, cx| {
                palette.focus_next_control(window, cx);
            }))
            .on_action(cx.listener(|palette, _: &FocusPrevious, window, cx| {
                palette.focus_previous_control(window, cx);
            }));

        // The palette is not itself deferred: GPUI collects deferred draws once per frame, so a
        // deferred palette could not host its own deferred footer menu. Its owner renders it last,
        // and the anchored full-window layer keeps it above the surrounding chrome.
        anchored()
            .anchor(Corner::TopLeft)
            .position(gpui::point(px(0.0), px(0.0)))
            .snap_to_window()
            .child(overlay)
            .into_any_element()
    }
}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    fn has_footer(&self) -> bool {
        !self.hints.is_empty() || !self.actions_menu.is_empty() || self.confirm.is_some()
    }

    fn confirm_activated(&mut self, cx: &mut gpui::Context<Self>) {
        if self.confirm.as_ref().is_none_or(|confirm| confirm.disabled) {
            return;
        }
        cx.emit(CommandPaletteEvent::<I>::Confirmed);
    }

    fn render_outside_tracker(
        &self,
        panel_bounds: gpui::Bounds<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let palette = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let move_palette = palette.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = move_palette
                            .update(cx, |palette, cx| palette.pointer_moved(event.position, cx));
                    }
                });
                let scroll_palette = palette.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = scroll_palette.update(cx, |palette, cx| {
                            palette.suppress_hover_for_scroll(event.position, cx);
                        });
                    }
                });
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture() {
                        return;
                    }
                    let _ = palette.update(cx, |palette, cx| {
                        palette.resume_pointer_interaction(cx);
                    });
                    if panel_bounds.contains(&event.position) {
                        return;
                    }
                    // A menu opened from this palette paints outside the panel and owns its own
                    // dismissal, so an outside press while it is open is not a palette dismissal.
                    if crate::menu::window_menu_is_open(window, cx) {
                        return;
                    }
                    window.prevent_default();
                    let _ = palette.update(cx, |palette, cx| {
                        palette.close(CommandPaletteCloseReason::Outside, window, cx)
                    });
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    fn render_panel(
        &self,
        width: Pixels,
        height: Pixels,
        list_height: Pixels,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let paint = theme.paint;
        let metrics = theme.metrics;
        let content = if self.loading {
            status_row("Loading\u{2026}", "command-palette-loading", metrics, paint)
                .into_any_element()
        } else if self.matches.is_empty() {
            status_row(
                self.no_results_text.clone(),
                "command-palette-no-results",
                metrics,
                paint,
            )
            .into_any_element()
        } else {
            self.render_results(list_height, theme, cx)
        };

        div()
            .debug_selector(|| "command-palette-panel".to_owned())
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(metrics.corner_radius)
            .shadow_lg()
            .border(metrics.border_width)
            .border_color(paint.border)
            .bg(paint.background)
            .block_mouse_except_scroll()
            .child(self.render_editor(theme, cx))
            .child(separator_line(metrics, paint))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .py(metrics.panel_padding)
                    .child(content),
            )
            .when(self.has_footer(), |panel| {
                panel
                    .child(separator_line(metrics, paint))
                    .child(self.render_footer(width, theme, cx))
            })
            .into_any_element()
    }

    /// Renders the borderless search line and its trailing controls as one continuous surface.
    fn render_editor(
        &self,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let metrics = theme.metrics;
        let palette = cx.entity().downgrade();
        div()
            .debug_selector(|| "command-palette-editor".to_owned())
            .w_full()
            .h(metrics.input_height)
            .flex_shrink_0()
            .pl(metrics.content_leading_inset())
            .pr(metrics.panel_padding)
            .flex()
            .flex_row()
            .items_center()
            .gap(metrics.gap)
            .text_size(metrics.input_size)
            .child(div().min_w_0().flex_1().child(self.input.clone()))
            .when(!self.header_actions.is_empty(), |editor| {
                editor.child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .children(
                            self.header_actions
                                .iter()
                                .enumerate()
                                .map(|(index, action)| {
                                    render_header_action(palette.clone(), index, action)
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_results(
        &self,
        list_height: Pixels,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let items = Rc::clone(&self.presented_items);
        let matches = Rc::clone(&self.matches);
        let presented_results = Rc::clone(&self.presented_results);
        let selected = self.selected.clone();
        let hover_suppressed = self.pointer_suppressed || self.hover_suppressed;
        let leading_reserved = self.leading_reserved;
        let palette = cx.entity().downgrade();
        div()
            .relative()
            .size_full()
            .child(
                list(self.list.clone(), move |index, _, _| {
                    let Some(row) = presented_results.row(index) else {
                        return div().into_any_element();
                    };
                    let row_height = row.height(theme.metrics);
                    match row {
                        PaletteRow::Section(label) => {
                            render_section(label.clone(), row_height, theme).into_any_element()
                        }
                        PaletteRow::Separator => {
                            render_row_separator(row_height, theme).into_any_element()
                        }
                        PaletteRow::Item { position, .. } => matches
                            .get(*position)
                            .and_then(|matched| {
                                items.get(matched.item_index).map(|item| {
                                    render_row(
                                        palette.clone(),
                                        *position,
                                        item,
                                        &matched.label_highlights,
                                        &matched.description_highlights,
                                        selected.as_ref() == Some(&item.id),
                                        hover_suppressed,
                                        leading_reserved,
                                        row_height,
                                        theme,
                                    )
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element()),
                    }
                })
                .h(list_height)
                .w_full(),
            )
            .child(self.scrollbar.clone())
            .into_any_element()
    }

    /// Renders the footer's caller actions.
    ///
    /// A lone ordinary action is offered directly, because a disclosure that reveals exactly one
    /// choice is more chrome than the choice it hides. Anything else stays a menu.
    fn render_actions(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        if self.actions_menu.is_empty() {
            return None;
        }
        let palette = cx.entity().downgrade();
        if let [entry] = self.actions_menu.as_slice()
            && let Some(action) = entry.plain_action()
        {
            let emitted = action.action.clone();
            return Some(
                Button::new("command-palette-actions-single", action.label.clone())
                    .variant(ButtonVariant::Ghost)
                    .size(FOOTER_CONTROL_SIZE)
                    .disabled(action.disabled)
                    .tab_stop(true)
                    .when_some(action.debug_selector, |button, selector| {
                        button.debug_selector(selector)
                    })
                    .on_activate(move |_, _, cx| {
                        let action = emitted.clone();
                        let _ = palette.update(cx, |_, cx| {
                            cx.emit(CommandPaletteEvent::<I>::MenuAction(action));
                        });
                    })
                    .into_any_element(),
            );
        }
        Some(
            Menu::new(
                "command-palette-actions-menu",
                self.actions_menu_label.clone(),
                self.actions_menu.clone(),
            )
            .size(MenuSize::Regular)
            .debug_selector("command-palette-actions-menu")
            .on_activate(move |activation: &MenuActivation<SharedString>, _, cx| {
                let action = activation.action().clone();
                let _ = palette.update(cx, |_, cx| {
                    cx.emit(CommandPaletteEvent::<I>::MenuAction(action));
                });
            })
            .into_any_element(),
        )
    }

    fn render_footer(
        &self,
        width: Pixels,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let paint = theme.paint;
        let metrics = theme.metrics;
        // Caller actions anchor the leading edge and the confirm anchors the trailing one, so an
        // escape hatch never reads as the peer of the primary action.
        div()
            .debug_selector(|| "command-palette-footer".to_owned())
            .w_full()
            .h(metrics.footer_height)
            .flex_shrink_0()
            .px(metrics.footer_inset())
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(metrics.gap)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(metrics.gap)
                    .children(self.render_actions(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(metrics.gap)
                    .children(
                        self.hints
                            .iter()
                            .filter(|_| width >= px(540.0) || self.confirm.is_none())
                            .map(|hint| render_hint(hint, metrics, paint)),
                    )
                    .when_some(self.confirm.clone(), |trailing, confirm| {
                        let confirm_palette = cx.entity().downgrade();
                        let shortcut = confirm.shortcut.clone();
                        let shortcut_size = metrics.secondary_size;
                        trailing.child(
                            Button::new("command-palette-confirm", confirm.label.clone())
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Small)
                                .disabled(confirm.disabled)
                                .tab_stop(true)
                                .when_some(confirm.debug_selector.clone(), |button, selector| {
                                    button.debug_selector(selector)
                                })
                                .trailing(move |foreground| {
                                    div()
                                        .text_size(shortcut_size)
                                        .text_color(foreground)
                                        .child(shortcut)
                                        .into_any_element()
                                })
                                .on_activate(move |_, _, cx| {
                                    let _ = confirm_palette
                                        .update(cx, |palette, cx| palette.confirm_activated(cx));
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn chrome_height(metrics: CommandPaletteMetrics, footer: bool) -> Pixels {
    let footer_height = if footer {
        metrics.border_width + metrics.footer_height
    } else {
        px(0.0)
    };
    metrics.panel_padding * 2.0 + metrics.input_height + metrics.border_width * 3.0 + footer_height
}

fn separator_line(metrics: CommandPaletteMetrics, paint: CommandPalettePaint) -> impl IntoElement {
    div()
        .w_full()
        .h(metrics.border_width)
        .flex_shrink_0()
        .bg(paint.separator)
}

fn render_hint(
    hint: &CommandPaletteHint,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(metrics.accessory_padding)
        .text_size(metrics.secondary_size)
        .child(
            div()
                .text_color(paint.footer_foreground)
                .child(hint.label.clone()),
        )
        .child(
            div()
                .text_color(paint.footer_key_foreground)
                .child(hint.key.clone()),
        )
}

/// Renders one search-line control as a ghost icon button from the installed button catalog.
fn render_header_action<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    index: usize,
    action: &CommandPaletteAction,
) -> AnyElement {
    let id = action.id.clone();
    let icon = action.icon.clone();
    let mut button = IconButton::new(
        ("command-palette-header-action", index),
        action.accessibility_name.clone(),
        move |foreground| icon(foreground),
    )
    .variant(ButtonVariant::Ghost)
    .size(ButtonSize::Compact)
    .disabled(action.disabled)
    .tab_stop(true)
    .on_activate(move |_, _, cx| {
        let id = id.clone();
        let _ = palette.update(cx, |_, cx| {
            cx.emit(CommandPaletteEvent::<I>::HeaderAction(id));
        });
    });
    if let Some(selector) = action.debug_selector.clone() {
        button = button.debug_selector(selector);
    }
    button.into_any_element()
}

fn render_section(
    label: SharedString,
    height: Pixels,
    theme: CommandPaletteTheme,
) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .w_full()
        .h(height)
        .pl(metrics.content_leading_inset())
        .flex()
        .items_center()
        .text_size(metrics.secondary_size)
        .text_color(theme.paint.section_foreground)
        .child(label)
}

fn render_row_separator(height: Pixels, theme: CommandPaletteTheme) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .w_full()
        .h(height)
        .px(metrics.panel_padding)
        .flex()
        .items_center()
        .child(
            div()
                .w_full()
                .h(metrics.border_width)
                .bg(theme.paint.separator),
        )
}

fn status_row(
    text: impl Into<SharedString>,
    debug_selector: &'static str,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .debug_selector(move || debug_selector.to_owned())
        .w_full()
        .h(metrics.row_height)
        .px(metrics.content_leading_inset())
        .flex()
        .items_center()
        .text_size(metrics.secondary_size)
        .text_color(paint.muted)
        .child(text.into())
}

fn row_foreground(paint: CommandPalettePaint, disabled: bool, selected: bool) -> Rgba {
    if disabled {
        paint.disabled
    } else if selected {
        paint.selected_foreground
    } else {
        paint.foreground
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one row's complete presentation inputs are clearer than an intermediate struct"
)]
fn render_row<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    position: usize,
    item: &CommandPaletteItem<I>,
    label_highlights: &[Range<usize>],
    description_highlights: &[Range<usize>],
    selected: bool,
    hover_suppressed: bool,
    leading_reserved: bool,
    height: Pixels,
    theme: CommandPaletteTheme,
) -> AnyElement {
    let paint = theme.paint;
    let metrics = theme.metrics;
    let foreground = row_foreground(paint, item.disabled, selected);
    let secondary = if item.disabled {
        paint.disabled
    } else {
        paint.muted
    };
    let match_foreground = if item.disabled {
        paint.disabled
    } else {
        paint.match_foreground
    };
    let active_background = if hover_suppressed {
        paint.selected_background
    } else {
        paint.hover_background
    };
    let logical_name = item.label.clone();
    let debug_selector = item.debug_selector.clone();
    let id = item.id.clone();
    let hover_palette = palette.clone();
    let mut row = div()
        .id(("command-palette-row", position))
        .debug_selector(move || debug_selector.unwrap_or_else(|| logical_name.to_string()))
        .relative()
        .w_full()
        .h(height)
        .px(metrics.horizontal_padding)
        .flex()
        .items_center()
        .gap(metrics.gap)
        .rounded(metrics.row_corner_radius())
        .text_color(foreground)
        .cursor_default()
        .when(selected, |row| row.bg(active_background))
        .when(!item.disabled, |row| {
            let id = id.clone();
            row.on_mouse_move(move |event, _, cx| {
                let _ = hover_palette.update(cx, |palette, cx| {
                    palette.pointer_hover(&id, event.position, cx)
                });
            })
        });

    if leading_reserved {
        let mut leading = div()
            .w(metrics.leading_width)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        if let Some(icon) = item.leading_icon.clone() {
            leading = leading.child(icon(if item.disabled {
                paint.disabled_icon_foreground
            } else {
                paint.icon_foreground
            }));
        }
        row = row.child(leading);
    }

    let label_line = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(metrics.gap)
        .child(div().min_w_0().flex_1().child(highlighted_text(
            item.label.clone(),
            label_highlights,
            foreground,
            match_foreground,
            metrics.label_size,
        )))
        .when_some(item.trailing.clone(), |line, accessory| {
            line.child(render_accessory(
                accessory,
                secondary,
                paint.selected_background,
                metrics,
            ))
        });
    let text = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap(metrics.row_line_gap)
        .child(label_line)
        .when_some(item.description.clone(), |text, description| {
            text.child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .child(highlighted_text(
                        description,
                        description_highlights,
                        secondary,
                        match_foreground,
                        metrics.secondary_size,
                    )),
            )
        });
    row = row.child(text);

    if !item.disabled {
        let down_palette = palette.clone();
        let up_palette = palette;
        let down_id = item.id.clone();
        let up_id = item.id.clone();
        row = row.child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |_, hitbox, window, _| {
                    let down_hitbox = hitbox.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if !phase.capture()
                            || event.button != MouseButton::Left
                            || !down_hitbox.is_hovered(window)
                        {
                            return;
                        }
                        window.prevent_default();
                        let id = down_id.clone();
                        let _ = down_palette.update(cx, |palette, cx| palette.pointer_down(id, cx));
                        cx.stop_propagation();
                    });
                    let up_hitbox = hitbox.clone();
                    let move_palette = up_palette.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture() || event.button != MouseButton::Left {
                            return;
                        }
                        let inside = up_hitbox.is_hovered(window);
                        let activate = up_palette
                            .update(cx, |palette, _| palette.pointer_up(&up_id, inside))
                            .unwrap_or(false);
                        if activate {
                            window.prevent_default();
                            let _ = up_palette.update(cx, |palette, cx| {
                                palette.selected = Some(up_id.clone());
                                palette.activate_selected(
                                    CommandPaletteActivationSource::Pointer,
                                    window,
                                    cx,
                                );
                            });
                            cx.stop_propagation();
                        }
                    });
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase.capture() && event.pressed_button != Some(MouseButton::Left) {
                            let _ = move_palette.update(cx, |palette, _| {
                                palette.pointer_press = None;
                            });
                        }
                    });
                },
            )
            .absolute()
            .inset_0(),
        );
    }
    div()
        .w_full()
        .px(metrics.panel_padding)
        .child(row)
        .into_any_element()
}

fn highlighted_text(
    text: SharedString,
    ranges: &[Range<usize>],
    foreground: Rgba,
    highlight: Rgba,
    size: Pixels,
) -> AnyElement {
    let source = text.as_ref();
    let mut content = div()
        .min_w_0()
        .flex()
        .items_center()
        .truncate()
        .text_size(size)
        .text_color(foreground);
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            content = content.child(source[cursor..range.start].to_owned());
        }
        content = content.child(
            div()
                .text_color(highlight)
                .child(source[range.clone()].to_owned()),
        );
        cursor = range.end;
    }
    if cursor < source.len() {
        content = content.child(source[cursor..].to_owned());
    }
    content.into_any_element()
}

fn render_accessory(
    accessory: CommandPaletteAccessory,
    color: Rgba,
    _status_background: Rgba,
    metrics: CommandPaletteMetrics,
) -> AnyElement {
    match accessory {
        CommandPaletteAccessory::Text(text) | CommandPaletteAccessory::Shortcut(text) => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Status(text) => div()
            .flex_shrink_0()
            .px(metrics.accessory_padding)
            .py(metrics.accessory_line_padding)
            .rounded(metrics.accessory_radius)
            .text_size(metrics.secondary_size)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Checkmark => {
            Icon::new(IconName::Check, px(12.0), color).into_any_element()
        }
    }
}

#[cfg(test)]
#[path = "command_palette_tests.rs"]
mod tests;
