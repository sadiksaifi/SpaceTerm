//! The signed release feed and the release asset names.
//!
//! A feed is compact UTF-8 JSON of at most [`MAX_FEED`] bytes beside a `.sig` of exactly 64 raw
//! Ed25519 bytes over the feed's exact bytes. [`read`] verifies that signature before it parses
//! any field, then refuses unknown fields and every value that breaks a field rule. It never
//! decides whether the version is newer, skipped or runnable on this host: the updater does.
use super::facts::{Platform, Protocols, Version, decimal, lowercase_hex};
use super::ordering::{Candidate, Floor};
use super::trust::{SIGNATURE_BYTES, UpdateTrust};
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Where every release asset lives.
pub const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";
/// The largest feed document, in bytes.
pub const MAX_FEED: usize = 4096;
/// The largest archive a feed names, in bytes.
pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
/// How far ahead of the reader's clock `published_at` may be, in seconds.
pub const MAX_CLOCK_SKEW: u64 = 300;
/// The only feed schema.
pub const SCHEMA: u32 = 1;
/// The macOS desktop's Sparkle feed. It carries no protocols and is no [`Feed`].
pub const APPCAST: &str = "appcast.xml";

/// The macOS desktop's disk image for `version`.
pub fn desktop_dmg(version: &Version) -> String {
    format!("SpaceTerm-{version}-macos-arm64.dmg")
}

/// The release notes page of `version`, which every feed names.
pub fn notes(version: &Version) -> String {
    format!("{RELEASES}/tag/v{version}")
}

/// A product with release assets. JSON: `"core"`, `"desktop"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    Core,
    Desktop,
}

/// A product and platform that publish a JSON feed. The macOS desktop publishes [`APPCAST`]
/// instead, so no target names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FeedTarget {
    product: Product,
    platform: Platform,
}

impl FeedTarget {
    pub const CORE_MACOS: Self = Self {
        product: Product::Core,
        platform: Platform::MacosArm64,
    };
    pub const CORE_LINUX: Self = Self {
        product: Product::Core,
        platform: Platform::LinuxX86_64,
    };
    pub const DESKTOP_LINUX: Self = Self {
        product: Product::Desktop,
        platform: Platform::LinuxX86_64,
    };

    /// None for the macOS desktop, which has no JSON feed.
    pub const fn new(product: Product, platform: Platform) -> Option<Self> {
        match (product, platform) {
            (Product::Desktop, Platform::MacosArm64) => None,
            _ => Some(Self { product, platform }),
        }
    }

    /// The SpaceTerm Core feed of `platform`.
    pub const fn core(platform: Platform) -> Self {
        Self {
            product: Product::Core,
            platform,
        }
    }

    pub const fn product(self) -> Product {
        self.product
    }

    pub const fn platform(self) -> Platform {
        self.platform
    }

    fn stem(self) -> String {
        let product = match self.product {
            Product::Core => "core",
            Product::Desktop => "desktop",
        };
        format!("spaceterm-{product}")
    }

    /// `spaceterm-<product>-<platform>.json`.
    pub fn feed(self) -> String {
        format!("{}-{}.json", self.stem(), self.platform.name())
    }

    /// The feed's detached signature.
    pub fn feed_signature(self) -> String {
        format!("{}.sig", self.feed())
    }

    /// `spaceterm-<product>-<version>-<platform>.tar.gz`.
    pub fn archive(self, version: &Version) -> String {
        format!("{}-{version}-{}.tar.gz", self.stem(), self.platform.name())
    }

    /// The archive's detached signature.
    pub fn archive_signature(self, version: &Version) -> String {
        format!("{}.sig", self.archive(version))
    }

    /// Where a reader fetches the feed of `release`, or of the newest release for None.
    pub fn feed_url(self, release: Option<&Version>) -> String {
        format!("{}/{}", download(release), self.feed())
    }

    /// Where a reader fetches the signature of the feed of `release`, or of the newest release
    /// for None.
    pub fn feed_signature_url(self, release: Option<&Version>) -> String {
        format!("{}/{}", download(release), self.feed_signature())
    }

    /// Where a reader fetches `version`'s archive.
    pub fn archive_url(self, version: &Version) -> String {
        format!("{RELEASES}/download/v{version}/{}", self.archive(version))
    }
}

/// Where the assets of `release` live, or of the newest release for None.
fn download(release: Option<&Version>) -> String {
    match release {
        Some(version) => format!("{RELEASES}/download/v{version}"),
        None => format!("{RELEASES}/latest/download"),
    }
}

