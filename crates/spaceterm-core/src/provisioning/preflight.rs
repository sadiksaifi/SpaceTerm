//! SSH preflight: what Add Machine learns about a computer before it uploads anything.
//!
//! The desktop runs [`SCRIPT`] over SSH right after authentication, for a new installation and
//! an existing one alike. It prints one [`MARKER`] line of facts. [`Preflight::parse`] reads
//! that line, and [`Preflight::refusal`] decides whether setup continues.
use super::{MINIMUM_GLIBC, MINIMUM_MACOS, SetupRefusal};

/// The first word of the script's facts line.
pub const MARKER: &str = "SPACETERM_PREFLIGHT";
/// The largest facts reply the desktop reads, in bytes.
pub const MAX_REPLY: usize = 1024;

/// A POSIX shell script that changes nothing. It prints exactly one line:
/// `SPACETERM_PREFLIGHT 1 os=.. arch=.. uid=.. system=.. manager=0|1 linger=0|1 gui=0|1
/// filevault=0|1`. `system` is the macOS version on a Mac and the glibc version on Linux, `0`
/// when unknown. `arch` is `arm64` on Apple silicon even under Rosetta.
pub const SCRIPT: &str = r#"set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
uid=$(id -u)
os=other arch=other system=0 manager=0 linger=0 gui=0 filevault=0
case "$(uname -s)" in
Darwin)
  os=macos
  if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then arch=arm64; else arch=x86_64; fi
  system=$(sw_vers -productVersion 2>/dev/null) || system=0
  launchctl print "gui/$uid" >/dev/null 2>&1 && gui=1
  [ "$(fdesetup isactive 2>/dev/null)" = true ] && filevault=1
  ;;
Linux)
  os=linux
  arch=$(uname -m)
  system=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc //p')
  [ -n "$system" ] || system=0
  XDG_RUNTIME_DIR="/run/user/$uid" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" \
    /usr/bin/systemctl --user --no-pager show-environment >/dev/null 2>&1 && manager=1
  [ "$(/usr/bin/loginctl show-user "$uid" --property=Linger --value 2>/dev/null)" = yes ] && linger=1
  ;;
esac
printf 'SPACETERM_PREFLIGHT 1 os=%s arch=%s uid=%s system=%s manager=%s linger=%s gui=%s filevault=%s\n' \
  "$os" "$arch" "$uid" "$system" "$manager" "$linger" "$gui" "$filevault"
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Macos,
    Linux,
    Other,
}

/// The facts one preflight reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preflight {
    pub system: System,
    /// `arm64` on a Mac, `x86_64` on Linux.
    pub supported_architecture: bool,
    pub uid: u32,
    /// The macOS or glibc version, as (major, minor). `None` when unknown.
    pub version: Option<(u16, u16)>,
    /// Linux: the account's systemd user manager answered.
    pub user_manager: bool,
    /// Linux: `loginctl` reports linger for the account.
    pub linger: bool,
    /// macOS: the account has an Aqua session.
    pub logged_in: bool,
    /// macOS: FileVault is on.
    pub filevault: bool,
}

/// The reply was not one well-formed facts line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

impl Preflight {
    /// Reads the script's output. Text around the one facts line is refused.
    pub fn parse(output: &str) -> Result<Self, Malformed> {
        if output.len() > MAX_REPLY {
            return Err(Malformed);
        }
        let line = output.strip_suffix('\n').ok_or(Malformed)?;
        if line.contains('\n') {
            return Err(Malformed);
        }
        let mut words = line.split(' ');
        if words.next() != Some(MARKER) || words.next() != Some("1") {
            return Err(Malformed);
        }
        let mut field = |name: &str| -> Result<&str, Malformed> {
            words
                .next()
                .and_then(|word| word.strip_prefix(name))
                .and_then(|word| word.strip_prefix('='))
                .ok_or(Malformed)
        };
        let flag = |value: &str| match value {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(Malformed),
        };
        let system = match field("os")? {
            "macos" => System::Macos,
            "linux" => System::Linux,
            "other" => System::Other,
            _ => return Err(Malformed),
        };
        let architecture = field("arch")?;
        let uid = field("uid")?.parse::<u32>().map_err(|_| Malformed)?;
        let version = version(field("system")?);
        let user_manager = flag(field("manager")?)?;
        let linger = flag(field("linger")?)?;
        let logged_in = flag(field("gui")?)?;
        let filevault = flag(field("filevault")?)?;
        if words.next().is_some() {
            return Err(Malformed);
        }
        Ok(Self {
            system,
            supported_architecture: match system {
                System::Macos => architecture == "arm64",
                System::Linux => architecture == "x86_64",
                System::Other => false,
            },
            uid,
            version,
            user_manager,
            linger,
            logged_in,
            filevault,
        })
    }

    /// The first refusal that applies, or `None` when setup may continue. A remote Linux machine
    /// without linger is not refused here: the desktop asks to authorize it with
    /// [`Preflight::needs_linger`], and refuses with [`SetupRefusal::LingerDeclined`] when the
    /// user declines.
    pub fn refusal(&self) -> Option<SetupRefusal> {
        let at_least = |minimum: (u16, u16)| self.version.is_some_and(|version| version >= minimum);
        match self.system {
            System::Other => Some(SetupRefusal::OtherSystem),
            System::Macos if !self.supported_architecture => Some(SetupRefusal::IntelMac),
            System::Linux if !self.supported_architecture => Some(SetupRefusal::ArmLinux),
            System::Macos if !at_least(MINIMUM_MACOS) => Some(SetupRefusal::MacosTooOld {
                found: self.version,
            }),
            System::Linux if !at_least(MINIMUM_GLIBC) => Some(SetupRefusal::GlibcTooOld {
                found: self.version,
            }),
            _ if self.uid == 0 => Some(SetupRefusal::RootAccount),
            System::Linux if !self.user_manager => Some(SetupRefusal::NoUserManager),
            System::Macos if !self.logged_in => Some(SetupRefusal::NobodyLoggedIn {
                filevault: self.filevault,
            }),
            System::Macos | System::Linux => None,
        }
    }

