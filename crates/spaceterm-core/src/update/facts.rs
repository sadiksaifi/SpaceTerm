//! What one Machine's SpaceTerm Core reports about its updates.
//!
//! These types travel as postcard on the work protocol and as JSON on the service lane. Postcard
//! is not self-describing, so they use no `#[serde(tag)]`, `untagged`, `flatten` or
//! `skip_serializing_if`. A minor may only append enum variants after the existing ones.
use crate::direct::PairingId;
use crate::protocol::version::ProtocolVersion;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// The longest [`Version`], in bytes.
pub const MAX_VERSION_BYTES: usize = 64;
/// The longest [`StrandedDevice::name`], in bytes.
pub const MAX_DEVICE_NAME_BYTES: usize = 192;
/// The most [`PendingVersion::stranded`] entries: one per Paired Device a Core keeps.
pub const MAX_STRANDED: usize = 256;
/// Seconds from [`PendingVersion::installed_at`] until a waiting Pending Version is overdue.
pub const OVERDUE_AFTER: u64 = 2 * 86_400;
/// Seconds from [`PendingVersion::installed_at`] to the value of
/// [`PendingVersion::overdue_since`]. Work and service 1.x fix it at one day, and every peer
/// refuses another value, so it stays apart from [`OVERDUE_AFTER`].
pub const OVERDUE_SINCE_AFTER: u64 = 86_400;

/// What one Machine's SpaceTerm Core reports about its updates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateFacts {
    /// 1 for the first facts of a Core run, then 1 more per change.
    pub revision: u64,
    pub platform: Platform,
    /// The version the Core runs now.
    pub running: Version,
    pub pending: Option<PendingVersion>,
    /// Live Terminal Sessions on this Machine.
    pub sessions: u32,
    pub operation: Option<Operation>,
    pub check: UpdateCheck,
    /// A version this Core refuses from every source until a newer one appears.
    pub skipped: Option<Version>,
    pub last_failure: Option<UpdateFailure>,
}

/// The work and service minor of major 1 that first carries [`Operation::RestartingSameVersion`],
/// [`CheckFailure::ClockBehind`], [`CheckFailure::SteppingStoneSkipped`] and
/// [`StartFailure::Interrupted`].
pub const MINOR_RESTART_FACTS: u16 = 1;

/// Facts that break a bound or an invariant of their types. A receiver treats them as a
/// protocol violation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("update facts break a protocol bound")]
pub struct InvalidFacts;

impl UpdateFacts {
    /// These facts as a connection whose admission selected `selected` reads them. A sender
    /// never sends a value the selected minor lacks (ADR 0035), so each value a later minor
    /// added becomes the nearest one the selected minor has. The work and service lanes add
    /// fact values at the same minors.
    #[must_use]
    pub fn for_selected(mut self, selected: ProtocolVersion) -> Self {
        if selected.minor >= MINOR_RESTART_FACTS {
            return self;
        }
        if self.operation == Some(Operation::RestartingSameVersion) {
            self.operation = Some(Operation::Restarting {
                to: self.running.clone(),
            });
        }
        if let UpdateCheck::Failing { reason, .. } = &mut self.check
            && matches!(
                reason,
                CheckFailure::ClockBehind | CheckFailure::SteppingStoneSkipped
            )
        {
            *reason = CheckFailure::Verification;
        }
        if self
            .last_failure
            .as_ref()
            .is_some_and(|failure| failure.kind == FailureKind::Start(StartFailure::Interrupted))
        {
            self.last_failure = None;
        }
        self
    }

    /// Checks the bounds and invariants that decoding alone does not.
    pub fn validate(&self) -> Result<(), InvalidFacts> {
        if let Some(pending) = &self.pending {
            pending.validate()?;
        }
        match &self.operation {
            Some(Operation::Installing {
                received, total, ..
            }) if received > total => Err(InvalidFacts),
            _ => Ok(()),
        }
    }
}

