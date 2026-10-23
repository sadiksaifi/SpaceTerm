use super::*;
use crate::direct::tests::Fixture;
use std::{
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
};

struct Network {
    addresses: Mutex<Vec<IpAddr>>,
    preferred: Vec<IpAddr>,
    unavailable: AtomicBool,
}

impl network::Adapter for Network {
    fn interface_addresses(&self) -> Result<Vec<IpAddr>, DirectError> {
        if self.unavailable.load(Ordering::Relaxed) {
            return Err(DirectError::Transport);
        }
        Ok(self.addresses.lock().unwrap().clone())
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
        fixture.management(Management::Invite),
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
