use std::time::Duration;

use gpui::{App, Global, Pixels, px};

use super::{
    MAX_ACTION_DEBUG_IDENTITY_CHARACTERS, MAX_ACTION_LABEL_CHARACTERS, MAX_MODAL_ID_CHARACTERS,
    MAX_MODAL_TITLE_CHARACTERS, ModalAction, ModalActionIntent, ModalActionRole, ModalMetrics,
    ModalTextField, ModalValidationError,
    alert::{
        Alert, MAX_ALERT_DETAIL_CHARACTERS, MAX_ALERT_MESSAGE_CHARACTERS, validate_accessory,
        validate_suppression,
    },
    dialog::{Dialog, DialogInitialFocus, validate_description},
    progress_dialog::{ProgressCancellation, ProgressDialog, validate_detail, validate_status},
    validate_bounded_text, validate_required_text,
};

const MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Logical text and control direction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextDirection {
    /// Leading is physically left and trailing is physically right.
    #[default]
    LeftToRight,
    /// Leading is physically right and trailing is physically left.
    RightToLeft,
}

/// Direction-independent edge used by desktop placement policy tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalEdge {
    /// Start edge in the current logical direction.
    Leading,
    /// End edge in the current logical direction.
    Trailing,
}

/// Resolved viewport edge used by desktop placement policy tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalEdge {
    /// Physical left edge.
    Left,
    /// Physical right edge.
    Right,
}

/// Adaptive action-area direction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionAxis {
    /// Actions fit in one logical leading-to-trailing row.
    Horizontal,
    /// Actions stack so every localized label remains reachable.
    Vertical,
}

/// Pure policy-selected entry focus before the renderer resolves live containment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalInitialFocus {
    /// Focus the action at this unchanged caller index.
    Action(usize),
    /// Focus the modal surface when no safe action is available.
    Surface,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DesktopShape {
    MacOs,
    #[cfg(test)]
    WinUi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DefaultActionPresentation {
    None,
    Emphasized,
}

pub(super) trait ArrangementActionFacts {
    fn arrangement_role(&self) -> ModalActionRole;
    fn arrangement_is_default(&self) -> bool;
}

impl<A> ArrangementActionFacts for ModalAction<A> {
    fn arrangement_role(&self) -> ModalActionRole {
        self.role()
    }

    fn arrangement_is_default(&self) -> bool {
        self.is_default()
    }
}

impl ArrangementActionFacts for super::core::ModalRenderAction {
    fn arrangement_role(&self) -> ModalActionRole {
        self.role
    }

    fn arrangement_is_default(&self) -> bool {
        self.is_default
    }
}

/// Immutable installed logical desktop policy, separate from paint and metrics.
///
/// Policy validates action identity, role, intent, enabled/default facts, facade-specific safe
/// dismissal, Alert bounds, and programmatic-only deadlines. It also selects initial focus, logical
/// leading/trailing action placement, right-to-left mirroring, and adaptive action axis.
/// Callers keep typed identity and logical order; physical order never changes result identity.
///
/// The application must explicitly install one policy with [`install_modal_policy`]. Production
/// uses [`Self::mac_os`]. An alternate WinUI-shaped profile exists only inside pure tests to prove
/// that public semantics do not depend on macOS ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModalDesktopPolicy {
    shape: DesktopShape,
    text_direction: TextDirection,
    maximum_programmatic_only_deadline: Duration,
}

impl ModalDesktopPolicy {
    /// Returns the macOS logical policy used by production application installation.
    ///
    /// In horizontal left-to-right rows Cancel is leading and the explicit default is trailing;
    /// right-to-left layout mirrors physical placement while retaining logical traversal and typed
    /// identity. Return activates only an explicit enabled default, and cancellation actions route
    /// only to the enabled safe Cancel path. The separately installed macOS modal keybinding
    /// profile adds Command-Period. Programmatic-only progress is capped at thirty minutes.
    pub const fn mac_os() -> Self {
        Self {
            shape: DesktopShape::MacOs,
            text_direction: TextDirection::LeftToRight,
            maximum_programmatic_only_deadline: MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE,
        }
    }

    /// Returns this desktop policy with the installed locale's logical direction.
    ///
    /// The Operating-System Window modal root consumes this fact automatically. Individual modal
    /// call sites do not select direction.
    pub const fn with_text_direction(mut self, direction: TextDirection) -> Self {
        self.text_direction = direction;
        self
    }

    pub(super) const fn text_direction(self) -> TextDirection {
        self.text_direction
    }

    #[cfg(test)]
    pub(super) const fn win_ui_for_tests() -> Self {
        Self {
            shape: DesktopShape::WinUi,
            text_direction: TextDirection::LeftToRight,
            maximum_programmatic_only_deadline: MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE,
        }
    }

