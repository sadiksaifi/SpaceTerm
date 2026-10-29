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

/// The token for revoking `device` on `server`.
pub fn token(server: &str, device: &DeviceInfo) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(b"spaceterm-cli-unpair-1\0");
    digest.update(server.as_bytes());
    digest.update(&device.pairing);
    digest.update(&device.public_key);
    format!("c1-{}", super::hex(digest.finish().as_ref()))
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
    let current = token(&server.id, device);
    let named = DeviceRef {
        id: super::hex(&device.pairing),
        name: device.name.as_str().to_owned(),
    };
    let confirmed = match given {
        Some(given) if given == current => true,
        Some(_) => return fail(Failure::new(Code::Changed)),
        None if context.interactive() => super::confirm(&format!("Unpair {}?", named.name)),
        None => false,
    };
    if !confirmed {
        let text = format!(
            "Unpair {} (id:{})? It can no longer connect until it pairs again.\nConfirm with: spaceterm server unpair id:{} --confirm {current}\n",
            named.name,
            &named.id[..8],
            named.id
        );
        let expires_at = jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::from_mins(10))
            .map(|v| {
                jiff::Timestamp::from_second(v.as_second())
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        return Reply::new(&preview_envelope(server, current, expires_at, named), text);
    }
    let mut operation = [0; 32];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut operation).is_err() {
        return fail(Failure::new(Code::StorageUnavailable));
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
