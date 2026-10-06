use std::{cell::Cell, ops::Range, rc::Rc, time::Duration};

use gpui::{
    Anchor, AnyElement, App, AppContext as _, BorrowAppContext as _, Entity, EventEmitter, Global,
    HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render,
    Rgba, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, WeakFocusHandle, Window, WindowId, accesskit, actions,
    anchored, canvas, div, list, prelude::FluentBuilder as _, px,
};

use crate::{
    ComboButton, FloatingRole, FloatingShell, Icon, IconName, MenuAlignment, MenuEntry,
    MenuPlacement, MenuPlacementConfig, ProgressBar, ProgressSize, ProgressState, TextInput,
    TextInputEvent, TextInputTabBehavior, TextInputVariant,
    button::{Button, ButtonSize, ButtonVariant, IconButton},
    fuzzy::{FuzzyTarget, fuzzy_filter, highlight_ranges},
    leading_columns::{LeadingColumnMetrics, LeadingColumns},
    overlay_scrollbar::{OverlayScrollbar, OverlayScrollbarEvent, ScrollMetrics},
};

const KEY_CONTEXT: &str = "SpaceTermCommandPalette";
/// The palette is the window's focal search surface.
const COMMAND_PALETTE_ROLE: FloatingRole = FloatingRole::Command;

/// Empty-state actions are the content's only controls, so they take the ordinary control size.
const EMPTY_ACTION_SIZE: ButtonSize = ButtonSize::Regular;
/// A description longer than this many lines is truncated rather than growing the panel.
const EMPTY_DESCRIPTION_LINE_LIMIT: usize = 3;
/// How long a load may run before the palette shows its loading state.
const LOADING_GRACE_PERIOD: Duration = Duration::from_millis(100);
/// How long a shown loading state stays before results replace it, so it never flickers.
const LOADING_MINIMUM_DISPLAY: Duration = Duration::from_millis(400);
/// The loading state's visible text, which also names its progress bar.
const LOADING_TEXT: &str = "Loading\u{2026}";

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
    /// The Linux set: the macOS set without Command chords, confirming with Control-Return.
    Linux,
}

/// Installs the platform-specific bindings for `profile`.
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
        CommandPaletteKeybindingProfile::Linux => cx.bind_keys([
            KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-p", MoveUp, Some(KEY_CONTEXT)),
            KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-n", MoveDown, Some(KEY_CONTEXT)),
            KeyBinding::new("pageup", MovePageUp, Some(KEY_CONTEXT)),
            KeyBinding::new("pagedown", MovePageDown, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-m", Activate, Some(KEY_CONTEXT)),
            KeyBinding::new("ctrl-enter", Confirm, Some(KEY_CONTEXT)),
            KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
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
type PaletteReplacementFocus = Rc<dyn Fn(&App) -> Option<CommandPaletteReplacementFocus>>;

struct ErasedPaletteRegistration {
    token: CommandPaletteRegistration,
    replacement_focus: PaletteReplacementFocus,
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
) -> (
    CommandPaletteRegistration,
    Option<u64>,
    Option<CommandPaletteReplacementFocus>,
) {
    let window_id = window.window_handle().window_id();
    let window_handle = window.window_handle();
    let suspend_window = window_handle;
    let focus_owner = owner.clone();
    let replacement_focus: PaletteReplacementFocus = Rc::new(move |cx| {
        focus_owner
            .read_with(cx, |palette, _| palette.captured_replacement_focus())
            .ok()
            .flatten()
    });
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
                        replacement_focus,
                        suspend,
                        resume,
                        replace,
                        replace_now,
                    },
                )
                .map(|registration| (registration.replace, registration.replacement_focus));
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
    let inherited = replaced.and_then(|(replace, replacement_focus)| {
        let predecessor = replacement_focus(cx);
        replace(cx);
        predecessor
    });
    (token, modal_suspension, inherited)
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
    /// An assistive technology press activated one row, such as VoiceOver's activation.
    Accessibility,
}

/// A typed command-palette activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteActivation<I> {
    item_id: I,
    source: CommandPaletteActivationSource,
}

impl<I> CommandPaletteActivation<I> {
    pub fn item_id(&self) -> &I {
        &self.item_id
    }

    pub fn source(&self) -> CommandPaletteActivationSource {
        self.source
    }

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
    replacement: CommandPaletteReplacementFocus,
    transferred_focus: bool,
}

/// Typed events emitted by a [`CommandPalette`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPaletteEvent<I> {
    /// The palette opened or closed.
    Lifecycle(CommandPaletteLifecycleEvent),
    /// An enabled semantic item was activated.
    Activated(CommandPaletteActivation<I>),
    /// The query changed.
    QueryChanged(String),
    /// A search-line control was activated.
    HeaderAction(SharedString),
    /// An empty-state action was activated by pointer, Return, or its own keyboard focus.
    EmptyAction(SharedString),
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

type IconBuilder = Rc<dyn Fn(Rgba, Pixels) -> AnyElement>;
type ActionIconBuilder = Rc<dyn Fn(Rgba) -> AnyElement>;

/// One control rendered at the trailing edge of the command-palette search line.
///
/// The caller owns the icon and the identity it receives back through
/// [`CommandPaletteEvent::HeaderAction`]; the palette owns the control's size and paint.
#[derive(Clone)]
pub struct CommandPaletteAction {
    id: SharedString,
    accessibility_name: SharedString,
    icon: ActionIconBuilder,
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

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    pub fn id(&self) -> &SharedString {
        &self.id
    }
}

/// The search line's primary command, presented as a labeled button at its trailing edge.
///
/// Activating it or one of its menu items emits [`CommandPaletteEvent::HeaderAction`] and leaves
/// the palette open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPalettePrimaryAction {
    id: SharedString,
    label: SharedString,
    shortcut: Option<SharedString>,
    disabled: bool,
    menu_items: Vec<(SharedString, SharedString)>,
    menu_disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPalettePrimaryAction {
    /// Creates an enabled action. The label is also its logical accessibility name.
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            shortcut: None,
            disabled: false,
            menu_items: Vec::new(),
            menu_disabled: false,
            debug_selector: None,
        }
    }

    /// Shows the displayed Shortcut of [`Confirm`], the key that activates the action, after
    /// its label.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Appends a related command to the action's menu, in presentation order.
    pub fn menu_item(
        mut self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        self.menu_items.push((id.into(), label.into()));
        self
    }

    pub fn menu_disabled(mut self, disabled: bool) -> Self {
        self.menu_disabled = disabled;
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

/// One control offered by the palette's empty state.
///
/// The caller owns the label and the identity it receives back through
/// [`CommandPaletteEvent::EmptyAction`]; the palette owns the control's size, paint, and order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPaletteEmptyAction {
    id: SharedString,
    label: SharedString,
    disabled: bool,
    debug_selector: Option<String>,
}

impl CommandPaletteEmptyAction {
    /// Creates an enabled action. The label is also its logical accessibility name.
    pub fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            disabled: false,
            debug_selector: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    pub fn id(&self) -> &SharedString {
        &self.id
    }
}

/// What the palette presents when a settled query has no results.
///
/// The first action is the default action: it takes the primary emphasis, and Return activates
/// it while the empty state is shown and the action is enabled. Activating any action leaves the
/// palette open; the caller decides what follows.
#[derive(Clone)]
pub struct CommandPaletteEmpty {
    icon: Option<IconBuilder>,
    title: SharedString,
    description: Option<SharedString>,
    actions: Vec<CommandPaletteEmptyAction>,
}

impl CommandPaletteEmpty {
    /// Creates an empty state with a single-line title and no description or actions.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            title: title.into(),
            description: None,
            actions: Vec::new(),
        }
    }

    /// Adds secondary text below the title. Long text wraps to a bounded number of lines.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds a prominent icon above the title, built with the muted foreground and live size.
    pub fn icon(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.icon = Some(Rc::new(build));
        self
    }

    /// Appends one action. The first appended action is the default action.
    pub fn action(mut self, action: CommandPaletteEmptyAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description_text(&self) -> Option<&str> {
        self.description.as_deref()
    }

    fn default_action(&self) -> Option<&CommandPaletteEmptyAction> {
        self.actions.first().filter(|action| !action.disabled)
    }
}

