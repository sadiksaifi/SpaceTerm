//! Strict decoding of read-only account and user-manager observations.

use super::command::MAX_PROBE_BYTES;
use super::process::BootstrapProbeFailure;
use spaceterm_core::bootstrap::*;

#[derive(Clone, Copy, Eq, PartialEq)]
/// Bounded unprivileged host observations, never native authority.
pub struct ReadOnlyHostProbe {
    /// Effective nonroot SSH account.
    pub account_uid: u32,
    /// Observed operating system.
    pub os: BootstrapOs,
    /// Observed machine architecture.
    pub architecture: BootstrapArchitecture,
    /// Operating-system release.
    pub os_version: HostVersion,
    /// Linux libc family and version.
    pub libc: BootstrapLibc,
    /// Running kernel release.
    pub kernel: HostVersion,
    /// Available system supervisor.
    pub supervision: BootstrapSupervision,
    /// Account-wide linger, queried without authorization.
    pub linger: BootstrapLinger,
    /// Fixed sudo binary availability, never a cached authorization.
    pub sudo: BootstrapSudoAvailability,
}
impl ReadOnlyHostProbe {
    /// Exact field order rejects duplicates, omitted fields and trailing output from shell startup.
    pub fn parse(bytes: &[u8], expected_uid: u32) -> Result<Self, BootstrapProbeFailure> {
        if bytes.len() > MAX_PROBE_BYTES {
            return Err(BootstrapProbeFailure::LimitExceeded);
        }
        let invalid = || BootstrapProbeFailure::InvalidProbe;
        if !bytes.is_ascii()
            || bytes
                .iter()
                .any(|byte| *byte != b'\n' && !(0x20..=0x7e).contains(byte))
        {
            return Err(invalid());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let mut lines = text.split('\n');
        if lines.next() != Some("STBP2") {
            return Err(invalid());
        }
        let mut field = |name: &str| -> Result<&str, BootstrapProbeFailure> {
            let line = lines.next().ok_or_else(invalid)?;
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            if key != name || value.is_empty() || value.len() > 128 {
                return Err(invalid());
            }
            Ok(value)
        };
        let uid = number::<u32>(field("uid")?).ok_or_else(invalid)?;
        if uid != expected_uid || matches!(uid, 0 | u32::MAX) {
            return Err(BootstrapProbeFailure::RouteChanged);
        }
        let os = match field("os")? {
            "Darwin" => BootstrapOs::MacOs,
            "Linux" => BootstrapOs::Linux,
            value if token(value) => BootstrapOs::Other,
            _ => return Err(invalid()),
        };
        let architecture = match field("arch")? {
            "arm64" | "aarch64" => BootstrapArchitecture::Arm64,
            "x86_64" => BootstrapArchitecture::X86_64,
            value if token(value) => BootstrapArchitecture::Other,
            _ => return Err(invalid()),
        };
        let os_version = version(field("version")?, false).ok_or_else(invalid)?;
        let libc = match field("libc")? {
            "none" if os == BootstrapOs::MacOs => BootstrapLibc::NotApplicable,
            "musl" => BootstrapLibc::Musl,
            "unknown" => BootstrapLibc::Unknown,
            value if value.starts_with("glibc ") => {
                BootstrapLibc::Glibc(version(&value[6..], false).ok_or_else(invalid)?)
            }
            _ => return Err(invalid()),
        };
        let kernel = version(field("kernel")?, true).ok_or_else(invalid)?;
        let manager = field("manager")?;
        let manager_version = number::<u16>(field("manager_version")?).ok_or_else(invalid)?;
        let reachable = match field("manager_reachable")? {
            "yes" => true,
            "no" => false,
            _ => return Err(invalid()),
        };
        let supervision = match (manager, manager_version, reachable) {
            ("launchagent", 1, gui_domain) if os == BootstrapOs::MacOs => {
                BootstrapSupervision::LaunchAgent { gui_domain }
            }
            ("systemd-user", 1.., reachable) if os == BootstrapOs::Linux => {
                BootstrapSupervision::SystemdUser {
                    version: manager_version,
                    reachable,
                }
            }
            ("missing", 0, false) => BootstrapSupervision::Missing,
            _ => return Err(invalid()),
        };
        let linger = match (os, field("linger")?) {
            (BootstrapOs::MacOs, "none") => BootstrapLinger::NotApplicable,
            (BootstrapOs::Linux, "yes") => BootstrapLinger::Enabled,
            (BootstrapOs::Linux, "no") => BootstrapLinger::Disabled,
            (_, "unavailable") => BootstrapLinger::Unavailable,
            _ => return Err(invalid()),
        };
        let sudo = match field("sudo")? {
            "available" => BootstrapSudoAvailability::Available,
            "unavailable" => BootstrapSudoAvailability::Unavailable,
            _ => return Err(invalid()),
        };
        if lines.next() != Some("") || lines.next().is_some() {
            return Err(invalid());
        }
        Ok(Self {
            account_uid: uid,
            os,
            architecture,
            os_version,
            libc,
            kernel,
            supervision,
            linger,
            sudo,
        })
    }
}
fn token(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
fn number<T: std::str::FromStr>(value: &str) -> Option<T> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}
fn version(value: &str, suffix: bool) -> Option<HostVersion> {
    let (numeric, extra) = value
        .find(['-', '+'])
        .map_or((value, None), |at| (&value[..at], Some(&value[at + 1..])));
    if extra.is_some_and(|rest| !suffix || rest.is_empty() || !token(rest)) {
        return None;
    }
    let mut parts = numeric.split('.');
    let major = number(parts.next()?)?;
    // Ubuntu VERSION_ID uses a zero-padded month, for example 22.04.
    let component = |value: &str| {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse::<u16>().ok()
    };
    let minor = parts.next().map(component).unwrap_or(Some(0))?;
    let patch = parts.next().map(component).unwrap_or(Some(0))?;
    if parts.next().is_some() {
        return None;
    }
    Some(HostVersion::new(major, minor, patch))
}
