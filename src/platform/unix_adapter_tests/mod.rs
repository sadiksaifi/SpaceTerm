//! Native corpus oracles shared by the POSIX hosts, separate from the portable release gate.
/// The physical short temporary root for native fixtures.
///
/// Socket and process fixtures need short paths, so they use `/tmp` instead of the per-user
/// temporary directory. On macOS `/tmp` is a symbolic link to `/private/tmp`, and the secure
/// filesystem refuses symbolic-link components while `pwd -P` reports physical paths, so fixtures
/// build their paths and expectations from this canonical directory.
pub(crate) fn short_temporary_root() -> &'static std::path::Path {
    static ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| std::fs::canonicalize("/tmp").expect("the host provides /tmp"))
}

pub(crate) fn local_filesystem() -> crate::platform::local_filesystem::LocalFilesystemAuthority {
    crate::platform::local_filesystem::LocalFilesystemAuthority::new(
        crate::local_path::LocalPathSemantics::Posix,
        std::sync::Arc::new(super::unix_local_identity::UnixLocalIdentity),
    )
}

mod selected_file;