/// The most releases an updater steps down through below the newest one in one check. Each step
/// lowers the version, so a valid chain ends without it. It only bounds a host that keeps
/// answering.
pub const MAX_STEPS: usize = 64;

/// The release an updater installs next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Next {
    pub feed: Feed,
    /// The release is the floor of a newer one, which the updater reaches through it.
    pub stepping_stone: bool,
}

/// The release an updater running `running` installs next. `read(None)` reads the newest
/// release's verified feed, and `read(Some(version))` the feed of `version`'s release. While
/// `running` is below a release's Update Floor, the updater reads the release at that floor, and
/// it takes the first release whose floor `running` meets.
///
/// Ok(None) when the floors lead to no release `running` may install: a release names its own
/// version as its floor, the release at a floor names another version, or the chain is longer
/// than [`MAX_STEPS`].
///
/// # Errors
/// Returns what `read` returns.
pub fn walk<E>(
    running: &Version,
    mut read: impl FnMut(Option<&Version>) -> Result<Feed, E>,
) -> Result<Option<Next>, E> {
    let mut feed = read(None)?;
    let mut steps = 0;
    loop {
        if feed.floor.admits(running) {
            return Ok(Some(Next {
                feed,
                stepping_stone: steps > 0,
            }));
        }
        let floor = feed.floor.version();
        if floor == feed.version || steps == MAX_STEPS {
            return Ok(None);
        }
        feed = read(Some(&floor))?;
        if feed.version != floor {
            return Ok(None);
        }
        steps += 1;
    }
}

/// The oldest host an archive runs on: the macOS version on `macos-arm64`, the glibc version on
/// `linux-x86_64`. JSON: `"<major>.<minor>"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemVersion {
    pub major: u16,
    pub minor: u16,
}

impl SystemVersion {
    fn parse(text: &str) -> Option<Self> {
        let (major, minor) = text.split_once('.')?;
        Some(Self {
            major: decimal(major)?,
            minor: decimal(minor)?,
        })
    }
}

/// A verified feed whose fields all keep their rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    pub target: FeedTarget,
    /// A Release version: `MAJOR.MINOR.PATCH`.
    pub version: Version,
    /// The release's Update Floor, never newer than `version`.
    pub floor: Floor,
    /// Unix seconds.
    pub published_at: u64,
    pub protocols: Protocols,
    pub minimum_system: SystemVersion,
    pub archive: FeedArchive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedArchive {
    /// [`FeedTarget::archive`] of the feed's version.
    pub name: String,
    /// Bytes, 1 to [`MAX_ARCHIVE`].
    pub size: u64,
    pub sha256: [u8; 32],
    /// The archive's Ed25519 signature, the same bytes as its `.sig` asset.
    pub signature: [u8; SIGNATURE_BYTES],
}

/// Why a feed was refused. Every variant is a verification failure to the updater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("the feed is larger than its limit")]
    TooLarge,
    #[error("the feed's signature does not verify")]
    Signature,
    #[error("the feed is not a feed document")]
    Malformed,
    #[error("the feed's schema is unknown")]
    Schema,
    #[error("the feed names another product or platform")]
    Target,
    #[error("the feed's version is not a release version")]
    Version,
    #[error("the feed's floor is not a release version at most its own")]
    Floor,
    #[error("the feed's publication time is out of range")]
    PublishedAt,
    #[error("the feed's minimum system is malformed")]
    MinimumSystem,
    #[error("the feed's release notes link is wrong")]
    Notes,
    #[error("the feed names another archive")]
    ArchiveName,
    #[error("the feed's archive size is out of range")]
    ArchiveSize,
    #[error("the feed's archive digest is malformed")]
    ArchiveDigest,
    #[error("the feed's archive signature is malformed")]
    ArchiveSignature,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    product: Product,
    platform: Platform,
    version: String,
    floor: String,
    published_at: u64,
    protocols: Protocols,
    minimum_system: String,
    notes: String,
    archive: DocumentArchive,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentArchive {
    name: String,
    size: u64,
    sha256: String,
    signature: String,
}

