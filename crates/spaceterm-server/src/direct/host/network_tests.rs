use super::*;
use crate::direct::tests::Fixture;
use std::{
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
};

struct Network {
    addresses: Mutex<Vec<IpAddr>>,
    /// Addresses of container or virtual machine bridges.
    bridges: Mutex<Vec<IpAddr>>,
    preferred: Vec<IpAddr>,
    unavailable: AtomicBool,
}

impl network::Adapter for Network {
    fn interface_addresses(&self) -> Result<Vec<network::Interface>, DirectError> {
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(DirectError::Transport);
        }
        let bridges = self.bridges.lock().unwrap();
        Ok(self
            .addresses
            .lock()
            .unwrap()
            .iter()
            .map(|address| network::Interface {
                address: *address,
                bridge: bridges.contains(address),
            })
            .collect())
    }
    fn route_source(&self, destination: IpAddr) -> Option<IpAddr> {
        self.preferred
            .iter()
            .copied()
            .find(|address| address.is_ipv4() == destination.is_ipv4())
    }
}

fn network(addresses: &[&str], preferred: &[&str]) -> Arc<Network> {
    Arc::new(Network {
        addresses: Mutex::new(
            addresses
                .iter()
                .map(|address| address.parse().unwrap())
                .collect(),
        ),
        bridges: Mutex::default(),
        preferred: preferred
            .iter()
            .map(|address| address.parse().unwrap())
            .collect(),
        unavailable: AtomicBool::new(false),
    })
}

fn fixture(network: Arc<Network>) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.state = State::open_with_network(&fixture.paths, network).unwrap();
    fixture
}

fn status(fixture: &Fixture) -> DirectStatus {
    let ManagementReply::Status(status) = fixture.management(Management::Status).unwrap() else {
        panic!()
    };
    status
}

#[test]
fn status_filters_owning_server_interfaces_and_suggests_only_a_current_route_source() {
    let fixture = fixture(network(
        &[
            "fd00::5",
            "10.0.0.5",
            "10.0.0.5",
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "169.254.1.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::5",
            "ff02::1",
            "::ffff:127.0.0.1",
        ],
        &["10.99.0.1", "fd00::5"],
    ));
    let status = status(&fixture);
    assert_eq!(
        status.network_addresses,
        Ok(NetworkAddresses {
            available: vec!["10.0.0.5".parse().unwrap(), "fd00::5".parse().unwrap()],
            suggested: Some("fd00::5".parse().unwrap()),
        })
    );
    assert_eq!(status.policy, None);
    assert_eq!(status.listener, ListenerState::Off);
}

#[test]
fn no_usable_address_is_distinct_from_failure_to_read_interfaces() {
    let network = network(&["127.0.0.1", "::1", "fe80::1"], &["10.0.0.5"]);
    let fixture = fixture(network.clone());
    assert_eq!(
        status(&fixture).network_addresses,
        Ok(NetworkAddresses {
            available: vec![],
            suggested: None
        })
    );
    network.unavailable.store(true, Ordering::Relaxed);
    assert_eq!(
        status(&fixture).network_addresses,
        Err(DirectError::Transport)
    );
}

#[test]
fn address_loss_reports_a_failed_listener_without_changing_the_retained_policy_or_identity() {
    let network = network(&["127.0.0.1", "10.0.0.5"], &["10.0.0.5"]);
    let fixture = fixture(network.clone());
    fixture.enable();
    let before = status(&fixture);
    assert!(matches!(before.listener, ListenerState::Listening { .. }));
    let retained = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    *network.addresses.lock().unwrap() = vec!["10.0.0.9".parse().unwrap()];
    let after = status(&fixture);
    assert_eq!(
        after.listener,
        ListenerState::Failed(DirectError::InvalidAddress)
    );
    assert_eq!(
        after.network_addresses.unwrap().available,
        vec!["10.0.0.9".parse::<IpAddr>().unwrap()]
    );
    assert_eq!(after.pin, before.pin);
    assert_eq!(after.policy, before.policy);
    assert!(matches!(
        fixture.management(Management::Invite { advertise: None }),
        Err(DirectError::Disabled)
    ));
    assert_eq!(
        std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
        retained
    );
    network
        .addresses
        .lock()
        .unwrap()
        .push("127.0.0.1".parse().unwrap());
    assert!(matches!(
        status(&fixture).listener,
        ListenerState::Listening { .. }
    ));
}

#[test]
fn every_interface_invitations_advertise_the_suggested_address_of_either_family() {
    for (bind_ip, preferred, expected) in [
        ("0.0.0.0", "fd00::9", "fd00::9"),
        ("::", "10.0.0.9", "10.0.0.9"),
        ("0.0.0.0", "10.0.0.9", "10.0.0.9"),
        ("::", "fd00::9", "fd00::9"),
    ] {
        let fixture = fixture(network(
            &["10.0.0.5", "10.0.0.9", "fd00::5", "fd00::9"],
            &[preferred],
        ));
        let bind = fixture.listen_at(bind_ip.parse().unwrap());
        assert!(
            bind.ip().is_unspecified() && bind.is_ipv4(),
            "kept as 0.0.0.0"
        );
        assert_eq!(
            status(&fixture).listener,
            ListenerState::Listening { addr: bind }
        );
        let invitation = fixture.invite();
        let advertised = std::net::SocketAddr::new(expected.parse().unwrap(), bind.port());
        assert_eq!(invitation.endpoint(), format!("quic://{advertised}"));
    }
}

