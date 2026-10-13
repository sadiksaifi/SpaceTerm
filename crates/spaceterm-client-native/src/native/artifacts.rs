//! Bounded signed account-server downloads using reqwest with WebPKI roots.
//! The distribution crate owns feed, archive and layout verification.

use crate::cli::arguments::StableVersion;
use spaceterm_client::management_cli::ManagementFailure;
use spaceterm_distribution::{
    BuildChannel, DistributionFailure, ServerArchiveVerifier, ServerPlatform, UpdateKey,
    VerifiedAccountServerArtifact, VerifiedAccountServerFeed,
    server_feed::{MAX_ARCHIVE_BYTES, MAX_FEED_BYTES, MAX_FEED_SIGNATURE_BYTES},
    verify_account_server_feed,
};
use spaceterm_host::startup::BuildIdentity;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const RELEASES: &str = "https://github.com/sadiksaifi/SpaceTerm/releases";

/// External download boundary. The source chooses every URL and enforces bounds
/// again, so an adapter cannot turn response metadata into installation authority.
pub trait ArtifactTransport: Send + Sync {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, ManagementFailure>;
}

#[derive(Default)]
pub struct HttpsArtifactTransport {
    // Construction stays read-only and runtime-free. The blocking client is
    // initialized only by a download on the blocking pool.
    client: std::sync::OnceLock<Result<reqwest::blocking::Client, ManagementFailure>>,
}
impl ArtifactTransport for HttpsArtifactTransport {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, ManagementFailure> {
        use std::io::Read;
        let client = self
            .client
            .get_or_init(|| {
                reqwest::blocking::Client::builder()
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::limited(4))
                    .timeout(Duration::from_secs(30 * 60))
                    .connect_timeout(Duration::from_secs(15))
                    .user_agent("SpaceTerm")
                    .build()
                    .map_err(|_| ManagementFailure::NativeUnavailable)
            })
            .as_ref()
            .map_err(|failure| *failure)?;
        let response = client
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| ManagementFailure::NativeUnavailable)?;
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err(ManagementFailure::LimitExceeded);
        }
        let maximum = (limit as u64)
            .checked_add(1)
            .ok_or(ManagementFailure::LimitExceeded)?;
        let mut bytes = Vec::new();
        response
            .take(maximum)
            .read_to_end(&mut bytes)
            .map_err(|_| ManagementFailure::NativeUnavailable)?;
        if bytes.len() > limit {
            return Err(ManagementFailure::LimitExceeded);
        }
        Ok(bytes)
    }
}

pub struct ServerArtifacts {
    transport: Arc<dyn ArtifactTransport>,
    verifier: ServerArchiveVerifier,
}
impl Default for ServerArtifacts {
    fn default() -> Self {
        Self::new(Arc::new(HttpsArtifactTransport::default()))
    }
}
impl ServerArtifacts {
    /// Trusted composition may replace only the external HTTP boundary, not the
    /// release key, verification policy, target or account-service authority.
    pub fn new(transport: Arc<dyn ArtifactTransport>) -> Self {
        Self {
            transport,
            verifier: ServerArchiveVerifier::new(),
        }
    }

    pub fn check(
        &self,
        build: BuildIdentity,
        version: Option<&StableVersion>,
    ) -> Result<VerifiedAccountServerFeed, ManagementFailure> {
        let key = UpdateKey::release().ok_or(ManagementFailure::UnsafeNativeState)?;
        self.check_with_key(&key, build, version)
    }

    fn check_with_key(
        &self,
        key: &UpdateKey,
        build: BuildIdentity,
        version: Option<&StableVersion>,
    ) -> Result<VerifiedAccountServerFeed, ManagementFailure> {
        let platform = platform()?;
        let channel = channel(build);
        let url = feed_url(platform, channel, version);
        let feed = self.download(&url, MAX_FEED_BYTES)?;
        let signature = self.download(&format!("{url}.sig"), MAX_FEED_SIGNATURE_BYTES)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ManagementFailure::NativeUnavailable)?
            .as_secs();
        let verified = verify_account_server_feed(key, channel, platform, &feed, &signature, now)
            .map_err(classify_distribution)?;
        if version.is_some_and(|version| version.as_str() != verified.document().version.as_str()) {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Ok(verified)
    }

    pub fn download_artifact(
        &self,
        build: BuildIdentity,
        version: Option<&StableVersion>,
    ) -> Result<VerifiedAccountServerArtifact, ManagementFailure> {
        let feed = self.check(build, version)?;
        let document = feed.document();
        let url = format!(
            "{RELEASES}/download/v{}/{}",
            document.version.as_str(),
            document.archive.name
        );
        let limit =
            usize::try_from(document.archive.size).map_err(|_| ManagementFailure::LimitExceeded)?;
        if document.archive.size > MAX_ARCHIVE_BYTES {
            return Err(ManagementFailure::LimitExceeded);
        }
        let bytes = self.download(&url, limit)?;
        let key = UpdateKey::release().ok_or(ManagementFailure::UnsafeNativeState)?;
        self.verifier
            .verify_account_owned(&key, feed, bytes)
            .map_err(classify_distribution)
    }

