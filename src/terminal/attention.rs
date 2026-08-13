use std::time::{Duration, Instant};

const BELL_RATE_LIMIT: Duration = Duration::from_millis(100);
const DOCK_RATE_LIMIT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttentionEvent {
    Bell,
    CommandFinished {
        exit_status: Option<i32>,
        duration: Duration,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AttentionFacts {
    pub(crate) terminal_input_focus: bool,
    pub(crate) surface_active: bool,
    pub(crate) application_active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AttentionPolicy {
    pub(crate) visual_bell: bool,
    pub(crate) audio_bell: bool,
    pub(crate) dock_attention: bool,
    pub(crate) notifications: bool,
}

impl Default for AttentionPolicy {
    fn default() -> Self {
        Self {
            visual_bell: true,
            audio_bell: true,
            dock_attention: true,
            notifications: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AttentionEffects {
    pub(crate) visual_bell: bool,
    pub(crate) audio_bell: bool,
    pub(crate) request_dock_attention: bool,
    pub(crate) cancel_dock_attention: bool,
    pub(crate) notification: Option<AttentionEvent>,
    pub(crate) cancel_notification: bool,
    pub(crate) unread_count: u32,
}

#[derive(Debug)]
pub(crate) struct AttentionState {
    policy: AttentionPolicy,
    unread_count: u32,
    visual_bell: bool,
    last_bell: Option<Instant>,
    last_dock: Option<Instant>,
}

impl Default for AttentionState {
    fn default() -> Self {
        Self::new(AttentionPolicy::default())
    }
}

impl AttentionState {
    pub(crate) const fn new(policy: AttentionPolicy) -> Self {
        Self {
            policy,
            unread_count: 0,
            visual_bell: false,
            last_bell: None,
            last_dock: None,
        }
    }

    pub(crate) fn observe(
        &mut self,
        event: AttentionEvent,
        facts: AttentionFacts,
        now: Instant,
    ) -> AttentionEffects {
        if event == AttentionEvent::Bell
            && self
                .last_bell
                .is_some_and(|last| now.saturating_duration_since(last) < BELL_RATE_LIMIT)
        {
            return AttentionEffects {
                unread_count: self.unread_count,
                ..AttentionEffects::default()
            };
        }
        if event == AttentionEvent::Bell {
            self.last_bell = Some(now);
        }

        let unattended = !facts.terminal_input_focus;
        if unattended {
            self.unread_count = self.unread_count.saturating_add(1);
        }
        let visual_bell = self.policy.visual_bell && event == AttentionEvent::Bell;
        self.visual_bell |= visual_bell;
        let audio_bell = self.policy.audio_bell && event == AttentionEvent::Bell;
        let request_dock_attention = unattended
            && !facts.surface_active
            && self.policy.dock_attention
            && self
                .last_dock
                .is_none_or(|last| now.saturating_duration_since(last) >= DOCK_RATE_LIMIT);
        if request_dock_attention {
            self.last_dock = Some(now);
        }
        let notification =
            (unattended && !facts.application_active && self.policy.notifications).then_some(event);

        AttentionEffects {
            visual_bell,
            audio_bell,
            request_dock_attention,
            cancel_dock_attention: false,
            notification,
            cancel_notification: false,
            unread_count: self.unread_count,
        }
    }

    pub(crate) fn clear(&mut self) -> AttentionEffects {
        let cancel_dock_attention = self.unread_count > 0;
        self.unread_count = 0;
        self.visual_bell = false;
        AttentionEffects {
            cancel_dock_attention,
            cancel_notification: cancel_dock_attention,
            ..AttentionEffects::default()
        }
    }

    pub(crate) const fn unread_count(&self) -> u32 {
        self.unread_count
    }

    pub(crate) const fn visual_bell(&self) -> bool {
        self.visual_bell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_clock_suppresses_bell_storms_without_suppressing_notification_demand() {
        let epoch = Instant::now();
        let mut state = AttentionState::default();
        let facts = AttentionFacts::default();

        let first = state.observe(AttentionEvent::Bell, facts, epoch);
        let storm = state.observe(
            AttentionEvent::Bell,
            facts,
            epoch + Duration::from_millis(20),
        );
        let later = state.observe(AttentionEvent::Bell, facts, epoch + Duration::from_secs(2));

        assert!(first.audio_bell && first.visual_bell && first.request_dock_attention);
        assert!(first.notification.is_some());
        assert_eq!(
            storm,
            AttentionEffects {
                unread_count: 1,
                ..Default::default()
            }
        );
        assert!(later.audio_bell && later.request_dock_attention);
        assert!(later.notification.is_some());
        assert_eq!(later.unread_count, 2);
    }

    #[test]
    fn ownership_facts_prevent_active_input_from_accumulating_attention() {
        let mut state = AttentionState::default();
        let effects = state.observe(
            AttentionEvent::CommandFinished {
                exit_status: Some(0),
                duration: Duration::from_secs(3),
            },
            AttentionFacts {
                terminal_input_focus: true,
                surface_active: true,
                application_active: true,
            },
            Instant::now(),
        );

        assert_eq!(effects.unread_count, 0);
        assert!(!effects.request_dock_attention);
        assert!(effects.notification.is_none());
    }

    #[test]
    fn focus_or_input_clear_resets_visual_unread_and_dock_state() {
        let mut state = AttentionState::default();
        state.observe(
            AttentionEvent::Bell,
            AttentionFacts::default(),
            Instant::now(),
        );

        let cleared = state.clear();

        assert!(cleared.cancel_dock_attention);
        assert!(cleared.cancel_notification);
        assert_eq!(state.unread_count(), 0);
        assert!(!state.visual_bell());
    }
}
