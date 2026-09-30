use crate::terminal::native_services::services::ServiceEndpoint;
use gpui::Window;
use std::rc::Rc;
use thiserror::Error;
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only a desktop Services Adapter reports registration failures")
)]
#[derive(Debug, Error)]
pub(crate) enum ServicesRegistrationError {
    #[error("the application is unavailable")]
    ApplicationUnavailable,
    #[error("the GPUI window did not expose a native view")]
    NativeViewUnavailable,
}

pub(crate) trait ServicesRegistration {
    fn register(&self) -> Result<(), ServicesRegistrationError>;
    fn install(
        &self,
        window: &Window,
        endpoint: Rc<dyn ServiceEndpoint>,
    ) -> Result<(), ServicesRegistrationError>;
}