/// Who decides which items a query presents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandPaletteMatching {
    /// The palette filters and ranks items with its own static semantic matcher.
    #[default]
    Semantic,
    /// The caller supplies exactly the items to present, already filtered, ordered, and carrying
    /// any matched label indices. Callers whose query is an address rather than a search term,
    /// such as a filesystem path, select this.
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
    matched_indices: Vec<usize>,
    matched_description_indices: Vec<usize>,
    disabled: bool,
    default_selectable: bool,
    leading_icon: Option<IconBuilder>,
    trailing: Option<CommandPaletteAccessory>,
    debug_selector: Option<String>,
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
            matched_indices: Vec::new(),
            matched_description_indices: Vec::new(),
            disabled: false,
            default_selectable: true,
            leading_icon: None,
            trailing: None,
            debug_selector: None,
        }
    }

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

    /// Supplies matched label character indices for caller-filtered results.
    pub fn matched_indices(mut self, indices: impl IntoIterator<Item = usize>) -> Self {
        self.matched_indices = indices.into_iter().collect();
        self
    }

    /// Supplies matched description character indices for caller-filtered results.
    pub fn matched_description_indices(mut self, indices: impl IntoIterator<Item = usize>) -> Self {
        self.matched_description_indices = indices.into_iter().collect();
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Keeps the palette from selecting this item on its own, such as a row that leaves the
    /// current context. Navigation and the pointer still reach it. When no other item is
    /// selectable, Return activates the primary action.
    pub fn outside_default_selection(mut self) -> Self {
        self.default_selectable = false;
        self
    }

    /// Adds a bounded leading icon built with the resolved row foreground color and live size.
    pub fn leading_icon(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.leading_icon = Some(Rc::new(build));
        self
    }

    pub fn trailing(mut self, accessory: CommandPaletteAccessory) -> Self {
        self.trailing = Some(accessory);
        self
    }

    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    pub fn id(&self) -> &I {
        &self.id
    }

    pub fn label(&self) -> &str {
        self.label.as_ref()
    }

    pub fn description_text(&self) -> Option<&str> {
        self.description.as_ref().map(AsRef::as_ref)
    }

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
    if matching == CommandPaletteMatching::Caller {
        return items
            .iter()
            .enumerate()
            .map(|(item_index, item)| CommandPaletteMatch {
                item_index,
                score: 0,
                label_highlights: highlight_ranges(&item.label, &item.matched_indices),
                description_highlights: item.description.as_ref().map_or_else(Vec::new, |text| {
                    highlight_ranges(text, &item.matched_description_indices)
                }),
            })
            .collect();
    }

    let mut section_groups = vec![0usize; items.len()];
    for item_index in 1..items.len() {
        section_groups[item_index] = section_groups[item_index - 1]
            + usize::from(items[item_index].section != items[item_index - 1].section);
    }
    let mut matches = fuzzy_filter(items, query, |item| {
        item.keywords
            .iter()
            .fold(FuzzyTarget::new(item.label.as_ref()), |target, keyword| {
                target.field(keyword.as_ref())
            })
    })
    .into_iter()
    .map(|matched| {
        let item_index = matched.item_index();
        CommandPaletteMatch {
            item_index,
            score: matched.score(),
            label_highlights: highlight_ranges(
                items[item_index].label.as_ref(),
                &matched.field_highlight_indices(0),
            ),
            description_highlights: Vec::new(),
        }
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

/// Application-owned command-palette paint values.
///
/// The panel's material, edge, internal rules, corners, and elevation belong to the shared command
/// surface. This catalog carries the palette's own content and row states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPalettePaint {
    rows: Option<crate::ListRowPaints>,
    foreground: Rgba,
    muted: Rgba,
    disabled: Rgba,
    hover_background: Rgba,
    hover_foreground: Rgba,
    selected_background: Rgba,
    selected_foreground: Rgba,
    match_foreground: Rgba,
    icon_foreground: Rgba,
    disabled_icon_foreground: Rgba,
    section_foreground: Rgba,
}

impl CommandPalettePaint {
    /// Creates the core paint catalog.
    ///
    /// Hover and section colors default to the closest core value so a caller only
    /// overrides what its theme distinguishes.
    pub fn new(
        foreground: Rgba,
        muted: Rgba,
        disabled: Rgba,
        selected_background: Rgba,
        selected_foreground: Rgba,
        match_foreground: Rgba,
    ) -> Self {
        Self {
            rows: None,
            foreground,
            muted,
            disabled,
            hover_background: selected_background,
            hover_foreground: selected_foreground,
            selected_background,
            selected_foreground,
            match_foreground,
            icon_foreground: foreground,
            disabled_icon_foreground: disabled,
            section_foreground: muted,
        }
    }

    pub fn rows(mut self, rows: crate::ListRowPaints) -> Self {
        self.rows = Some(rows);
        self
    }

    /// Resolves the same complete row state consumed by the production renderer.
    ///
    /// Keyboard selection and pointer hover are independent facts about one row. Pointer hover in
    /// this control also moves the selection, so the row under the pointer is normally both, and
    /// the combined state is the one a reader actually sees.
    pub fn row_paint(
        self,
        disabled: bool,
        selected: bool,
        hovered: bool,
        collection_focused: bool,
    ) -> crate::ListRowPaint {
        if let Some(rows) = self.rows {
            return rows.resolve_for_collection(!disabled, selected, hovered, collection_focused);
        }
        let foreground = if disabled {
            self.disabled
        } else if hovered {
            self.hover_foreground
        } else if selected {
            self.selected_foreground
        } else {
            self.foreground
        };
        crate::ListRowPaint::new(
            if hovered {
                self.hover_background
            } else if selected {
                self.selected_background
            } else {
                gpui::rgba(0)
            },
            foreground,
            if disabled {
                self.disabled
            } else if selected || hovered {
                foreground
            } else {
                self.muted
            },
            if disabled {
                self.disabled_icon_foreground
            } else if selected || hovered {
                foreground
            } else {
                self.icon_foreground
            },
            if disabled {
                self.disabled
            } else if selected || hovered {
                foreground
            } else {
                self.match_foreground
            },
            gpui::rgba(0),
        )
    }

    pub fn icons(mut self, normal: Rgba, disabled: Rgba) -> Self {
        self.icon_foreground = normal;
        self.disabled_icon_foreground = disabled;
        self
    }

    /// Sets the pointer-hover row background, which stays distinct from the selected background.
    pub fn hover_background(mut self, color: Rgba) -> Self {
        self.hover_background = color;
        self
    }

    pub fn hover_foreground(mut self, color: Rgba) -> Self {
        self.hover_foreground = color;
        self
    }

    pub fn section_foreground(mut self, color: Rgba) -> Self {
        self.section_foreground = color;
        self
    }
}

/// Native desktop dimensions for the command-palette panel.
///
/// The panel's corner radius, content inset, and hairline are resolved by the shared command
/// surface and cached here for the geometry this family computes.
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
    empty_padding: Pixels,
    empty_line_gap: Pixels,
    empty_actions_gap: Pixels,
    loading_bar_width: Pixels,
    horizontal_padding: Pixels,
    leading_width: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    input_size: Pixels,
    input_icon_size: Pixels,
    label_size: Pixels,
    secondary_size: Pixels,
    section_size: Pixels,
    accessory_padding: Pixels,
    accessory_line_padding: Pixels,
    accessory_radius: Pixels,
    body_line_height: Pixels,
    secondary_line_height: Pixels,
    section_line_height: Pixels,
    icon_size: Pixels,
    icon_baseline_center: Pixels,
}

impl CommandPaletteMetrics {
    /// Creates compact native defaults around a panel width and row height.
    pub fn new(panel_width: Pixels, row_height: Pixels) -> Self {
        let shell = crate::FloatingSurfaceTheme::default().shell(COMMAND_PALETTE_ROLE);
        Self {
            panel_width,
            maximum_height: px(480.0),
            top_offset: px(52.0),
            viewport_margin: px(16.0),
            panel_padding: shell.content_inset(),
            input_height: px(42.0),
            row_height,
            single_line_row_height: row_height,
            row_line_gap: px(2.0),
            section_height: px(22.0),
            separator_height: px(9.0),
            empty_padding: px(20.0),
            empty_line_gap: px(4.0),
            empty_actions_gap: px(14.0),
            loading_bar_width: px(160.0),
            horizontal_padding: px(12.0),
            leading_width: px(18.0),
            gap: px(10.0),
            corner_radius: shell.corner_radius(),
            border_width: shell.hairline(),
            input_size: px(14.0),
            input_icon_size: px(14.0),
            label_size: px(13.0),
            secondary_size: px(11.0),
            section_size: px(13.0),
            accessory_padding: px(5.0),
            accessory_line_padding: px(2.0),
            accessory_radius: px(4.0),
            body_line_height: px(16.0),
            secondary_line_height: px(15.0),
            section_line_height: px(17.0),
            icon_size: px(12.0),
            icon_baseline_center: px(4.0),
        }
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

    pub fn panel_geometry(mut self, maximum_height: Pixels, top_offset: Pixels) -> Self {
        self.maximum_height = maximum_height;
        self.top_offset = top_offset;
        self
    }

    pub fn viewport_margin(mut self, margin: Pixels) -> Self {
        self.viewport_margin = margin;
        self
    }

    pub fn editor_height(mut self, input_height: Pixels) -> Self {
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

    pub fn row_line_gap(mut self, gap: Pixels) -> Self {
        self.row_line_gap = gap;
        self
    }

    pub fn section_spacing(mut self, section_height: Pixels, separator_height: Pixels) -> Self {
        self.section_height = section_height;
        self.separator_height = separator_height;
        self
    }

    /// Sets editor, primary, and secondary font sizes.
    pub fn font_sizes(mut self, input: Pixels, label: Pixels, secondary: Pixels) -> Self {
        self.input_size = input;
        self.label_size = label;
        self.secondary_size = secondary;
        self
    }

    pub fn section_font_size(mut self, size: Pixels) -> Self {
        self.section_size = size;
        self
    }

    /// Sets each text role's line box and the chrome glyph size.
    pub fn text_geometry(
        mut self,
        body_line_height: Pixels,
        secondary_line_height: Pixels,
        section_line_height: Pixels,
        icon_size: Pixels,
    ) -> Self {
        self.body_line_height = body_line_height;
        self.secondary_line_height = secondary_line_height;
        self.section_line_height = section_line_height;
        self.icon_size = icon_size;
        self
    }

    /// Sets the center-above-baseline metric for icons paired with result labels.
    pub fn icon_baseline_center(mut self, center: Pixels) -> Self {
        self.icon_baseline_center = center;
        self
    }

    pub fn input_icon_size(mut self, size: Pixels) -> Self {
        self.input_icon_size = size;
        self
    }

    fn scaled(self, spacing_scale: f32) -> Self {
        let width_scale = crate::appearance::normalized_scale(spacing_scale).max(1.0);
        Self {
            panel_width: self.panel_width * width_scale,
            maximum_height: crate::appearance::scale_metric(self.maximum_height, spacing_scale),
            top_offset: crate::appearance::scale_metric(self.top_offset, spacing_scale),
            viewport_margin: crate::appearance::scale_metric(self.viewport_margin, spacing_scale),
            input_height: crate::appearance::scale_line_box(
                self.input_height,
                self.body_line_height,
                spacing_scale,
            ),
            row_height: crate::appearance::scale_line_box(
                self.row_height,
                self.body_line_height + self.secondary_line_height,
                spacing_scale,
            ),
            single_line_row_height: crate::appearance::scale_line_box(
                self.single_line_row_height,
                self.body_line_height,
                spacing_scale,
            ),
            row_line_gap: crate::appearance::scale_metric(self.row_line_gap, spacing_scale),
            section_height: crate::appearance::scale_line_box(
                self.section_height,
                self.section_line_height,
                spacing_scale,
            ),
            separator_height: crate::appearance::scale_metric(self.separator_height, spacing_scale),
            empty_padding: crate::appearance::scale_metric(self.empty_padding, spacing_scale),
            empty_line_gap: crate::appearance::scale_metric(self.empty_line_gap, spacing_scale),
            empty_actions_gap: crate::appearance::scale_metric(
                self.empty_actions_gap,
                spacing_scale,
            ),
            loading_bar_width: crate::appearance::scale_metric(
                self.loading_bar_width,
                spacing_scale,
            ),
            horizontal_padding: crate::appearance::scale_metric(
                self.horizontal_padding,
                spacing_scale,
            ),
            leading_width: crate::appearance::scale_metric(self.leading_width, spacing_scale)
                .max(self.icon_size),
            gap: crate::appearance::scale_metric(self.gap, spacing_scale),
            accessory_padding: crate::appearance::scale_metric(
                self.accessory_padding,
                spacing_scale,
            ),
            accessory_line_padding: crate::appearance::scale_metric(
                self.accessory_line_padding,
                spacing_scale,
            ),
            ..self
        }
    }

    fn leading_column_metrics(&self) -> LeadingColumnMetrics {
        LeadingColumnMetrics {
            state_width: px(0.0),
            icon_width: self.leading_width,
            column_gap: px(0.0),
        }
    }

    fn empty_icon_size(&self) -> Pixels {
        self.body_line_height * 2.0
    }

    fn header_action_size(&self) -> Pixels {
        (self.input_height - self.panel_padding * 2.0).max(self.input_icon_size)
    }

    /// Returns the shared left edge of the editor, headings, status text, and row content.
    fn content_leading_inset(&self) -> Pixels {
        self.panel_padding + self.horizontal_padding
    }

    fn row_corner_radius(&self) -> Pixels {
        (self.corner_radius - self.panel_padding).max(px(0.0))
    }
}

/// Application-owned presentation installed once for every command palette.
///
/// The panel's surface treatment belongs to the shared command-surface role, which carries a
/// stronger elevation than an anchored popup because the palette takes the window rather than
/// hanging from a control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommandPaletteTheme {
    paint: CommandPalettePaint,
    metrics: CommandPaletteMetrics,
    shell: FloatingShell,
}

impl CommandPaletteTheme {
    /// Creates a complete command-palette theme.
    pub fn new(paint: CommandPalettePaint, metrics: CommandPaletteMetrics) -> Self {
        Self {
            paint,
            metrics,
            shell: crate::FloatingSurfaceTheme::default().shell(COMMAND_PALETTE_ROLE),
        }
    }

    pub(crate) fn scaled_spacing(self, spacing_scale: f32) -> Self {
        Self {
            metrics: self.metrics.scaled(spacing_scale),
            ..self
        }
    }
}

impl Global for CommandPaletteTheme {}

/// Resolves the installed palette theme against the shared command surface.
fn command_palette_theme(cx: &App) -> CommandPaletteTheme {
    let mut theme = *crate::control_theme_catalog(cx).map_or_else(
        || cx.global::<CommandPaletteTheme>(),
        |catalog| &catalog.command_palette,
    );
    let shell = crate::floating_surface::shell(COMMAND_PALETTE_ROLE, cx);
    theme.metrics.corner_radius = shell.corner_radius();
    theme.metrics.panel_padding = shell.content_inset();
    theme.metrics.border_width = shell.hairline();
    theme.shell = shell;
    theme
}

/// A reusable entity-backed command palette with typed semantic items.
pub struct CommandPalette<I: Clone + Eq + 'static> {
    /// Names the presented panel for assistive technology.
    accessibility_name: SharedString,
    empty: CommandPaletteEmpty,
    items: Rc<[CommandPaletteItem<I>]>,
    matches: Rc<[CommandPaletteMatch]>,
    presented_results: Rc<PresentedResults>,
    leading_columns: LeadingColumns,
    input_leading_icon: Option<IconBuilder>,
    query_prefix: Option<SharedString>,
    query_note: Option<SharedString>,
    header_actions: Vec<CommandPaletteAction>,
    primary_action: Option<CommandPalettePrimaryAction>,
    results_note: Option<SharedString>,
    matching: CommandPaletteMatching,
    activation: CommandPaletteActivationPolicy,
    selected: Option<I>,
    preferred: Option<I>,
    query: String,
    /// Whether the caller's results are still arriving.
    loading: bool,
    loading_presentation: LoadingPresentation,
    /// Advances `loading_presentation` when its current phase ends.
    loading_timer: Option<Task<()>>,
    /// Whether the panel has been visible since the palette opened.
    presented: bool,
    /// The results area's height when results were last presented, kept during a reload.
    results_height: Option<Pixels>,
    dismissible: bool,
    escape_cancellable: bool,
    open: bool,
    pending_open: Option<PendingCommandPaletteOpen>,
    suspended_by_modal: Option<u64>,
    coordinator_registration: Option<CommandPaletteRegistration>,
    input: Entity<TextInput>,
    focus_scope: gpui::FocusHandle,
    /// The primary action's focus, retained so a disabled primary action can return focus to the
    /// query instead of leaving the window without a focused element.
    primary_action_focus: gpui::FocusHandle,
    scrollbar: Entity<OverlayScrollbar<f32>>,
    restore_focus: Option<WeakFocusHandle>,
    restore_on_activation: Option<WeakFocusHandle>,
    pointer_press: Option<I>,
    pointer_suppressed: bool,
    hover_suppressed: bool,
    /// The row the pointer is actually over, which is normally also the selected row.
    hovered_row: Option<I>,
    pointer_anchor: gpui::Point<Pixels>,
    list: ListState,
    list_row_heights: Option<[Pixels; 4]>,
    scrollbar_reveal_pending: bool,
    selection_reveal_pending: bool,
    _input_subscription: Subscription,
    _focus_subscription: Subscription,
    _scrollbar_subscription: Subscription,
}

/// How the palette presents results that are still loading.
///
/// A load that finishes within [`LOADING_GRACE_PERIOD`] never shows a loading state, so a fast
/// load appears fully drawn instead of flashing a short panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadingPresentation {
    /// The current results are presented.
    Settled,
    /// A load is within its grace period. A palette that has not been visible since it opened
    /// stays hidden, and no results are presented.
    Grace,
    /// The loading state is presented. It stays until [`LOADING_MINIMUM_DISPLAY`] has elapsed,
    /// even when results arrive sooner.
    Shown { minimum_elapsed: bool },
}

