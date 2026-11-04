//! Version ordering: which versions a Core installs, from any source. Its feed, a desktop push,
//! a local archive and `install.sh` all pass through [`admit`], so no source installs a version
//! that is not newer than the newest one the Core has, or the skipped one.
//!
//! Release versions order by `MAJOR.MINOR.PATCH`. Preflight and Development versions name a
//! commit and have no order, so among them only equality refuses. A Release version and a
//! development version never order against each other.
use super::facts::Version;

/// Why the Core refuses a version from every source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The Core runs this version or a newer one, or has one of them as its Pending Version.
    #[error("SpaceTerm Core has this version or a newer one")]
    NotNewer,
    /// The version failed to start and is skipped until a newer one appears.
    #[error("this SpaceTerm Core version failed to start and is skipped")]
    Skipped,
}

/// Decides `candidate` against `installed`, the versions the Core runs and has as its Pending
/// Version, and `skipped`, the version it refuses until a newer one appears.
pub fn admit(
    candidate: &Version,
    installed: &[Version],
    skipped: Option<&Version>,
) -> Result<(), Refusal> {
    if skipped == Some(candidate) {
        return Err(Refusal::Skipped);
    }
    if installed.contains(candidate) {
        return Err(Refusal::NotNewer);
    }
    let Some(release) = candidate.release() else {
        return Ok(());
    };
    if installed
        .iter()
        .filter_map(Version::release)
        .any(|have| have > release)
    {
        return Err(Refusal::NotNewer);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn release_versions_install_only_when_newer_than_every_installed_one() {
        let installed = [version("1.4.0"), version("1.5.2")];
        assert_eq!(admit(&version("1.5.3"), &installed, None), Ok(()));
        assert_eq!(admit(&version("2.0.0"), &installed, None), Ok(()));
        assert_eq!(
            admit(&version("1.10.0"), &installed, None),
            Ok(()),
            "numbers compare as numbers, not text"
        );
        for refused in ["1.5.2", "1.5.1", "1.4.0", "1.4.9", "0.9.9"] {
            assert_eq!(
                admit(&version(refused), &installed, None),
                Err(Refusal::NotNewer),
                "{refused}"
            );
        }
        assert_eq!(admit(&version("1.0.0"), &[], None), Ok(()));
    }

    #[test]
    fn a_skipped_version_is_refused_until_a_newer_one_appears() {
        let skipped = version("1.5.0");
        assert_eq!(
            admit(&skipped, &[version("1.4.0")], Some(&skipped)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(
                &skipped,
                &[version("1.4.0"), skipped.clone()],
                Some(&skipped)
            ),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(&version("1.5.1"), &[version("1.4.0")], Some(&skipped)),
            Ok(())
        );
    }

    #[test]
    fn development_versions_refuse_only_an_installed_one() {
        let running = version("dev.0123456789ab");
        assert_eq!(
            admit(
                &version("dev.ba9876543210"),
                std::slice::from_ref(&running),
                None
            ),
            Ok(())
        );
        assert_eq!(
            admit(
                &version("dev.0123456789ab.dirty"),
                std::slice::from_ref(&running),
                None
            ),
            Ok(())
        );
        assert_eq!(
            admit(&running, std::slice::from_ref(&running), None),
            Err(Refusal::NotNewer)
        );
        assert_eq!(
            admit(&running, &[version("dev.ba9876543210")], Some(&running)),
            Err(Refusal::Skipped)
        );
        assert_eq!(
            admit(&version("dev.ba9876543210"), &[version("9.9.9")], None),
            Ok(()),
            "a development version never orders against a release one"
        );
        assert_eq!(admit(&version("1.0.0"), &[running], None), Ok(()));
    }
}