/// Verifies and reads the feed fetched for `target`, at `now` in Unix seconds.
pub fn read(
    target: FeedTarget,
    feed: &[u8],
    signature: &[u8],
    trust: &UpdateTrust,
    now: u64,
) -> Result<Feed, FeedError> {
    if feed.len() > MAX_FEED {
        return Err(FeedError::TooLarge);
    }
    trust
        .verify(feed, signature)
        .map_err(|_| FeedError::Signature)?;
    let document: Document = serde_json::from_slice(feed).map_err(|_| FeedError::Malformed)?;
    if document.schema != SCHEMA {
        return Err(FeedError::Schema);
    }
    if FeedTarget::new(document.product, document.platform) != Some(target) {
        return Err(FeedError::Target);
    }
    let version = Version::parse(&document.version)
        .ok()
        .filter(|version| version.release().is_some())
        .ok_or(FeedError::Version)?;
    let floor = Floor::parse(&document.floor)
        .ok()
        .filter(|floor| Candidate::new(version.clone(), Some(*floor)).is_ok())
        .ok_or(FeedError::Floor)?;
    if document.published_at == 0 || document.published_at > now.saturating_add(MAX_CLOCK_SKEW) {
        return Err(FeedError::PublishedAt);
    }
    let minimum_system =
        SystemVersion::parse(&document.minimum_system).ok_or(FeedError::MinimumSystem)?;
    if document.notes != notes(&version) {
        return Err(FeedError::Notes);
    }
    let archive = document.archive;
    if archive.name != target.archive(&version) {
        return Err(FeedError::ArchiveName);
    }
    if !(1..=MAX_ARCHIVE).contains(&archive.size) {
        return Err(FeedError::ArchiveSize);
    }
    let sha256 = lowercase_hex(&archive.sha256).ok_or(FeedError::ArchiveDigest)?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(&archive.signature)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(FeedError::ArchiveSignature)?;
    Ok(Feed {
        target,
        version,
        floor,
        published_at: document.published_at,
        protocols: document.protocols,
        minimum_system,
        archive: FeedArchive {
            name: archive.name,
            size: archive.size,
            sha256,
            signature,
        },
    })
}

impl Feed {
    /// The release as a source offers it to the engine: its version and Update Floor.
    pub fn candidate(&self) -> Candidate {
        Candidate::new(self.version.clone(), Some(self.floor))
            .expect("a read feed keeps its floor rule")
    }

    /// The feed document: compact UTF-8 JSON with no trailing newline.
    pub fn to_bytes(&self) -> Vec<u8> {
        let hex: String = self
            .archive
            .sha256
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let document = Document {
            schema: SCHEMA,
            product: self.target.product,
            platform: self.target.platform,
            version: self.version.to_string(),
            floor: self.floor.to_string(),
            published_at: self.published_at,
            protocols: self.protocols,
            minimum_system: format!(
                "{}.{}",
                self.minimum_system.major, self.minimum_system.minor
            ),
            notes: notes(&self.version),
            archive: DocumentArchive {
                name: self.archive.name.clone(),
                size: self.archive.size,
                sha256: hex,
                signature: base64::engine::general_purpose::STANDARD.encode(self.archive.signature),
            },
        };
        serde_json::to_vec(&document).expect("a feed document always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::super::trust::testing::{TestKey, trust};
    use super::*;
    use crate::protocol::version::ProtocolVersion;
    use serde_json::{Value, json};

    const NOW: u64 = 1_793_750_400;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// The contract's example feed.
    fn example() -> Value {
        json!({
            "schema": 1,
            "product": "core",
            "platform": "linux-x86_64",
            "version": "1.4.0",
            "floor": "1.2.0",
            "published_at": NOW,
            "protocols": {
                "work": { "major": 2, "minor": 1 },
                "service": { "major": 1, "minor": 3 },
                "desktop_channel": { "major": 1, "minor": 0 }
            },
            "minimum_system": "2.35",
            "notes": "https://github.com/sadiksaifi/SpaceTerm/releases/tag/v1.4.0",
            "archive": {
                "name": "spaceterm-core-1.4.0-linux-x86_64.tar.gz",
                "size": 31_457_280,
                "sha256": "ab".repeat(32),
                "signature": base64::engine::general_purpose::STANDARD.encode([5; 64])
            }
        })
    }

    struct Signer {
        key: TestKey,
        trust: UpdateTrust,
    }

    impl Signer {
        fn new() -> Self {
            let key = TestKey::generate();
            let trust = trust(&key, &TestKey::generate());
            Self { key, trust }
        }

        fn read(&self, target: FeedTarget, feed: &Value) -> Result<Feed, FeedError> {
            let bytes = serde_json::to_vec(feed).unwrap();
            read(target, &bytes, &self.key.sign(&bytes), &self.trust, NOW)
        }
    }

    fn edited(edit: impl FnOnce(&mut Value)) -> Value {
        let mut feed = example();
        edit(&mut feed);
        feed
    }

    /// The releases a host publishes, as `(version, floor)`, newest first. The walk reads them by
    /// version and records each read.
    struct Published {
        releases: Vec<(&'static str, &'static str)>,
        reads: Vec<Option<Version>>,
    }

    impl Published {
        fn new(releases: &[(&'static str, &'static str)]) -> Self {
            Self {
                releases: releases.to_vec(),
                reads: Vec::new(),
            }
        }

        fn read(&mut self, release: Option<&Version>) -> Result<Feed, &'static str> {
            self.reads.push(release.cloned());
            let (text, floor) = match release {
                None => self.releases[0],
                Some(release) => *self
                    .releases
                    .iter()
                    .find(|(text, _)| version(text) == *release)
                    .ok_or("unpublished")?,
            };
            let mut feed = Signer::new()
                .read(FeedTarget::CORE_LINUX, &example())
                .unwrap();
            feed.version = version(text);
            feed.floor = Floor::parse(floor).unwrap();
            Ok(feed)
        }

        fn walk(&mut self, running: &str) -> Result<Option<(String, bool)>, &'static str> {
            walk(&version(running), |release| self.read(release))
                .map(|next| next.map(|next| (next.feed.version.to_string(), next.stepping_stone)))
        }
    }

