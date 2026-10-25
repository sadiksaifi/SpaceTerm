use super::{DEFAULT_PORT, DirectError};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{OnceLock, mpsc};
use std::time::Instant;

/// A client-reachable WSS authority. DNS names remain names until each connection attempt.
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
    /// Parse DNS or IP with an optional WSS port, without resolving DNS.
    pub fn parse(authority: &str) -> Result<Self, DirectError> {
        let endpoint = Self::authority_value(authority)?;
        if endpoint
            .host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        {
            return Err(DirectError::InvalidAddress);
        }
        Ok(endpoint)
    }

    /// Validate an existing WSS endpoint, including local loopback connections.
    pub fn from_url(url: &str) -> Result<Self, DirectError> {
        let authority = url
            .strip_prefix("wss://")
            .and_then(|rest| rest.strip_suffix("/spaceterm"))
            .ok_or(DirectError::InvalidAddress)?;
        Self::authority_value(authority)
    }

    fn authority_value(authority: &str) -> Result<Self, DirectError> {
        if authority.is_empty()
            || authority.len() > 320
            || authority.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(DirectError::InvalidAddress);
        }
        let (host, port) = if let Ok(address) = authority.parse::<IpAddr>() {
            (address.to_string(), DEFAULT_PORT)
        } else if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, rest) = bracketed
                .split_once(']')
                .ok_or(DirectError::InvalidAddress)?;
            let address = host
                .parse::<std::net::Ipv6Addr>()
                .map_err(|_| DirectError::InvalidAddress)?;
            let port = if rest.is_empty() {
                DEFAULT_PORT
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
                None => (authority, DEFAULT_PORT),
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
        format!("wss://{}/spaceterm", self.authority())
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
    fn authorities_retain_dns_names_and_apply_the_wss_port_independently_of_ssh() {
        for (input, expected) in [
            ("Penguin.EXAMPLE", "penguin.example:9231"),
            ("penguin.example:9443", "penguin.example:9443"),
            ("192.0.2.8", "192.0.2.8:9231"),
            ("[2001:db8::8]:9443", "[2001:db8::8]:9443"),
        ] {
            let endpoint = Endpoint::parse(input).unwrap();
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
            "wss://host/spaceterm",
        ] {
            assert!(Endpoint::parse(input).is_err());
        }
    }
}
