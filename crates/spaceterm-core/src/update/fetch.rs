//! Fetching a Release Core's feed and the archive it names from the public GitHub releases. The
//! bytes fetched here grant nothing: [`super::feed::read`] verifies the feed, and the updater
//! verifies the archive's size, digest and signature before it installs anything.
use super::facts::Version;
use super::feed::{FeedTarget, MAX_FEED};
use super::proxy::Proxy;
use super::trust::SIGNATURE_BYTES;
use std::io::{Read, Write};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_REDIRECTS: u32 = 4;

/// A feed and its detached signature, exactly as served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub feed: Vec<u8>,
    pub signature: Vec<u8>,
}

/// Why a fetch failed. It carries no URL, host, address or native error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// DNS, connection, TLS or HTTP failure with no proxy configured, or an HTTP status from the
    /// release host through the proxy.
    #[error("the release host could not be reached")]
    Network,
    /// The configured proxy refused the request or did not answer.
    #[error("the configured proxy could not be reached")]
    Proxy,
    /// The response was larger than its limit.
    #[error("the response is larger than its limit")]
    TooLarge,
    /// The archive could not be written because the disk is full.
    #[error("the disk is full")]
    DiskFull,
    /// The archive could not be written.
    #[error("the archive could not be written")]
    Storage,
}

/// Where a Release Core's feeds and archives come from.
pub trait FeedSource: Send + Sync {
    /// The feed for `target` of `release`, or of the newest release for None, and its
    /// signature.
    fn feed(&self, target: FeedTarget, release: Option<&Version>) -> Result<Signed, FetchError>;
    /// Writes exactly `size` bytes of the archive of `version` for `target` into `into`.
    fn archive(
        &self,
        target: FeedTarget,
        version: &Version,
        size: u64,
        into: &mut dyn Write,
    ) -> Result<(), FetchError>;
}

/// The public GitHub releases, directly or through one HTTP proxy.
pub struct Releases {
    agent: ureq::Agent,
    proxied: bool,
}

impl Releases {
    /// Fetches through `proxy` when one is set. The process environment is never read, so a
    /// proxy variable a service inherits changes nothing.
    pub fn new(proxy: Option<&Proxy>) -> Self {
        let proxy = proxy.and_then(|proxy| ureq::Proxy::new(proxy.as_str()).ok());
        let proxied = proxy.is_some();
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(MAX_REDIRECTS)
            .timeout_global(Some(TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .user_agent("SpaceTerm")
            .proxy(proxy)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::WebPki)
                    .build(),
            )
            .build()
            .into();
        Self { agent, proxied }
    }

    fn failure(&self, error: &ureq::Error) -> FetchError {
        classify(error, self.proxied)
    }

    fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        self.agent
            .get(url)
            .call()
            .map_err(|error| self.failure(&error))?
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|error| self.failure(&error))
    }
}

/// The failure class of a ureq error. Through a proxy every connection failure is the proxy's,
/// because the proxy resolves and connects to the release host.
fn classify(error: &ureq::Error, proxied: bool) -> FetchError {
    match error {
        ureq::Error::BodyExceedsLimit(_) => FetchError::TooLarge,
        ureq::Error::ConnectProxyFailed(_) | ureq::Error::InvalidProxyUrl => FetchError::Proxy,
        ureq::Error::StatusCode(_) | ureq::Error::Tls(_) | ureq::Error::Rustls(_) => {
            FetchError::Network
        }
        _ if proxied => FetchError::Proxy,
        _ => FetchError::Network,
    }
}

impl FeedSource for Releases {
    fn feed(&self, target: FeedTarget, release: Option<&Version>) -> Result<Signed, FetchError> {
        // One byte over each limit, so the reader refuses an oversized feed or signature as a
        // verification failure instead of a short read.
        let feed = self.get(&target.feed_url(release), MAX_FEED as u64 + 1)?;
        let signature = self.get(
            &target.feed_signature_url(release),
            SIGNATURE_BYTES as u64 + 1,
        )?;
        Ok(Signed { feed, signature })
    }

    fn archive(
        &self,
        target: FeedTarget,
        version: &Version,
        size: u64,
        into: &mut dyn Write,
    ) -> Result<(), FetchError> {
        let response = self
            .agent
            .get(&target.archive_url(version))
            .config()
            .timeout_global(None)
            .timeout_recv_response(Some(TIMEOUT))
            .timeout_recv_body(Some(DOWNLOAD_TIMEOUT))
            .build()
            .call()
            .map_err(|error| self.failure(&error))?;
        let mut reader = response.into_body().into_with_config().limit(size).reader();
        copy_exactly(&mut reader, into, size)
    }
}

/// Copies exactly `size` bytes, refusing a shorter source. A read failure is the network's, a
/// write failure the disk's.
pub fn copy_exactly(
    from: &mut dyn Read,
    into: &mut dyn Write,
    size: u64,
) -> Result<(), FetchError> {
    let mut buffer = vec![0; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let wanted = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = match from.read(&mut buffer[..wanted]) {
            Ok(0) | Err(_) => return Err(FetchError::Network),
            Ok(read) => read,
        };
        into.write_all(&buffer[..read])
            .map_err(|error| storage(&error))?;
        remaining -= read as u64;
    }
    Ok(())
}