struct CommandPalettePanelLayout {
    width: Pixels,
    height: Pixels,
    list_height: Pixels,
    icon_offset: Pixels,
}

mod presented_results {
    use gpui::{ListOffset, Pixels, SharedString, px};

    use super::{CommandPaletteItem, CommandPaletteMatch, CommandPaletteMetrics};

    /// One presented list row. Section headings, separators, and the results note are derived,
    /// never caller-painted.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(super) enum PaletteRow {
        Section(SharedString),
        Separator,
        Item { position: usize, single_line: bool },
        Note(SharedString),
    }

    impl PaletteRow {
        pub(super) fn height(&self, metrics: CommandPaletteMetrics) -> Pixels {
            match self {
                Self::Section(_) => metrics.section_height,
                Self::Separator => metrics.separator_height,
                Self::Item {
                    single_line: true, ..
                }
                | Self::Note(_) => metrics.single_line_row_height,
                Self::Item { .. } => metrics.row_height,
            }
        }

        pub(super) const fn item_position(&self) -> Option<usize> {
            match self {
                Self::Item { position, .. } => Some(*position),
                Self::Section(_) | Self::Separator | Self::Note(_) => None,
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
            note: Option<&SharedString>,
        ) -> Self {
            let mut rows = Vec::with_capacity(matches.len() + 1);
            let mut current_section: Option<SharedString> = None;
            let mut started = false;
            for (position, matched) in matches.iter().enumerate() {
                let Some(item) = items.get(matched.item_index) else {
                    continue;
                };
                if !started || item.section != current_section {
                    if started {
                        rows.push(PaletteRow::Separator);
                    }
                    if let Some(section) = item.section.clone() {
                        rows.push(PaletteRow::Section(section));
                    }
                    current_section = item.section.clone();
                }
                started = true;
                rows.push(PaletteRow::Item {
                    position,
                    single_line: item.description.is_none(),
                });
            }
            if started && let Some(note) = note {
                rows.push(PaletteRow::Note(note.clone()));
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
            self.top_of(self.rows.len(), metrics)
        }

        /// Returns the content offset of the row at `index`, or the total height past the end.
        pub(super) fn top_of(&self, index: usize, metrics: CommandPaletteMetrics) -> Pixels {
            self.rows
                .iter()
                .take(index)
                .fold(px(0.0), |height, row| height + row.height(metrics))
        }

        /// Converts a content offset into the list position that scrolls it to the top.
        pub(super) fn offset_at(&self, y: Pixels, metrics: CommandPaletteMetrics) -> ListOffset {
            let mut row_top = px(0.0);
            for (item_ix, row) in self.rows.iter().enumerate() {
                let row_bottom = row_top + row.height(metrics);
                if y < row_bottom {
                    return ListOffset {
                        item_ix,
                        offset_in_item: (y - row_top).max(px(0.0)),
                    };
                }
                row_top = row_bottom;
            }
            ListOffset {
                item_ix: self.rows.len(),
                offset_in_item: px(0.0),
            }
        }

        pub(super) fn list_index_for_match(&self, position: usize) -> Option<usize> {
            self.rows
                .iter()
                .position(|row| row.item_position() == Some(position))
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
        let focus_subscription =
            cx.on_focus_out(&focus_scope, window, |palette, event, window, cx| {
                if palette.open
                    && palette.suspended_by_modal.is_none()
                    && !crate::menu::window_menu_is_open(window, cx)
                {
                    // Unmounting removes the focus path before GPUI clears the responder. An
                    // unchanged responder belongs to the removed palette, not a new focus owner.
                    let retired_focus = event.blurred.upgrade();
                    let removed_responder = window.is_window_active()
                        && retired_focus
                            .as_ref()
                            .is_some_and(|focus| focus.is_focused(window));
                    let predecessor = palette.restore_focus.clone();
                    let closed = palette.close(CommandPaletteCloseReason::FocusLost, window, cx)
                        || (removed_responder
                            && palette.close(CommandPaletteCloseReason::Programmatic, window, cx));
                    if closed
                        && removed_responder
                        && retired_focus.is_some_and(|focus| focus.is_focused(window))
                    {
                        if let Some(predecessor) = predecessor.and_then(|focus| focus.upgrade()) {
                            predecessor.focus(window, cx);
                        } else {
                            window.blur(cx);
                        }
                    }
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
                focus.focus(window, cx);
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
        let presented_results = Rc::new(PresentedResults::new(&items, &matches, None));
        let leading_columns = palette_leading_columns(&items);
        let list =
            ListState::new(presented_results.len(), ListAlignment::Top, px(0.0)).measure_all();
        let mut palette = Self {
            accessibility_name: placeholder.clone(),
            empty: CommandPaletteEmpty::new("No matching items"),
            items,
            matches,
            presented_results,
            leading_columns,
            input_leading_icon: None,
            query_prefix: None,
            query_note: None,
            header_actions: Vec::new(),
            primary_action: None,
            results_note: None,
            matching: CommandPaletteMatching::Semantic,
            activation: CommandPaletteActivationPolicy::Close,
            selected,
            preferred: None,
            query: String::new(),
            loading: false,
            loading_presentation: LoadingPresentation::Settled,
            loading_timer: None,
            presented: false,
            results_height: None,
            dismissible: true,
            escape_cancellable: false,
            open: false,
            pending_open: None,
            suspended_by_modal: None,
            coordinator_registration: None,
            input,
            focus_scope,
            primary_action_focus: cx.focus_handle(),
            scrollbar,
            restore_focus: None,
            restore_on_activation: None,
            pointer_press: None,
            pointer_suppressed: false,
            hover_suppressed: false,
            hovered_row: None,
            pointer_anchor: gpui::point(px(0.0), px(0.0)),
            list,
            list_row_heights: None,
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

    /// Replaces what the palette presents when a settled query has no results.
    ///
    /// Activating one of its actions emits [`CommandPaletteEvent::EmptyAction`].
    pub fn set_empty(&mut self, empty: CommandPaletteEmpty, cx: &mut gpui::Context<Self>) {
        self.empty = empty;
        cx.notify();
    }

    /// Sets the caption presented after the last result, such as a bound on listed results.
    ///
    /// The note is never selectable, and the empty state replaces it when nothing matches.
    pub fn set_results_note(&mut self, note: Option<SharedString>, cx: &mut gpui::Context<Self>) {
        if self.results_note == note {
            return;
        }
        self.results_note = note;
        self.present_results();
        cx.notify();
    }

    /// Replaces the search line's primary command.
    ///
    /// `None` leaves the confirm key for the surrounding application.
    pub fn set_primary_action(
        &mut self,
        action: Option<CommandPalettePrimaryAction>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.primary_action = action;
        cx.notify();
    }

    /// Sets the decorative icon rendered before the query with the search line's tint and size.
    pub fn set_input_leading_icon(
        &mut self,
        build: impl Fn(Rgba, Pixels) -> AnyElement + 'static,
        cx: &mut gpui::Context<Self>,
    ) {
        self.input_leading_icon = Some(Rc::new(build));
        cx.notify();
    }

    /// Replaces the quiet text that leads the query, such as the machine a path belongs to.
    ///
    /// The prefix is not part of the editable query, so editing never changes or removes it.
    pub fn set_query_prefix(&mut self, prefix: Option<SharedString>, cx: &mut gpui::Context<Self>) {
        self.query_prefix = prefix;
        cx.notify();
    }

    /// Replaces the quiet text that trails the query, such as what the primary action will do.
    pub fn set_query_note(&mut self, note: Option<SharedString>, cx: &mut gpui::Context<Self>) {
        self.query_note = note;
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
                self.input.read(cx).focus_handle().focus(window, cx);
            }
            return false;
        }
        if self.pending_open.is_some() {
            return false;
        }
        if command_palette_modal_generation(window.window_handle().window_id(), cx).is_some()
            || crate::modal::window_modal_is_open(window, cx)
        {
            let (registration, modal_suspension, inherited) =
                register_open_palette(cx.entity().downgrade(), window, cx);
            if let Some(generation) = modal_suspension {
                let replacement = replacement.or(inherited);
                let transferred_focus = replacement.is_some();
                self.coordinator_registration = Some(registration);
                self.suspended_by_modal = Some(generation);
                self.pending_open = Some(PendingCommandPaletteOpen {
                    replacement: replacement.unwrap_or_else(|| CommandPaletteReplacementFocus {
                        restore_focus: crate::modal::window_modal_predecessor_focus(window, cx),
                    }),
                    transferred_focus,
                });
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
        let replaced_combo_box = combo_replacement.is_some();
        if let Some(combo_replacement) = combo_replacement {
            cx.defer(move |cx| combo_replacement.finish(cx));
        }
        let explicit_replacement = replacement.is_some();
        self.restore_focus = match replacement {
            Some(replacement) => replacement.restore_focus,
            None if replaced_combo_box => combo_focus,
            None => match menu_replacement {
                Some(crate::menu::MenuReplacementFocus(focus)) => focus,
                None => window.focused(cx).map(|focus| focus.downgrade()),
            },
        };
        self.open = true;
        if self.coordinator_registration.is_none() {
            let (registration, modal_suspension, inherited) =
                register_open_palette(cx.entity().downgrade(), window, cx);
            self.coordinator_registration = Some(registration);
            self.suspended_by_modal = modal_suspension;
            if !explicit_replacement && let Some(inherited) = inherited {
                self.restore_focus = inherited.restore_focus;
            }
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
        self.hovered_row = None;
        self.pointer_anchor = window.mouse_position();
        self.selected = None;
        if !self.query.is_empty() {
            self.input.update(cx, |input, cx| input.set_value("", cx));
            self.query.clear();
            self.recompute_matches();
        }
        self.repair_selection();
        self.selection_reveal_pending = true;
        if self.suspended_by_modal.is_none() && !crate::modal::window_modal_is_open(window, cx) {
            self.input.read(cx).focus_handle().focus(window, cx);
        }
        if replaced_combo_box {
            let palette = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = palette.update(cx, |_, cx| {
                    cx.emit(CommandPaletteEvent::Lifecycle(
                        CommandPaletteLifecycleEvent::Opened,
                    ));
                });
            });
        } else {
            cx.emit(CommandPaletteEvent::Lifecycle(
                CommandPaletteLifecycleEvent::Opened,
            ));
        }
        self.emit_query(cx);
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
            self.input.read(cx).focus_handle().focus(window, cx);
        }
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

    /// Returns the focus owner a replacing palette inherits, once this palette has captured one.
    ///
    /// A pending open inherits the modal predecessor before the modal closes.
    fn captured_replacement_focus(&self) -> Option<CommandPaletteReplacementFocus> {
        let restore_focus = if self.open {
            self.restore_focus.clone()
        } else {
            self.pending_open
                .as_ref()?
                .replacement
                .restore_focus
                .clone()
        };
        Some(CommandPaletteReplacementFocus { restore_focus })
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected_item_id(&self) -> Option<&I> {
        self.selected.as_ref()
    }

    pub fn results_note(&self) -> Option<&SharedString> {
        self.results_note.as_ref()
    }

    /// Replaces items immediately.
    ///
    /// Semantic matching preserves selection by stable identity. Caller-ranked results treat the
    /// supplied order as authoritative and select the preferred item or first enabled item.
    pub fn set_items(&mut self, items: Vec<CommandPaletteItem<I>>, cx: &mut gpui::Context<Self>) {
        if self.matching == CommandPaletteMatching::Caller {
            self.selected = None;
        }
        self.items = unique_items(items).into();
        self.set_loading_state(false, cx);
        self.recompute_matches();
        cx.notify();
    }

    /// Sets the current loading presentation without changing items.
    pub fn set_loading(&mut self, loading: bool, cx: &mut gpui::Context<Self>) {
        if self.loading != loading {
            self.set_loading_state(loading, cx);
            cx.notify();
        }
    }

    fn set_loading_state(&mut self, loading: bool, cx: &mut gpui::Context<Self>) {
        self.loading = loading;
        match (loading, self.loading_presentation) {
            (true, LoadingPresentation::Settled) => {
                self.loading_presentation = LoadingPresentation::Grace;
                self.loading_timer = Some(cx.spawn(async move |palette, cx| {
                    cx.background_executor().timer(LOADING_GRACE_PERIOD).await;
                    let _ = palette.update(cx, |palette, cx| palette.finish_loading_grace(cx));
                }));
            }
            (
                false,
                LoadingPresentation::Grace
                | LoadingPresentation::Shown {
                    minimum_elapsed: true,
                },
            ) => self.settle_loading_presentation(),
            _ => {}
        }
    }

    fn finish_loading_grace(&mut self, cx: &mut gpui::Context<Self>) {
        if self.loading_presentation != LoadingPresentation::Grace {
            return;
        }
        self.loading_presentation = LoadingPresentation::Shown {
            minimum_elapsed: false,
        };
        self.loading_timer = Some(cx.spawn(async move |palette, cx| {
            cx.background_executor()
                .timer(LOADING_MINIMUM_DISPLAY)
                .await;
            let _ = palette.update(cx, |palette, cx| palette.finish_loading_minimum(cx));
        }));
        cx.notify();
    }

    fn finish_loading_minimum(&mut self, cx: &mut gpui::Context<Self>) {
        if !matches!(self.loading_presentation, LoadingPresentation::Shown { .. }) {
            return;
        }
        if self.loading {
            self.loading_presentation = LoadingPresentation::Shown {
                minimum_elapsed: true,
            };
            self.loading_timer = None;
        } else {
            self.settle_loading_presentation();
            cx.notify();
        }
    }

    fn settle_loading_presentation(&mut self) {
        self.loading_presentation = LoadingPresentation::Settled;
        self.loading_timer = None;
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

    /// Replaces the editor query and emits one query event.
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
        self.set_loading_state(false, cx);
        self.recompute_matches();
        self.emit_query(cx);
        cx.notify();
    }

    fn emit_query(&self, cx: &mut gpui::Context<Self>) {
        cx.emit(CommandPaletteEvent::QueryChanged(self.query.clone()));
    }

    fn recompute_matches(&mut self) {
        self.pointer_press = None;
        self.matches = match_command_palette_items(&self.items, &self.query, self.matching).into();
        self.leading_columns = palette_leading_columns(&self.items);
        self.present_results();
        self.repair_selection();
        self.selection_reveal_pending = true;
    }

    fn present_results(&mut self) {
        self.presented_results = Rc::new(PresentedResults::new(
            &self.items,
            &self.matches,
            self.results_note.as_ref(),
        ));
        self.list.reset(self.presented_results.len());
    }

    fn scrollbar_metrics(&self) -> Option<ScrollMetrics<f32>> {
        let track_height = f32::from(self.list.viewport_bounds().size.height);
        let maximum_offset = f32::from(self.list.max_offset_for_scrollbar().y);
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
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *selected)
            })
        });
        if stable {
            return;
        }
        self.selected = self.preferred.as_ref().and_then(|preferred| {
            self.matches.iter().find_map(|matched| {
                self.items.get(matched.item_index).and_then(|item| {
                    (!item.disabled && item.id == *preferred).then(|| item.id.clone())
                })
            })
        });
        if self.selected.is_none() {
            self.selected = first_enabled_id(&self.items, &self.matches);
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
        let metrics = command_palette_theme(cx).metrics;
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

    /// Scrolls the least distance that shows the selected row and a section heading directly
    /// above the first result.
    ///
    /// Positions come from the palette's exact row heights, so the reveal is correct before GPUI
    /// has measured the rows and never hides rows above a selection that already fits.
    fn reveal_selected(&mut self, metrics: CommandPaletteMetrics, list_height: Pixels) {
        let Some(position) = self.selected_match_position() else {
            return;
        };
        let Some(row) = self.presented_results.list_index_for_match(position) else {
            return;
        };
        let reveal_row = if position == 0
            && row > 0
            && matches!(
                self.presented_results.row(row - 1),
                Some(PaletteRow::Section(_))
            ) {
            row - 1
        } else {
            row
        };
        let reveal_top = self.presented_results.top_of(reveal_row, metrics);
        let row_bottom = self.presented_results.top_of(row + 1, metrics);
        let scroll_top = self.list.logical_scroll_top();
        let scrolled =
            self.presented_results.top_of(scroll_top.item_ix, metrics) + scroll_top.offset_in_item;
        let target = if reveal_top < scrolled {
            reveal_top
        } else if row_bottom > scrolled + list_height {
            row_bottom - list_height
        } else {
            return;
        };
        self.list
            .scroll_to(self.presented_results.offset_at(target, metrics));
    }

    fn enabled_match_positions(&self) -> Vec<usize> {
        self.matches
            .iter()
            .enumerate()
            .filter_map(|(position, matched)| {
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled)
                    .then_some(position)
            })
            .collect()
    }

    fn selected_match_position(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.matches.iter().position(|matched| {
            self.items
                .get(matched.item_index)
                .is_some_and(|item| item.id == *selected)
        })
    }

    fn select_match_position(&mut self, position: usize, cx: &mut gpui::Context<Self>) {
        let next = self
            .matches
            .get(position)
            .and_then(|matched| self.items.get(matched.item_index))
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

    /// Records the row the pointer entered or left, independently of keyboard selection.
    fn set_hovered_row(&mut self, id: &I, hovered: bool, cx: &mut gpui::Context<Self>) {
        if hovered {
            if self.hovered_row.as_ref() == Some(id) {
                return;
            }
            self.hovered_row = Some(id.clone());
        } else if self.hovered_row.as_ref() == Some(id) {
            self.hovered_row = None;
        } else {
            return;
        }
        cx.notify();
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
            self.items
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
                self.items
                    .get(matched.item_index)
                    .is_some_and(|item| !item.disabled && item.id == *id)
            })
    }

    fn focus_next_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_next(cx);
        if !self.focus_scope.contains_focused(window, cx) {
            self.input.read(cx).focus_handle().focus(window, cx);
        }
        cx.stop_propagation();
    }

    fn focus_previous_control(&self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        window.focus_prev(cx);
        if self.focus_scope.contains_focused(window, cx) {
            cx.stop_propagation();
            return;
        }

        let input_focus = self.input.read(cx).focus_handle();
        input_focus.focus(window, cx);
        let mut last_internal = input_focus;
        let maximum_steps = self.header_actions.len()
            + self
                .primary_action
                .as_ref()
                .map_or(0, |action| 1 + usize::from(!action.menu_items.is_empty()))
            + self.presented_empty_actions().len();
        for _ in 0..maximum_steps {
            window.focus_next(cx);
            if !self.focus_scope.contains_focused(window, cx) {
                last_internal.focus(window, cx);
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
        if self.loading_presentation != LoadingPresentation::Settled {
            return;
        }
        if self.matches.is_empty() {
            if let Some(action) = self.empty.default_action() {
                cx.emit(CommandPaletteEvent::EmptyAction(action.id.clone()));
            } else {
                self.activate_primary_action(cx);
            }
            return;
        }
        let Some(item_id) = self.selected.clone() else {
            self.activate_primary_action(cx);
            return;
        };
        self.activate_item(item_id, source, window, cx);
    }

    /// Availability belongs to the action's disabled state, so pending results never block it.
    fn activate_primary_action(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(action) = self
            .primary_action
            .as_ref()
            .filter(|action| !action.disabled)
        {
            cx.emit(CommandPaletteEvent::HeaderAction(action.id.clone()));
        }
    }

    fn activate_item(
        &mut self,
        item_id: I,
        source: CommandPaletteActivationSource,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let enabled = self.matches.iter().any(|matched| {
            self.items
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
            .is_some_and(|pending| pending.transferred_focus);
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
        if let Some(mut pending) = self.pending_open.take() {
            if !pending.transferred_focus
                && let Some(target) = crate::modal::take_window_modal_focus_restoration(window, cx)
            {
                pending.replacement.restore_focus = Some(target);
            }
            return self.finish_open(Some(pending.replacement), window, cx);
        }
        if !self.open {
            return false;
        }
        self.pointer_press = None;
        self.pointer_suppressed = true;
        self.hover_suppressed = true;
        self.hovered_row = None;
        self.pointer_anchor = window.mouse_position();
        self.input.read(cx).focus_handle().focus(window, cx);
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
        self.settle_loading_presentation();
        self.presented = false;
        self.results_height = None;
        self.pointer_press = None;
        self.pointer_suppressed = false;
        self.hover_suppressed = false;
        self.hovered_row = None;
        self.scrollbar
            .update(cx, |scrollbar, cx| scrollbar.reset(cx));
        let restore_focus = self.restore_focus.take();
        if reason == CommandPaletteCloseReason::Deactivated {
            self.restore_on_activation = restore_focus;
        } else {
            self.restore_on_activation = None;
            if reason.restores_focus() {
                if let Some(focus) = restore_focus.and_then(|focus| focus.upgrade()) {
                    focus.focus(window, cx);
                } else if self.focus_scope.contains_focused(window, cx) {
                    window.blur(cx);
                }
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
            .filter(|item| !item.disabled && item.default_selectable)
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
        crate::modal::present_modal_transient();
        // A disabled button gives up focus. The query takes it, so the palette's keys, such as
        // Escape during a cancellable operation, keep reaching the palette.
        if self
            .primary_action
            .as_ref()
            .is_some_and(|action| action.disabled)
            && self.primary_action_focus.is_focused(window)
        {
            self.input.read(cx).focus_handle().focus(window, cx);
        }
        let theme = command_palette_theme(cx);
        let collection_focused = self.focus_scope.contains_focused(window, cx);
        let metrics = theme.metrics;
        let typography = crate::control_typography(cx);
        let font = typography.regular().clone();
        if std::mem::take(&mut self.scrollbar_reveal_pending) {
            self.reveal_scrollbar(cx);
        } else {
            self.sync_scrollbar(cx);
        }
        let row_heights = [
            metrics.single_line_row_height,
            metrics.row_height,
            metrics.section_height,
            metrics.separator_height,
        ];
        if self
            .list_row_heights
            .replace(row_heights)
            .is_some_and(|previous| previous != row_heights)
        {
            // GPUI retains offscreen heights until reset, even when the list width is unchanged.
            let scroll_top = self.list.logical_scroll_top();
            self.list.reset(self.presented_results.len());
            self.list.scroll_to(scroll_top);
            // Synchronize the scrollbar once the current layout has remeasured the list.
            let palette = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = palette.update(cx, |_, cx| cx.notify());
            });
        }
        let content_viewport = crate::content_viewport(window);
        let viewport = content_viewport.size;
        let available_width = (viewport.width - metrics.viewport_margin * 2.0).max(px(0.0));
        let panel_width = metrics.panel_width.min(available_width);
        let left = ((viewport.width - panel_width) / 2.0).max(px(0.0));
        let top = metrics
            .top_offset
            .min((viewport.height - metrics.viewport_margin).max(px(0.0)));

        let panel_visible = self.panel_is_visible();
        self.presented |= panel_visible;
        // A reload keeps the results area at its presented height, growing only to fit the
        // loading state once it shows.
        let content_height = match self.loading_presentation {
            LoadingPresentation::Settled if self.matches.is_empty() => {
                self.empty_state_height(panel_width, metrics, &font, window, cx)
            }
            LoadingPresentation::Settled => self.presented_results.total_height(metrics),
            LoadingPresentation::Grace => self
                .results_height
                .unwrap_or_else(|| loading_state_height(metrics, cx)),
            LoadingPresentation::Shown { .. } => {
                let loading_height = loading_state_height(metrics, cx);
                self.results_height
                    .map_or(loading_height, |height| height.max(loading_height))
            }
        };
        let chrome_height = chrome_height(metrics);
        let available_height = (viewport.height - top - metrics.viewport_margin).max(px(0.0));
        let panel_height = (chrome_height + content_height)
            .min(metrics.maximum_height)
            .min(available_height);
        let list_height = (panel_height - chrome_height).max(px(0.0));
        let mut fitted = px(0.0);
        if self.loading_presentation == LoadingPresentation::Settled && !self.matches.is_empty() {
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
        if std::mem::take(&mut self.selection_reveal_pending) {
            self.reveal_selected(metrics, list_height);
        }
        if panel_visible && self.loading_presentation == LoadingPresentation::Settled {
            self.results_height = Some(list_height);
        }
        let panel_height = chrome_height + list_height;

        let panel_bounds = gpui::Bounds::new(
            gpui::point(left, top) + content_viewport.origin,
            gpui::size(panel_width, panel_height),
        );
        let outside = self.render_outside_tracker(panel_bounds, cx);
        let icon_offset = crate::icon::text_alignment_offset(
            typography.regular(),
            metrics.label_size,
            metrics.body_line_height,
            metrics.icon_baseline_center,
            window,
        );
        let panel = self.render_panel(
            CommandPalettePanelLayout {
                width: panel_width,
                height: panel_height,
                list_height,
                icon_offset,
            },
            theme,
            typography,
            collection_focused,
            cx,
        );
        let overlay = div()
            .relative()
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .key_context(KEY_CONTEXT)
            .font(font)
            .line_height(metrics.body_line_height)
            .track_focus(&self.focus_scope)
            .tab_group()
            .child(outside)
            .child(
                div()
                    .absolute()
                    .left(panel_bounds.origin.x)
                    .top(panel_bounds.origin.y)
                    // A hidden panel keeps its focus and key handling, so typing is not lost.
                    .when(!panel_visible, |panel| panel.opacity(0.0))
                    .child(panel),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
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
            .when(self.primary_action.is_some(), |overlay| {
                overlay.on_action(cx.listener(|palette, _: &Confirm, _, cx| {
                    palette.activate_primary_action(cx);
                    cx.stop_propagation();
                }))
            })
            .on_action(cx.listener(|palette, _: &FocusNext, window, cx| {
                palette.focus_next_control(window, cx);
            }))
            .on_action(cx.listener(|palette, _: &FocusPrevious, window, cx| {
                palette.focus_previous_control(window, cx);
            }));

        // The query field can open its context menu, and GPUI collects deferred draws once per
        // frame, so the palette draws normally and lets that menu defer above it. Its owner
        // renders it last, and the anchored full-window layer keeps it above the chrome.
        crate::floating_surface::present(
            theme.shell.layer(true),
            anchored()
                .anchor(Anchor::TopLeft)
                .position(gpui::point(px(0.0), px(0.0)))
                .snap_to_window()
                .child(overlay),
        )
    }
}

impl<I: Clone + Eq + 'static> CommandPalette<I> {
    fn panel_is_visible(&self) -> bool {
        self.presented || self.loading_presentation != LoadingPresentation::Grace
    }

    /// Returns the empty-state actions while the empty state is the presented content.
    fn presented_empty_actions(&self) -> &[CommandPaletteEmptyAction] {
        if self.loading_presentation != LoadingPresentation::Settled || !self.matches.is_empty() {
            return &[];
        }
        &self.empty.actions
    }

    /// Measures the empty state so the panel fits it exactly, including a wrapped description.
    fn empty_state_height(
        &self,
        panel_width: Pixels,
        metrics: CommandPaletteMetrics,
        font: &gpui::Font,
        window: &Window,
        cx: &App,
    ) -> Pixels {
        let mut height = metrics.empty_padding * 2.0 + metrics.body_line_height;
        if self.empty.icon.is_some() {
            height += metrics.empty_icon_size() + metrics.empty_line_gap * 2.0;
        }
        if let Some(description) = &self.empty.description {
            let wrap_width = (panel_width - metrics.content_leading_inset() * 2.0).max(px(1.0));
            let lines = wrapped_line_count(
                description,
                font,
                metrics.secondary_size,
                wrap_width,
                window,
            );
            height += metrics.empty_line_gap + metrics.secondary_line_height * lines as f32;
        }
        if !self.empty.actions.is_empty() {
            height += metrics.empty_actions_gap
                + crate::ControlHost::Floating
                    .button_theme(cx)
                    .control_height(EMPTY_ACTION_SIZE);
        }
        height
    }

    /// Renders the caller's title, description, and actions centered in the result area.
    fn render_empty_state(
        &self,
        theme: CommandPaletteTheme,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let metrics = theme.metrics;
        let paint = theme.paint;
        let palette = cx.entity().downgrade();
        div()
            .debug_selector(|| "command-palette-empty".to_owned())
            .w_full()
            .px(metrics.content_leading_inset())
            .py(metrics.empty_padding)
            .flex()
            .flex_col()
            .items_center()
            .when_some(self.empty.icon.as_ref(), |empty, icon| {
                let size = metrics.empty_icon_size();
                empty.child(
                    div()
                        .size(size)
                        .flex_shrink_0()
                        .mb(metrics.empty_line_gap * 2.0)
                        .child(icon(paint.muted, size)),
                )
            })
            .child(
                div()
                    .debug_selector(|| "command-palette-empty-title".to_owned())
                    .w_full()
                    .text_center()
                    .truncate()
                    .text_size(metrics.label_size)
                    .line_height(metrics.body_line_height)
                    .text_color(paint.foreground)
                    .child(self.empty.title.clone()),
            )
            .when_some(self.empty.description.clone(), |empty, description| {
                empty.child(
                    div()
                        .debug_selector(|| "command-palette-empty-description".to_owned())
                        .w_full()
                        .mt(metrics.empty_line_gap)
                        .text_center()
                        .line_clamp(EMPTY_DESCRIPTION_LINE_LIMIT)
                        .text_size(metrics.secondary_size)
                        .line_height(metrics.secondary_line_height)
                        .text_color(paint.muted)
                        .child(description),
                )
            })
            .when(!self.empty.actions.is_empty(), |empty| {
                empty.child(
                    div()
                        .mt(metrics.empty_actions_gap)
                        .flex()
                        .flex_row()
                        .justify_center()
                        .gap(metrics.gap)
                        .children(
                            self.empty
                                .actions
                                .iter()
                                .enumerate()
                                .map(|(index, action)| {
                                    render_empty_action(palette.clone(), index, action)
                                }),
                        ),
                )
            })
            .into_any_element()
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
        layout: CommandPalettePanelLayout,
        theme: CommandPaletteTheme,
        typography: crate::ControlTypography,
        collection_focused: bool,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let CommandPalettePanelLayout {
            width,
            height,
            list_height,
            icon_offset,
        } = layout;
        let paint = theme.paint;
        let metrics = theme.metrics;
        let content = if self.loading_presentation == LoadingPresentation::Grace {
            div().into_any_element()
        } else if matches!(self.loading_presentation, LoadingPresentation::Shown { .. }) {
            loading_state(list_height, metrics, paint).into_any_element()
        } else if self.matches.is_empty() {
            self.render_empty_state(theme, cx)
        } else {
            self.render_results(
                list_height,
                theme,
                typography,
                icon_offset,
                collection_focused,
                cx,
            )
        };

        let panel = div()
            .id("command-palette")
            .role(accesskit::Role::Dialog)
            .aria_label(self.accessibility_name.clone())
            .aria_modal(true)
            .debug_selector(|| "command-palette-panel".to_owned())
            .w(width)
            .h(height)
            .flex()
            .flex_col()
            .block_mouse_except_scroll()
            .child(self.render_editor(theme, cx))
            .child(separator_line(theme))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .py(metrics.panel_padding)
                    .child(content),
            );
        theme.shell.mount(panel).into_any_element()
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
            .pl(if self.input_leading_icon.is_some() {
                metrics.panel_padding
            } else {
                metrics.content_leading_inset()
            })
            .pr(metrics.panel_padding)
            .flex()
            .flex_row()
            .items_center()
            .gap(metrics.panel_padding)
            .text_size(metrics.input_size)
            .line_height(metrics.body_line_height)
            .when_some(self.input_leading_icon.as_ref(), |editor, icon| {
                editor.child(
                    div()
                        .size(metrics.header_action_size())
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(theme.paint.muted, metrics.input_icon_size)),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .when_some(self.query_prefix.clone(), |query, prefix| {
                        query.child(
                            div()
                                .debug_selector(|| "command-palette-query-prefix".to_owned())
                                .flex_shrink_0()
                                .max_w(gpui::relative(0.4))
                                .truncate()
                                .text_color(theme.paint.muted)
                                .child(prefix),
                        )
                    })
                    .child(div().min_w_0().flex_1().child(self.input.clone())),
            )
            .when_some(self.query_note.clone(), |editor, note| {
                editor.child(
                    div()
                        .debug_selector(|| "command-palette-query-note".to_owned())
                        .flex_shrink_0()
                        .max_w(gpui::relative(0.4))
                        .truncate()
                        .text_color(theme.paint.muted)
                        .child(note),
                )
            })
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
                                    render_header_action(palette.clone(), index, action, theme)
                                }),
                        ),
                )
            })
            .when_some(self.primary_action.as_ref(), |editor, action| {
                editor.child(render_primary_action(
                    palette.clone(),
                    action,
                    self.primary_action_focus.clone(),
                ))
            })
            .into_any_element()
    }

    fn render_results(
        &self,
        list_height: Pixels,
        theme: CommandPaletteTheme,
        typography: crate::ControlTypography,
        icon_offset: Pixels,
        collection_focused: bool,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let items = Rc::clone(&self.items);
        let matches = Rc::clone(&self.matches);
        let presented_results = Rc::clone(&self.presented_results);
        let selected = self.selected.clone();
        // Keyboard navigation and wheel scrolling park the pointer, so a stationary pointer does
        // not keep claiming the row it happens to rest over.
        let hover_suppressed = self.pointer_suppressed || self.hover_suppressed;
        let hovered = self.hovered_row.clone().filter(|_| !hover_suppressed);
        let leading_columns = self.leading_columns;
        let palette = cx.entity().downgrade();
        div()
            .id("command-palette-results")
            .role(accesskit::Role::ListBox)
            .aria_label(self.accessibility_name.clone())
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
                            let font = typography.section_font().clone();
                            render_section(label.clone(), row_height, theme, font)
                                .into_any_element()
                        }
                        PaletteRow::Separator => {
                            render_row_separator(row_height, theme).into_any_element()
                        }
                        PaletteRow::Note(note) => {
                            let font = typography.section_font().clone();
                            render_results_note(note.clone(), row_height, theme, font)
                                .into_any_element()
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
                                        hovered.as_ref() == Some(&item.id),
                                        leading_columns,
                                        row_height,
                                        theme,
                                        typography.regular().clone(),
                                        icon_offset,
                                        collection_focused,
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
}

fn chrome_height(metrics: CommandPaletteMetrics) -> Pixels {
    metrics.panel_padding * 2.0 + metrics.input_height + metrics.border_width * 3.0
}

fn separator_line(theme: CommandPaletteTheme) -> impl IntoElement {
    div()
        .w_full()
        .h(theme.shell.hairline())
        .flex_shrink_0()
        .bg(theme.shell.divider())
}

/// Counts the lines `text` occupies at `wrap_width`, bounded by the empty-state line limit.
fn wrapped_line_count(
    text: &SharedString,
    font: &gpui::Font,
    font_size: Pixels,
    wrap_width: Pixels,
    window: &Window,
) -> usize {
    let run = gpui::TextRun {
        len: text.len(),
        font: font.clone(),
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(
            text.clone(),
            font_size,
            &[run],
            Some(wrap_width),
            Some(EMPTY_DESCRIPTION_LINE_LIMIT),
        )
        .map(|lines| {
            lines
                .iter()
                .map(|line| line.wrap_boundaries().len() + 1)
                .sum::<usize>()
        })
        .unwrap_or(1)
        .clamp(1, EMPTY_DESCRIPTION_LINE_LIMIT)
}

/// Renders one empty-state action. The first action carries the default emphasis.
fn render_empty_action<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    index: usize,
    action: &CommandPaletteEmptyAction,
) -> AnyElement {
    let id = action.id.clone();
    Button::new(
        ("command-palette-empty-action", index),
        action.label.clone(),
    )
    .variant(if index == 0 {
        ButtonVariant::Primary
    } else {
        ButtonVariant::Secondary
    })
    .size(EMPTY_ACTION_SIZE)
    .disabled(action.disabled)
    .tab_stop(true)
    .when_some(action.debug_selector.clone(), |button, selector| {
        button.debug_selector(selector)
    })
    .on_activate(move |_, _, cx| {
        let id = id.clone();
        let _ = palette.update(cx, |_, cx| {
            cx.emit(CommandPaletteEvent::<I>::EmptyAction(id));
        });
    })
    .into_any_element()
}

/// Renders one trailing search-line control as a ghost icon button.
fn render_header_action<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    index: usize,
    action: &CommandPaletteAction,
    theme: CommandPaletteTheme,
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
    .target_size(theme.metrics.header_action_size())
    .corner_radius(theme.shell.nested_radius())
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

/// Renders the search line's primary command as a prominent capsule at its trailing edge, joined
/// by a menu segment when it has related commands.
fn render_primary_action<I: Clone + Eq + 'static>(
    palette: WeakEntity<CommandPalette<I>>,
    action: &CommandPalettePrimaryAction,
    focus_handle: gpui::FocusHandle,
) -> AnyElement {
    let id = action.id.clone();
    let menu_palette = palette.clone();
    let entries = action
        .menu_items
        .iter()
        .map(|(id, label)| {
            MenuEntry::action(label.clone(), id.clone())
                .debug_selector(format!("command-palette-primary-menu-{id}"))
        })
        .collect();
    ComboButton::new(
        "command-palette-primary-action",
        action.label.clone(),
        entries,
    )
    .variant(ButtonVariant::Primary)
    .size(ButtonSize::Small)
    .when_some(action.shortcut.clone(), |button, shortcut| {
        button.shortcut(shortcut)
    })
    .disabled(action.disabled)
    .menu_disabled(action.menu_disabled)
    .focus_handle(focus_handle)
    .tab_stop(true)
    .placement(MenuPlacementConfig::new(
        MenuPlacement::Bottom,
        MenuAlignment::End,
    ))
    .when_some(action.debug_selector.clone(), |button, selector| {
        button.debug_selector(selector)
    })
    .on_activate(move |_, _, cx| {
        let id = id.clone();
        let _ = palette.update(cx, |_, cx| {
            cx.emit(CommandPaletteEvent::<I>::HeaderAction(id));
        });
    })
    .on_menu_activate(move |activation, _, cx| {
        let id = activation.action().clone();
        let _ = menu_palette.update(cx, |_, cx| {
            cx.emit(CommandPaletteEvent::<I>::HeaderAction(id));
        });
    })
    .into_any_element()
}

