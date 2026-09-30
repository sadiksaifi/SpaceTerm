//! Linux desktop notifications. Delivery through the notification portal arrives with a later
//! desktop wave; until then authorization reads as denied so the runtime suppresses delivery.
use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion,
};
use crate::terminal::attention_runtime::AttentionFailure;

pub(super) struct LinuxNotificationAdapter;

impl NotificationAdapter for LinuxNotificationAdapter {
    fn settings(&self, completion: SettingsCompletion) {
        completion(Ok(NotificationSettings {
            authorization: NotificationAuthorization::Denied,
            alert_enabled: false,
            center_enabled: false,
        }));
    }

    fn authorize_provisionally(&self, completion: AuthorizationCompletion) {
        completion(Err(AttentionFailure::Unavailable));
    }

    fn submit(&self, _: u32) -> Result<(), AttentionFailure> {
        Err(AttentionFailure::Unavailable)
    }

    fn clear(&self) -> Result<(), AttentionFailure> {
        Ok(())
    }
}
