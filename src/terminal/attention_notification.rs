//! Content-free authorization and asynchronous delivery ownership for Terminal Attention.

use std::sync::{Arc, Mutex};

use super::attention_runtime::{AttentionFailure, NotificationDelivery, NotificationDriver};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotificationAuthorization {
    NotDetermined,
    Denied,
    Authorized,
    Provisional,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NotificationSettings {
    pub(crate) authorization: NotificationAuthorization,
    pub(crate) alert_enabled: bool,
    pub(crate) center_enabled: bool,
}

pub(crate) type SettingsCompletion =
    Box<dyn FnOnce(Result<NotificationSettings, AttentionFailure>) + Send>;
pub(crate) type AuthorizationCompletion = Box<dyn FnOnce(Result<(), AttentionFailure>) + Send>;

/// Only native settings, authorization, and notification-center operations cross this interface.
pub(crate) trait NotificationAdapter: Send + Sync {
    fn settings(&self, completion: SettingsCompletion);
    fn authorize_provisionally(&self, completion: AuthorizationCompletion);
    fn submit(&self, aggregate_count: u32) -> Result<(), AttentionFailure>;
    fn clear(&self) -> Result<(), AttentionFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationDecision {
    Submit,
    RequestProvisional,
    Suppress,
}

fn authorization_decision(settings: NotificationSettings) -> AuthorizationDecision {
    match settings.authorization {
        NotificationAuthorization::Authorized | NotificationAuthorization::Provisional
            if settings.alert_enabled || settings.center_enabled =>
        {
            AuthorizationDecision::Submit
        }
        NotificationAuthorization::NotDetermined => AuthorizationDecision::RequestProvisional,
        _ => AuthorizationDecision::Suppress,
    }
}

pub(crate) fn notification_body(aggregate_count: u32) -> String {
    format!("Terminal requested attention ({})", aggregate_count.max(1))
}

#[derive(Default)]
struct DeliveryState {
    generation: u64,
    latest: Option<(u64, u32)>,
    authorization_in_flight: bool,
    failure: Option<AttentionFailure>,
}

struct DeliveryOwner {
    adapter: Arc<dyn NotificationAdapter>,
    state: Mutex<DeliveryState>,
}

impl DeliveryOwner {
    fn query(self: &Arc<Self>, generation: u64, count: u32) {
        let owner = Arc::clone(self);
        self.adapter.settings(Box::new(move |settings| {
            let mut state = owner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.latest != Some((generation, count)) {
                return;
            }
            let settings = match settings {
                Ok(settings) => settings,
                Err(failure) => {
                    state.failure = Some(failure);
                    return;
                }
            };
            match authorization_decision(settings) {
                AuthorizationDecision::Submit => {
                    // Submit and clear serialize through the same lock. A cleared generation can
                    // never publish after its native clear even when callbacks arrive concurrently.
                    state.failure = owner.adapter.submit(count).err();
                }
                AuthorizationDecision::RequestProvisional => {
                    if state.authorization_in_flight {
                        return;
                    }
                    state.authorization_in_flight = true;
                    drop(state);
                    let completion_owner = Arc::clone(&owner);
                    owner
                        .adapter
                        .authorize_provisionally(Box::new(move |result| {
                            let mut state = completion_owner
                                .state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            state.authorization_in_flight = false;
                            if let Err(failure) = result {
                                state.failure = Some(failure);
                                return;
                            }
                            let latest = state.latest;
                            drop(state);
                            if let Some((generation, count)) = latest {
                                completion_owner.query(generation, count);
                            }
                        }));
                }
                AuthorizationDecision::Suppress => {
                    state.failure = Some(AttentionFailure::Denied);
                }
            }
        }));
    }

    fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.latest = None;
        state.generation = state.generation.wrapping_add(1);
        state.failure = self.adapter.clear().err();
    }
}

pub(crate) struct AttentionNotifications(Arc<DeliveryOwner>);

impl AttentionNotifications {
    pub(crate) fn new(adapter: Arc<dyn NotificationAdapter>) -> Self {
        Self(Arc::new(DeliveryOwner {
            adapter,
            state: Mutex::new(DeliveryState::default()),
        }))
    }
}

impl NotificationDriver for AttentionNotifications {
    fn deliver(&mut self, delivery: NotificationDelivery) {
        let count = delivery.aggregate_count.max(1);
        let generation = {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            state.latest = Some((generation, count));
            generation
        };
        self.0.query(generation, count);
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

impl Drop for AttentionNotifications {
    fn drop(&mut self) {
        let pending = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest
            .is_some();
        if pending {
            self.0.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingAdapter {
        settings: Mutex<Vec<SettingsCompletion>>,
        authorizations: Mutex<Vec<AuthorizationCompletion>>,
        deliveries: Mutex<Vec<u32>>,
        clears: Mutex<usize>,
    }

    impl NotificationAdapter for RecordingAdapter {
        fn settings(&self, completion: SettingsCompletion) {
            self.settings.lock().unwrap().push(completion);
        }
        fn authorize_provisionally(&self, completion: AuthorizationCompletion) {
            self.authorizations.lock().unwrap().push(completion);
        }
        fn submit(&self, count: u32) -> Result<(), AttentionFailure> {
            self.deliveries.lock().unwrap().push(count);
            Ok(())
        }
        fn clear(&self) -> Result<(), AttentionFailure> {
            *self.clears.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn settings(authorization: NotificationAuthorization) -> NotificationSettings {
        NotificationSettings {
            authorization,
            alert_enabled: true,
            center_enabled: true,
        }
    }

    fn answer_settings(adapter: &RecordingAdapter, authorization: NotificationAuthorization) {
        let completion = adapter.settings.lock().unwrap().remove(0);
        completion(Ok(settings(authorization)));
    }

    #[test]
    fn authorization_respects_native_permission_and_enabled_surfaces() {
        for authorization in [
            NotificationAuthorization::Authorized,
            NotificationAuthorization::Provisional,
        ] {
            assert_eq!(
                authorization_decision(settings(authorization)),
                AuthorizationDecision::Submit
            );
            assert_eq!(
                authorization_decision(NotificationSettings {
                    authorization,
                    alert_enabled: false,
                    center_enabled: false
                }),
                AuthorizationDecision::Suppress
            );
        }
        for authorization in [
            NotificationAuthorization::Denied,
            NotificationAuthorization::Unknown,
        ] {
            assert_eq!(
                authorization_decision(settings(authorization)),
                AuthorizationDecision::Suppress
            );
        }
        assert_eq!(
            authorization_decision(settings(NotificationAuthorization::NotDetermined)),
            AuthorizationDecision::RequestProvisional
        );
    }

    #[test]
    fn stale_settings_and_cleared_authorization_cannot_deliver() {
        let adapter = Arc::new(RecordingAdapter::default());
        let mut notifications = AttentionNotifications::new(adapter.clone());
        notifications.deliver(NotificationDelivery { aggregate_count: 1 });
        notifications.deliver(NotificationDelivery { aggregate_count: 2 });
        answer_settings(&adapter, NotificationAuthorization::Authorized);
        answer_settings(&adapter, NotificationAuthorization::NotDetermined);
        notifications.clear();
        let completion = adapter.authorizations.lock().unwrap().remove(0);
        completion(Ok(()));
        assert!(adapter.deliveries.lock().unwrap().is_empty());
        assert!(adapter.settings.lock().unwrap().is_empty());
    }

    #[test]
    fn one_authorization_completion_uses_matching_latest_count() {
        let adapter = Arc::new(RecordingAdapter::default());
        let mut notifications = AttentionNotifications::new(adapter.clone());
        notifications.deliver(NotificationDelivery { aggregate_count: 1 });
        answer_settings(&adapter, NotificationAuthorization::NotDetermined);
        notifications.deliver(NotificationDelivery { aggregate_count: 7 });
        answer_settings(&adapter, NotificationAuthorization::NotDetermined);
        assert_eq!(adapter.authorizations.lock().unwrap().len(), 1);
        let completion = adapter.authorizations.lock().unwrap().remove(0);
        completion(Ok(()));
        answer_settings(&adapter, NotificationAuthorization::Provisional);
        assert_eq!(*adapter.deliveries.lock().unwrap(), vec![7]);
    }

    #[test]
    fn adapter_failure_is_closed_and_content_free() {
        let adapter = Arc::new(RecordingAdapter::default());
        let mut notifications = AttentionNotifications::new(adapter.clone());
        notifications.deliver(NotificationDelivery { aggregate_count: 1 });
        let completion = adapter.settings.lock().unwrap().remove(0);
        completion(Err(AttentionFailure::Unavailable));
        assert_eq!(
            notifications.0.state.lock().unwrap().failure,
            Some(AttentionFailure::Unavailable)
        );
        assert!(adapter.deliveries.lock().unwrap().is_empty());
    }

    #[test]
    fn dropped_owner_invalidates_in_flight_delivery() {
        let adapter = Arc::new(RecordingAdapter::default());
        let mut notifications = AttentionNotifications::new(adapter.clone());
        notifications.deliver(NotificationDelivery { aggregate_count: 1 });
        drop(notifications);
        answer_settings(&adapter, NotificationAuthorization::Authorized);
        assert!(adapter.deliveries.lock().unwrap().is_empty());
    }

    #[test]
    fn notification_body_contains_only_bounded_count() {
        assert_eq!(notification_body(0), "Terminal requested attention (1)");
        assert_eq!(notification_body(2), "Terminal requested attention (2)");
    }
}
