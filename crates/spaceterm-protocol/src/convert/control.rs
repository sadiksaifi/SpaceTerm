//! Exact control comparisons for both profiles. The caller supplies the Client
//! from authentication; a ControlRequest cannot identify its own principal.

use super::identity::*;
use crate::ProtocolError;
use spaceterm_core::{authority::*, control::*, identity::ClientId, operations::Command};
use std::num::NonZeroU64;

macro_rules! profile {
    ($p:ident) => {
        pub mod $p {
            use super::*;
            use crate::wire::$p as wire;
            pub fn decode_request(
                value: &wire::ControlRequest,
                client: ClientId,
            ) -> Result<Command, ProtocolError> {
                NonZeroU64::new(value.request).ok_or(ProtocolError::Malformed)?;
                let pane = decode_pane(required(&value.pane)?)?;
                if pane.server() != client.server() || value.epoch == 0 {
                    return Err(ProtocolError::Malformed);
                }
                let epoch = ServerEpoch::new(value.epoch);
                match value.action {
                    1 => Ok(Command::CompareAcquire(CompareAcquireRequest {
                        epoch,
                        pane,
                        client,
                        expected_generation: ControlGeneration::new(value.expected_generation),
                    })),
                    2 if value.expected_generation == 0 => Ok(Command::TakeOver(TakeOverRequest {
                        epoch,
                        pane,
                        client,
                    })),
                    3 if value.expected_generation != 0 => {
                        Ok(Command::ReleaseControl(ReleaseControlRequest {
                            epoch,
                            grant: ControlGrant::new(
                                pane,
                                client,
                                ControlGeneration::new(value.expected_generation),
                            )
                            .map_err(|_| ProtocolError::Malformed)?,
                        }))
                    }
                    _ => Err(ProtocolError::Malformed),
                }
            }
            /// Keeps the read/response correlation separate from the identified
            /// operation binding. Callers retain this value in their reply map.
            pub fn encode_request(
                value: &Command,
                request: NonZeroU64,
            ) -> Result<wire::ControlRequest, ProtocolError> {
                let (pane, epoch, action, expected_generation) = match value {
                    Command::CompareAcquire(v) => (v.pane, v.epoch, 1, v.expected_generation.get()),
                    Command::TakeOver(v) => (v.pane, v.epoch, 2, 0),
                    Command::ReleaseControl(v) => {
                        (v.grant.pane(), v.epoch, 3, v.grant.generation().get())
                    }
                    _ => return Err(ProtocolError::UnexpectedMessage),
                };
                Ok(wire::ControlRequest {
                    pane: Some(encode_pane(pane)),
                    epoch: epoch.get(),
                    action,
                    expected_generation,
                    request: request.get(),
                })
            }
            pub fn decode_release(value: &wire::ReleaseControl) -> Result<Command, ProtocolError> {
                let pane = decode_pane(required(&value.pane)?)?;
                let client = decode_client(required(&value.client)?)?;
                if value.epoch == 0 || value.generation == 0 {
                    return Err(ProtocolError::Malformed);
                }
                Ok(Command::ReleaseControl(ReleaseControlRequest {
                    epoch: ServerEpoch::new(value.epoch),
                    grant: ControlGrant::new(
                        pane,
                        client,
                        ControlGeneration::new(value.generation),
                    )
                    .map_err(|_| ProtocolError::Malformed)?,
                }))
            }
            pub fn encode_release(value: ReleaseControlRequest) -> wire::ReleaseControl {
                wire::ReleaseControl {
                    pane: Some(encode_pane(value.grant.pane())),
                    client: Some(encode_client(value.grant.client())),
                    epoch: value.epoch.get(),
                    generation: value.grant.generation().get(),
                }
            }
            pub fn encode_grant(value: GrantedControl) -> wire::GrantedControl {
                wire::GrantedControl {
                    epoch: value.epoch().get(),
                    pane: Some(encode_pane(value.grant().pane())),
                    client: Some(encode_client(value.grant().client())),
                    generation: value.grant().generation().get(),
                }
            }
            pub fn decode_grant(
                value: &wire::GrantedControl,
            ) -> Result<GrantedControl, ProtocolError> {
                let grant = ControlGrant::new(
                    decode_pane(required(&value.pane)?)?,
                    decode_client(required(&value.client)?)?,
                    ControlGeneration::new(value.generation),
                )
                .map_err(|_| ProtocolError::Malformed)?;
                GrantedControl::new(ServerEpoch::new(value.epoch), grant)
                    .map_err(|_| ProtocolError::Malformed)
            }
        }
    };
}
profile!(v1);
profile!(v2);

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_core::identity::{PaneId, ServerId};
    macro_rules! cases {
        ($test:ident,$p:ident) => {
            #[test]
            fn $test() {
                let server = ServerId::new([1; 16]);
                let client = ClientId::new(server, 2).unwrap();
                let pane = PaneId::new(server, 3).unwrap();
                let acquire = Command::CompareAcquire(CompareAcquireRequest {
                    epoch: ServerEpoch::new(4),
                    pane,
                    client,
                    expected_generation: ControlGeneration::new(0),
                });
                assert_eq!(
                    $p::decode_request(
                        &$p::encode_request(&acquire, NonZeroU64::new(17).unwrap()).unwrap(),
                        client
                    )
                    .unwrap(),
                    acquire
                );
                let release = ReleaseControlRequest {
                    epoch: ServerEpoch::new(4),
                    grant: ControlGrant::new(pane, client, ControlGeneration::new(9)).unwrap(),
                };
                assert_eq!(
                    $p::decode_release(&$p::encode_release(release)).unwrap(),
                    Command::ReleaseControl(release)
                );
                let mut forged = $p::encode_release(release);
                forged.client.as_mut().unwrap().server = Some(encode_global([2; 16]));
                assert_eq!($p::decode_release(&forged), Err(ProtocolError::Malformed));
                let grant = GrantedControl::new(release.epoch, release.grant).unwrap();
                assert_eq!($p::decode_grant(&$p::encode_grant(grant)), Ok(grant));
            }
        };
    }
    cases!(
        major_1_control_comparisons_preserve_original_epoch_and_generation,
        v1
    );
    cases!(
        major_2_control_comparisons_preserve_original_epoch_and_generation,
        v2
    );
}
