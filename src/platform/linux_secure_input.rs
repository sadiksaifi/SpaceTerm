//! Linux has no process-wide secure keyboard entry. Every transition is acknowledged without a
//! native effect, and the Secure Input presentation stays absent on Linux.
use crate::terminal::secure_input::{SecureInputAdapter, SecureInputError};

pub(super) struct LinuxSecureInputAdapter;

impl SecureInputAdapter for LinuxSecureInputAdapter {
    fn set_enabled(&mut self, _: bool) -> Result<(), SecureInputError> {
        Ok(())
    }
}
