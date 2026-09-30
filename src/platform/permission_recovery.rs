#![cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only desktops with a permission recovery route compose this policy")
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionRecoveryError {
    OffMainThread,
    PlatformUnavailable,
    PlatformRejected,
}

pub(crate) trait PermissionRecoveryOpener {
    fn open(&self) -> Result<(), PermissionRecoveryError>;
}

pub(crate) struct PermissionRecovery {
    launcher: Box<dyn UrlLauncher>,
    preferred: &'static str,
    fallback: &'static str,
}

impl PermissionRecoveryOpener for PermissionRecovery {
    fn open(&self) -> Result<(), PermissionRecoveryError> {
        open_settings_url(self.launcher.as_ref(), self.preferred, self.fallback)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UrlLaunchError {
    OffMainThread,
    Unavailable,
    Rejected,
}

pub(crate) trait UrlLauncher {
    fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError>;
}

fn open_settings_url(
    launcher: &dyn UrlLauncher,
    preferred: &'static str,
    fallback: &'static str,
) -> Result<(), PermissionRecoveryError> {
    match launcher.open_url(preferred) {
        Ok(()) => Ok(()),
        Err(UrlLaunchError::Rejected) => launcher
            .open_url(fallback)
            .map_err(PermissionRecoveryError::from),
        Err(error @ (UrlLaunchError::OffMainThread | UrlLaunchError::Unavailable)) => {
            Err(PermissionRecoveryError::from(error))
        }
    }
}

impl From<UrlLaunchError> for PermissionRecoveryError {
    fn from(error: UrlLaunchError) -> Self {
        match error {
            UrlLaunchError::OffMainThread => Self::OffMainThread,
            UrlLaunchError::Unavailable => Self::PlatformUnavailable,
            UrlLaunchError::Rejected => Self::PlatformRejected,
        }
    }
}

impl PermissionRecovery {
    pub(crate) fn new(
        launcher: Box<dyn UrlLauncher>,
        preferred: &'static str,
        fallback: &'static str,
    ) -> Self {
        Self {
            launcher,
            preferred,
            fallback,
        }
    }
}
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    const PREFERRED_SETTINGS_URI: &str = "x-spaceterm-test:preferred";
    const FALLBACK_SETTINGS_URI: &str = "x-spaceterm-test:fallback";

    struct RecordingUrlLauncher {
        results: RefCell<VecDeque<Result<(), UrlLaunchError>>>,
        opened_uris: RefCell<Vec<&'static str>>,
    }

    impl RecordingUrlLauncher {
        fn new(results: impl IntoIterator<Item = Result<(), UrlLaunchError>>) -> Self {
            Self {
                results: RefCell::new(results.into_iter().collect()),
                opened_uris: RefCell::new(Vec::new()),
            }
        }
    }

    impl UrlLauncher for RecordingUrlLauncher {
        fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError> {
            self.opened_uris.borrow_mut().push(uri);
            self.results
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err(UrlLaunchError::Rejected))
        }
    }

    #[test]
    fn preferred_success_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Ok(())]);

        let result = open_settings_url(&launcher, PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (Ok(()), vec![PREFERRED_SETTINGS_URI])
        );
    }

    #[test]
    fn preferred_rejection_launches_successful_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::Rejected), Ok(())]);

        let result = open_settings_url(&launcher, PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (Ok(()), vec![PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI,])
        );
    }

    #[test]
    fn both_rejected_return_content_free_platform_error() {
        let launcher = RecordingUrlLauncher::new([
            Err(UrlLaunchError::Rejected),
            Err(UrlLaunchError::Rejected),
        ]);

        let result = open_settings_url(&launcher, PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(PermissionRecoveryError::PlatformRejected),
                vec![PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI,]
            )
        );
    }

    #[test]
    fn non_appkit_failure_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::OffMainThread)]);

        let result = open_settings_url(&launcher, PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(PermissionRecoveryError::OffMainThread),
                vec![PREFERRED_SETTINGS_URI]
            )
        );
    }

    #[test]
    fn platform_unavailability_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::Unavailable)]);

        let result = open_settings_url(&launcher, PREFERRED_SETTINGS_URI, FALLBACK_SETTINGS_URI);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(PermissionRecoveryError::PlatformUnavailable),
                vec![PREFERRED_SETTINGS_URI]
            )
        );
    }

    #[test]
    fn error_identifiers_carry_no_content() {
        assert_eq!(
            [
                PermissionRecoveryError::OffMainThread,
                PermissionRecoveryError::PlatformUnavailable,
                PermissionRecoveryError::PlatformRejected,
            ]
            .map(|error| format!("{error:?}")),
            [
                "OffMainThread".to_owned(),
                "PlatformUnavailable".to_owned(),
                "PlatformRejected".to_owned(),
            ]
        );
    }
}