    /// Linux without linger. Remote Linux requires it before anything is uploaded.
    pub fn needs_linger(&self) -> bool {
        self.system == System::Linux && !self.linger
    }
}

/// `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`, ignoring the patch. The script's `0` means unknown.
fn version(text: &str) -> Option<(u16, u16)> {
    if text == "0" {
        return None;
    }
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |part| part.parse().ok())?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(fields: &str) -> String {
        format!("{MARKER} 1 {fields}\n")
    }

    fn refusal(fields: &str) -> Option<SetupRefusal> {
        Preflight::parse(&facts(fields)).unwrap().refusal()
    }

    const MAC: &str =
        "os=macos arch=arm64 uid=501 system=26.1 manager=0 linger=0 gui=1 filevault=0";
    const LINUX: &str =
        "os=linux arch=x86_64 uid=1000 system=2.39 manager=1 linger=1 gui=0 filevault=0";

    #[test]
    fn a_supported_logged_in_computer_continues() {
        assert_eq!(refusal(MAC), None);
        assert_eq!(refusal(LINUX), None);
        assert!(!Preflight::parse(&facts(LINUX)).unwrap().needs_linger());
    }

    #[test]
    fn each_unsupported_computer_has_its_own_refusal() {
        let cases = [
            (
                MAC.replace("arch=arm64", "arch=x86_64"),
                SetupRefusal::IntelMac,
            ),
            (
                LINUX.replace("arch=x86_64", "arch=aarch64"),
                SetupRefusal::ArmLinux,
            ),
            (
                LINUX.replace("os=linux", "os=other"),
                SetupRefusal::OtherSystem,
            ),
            (
                MAC.replace("system=26.1", "system=15.7.2"),
                SetupRefusal::MacosTooOld {
                    found: Some((15, 7)),
                },
            ),
            (
                LINUX.replace("system=2.39", "system=2.31"),
                SetupRefusal::GlibcTooOld {
                    found: Some((2, 31)),
                },
            ),
            (
                LINUX.replace("system=2.39", "system=0"),
                SetupRefusal::GlibcTooOld { found: None },
            ),
            (
                LINUX.replace("system=2.39", "system=musl"),
                SetupRefusal::GlibcTooOld { found: None },
            ),
            (
                LINUX.replace("uid=1000", "uid=0"),
                SetupRefusal::RootAccount,
            ),
            (MAC.replace("uid=501", "uid=0"), SetupRefusal::RootAccount),
            (
                LINUX.replace("manager=1", "manager=0"),
                SetupRefusal::NoUserManager,
            ),
            (
                MAC.replace("gui=1", "gui=0"),
                SetupRefusal::NobodyLoggedIn { filevault: false },
            ),
            (
                MAC.replace("gui=1 filevault=0", "gui=0 filevault=1"),
                SetupRefusal::NobodyLoggedIn { filevault: true },
            ),
        ];
        for (fields, expected) in cases {
            assert_eq!(refusal(&fields), Some(expected), "{fields}");
        }
    }

    #[test]
    fn the_platform_refusal_comes_before_the_account_refusal() {
        let fields = LINUX
            .replace("arch=x86_64", "arch=aarch64")
            .replace("uid=1000", "uid=0");
        assert_eq!(refusal(&fields), Some(SetupRefusal::ArmLinux));
    }

    #[test]
    fn linux_without_linger_continues_only_after_authorization() {
        let preflight = Preflight::parse(&facts(&LINUX.replace("linger=1", "linger=0"))).unwrap();
        assert_eq!(preflight.refusal(), None);
        assert!(preflight.needs_linger());
        let mac = Preflight::parse(&facts(MAC)).unwrap();
        assert!(!mac.needs_linger());
    }

    #[test]
    fn anything_but_one_facts_line_is_malformed() {
        for output in [
            String::new(),
            facts(MAC).trim_end().to_owned(),
            format!("motd\n{}", facts(MAC)),
            format!("{}extra\n", facts(MAC)),
            facts(&format!("{MAC} more=1")),
            facts(&MAC.replace("gui=1", "gui=yes")),
            facts(&MAC.replace("uid=501", "uid=-1")),
            format!("{MARKER} 2 {MAC}\n"),
        ] {
            assert_eq!(Preflight::parse(&output), Err(Malformed), "{output:?}");
        }
    }

    #[test]
    fn the_refusal_serializes_with_its_flag() {
        let json =
            serde_json::to_string(&SetupRefusal::NobodyLoggedIn { filevault: true }).unwrap();
        assert_eq!(json, r#"{"refusal":"nobody_logged_in","filevault":true}"#);
        let json = serde_json::to_string(&SetupRefusal::UdpBlocked { port: 9231 }).unwrap();
        assert_eq!(json, r#"{"refusal":"udp_blocked","port":9231}"#);
        let json = serde_json::to_string(&SetupRefusal::GlibcTooOld {
            found: Some((2, 31)),
        })
        .unwrap();
        assert_eq!(json, r#"{"refusal":"glibc_too_old","found":[2,31]}"#);
    }
}