    #[test]
    fn an_updater_below_a_floor_steps_down_to_the_first_release_whose_floor_it_meets() {
        let chain = [
            ("1.2.2", "1.2.0"),
            ("1.2.0", "1.1.0"),
            ("1.1.0", "1.0.0"),
            ("1.0.0", "1.0.0"),
        ];
        let cases = [
            ("1.2.1", Some(("1.2.2", false)), vec![None]),
            ("1.2.0", Some(("1.2.2", false)), vec![None]),
            ("1.1.5", Some(("1.2.0", true)), vec![None, Some("1.2.0")]),
            (
                "1.0.0",
                Some(("1.1.0", true)),
                vec![None, Some("1.2.0"), Some("1.1.0")],
            ),
            (
                "0.9.0",
                None,
                vec![None, Some("1.2.0"), Some("1.1.0"), Some("1.0.0")],
            ),
            ("dev.0123456789ab", Some(("1.2.2", false)), vec![None]),
        ];
        for (running, next, reads) in cases {
            let mut published = Published::new(&chain);
            assert_eq!(
                published.walk(running),
                Ok(next.map(|(text, stone)| (text.to_owned(), stone))),
                "{running}"
            );
            let reads: Vec<_> = reads.into_iter().map(|read| read.map(version)).collect();
            assert_eq!(published.reads, reads, "{running}");
        }
    }

    #[test]
    fn a_broken_or_endless_chain_of_floors_leads_nowhere() {
        let mut misnamed = Published::new(&[("1.2.2", "1.2.0"), ("1.2.1", "1.0.0")]);
        let served = version("1.2.1");
        // The host serves 1.2.1's feed at the address of 1.2.0's.
        assert_eq!(
            walk(&version("1.1.0"), |release| misnamed
                .read(release.map(|_| &served))),
            Ok(None)
        );
        let mut unpublished = Published::new(&[("1.2.2", "1.2.0")]);
        assert_eq!(unpublished.walk("1.1.0"), Err("unpublished"));
        let long: Vec<_> = (0..=MAX_STEPS + 1)
            .rev()
            .map(|minor| {
                let text: &'static str = format!("1.{}.0", minor + 1).leak();
                let floor: &'static str = format!("1.{minor}.0").leak();
                (text, floor)
            })
            .collect();
        let mut endless = Published::new(&long);
        assert_eq!(endless.walk("0.9.0"), Ok(None));
        assert_eq!(endless.reads.len(), MAX_STEPS + 1);
        let mut reachable = Published::new(&long);
        assert_eq!(
            reachable.walk("1.1.0"),
            Ok(Some(("1.2.0".to_owned(), true))),
            "a chain of MAX_STEPS steps is walked to its end"
        );
        assert_eq!(reachable.reads.len(), MAX_STEPS + 1);
    }

