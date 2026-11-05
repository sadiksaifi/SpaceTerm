//! The desktop's own notification authorization, which the Privacy section presents. The desktop
//! posts every notification, so this grant is its own and not SpaceTerm Core's: see ADR 0063.

/// What the desktop knows about posting notifications.
#[allow(
    dead_code,
    reason = "each host constructs only the states its notification system has"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotificationAccessState {
    /// The person has not decided yet.
    NotDetermined,
    /// Notifications show as alerts or in Notification Center.
    Allowed,
    /// Provisional authorization: notifications show only in Notification Center, without alerts.
    Quiet,
    /// Allowed, but alerts and Notification Center are both off, so nothing shows.
    Hidden,
    Denied,
    /// A notification service answers on the session bus. Linux has no per-application grant.
    ServiceAvailable,
    /// No notification service answers on the session bus.
    ServiceUnavailable,
}

/// Content-free failures from reading, requesting, or opening notification authorization.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only macOS asks each application for notification authorization"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum NotificationAccessError {
    #[error("notification authorization is unavailable")]
    Unavailable,
    #[error("the system rejected the notification authorization operation")]
    Rejected,
}

pub(crate) type NotificationStateCompletion =
    Box<dyn FnOnce(Result<NotificationAccessState, NotificationAccessError>) + Send>;
pub(crate) type NotificationRequestCompletion =
    Box<dyn FnOnce(Result<(), NotificationAccessError>) + Send>;

/// Reads and recovers the desktop's notification authorization. Completions may run on any
/// thread.
pub(crate) trait NotificationAccess {
    fn read(&self, completion: NotificationStateCompletion);

    /// Asks the person to allow alerts, which shows the system prompt. Callers request only from
    /// `NotDetermined`, because the system shows its prompt once.
    fn request(&self, completion: NotificationRequestCompletion);

    /// Opens the desktop's own entry in the system's notification settings.
    fn open_settings(&self) -> Result<(), NotificationAccessError>;
}