/// The failure class of a write error.
pub fn storage(error: &std::io::Error) -> FetchError {
    if error.kind() == std::io::ErrorKind::StorageFull
        || error.raw_os_error() == Some(libc::ENOSPC)
        || error.raw_os_error() == Some(libc::EDQUOT)
    {
        FetchError::DiskFull
    } else {
        FetchError::Storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copying_an_archive_refuses_a_short_source_and_stops_at_its_size() {
        let mut out = Vec::new();
        assert_eq!(copy_exactly(&mut &b"abc"[..], &mut out, 3), Ok(()));
        assert_eq!(
            copy_exactly(&mut &b"ab"[..], &mut Vec::new(), 3),
            Err(FetchError::Network)
        );
        assert_eq!(copy_exactly(&mut &b"abcd"[..], &mut out, 3), Ok(()));
        assert_eq!(out, b"abcabc");
    }

    #[test]
    fn a_full_disk_is_told_apart_from_other_write_failures() {
        struct Full(std::io::Error);
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from_raw_os_error(
                    self.0.raw_os_error().unwrap(),
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let full = Full(std::io::Error::from_raw_os_error(libc::ENOSPC));
        assert_eq!(
            copy_exactly(&mut &b"abc"[..], &mut { full }, 3),
            Err(FetchError::DiskFull)
        );
        let denied = Full(std::io::Error::from_raw_os_error(libc::EACCES));
        assert_eq!(
            copy_exactly(&mut &b"abc"[..], &mut { denied }, 3),
            Err(FetchError::Storage)
        );
    }

    #[test]
    fn connection_failures_are_the_proxys_only_through_one() {
        assert_eq!(
            classify(&ureq::Error::ConnectionFailed, false),
            FetchError::Network
        );
        assert_eq!(
            classify(&ureq::Error::ConnectionFailed, true),
            FetchError::Proxy
        );
        assert_eq!(
            classify(&ureq::Error::HostNotFound, true),
            FetchError::Proxy
        );
        assert_eq!(
            classify(&ureq::Error::ConnectProxyFailed("407".into()), true),
            FetchError::Proxy
        );
        assert_eq!(
            classify(&ureq::Error::StatusCode(404), true),
            FetchError::Network,
            "the release host answered through the proxy"
        );
        assert_eq!(
            classify(&ureq::Error::BodyExceedsLimit(4097), false),
            FetchError::TooLarge
        );
    }

    /// A proxy that refuses every `CONNECT` is a proxy failure, and nothing reaches the release
    /// host.
    #[test]
    fn a_refusing_proxy_fails_the_fetch_as_the_proxys() {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut stream = stream;
            stream
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
            request
        });
        let releases = Releases::new(Some(
            &Proxy::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        ));
        assert_eq!(
            releases.feed(FeedTarget::CORE_LINUX, None),
            Err(FetchError::Proxy)
        );
        let request = proxy.join().unwrap();
        assert!(request.starts_with("CONNECT github.com:443 "), "{request}");
    }

    /// Every variable ureq reads a proxy from.
    const PROXY_VARIABLES: [&str; 6] = [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ];

    /// Runs only inside [`environment_proxies_never_reach_a_fetch`]'s child process.
    #[test]
    fn environment_proxy_probe() {
        if std::env::var_os("SPACETERM_PROXY_PROBE").is_none() {
            return;
        }
        let environment = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        environment.set_nonblocking(true).unwrap();
        let named = format!("http://{}", environment.local_addr().unwrap());
        for variable in PROXY_VARIABLES {
            // SAFETY: this child process runs this one test on one thread, before it starts any
            // other thread, so nothing reads the environment concurrently.
            unsafe { std::env::set_var(variable, &named) };
        }
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
        }
        assert!(
            ureq::Proxy::try_from_env().is_some(),
            "the environment names a proxy ureq would use"
        );
        assert!(
            Releases::new(None).agent.config().proxy().is_none(),
            "no configured proxy means a direct connection"
        );
        let explicit = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let explicit_port = explicit.local_addr().unwrap().port();
        let refusing = std::thread::spawn(move || {
            use std::io::BufRead;
            let (stream, _) = explicit.accept().unwrap();
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let mut stream = stream;
            stream
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .unwrap();
            request
        });
        let configured = Releases::new(Some(
            &Proxy::parse(&format!("http://127.0.0.1:{explicit_port}")).unwrap(),
        ));
        let proxy = configured.agent.config().proxy().unwrap();
        assert_eq!((proxy.host(), proxy.port()), ("127.0.0.1", explicit_port));
        assert_eq!(
            configured.feed(FeedTarget::CORE_LINUX, None),
            Err(FetchError::Proxy)
        );
        assert!(
            refusing
                .join()
                .unwrap()
                .starts_with("CONNECT github.com:443 ")
        );
        assert_eq!(
            environment.accept().map(drop).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "no request reached the environment's proxy"
        );
    }

    /// A service inherits whatever proxy variables its manager passes. Feed fetches ignore every
    /// one of them. The child process keeps the variables from every other test.
    #[test]
    fn environment_proxies_never_reach_a_fetch() {
        if std::env::var_os("SPACETERM_PROXY_PROBE").is_some() {
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "update::fetch::tests::environment_proxy_probe",
                "--exact",
                "--test-threads=1",
            ])
            .env("SPACETERM_PROXY_PROBE", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