    #[test]
    fn a_chain_longer_than_a_few_steps_is_walked_to_its_end() {
        let chain: Vec<_> = (1..=9)
            .rev()
            .map(|minor| {
                let text: &'static str = format!("1.{}.0", minor + 1).leak();
                let floor: &'static str = format!("1.{minor}.0").leak();
                (text, floor)
            })
            .chain([("1.1.0", "1.0.0")])
            .collect();
        let mut published = Published::new(&chain);
        assert_eq!(
            published.walk("1.0.0"),
            Ok(Some(("1.1.0".to_owned(), true)))
        );
        assert_eq!(published.reads.len(), 10, "the newest feed and nine steps");
    }

    #[test]
    fn a_signed_feed_reads_as_its_fields() {
        let feed = Signer::new()
            .read(FeedTarget::CORE_LINUX, &example())
            .unwrap();
        assert_eq!(
            feed,
            Feed {
                target: FeedTarget::CORE_LINUX,
                version: version("1.4.0"),
                floor: Floor::parse("1.2.0").unwrap(),
                published_at: NOW,
                protocols: Protocols {
                    work: ProtocolVersion { major: 2, minor: 1 },
                    service: ProtocolVersion { major: 1, minor: 3 },
                    desktop_channel: ProtocolVersion { major: 1, minor: 0 },
                },
                minimum_system: SystemVersion {
                    major: 2,
                    minor: 35
                },
                archive: FeedArchive {
                    name: "spaceterm-core-1.4.0-linux-x86_64.tar.gz".into(),
                    size: 31_457_280,
                    sha256: [0xab; 32],
                    signature: [5; 64],
                },
            }
        );
        let written: Value = serde_json::from_slice(&feed.to_bytes()).unwrap();
        assert_eq!(written, example());
        assert!(!feed.to_bytes().ends_with(b"\n"));
    }

    #[test]
    fn the_signature_is_checked_before_any_field() {
        let signer = Signer::new();
        let feed = b"not json";
        assert_eq!(
            read(
                FeedTarget::CORE_LINUX,
                feed,
                &TestKey::generate().sign(feed),
                &signer.trust,
                NOW
            ),
            Err(FeedError::Signature)
        );
        assert_eq!(
            read(
                FeedTarget::CORE_LINUX,
                feed,
                &signer.key.sign(feed),
                &signer.trust,
                NOW
            ),
            Err(FeedError::Malformed)
        );
    }

    #[test]
    fn a_signed_feed_of_at_most_4096_bytes_is_read() {
        let signer = Signer::new();
        let padded = |length: usize| {
            let mut bytes = serde_json::to_vec(&example()).unwrap();
            bytes.resize(length, b' ');
            let signature = signer.key.sign(&bytes);
            read(
                FeedTarget::CORE_LINUX,
                &bytes,
                &signature,
                &signer.trust,
                NOW,
            )
        };
        assert!(padded(MAX_FEED).is_ok());
        assert_eq!(padded(MAX_FEED + 1), Err(FeedError::TooLarge));
    }