    fn download(&self, url: &str, limit: usize) -> Result<Vec<u8>, ManagementFailure> {
        let bytes = self.transport.get(url, limit)?;
        if bytes.len() > limit {
            return Err(ManagementFailure::LimitExceeded);
        }
        Ok(bytes)
    }
}
fn platform() -> Result<ServerPlatform, ManagementFailure> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(ServerPlatform::DarwinArm64)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(ServerPlatform::LinuxX86_64)
    } else {
        Err(ManagementFailure::UnsupportedHost)
    }
}
fn channel(build: BuildIdentity) -> BuildChannel {
    match build {
        BuildIdentity::Release => BuildChannel::Release,
        BuildIdentity::Preflight => BuildChannel::Preflight,
        BuildIdentity::Development => BuildChannel::Development,
    }
}
fn feed_url(
    platform: ServerPlatform,
    channel: BuildChannel,
    version: Option<&StableVersion>,
) -> String {
    let suffix = match channel {
        BuildChannel::Release => "",
        BuildChannel::Preflight => "-preflight",
        BuildChannel::Development => "-development",
    };
    let location = version.map_or_else(
        || "latest".to_owned(),
        |version| format!("download/v{}", version.as_str()),
    );
    let location = if version.is_some() {
        location
    } else {
        format!("{location}/download")
    };
    format!(
        "{RELEASES}/{location}/latest-server{suffix}-{}.json",
        platform.as_str()
    )
}
fn classify_distribution(failure: DistributionFailure) -> ManagementFailure {
    match failure {
        DistributionFailure::LimitExceeded => ManagementFailure::LimitExceeded,
        _ => ManagementFailure::UnsafeNativeState,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::sync::Mutex;
    struct Transport {
        replies: Mutex<std::collections::VecDeque<Vec<u8>>>,
        calls: Mutex<Vec<(String, usize)>>,
    }
    impl ArtifactTransport for Transport {
        fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, ManagementFailure> {
            self.calls.lock().unwrap().push((url.to_owned(), limit));
            Ok(self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected download"))
        }
    }
    fn replies_transport(replies: Vec<Vec<u8>>) -> Arc<Transport> {
        Arc::new(Transport {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(vec![]),
        })
    }
    #[test]
    fn unsigned_or_oversized_feed_never_requests_an_archive() {
        let transport = replies_transport(vec![
            b"{\"version\":\"1.2.3\"}".to_vec(),
            STANDARD.encode([0; 64]).into_bytes(),
        ]);
        let source = ServerArtifacts::new(transport.clone());
        assert!(matches!(
            source.download_artifact(BuildIdentity::Development, None),
            Err(ManagementFailure::UnsafeNativeState)
        ));
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].0.contains("latest-server-development-"));
        assert!(calls[1].0.ends_with(".json.sig"));
        drop(calls);
        let oversized = replies_transport(vec![vec![0; MAX_FEED_BYTES + 1]]);
        assert!(matches!(
            ServerArtifacts::new(oversized.clone()).download_artifact(BuildIdentity::Release, None),
            Err(ManagementFailure::LimitExceeded)
        ));
        assert_eq!(oversized.calls.lock().unwrap().len(), 1);
    }
    #[test]
    fn authenticated_feed_must_match_compiled_channel_target_and_requested_version() {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[11; 32]).unwrap();
        let key = UpdateKey::from_base64(&STANDARD.encode(pair.public_key().as_ref())).unwrap();
        let target = platform().unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version":2,"component":"server","build_identity":"release","platform":target,
            "version":"1.2.3","published_at":1,"protocol_majors":[1,2],
            "storage":{"read":[1],"write":1,"migrate_from":[]},
            "installation":{"manifest_schema":2,"registration_schema":1,"journal_schema":1},
            "archive":{"name":target.account_archive_name(BuildChannel::Release,"1.2.3"),"size":1,"sha256":"11".repeat(32),"signature":STANDARD.encode([1;64]),"manifest_sha256":"22".repeat(32)}
        })).unwrap();
        let signature = STANDARD.encode(pair.sign(&bytes).as_ref()).into_bytes();
        let transport = replies_transport(vec![
            bytes.clone(),
            signature.clone(),
            bytes.clone(),
            signature.clone(),
            bytes,
            signature,
        ]);
        let source = ServerArtifacts::new(transport.clone());
        assert_eq!(
            source
                .check_with_key(&key, BuildIdentity::Release, None)
                .unwrap()
                .document()
                .version
                .as_str(),
            "1.2.3"
        );
        assert!(matches!(
            source.check_with_key(&key, BuildIdentity::Development, None),
            Err(ManagementFailure::UnsafeNativeState)
        ));
        let invocation =
            crate::cli::arguments::parse(["install", "--version", "1.2.4"].map(Into::into))
                .unwrap();
        let crate::cli::arguments::CliCommand::Install { version } = invocation.command else {
            panic!("missing version")
        };
        assert!(matches!(
            source.check_with_key(&key, BuildIdentity::Release, version.as_ref()),
            Err(ManagementFailure::UnsafeNativeState)
        ));
        assert!(
            transport.calls.lock().unwrap()[4]
                .0
                .contains("/download/v1.2.4/")
        );
    }
}
