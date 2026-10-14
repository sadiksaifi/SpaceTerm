use super::connector;
use spaceterm_client::{
    api::{ConnectionTarget, RouteDescription, RouteParts},
    failure::ClientFailureClass,
    ports::{ConnectionPurpose, RouteConnector},
};
use spaceterm_core::{
    identity::{ClientId, DeviceId, PairingId, ServerId},
    pairing::{CredentialReference, LocalPairingGeneration},
};
use spaceterm_protocol::authentication::ServerPin;

#[tokio::test]
async fn desktop_route_does_not_create_an_ssh_fallback_without_native_configuration() {
    let server = ServerId::new([1; 16]);
    let target = ConnectionTarget::new(
        ServerPin {
            identity: server.bytes(),
            public_key: [2; 32],
        },
        DeviceId::new([3; 16]),
        PairingId::new(server, 1).unwrap(),
        ClientId::new(server, 1).unwrap(),
        RouteDescription::new(RouteParts::Ssh {
            destination: "selected-host".into(),
            remote_socket: "/private/server".into(),
        })
        .unwrap(),
        CredentialReference::new([4; 16]),
        LocalPairingGeneration::new(1).unwrap(),
    )
    .unwrap();
    let connector = connector(vec![], None).unwrap();
    let error = match connector.open(&target, ConnectionPurpose::Ordinary).await {
        Ok(_) => panic!("desktop route manufactured native SSH authority"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::UnsafeEndpoint);
}