/// A host platform a SpaceTerm Core build runs on. JSON: `"macos-arm64"`, `"linux-x86_64"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "macos-arm64")]
    MacosArm64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
}

impl Platform {
    /// The platform this build runs on.
    #[cfg(target_os = "macos")]
    pub const HOST: Self = Self::MacosArm64;
    /// The platform this build runs on.
    #[cfg(not(target_os = "macos"))]
    pub const HOST: Self = Self::LinuxX86_64;

    /// The name release assets and feeds use.
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacosArm64 => "macos-arm64",
            Self::LinuxX86_64 => "linux-x86_64",
        }
    }
}

/// `MAJOR.MINOR.PATCH` for Release, or `dev.<12 hex digits>[.dirty]` for Preflight and
/// Development. At most [`MAX_VERSION_BYTES`] bytes. Decoding refuses any other text.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

/// Text that is not a [`Version`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a SpaceTerm version")]
pub struct InvalidVersion;

impl Version {
    pub fn parse(text: &str) -> Result<Self, InvalidVersion> {
        if text.len() <= MAX_VERSION_BYTES && (release_parts(text).is_some() || development(text)) {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidVersion)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `[MAJOR, MINOR, PATCH]` of a Release version. None for a Preflight or Development one.
    pub fn release(&self) -> Option<[u64; 3]> {
        release_parts(&self.0)
    }
}

/// Parses `MAJOR.MINOR.PATCH`: ASCII digits, no leading zeros.
fn release_parts(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = decimal(parts.next()?)?;
    }
    parts.next().is_none().then_some(out)
}

/// An unsigned decimal of ASCII digits with no leading zero.
pub(crate) fn decimal<T: std::str::FromStr>(text: &str) -> Option<T> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let canonical = text == "0" || !text.starts_with('0');
    if digits && canonical {
        text.parse().ok()
    } else {
        None
    }
}

