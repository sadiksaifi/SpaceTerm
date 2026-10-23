use super::{DirectError, NetworkAddresses};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket},
};

const MAX_ADDRESSES: usize = 256;

/// Server-local OS observations. Construction and observation grant no host authority.
pub(super) trait Adapter: Send + Sync {
    fn interface_addresses(&self) -> Result<Vec<IpAddr>, DirectError>;
    fn route_source(&self, destination: IpAddr) -> Option<IpAddr>;
}

pub(super) struct Native;

impl Adapter for Native {
    fn interface_addresses(&self) -> Result<Vec<IpAddr>, DirectError> {
        let mut head = std::ptr::null_mut();
        // SAFETY: getifaddrs initializes this pointer and owns the resulting list.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return Err(DirectError::Transport);
        }
        let interfaces = Interfaces(head);
        let mut current = interfaces.0;
        let mut addresses = BTreeSet::new();
        // SAFETY: current is either null or a node in the retained getifaddrs list.
        while let Some(interface) = unsafe { current.as_ref() } {
            // SAFETY: every node and address belongs to the retained getifaddrs list.
            let address = unsafe { interface.ifa_addr.as_ref() };
            let active = interface.ifa_flags & (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_uint
                == (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_uint;
            if active && let Some(address) = address {
                let ip = match i32::from(address.sa_family) {
                    libc::AF_INET => {
                        // SAFETY: AF_INET identifies an initialized sockaddr_in.
                        let address = unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in>() };
                        Some(IpAddr::V4(Ipv4Addr::from(
                            address.sin_addr.s_addr.to_ne_bytes(),
                        )))
                    }
                    libc::AF_INET6 => {
                        // SAFETY: AF_INET6 identifies an initialized sockaddr_in6.
                        let address = unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in6>() };
                        Some(IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr)))
                    }
                    _ => None,
                };
                if let Some(ip) = ip {
                    addresses.insert(ip);
                    if addresses.len() > MAX_ADDRESSES {
                        return Err(DirectError::Capacity);
                    }
                }
            }
            current = interface.ifa_next;
        }
        Ok(addresses.into_iter().collect())
    }

    fn route_source(&self, destination: IpAddr) -> Option<IpAddr> {
        let bind = match destination {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let socket = UdpSocket::bind((bind, 0)).ok()?;
        // UDP connect selects a route and source address in the kernel. No send,
        // DNS, remote handshake, or external command is involved.
        socket.connect((destination, 9)).ok()?;
        Some(socket.local_addr().ok()?.ip())
    }
}

struct Interfaces(*mut libc::ifaddrs);

impl Drop for Interfaces {
    fn drop(&mut self) {
        // SAFETY: this is the original successful getifaddrs allocation, freed once.
        if !self.0.is_null() {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
}

pub(super) struct Snapshot {
    pub addresses: NetworkAddresses,
    assigned: BTreeSet<IpAddr>,
}

impl Snapshot {
    pub fn contains(&self, address: &IpAddr) -> bool {
        self.assigned.contains(address)
    }
}

pub(super) fn snapshot(adapter: &dyn Adapter) -> Result<Snapshot, DirectError> {
    let assigned: BTreeSet<_> = adapter.interface_addresses()?.into_iter().collect();
    if assigned.len() > MAX_ADDRESSES {
        return Err(DirectError::Capacity);
    }
    let available: Vec<_> = assigned.iter().copied().filter(usable).collect();
    let suggested = if available.is_empty() {
        None
    } else {
        // Documentation destinations require only kernel routing lookup. IPv4
        // is preferred when both families have a usable source route.
        [
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        ]
        .into_iter()
        .filter_map(|destination| adapter.route_source(destination))
        .find(|source| available.contains(source))
    };
    Ok(Snapshot {
        addresses: NetworkAddresses {
            available,
            suggested,
        },
        assigned,
    })
}

fn usable(address: &IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_multicast()
                && !address.is_link_local()
                && address.octets()[0] != 0
                && address.octets()[0] < 240
        }
        IpAddr::V6(address) => {
            !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_multicast()
                && !address.is_unicast_link_local()
                && address.to_ipv4_mapped().is_none()
        }
    }
}
