//! `server unpair DEVICE`: revoke another Paired Device, on the server's own machine.
//!
//! `DEVICE` is an exact device name or `id:PAIRING`, a unique prefix of at least 6 hex
//! characters. The confirmation token binds the exact pairing and server, so it changes when
//! either does.
use super::{Context, Reply};
use crate::contract::{
    Code, Confirmation, Details, DeviceRef, Envelope, Failure, Level, ServerUnpair, TargetDetails,
};
use serde::Serialize;
use spaceterm_server::direct::DeviceInfo;

const COMMAND: &str = "server.unpair";
/// What an unpair token confirms, so it confirms nothing else.
const PURPOSE: &str = "unpair";

/// `effects` of an unpair confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnpairEffects {
    pub device: DeviceRef,
}

/// The confirmation envelope of an unpair preview.
pub(crate) fn preview_envelope(
    server: crate::contract::ServerRef,
    token: String,
    expires_at: String,
    device: DeviceRef,
) -> Envelope<crate::contract::ConfirmationData<UnpairEffects>> {
    Envelope::confirmation(
        COMMAND,
        Some(server),
        Confirmation {
            token,
            expires_at,
            effects: UnpairEffects { device },
        },
    )
}

/// Which paired devices `selector` names.
pub fn select<'a>(devices: &'a [DeviceInfo], selector: &str) -> Result<&'a DeviceInfo, Failure> {
    let matches: Vec<&DeviceInfo> = match selector.strip_prefix("id:") {
        Some(prefix) => {
            if prefix.len() < 6
                || prefix.len() > 64
                || !prefix
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            {
                return Err(Failure::new(Code::InvalidArguments));
            }
            devices
                .iter()
                .filter(|v| super::hex(&v.pairing).starts_with(prefix))
                .collect()
        }
        None => devices
            .iter()
            .filter(|v| v.name.as_str() == selector)
            .collect(),
    };
    match matches.as_slice() {
        [device] => Ok(device),
        [] => Err(Failure::with(
            Code::NotFound,
            Details::Target(TargetDetails::level(Level::Device)),
        )),
        many => Err(Failure::with(
            Code::AmbiguousTarget,
            Details::Target(TargetDetails {
                level: Level::Device,
                parent: None,
                count: Some(u32::try_from(many.len()).unwrap_or(u32::MAX)),
                ids: many.iter().map(|v| super::hex(&v.pairing)).collect(),
            }),
        )),
    }
}

/// What an unpair token binds: the exact server and pairing, and the device's key.
pub fn observation(server: &str, device: &DeviceInfo) -> Vec<u8> {
    let mut bytes = server.as_bytes().to_vec();
    bytes.extend_from_slice(&device.pairing);
    bytes.extend_from_slice(&device.public_key);
    bytes
}

pub fn run(context: &Context<'_>, selector: &str, given: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    let routed = match context.routed() {
        Ok(routed) => routed,
        Err(error) => return Reply::routing_failure(COMMAND, error),
    };
    let server = routed.server.clone();
    let fail = |failure: Failure| Reply::failure(COMMAND, Some(server.clone()), failure);
    let connection = match &routed.connection {
        Ok(connection) => connection,
        Err(failure) => return fail(failure.clone()),
    };
    let devices = match connection.paired_devices() {
        Ok(devices) => devices,
        Err(error) => return fail(super::local_direct(error)),
    };
    let device = match select(&devices, selector) {
        Ok(device) => device,
        Err(failure) => return fail(failure),
    };
    let observed = observation(&server.id, device);
    let now = std::time::SystemTime::now();
    let named = DeviceRef {
        id: super::hex(&device.pairing),
        name: device.name.as_str().to_owned(),
    };
    let confirmed = match given {
        Some(given) => match super::offline_token::verify(PURPOSE, &observed, given, now) {
            Ok(()) => true,
            Err(failure) => return fail(failure),
        },
        None if context.interactive() => super::confirm(&format!("Unpair {}?", named.name)),
        None => false,
    };
    if !confirmed {
        let super::offline_token::Issued {
            token: current,
            expires_at,
        } = super::offline_token::issue(PURPOSE, &observed, now);
        let text = format!(
            "Unpair {} (id:{})? It can no longer connect until it pairs again.\nConfirm with: spaceterm server unpair id:{} --confirm {current}\n",
            named.name,
            &named.id[..8],
            named.id
        );
        return Reply::new(&preview_envelope(server, current, expires_at, named), text);
    }
    let mut operation = [0; 32];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut operation).is_err() {
        return fail(Failure::new(Code::StorageUnavailable));
    }
    if let Err(failure) = crate::interrupt::dispatch() {
        return fail(failure);
    }
    match connection.revoke_device(operation, device.pairing) {
        Ok(_) => {
            let text = format!("Unpaired {}.\n", named.name);
            Reply::new(
                &Envelope::ok(COMMAND, Some(server), ServerUnpair { revoked: named }),
                text,
            )
        }
        Err(error) => fail(super::local_direct(error)),
    }
}
