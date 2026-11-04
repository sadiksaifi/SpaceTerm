//! The proxy a Release Core's feed fetches go through: `update-proxy.json` in the Core's data
//! directory. Without it, fetches connect directly. The service environment is never read, so a
//! proxy variable in a login shell changes nothing.
//!
//! The setting is one HTTP proxy, `http://HOST:PORT`, reached with `CONNECT`. It carries no
//! credentials, path or query, so the file and every error stay free of secrets.
use crate::CoreError;
use crate::storage::Directory;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The setting's file in the Core's data directory.
pub const FILE: &str = "update-proxy.json";
const FILE_NAME: &std::ffi::CStr = c"update-proxy.json";
/// The longest proxy text, in bytes.
pub const MAX_PROXY: usize = 255;

/// `http://HOST:PORT`: a DNS name, an IPv4 address or a bracketed IPv6 address, and a nonzero
/// port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Proxy(String);

/// Text that is not a [`Proxy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not an HTTP proxy address")]
pub struct InvalidProxy;

impl Proxy {
    pub fn parse(text: &str) -> Result<Self, InvalidProxy> {
        if text.len() > MAX_PROXY {
            return Err(InvalidProxy);
        }
        let authority = text.strip_prefix("http://").ok_or(InvalidProxy)?;
        let (host, port) = authority.rsplit_once(':').ok_or(InvalidProxy)?;
        let port: u16 = crate::update::facts::decimal(port).ok_or(InvalidProxy)?;
        if port == 0 || !host_valid(host) {
            return Err(InvalidProxy);
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn host_valid(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

impl TryFrom<String> for Proxy {
    type Error = InvalidProxy;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Proxy> for String {
    fn from(proxy: Proxy) -> Self {
        proxy.0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    proxy: Proxy,
}

/// The proxy set in `data`, the Core's data directory. None when no proxy is set.
pub fn read(data: &Path) -> Result<Option<Proxy>, CoreError> {
    let directory = match Directory::open(data, false) {
        Ok(directory) => directory,
        Err(CoreError::NotSetUp) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(directory
        .read_json::<Document>(FILE)?
        .map(|document| document.proxy))
}

/// Sets the proxy in `data`, or removes it with `None`.
pub fn write(data: &Path, proxy: Option<&Proxy>) -> Result<(), CoreError> {
    let directory = Directory::open(data, true)?;
    match proxy {
        Some(proxy) => directory.write_json(
            FILE_NAME,
            &Document {
                proxy: proxy.clone(),
            },
        ),
        None => match directory.remove(FILE_NAME) {
            Ok(()) | Err(CoreError::NotSetUp) => Ok(()),
            Err(error) => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxy_is_an_http_host_and_a_nonzero_port() {
        for valid in [
            "http://proxy.example:3128",
            "http://10.0.0.1:8080",
            "http://[fd00::1]:8080",
            "http://proxy:1",
        ] {
            assert_eq!(Proxy::parse(valid).unwrap().as_str(), valid);
        }
        for invalid in [
            "https://proxy.example:3128",
            "http://proxy.example",
            "http://proxy.example:0",
            "http://proxy.example:03128",
            "http://proxy.example:65536",
            "http://user:secret@proxy.example:3128",
            "http://proxy.example:3128/path",
            "http://-proxy:3128",
            "http://[not-ipv6]:3128",
            "http://:3128",
            "socks5://proxy:1080",
        ] {
            assert_eq!(Proxy::parse(invalid), Err(InvalidProxy), "{invalid}");
        }
        let long = format!("http://{}:1", "a.".repeat(124));
        assert!(long.len() > MAX_PROXY);
        assert_eq!(Proxy::parse(&long), Err(InvalidProxy));
    }

    #[test]
    fn the_setting_is_written_read_and_removed() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "spaceterm-proxy-{:x}",
            u64::from_be_bytes(crate::protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        assert_eq!(read(&root), Ok(None));
        let proxy = Proxy::parse("http://proxy.example:3128").unwrap();
        write(&root, Some(&proxy)).unwrap();
        assert_eq!(read(&root), Ok(Some(proxy)));
        assert_eq!(
            std::fs::read_to_string(root.join(FILE)).unwrap(),
            r#"{"proxy":"http://proxy.example:3128"}"#
        );
        write(&root, None).unwrap();
        assert_eq!(read(&root), Ok(None));
        write(&root, None).unwrap();
        std::fs::write(root.join(FILE), r#"{"proxy":"http://a:1","user":"x"}"#).unwrap();
        assert_eq!(read(&root), Err(CoreError::UnsafeStorage));
        let _ = std::fs::remove_dir_all(root);
    }
}