/// `dev.<12 lowercase hex digits>` with an optional `.dirty`.
fn development(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("dev.") else {
        return false;
    };
    let commit = rest.strip_suffix(".dirty").unwrap_or(rest);
    commit.len() == 12
        && commit
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

impl TryFrom<String> for Version {
    type Error = InvalidVersion;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A release's protocol constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protocols {
    pub work: ProtocolVersion,
    pub service: ProtocolVersion,
    pub desktop_channel: ProtocolVersion,
}

impl Protocols {
    /// This build's protocol constants.
    pub const CURRENT: Self = Self {
        work: crate::protocol::version::WORK,
        service: crate::protocol::version::SERVICE,
        desktop_channel: crate::protocol::version::DESKTOP_CHANNEL,
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingVersion {
    pub version: Version,
    /// The Pending Version's own protocol constants.
    pub protocols: Protocols,
    /// Its work or service major differs from the running Core's, so it never restarts
    /// silently and its restart always asks.
    pub breaking: bool,
    /// Unix seconds when it finished installing.
    pub installed_at: u64,
    /// `installed_at + OVERDUE_SINCE_AFTER` once the version is overdue. This Core sets it once
    /// [`OVERDUE_AFTER`] passed; a Core of 0.11.2 or earlier, after one day. A reader decides
    /// with [`PendingVersion::overdue`].
    /// None before that time.
    pub overdue_since: Option<u64>,
    /// Paired Devices that cannot connect after the restart: those whose last admitted Hello
    /// offered no version with the Pending Version's work major. Empty unless breaking.
    pub stranded: Vec<StrandedDevice>,
}

impl PendingVersion {
    /// Whether the version waited [`OVERDUE_AFTER`] at `now`, Unix seconds, whichever Core
    /// reported it.
    pub fn overdue(&self, now: u64) -> bool {
        now.saturating_sub(self.installed_at) >= OVERDUE_AFTER
    }

    fn validate(&self) -> Result<(), InvalidFacts> {
        let overdue = self
            .overdue_since
            .is_none_or(|since| Some(since) == self.installed_at.checked_add(OVERDUE_SINCE_AFTER));
        let stranded = self.stranded.len() <= MAX_STRANDED
            && (self.breaking || self.stranded.is_empty())
            && self
                .stranded
                .iter()
                .all(|device| device.name.len() <= MAX_DEVICE_NAME_BYTES);
        if overdue && stranded {
            Ok(())
        } else {
            Err(InvalidFacts)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrandedDevice {
    /// JSON: 64 lowercase hex digits.
    #[serde(with = "pairing_text")]
    pub pairing: PairingId,
    /// The Paired Device's name, at most [`MAX_DEVICE_NAME_BYTES`] bytes.
    pub name: String,
}

/// A [`PairingId`] as 64 lowercase hex digits in JSON and as its 32 bytes in postcard.
mod pairing_text {
    use super::*;
    use serde::de::Error;

    pub fn serialize<S: Serializer>(id: &PairingId, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            let text: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
            serializer.serialize_str(&text)
        } else {
            id.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PairingId, D::Error> {
        if !deserializer.is_human_readable() {
            return PairingId::deserialize(deserializer);
        }
        let text = String::deserialize(deserializer)?;
        lowercase_hex(&text).ok_or_else(|| D::Error::custom("not a pairing ID"))
    }
}

/// Exactly `N` bytes as `2 * N` lowercase hex digits.
pub(crate) fn lowercase_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    if bytes.len() != 2 * N {
        return None;
    }
    let mut out = [0; N];
    for (slot, [high, low]) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *slot = digit(*high)? << 4 | digit(*low)?;
    }
    Some(out)
}

/// The update operation a Core runs now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Checking,
    /// A feed download or a desktop push, then verification and unpacking. `received` equals
    /// `total` while it verifies and unpacks.
    Installing {
        version: Version,
        received: u64,
        total: u64,
    },
    /// The helper restarts the Core into its Pending Version `to`: Update Core. The Core closes
    /// its connections next.
    Restarting {
        to: Version,
    },
    Stopping,
    /// Remove Machine: the Core stops, revokes every pairing and removes itself. It does not
    /// come back.
    Uninstalling,
    /// The helper restarts the Core into the version it runs: Restart Core, with no version
    /// change. Since [`MINOR_RESTART_FACTS`].
    RestartingSameVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateCheck {
    /// This build identity never fetches a feed: Preflight and Development.
    Disabled,
    /// No check has finished since installation.
    Never,
    /// The last check succeeded at `at`.
    Succeeded { at: u64 },
    /// Every check since `since` failed.
    Failing {
        since: u64,
        last_success: Option<u64>,
        reason: CheckFailure,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckFailure {
    /// DNS, connection, TLS or HTTP failure with no proxy configured.
    Network,
    /// The configured proxy refused the request or did not answer.
    Proxy,
    /// The feed or its signature did not verify, or the feed broke a field rule.
    Verification,
    /// The newest release needs a newer system than this host, per `minimum_system`.
    SystemTooOld,
    /// The feed verified, but it was published more than [`super::feed::MAX_CLOCK_SKEW`]
    /// seconds after this Machine's clock: the clock is behind. Since [`MINOR_RESTART_FACTS`].
    ClockBehind,
    /// The newest release needs a Stepping Stone first, and that Stepping Stone is `skipped`
    /// because it failed to start. The Core can't reach the newest release from its feed until
    /// a newer Stepping Stone appears. Since [`MINOR_RESTART_FACTS`].
    SteppingStoneSkipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateFailure {
    /// Increases per Core and survives restarts. A client retires its notice for one Machine
    /// and id, on that client only.
    pub id: u64,
    /// Unix seconds.
    pub at: u64,
    pub version: Version,
    pub kind: FailureKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// "Install failed": nothing changed, and the Core tries the install again later.
    Install(InstallFailure),
    /// The Pending Version did not become ready. The Core restored its retained state and runs
    /// `running` again.
    Start(StartFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFailure {
    DiskFull,
    Download,
    Verification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartFailure {
    /// "Update failed": the version is now `skipped`.
    NotReady,
    /// Transient: the version stays the Pending Version, and the next restart tries again.
    PortInUse,
    /// Transient, as `PortInUse`.
    DiskFull,
    /// Transient: a reboot, logout or crash ended the restart before the version could become
    /// ready, so its readiness was never measured. Since [`MINOR_RESTART_FACTS`].
    Interrupted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn pending(breaking: bool) -> PendingVersion {
        PendingVersion {
            version: version("1.4.0"),
            protocols: Protocols {
                work: ProtocolVersion { major: 2, minor: 1 },
                service: ProtocolVersion { major: 1, minor: 3 },
                desktop_channel: ProtocolVersion { major: 1, minor: 0 },
            },
            breaking,
            installed_at: 1_793_750_400,
            overdue_since: Some(1_793_750_400 + OVERDUE_SINCE_AFTER),
            stranded: if breaking {
                vec![StrandedDevice {
                    pairing: [0x9c; 32],
                    name: "old-mini".into(),
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn facts() -> UpdateFacts {
        UpdateFacts {
            revision: 7,
            platform: Platform::LinuxX86_64,
            running: version("dev.0123456789ab.dirty"),
            pending: Some(pending(true)),
            sessions: 3,
            operation: Some(Operation::Installing {
                version: version("1.4.1"),
                received: 10,
                total: 20,
            }),
            check: UpdateCheck::Failing {
                since: 100,
                last_success: Some(50),
                reason: CheckFailure::Proxy,
            },
            skipped: Some(version("1.3.9")),
            last_failure: Some(UpdateFailure {
                id: 4,
                at: 90,
                version: version("1.3.9"),
                kind: FailureKind::Start(StartFailure::NotReady),
            }),
        }
    }

    #[test]
    fn facts_survive_postcard_and_json() {
        let facts = facts();
        let bytes = postcard::to_allocvec(&facts).unwrap();
        assert_eq!(postcard::from_bytes::<UpdateFacts>(&bytes).unwrap(), facts);
        let json = serde_json::to_string(&facts).unwrap();
        assert_eq!(serde_json::from_str::<UpdateFacts>(&json).unwrap(), facts);
        for check in [
            UpdateCheck::Disabled,
            UpdateCheck::Never,
            UpdateCheck::Succeeded { at: 3 },
        ] {
            let facts = UpdateFacts {
                check,
                operation: Some(Operation::Restarting {
                    to: version("2.0.0"),
                }),
                ..facts.clone()
            };
            let bytes = postcard::to_allocvec(&facts).unwrap();
            assert_eq!(postcard::from_bytes::<UpdateFacts>(&bytes).unwrap(), facts);
        }
    }

    #[test]
    fn a_connection_at_minor_0_reads_each_later_value_as_the_nearest_one_it_has() {
        let older = ProtocolVersion { major: 1, minor: 0 };
        let current = ProtocolVersion {
            major: 1,
            minor: MINOR_RESTART_FACTS,
        };
        let later = UpdateFacts {
            operation: Some(Operation::RestartingSameVersion),
            check: UpdateCheck::Failing {
                since: 100,
                last_success: None,
                reason: CheckFailure::SteppingStoneSkipped,
            },
            last_failure: Some(UpdateFailure {
                id: 5,
                at: 90,
                version: version("1.4.0"),
                kind: FailureKind::Start(StartFailure::Interrupted),
            }),
            ..facts()
        };
        assert_eq!(later.clone().for_selected(current), later);
        let read = later.for_selected(older);
        assert_eq!(
            read.operation,
            Some(Operation::Restarting {
                to: version("dev.0123456789ab.dirty")
            })
        );
        assert!(matches!(
            read.check,
            UpdateCheck::Failing {
                reason: CheckFailure::Verification,
                ..
            }
        ));
        assert_eq!(read.last_failure, None);
        // Values minor 0 has pass unchanged.
        assert_eq!(facts().for_selected(older), facts());
        let bytes = postcard::to_allocvec(&read).unwrap();
        assert_eq!(postcard::from_bytes::<UpdateFacts>(&bytes).unwrap(), read);
    }

    #[test]
    fn overdue_facts_keep_the_value_every_1_x_peer_accepts() {
        // A Core of 0.11.2 sets `overdue_since` one day after the install.
        let released = PendingVersion {
            overdue_since: Some(1_793_750_400 + 86_400),
            ..pending(false)
        };
        assert_eq!(released.validate(), Ok(()));
        // Overdue follows the two days, whichever Core reported the version.
        assert!(!released.overdue(1_793_750_400 + 86_400));
        assert!(released.overdue(1_793_750_400 + OVERDUE_AFTER));
        let two_days = PendingVersion {
            overdue_since: Some(1_793_750_400 + OVERDUE_AFTER),
            ..pending(false)
        };
        assert_eq!(two_days.validate(), Err(InvalidFacts));
    }

    #[test]
    fn json_names_platforms_and_pairings_as_text() {
        let json = serde_json::to_value(facts()).unwrap();
        assert_eq!(json["platform"], "linux-x86_64");
        assert_eq!(json["pending"]["stranded"][0]["pairing"], "9c".repeat(32));
        assert_eq!(
            serde_json::to_value(Platform::MacosArm64).unwrap(),
            "macos-arm64"
        );
    }

    #[test]
    fn json_refuses_unknown_fields_and_malformed_pairings() {
        let mut json = serde_json::to_value(facts()).unwrap();
        json["staged"] = serde_json::json!([]);
        assert!(serde_json::from_value::<UpdateFacts>(json).is_err());
        let mut json = serde_json::to_value(facts()).unwrap();
        json["pending"]["stranded"][0]["pairing"] = "9C".repeat(32).into();
        assert!(serde_json::from_value::<UpdateFacts>(json).is_err());
    }

    #[test]
    fn versions_are_release_or_development_text() {
        assert_eq!(version("10.0.20").release(), Some([10, 0, 20]));
        assert_eq!(version("dev.0123456789ab").release(), None);
        for refused in [
            "",
            "1.4",
            "1.4.0.1",
            "01.4.0",
            "1.4.0-rc1",
            "v1.4.0",
            "+1.4.0",
            "dev.0123456789AB",
            "dev.0123456789a",
            "dev.0123456789ab.clean",
            "99999999999999999999.0.0",
        ] {
            assert_eq!(Version::parse(refused), Err(InvalidVersion), "{refused:?}");
        }
        let long = format!("1.0.{}", "1".repeat(61));
        assert_eq!(Version::parse(&long), Err(InvalidVersion));
        let bytes = postcard::to_allocvec("1.04.0").unwrap();
        assert!(postcard::from_bytes::<Version>(&bytes).is_err());
    }

    #[test]
    fn validation_refuses_facts_that_break_an_invariant() {
        assert_eq!(facts().validate(), Ok(()));
        let broken = [
            UpdateFacts {
                operation: Some(Operation::Installing {
                    version: version("1.4.1"),
                    received: 21,
                    total: 20,
                }),
                ..facts()
            },
            UpdateFacts {
                pending: Some(PendingVersion {
                    stranded: pending(true).stranded,
                    ..pending(false)
                }),
                ..facts()
            },
            UpdateFacts {
                pending: Some(PendingVersion {
                    overdue_since: Some(1_793_750_400 + OVERDUE_SINCE_AFTER - 1),
                    ..pending(false)
                }),
                ..facts()
            },
            UpdateFacts {
                pending: Some(PendingVersion {
                    stranded: vec![StrandedDevice {
                        pairing: [1; 32],
                        name: "n".repeat(MAX_DEVICE_NAME_BYTES + 1),
                    }],
                    ..pending(true)
                }),
                ..facts()
            },
        ];
        for facts in broken {
            assert_eq!(facts.validate(), Err(InvalidFacts));
        }
    }
}
