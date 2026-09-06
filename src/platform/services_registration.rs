use crate::terminal::native_services::services::ServiceEndpoint;
use gpui::Window;
use std::rc::Rc;
use thiserror::Error;
#[derive(Debug, Error)]
pub(crate) enum ServicesRegistrationError {
    #[error("the application is unavailable")]
    ApplicationUnavailable,
    #[error("the GPUI window did not expose a native view")]
    NativeViewUnavailable,
    #[error("the SpaceTerm Services responder class could not be registered")]
    ResponderClassUnavailable,
    #[error("the SpaceTerm Services responder could not be allocated")]
    ResponderAllocationFailed,
}

pub(crate) trait ServicesRegistration {
    fn register(&self) -> Result<(), ServicesRegistrationError>;
    fn install(
        &self,
        window: &Window,
        endpoint: Rc<dyn ServiceEndpoint>,
    ) -> Result<(), ServicesRegistrationError>;
}
