use crate::platform::terminal_accessibility::TerminalAccessibilityAdapterFactory;
use crate::terminal::native_services::NativeServiceAdapters;
use crate::terminal::{TerminalKeyInputAdapterFactory, WorkspaceTerminalSessionFactory};
use gpui::{Context, Entity, Window};
use std::rc::Rc;

use crate::platform::application_activity::ApplicationActivity;
use crate::platform::window_visibility::WindowVisibilityFactory;
use crate::terminal::attention_runtime::AttentionRuntime;
use crate::terminal::secure_input::SecureInputHandle;
use crate::terminal::wheel_phase::WheelPhaseEnrichment;
use crate::ui::permission_setup::PermissionSetup;

/// Constructor wiring for independent capabilities and application-owned coordinators.
/// This value defines no platform operations or surface policy.
#[derive(Clone)]
pub(crate) struct PaneLifecycleDependencies {
    pub(crate) activity: Rc<dyn ApplicationActivity>,
    pub(crate) visibility: Rc<dyn WindowVisibilityFactory>,
    pub(crate) wheel: Rc<dyn WheelPhaseEnrichment>,
    pub(crate) attention: AttentionRuntime,
    pub(crate) secure_input: SecureInputHandle,
    /// The application's Permission Setup, which a Pane offers for a Permission Request. The
    /// application creates it once it runs, so a composition made before then holds none.
    pub(crate) permission_setup: Option<Entity<PermissionSetup>>,
    /// The desktop's one Terminal Diagnostics counter set, which its Panes record into and the
    /// Desktop Channel reports.
    pub(crate) counters: std::sync::Arc<spaceterm_server::diagnostics::TerminalCounters>,
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
            permission_setup: None,
            counters: std::sync::Arc::default(),
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

    /// The counters Panes and their server connections record into.
    pub(crate) fn counters(
        &self,
    ) -> &std::sync::Arc<spaceterm_server::diagnostics::TerminalCounters> {
        &self.lifecycle.counters
    }

    /// The clipboard that application actions such as Copy Path write.
    pub(crate) fn text_clipboard(
        &self,
    ) -> &Rc<dyn crate::terminal::native_services::clipboard::TextClipboard> {
        &self.native_services.text_clipboard
    }

    pub(crate) fn create(
        &self,
        session_factory: WorkspaceTerminalSessionFactory,
        window: &mut Window,
        cx: &mut Context<super::TerminalPane>,
    ) -> super::TerminalPane {
        super::TerminalPane::with_adapters(
            session_factory,
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
