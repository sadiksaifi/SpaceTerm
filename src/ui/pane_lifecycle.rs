use std::rc::Rc;

use crate::platform::application_activity::ApplicationActivity;
use crate::platform::window_visibility::WindowVisibilityFactory;
use crate::terminal::attention_runtime::AttentionRuntime;
use crate::terminal::secure_input::SecureInputHandle;
use crate::terminal::wheel_phase::WheelPhaseEnrichment;

/// Constructor wiring for independent capabilities and application-owned coordinators.
/// This value defines no platform operations or surface policy.
#[derive(Clone)]
pub(crate) struct PaneLifecycleDependencies {
    pub(crate) observation: Option<crate::observation::AuthenticatedObservation>,
    pub(crate) activity: Rc<dyn ApplicationActivity>,
    pub(crate) visibility: Rc<dyn WindowVisibilityFactory>,
    pub(crate) wheel: Rc<dyn WheelPhaseEnrichment>,
    pub(crate) attention: AttentionRuntime,
    pub(crate) secure_input: SecureInputHandle,
}

#[cfg(test)]
impl PaneLifecycleDependencies {
    pub(crate) fn testing() -> Self {
        let activity: Rc<dyn ApplicationActivity> =
            Rc::new(crate::platform::application_activity::TestApplicationActivity);
        Self {
            observation: None,
            attention: AttentionRuntime::testing(Rc::clone(&activity)),
            secure_input: SecureInputHandle::testing(),
            activity,
            visibility: Rc::new(
                crate::platform::window_visibility::RecordingWindowVisibilityFactory::default(),
            ),
            wheel: Rc::new(crate::terminal::wheel_phase::NoWheelPhaseEnrichment),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrated_surface_modules_do_not_name_concrete_platform_implementations() {
        for source in [
            include_str!("terminal_pane.rs"),
            include_str!("render_lifecycle.rs"),
            include_str!("terminal_focus.rs"),
            include_str!("../terminal/attention_runtime.rs"),
            include_str!("../terminal/attention_notification.rs"),
            include_str!("../terminal/secure_input.rs"),
            include_str!("../terminal/wheel_phase.rs"),
            include_str!("../platform/window_visibility.rs"),
        ] {
            for forbidden in ["macos_", "Macos", "use cocoa::", "use objc::", "target_os"] {
                assert!(
                    !source.contains(forbidden),
                    "portable surface boundary contains {forbidden}"
                );
            }
        }
    }
}