#[test]
fn every_interface_invitations_without_a_suggestion_advertise_the_first_available_address() {
    let fixture = fixture(network(&["fd00::5", "10.0.0.5"], &[]));
    let bind = fixture.listen_at("0.0.0.0".parse().unwrap());
    let advertised = std::net::SocketAddr::new("10.0.0.5".parse().unwrap(), bind.port());
    assert_eq!(fixture.invite().endpoint(), format!("quic://{advertised}"));
}

#[test]
fn every_interface_invitation_without_an_available_address_refuses_before_allocation() {
    let fixture = fixture(network(&["127.0.0.1", "fe80::1"], &[]));
    let bind = fixture.listen_at("0.0.0.0".parse().unwrap());
    let before = status(&fixture);
    assert_eq!(before.listener, ListenerState::Listening { addr: bind });
    let retained = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    assert!(matches!(
        fixture.management(Management::Invite { advertise: None }),
        Err(DirectError::InvalidAddress)
    ));
    assert!(fixture.state.lock().unwrap().invitations.is_empty());
    assert!(fixture.state.lock().unwrap().claims.is_empty());
    assert!(
        retained == std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
        "an unadvertisable listener must not change the retained record"
    );
    let after = status(&fixture);
    assert_eq!(after.pin, before.pin);
    assert_eq!(after.policy, before.policy);
    assert_eq!(after.enabled, before.enabled);
    assert_eq!(after.listener, before.listener);
}

#[test]
fn every_interface_invitations_skip_container_bridges_unless_the_owner_chooses_one() {
    let network = network(&["172.17.0.1", "192.168.1.5"], &[]);
    *network.bridges.lock().unwrap() = vec!["172.17.0.1".parse().unwrap()];
    let fixture = fixture(network.clone());
    let bind = fixture.listen_at("0.0.0.0".parse().unwrap());
    let at = |ip: &str| {
        format!(
            "quic://{}",
            std::net::SocketAddr::new(ip.parse().unwrap(), bind.port())
        )
    };
    assert_eq!(fixture.invite().endpoint(), at("192.168.1.5"));
    let ManagementReply::Invitation(text) = fixture
        .management(Management::Invite {
            advertise: Some("172.17.0.1".into()),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        Invitation::parse(&text).unwrap().endpoint(),
        at("172.17.0.1")
    );
    // With only a bridge left, the server makes no choice of its own.
    *network.addresses.lock().unwrap() = vec!["172.17.0.1".parse().unwrap()];
    assert!(matches!(
        fixture.management(Management::Invite { advertise: None }),
        Err(DirectError::InvalidAddress)
    ));
}

#[test]
fn invitations_advertise_only_a_chosen_address_the_listener_takes() {
    let fixture = fixture(network(
        &["10.0.0.5", "fd00::5", "127.0.0.1"],
        &["10.0.0.5"],
    ));
    let bind = fixture.listen_at("0.0.0.0".parse().unwrap());
    let ManagementReply::Invitation(text) = fixture
        .management(Management::Invite {
            advertise: Some("fd00::5".into()),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        Invitation::parse(&text).unwrap().endpoint(),
        format!("quic://[fd00::5]:{}", bind.port())
    );
    // An address that isn't available, such as loopback or another machine's, is refused.
    for refused in ["127.0.0.1", "10.0.0.9"] {
        assert!(matches!(
            fixture.management(Management::Invite {
                advertise: Some(refused.into()),
            }),
            Err(DirectError::InvalidAddress)
        ));
    }
    // A listener at one address advertises only that one.
    let one = fixture.listen_at("127.0.0.1".parse().unwrap());
    assert!(matches!(
        fixture.management(Management::Invite {
            advertise: Some("10.0.0.5".into()),
        }),
        Err(DirectError::InvalidAddress)
    ));
    let ManagementReply::Invitation(text) = fixture
        .management(Management::Invite {
            advertise: Some(one.ip().to_string()),
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        Invitation::parse(&text).unwrap().endpoint(),
        format!("quic://{one}")
    );
}

#[test]
fn exposure_refuses_an_address_this_machine_doesnt_have() {
    let fixture = fixture(network(&["127.0.0.1", "10.0.0.5"], &["10.0.0.5"]));
    let bind = fixture.listen_at("127.0.0.1".parse().unwrap());
    let retained = std::fs::read(fixture.paths.data.join("direct.json")).unwrap();
    assert_eq!(
        fixture
            .management(Management::SetPolicy {
                enabled: true,
                policy: Some(NetworkPolicy::new("10.0.0.9:9231".parse().unwrap()).unwrap()),
            })
            .err(),
        Some(DirectError::InvalidAddress)
    );
    assert_eq!(
        std::fs::read(fixture.paths.data.join("direct.json")).unwrap(),
        retained
    );
    assert_eq!(
        status(&fixture).listener,
        ListenerState::Listening { addr: bind }
    );
}

#[test]
fn a_retained_ipv6_wildcard_reads_as_every_interface() {
    let network = network(&["127.0.0.1"], &[]);
    let fixture = fixture(network.clone());
    let bind = fixture.listen_at("0.0.0.0".parse().unwrap());
    fixture.state.lock().unwrap().stop();
    let path = fixture.paths.data.join("direct.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let stored = format!("\"0.0.0.0:{}\"", bind.port());
    assert!(text.contains(&stored));
    std::fs::write(
        &path,
        text.replace(&stored, &format!("\"[::]:{}\"", bind.port())),
    )
    .unwrap();
    let reopened = State::open_with_network(&fixture.paths, network).unwrap();
    let ManagementReply::Status(status) = reopened
        .lock()
        .unwrap()
        .manage(None, Management::Status)
        .unwrap()
        .0
    else {
        panic!()
    };
    assert_eq!(status.policy.unwrap().bind, bind);
    assert_eq!(status.listener, ListenerState::Listening { addr: bind });
    reopened.lock().unwrap().stop();
}