    pub(super) fn validate_alert<A: Eq>(
        &self,
        alert: &Alert<A>,
    ) -> Result<(), ModalValidationError> {
        validate_modal_header(&alert.id, &alert.accessibility_title, &alert.title)?;
        validate_required_text(&alert.message, ModalTextField::AlertMessage)?;
        validate_bounded_text(
            &alert.message,
            ModalTextField::AlertMessage,
            MAX_ALERT_MESSAGE_CHARACTERS,
        )?;
        if let Some(detail) = &alert.detail {
            validate_bounded_text(detail, ModalTextField::Detail, MAX_ALERT_DETAIL_CHARACTERS)?;
        }
        if let Some(accessory) = &alert.accessory {
            validate_accessory(accessory)?;
        }
        if let Some(suppression) = &alert.suppression {
            validate_suppression(suppression)?;
        }
        if !(1..=3).contains(&alert.actions.len()) {
            return Err(ModalValidationError::AlertDecisionCount {
                count: alert.actions.len(),
            });
        }
        if let Some(index) = alert
            .actions
            .iter()
            .position(|action| action.role() == ModalActionRole::Help)
        {
            return Err(ModalValidationError::AlertHelpMustBeSeparate { index });
        }
        if let Some(help) = &alert.help
            && help.role() != ModalActionRole::Help
        {
            return Err(ModalValidationError::InvalidHelpActionRole);
        }

        let mut all_actions = alert.actions.iter().collect::<Vec<_>>();
        if let Some(help) = &alert.help {
            all_actions.push(help);
        }
        let facts = validate_actions(&all_actions)?;
        validate_alert_dismissal(&alert.actions, facts.safe_cancel)?;
        self.alert_initial_focus(&alert.actions)
            .ok_or(ModalValidationError::MissingSafeDismissal)?;
        Ok(())
    }

    pub(super) fn validate_dialog<A: Eq>(
        &self,
        dialog: &Dialog<A>,
    ) -> Result<(), ModalValidationError> {
        validate_modal_header(&dialog.id, &dialog.accessibility_title, &dialog.title)?;
        if let Some(description) = &dialog.description {
            validate_description(description)?;
        }
        let actions = dialog.actions.iter().collect::<Vec<_>>();
        let facts = validate_actions(&actions)?;
        validate_dialog_dismissal(&dialog.actions, facts.safe_cancel)?;

        if let DialogInitialFocus::Action(action_id) = &dialog.initial_focus {
            let action = dialog
                .actions
                .iter()
                .find(|action| action.id() == action_id);
            if !action.is_some_and(|action| action.is_enabled()) {
                return Err(ModalValidationError::InvalidDialogInitialFocus);
            }
        }
        Ok(())
    }

    pub(super) fn validate_progress_dialog<A: Eq>(
        &self,
        progress: &ProgressDialog<A>,
    ) -> Result<(), ModalValidationError> {
        validate_modal_header(&progress.id, &progress.accessibility_title, &progress.title)?;
        validate_status(&progress.status)?;
        if let Some(detail) = &progress.detail {
            validate_detail(detail)?;
        }
        match &progress.cancellation {
            ProgressCancellation::Cancellable(action) => {
                let actions = [action];
                validate_actions(&actions)?;
                if !is_cancel_capability(action.role(), action.intent()) || action.is_default() {
                    return Err(ModalValidationError::InvalidProgressCancelAction);
                }
            }
            ProgressCancellation::ProgrammaticOnly { deadline } => {
                if deadline.is_zero() || *deadline > self.maximum_programmatic_only_deadline {
                    return Err(ModalValidationError::InvalidProgrammaticOnlyDeadline {
                        deadline: *deadline,
                        maximum: self.maximum_programmatic_only_deadline,
                    });
                }
            }
        }
        Ok(())
    }

