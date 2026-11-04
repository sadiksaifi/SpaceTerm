use super::DirectError;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{OnceLock, mpsc};
use std::time::Instant;

/// A client-reachable QUIC authority, an address and UDP port. DNS names remain names until each connection attempt.
#[derive(Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Endpoint")
    }
}

impl Endpoint {
    /// Parse DNS or IP with an optional port, without resolving DNS. `default_port` applies
    /// when no port is given: [`super::default_port`] of the build identity.
    pub fn parse(authority: &str, default_port: u16) -> Result<Self, DirectError> {
        let endpoint = Self::authority_value(authority, Some(default_port))?;
        if endpoint
            .host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        {
            return Err(DirectError::InvalidAddress);
        }
        Ok(endpoint)
    }

    /// Validate an existing QUIC endpoint, including local loopback connections. [`Self::url`]
    /// always writes the port, so a URL without one is refused.
    pub fn from_url(url: &str) -> Result<Self, DirectError> {
        let authority = url
            .strip_prefix("quic://")
            .ok_or(DirectError::InvalidAddress)?;
        Self::authority_value(authority, None)
    }

    /// `default_port` is `None` when the port is required.
    fn authority_value(authority: &str, default_port: Option<u16>) -> Result<Self, DirectError> {
        let default_port = || default_port.ok_or(DirectError::InvalidAddress);
        if authority.is_empty()
            || authority.len() > 320
            || authority.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(DirectError::InvalidAddress);
        }
        let (host, port) = if let Ok(address) = authority.parse::<IpAddr>() {
            (address.to_string(), default_port()?)
        } else if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, rest) = bracketed
                .split_once(']')
                .ok_or(DirectError::InvalidAddress)?;
            let address = host
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| DirectError::InvalidAddress)?;
            let port = if rest.is_empty() {
                default_port()?
            } else {
                rest.strip_prefix(':')
                    .ok_or(DirectError::InvalidAddress)?
                    .parse::<u16>()
                    .map_err(|_| DirectError::InvalidAddress)?
            };
            (address.to_string(), port)
        } else {
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (
                    host,
                    port.parse::<u16>()
                        .map_err(|_| DirectError::InvalidAddress)?,
                ),
                None => (authority, default_port()?),
            };
            let host = host.strip_suffix('.').unwrap_or(host);
            if host.is_empty()
                || host.len() > 253
                || !host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
            {
                return Err(DirectError::InvalidAddress);
            }
            (host.to_ascii_lowercase(), port)
        };
        if port == 0
            || host
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(DirectError::InvalidAddress);
        }
        Ok(Self { host, port })
    }

    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn authority(&self) -> String {
        if self.host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    pub fn url(&self) -> String {
        format!("quic://{}", self.authority())
    }

    pub(super) fn resolve(&self, deadline: Instant) -> Result<Vec<SocketAddr>, DirectError> {
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, self.port)]);
        }
        static RESOLVER: OnceLock<Result<Resolver, DirectError>> = OnceLock::new();
        let resolver = RESOLVER
            .get_or_init(Resolver::new)
            .as_ref()
            .map_err(|error| *error)?;
        let (reply, receive) = mpsc::sync_channel(1);
        resolver
            .requests
            .try_send(Lookup {
                host: self.host.clone(),
                port: self.port,
                deadline,
                reply,
            })
            .map_err(|_| DirectError::Transport)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(DirectError::Transport)?;
        receive
            .recv_timeout(remaining)
            .map_err(|_| DirectError::Transport)?
    }
}

impl TryFrom<String> for Endpoint {
    type Error = DirectError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_url(&value)
    }
}
impl From<Endpoint> for String {
    fn from(value: Endpoint) -> Self {
        value.url()
    }
}

struct Lookup {
    host: String,
    port: u16,
    deadline: Instant,
    reply: mpsc::SyncSender<Result<Vec<SocketAddr>, DirectError>>,
}
struct Resolver {
    requests: mpsc::SyncSender<Lookup>,
}
impl Resolver {
    fn new() -> Result<Self, DirectError> {
        // One process-owned resolver bounds resources even if a native lookup stalls.
        let (requests, pending) = mpsc::sync_channel::<Lookup>(8);
        std::thread::Builder::new()
            .name("spaceterm-dns".into())
            .spawn(move || {
                while let Ok(request) = pending.recv() {
                    if Instant::now() >= request.deadline {
                        continue;
                    }
                    let result = (request.host.as_str(), request.port)
                        .to_socket_addrs()
                        .map(|addresses| addresses.take(32).collect::<Vec<_>>())
                        .map_err(|_| DirectError::Transport)
                        .and_then(|addresses| {
                            if addresses.is_empty() {
                                Err(DirectError::Transport)
                            } else {
                                Ok(addresses)
                            }
                        });
                    let _ = request.reply.send(result);
                }
            })
            .map_err(|_| DirectError::Transport)?;
        Ok(Self { requests })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorities_retain_dns_names_and_apply_the_quic_port_independently_of_ssh() {
        for (input, expected) in [
            ("Penguin.EXAMPLE", "penguin.example:9231"),
            ("penguin.example:9443", "penguin.example:9443"),
            ("192.0.2.8", "192.0.2.8:9231"),
            ("[2001:db8::8]:9443", "[2001:db8::8]:9443"),
        ] {
            let endpoint = Endpoint::parse(input, 9231).unwrap();
            assert_eq!(endpoint.authority(), expected);
            assert_eq!(Endpoint::from_url(&endpoint.url()).unwrap(), endpoint);
        }
        for input in [
            "",
            "user@host",
            "host/path",
            "host:0",
            "0.0.0.0",
            "127.0.0.1",
            "[::]",
            "224.0.0.1",
            "bad..example",
            "-bad.example",
            "host:22:9231",
            "quic://host",
        ] {
            assert!(Endpoint::parse(input, 9231).is_err());
        }
    }

    #[test]
    fn the_default_port_is_the_build_identitys_and_a_url_names_its_port() {
        assert_eq!(super::super::default_port("spaceterm"), 9231);
        assert_eq!(super::super::default_port("spaceterm-preflight"), 9232);
        assert_eq!(super::super::default_port("spaceterm-development"), 9233);
        let preflight = super::super::default_port("spaceterm-preflight");
        assert_eq!(
            Endpoint::parse("penguin", preflight).unwrap().authority(),
            "penguin:9232"
        );
        assert_eq!(
            Endpoint::from_url("quic://penguin"),
            Err(DirectError::InvalidAddress)
        );
        assert_eq!(
            Endpoint::from_url("quic://[2001:db8::8]"),
            Err(DirectError::InvalidAddress)
        );
    }
}
