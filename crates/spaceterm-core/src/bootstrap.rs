//! Comparison-only SSH Bootstrap host facts. These observations grant no native authority.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}
impl HostVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
    pub fn at_least(self, major: u16, minor: u16) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapOs {
    MacOs,
    Linux,
    Other,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapArchitecture {
    Arm64,
    X86_64,
    Other,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapLibc {
    NotApplicable,
    Glibc(HostVersion),
    Musl,
    Unknown,
}
/// Read-only observations of the owning account's service manager.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapSupervision {
    LaunchAgent { gui_domain: bool },
    SystemdUser { version: u16, reachable: bool },
    Missing,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapLinger {
    NotApplicable,
    Enabled,
    Disabled,
    Unavailable,
}
/// An installed sudo binary is not an authorization grant or proof of sudoers policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapSudoAvailability {
    Available,
    Unavailable,
}