fn render_section(
    label: SharedString,
    height: Pixels,
    theme: CommandPaletteTheme,
    font: gpui::Font,
) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .w_full()
        .h(height)
        .pl(metrics.content_leading_inset())
        .flex()
        .items_center()
        .text_size(metrics.section_size)
        .line_height(metrics.section_line_height)
        .font(font)
        .text_color(theme.paint.section_foreground)
        .child(label)
}

/// Sets the note in caption type on the row grid, aligned with row labels.
fn render_results_note(
    note: SharedString,
    height: Pixels,
    theme: CommandPaletteTheme,
    font: gpui::Font,
) -> impl IntoElement {
    let metrics = theme.metrics;
    div()
        .debug_selector(|| "command-palette-results-note".to_owned())
        .w_full()
        .h(height)
        .pl(metrics.content_leading_inset())
        .pr(metrics.panel_padding)
        .flex()
        .items_center()
        .overflow_hidden()
        .text_size(metrics.section_size)
        .line_height(metrics.section_line_height)
        .font(font)
        .text_color(theme.paint.muted)
        .child(div().min_w_0().truncate().child(note))
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
                .h(theme.shell.hairline())
                .bg(theme.shell.divider()),
        )
}

/// Measures the loading state: its text above a bar, with the empty state's padding and gap.
fn loading_state_height(metrics: CommandPaletteMetrics, cx: &App) -> Pixels {
    metrics.empty_padding * 2.0
        + metrics.secondary_line_height
        + metrics.empty_line_gap * 2.0
        + ProgressBar::thickness(ProgressSize::Regular, cx)
}