    pub(super) fn action_arrangement<A: ArrangementActionFacts>(
        &self,
        actions: &[A],
        axis: ActionAxis,
    ) -> ActionArrangement {
        let row = actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                (action.arrangement_role() != ModalActionRole::Help).then_some(index)
            })
            .collect::<Vec<_>>();
        let default = row
            .iter()
            .copied()
            .find(|index| actions[*index].arrangement_is_default());
        let cancel = row
            .iter()
            .copied()
            .find(|index| actions[*index].arrangement_role() == ModalActionRole::Cancel);
        let traversal = match (self.shape, axis) {
            (DesktopShape::MacOs, ActionAxis::Horizontal) => ordered_indices(
                cancel,
                row.iter()
                    .copied()
                    .filter(|index| Some(*index) != cancel && Some(*index) != default),
                default,
            ),
            (DesktopShape::MacOs, ActionAxis::Vertical) => ordered_indices(
                default,
                row.iter()
                    .copied()
                    .filter(|index| Some(*index) != default && Some(*index) != cancel),
                cancel,
            ),
            #[cfg(test)]
            (DesktopShape::WinUi, _) => ordered_indices(
                default,
                row.iter()
                    .copied()
                    .filter(|index| Some(*index) != default && Some(*index) != cancel),
                cancel,
            ),
        };
        let mut physical = traversal.clone();
        if axis == ActionAxis::Horizontal && self.text_direction == TextDirection::RightToLeft {
            physical.reverse();
        }
        let help = actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                (action.arrangement_role() == ModalActionRole::Help).then_some(index)
            })
            .collect();
        ActionArrangement {
            physical,
            traversal,
            help,
        }
    }

    pub(super) fn default_action_presentation(
        &self,
        action: &super::core::ModalRenderAction,
    ) -> DefaultActionPresentation {
        if action.is_default && self.shape == DesktopShape::MacOs {
            DefaultActionPresentation::Emphasized
        } else {
            DefaultActionPresentation::None
        }
    }

    pub(super) fn return_action<A>(&self, actions: &[ModalAction<A>]) -> Option<usize> {
        actions
            .iter()
            .position(|action| action.is_default() && action.is_enabled())
    }

    pub(super) fn cancel_action<A>(&self, actions: &[ModalAction<A>]) -> Option<usize> {
        actions
            .iter()
            .position(|action| is_safe_cancel(action.role(), action.intent(), action.is_enabled()))
    }

    pub(super) fn alert_initial_focus<A>(
        &self,
        actions: &[ModalAction<A>],
    ) -> Option<ModalInitialFocus> {
        let destructive = actions
            .iter()
            .any(|action| action.intent() == ModalActionIntent::Destructive);
        let index = if destructive {
            self.cancel_action(actions).or_else(|| {
                actions.iter().position(|action| {
                    action.is_enabled() && action.intent() == ModalActionIntent::Ordinary
                })
            })
        } else {
            self.return_action(actions)
                .or_else(|| {
                    let mut enabled = actions
                        .iter()
                        .enumerate()
                        .filter(|(_, action)| action.is_enabled());
                    let first = enabled.next().map(|(index, _)| index);
                    first.filter(|_| enabled.next().is_none())
                })
                .or_else(|| self.cancel_action(actions))
                .or_else(|| actions.iter().position(ModalAction::is_enabled))
        };
        index.map(ModalInitialFocus::Action)
    }

    pub(super) fn progress_initial_focus<A>(
        &self,
        cancellation: &ProgressCancellation<A>,
    ) -> ModalInitialFocus {
        match cancellation {
            ProgressCancellation::Cancellable(action) if action.is_enabled() => {
                ModalInitialFocus::Action(0)
            }
            ProgressCancellation::Cancellable(_)
            | ProgressCancellation::ProgrammaticOnly { .. } => ModalInitialFocus::Surface,
        }
    }
}

impl Default for ModalDesktopPolicy {
    fn default() -> Self {
        Self::mac_os()
    }
}

impl Global for ModalDesktopPolicy {}

