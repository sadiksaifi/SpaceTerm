use crate::observation::AcceptanceObservationError;
use crate::observation::{
    LaunchAuthentication, LaunchProof, ObservationTransport, PackagedExecutable, encode_value,
    parse_records, read_frame, write_frame,
};
use std::io;
use std::{
    env,
    fs::File,
    os::fd::AsRawFd,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
pub(crate) struct PackageIdentity {
    executable: PathBuf,
    device: u64,
    inode: u64,
}
pub(crate) fn capture() -> Result<PackageIdentity, AcceptanceObservationError> {
    let executable = env::current_exe()?.canonicalize()?;
    if !executable.ends_with(Path::new("SpaceTerm.app/Contents/MacOS/SpaceTerm")) {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let file = File::open(&executable)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_flags & u32::try_from(libc::MNT_RDONLY).unwrap_or_default() == 0 {
        return Err(AcceptanceObservationError::InvalidChallenge);
    }
    Ok(PackageIdentity {
        executable,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

impl PackageIdentity {
    pub(crate) fn authenticate(
        &self,
        transport: &mut dyn ObservationTransport,
    ) -> Result<LaunchAuthentication, AcceptanceObservationError> {
        let frame = read_frame(transport)?;
        let text = std::str::from_utf8(&frame)
            .map_err(|_| AcceptanceObservationError::InvalidChallenge)?;
        let mut records = parse_records(text)?;
        if records.get(4) != Some(&("package.app.executable.device", self.device.to_string()))
            || records.get(5) != Some(&("package.app.executable.inode", self.inode.to_string()))
        {
            return Err(AcceptanceObservationError::InvalidChallenge);
        }
        records.drain(4..6);
        let mut portable = String::new();
        for (key, value) in records {
            portable.push_str(key);
            portable.push('\t');
            portable.push_str(&encode_value(&value));
            portable.push('\n');
        }
        crate::observation::parse_challenge(portable.as_bytes())
    }
}
impl PackagedExecutable for PackageIdentity {
    fn publish(
        &self,
        transport: &mut dyn ObservationTransport,
        proof: LaunchProof,
    ) -> Result<(), AcceptanceObservationError> {
        let mut frame = proof.prefix().to_owned();
        for (key, value) in [
            ("process.pid", std::process::id().to_string()),
            (
                "process.executable.path",
                self.executable.to_string_lossy().into_owned(),
            ),
            ("process.executable.device", self.device.to_string()),
            ("process.executable.inode", self.inode.to_string()),
        ] {
            frame.push_str(key);
            frame.push('\t');
            frame.push_str(&encode_value(&value));
            frame.push('\n');
        }
        frame.push_str(proof.suffix());
        write_frame(transport, frame.as_bytes()).map_err(Into::into)
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;
    #[test]
    fn source_test_executable_should_not_receive_mounted_package_authority() {
        assert!(capture().is_err());
    }
}