/// Renders the state shown while a caller's results are still arriving: muted text above an
/// indeterminate bar, centered in the results area. The bar is named by the visible text.
fn loading_state(
    height: Pixels,
    metrics: CommandPaletteMetrics,
    paint: CommandPalettePaint,
) -> impl IntoElement {
    div()
        .debug_selector(|| "command-palette-loading".to_owned())
        .w_full()
        .h(height)
        .px(metrics.content_leading_inset())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(metrics.empty_line_gap * 2.0)
        .child(
            div()
                .debug_selector(|| "command-palette-loading-label".to_owned())
                .text_size(metrics.secondary_size)
                .line_height(metrics.secondary_line_height)
                .text_color(paint.muted)
                .child(LOADING_TEXT),
        )
        .child(
            div().w(metrics.loading_bar_width).max_w_full().child(
                ProgressBar::new(
                    "command-palette-loading-progress",
                    LOADING_TEXT,
                    ProgressState::Indeterminate,
                )
                .size(ProgressSize::Regular)
                .debug_selector("command-palette-loading-progress"),
            ),
        )
}

/// Palette rows are commands and never carry a checkmark. They reserve the icon column when one
/// presented row has an icon.
fn palette_leading_columns<I>(items: &[CommandPaletteItem<I>]) -> LeadingColumns {
    LeadingColumns::new(false, items.iter().any(|item| item.leading_icon.is_some()))
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
    hovered: bool,
    leading_columns: LeadingColumns,
    height: Pixels,
    theme: CommandPaletteTheme,
    label_font: gpui::Font,
    icon_offset: Pixels,
    collection_focused: bool,
) -> AnyElement {
    let paint = theme.paint;
    let metrics = theme.metrics;
    let row_paint = paint.row_paint(item.disabled, selected, hovered, collection_focused);
    let foreground = row_paint.foreground;
    let secondary = row_paint.secondary;
    let match_foreground = row_paint.matched;
    let logical_name = item.label.clone();
    let debug_selector = item.debug_selector.clone();
    let id = item.id.clone();
    let hover_palette = palette.clone();
    let press_palette = palette.clone();
    let mut row = div()
        .id(("command-palette-row", position))
        .role(accesskit::Role::ListBoxOption)
        .aria_label(item.label.clone())
        .when_some(item.description.clone(), |row, description| {
            row.aria_description(description)
        })
        .aria_selected(selected)
        .aria_disabled(item.disabled)
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
        .bg(row_paint.background)
        .border(theme.shell.hairline())
        .border_color(row_paint.border)
        .when(!item.disabled, |row| {
            let id = id.clone();
            let entered_id = id.clone();
            let entered_palette = hover_palette.clone();
            let press_id = id.clone();
            row.on_a11y_action(accesskit::Action::Click, move |_, window, cx| {
                let _ = press_palette.update(cx, |palette, cx| {
                    palette.selected = Some(press_id.clone());
                    palette.activate_selected(
                        CommandPaletteActivationSource::Accessibility,
                        window,
                        cx,
                    );
                });
            })
            .on_hover(move |hovered, _, cx| {
                let _ = entered_palette.update(cx, |palette, cx| {
                    palette.set_hovered_row(&entered_id, *hovered, cx);
                });
            })
            .on_mouse_move(move |event, _, cx| {
                let _ = hover_palette.update(cx, |palette, cx| {
                    palette.pointer_hover(&id, event.position, cx)
                });
            })
        });

    let icon = item
        .leading_icon
        .clone()
        .map(|icon| icon(row_paint.icon, metrics.icon_size));
    let leading = leading_columns
        .render(metrics.leading_column_metrics(), None, icon)
        .map(|columns| columns.relative().top(icon_offset));
    row = row.children(leading);

    let label_line = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(metrics.gap)
        .child(
            div()
                .min_w_0()
                .flex_1()
                .font(label_font)
                .child(highlighted_text(
                    item.label.clone(),
                    label_highlights,
                    foreground,
                    match_foreground,
                    metrics.label_size,
                )),
        )
        .when_some(item.trailing.clone(), |line, accessory| {
            line.child(render_accessory(accessory, secondary, metrics, icon_offset))
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
                    .line_height(metrics.secondary_line_height)
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
                            || !crate::PointerConventions::get(cx)
                                .primary(event.button, event.modifiers)
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
                        let primary = crate::PointerConventions::get(cx)
                            .primary(event.button, event.modifiers);
                        let activate = up_palette
                            .update(cx, |palette, _| {
                                palette.pointer_up(&up_id, primary && inside)
                            })
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
    metrics: CommandPaletteMetrics,
    icon_offset: Pixels,
) -> AnyElement {
    match accessory {
        CommandPaletteAccessory::Text(text) => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .line_height(metrics.secondary_line_height)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Shortcut(shortcut) => div()
            .flex_shrink_0()
            .text_size(metrics.secondary_size)
            .line_height(metrics.secondary_line_height)
            .text_color(color)
            .child(crate::ShortcutLabel::new(shortcut))
            .into_any_element(),
        CommandPaletteAccessory::Status(text) => div()
            .flex_shrink_0()
            .px(metrics.accessory_padding)
            .py(metrics.accessory_line_padding)
            .rounded(metrics.accessory_radius)
            .text_size(metrics.secondary_size)
            .line_height(metrics.secondary_line_height)
            .text_color(color)
            .child(text)
            .into_any_element(),
        CommandPaletteAccessory::Checkmark => div()
            .relative()
            .top(icon_offset)
            .child(Icon::new(IconName::Check, metrics.icon_size, color))
            .into_any_element(),
    }
}

#[cfg(test)]
#[path = "command_palette_tests.rs"]
mod tests;
