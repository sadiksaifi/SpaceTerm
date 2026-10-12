use super::*;
impl LocalFilesystemAuthority {
    pub fn testing() -> Self {
        Self::new(
            crate::local_path::LocalPathSemantics::Posix,
            Arc::new(FixtureIdentities),
        )
    }

    pub fn testing_without_access() -> Self {
        struct NoAccess;
        impl LocalIdentitySource for NoAccess {
            fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
                panic!("this context has no local filesystem authority")
            }
        }
        Self::new(
            crate::local_path::LocalPathSemantics::Posix,
            Arc::new(NoAccess),
        )
    }

    pub fn testing_with_failure(error: LocalFilesystemError) -> Self {
        struct Unavailable(LocalFilesystemError);
        impl LocalIdentitySource for Unavailable {
            fn identify(&self, _: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
                Err(self.0)
            }
        }
        Self::new(
            crate::local_path::LocalPathSemantics::Posix,
            Arc::new(Unavailable(error)),
        )
    }

    pub fn same_source(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}

/// Logical path identity for policy tests, not native replacement authority.
struct FixtureIdentities;
impl LocalIdentitySource for FixtureIdentities {
    fn identify(&self, path: &Path) -> Result<LocalIdentityObservation, LocalFilesystemError> {
        let canonical = path.canonicalize().map_err(classify_io_error)?;
        let metadata = fs::metadata(&canonical).map_err(classify_io_error)?;
        let kind = if metadata.is_dir() {
            LocalObjectKind::Directory
        } else if metadata.is_file() {
            LocalObjectKind::File
        } else {
            LocalObjectKind::Other
        };
        Ok(LocalIdentityObservation {
            identity: LocalObjectIdentity(IdentityValue::FixturePath(canonical), None),
            kind,
        })
    }
}
