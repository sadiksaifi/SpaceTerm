use crate::terminal::secure_input::{SecureInputAdapter, SecureInputError};

/// Carbon supplies only the process-wide physical transition.
pub(crate) struct MacosSecureInputAdapter;

impl MacosSecureInputAdapter {
    pub(crate) const fn new() -> Self {
        Self
    }
}

impl SecureInputAdapter for MacosSecureInputAdapter {
    fn set_enabled(&mut self, enabled: bool) -> Result<(), SecureInputError> {
        let status = if enabled {
            // SAFETY: the injected coordinator is confined to GPUI's main thread.
            unsafe { EnableSecureEventInput() }
        } else {
            // SAFETY: the injected coordinator is confined to GPUI's main thread.
            unsafe { DisableSecureEventInput() }
        };
        classify_status(status)
    }
}

fn classify_status(status: i32) -> Result<(), SecureInputError> {
    (status == 0)
        .then_some(())
        .ok_or(SecureInputError::TransitionRejected)
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_status_is_discarded_at_the_capability_boundary() {
        assert_eq!(classify_status(0), Ok(()));
        for status in [i32::MIN, -1, 1, i32::MAX] {
            assert_eq!(
                classify_status(status),
                Err(SecureInputError::TransitionRejected)
            );
        }
    }
}
