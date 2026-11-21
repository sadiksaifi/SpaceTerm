//! Capabilities derived from the client's latest admissions.
pub use spaceterm_client::admission::Admissions;
pub use spaceterm_protocol::protocol::capability::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::version::Incompatible;
    use crate::protocol::{CoreVersions, Lane, Negotiated, Refused, version::ProtocolVersion};

    const fn v(major: u16, minor: u16) -> ProtocolVersion {
        ProtocolVersion { major, minor }
    }

    fn core(work: ProtocolVersion) -> CoreVersions {
        CoreVersions {
            work,
            ..crate::protocol::versions_for_tests()
        }
    }

    #[test]
    fn a_machines_capabilities_follow_its_latest_admission_on_each_lane() {
        let mut admissions = Admissions::default();
        assert_eq!(
            admissions.capabilities(false).check(Capability::Restart),
            Err(Unavailable::NotConnected)
        );
        admissions.record(
            Lane::Work,
            &Err(Refused::Incompatible {
                older: Incompatible::ListenerTooOld,
                core: core(v(1, 0)),
            }),
        );
        admissions.record(
            Lane::Service,
            &Ok(Negotiated {
                selected: v(1, 0),
                core: core(v(1, 0)),
            }),
        );
        let capabilities = admissions.capabilities(false);
        assert_eq!(
            capabilities.check(Capability::SwapPanes),
            Err(Unavailable::CoreTooOld)
        );
        assert_eq!(capabilities.check(Capability::Install), Ok(()));
        assert_eq!(admissions.core(), Some(&core(v(1, 0))));

        admissions.record(
            Lane::Work,
            &Err(Refused::Incompatible {
                older: Incompatible::CallerTooOld,
                core: core(v(3, 0)),
            }),
        );
        assert_eq!(
            admissions.capabilities(false).check(Capability::MergeTab),
            Err(Unavailable::ClientTooOld)
        );
        assert_eq!(admissions.core(), Some(&core(v(3, 0))));

        admissions.record(
            Lane::Work,
            &Ok(Negotiated {
                selected: v(1, 0),
                core: core(v(1, 0)),
            }),
        );
        assert_eq!(
            admissions.capabilities(true).check(Capability::MergeTab),
            Ok(())
        );
        assert_eq!(
            admissions.capabilities(false).check(Capability::MergeTab),
            Err(Unavailable::NotConnected),
            "a work link that ended is not connected"
        );
        admissions.record(
            Lane::Service,
            &Err(Refused::Failed(crate::CoreError::NotRunning)),
        );
        assert_eq!(
            admissions.capabilities(true).check(Capability::Uninstall),
            Err(Unavailable::NotConnected)
        );
        assert_eq!(admissions.core(), Some(&core(v(1, 0))));
    }
}
