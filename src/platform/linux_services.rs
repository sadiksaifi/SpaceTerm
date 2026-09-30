//! Linux desktops have no system Services menu. Registration succeeds without a native effect.
use std::rc::Rc;

use gpui::Window;

use super::services_registration::{ServicesRegistration, ServicesRegistrationError};
use crate::terminal::native_services::services::ServiceEndpoint;

pub(super) struct LinuxServicesRegistration;

impl ServicesRegistration for LinuxServicesRegistration {
    fn register(&self) -> Result<(), ServicesRegistrationError> {
        Ok(())
    }

    fn install(
        &self,
        _: &Window,
        _: Rc<dyn ServiceEndpoint>,
    ) -> Result<(), ServicesRegistrationError> {
        Ok(())
    }
}