/// Explicitly installs the immutable logical desktop policy selected by the application.
///
/// Installation is intentionally separate from [`super::install_modal_theme`]: desktop semantics
/// do not depend on application paint or metrics, and the platform-neutral library never detects
/// the host platform.
pub fn install_modal_policy(cx: &mut App, policy: ModalDesktopPolicy) {
    cx.set_global(policy);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ActionArrangement {
    pub(super) physical: Vec<usize>,
    pub(super) traversal: Vec<usize>,
    pub(super) help: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActionFacts {
    safe_cancel: Option<usize>,
}

fn validate_modal_header(
    id: &super::ModalId,
    accessibility_title: &str,
    visible_title: &str,
) -> Result<(), ModalValidationError> {
    validate_required_text(id.as_str(), ModalTextField::ModalId)?;
    validate_bounded_text(
        id.as_str(),
        ModalTextField::ModalId,
        MAX_MODAL_ID_CHARACTERS,
    )?;
    validate_required_text(accessibility_title, ModalTextField::AccessibilityTitle)?;
    validate_bounded_text(
        accessibility_title,
        ModalTextField::AccessibilityTitle,
        MAX_MODAL_TITLE_CHARACTERS,
    )?;
    validate_required_text(visible_title, ModalTextField::VisibleTitle)?;
    validate_bounded_text(
        visible_title,
        ModalTextField::VisibleTitle,
        MAX_MODAL_TITLE_CHARACTERS,
    )
}

fn validate_actions<A: Eq>(
    actions: &[&ModalAction<A>],
) -> Result<ActionFacts, ModalValidationError> {
    let mut default = None;
    let mut cancel = None;
    let mut safe_cancel = None;
    for (index, action) in actions.iter().enumerate() {
        validate_required_text(action.label(), ModalTextField::ActionLabel)?;
        validate_bounded_text(
            action.label(),
            ModalTextField::ActionLabel,
            MAX_ACTION_LABEL_CHARACTERS,
        )?;
        validate_required_text(action.debug_identity(), ModalTextField::ActionDebugIdentity)?;
        validate_bounded_text(
            action.debug_identity(),
            ModalTextField::ActionDebugIdentity,
            MAX_ACTION_DEBUG_IDENTITY_CHARACTERS,
        )?;
        for (previous_index, previous) in actions[..index].iter().enumerate() {
            if previous.id() == action.id() {
                return Err(ModalValidationError::DuplicateActionIdentity {
                    first: previous_index,
                    duplicate: index,
                });
            }
            if previous.debug_identity() == action.debug_identity() {
                return Err(ModalValidationError::DuplicateActionDebugIdentity {
                    first: previous_index,
                    duplicate: index,
                });
            }
        }
        if action.role() == ModalActionRole::Cancel
            && action.intent() == ModalActionIntent::Destructive
        {
            return Err(ModalValidationError::DestructiveCancelAction { index });
        }
        if action.is_default() {
            if let Some(first) = default {
                return Err(ModalValidationError::MultipleDefaultActions {
                    first,
                    duplicate: index,
                });
            }
            if !action.is_enabled() {
                return Err(ModalValidationError::DisabledDefaultAction { index });
            }
            if action.role() == ModalActionRole::Cancel {
                return Err(ModalValidationError::CancelDefaultAction { index });
            }
            default = Some(index);
        }
        if action.role() == ModalActionRole::Cancel {
            if let Some(first) = cancel {
                return Err(ModalValidationError::MultipleCancelActions {
                    first,
                    duplicate: index,
                });
            }
            cancel = Some(index);
            if is_safe_cancel(action.role(), action.intent(), action.is_enabled()) {
                safe_cancel = Some(index);
            }
        }
        if action.role() == ModalActionRole::Help
            && (action.is_default() || action.intent() == ModalActionIntent::Destructive)
        {
            return Err(ModalValidationError::InvalidHelpAction { index });
        }
    }
    Ok(ActionFacts { safe_cancel })
}

pub(super) const fn is_cancel_capability(role: ModalActionRole, intent: ModalActionIntent) -> bool {
    matches!(role, ModalActionRole::Cancel) && matches!(intent, ModalActionIntent::Ordinary)
}

pub(super) const fn is_safe_cancel(
    role: ModalActionRole,
    intent: ModalActionIntent,
    enabled: bool,
) -> bool {
    is_cancel_capability(role, intent) && enabled
}

fn validate_destructive_decision<A>(
    actions: &[ModalAction<A>],
    safe_cancel: Option<usize>,
) -> Result<(), ModalValidationError> {
    if actions
        .iter()
        .any(|action| action.intent() == ModalActionIntent::Destructive)
        && safe_cancel.is_none()
    {
        return Err(ModalValidationError::UnsafeDestructiveDecision);
    }
    Ok(())
}

fn validate_alert_dismissal<A>(
    actions: &[ModalAction<A>],
    safe_cancel: Option<usize>,
) -> Result<(), ModalValidationError> {
    validate_destructive_decision(actions, safe_cancel)?;
    let has_enabled_acknowledgement = actions.iter().any(|action| {
        action.is_enabled()
            && action.role() != ModalActionRole::Help
            && action.intent() == ModalActionIntent::Ordinary
    });
    if !has_enabled_acknowledgement {
        return Err(ModalValidationError::MissingSafeDismissal);
    }
    Ok(())
}

fn validate_dialog_dismissal<A>(
    actions: &[ModalAction<A>],
    safe_cancel: Option<usize>,
) -> Result<(), ModalValidationError> {
    validate_destructive_decision(actions, safe_cancel)?;
    if safe_cancel.is_none() {
        return Err(ModalValidationError::MissingSafeDismissal);
    }
    Ok(())
}

fn ordered_indices(
    first: Option<usize>,
    middle: impl Iterator<Item = usize>,
    last: Option<usize>,
) -> Vec<usize> {
    first.into_iter().chain(middle).chain(last).collect()
}

#[cfg(test)]
pub(super) const fn physical_edge(edge: LogicalEdge, direction: TextDirection) -> PhysicalEdge {
    match (edge, direction) {
        (LogicalEdge::Leading, TextDirection::LeftToRight)
        | (LogicalEdge::Trailing, TextDirection::RightToLeft) => PhysicalEdge::Left,
        (LogicalEdge::Trailing, TextDirection::LeftToRight)
        | (LogicalEdge::Leading, TextDirection::RightToLeft) => PhysicalEdge::Right,
    }
}

pub(super) fn select_action_axis(
    surface_width: Pixels,
    available_width: Pixels,
    button_widths: &[Pixels],
    metrics: ModalMetrics,
) -> ActionAxis {
    if surface_width < metrics.horizontal_action_threshold() {
        return ActionAxis::Vertical;
    }
    let buttons_width = button_widths
        .iter()
        .copied()
        .map(|width| width.max(metrics.minimum_action_width()))
        .fold(px(0.0), |sum, width| sum + width);
    let gaps = metrics.action_gap() * button_widths.len().saturating_sub(1) as f32;
    if buttons_width + gaps <= available_width {
        ActionAxis::Horizontal
    } else {
        ActionAxis::Vertical
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::px;

    use super::*;
    use crate::modal::{
        AlertIntent, DeterminateProgress, ModalActionIntent, ModalId, ProgressState,
    };

    fn action(
        id: &'static str,
        role: ModalActionRole,
        debug_identity: &'static str,
    ) -> ModalAction<&'static str> {
        ModalAction::new(id, id, role, debug_identity)
    }

    fn ordinary_alert(actions: Vec<ModalAction<&'static str>>) -> Alert<&'static str> {
        Alert::new(
            ModalId::new("alert"),
            "Logical alert title",
            "Alert title",
            "A concise message",
            actions,
        )
    }

    fn dialog(actions: Vec<ModalAction<&'static str>>) -> Dialog<&'static str> {
        Dialog::new(
            ModalId::new("dialog"),
            "Logical dialog title",
            "Dialog title",
            actions,
            DialogInitialFocus::Action("cancel"),
        )
    }

    #[test]
    fn modal_validation_rejects_empty_mandatory_title() {
        let alert = Alert::new(
            ModalId::new("alert"),
            "Logical alert title",
            "",
            "A concise message",
            vec![action("okay", ModalActionRole::Affirmative, "okay")],
        );

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::EmptyText(
                ModalTextField::VisibleTitle
            ))
        );
    }

    #[test]
    fn modal_validation_rejects_bounded_text_overflow() {
        let alert = Alert::new(
            ModalId::new("alert"),
            "Logical alert title",
            "Alert title",
            "x".repeat(MAX_ALERT_MESSAGE_CHARACTERS + 1),
            vec![action("okay", ModalActionRole::Affirmative, "okay")],
        );

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::TextTooLong {
                field: ModalTextField::AlertMessage,
                maximum: MAX_ALERT_MESSAGE_CHARACTERS,
            })
        );
    }

    #[test]
    fn modal_validation_rejects_duplicate_caller_action_identity() {
        let alert = ordinary_alert(vec![
            action("same", ModalActionRole::Affirmative, "first"),
            action("same", ModalActionRole::Cancel, "second"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::DuplicateActionIdentity {
                first: 0,
                duplicate: 1,
            })
        );
    }

    #[test]
    fn modal_validation_rejects_duplicate_debug_identity() {
        let alert = ordinary_alert(vec![
            action("save", ModalActionRole::Affirmative, "same"),
            action("cancel", ModalActionRole::Cancel, "same"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::DuplicateActionDebugIdentity {
                first: 0,
                duplicate: 1,
            })
        );
    }

    #[test]
    fn modal_validation_rejects_multiple_defaults() {
        let alert = ordinary_alert(vec![
            action("save", ModalActionRole::Affirmative, "save").default_action(true),
            action("replace", ModalActionRole::Auxiliary, "replace").default_action(true),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::MultipleDefaultActions {
                first: 0,
                duplicate: 1,
            })
        );
    }

    #[test]
    fn modal_validation_rejects_multiple_cancel_actions() {
        let alert = ordinary_alert(vec![
            action("cancel", ModalActionRole::Cancel, "cancel"),
            action("close", ModalActionRole::Cancel, "close"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::MultipleCancelActions {
                first: 0,
                duplicate: 1,
            })
        );
    }

    #[test]
    fn modal_validation_rejects_disabled_default() {
        let alert = ordinary_alert(vec![
            action("save", ModalActionRole::Affirmative, "save")
                .default_action(true)
                .enabled(false),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::DisabledDefaultAction { index: 0 })
        );
    }

    #[test]
    fn modal_validation_rejects_cancel_as_default() {
        let alert = ordinary_alert(vec![
            action("save", ModalActionRole::Affirmative, "save"),
            action("cancel", ModalActionRole::Cancel, "cancel").default_action(true),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::CancelDefaultAction { index: 1 })
        );
    }

    #[test]
    fn alert_validation_rejects_zero_decisions() {
        let alert = ordinary_alert(Vec::new());

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::AlertDecisionCount { count: 0 })
        );
    }

    #[test]
    fn alert_validation_rejects_more_than_three_decisions() {
        let alert = ordinary_alert(vec![
            action("one", ModalActionRole::Affirmative, "one"),
            action("two", ModalActionRole::Auxiliary, "two"),
            action("three", ModalActionRole::Auxiliary, "three"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::AlertDecisionCount { count: 4 })
        );
    }

    #[test]
    fn alert_validation_requires_help_to_be_separate() {
        let alert = ordinary_alert(vec![action("help", ModalActionRole::Help, "help")]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::AlertHelpMustBeSeparate { index: 0 })
        );
    }

    #[test]
    fn alert_validation_rejects_non_help_in_help_slot() {
        let alert = ordinary_alert(vec![action("okay", ModalActionRole::Affirmative, "okay")])
            .help_action(action("more", ModalActionRole::Auxiliary, "more"));

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidHelpActionRole)
        );
    }

    #[test]
    fn alert_validation_rejects_invalid_separate_help_action() {
        let alert = ordinary_alert(vec![action("okay", ModalActionRole::Affirmative, "okay")])
            .help_action(
                action("help", ModalActionRole::Help, "help")
                    .with_intent(ModalActionIntent::Destructive),
            );

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidHelpAction { index: 1 })
        );
    }

    #[test]
    fn dialog_validation_rejects_missing_enabled_dismissal() {
        let dialog = dialog(vec![
            action("cancel", ModalActionRole::Cancel, "cancel").enabled(false),
        ]);

        assert_eq!(
            dialog.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::MissingSafeDismissal)
        );
    }

    #[test]
    fn dialog_validation_rejects_missing_action_initial_focus_identity() {
        let dialog = Dialog::new(
            ModalId::new("dialog"),
            "Logical dialog title",
            "Dialog title",
            vec![action("cancel", ModalActionRole::Cancel, "cancel")],
            DialogInitialFocus::Action("missing"),
        );

        assert_eq!(
            dialog.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidDialogInitialFocus)
        );
    }

    #[test]
    fn alert_validation_rejects_destructive_cancel_with_ordinary_sibling() {
        let alert = ordinary_alert(vec![
            action("continue", ModalActionRole::Affirmative, "continue"),
            action("cancel", ModalActionRole::Cancel, "cancel")
                .with_intent(ModalActionIntent::Destructive),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::DestructiveCancelAction { index: 1 })
        );
    }

    #[test]
    fn dialog_validation_rejects_destructive_cancel_with_ordinary_sibling() {
        let dialog = dialog(vec![
            action("continue", ModalActionRole::Affirmative, "continue"),
            action("cancel", ModalActionRole::Cancel, "cancel")
                .with_intent(ModalActionIntent::Destructive),
        ]);

        assert_eq!(
            dialog.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::DestructiveCancelAction { index: 1 })
        );
    }

    #[test]
    fn cancel_key_equivalents_never_target_a_destructive_cancel() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("continue", ModalActionRole::Affirmative, "continue"),
            action("cancel", ModalActionRole::Cancel, "cancel")
                .with_intent(ModalActionIntent::Destructive),
        ];

        assert_eq!(policy.cancel_action(&actions), None);
    }

    #[test]
    fn alert_validation_rejects_destructive_decision_without_cancel() {
        let alert = ordinary_alert(vec![
            action("delete", ModalActionRole::Affirmative, "delete")
                .with_intent(ModalActionIntent::Destructive),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::UnsafeDestructiveDecision)
        );
    }

    #[test]
    fn alert_validation_rejects_destructive_decision_with_disabled_cancel() {
        let alert = ordinary_alert(vec![
            action("delete", ModalActionRole::Affirmative, "delete")
                .with_intent(ModalActionIntent::Destructive),
            action("cancel", ModalActionRole::Cancel, "cancel").enabled(false),
        ]);

        assert_eq!(
            alert.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::UnsafeDestructiveDecision)
        );
    }

    #[test]
    fn alert_destructive_default_keeps_safe_cancel_initial_focus() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("delete", ModalActionRole::Affirmative, "delete")
                .with_intent(ModalActionIntent::Destructive)
                .default_action(true),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ];
        let alert = ordinary_alert(actions.clone()).intent(AlertIntent::Critical);
        alert
            .validate(&policy)
            .expect("destructive alert should be valid");

        assert_eq!(
            policy.alert_initial_focus(&actions),
            Some(ModalInitialFocus::Action(1))
        );
    }

    #[test]
    fn modal_policy_allows_intentionally_absent_default() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("save", ModalActionRole::Affirmative, "save"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ];

        assert_eq!(policy.return_action(&actions), None);
    }

    #[test]
    fn mac_os_horizontal_ltr_arranges_shared_typed_facts() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("replace", ModalActionRole::Affirmative, "replace").default_action(true),
            action("help", ModalActionRole::Help, "help"),
            action("options", ModalActionRole::Auxiliary, "options"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ];

        let arrangement = policy.action_arrangement(&actions, ActionAxis::Horizontal);
        let physical_ids = arrangement
            .physical
            .iter()
            .map(|index| *actions[*index].id())
            .collect::<Vec<_>>();

        assert_eq!(
            (
                physical_ids,
                arrangement.physical,
                arrangement.traversal,
                arrangement.help,
            ),
            (
                vec!["cancel", "options", "replace"],
                vec![3, 2, 0],
                vec![3, 2, 0],
                vec![1],
            )
        );
    }

    #[test]
    fn mac_os_horizontal_rtl_mirrors_only_physical_shared_typed_facts() {
        let policy = ModalDesktopPolicy::mac_os().with_text_direction(TextDirection::RightToLeft);
        let actions = vec![
            action("replace", ModalActionRole::Affirmative, "replace").default_action(true),
            action("help", ModalActionRole::Help, "help"),
            action("options", ModalActionRole::Auxiliary, "options"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ];

        let arrangement = policy.action_arrangement(&actions, ActionAxis::Horizontal);
        let physical_ids = arrangement
            .physical
            .iter()
            .map(|index| *actions[*index].id())
            .collect::<Vec<_>>();

        assert_eq!(
            (
                physical_ids,
                arrangement.physical,
                arrangement.traversal,
                arrangement.help,
            ),
            (
                vec!["replace", "options", "cancel"],
                vec![0, 2, 3],
                vec![3, 2, 0],
                vec![1],
            )
        );
    }

    #[test]
    fn mac_os_vertical_places_default_first_and_cancel_last() {
        let policy = ModalDesktopPolicy::mac_os().with_text_direction(TextDirection::RightToLeft);
        let actions = vec![
            action("options", ModalActionRole::Auxiliary, "options"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
            action("replace", ModalActionRole::Affirmative, "replace").default_action(true),
        ];

        let arrangement = policy.action_arrangement(&actions, ActionAxis::Vertical);

        assert_eq!(
            (arrangement.physical, arrangement.traversal),
            (vec![2, 0, 1], vec![2, 0, 1])
        );
    }

    #[test]
    fn alternate_policy_arranges_shared_typed_facts_and_preserves_identity() {
        let policy = ModalDesktopPolicy::win_ui_for_tests();
        let actions = vec![
            action("cancel", ModalActionRole::Cancel, "cancel"),
            action("help", ModalActionRole::Help, "help"),
            action("options", ModalActionRole::Auxiliary, "options"),
            action("save", ModalActionRole::Affirmative, "save").default_action(true),
        ];

        let arrangement = policy.action_arrangement(&actions, ActionAxis::Horizontal);
        let physical_ids = arrangement
            .physical
            .iter()
            .map(|index| *actions[*index].id())
            .collect::<Vec<_>>();

        assert_eq!(
            (
                physical_ids,
                arrangement.physical,
                arrangement.traversal,
                arrangement.help,
            ),
            (
                vec!["save", "options", "cancel"],
                vec![3, 2, 0],
                vec![3, 2, 0],
                vec![1],
            )
        );
    }

    #[test]
    fn return_activates_only_explicit_enabled_default() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("save", ModalActionRole::Affirmative, "save"),
            action("cancel", ModalActionRole::Cancel, "cancel"),
        ];

        assert_eq!(policy.return_action(&actions), None);
    }

    #[test]
    fn escape_and_command_period_resolve_only_enabled_cancel() {
        let policy = ModalDesktopPolicy::mac_os();
        let actions = vec![
            action("save", ModalActionRole::Affirmative, "save").default_action(true),
            action("cancel", ModalActionRole::Cancel, "cancel").enabled(false),
        ];

        assert_eq!(policy.cancel_action(&actions), None);
    }

    #[test]
    fn determinate_progress_clamps_finite_values() {
        let below = DeterminateProgress::new(-4.0).expect("finite progress should normalize");
        let above = DeterminateProgress::new(4.0).expect("finite progress should normalize");

        assert_eq!((below.value(), above.value()), (0.0, 1.0));
    }

    #[test]
    fn determinate_progress_rejects_nan() {
        assert_eq!(
            DeterminateProgress::new(f64::NAN),
            Err(super::super::ProgressValueError::NotFinite)
        );
    }

    #[test]
    fn determinate_progress_rejects_infinity() {
        assert_eq!(
            DeterminateProgress::new(f64::INFINITY),
            Err(super::super::ProgressValueError::NotFinite)
        );
    }

    #[test]
    fn determinate_progress_maximum_does_not_encode_completion() {
        let progress = ProgressState::Determinate(
            DeterminateProgress::new(1.0).expect("finite progress should normalize"),
        );

        assert!(matches!(progress, ProgressState::Determinate(value) if value.is_maximum()));
    }

    #[test]
    fn cancellable_progress_requires_ordinary_cancel_capability() {
        let progress = ProgressDialog::new(
            ModalId::new("progress"),
            "Logical progress title",
            "Progress title",
            "Working",
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(action("stop", ModalActionRole::Auxiliary, "stop")),
        );

        assert_eq!(
            progress.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidProgressCancelAction)
        );
    }

    #[test]
    fn cancellable_progress_accepts_initially_disabled_cancel_capability() {
        let progress = ProgressDialog::new(
            ModalId::new("progress"),
            "Logical progress title",
            "Progress title",
            "Working",
            ProgressState::Indeterminate,
            ProgressCancellation::Cancellable(
                action("cancel", ModalActionRole::Cancel, "cancel").enabled(false),
            ),
        );

        assert_eq!(progress.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
    }

    #[test]
    fn progress_update_rejects_status_overflow() {
        let update = super::super::ProgressDialogUpdate::new()
            .status("x".repeat(super::super::MAX_PROGRESS_STATUS_CHARACTERS + 1));

        assert_eq!(
            update.validate(),
            Err(ModalValidationError::TextTooLong {
                field: ModalTextField::ProgressStatus,
                maximum: super::super::MAX_PROGRESS_STATUS_CHARACTERS,
            })
        );
    }

    #[test]
    fn programmatic_only_progress_rejects_zero_deadline() {
        let progress = ProgressDialog::<&'static str>::new(
            ModalId::new("progress"),
            "Logical progress title",
            "Progress title",
            "Working",
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(Duration::ZERO),
        );

        assert_eq!(
            progress.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidProgrammaticOnlyDeadline {
                deadline: Duration::ZERO,
                maximum: MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE,
            })
        );
    }

    #[test]
    fn programmatic_only_progress_rejects_deadline_above_installed_policy_maximum() {
        let deadline = MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE + Duration::from_millis(1);
        let progress = ProgressDialog::<&'static str>::new(
            ModalId::new("progress"),
            "Logical progress title",
            "Progress title",
            "Working",
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(deadline),
        );

        assert_eq!(
            progress.validate(&ModalDesktopPolicy::mac_os()),
            Err(ModalValidationError::InvalidProgrammaticOnlyDeadline {
                deadline,
                maximum: MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE,
            })
        );
    }

    #[test]
    fn programmatic_only_progress_accepts_installed_policy_maximum_deadline() {
        let progress = ProgressDialog::<&'static str>::new(
            ModalId::new("progress"),
            "Logical progress title",
            "Progress title",
            "Working",
            ProgressState::Indeterminate,
            ProgressCancellation::programmatic_only(MAXIMUM_PROGRAMMATIC_ONLY_DEADLINE),
        );

        assert_eq!(progress.validate(&ModalDesktopPolicy::mac_os()), Ok(()));
    }

    #[test]
    fn action_axis_stays_horizontal_when_threshold_and_labels_fit() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));

        assert_eq!(
            select_action_axis(px(400.0), px(368.0), &[px(90.0), px(100.0)], metrics,),
            ActionAxis::Horizontal
        );
    }

    #[test]
    fn compact_surface_stays_horizontal_when_labels_fit_its_padded_content() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));

        assert_eq!(
            select_action_axis(
                px(360.0),
                px(328.0),
                &[px(72.0), px(72.0), px(72.0)],
                metrics,
            ),
            ActionAxis::Horizontal
        );
    }

    #[test]
    fn action_axis_becomes_vertical_for_long_localized_labels() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));

        assert_eq!(
            select_action_axis(px(400.0), px(368.0), &[px(220.0), px(220.0)], metrics,),
            ActionAxis::Vertical
        );
    }

    #[test]
    fn action_axis_becomes_vertical_below_adaptive_threshold() {
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));

        assert_eq!(
            select_action_axis(px(320.0), px(288.0), &[px(80.0)], metrics),
            ActionAxis::Vertical
        );
    }

    #[test]
    fn logical_edges_mirror_in_right_to_left_layout() {
        assert_eq!(
            (
                physical_edge(LogicalEdge::Leading, TextDirection::RightToLeft),
                physical_edge(LogicalEdge::Trailing, TextDirection::RightToLeft),
            ),
            (PhysicalEdge::Right, PhysicalEdge::Left)
        );
    }
}
