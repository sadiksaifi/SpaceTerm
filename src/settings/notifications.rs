//! The Notification Settings: which notifications the desktop posts. Authorization to post any
//! belongs to the desktop itself, as ADR 0063 records.

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct NotificationPreferences {
    /// Whether a bell or a finished command in a Pane without Terminal Input Focus posts a
    /// notification while SpaceTerm is in the background. Update Reminders do not follow it.
    pub(crate) terminal_attention: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            terminal_attention: true,
        }
    }
}
