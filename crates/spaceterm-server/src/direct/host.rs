use super::*;
use crate::connection::Wake;
use crate::connection::registry::{PairedConnections, Sever};
use std::{
    net::{TcpListener, TcpStream},
    os::fd::{AsRawFd, RawFd},
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

/// Severs a paired connection by shutting its TCP read side. Its I/O thread then writes the queued
/// goodbye and closes.
struct ReadSide(TcpStream);
impl Sever for ReadSide {
    fn sever(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Read);
    }
}
struct PendingInvitation {
    id: ClaimId,
    token: [u8; 32],
    expires: u64,
    used: bool,
}
struct PendingClaim {
    claim: PairingClaim,
    decision: Option<Result<DeviceInfo, DirectError>>,
}

pub(crate) struct State {
    store: storage::Store,
    record: storage::Record,
    pub(super) pin: ServerPin,
    listener: Option<TcpListener>,
    /// The QUIC prototype's endpoint on the same address and port in UDP.
    #[cfg(feature = "iroh")]
    quic: Option<crate::connection::quic::QuicListener>,
    listener_state: ListenerState,
    invitations: Vec<PendingInvitation>,
    claims: Vec<PendingClaim>,
    connections: PairedConnections,
    uncertain: bool,
    generation: [u8; 32],
    released: std::collections::BTreeSet<[u8; 32]>,
    network: Arc<dyn network::Adapter>,
    /// Wakes the host's accept loop when the listener changes.
    wake: Option<Arc<dyn Wake>>,
}
impl State {
    pub(crate) fn open(paths: &crate::host::Paths) -> Result<Shared, DirectError> {
        Self::open_with_network(paths, Arc::new(network::Native))
    }
    fn open_with_network(
        paths: &crate::host::Paths,
        network: Arc<dyn network::Adapter>,
    ) -> Result<Shared, DirectError> {
        let store = storage::Store::open(paths)?;
        let record = store.load()?;
        let pin = store.pin(&record)?;
        let mut state = Self {
            store,
            record,
            pin,
            listener: None,
            #[cfg(feature = "iroh")]
            quic: None,
            listener_state: ListenerState::Off,
            invitations: Vec::new(),
            claims: Vec::new(),
            connections: PairedConnections::default(),
            uncertain: false,
            generation: crypto::random()?,
            released: Default::default(),
            network,
            wake: None,
        };
        state.listen();
        Ok(Arc::new(Mutex::new(state)))
    }
    pub(crate) fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        #[cfg(feature = "iroh")]
        if let Some(quic) = &self.quic {
            quic.set_wake(Arc::clone(&wake));
        }
        self.wake = Some(wake);
    }
    /// The listener the host's accept loop waits on.
    pub(crate) fn listener_fd(&self) -> Option<RawFd> {
        self.listener.as_ref().map(AsRawFd::as_raw_fd)
    }
    fn listener_changed(&self) {
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }
    fn listen(&mut self) {
        self.listener_changed();
        self.listener = None;
        #[cfg(feature = "iroh")]
        {
            self.quic = None;
        }
        self.listener_state = match self.record.policy.as_ref().filter(|_| self.record.enabled) {
            None => ListenerState::Off,
            Some(policy) => match bind_listener(policy.bind) {
                Ok(listener) => {
                    self.listener = Some(listener);
                    ListenerState::Listening { addr: policy.bind }
                }
                Err(_) => ListenerState::Failed(DirectError::Transport),
            },
        };
        #[cfg(feature = "iroh")]
        if let ListenerState::Listening { addr } = self.listener_state {
            self.quic = self.bind_quic(addr);
        }
    }
    /// Binds the QUIC prototype under the server's pinned key. WSS keeps working without it.
    #[cfg(feature = "iroh")]
    fn bind_quic(
        &self,
        address: std::net::SocketAddr,
    ) -> Option<crate::connection::quic::QuicListener> {
        use crate::connection::quic;
        let secret = quic::secret_key(&self.record.key, &self.pin.public_key).ok()?;
        let listener = quic::QuicListener::bind(secret, address).ok()?;
        if let Some(wake) = &self.wake {
            listener.set_wake(Arc::clone(wake));
        }
        Some(listener)
    }
    /// The next QUIC connection whose handshake completed, and the listener generation it
    /// belongs to.
    #[cfg(feature = "iroh")]
    pub(crate) fn accept_quic(
        &mut self,
    ) -> Option<(crate::connection::quic::QuicStream, [u8; 32])> {
        let stream = self.quic.as_ref()?.accept()?;
        Some((stream, self.generation))
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
            self.connections.end_all();
            self.listener = None;
            #[cfg(feature = "iroh")]
            {
                self.quic = None;
            }
            self.listener_changed();
            self.listener_state = ListenerState::Failed(error);
            return Err(error);
        }
        self.record = next;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn accept(&mut self) -> Option<(TcpStream, [u8; 32])> {
        self.accept_next().ok().flatten()
    }
    /// The next waiting connection and the listener generation it belongs to. `Ok(None)` means
    /// none waits; an error is the listener's, such as running out of descriptors.
    pub(crate) fn accept_next(&mut self) -> std::io::Result<Option<(TcpStream, [u8; 32])>> {
        let Some(listener) = self.listener.as_ref() else {
            return Ok(None);
        };
        loop {
            match listener.accept() {
                Ok((tcp, _)) => return Ok(Some((tcp, self.generation))),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if crate::host::accept_retries(&error) => {}
                Err(error) => return Err(error),
            }
        }
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
    /// Registers a paired connection of listener `generation`, so revocation reaches it. The
    /// authorization check and the registration share this lock with every revocation.
    pub(crate) fn register(
        &mut self,
        paired: &PairedDevice,
        generation: [u8; 32],
        connection: [u8; 32],
        outbox: crate::connection::outbox::Outbox,
        tcp: &TcpStream,
    ) -> Result<(), DirectError> {
        let read_side = tcp.try_clone().map_err(|_| DirectError::Transport)?;
        self.register_with(
            paired,
            generation,
            connection,
            outbox,
            Box::new(ReadSide(read_side)),
        )
    }
    /// Like `register`, with the transport's own way to sever the connection.
    pub(crate) fn register_with(
        &mut self,
        paired: &PairedDevice,
        generation: [u8; 32],
        connection: [u8; 32],
        outbox: crate::connection::outbox::Outbox,
        sever: Box<dyn Sever>,
    ) -> Result<(), DirectError> {
        self.authorized(paired)?;
        if generation != self.generation {
            return Err(DirectError::Disabled);
        }
        self.connections
            .register(
                connection,
                crate::connection::service::Pairing {
                    pairing: paired.pairing,
                    generation,
                },
                outbox,
                sever,
            )
            .map_err(|_| DirectError::Capacity)
    }
    /// The enrolled public key of `paired`'s device.
    #[cfg(feature = "iroh")]
    fn device_key(&self, paired: &PairedDevice) -> Option<[u8; 32]> {
        self.record
            .devices
            .iter()
            .find(|device| device.info.pairing == paired.pairing)
            .map(|device| device.info.public_key)
    }
    pub(crate) fn unregister(&mut self, connection: [u8; 32]) {
        self.connections.unregister(connection);
    }
    fn note_connection(&mut self, paired: &PairedDevice) -> Result<(), DirectError> {
        self.authorized(paired)?;
        let Some(now) = wire::now()
            .ok()
            .filter(|time| *time <= storage::MAX_CONNECTION_TIME)
        else {
            return Ok(());
        };
        let device = self
            .record
            .devices
            .iter_mut()
            .find(|device| device.info.pairing == paired.pairing)
            .ok_or(DirectError::AuthenticationRefused)?;
        if device
            .info
            .last_connected_at_unix_seconds
            .is_some_and(|previous| previous >= now)
        {
            return Ok(());
        }
        device.info.last_connected_at_unix_seconds = Some(now);
        // This locked snapshot changes metadata only. A failed metadata write must
        // not fence existing connections, rotate exposure, or change authority.
        let _ = self.store.save(&self.record);
        Ok(())
    }
    fn expire(&mut self) -> Result<(), DirectError> {
        let now = wire::now()?;
        self.invitations.retain(|v| v.expires > now);
        self.claims
            .retain(|v| v.claim.expires_at_unix_seconds > now);
        Ok(())
    }
    fn server_name() -> String {
        let mut name = [0u8; 256];
        // SAFETY: gethostname writes at most the supplied byte length.
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } == 0 {
            let count = name.iter().position(|b| *b == 0).unwrap_or(name.len());
            ClientName::from(String::from_utf8_lossy(&name[..count]).into_owned())
                .as_str()
                .to_owned()
        } else {
            String::new()
        }
    }
    fn status(&self) -> DirectStatus {
        let network = network::snapshot(self.network.as_ref());
        let listener = match (&self.listener_state, &network) {
            (ListenerState::Listening { addr }, Ok(snapshot))
                if !addr.ip().is_unspecified() && !snapshot.contains(&addr.ip()) =>
            {
                ListenerState::Failed(DirectError::InvalidAddress)
            }
            _ => self.listener_state.clone(),
        };
        DirectStatus {
            setup_generation: self.record.setup_generation,
            pin: self.pin.clone(),
            policy: self.record.policy.clone(),
            enabled: self.record.enabled,
            server_name: Self::server_name(),
            listener,
            network_addresses: network.map(|snapshot| snapshot.addresses),
        }
    }
    pub(crate) fn manage(
        &mut self,
        actor: Option<PairingId>,
        request: Management,
    ) -> Result<(ManagementReply, Option<[u8; 32]>), DirectError> {
        // Only authenticated owning-account IPC grants administration authority.
        // Refuse before recovery, expiry or mutation can affect retained state.
        if actor.is_some()
            && !matches!(
                &request,
                Management::Status
                    | Management::Claims
                    | Management::Devices
                    | Management::Revoke { .. }
            )
        {
            return Err(DirectError::AuthenticationRefused);
        }
        if let (Some(actor), Management::Revoke { target, .. }) = (actor, &request)
            && actor != *target
        {
            return Err(DirectError::AuthenticationRefused);
        }
        self.recover()?;
        self.expire()?;
        let reply = match request {
            Management::Status => ManagementReply::Status(self.status()),
            Management::PrepareSetup {
                generation,
                pin,
                expected_policy,
                expected_enabled,
                endpoint,
                policy,
                operation,
                public_key,
                name,
            } => {
                if actor.is_some() {
                    return Err(DirectError::AuthenticationRefused);
                }
                if setup_generation(&operation) != generation {
                    return Err(DirectError::Substituted);
                }
                let binding = storage::SetupReservation {
                    operation,
                    expected_policy,
                    expected_enabled,
                    endpoint,
                    policy,
                    public_key,
                    name,
                };
                self.check_setup(&pin, &binding)?;
                let storage::SetupReservation {
                    operation,
                    expected_policy,
                    expected_enabled,
                    endpoint,
                    policy,
                    public_key,
                    name,
                } = binding;
                if !self
                    .record
                    .devices
                    .iter()
                    .any(|device| device.claim == operation)
                    && (self.record.policy != expected_policy
                        || self.record.enabled != expected_enabled)
                {
                    return Err(DirectError::PolicyChanged);
                }
                if !self
                    .record
                    .setups
                    .iter()
                    .any(|setup| setup.operation == operation)
                {
                    if generation != self.record.setup_generation {
                        return Err(DirectError::AlreadyDecided);
                    }
                    if self.record.setups.len() >= storage::MAX_DEVICES {
                        return Err(DirectError::Capacity);
                    }
                    let next_generation = generation.checked_add(1).ok_or(DirectError::Capacity)?;
                    let mut next = self.record.clone();
                    next.setup_generation = next_generation;
                    next.setups.push(storage::SetupReservation {
                        operation,
                        expected_policy,
                        expected_enabled,
                        endpoint,
                        policy,
                        public_key,
                        name,
                    });
                    self.commit(next)?;
                }
                ManagementReply::SetupPrepared
            }
            Management::Setup {
                generation,
                pin,
                expected_policy,
                expected_enabled,
                endpoint,
                policy,
                operation,
                public_key,
                name,
            } => {
                if actor.is_some() {
                    return Err(DirectError::AuthenticationRefused);
                }
                if setup_generation(&operation) != generation {
                    return Err(DirectError::Substituted);
                }
                let binding = storage::SetupReservation {
                    operation,
                    expected_policy,
                    expected_enabled,
                    endpoint,
                    policy,
                    public_key,
                    name,
                };
                self.check_setup(&pin, &binding)?;
                let storage::SetupReservation {
                    operation,
                    expected_policy,
                    expected_enabled,
                    endpoint: _,
                    policy,
                    public_key,
                    name,
                } = binding;
                if self
                    .record
                    .devices
                    .iter()
                    .any(|device| device.claim == operation && device.revoked)
                {
                    return Err(DirectError::Revoked);
                }
                if !self
                    .record
                    .setups
                    .iter()
                    .any(|setup| setup.operation == operation)
                {
                    return Err(if generation < self.record.setup_generation {
                        DirectError::AlreadyDecided
                    } else {
                        DirectError::InvalidRequest
                    });
                }
                let info = if let Some(device) =
                    self.record.devices.iter().find(|d| d.claim == operation)
                {
                    if device.revoked {
                        return Err(DirectError::Revoked);
                    }
                    device.info.clone()
                } else {
                    if self.record.policy != expected_policy
                        || self.record.enabled != expected_enabled
                    {
                        return Err(DirectError::PolicyChanged);
                    }
                    if self.record.devices.len() >= storage::MAX_DEVICES {
                        return Err(DirectError::Capacity);
                    }
                    let info = DeviceInfo {
                        pairing: crypto::random()?,
                        client: crypto::random()?,
                        name,
                        public_key,
                        last_connected_at_unix_seconds: None,
                    };
                    let changed_policy =
                        self.record.policy.as_ref() != Some(&policy) || !self.record.enabled;
                    let generation = crypto::random()?;
                    let mut next = self.record.clone();
                    next.policy = Some(policy.clone());
                    next.enabled = true;
                    next.devices.push(storage::Device {
                        claim: operation,
                        info: info.clone(),
                        revoked: false,
                    });
                    self.commit(next)?;
                    if changed_policy {
                        self.generation = generation;
                        self.connections.retire_generation(generation);
                        self.invitations.clear();
                        for pending in &mut self.claims {
                            if pending.decision.is_none() {
                                pending.decision = Some(Err(DirectError::AlreadyDecided));
                            }
                        }
                        self.listen();
                    }
                    info
                };
                ManagementReply::Setup(PairedDevice {
                    pin: self.pin.clone(),
                    pairing: info.pairing,
                    client: info.client,
                })
            }
            Management::CancelSetup { pin, operation } => {
                if actor.is_some() {
                    return Err(DirectError::AuthenticationRefused);
                }
                if pin != self.pin {
                    return Err(DirectError::PinMismatch);
                }
                if operation == [0; 32] {
                    return Err(DirectError::InvalidRequest);
                }
                if let Some(device) = self.record.devices.iter().find(|d| d.claim == operation) {
                    let pairing = device.info.pairing;
                    let cancellation = crypto::hash(&wire::transcript(
                        "cancel", &self.pin, &operation, &pairing,
                    )?);
                    let (_, client) = self.revoke(Some(pairing), cancellation, pairing)?;
                    return Ok((
                        ManagementReply::SetupCancelled {
                            pin: self.pin.clone(),
                            operation,
                        },
                        Some(client),
                    ));
                }
                self.cancel_reservation(operation, None, true)?;
                ManagementReply::SetupCancelled {
                    pin: self.pin.clone(),
                    operation,
                }
            }
            Management::SetPolicy { policy, enabled } => {
                if enabled && policy.is_none() && self.record.policy.is_none() {
                    return Err(DirectError::InvalidRequest);
                }
                if let Some(policy) = &policy {
                    NetworkPolicy::new(policy.bind)?;
                }
                let generation = crypto::random()?;
                let mut next = self.record.clone();
                if let Some(policy) = policy {
                    next.policy = Some(policy);
                }
                next.enabled = enabled;
                self.commit(next)?;
                self.generation = generation;
                // Network administration arrives only over IPC. Retire all WSS authority.
                self.connections.retire_generation(generation);
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
                let status = self.status();
                let endpoint = match status.listener {
                    ListenerState::Listening { addr } => {
                        let advertised = if addr.ip().is_unspecified() {
                            let addresses =
                                status.network_addresses.as_ref().map_err(|error| *error)?;
                            std::net::SocketAddr::new(
                                addresses
                                    .suggested
                                    .filter(|ip| ip.is_ipv4() == addr.is_ipv4())
                                    .or_else(|| {
                                        addresses
                                            .available
                                            .iter()
                                            .copied()
                                            .find(|ip| ip.is_ipv4() == addr.is_ipv4())
                                    })
                                    .ok_or(DirectError::InvalidAddress)?,
                                addr.port(),
                            )
                        } else {
                            addr
                        };
                        format!("wss://{advertised}/spaceterm")
                    }
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
                    server_name: status.server_name,
                    token: crypto::random()?,
                    expires: wire::now()? + wire::INVITATION_SECONDS,
                    valid_for_seconds: wire::INVITATION_SECONDS,
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
                    .filter(|c| c.claim.invitation_id == id && c.decision.is_none())
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
                    last_connected_at_unix_seconds: None,
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
    fn check_setup(
        &self,
        pin: &ServerPin,
        requested: &storage::SetupReservation,
    ) -> Result<(), DirectError> {
        if pin != &self.pin {
            return Err(DirectError::PinMismatch);
        }
        if let Some(original) = self
            .record
            .setups
            .iter()
            .find(|setup| setup.operation == requested.operation)
            && (original.policy != requested.policy
                || original.expected_policy != requested.expected_policy
                || original.expected_enabled != requested.expected_enabled
                || original.endpoint != requested.endpoint
                || original.public_key != requested.public_key
                || original.name != requested.name)
        {
            return Err(DirectError::Substituted);
        }
        NetworkPolicy::new(requested.policy.bind)?;
        let endpoint = Endpoint::from_url(&requested.endpoint)?;
        if endpoint.url() != requested.endpoint {
            return Err(DirectError::InvalidAddress);
        }
        if let Some(policy) = &requested.expected_policy {
            NetworkPolicy::new(policy.bind)?;
        }
        if (requested.expected_enabled && requested.expected_policy.is_none())
            || requested.operation == [0; 32]
            || requested.public_key == [0; 32]
        {
            return Err(DirectError::InvalidRequest);
        }
        Ok(())
    }
    fn cancel_reservation(
        &mut self,
        operation: OperationId,
        key: Option<[u8; 32]>,
        account_local: bool,
    ) -> Result<(), DirectError> {
        if let Some(index) = self
            .record
            .setups
            .iter()
            .position(|setup| setup.operation == operation)
        {
            if key.is_some_and(|key| key != self.record.setups[index].public_key) {
                return Err(DirectError::Substituted);
            }
            let mut next = self.record.clone();
            next.setups.remove(index);
            return self.commit(next);
        }
        let generation = setup_generation(&operation);
        if generation < self.record.setup_generation {
            return Ok(());
        }
        if !account_local || generation != self.record.setup_generation {
            return Err(DirectError::Expired);
        }
        let mut next = self.record.clone();
        next.setup_generation = generation.checked_add(1).ok_or(DirectError::Capacity)?;
        self.commit(next)
    }
    pub(crate) fn revoke(
        &mut self,
        actor: Option<PairingId>,
        operation: OperationId,
        target: PairingId,
    ) -> Result<(RevocationReceipt, [u8; 32]), DirectError> {
        if actor.is_some_and(|actor| actor != target) {
            return Err(DirectError::AuthenticationRefused);
        }
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
            next.setups.retain(|setup| setup.operation != device.claim);
            next.revocations.push(storage::Revocation {
                actor,
                receipt: receipt.clone(),
            });
            self.commit(next)?;
            receipt
        };
        self.connections.revoke(target);
        Ok((receipt, client))
    }
    pub(super) fn challenge(&self) -> Result<wire::Challenge, DirectError> {
        if !matches!(self.listener_state, ListenerState::Listening { .. }) {
            return Err(DirectError::Disabled);
        }
        let nonce = crypto::random()?;
        let server_name = Self::server_name();
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
    /// Authenticates one pairing exchange, and names the client whose grants a revocation or
    /// cancellation in it retired.
    pub(super) fn authenticate_and_resolve(
        &mut self,
        challenge: &wire::Challenge,
        authentication: &wire::Authentication,
    ) -> (Result<wire::Answer, DirectError>, Option<[u8; 32]>) {
        let answer = self.authenticate(challenge, authentication);
        let retired = match &answer {
            Ok(wire::Answer::Revoked(receipt) | wire::Answer::Cancelled(Some(receipt))) => {
                self.revocation_client(receipt.target)
            }
            _ => None,
        };
        (answer, retired)
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
            wire::Action::Revoke { actor, target, .. } => {
                if actor.pin != self.pin {
                    return Err(DirectError::WrongServer);
                }
                if actor.pairing != *target {
                    return Err(DirectError::AuthenticationRefused);
                }
                let device = self
                    .record
                    .devices
                    .iter()
                    .find(|d| d.info.pairing == actor.pairing && d.info.client == actor.client)
                    .ok_or(DirectError::AuthenticationRefused)?;
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
                    return Ok(wire::Answer::Claim(pending.claim.clone()));
                }
                if invitation.used {
                    return Err(DirectError::AlreadyDecided);
                }
                if self.claims.len() >= MAX_PENDING {
                    return Err(DirectError::Capacity);
                }
                let claim = PairingClaim {
                    id: *id,
                    invitation_id: invitation.id,
                    server: self.pin.server,
                    device_public_key: *key,
                    device_name: name.clone(),
                    expires_at_unix_seconds: invitation.expires,
                    exchange_hash: wire::exchange_hash(challenge, authentication)?,
                };
                self.invitations[invitation_index].used = true;
                self.claims.push(PendingClaim {
                    claim: claim.clone(),
                    decision: None,
                });
                Ok(wire::Answer::Claim(claim))
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
                if self
                    .record
                    .setups
                    .iter()
                    .any(|setup| setup.operation == *claim)
                {
                    self.cancel_reservation(*claim, Some(*key), false)?;
                    return Ok(wire::Answer::Cancelled(None));
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
                // A retired setup generation can never prepare or admit another operation.
                if setup_generation(claim) < self.record.setup_generation {
                    return Ok(wire::Answer::Cancelled(None));
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
        #[cfg(feature = "iroh")]
        {
            self.quic = None;
        }
        self.listener_state = ListenerState::Off;
        self.listener_changed();
        self.connections.end_all();
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

/// Serves one accepted TCP connection: the pinned WSS handshake, one pairing exchange, and for a
/// paired device admission and then the protocol connection over a [`crate::connection::wss::WssCarrier`].
pub(crate) fn serve(
    tcp: TcpStream,
    generation: [u8; 32],
    state: Shared,
    environment: &crate::host::Environment,
    waker: crate::connection::io::Waker,
    wake: Arc<dyn Wake>,
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
        // Revocation ends registered connections inside the direct-network lock, before the
        // service learns of it. The locks are taken in the same order as everywhere else.
        let answer = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .authenticate_and_resolve(&challenge, &authentication);
        let (answer, retired) = answer;
        if let Some(client) = retired {
            environment
                .panes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .revoked(client);
        }
        answer
    };
    let connected = matches!(answer, Ok(wire::Answer::Connected));
    wss::send(&mut ws, &answer)?;
    if !connected {
        return Ok(());
    }
    let paired = paired.ok_or(ServerError::AuthenticationRefused)?;
    let (_, client, name) = crate::protocol::admit_direct_server(&mut ws, &paired)?;
    let connection = crate::protocol::random()?;
    let outbox = crate::connection::outbox::Outbox::new(Arc::clone(&wake));
    state.lock().unwrap_or_else(|e| e.into_inner()).register(
        &paired,
        generation,
        connection,
        outbox.clone(),
        ws.get_ref().tcp(),
    )?;
    // Unregisters when serving ends, also when it panics.
    let _registered = Registered {
        state: Arc::clone(&state),
        connection,
    };
    (|| {
        state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .note_connection(&paired)?;
        let peer = crate::connection::service::Peer {
            client,
            connection,
            pairing: Some(crate::connection::service::Pairing {
                pairing: paired.pairing,
                generation,
            }),
        };
        crate::host::serve_admitted(
            Box::new(crate::connection::wss::WssCarrier::new(ws)),
            peer,
            name,
            outbox,
            waker,
            environment,
        );
        Ok::<_, ServerError>(())
    })()
}

/// Serves one QUIC connection like [`serve`]: one pairing exchange on its L0 stream, and for a
/// paired device whose proven iroh key is its enrolled key, admission and then the protocol
/// connection over a [`crate::connection::quic::IrohCarrier`]. The prototype carries connections
/// only, so any other action is refused.
#[cfg(feature = "iroh")]
pub(crate) fn serve_quic(
    mut stream: crate::connection::quic::QuicStream,
    generation: [u8; 32],
    state: Shared,
    environment: &crate::host::Environment,
    waker: crate::connection::io::Waker,
    wake: Arc<dyn Wake>,
) -> Result<(), ServerError> {
    use super::client::Messages;
    let challenge = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        if generation != state.generation {
            return Err(DirectError::Disabled.into());
        }
        state.challenge()?
    };
    stream.send(&challenge)?;
    let authentication: wire::Authentication = stream.receive()?;
    let wire::Action::Connect { paired } = &authentication.action else {
        return Err(DirectError::InvalidRequest.into());
    };
    let paired = paired.clone();
    let answer = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if state.device_key(&paired) != Some(stream.remote_key()) {
            return Err(ServerError::AuthenticationRefused);
        }
        state
            .authenticate_and_resolve(&challenge, &authentication)
            .0
    };
    let connected = matches!(answer, Ok(wire::Answer::Connected));
    stream.send(&answer)?;
    if !connected {
        return Ok(());
    }
    let (_, client, name) = crate::protocol::admit_direct_server(&mut stream, &paired)?;
    let connection = crate::protocol::random()?;
    let outbox = crate::connection::outbox::Outbox::new(Arc::clone(&wake));
    let carrier = stream.into_carrier().map_err(|_| ServerError::Protocol)?;
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .register_with(
            &paired,
            generation,
            connection,
            outbox.clone(),
            Box::new(carrier.sever()),
        )?;
    let _registered = Registered {
        state: Arc::clone(&state),
        connection,
    };
    state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .note_connection(&paired)?;
    let peer = crate::connection::service::Peer {
        client,
        connection,
        pairing: Some(crate::connection::service::Pairing {
            pairing: paired.pairing,
            generation,
        }),
    };
    crate::host::serve_admitted(Box::new(carrier), peer, name, outbox, waker, environment);
    Ok(())
}

/// A paired connection's place in the registry, removed when it drops.
struct Registered {
    state: Shared,
    connection: [u8; 32],
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unregister(self.connection);
    }
}

#[cfg(test)]
mod network_tests;

#[cfg(test)]
mod connection_time_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_invitations_with_the_same_expiry_keep_exact_claim_and_approval_bindings() {
        let fixture = super::super::tests::Fixture::new();
        fixture.enable();
        let mut first = fixture.invite();
        let mut second = fixture.invite();
        // Give both real invitations the same deadline regardless of a clock tick
        // between issuance. Their independent secrets and identities stay intact.
        let expires = wire::now().unwrap() + 300;
        first.payload.expires = expires;
        second.payload.expires = expires;
        for invitation in &mut fixture.state.lock().unwrap().invitations {
            invitation.expires = expires;
        }
        let credential = DeviceCredential::generate().unwrap();
        let first_claim = fixture.claim(&first, &credential);
        let second_claim = fixture.claim(&second, &credential);
        assert_eq!(
            first_claim.expires_at_unix_seconds,
            second_claim.expires_at_unix_seconds
        );
        assert_ne!(first_claim.id, second_claim.id);
        assert_eq!(first_claim.invitation_id, first.id());
        assert_eq!(second_claim.invitation_id, second.id());
        assert_ne!(first_claim.invitation_id, second_claim.invitation_id);
        let mut substituted = first_claim.clone();
        substituted.invitation_id = second.id();
        assert!(matches!(
            fixture.management(Management::Approve(substituted)),
            Err(DirectError::Substituted)
        ));
        // Withdrawing one invitation cannot reject a claim from the other, even
        // when device key, name and expiry are identical.
        fixture
            .management(Management::Withdraw(first.id()))
            .unwrap();
        assert!(matches!(
            fixture.management(Management::Approve(first_claim)),
            Err(DirectError::AlreadyDecided)
        ));
        assert!(matches!(
            fixture.management(Management::Approve(second_claim)),
            Ok(ManagementReply::Approved(_))
        ));
    }

    #[test]
    fn five_minute_invitations_stay_one_use_while_pending_and_after_denial() {
        let fixture = super::super::tests::Fixture::new();
        fixture.enable();
        let before = wire::now().unwrap();
        let invitation = fixture.invite();
        let after = wire::now().unwrap();
        assert!((before + 300..=after + 300).contains(&invitation.expires_at_unix_seconds()));
        assert_eq!(invitation.lifetime(), std::time::Duration::from_secs(300));
        let original = DeviceCredential::generate().unwrap();
        let claim = fixture.claim(&invitation, &original);
        let other = DeviceCredential::generate().unwrap();
        let prepared = Enrollment::prepare(&invitation, "Other device".into(), &other).unwrap();
        for denied in [false, true] {
            if denied {
                fixture.management(Management::Reject(claim.id)).unwrap();
            }
            assert!(matches!(
                fixture.authenticate(
                    &other,
                    wire::Action::Claim {
                        id: prepared.claim_id(),
                        token: invitation.payload.token,
                        key: crypto::public(other.expose_bytes()).unwrap(),
                        name: "Other device".into(),
                    }
                ),
                Err(DirectError::AlreadyDecided)
            ));
        }
        assert!(matches!(
            fixture.authenticate(
                &original,
                wire::Action::Finish {
                    claim: claim.id,
                    key: claim.device_public_key,
                }
            ),
            Err(DirectError::Rejected)
        ));
        assert!(
            matches!(fixture.management(Management::Devices), Ok(ManagementReply::Devices(devices)) if devices.is_empty())
        );
    }

    #[test]
    fn signed_cancellation_after_expiry_and_lost_approval_revokes_only_the_created_pairing() {
        let fixture = super::super::tests::Fixture::new();
        fixture.enable();
        let credential = DeviceCredential::generate().unwrap();
        let invitation = fixture.invite();
        let claim = fixture.claim(&invitation, &credential);
        let ManagementReply::Approved(created) = fixture
            .management(Management::Approve(claim.clone()))
            .unwrap()
        else {
            panic!()
        };
        {
            let mut state = fixture.state.lock().unwrap();
            state.invitations[0].expires = 0;
            state.claims[0].claim.expires_at_unix_seconds = 0;
        }
        // The client never observed Finish. Expiry removes transient state, while
        // a later invitation grants the same key a separate pairing.
        let newer = fixture.enroll(&credential);
        assert!(
            fixture
                .state
                .lock()
                .unwrap()
                .claims
                .iter()
                .all(|pending| pending.claim.id != claim.id)
        );
        let mut original = Enrollment::resume(
            invitation.endpoint(),
            invitation.server_pin().clone(),
            claim.id,
            &credential,
        )
        .unwrap();
        let server = fixture.serve_next();
        let receipt = original.cancel().unwrap().unwrap();
        server.join().unwrap().unwrap();
        assert_eq!(receipt.target, created.pairing);
        let server = fixture.serve_next();
        assert_eq!(original.cancel(), Ok(Some(receipt)));
        server.join().unwrap().unwrap();
        assert!(matches!(
            fixture.authenticate(
                &credential,
                wire::Action::Connect {
                    paired: newer.clone()
                }
            ),
            Ok(wire::Answer::Connected)
        ));
        let ManagementReply::Devices(devices) = fixture.management(Management::Devices).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            devices
                .iter()
                .map(|device| device.pairing)
                .collect::<Vec<_>>(),
            vec![newer.pairing]
        );
    }

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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (tcp, _) = listener.accept().unwrap();
        let outbox = crate::connection::outbox::Outbox::new(Arc::new(|| {}));
        let registered = state.register(&paired, generation, [5; 32], outbox, &tcp);
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
        assert!(
            Enrollment::prepare(&invitation, "Expired".into(), &credential).is_ok(),
            "only the server's clock decides whether an invitation is expired"
        );
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
