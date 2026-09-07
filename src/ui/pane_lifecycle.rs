use crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory;
use crate::terminal::native_services::NativeServiceAdapters;
use crate::terminal::{
    PreparedWorkspaceTerminalLaunch, TerminalKeyInputAdapterFactory,
    WorkspaceTerminalSessionFactory,
};
use gpui::{Context, Window};
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

/// Creates each Pane's independently owned input, accessibility, and native lifecycle capabilities.
#[derive(Clone)]
pub(crate) struct PaneConstruction {
    key_input: Rc<dyn TerminalKeyInputAdapterFactory>,
    accessibility: Rc<dyn TerminalAccessibilityAdapterFactory>,
    native_services: NativeServiceAdapters,
    lifecycle: PaneLifecycleDependencies,
}

impl PaneConstruction {
    pub(crate) fn new(
        key_input: Rc<dyn TerminalKeyInputAdapterFactory>,
        accessibility: Rc<dyn TerminalAccessibilityAdapterFactory>,
        native_services: NativeServiceAdapters,
        lifecycle: PaneLifecycleDependencies,
    ) -> Self {
        Self {
            key_input,
            accessibility,
            native_services,
            lifecycle,
        }
    }

    pub(crate) fn create(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        prepared_launch: PreparedWorkspaceTerminalLaunch,
        window: &mut Window,
        cx: &mut Context<super::TerminalPane>,
    ) -> super::TerminalPane {
        super::TerminalPane::new_with_prepared_launch(
            session_factory,
            prepared_launch,
            self.key_input.create(),
            self.accessibility.as_ref(),
            self.native_services.clone(),
            self.lifecycle.clone(),
            window,
            cx,
        )
    }

    #[cfg(test)]
    pub(crate) fn testing() -> Self {
        Self::new(
            Rc::new(crate::terminal::GpuiTerminalKeyInputAdapterFactory::default()),
            Rc::new(crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default()),
            crate::terminal::native_services::testing::adapters(),
            PaneLifecycleDependencies::testing(),
        )
    }

    #[cfg(test)]
    pub(crate) fn assert_application_capabilities(
        &self,
        expected: &crate::app::ApplicationCapabilities,
    ) {
        assert!(Rc::ptr_eq(&self.key_input, &expected.key_input));
        assert!(Rc::ptr_eq(&self.accessibility, &expected.accessibility));
        assert!(Rc::ptr_eq(
            &self.native_services.selection_clipboard,
            &expected.native_services.selection_clipboard
        ));
        assert!(Rc::ptr_eq(
            &self.native_services.file_clipboard,
            &expected.native_services.file_clipboard
        ));
        assert!(Rc::ptr_eq(
            &self.native_services.file_preview,
            &expected.native_services.file_preview
        ));
        assert!(Rc::ptr_eq(
            &self.lifecycle.activity,
            &expected.lifecycle.activity
        ));
        assert!(Rc::ptr_eq(
            &self.lifecycle.visibility,
            &expected.lifecycle.visibility
        ));
        assert!(Rc::ptr_eq(&self.lifecycle.wheel, &expected.lifecycle.wheel));
        assert!(
            self.lifecycle
                .attention
                .same_coordinator(&expected.lifecycle.attention)
        );
        assert!(
            self.lifecycle
                .secure_input
                .same_coordinator(&expected.lifecycle.secure_input)
        );
    }
}

/// Invalidates pending child launches whenever the hierarchy changes remote lifecycle.
#[derive(Default)]
pub(crate) struct RemoteHierarchyLifecycle {
    disconnected_generation: Option<u64>,
    child_launch_generation: u64,
}

impl RemoteHierarchyLifecycle {
    pub(crate) fn begin_child_launch(&mut self) -> u64 {
        self.child_launch_generation = self.child_launch_generation.wrapping_add(1);
        self.child_launch_generation
    }

    pub(crate) fn is_current_child_launch(&self, generation: u64) -> bool {
        self.child_launch_generation == generation
    }

    pub(crate) const fn disconnected_generation(&self) -> Option<u64> {
        self.disconnected_generation
    }

    pub(crate) fn disconnect(&mut self, generation: u64) {
        self.disconnected_generation = Some(generation);
        self.begin_child_launch();
    }

    pub(crate) fn restarted(&mut self) {
        self.disconnected_generation = None;
        self.begin_child_launch();
    }
}
