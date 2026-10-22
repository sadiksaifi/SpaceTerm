use super::*;
use std::{
    net::{TcpListener, TcpStream},
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
};

const MAX_PENDING: usize = 32;
fn bind_listener(address: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(32)?;
    Ok(socket.into())
}
pub(crate) type Shared = Arc<Mutex<State>>;
struct PendingInvitation {
    id: ClaimId,
    token: [u8; 32],
    expires: u64,
    used: bool,
}
struct PendingClaim {
    invitation: ClaimId,
    claim: PairingClaim,
    decision: Option<Result<DeviceInfo, DirectError>>,
}

pub(crate) struct State {
    store: storage::Store,
    record: storage::Record,
    pub(super) pin: ServerPin,
    listener: Option<TcpListener>,
    listener_state: ListenerState,
    invitations: Vec<PendingInvitation>,
    claims: Vec<PendingClaim>,
    connections: Vec<(PairingId, UnixStream)>,
    uncertain: bool,
    generation: [u8; 32],
    released: std::collections::BTreeSet<[u8; 32]>,
}
impl State {
    pub(crate) fn open(paths: &crate::host::Paths) -> Result<Shared, DirectError> {
        let store = storage::Store::open(paths)?;
        let record = store.load()?;
        let pin = store.pin(&record)?;
        let mut state = Self {
            store,
            record,
            pin,
            listener: None,
            listener_state: ListenerState::Off,
            invitations: Vec::new(),
            claims: Vec::new(),
            connections: Vec::new(),
            uncertain: false,
            generation: crypto::random()?,
            released: Default::default(),
        };
        state.listen();
        Ok(Arc::new(Mutex::new(state)))
    }
    fn listen(&mut self) {
        self.listener = None;
        self.listener_state = match &self.record.policy {
            None => ListenerState::Off,
            Some(policy) => match bind_listener(policy.bind) {
                Ok(listener) => {
                    self.listener = Some(listener);
                    ListenerState::Listening { addr: policy.bind }
                }
                Err(_) => ListenerState::Failed(DirectError::Transport),
            },
        };
    }
    fn recover(&mut self) -> Result<(), DirectError> {
        if self.uncertain {
            self.record = self.store.load()?;
            if self.store.pin(&self.record)? != self.pin {
                return Err(DirectError::StorageUnavailable);
            }
            self.uncertain = false;
            self.generation = crypto::random()?;
            self.listen();
        }
        Ok(())
    }
    fn commit(&mut self, next: storage::Record) -> Result<(), DirectError> {
        if let Err(error) = self.store.save(&next) {
            self.uncertain = true;
            // A renamed but unsynced revocation must never leave ordinary authority usable.
            for (_, stream) in &self.connections {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            self.listener = None;
            self.listener_state = ListenerState::Failed(error);
            return Err(error);
        }
        self.record = next;
        Ok(())
    }
    pub(crate) fn accept(&mut self) -> Option<(TcpStream, [u8; 32])> {
        self.connections.retain(|(_, stream)| {
            let mut byte = [0];
            // A disconnected private bridge answers EOF; WouldBlock is a live idle bridge.
            // SAFETY: recv only observes this retained socket into one writable byte.
            unsafe {
                libc::recv(
                    stream.as_raw_fd(),
                    byte.as_mut_ptr().cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                ) != 0
            }
        });
        self.listener
            .as_ref()?
            .accept()
            .ok()
            .map(|v| (v.0, self.generation))
    }
    pub(crate) fn certificate(&self) -> (&[u8], &[u8]) {
        (&self.record.certificate, &self.record.key)
    }
    pub(crate) fn authorized(&mut self, paired: &PairedDevice) -> Result<(), DirectError> {
        self.recover()?;
        if !matches!(self.listener_state, ListenerState::Listening { .. }) {
            return Err(DirectError::Disabled);
        }
        if paired.pin != self.pin {
            return Err(DirectError::WrongServer);
        }
        let device = self
            .record
            .devices
            .iter()
            .find(|d| d.info.pairing == paired.pairing)
            .ok_or(DirectError::AuthenticationRefused)?;
        if device.info.client != paired.client {
            return Err(DirectError::AuthenticationRefused);
        }
        if device.revoked {
            return Err(DirectError::Revoked);
        }
        Ok(())
    }
    pub(crate) fn register(
        &mut self,
        paired: &PairedDevice,
        stream: &UnixStream,
        generation: [u8; 32],
    ) -> Result<(), DirectError> {
        self.authorized(paired)?;
        if generation != self.generation {
            return Err(DirectError::Disabled);
        }
        if self.connections.len() >= 64 {
            return Err(DirectError::Capacity);
        }
        let clone = stream.try_clone().map_err(|_| DirectError::Transport)?;
        clone
            .set_nonblocking(true)
            .map_err(|_| DirectError::Transport)?;
        self.connections.push((paired.pairing, clone));
        Ok(())
    }
    fn expire(&mut self) -> Result<(), DirectError> {
        let now = wire::now()?;
        self.invitations.retain(|v| v.expires > now);
        self.claims
            .retain(|v| v.claim.expires_at_unix_seconds > now);
        Ok(())
    }
    fn status(&self) -> DirectStatus {
        let mut name = [0u8; 256];
        // SAFETY: gethostname writes at most the supplied byte length.
        let server_name = if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0
        {
            let count = name.iter().position(|b| *b == 0).unwrap_or(name.len());
            ClientName::from(String::from_utf8_lossy(&name[..count]).into_owned())
                .as_str()
                .to_owned()
        } else {
            String::new()
        };
        DirectStatus {
            pin: self.pin.clone(),
            policy: self.record.policy.clone(),
            server_name,
            listener: self.listener_state.clone(),
        }
    }
    pub(crate) fn manage(
        &mut self,
        actor: Option<PairingId>,
        request: Management,
    ) -> Result<(ManagementReply, Option<[u8; 32]>), DirectError> {
        self.recover()?;
        self.expire()?;
        let reply = match request {
            Management::Status => ManagementReply::Status(self.status()),
            Management::SetPolicy(policy) => {
                if let Some(policy) = &policy {
                    NetworkPolicy::new(policy.bind)?;
                }
                let generation = crypto::random()?;
                let mut next = self.record.clone();
                next.policy = policy;
                self.commit(next)?;
                self.generation = generation;
                for (_, stream) in &self.connections {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                self.connections.clear();
                self.invitations.clear();
                for pending in &mut self.claims {
                    if pending.decision.is_none() {
                        pending.decision = Some(Err(DirectError::AlreadyDecided));
                    }
                }
                self.listen();
                ManagementReply::Status(self.status())
            }
            Management::Invite => {
                let endpoint = match self.listener_state {
                    ListenerState::Listening { addr } => format!("wss://{addr}/spaceterm"),
                    _ => return Err(DirectError::Disabled),
                };
                if self.invitations.len() >= MAX_PENDING {
                    return Err(DirectError::Capacity);
                }
                let payload = wire::Invite {
                    schema: 1,
                    id: crypto::random()?,
                    endpoint,
                    pin: self.pin.clone(),
                    server_name: self.status().server_name,
                    token: crypto::random()?,
                    expires: wire::now()? + wire::INVITATION_SECONDS,
                };
                self.invitations.push(PendingInvitation {
                    id: payload.id,
                    token: crypto::hash(&payload.token),
                    expires: payload.expires,
                    used: false,
                });
                ManagementReply::Invitation(client::invitation(payload)?.display_text().to_string())
            }
            Management::Withdraw(id) => {
                self.invitations.retain(|v| v.id != id);
                for claim in self
                    .claims
                    .iter_mut()
                    .filter(|c| c.invitation == id && c.decision.is_none())
                {
                    claim.decision = Some(Err(DirectError::AlreadyDecided));
                }
                ManagementReply::Rejected
            }
            Management::Claims => ManagementReply::Claims(
                self.claims
                    .iter()
                    .filter(|c| c.decision.is_none())
                    .map(|c| c.claim.clone())
                    .collect(),
            ),
            Management::Approve(claim) => {
                if claim.server != self.pin.server {
                    return Err(DirectError::WrongServer);
                }
                let index = self
                    .claims
                    .iter()
                    .position(|c| c.claim.id == claim.id)
                    .ok_or(DirectError::Expired)?;
                if self.claims[index].claim != claim {
                    return Err(DirectError::Substituted);
                }
                if self.claims[index].decision.is_some() {
                    return Err(DirectError::AlreadyDecided);
                }
                if self.record.devices.len() >= storage::MAX_DEVICES {
                    return Err(DirectError::Capacity);
                }
                let info = DeviceInfo {
                    pairing: crypto::random()?,
                    client: crypto::random()?,
                    name: claim.device_name.clone(),
                    public_key: claim.device_public_key,
                };
                let mut next = self.record.clone();
                next.devices.push(storage::Device {
                    claim: claim.id,
                    info: info.clone(),
                    revoked: false,
                });
                self.commit(next)?;
                self.claims[index].decision = Some(Ok(info.clone()));
                ManagementReply::Approved(info)
            }
            Management::Reject(id) => {
                let claim = self
                    .claims
                    .iter_mut()
                    .find(|c| c.claim.id == id)
                    .ok_or(DirectError::Expired)?;
                if claim.decision.is_some() {
                    return Err(DirectError::AlreadyDecided);
                }
                claim.decision = Some(Err(DirectError::Rejected));
                ManagementReply::Rejected
            }
            Management::Devices => ManagementReply::Devices(
                self.record
                    .devices
                    .iter()
                    .filter(|d| !d.revoked)
                    .map(|d| d.info.clone())
                    .collect(),
            ),
            Management::Revoke { operation, target } => {
                let (receipt, client) = self.revoke(actor, operation, target)?;
                return Ok((ManagementReply::Revoked(receipt), Some(client)));
            }
        };
        Ok((reply, None))
    }
    pub(crate) fn revoke(
        &mut self,
        actor: Option<PairingId>,
        operation: OperationId,
        target: PairingId,
    ) -> Result<(RevocationReceipt, [u8; 32]), DirectError> {
        self.recover()?;
        if operation == [0; 32] {
            return Err(DirectError::InvalidRequest);
        }
        let device = self
            .record
            .devices
            .iter()
            .find(|d| d.info.pairing == target)
            .ok_or(DirectError::AuthenticationRefused)?;
        let client = device.info.client;
        let receipt = if let Some(original) = self
            .record
            .revocations
            .iter()
            .find(|r| r.actor == actor && r.receipt.operation == operation)
        {
            if original.receipt.target != target {
                return Err(DirectError::Substituted);
            }
            original.receipt.clone()
        } else {
            // An already-retired key may confirm only its own revoked identity. This is an
            // observation with no new grants, effects or ledger allocation, even when another
            // owner's revocation won the race before this original was dispatched.
            if device.revoked && actor == Some(target) {
                return Ok((
                    RevocationReceipt {
                        server: self.pin.server,
                        operation,
                        target,
                    },
                    client,
                ));
            }
            if let Some(actor) = actor {
                let actor = self
                    .record
                    .devices
                    .iter()
                    .find(|d| d.info.pairing == actor)
                    .ok_or(DirectError::AuthenticationRefused)?;
                if actor.revoked {
                    return Err(DirectError::Revoked);
                }
            }
            if self.record.revocations.len() >= storage::MAX_REVOCATIONS {
                return Err(DirectError::Capacity);
            }
            let receipt = RevocationReceipt {
                server: self.pin.server,
                operation,
                target,
            };
            let mut next = self.record.clone();
            next.devices
                .iter_mut()
                .find(|d| d.info.pairing == target)
                .ok_or(DirectError::AuthenticationRefused)?
                .revoked = true;
            next.revocations.push(storage::Revocation {
                actor,
                receipt: receipt.clone(),
            });
            self.commit(next)?;
            receipt
        };
        for (_, stream) in self
            .connections
            .iter()
            .filter(|(pairing, _)| *pairing == target)
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        self.connections.retain(|(pairing, _)| *pairing != target);
        Ok((receipt, client))
    }
    pub(super) fn challenge(&self) -> Result<wire::Challenge, DirectError> {
        if !matches!(self.listener_state, ListenerState::Listening { .. }) {
            return Err(DirectError::Disabled);
        }
        let nonce = crypto::random()?;
        let server_name = self.status().server_name;
        let proof = crypto::sign(
            &self.record.key,
            &wire::transcript(
                "server",
                &self.pin,
                &nonce,
                &(crate::protocol::MAJOR, &server_name, &self.generation),
            )?,
        )?;
        Ok(wire::Challenge {
            major: crate::protocol::MAJOR,
            pin: self.pin.clone(),
            nonce,
            generation: self.generation,
            server_name,
            proof,
        })
    }
    pub(super) fn authenticate(
        &mut self,
        challenge: &wire::Challenge,
        authentication: &wire::Authentication,
    ) -> Result<wire::Answer, DirectError> {
        self.recover()?;
        if let wire::Action::Claim { token, .. } = &authentication.action {
            let now = wire::now()?;
            if self
                .invitations
                .iter()
                .any(|v| v.token == crypto::hash(token) && v.expires <= now)
            {
                return Err(DirectError::Expired);
            }
        }
        self.expire()?;
        if challenge.generation != self.generation
            || !matches!(self.listener_state, ListenerState::Listening { .. })
        {
            return Err(DirectError::Disabled);
        }
        let public = match &authentication.action {
            wire::Action::Connect { paired } => {
                self.authorized(paired)?;
                self.record
                    .devices
                    .iter()
                    .find(|d| d.info.pairing == paired.pairing)
                    .ok_or(DirectError::AuthenticationRefused)?
                    .info
                    .public_key
            }
            wire::Action::Claim { key, .. }
            | wire::Action::Finish { key, .. }
            | wire::Action::Cancel { key, .. } => *key,
            wire::Action::Revoke {
                actor,
                operation,
                target,
            } => {
                if actor.pin != self.pin {
                    return Err(DirectError::WrongServer);
                }
                let device = self
                    .record
                    .devices
                    .iter()
                    .find(|d| d.info.pairing == actor.pairing && d.info.client == actor.client)
                    .ok_or(DirectError::AuthenticationRefused)?;
                // Retired authority is restricted to the exact admitted original, never a new target.
                if device.revoked
                    && actor.pairing != *target
                    && !self.record.revocations.iter().any(|r| {
                        r.actor == Some(actor.pairing)
                            && r.receipt.operation == *operation
                            && r.receipt.target == *target
                    })
                {
                    return Err(DirectError::Revoked);
                }
                device.info.public_key
            }
        };
        crypto::verify(
            &public,
            &wire::transcript(
                "device",
                &self.pin,
                &challenge.nonce,
                &authentication.action,
            )?,
            &authentication.proof,
        )?;
        match &authentication.action {
            wire::Action::Connect { .. } => Ok(wire::Answer::Connected),
            wire::Action::Claim {
                id,
                token,
                key,
                name,
            } => {
                let invitation_index = self
                    .invitations
                    .iter()
                    .position(|v| v.token == crypto::hash(token))
                    .ok_or(DirectError::InvalidInvitation)?;
                let invitation = &self.invitations[invitation_index];
                if wire::claim_id(&self.pin, invitation.id, invitation.token, *key)? != *id {
                    return Err(DirectError::Substituted);
                }
                if let Some(pending) = self.claims.iter().find(|c| c.claim.id == *id) {
                    if pending.claim.device_public_key != *key || pending.claim.device_name != *name
                    {
                        return Err(DirectError::Substituted);
                    }
                    if let Some(Err(error)) = &pending.decision {
                        return Err(*error);
                    }
                    return Ok(wire::Answer::Claim(*id));
                }
                if invitation.used {
                    return Err(DirectError::AlreadyDecided);
                }
                if self.claims.len() >= MAX_PENDING {
                    return Err(DirectError::Capacity);
                }
                let claim = PairingClaim {
                    id: *id,
                    server: self.pin.server,
                    device_public_key: *key,
                    device_name: name.clone(),
                    expires_at_unix_seconds: invitation.expires,
                };
                let id = claim.id;
                let invitation = invitation.id;
                self.invitations[invitation_index].used = true;
                self.claims.push(PendingClaim {
                    invitation,
                    claim,
                    decision: None,
                });
                Ok(wire::Answer::Claim(id))
            }
            wire::Action::Finish { claim, key } => {
                if let Some(device) = self.record.devices.iter().find(|d| d.claim == *claim) {
                    if device.info.public_key != *key {
                        return Err(DirectError::Substituted);
                    }
                    if device.revoked {
                        return Err(DirectError::Revoked);
                    }
                    return Ok(wire::Answer::Enrollment(Some(PairedDevice {
                        pin: self.pin.clone(),
                        pairing: device.info.pairing,
                        client: device.info.client,
                    })));
                }
                let pending = self
                    .claims
                    .iter()
                    .find(|c| c.claim.id == *claim)
                    .ok_or(DirectError::Expired)?;
                if pending.claim.device_public_key != *key {
                    return Err(DirectError::Substituted);
                }
                let device = pending.decision.clone().transpose()?;
                if let Some(device) = &device
                    && self
                        .record
                        .devices
                        .iter()
                        .any(|d| d.info.pairing == device.pairing && d.revoked)
                {
                    return Err(DirectError::Revoked);
                }
                Ok(wire::Answer::Enrollment(device.map(|d| PairedDevice {
                    pin: self.pin.clone(),
                    pairing: d.pairing,
                    client: d.client,
                })))
            }
            wire::Action::Revoke {
                actor,
                operation,
                target,
            } => self
                .revoke(Some(actor.pairing), *operation, *target)
                .map(|v| wire::Answer::Revoked(v.0)),
            wire::Action::Cancel { claim, key } => {
                if let Some(device) = self.record.devices.iter().find(|d| d.claim == *claim) {
                    if device.info.public_key != *key {
                        return Err(DirectError::Substituted);
                    }
                    let pairing = device.info.pairing;
                    let operation =
                        crypto::hash(&wire::transcript("cancel", &self.pin, claim, &pairing)?);
                    return self
                        .revoke(Some(pairing), operation, pairing)
                        .map(|v| wire::Answer::Cancelled(Some(v.0)));
                }
                if let Some(pending) = self.claims.iter_mut().find(|c| c.claim.id == *claim) {
                    if pending.claim.device_public_key != *key {
                        return Err(DirectError::Substituted);
                    }
                    pending.decision = Some(Err(DirectError::AlreadyDecided));
                    return Ok(wire::Answer::Cancelled(None));
                }
                // A prepared original may be cancelled before an older socket submits it.
                // Derivation proves knowledge of this invitation's capability and binds the
                // signing key. Consume the existing bounded invitation slot; allocate no
                // tombstone for arbitrary unauthenticated IDs. Restart invalidates every
                // transient invitation, so an old submission cannot escape this fence.
                for invitation in &mut self.invitations {
                    if wire::claim_id(&self.pin, invitation.id, invitation.token, *key)? == *claim {
                        invitation.used = true;
                        return Ok(wire::Answer::Cancelled(None));
                    }
                }
                Err(DirectError::Expired)
            }
        }
    }
    pub(crate) fn revocation_client(&self, target: PairingId) -> Option<[u8; 32]> {
        self.record
            .devices
            .iter()
            .find(|d| d.info.pairing == target)
            .map(|d| d.info.client)
    }
    pub(crate) fn stop(&mut self) {
        self.listener = None;
        self.listener_state = ListenerState::Off;
        for (_, stream) in &self.connections {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        self.connections.clear();
        self.invitations.clear();
        self.claims.clear();
    }
    pub(crate) fn pending_releases(&mut self) -> Result<Vec<[u8; 32]>, DirectError> {
        self.recover()?;
        let clients: Vec<_> = self
            .record
            .devices
            .iter()
            .filter(|d| d.revoked && !self.released.contains(&d.info.client))
            .map(|d| d.info.client)
            .collect();
        self.released.extend(clients.iter().copied());
        Ok(clients)
    }
}

pub(crate) fn serve(
    tcp: TcpStream,
    generation: [u8; 32],
    state: Shared,
    environment: crate::host::Environment,
) -> Result<(), ServerError> {
    let (certificate, key) = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        let (certificate, key) = state.certificate();
        (certificate.to_vec(), zeroize::Zeroizing::new(key.to_vec()))
    };
    let mut ws = wss::accept(tcp, &certificate, &key)?;
    let challenge = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        if generation != state.generation {
            return Err(DirectError::Disabled.into());
        }
        state.challenge()?
    };
    wss::send(&mut ws, &challenge)?;
    let authentication: wire::Authentication = wss::receive(&mut ws)?;
    let paired = match &authentication.action {
        wire::Action::Connect { paired } => Some(paired.clone()),
        _ => None,
    };
    let answer = {
        // The same lock fences both original requests and revocation, including cancellation.
        let mut panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let answer = state.authenticate(&challenge, &authentication);
        let receipt = match &answer {
            Ok(wire::Answer::Revoked(receipt) | wire::Answer::Cancelled(Some(receipt))) => {
                Some(receipt)
            }
            _ => None,
        };
        if let Some(client) = receipt.and_then(|r| state.revocation_client(r.target))
            && let Some(panes) = panes.as_mut()
        {
            panes.revoked(client);
        }
        answer
    };
    let connected = matches!(answer, Ok(wire::Answer::Connected));
    wss::send(&mut ws, &answer)?;
    if !connected {
        return Ok(());
    }
    let paired = paired.ok_or(ServerError::AuthenticationRefused)?;
    let mut bridge = wss::Bridge::start(ws)?;
    {
        let _panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
        state.lock().unwrap_or_else(|e| e.into_inner()).register(
            &paired,
            bridge.stream(),
            generation,
        )?;
    }
    let (_, client, name) = crate::protocol::admit_direct_server(&mut bridge, &paired)?;
    crate::host::serve_admitted(&mut bridge, client, name, Some(&paired), &environment)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_storage_recovery_fences_registration_from_the_prior_exposure_generation() {
        let fixture = super::super::tests::Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let invitation = fixture.invite();
        let claim = fixture.claim(&invitation, &credential);
        let ManagementReply::Approved(device) =
            fixture.management(Management::Approve(claim)).unwrap()
        else {
            panic!()
        };
        let mut state = fixture.state.lock().unwrap();
        let paired = PairedDevice {
            pin: state.pin.clone(),
            pairing: device.pairing,
            client: device.client,
        };
        let generation = state.generation;
        // A failed atomic save leaves its durable outcome uncertain. Its next authority
        // check reloads the document and rotates exposure before registering a stream.
        state.uncertain = true;
        let (stream, _peer) = UnixStream::pair().unwrap();
        let registered = state.register(&paired, &stream, generation);
        let empty = state.connections.is_empty();
        drop(state);
        assert_eq!(registered, Err(DirectError::Disabled));
        assert!(empty);
    }
    #[test]
    fn invitation_and_claim_expiry_refuse_enrollment_and_confirm_no_retained_pairing_on_cancel() {
        let fixture = super::super::tests::Fixture::new();
        fixture.enable();
        let mut invitation = fixture.invite();
        let credential = DeviceCredential::generate().unwrap();
        invitation.payload.expires = 0;
        assert!(matches!(
            Enrollment::claim(&invitation, "Expired".into(), &credential),
            Err(DirectError::Expired)
        ));
        fixture.state.lock().unwrap().invitations[0].expires = 0;
        assert!(matches!(
            fixture.authenticate(
                &credential,
                wire::Action::Claim {
                    id: [1; 32],
                    token: invitation.payload.token,
                    key: crypto::public(credential.expose_bytes()).unwrap(),
                    name: "Expired".into()
                }
            ),
            Err(DirectError::Expired)
        ));
        let invitation = fixture.invite();
        let claim = fixture.claim(&invitation, &credential);
        {
            let mut state = fixture.state.lock().unwrap();
            state.claims[0].claim.expires_at_unix_seconds = 0;
            state.invitations[0].expires = 0;
        }
        assert!(matches!(
            fixture.management(Management::Approve(claim.clone())),
            Err(DirectError::Expired)
        ));
        assert!(matches!(
            fixture.authenticate(
                &credential,
                wire::Action::Cancel {
                    claim: claim.id,
                    key: claim.device_public_key
                }
            ),
            Err(DirectError::Expired)
        ));
        let ManagementReply::Devices(devices) = fixture.management(Management::Devices).unwrap()
        else {
            panic!()
        };
        assert!(devices.is_empty());
    }
}