    #[test]
    fn every_field_rule_refuses_its_breach() {
        let signer = Signer::new();
        type Breach = (fn(&mut Value), FeedError);
        let refused: [Breach; 22] = [
            (|f| f["channel"] = "stable".into(), FeedError::Malformed),
            (|f| f["archive"]["url"] = "x".into(), FeedError::Malformed),
            (
                |f| f["protocols"]["future"] = 0.into(),
                FeedError::Malformed,
            ),
            (
                |f| f["protocols"]["work"]["patch"] = 0.into(),
                FeedError::Malformed,
            ),
            (
                |f| {
                    f.as_object_mut().unwrap().remove("notes");
                },
                FeedError::Malformed,
            ),
            (|f| f["minimum_system"] = Value::Null, FeedError::Malformed),
            (
                |f| {
                    f.as_object_mut().unwrap().remove("floor");
                },
                FeedError::Malformed,
            ),
            (|f| f["floor"] = "1.2".into(), FeedError::Floor),
            (|f| f["floor"] = "dev.0123456789ab".into(), FeedError::Floor),
            (|f| f["floor"] = "1.4.1".into(), FeedError::Floor),
            (|f| f["schema"] = 2.into(), FeedError::Schema),
            (|f| f["product"] = "desktop".into(), FeedError::Target),
            (
                |f| f["version"] = "dev.0123456789ab".into(),
                FeedError::Version,
            ),
            (|f| f["version"] = "1.4.00".into(), FeedError::Version),
            (|f| f["published_at"] = 0.into(), FeedError::PublishedAt),
            (
                |f| f["published_at"] = (NOW + MAX_CLOCK_SKEW + 1).into(),
                FeedError::PublishedAt,
            ),
            (
                |f| f["minimum_system"] = "2".into(),
                FeedError::MinimumSystem,
            ),
            (
                |f| {
                    f["notes"] = "https://github.com/sadiksaifi/SpaceTerm/releases/tag/1.4.0".into()
                },
                FeedError::Notes,
            ),
            (
                |f| f["archive"]["name"] = "spaceterm-core-1.4.0-macos-arm64.tar.gz".into(),
                FeedError::ArchiveName,
            ),
            (|f| f["archive"]["size"] = 0.into(), FeedError::ArchiveSize),
            (
                |f| f["archive"]["sha256"] = "AB".repeat(32).into(),
                FeedError::ArchiveDigest,
            ),
            (
                |f| {
                    f["archive"]["signature"] = base64::engine::general_purpose::STANDARD_NO_PAD
                        .encode([5; 64])
                        .into();
                },
                FeedError::ArchiveSignature,
            ),
        ];
        for (edit, error) in refused {
            let feed = edited(edit);
            assert_eq!(
                signer.read(FeedTarget::CORE_LINUX, &feed),
                Err(error),
                "{feed}"
            );
        }
        let largest = edited(|f| f["archive"]["size"] = MAX_ARCHIVE.into());
        assert!(signer.read(FeedTarget::CORE_LINUX, &largest).is_ok());
        let oversized = edited(|f| f["archive"]["size"] = (MAX_ARCHIVE + 1).into());
        assert_eq!(
            signer.read(FeedTarget::CORE_LINUX, &oversized),
            Err(FeedError::ArchiveSize)
        );
        let skewed = edited(|f| f["published_at"] = (NOW + MAX_CLOCK_SKEW).into());
        assert!(signer.read(FeedTarget::CORE_LINUX, &skewed).is_ok());
        let own = edited(|f| f["floor"] = "1.4.0".into());
        assert!(
            signer.read(FeedTarget::CORE_LINUX, &own).is_ok(),
            "the first release with floors names itself as its floor"
        );
    }

    #[test]
    fn a_feed_must_name_the_feed_file_it_was_fetched_as() {
        let signer = Signer::new();
        assert_eq!(
            signer.read(FeedTarget::CORE_MACOS, &example()),
            Err(FeedError::Target)
        );
        let desktop = edited(|f| {
            f["product"] = "desktop".into();
            f["archive"]["name"] = "spaceterm-desktop-1.4.0-linux-x86_64.tar.gz".into();
        });
        assert!(signer.read(FeedTarget::DESKTOP_LINUX, &desktop).is_ok());
        let macos_desktop = edited(|f| {
            f["product"] = "desktop".into();
            f["platform"] = "macos-arm64".into();
        });
        assert_eq!(
            signer.read(FeedTarget::CORE_MACOS, &macos_desktop),
            Err(FeedError::Target)
        );
    }

    #[test]
    fn release_assets_follow_the_naming_table() {
        let v = version("1.4.0");
        let expected = [
            (
                FeedTarget::CORE_MACOS,
                "spaceterm-core-macos-arm64.json",
                "spaceterm-core-1.4.0-macos-arm64.tar.gz",
            ),
            (
                FeedTarget::CORE_LINUX,
                "spaceterm-core-linux-x86_64.json",
                "spaceterm-core-1.4.0-linux-x86_64.tar.gz",
            ),
            (
                FeedTarget::DESKTOP_LINUX,
                "spaceterm-desktop-linux-x86_64.json",
                "spaceterm-desktop-1.4.0-linux-x86_64.tar.gz",
            ),
        ];
        for (target, feed, archive) in expected {
            assert_eq!(target.feed(), feed);
            assert_eq!(target.feed_signature(), format!("{feed}.sig"));
            assert_eq!(target.archive(&v), archive);
            assert_eq!(target.archive_signature(&v), format!("{archive}.sig"));
            assert_eq!(
                target.feed_url(None),
                format!("https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/{feed}")
            );
            assert_eq!(
                target.feed_signature_url(Some(&v)),
                format!(
                    "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.4.0/{feed}.sig"
                )
            );
            assert_eq!(
                target.archive_url(&v),
                format!(
                    "https://github.com/sadiksaifi/SpaceTerm/releases/download/v1.4.0/{archive}"
                )
            );
        }
        assert_eq!(
            FeedTarget::new(Product::Desktop, Platform::MacosArm64),
            None
        );
        assert_eq!(desktop_dmg(&v), "SpaceTerm-1.4.0-macos-arm64.dmg");
    }
}
