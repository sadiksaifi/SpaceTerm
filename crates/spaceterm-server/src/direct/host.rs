use super::*;
use crate::connection::Wake;
use crate::connection::quic::{QuicListener, QuicStream};
use crate::connection::registry::{PairedConnections, Sever};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

const MAX_PENDING: usize = 32;
/// Why a listener did not bind.
enum Bind {
    /// Another socket, such as a retired listener's, holds the address.
    InUse,
    Other,
}
/// How often a `Starting` listener tries its address again while it waits.
const LISTEN_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
pub(crate) type Shared = Arc<Mutex<State>>;
/// What a revocation also forgets outside this store.
pub(crate) type Revoked = Box<dyn Fn(&PairingId) + Send>;

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
    listener: Option<QuicListener>,
    /// When a `Starting` listener first found its address in use.
    starting_since: Option<std::time::Instant>,
    /// Listeners replaced by a policy change, still routing to their connections until those
    /// end. A listener at the same address is taken back instead of binding again.
    retired: Vec<QuicListener>,
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
    /// Runs after each revocation, whichever transport asked for it.
    revoked: Option<Revoked>,
    /// Agent identities that may not register yet, each with the number of [`Retirement`]s that
    /// hold it, and the signal that one ended. An admission that would evict a name holds every
    /// idle name while the pane service reports which of them hold nothing, and an evicted
    /// identity is held while its control grants are released. Both run outside this lock, so a
    /// returning agent waits for them.
    retiring: std::collections::BTreeMap<[u8; 32], usize>,
    retired_agent: Arc<Condvar>,
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
            starting_since: None,
            retired: Vec::new(),
            listener_state: ListenerState::Off,
            invitations: Vec::new(),
            claims: Vec::new(),
            connections: PairedConnections::default(),
            uncertain: false,
            generation: crypto::random()?,
            released: Default::default(),
            network,
            wake: None,
            revoked: None,
            retiring: Default::default(),
            retired_agent: Arc::default(),
        };
        state.listen();
        Ok(Arc::new(Mutex::new(state)))
    }
    pub(crate) fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        if let Some(listener) = &self.listener {
            listener.set_wake(Arc::clone(&wake));
        }
        self.wake = Some(wake);
    }
    /// Runs `revoked` after every revocation, so state outside this store forgets the pairing.
    pub(crate) fn on_revoke(&mut self, revoked: Revoked) {
        self.revoked = Some(revoked);
    }
    fn listener_changed(&self) {
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }
    fn listen(&mut self) {
        self.listener_changed();
        self.retire_listener();
        self.starting_since = None;
        self.listener_state = match self.record.policy.as_ref().filter(|_| self.record.enabled) {
            None => ListenerState::Off,
            Some(policy) => self.open_at(policy.bind),
        };
    }
    /// Listens at `address`: takes back a retired listener there, or binds a new one. A bind
    /// that a retired listener's live connections still block waits as `Starting` until they
    /// end; [`Self::retry_listen`] then binds again.
    fn open_at(&mut self, address: std::net::SocketAddr) -> ListenerState {
        let revived = self
            .retired
            .iter()
            .position(|listener| listener.address() == address && listener.revive())
            .map(|index| self.retired.remove(index));
        if let Some(listener) = revived {
            self.listener = Some(listener);
            return ListenerState::Listening { addr: address };
        }
        self.retired.retain(|listener| !listener.finished());
        match self.bind(address) {
            Ok(listener) => {
                self.listener = Some(listener);
                ListenerState::Listening { addr: address }
            }
            Err(Bind::InUse) if !self.retired.is_empty() => {
                ListenerState::Starting { addr: address }
            }
            // A connection that just ended may still hold the socket for a moment.
            Err(Bind::InUse)
                if self
                    .starting_since
                    .is_none_or(|since| since.elapsed() < crate::protocol::limits::STALL_AFTER) =>
            {
                self.starting_since
                    .get_or_insert_with(std::time::Instant::now);
                ListenerState::Starting { addr: address }
            }
            Err(_) => ListenerState::Failed(DirectError::Transport),
        }
    }
    /// Binds again while `Starting`, once retired listeners finished. Returns when to try
    /// again, if still waiting.
    pub(crate) fn retry_listen(&mut self) -> Option<std::time::Instant> {
        let ListenerState::Starting { addr } = self.listener_state else {
            self.starting_since = None;
            return None;
        };
        if self
            .starting_since
            .is_some_and(|since| since.elapsed() >= crate::protocol::limits::STALL_AFTER)
            && self.retired.iter().all(QuicListener::finished)
        {
            self.listener_state = ListenerState::Failed(DirectError::Transport);
            return None;
        }
        self.listener_state = self.open_at(addr);
        if matches!(self.listener_state, ListenerState::Starting { .. }) {
            self.starting_since
                .get_or_insert_with(std::time::Instant::now);
            Some(std::time::Instant::now() + LISTEN_RETRY)
        } else {
            self.starting_since = None;
            self.listener_changed();
            None
        }
    }
    /// Keeps the listener, its connections and its generation when the saved exposure covers the
    /// address it listens at, as when exposure widens from one address to every interface at the
    /// same port. Nothing that was reachable becomes unreachable, so nothing is retired. Returns
    /// false when the listener must start over instead.
    fn widen(&mut self) -> bool {
        let ListenerState::Listening { addr } = self.listener_state else {
            return false;
        };
        let Some(policy) = self.record.policy.clone().filter(|_| self.record.enabled) else {
            return false;
        };
        if !policy.covers(addr) {
            return false;
        }
        if policy.bind == addr {
            return true;
        }
        // Connections of an earlier address that still end hold its port, so the wildcard can't
        // bind; then this starts over without closing anything first.
        self.retired.retain(|listener| !listener.finished());
        if self
            .retired
            .iter()
            .any(|listener| listener.address().port() == addr.port())
        {
            return false;
        }
        let Some(listener) = &self.listener else {
            return false;
        };
        if listener.widen().is_err() {
            return false;
        }
        self.listener_state = ListenerState::Listening { addr: policy.bind };
        self.listener_changed();
        true
    }
    /// Stops taking connections. A listener with live connections keeps routing to them until
    /// they end; one without closes its socket now.
    fn retire_listener(&mut self) {
        if let Some(listener) = self.listener.take()
            && listener.retire()
        {
            self.retired.push(listener);
        }
        self.retired.retain(|listener| !listener.finished());
    }
    /// Binds QUIC on UDP under the pinned certificate. The listener rings the host's wake when a
    /// connection arrives.
    fn bind(&self, address: std::net::SocketAddr) -> Result<QuicListener, Bind> {
        let config = super::quic::server_config(&self.record.certificate, &self.record.key)
            .map_err(|_| Bind::Other)?;
        let listener = QuicListener::bind(address, config).map_err(|error| match error {
            crate::connection::quic::QuicError::AddressInUse => Bind::InUse,
            _ => Bind::Other,
        })?;
        if let Some(wake) = &self.wake {
            listener.set_wake(Arc::clone(wake));
        }
        Ok(listener)
    }
    /// The next new connection and the listener generation it belongs to. Its own thread
    /// completes the handshake.
    pub(crate) fn accept(&mut self) -> Option<(QuicStream, [u8; 32])> {
        let stream = self.listener.as_ref()?.accept()?;
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
            self.retire_listener();
            self.listener_changed();
            self.listener_state = ListenerState::Failed(error);
            return Err(error);
        }
        self.record = next;
        Ok(())
    }
    #[cfg(test)]
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
    /// Whether `paired`, admitted on listener `generation`, still holds its authority: the
    /// pairing is not revoked and Network Access has not changed since.
    pub(crate) fn current(
        &mut self,
        paired: &PairedDevice,
        generation: [u8; 32],
    ) -> Result<(), DirectError> {
        self.authorized(paired)?;
        if generation != self.generation {
            return Err(DirectError::Disabled);
        }
        Ok(())
    }
    /// Registers a paired connection of listener `generation`, so revocation reaches it through
    /// `sever`. The authorization check and the registration share this lock with every
    /// revocation. `client` is the connection's effective Client Identity. A service-lane
    /// connection has no `outbox`.
    pub(crate) fn register(
        &mut self,
        paired: &PairedDevice,
        generation: [u8; 32],
        connection: [u8; 32],
        client: [u8; 32],
        outbox: Option<crate::connection::outbox::Outbox>,
        sever: Box<dyn Sever>,
    ) -> Result<(), DirectError> {
        self.current(paired, generation)?;
        self.connections
            .register(
                connection,
                client,
                crate::connection::service::Pairing {
                    pairing: paired.pairing,
                    generation,
                },
                outbox,
                sever,
            )
            .map_err(|_| DirectError::Capacity)
    }
    /// Whether admitting `agent` under `paired` now evicts another name.
    pub(crate) fn evicts(
        &mut self,
        paired: &PairedDevice,
        agent: &crate::protocol::AgentName,
    ) -> Result<bool, DirectError> {
        self.authorized(paired)?;
        let device = self
            .record
            .devices
            .iter()
            .find(|device| device.info.pairing == paired.pairing)
            .ok_or(DirectError::AuthenticationRefused)?;
        Ok(!device.agents.contains(agent) && device.agents.len() >= storage::MAX_AGENTS)
    }
    /// The agent identities that admitting `agent` under `paired` could evict: every listed name
    /// with no live connection, from least to most recently admitted. They are held in
    /// `retiring` until the caller releases them, so none registers, and none can gain an edit
    /// outcome or a run receipt, while the pane service reports which of them hold one. `None`
    /// when `agent` evicts nothing.
    pub(crate) fn reserve_evictable(
        &mut self,
        paired: &PairedDevice,
        agent: &crate::protocol::AgentName,
    ) -> Result<Option<Vec<[u8; 32]>>, DirectError> {
        if !self.evicts(paired, agent)? {
            return Ok(None);
        }
        let idle: Vec<_> = self
            .record
            .devices
            .iter()
            .filter(|device| device.info.pairing == paired.pairing)
            .flat_map(|device| {
                device
                    .agents
                    .iter()
                    .map(|known| known.client(&device.info.client))
            })
            .filter(|client| !self.connections.serves(*client))
            .collect();
        self.hold_retiring(&idle);
        Ok(Some(idle))
    }
    /// Records that `paired` admitted `agent`, before the agent's connection can create any
    /// state, so revoking the pairing retires the agent too. Callers hold this lock through
    /// [`Self::register`], so no revocation falls between the two.
    ///
    /// The pairing's names are kept from least to most recently admitted. A new name past
    /// [`storage::MAX_AGENTS`] evicts the least recently admitted name whose identity is in
    /// `settled` and has no live connection, and returns its Client Identity. `settled` holds
    /// the identities from [`Self::reserve_evictable`] that keep no edit outcome or run receipt,
    /// because revocation reaches only listed names and nothing else would ever free those. The
    /// new name is refused with `Capacity` when no listed name is settled and idle. The caller
    /// releases the evicted identity's control grants through [`Retirement`], so revocation
    /// never needs a name it forgot. An evicted agent that returns is admitted again once that
    /// release ends.
    pub(crate) fn admit_agent(
        &mut self,
        paired: &PairedDevice,
        agent: &crate::protocol::AgentName,
        settled: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<Option<[u8; 32]>, DirectError> {
        self.authorized(paired)?;
        let device = self
            .record
            .devices
            .iter()
            .find(|device| device.info.pairing == paired.pairing)
            .ok_or(DirectError::AuthenticationRefused)?;
        if device.agents.last() == Some(agent) {
            return Ok(None);
        }
        let mut agents = device.agents.clone();
        agents.retain(|known| known != agent);
        let mut evicted = None;
        if agents.len() >= storage::MAX_AGENTS {
            let index = agents
                .iter()
                .map(|known| known.client(&device.info.client))
                .position(|client| settled.contains(&client) && !self.connections.serves(client))
                .ok_or(DirectError::Capacity)?;
            evicted = Some(agents.remove(index).client(&device.info.client));
        }
        agents.push(agent.clone());
        let mut next = self.record.clone();
        next.devices
            .iter_mut()
            .find(|device| device.info.pairing == paired.pairing)
            .ok_or(DirectError::AuthenticationRefused)?
            .agents = agents;
        self.commit(next)?;
        if let Some(evicted) = evicted {
            self.hold_retiring(&[evicted]);
        }
        Ok(evicted)
    }
    /// Holds each of `clients` in `retiring` once more.
    fn hold_retiring(&mut self, clients: &[[u8; 32]]) {
        for client in clients {
            *self.retiring.entry(*client).or_default() += 1;
        }
    }
    /// Ends one hold of each of `clients` in `retiring`, and wakes the agents waiting on them.
    fn release_retiring(&mut self, clients: &[[u8; 32]]) {
        for client in clients {
            if let std::collections::btree_map::Entry::Occupied(mut held) =
                self.retiring.entry(*client)
            {
                *held.get_mut() -= 1;
                if *held.get() == 0 {
                    held.remove();
                }
            }
        }
        self.retired_agent.notify_all();
    }
    /// The enrolled public key of `paired`'s device.
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
            abandoned_setups: self.record.abandoned_setups.clone(),
        }
    }
    pub(crate) fn manage(
        &mut self,
        actor: Option<PairingId>,
        request: Management,
    ) -> Result<(ManagementReply, Vec<[u8; 32]>), DirectError> {
        // Only authenticated owning-account IPC grants administration authority.
        // Refuse before recovery, expiry or mutation can affect retained state.
        if actor.is_some()
            && !matches!(
                &request,
                Management::Status
                    | Management::Claims { .. }
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
            Management::Status => {
                let _ = self.retry_listen();
                ManagementReply::Status(self.status())
            }
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
                let policy = NetworkPolicy::new(policy.bind)?;
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
                        agents: Vec::new(),
                    });
                    self.commit(next)?;
                    if changed_policy && !self.widen() {
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
                    let (_, clients) = self.revoke(Some(pairing), cancellation, pairing)?;
                    return Ok((
                        ManagementReply::SetupCancelled {
                            pin: self.pin.clone(),
                            operation,
                        },
                        clients,
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
                let policy = policy
                    .map(|policy| NetworkPolicy::new(policy.bind))
                    .transpose()?;
                // One address must belong to this machine. Every interface always does.
                if let Some(policy) = &policy
                    && !policy.is_every_interface()
                    && !network::snapshot(self.network.as_ref())?.contains(&policy.bind.ip())
                {
                    return Err(DirectError::InvalidAddress);
                }
                let generation = crypto::random()?;
                let mut next = self.record.clone();
                if let Some(policy) = policy {
                    next.policy = Some(policy);
                }
                next.enabled = enabled;
                self.commit(next)?;
                if self.widen() {
                    return Ok((ManagementReply::Status(self.status()), Vec::new()));
                }
                self.generation = generation;
                // Network administration arrives only over IPC. Any other change retires all
                // QUIC authority.
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
            Management::Invite { advertise } => {
                let status = self.status();
                let ListenerState::Listening { addr } = status.listener else {
                    return Err(DirectError::Disabled);
                };
                let endpoint = if let Some(authority) = advertise {
                    super::Endpoint::parse(&authority)?.url()
                } else {
                    let address = if addr.ip().is_unspecified() {
                        network::snapshot(self.network.as_ref())?
                            .advertised()
                            .ok_or(DirectError::InvalidAddress)?
                    } else {
                        addr.ip()
                    };
                    format!("quic://{}", std::net::SocketAddr::new(address, addr.port()))
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
            Management::Claims { invitation } => ManagementReply::Claims(
                self.claims
                    .iter()
                    .filter(|c| {
                        c.decision.is_none()
                            && invitation.is_none_or(|id| c.claim.invitation_id == id)
                    })
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
                    agents: Vec::new(),
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
                let (receipt, clients) = self.revoke(actor, operation, target)?;
                return Ok((ManagementReply::Revoked(receipt), clients));
            }
        };
        Ok((reply, Vec::new()))
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
    ) -> Result<(RevocationReceipt, Vec<[u8; 32]>), DirectError> {
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
        let clients = device.clients();
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
                    clients,
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
        if let Some(revoked) = &self.revoked {
            revoked(&target);
        }
        Ok((receipt, clients))
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
    /// Authenticates one pairing exchange, and names the clients whose grants a revocation or
    /// cancellation in it retired: the pairing's own and its agents'.
    pub(super) fn authenticate_and_resolve(
        &mut self,
        challenge: &wire::Challenge,
        authentication: &wire::Authentication,
    ) -> (Result<wire::Answer, DirectError>, Vec<[u8; 32]>) {
        let answer = self.authenticate(challenge, authentication);
        let retired = match &answer {
            Ok(wire::Answer::Revoked(receipt) | wire::Answer::Cancelled(Some(receipt))) => {
                self.revocation_clients(receipt.target)
            }
            _ => Vec::new(),
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
    /// The Client Identities a revocation of `target` retires: the pairing's own and its agents'.
    pub(crate) fn revocation_clients(&self, target: PairingId) -> Vec<[u8; 32]> {
        self.record
            .devices
            .iter()
            .find(|d| d.info.pairing == target)
            .map(storage::Device::clients)
            .unwrap_or_default()
    }
    pub(crate) fn stop(&mut self) {
        self.listener = None;
        self.retired.clear();
        self.listener_state = ListenerState::Off;
        self.listener_changed();
        self.connections.end_all();
        self.invitations.clear();
        self.claims.clear();
    }
    pub(crate) fn pending_releases(&mut self) -> Result<Vec<[u8; 32]>, DirectError> {
        self.recover()?;
        let devices: Vec<_> = self
            .record
            .devices
            .iter()
            .filter(|d| d.revoked && !self.released.contains(&d.info.client))
            .collect();
        self.released.extend(devices.iter().map(|d| d.info.client));
        Ok(devices
            .into_iter()
            .flat_map(storage::Device::clients)
            .collect())
    }
}

/// Serves one QUIC connection: the pinned handshake, one pairing exchange on its L0 stream, and
/// for a paired device admission and then the protocol connection over a
/// [`crate::connection::quic::QuicCarrier`]. Every action's device key must be the key of the
/// certificate the client proved in the handshake.
#[allow(clippy::too_many_arguments, reason = "one connection's whole context")]
pub(crate) fn serve(
    mut stream: QuicStream,
    generation: [u8; 32],
    state: Shared,
    environment: &crate::host::Environment,
    waker: crate::connection::io::Waker,
    wake: Arc<dyn Wake>,
    stopping: &LaneStop,
) -> Result<(), ServerError> {
    use super::client::Messages;
    // A connection the listener accepted before its policy changed never joins the new one.
    if generation != state.lock().unwrap_or_else(|e| e.into_inner()).generation {
        return Err(DirectError::Disabled.into());
    }
    stream
        .handshake(std::time::Instant::now() + super::quic::HANDSHAKE_LIMIT)
        .map_err(super::quic::transport)?;
    let device = stream
        .peer_certificates()
        .and_then(|certificates| super::quic::certificate_key(certificates.first()?))
        .ok_or(ServerError::AuthenticationRefused)?;
    let challenge = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        if generation != state.generation {
            return Err(DirectError::Disabled.into());
        }
        state.challenge()?
    };
    stream.send(&challenge)?;
    let authentication: wire::Authentication = stream.receive()?;
    let paired = match &authentication.action {
        wire::Action::Connect { paired } => Some(paired.clone()),
        _ => None,
    };
    let answer = {
        // Revocation ends registered connections inside the direct-network lock, before the
        // service learns of it. The locks are taken in the same order as everywhere else.
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let key = match &authentication.action {
            wire::Action::Connect { paired } => state.device_key(paired),
            wire::Action::Revoke { actor, .. } => state.device_key(actor),
            wire::Action::Claim { key, .. }
            | wire::Action::Finish { key, .. }
            | wire::Action::Cancel { key, .. } => Some(*key),
        };
        // A proof from another device's certificate is refused like a bad proof.
        let (answer, retired) = if key == Some(device) {
            state.authenticate_and_resolve(&challenge, &authentication)
        } else {
            (Err(DirectError::AuthenticationRefused), Vec::new())
        };
        drop(state);
        if !retired.is_empty() {
            let mut panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
            for client in retired {
                panes.revoked(client);
            }
        }
        answer
    };
    let connected = matches!(answer, Ok(wire::Answer::Connected));
    stream.send(&answer)?;
    if !connected {
        // The answer must reach the client before the connection closes.
        let _ = stream.finish(std::time::Instant::now() + super::quic::HANDSHAKE_LIMIT);
        return Ok(());
    }
    let paired = paired.ok_or(ServerError::AuthenticationRefused)?;
    let crate::protocol::Admitted {
        client,
        agent,
        name,
        ..
    } = match crate::protocol::admit_direct_server(&mut stream, &paired)? {
        crate::protocol::Admission::Work(admitted) => admitted,
        crate::protocol::Admission::Service(actor) => {
            return serve_lane(
                stream,
                &paired,
                generation,
                &state,
                environment,
                actor,
                stopping,
            );
        }
    };
    let connection = crate::protocol::random()?;
    let outbox = crate::connection::outbox::Outbox::new(Arc::clone(&wake));
    let carrier = stream.into_carrier();
    // A new name past the limit may evict only a name that holds nothing. The idle names stay
    // held while the pane service reports which hold something, and the two locks are never
    // nested.
    let (evicted, registered, reserved) = loop {
        let reserved = match &agent {
            Some(agent) => state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reserve_evictable(&paired, agent)?,
            None => None,
        }
        .map(|clients| Retirement {
            state: Arc::clone(&state),
            clients,
        });
        let settled = match &reserved {
            Some(reserved) if !reserved.clients.is_empty() => {
                let panes = environment.panes.lock().unwrap_or_else(|e| e.into_inner());
                reserved
                    .clients
                    .iter()
                    .filter(|client| !panes.holds(**client))
                    .copied()
                    .collect()
            }
            _ => Default::default(),
        };
        let mut locked = after_retirement(&state, client);
        // Another admission filled the list or evicted this name after the reservation, so the
        // eviction it now needs was never reserved.
        if let Some(agent) = &agent
            && reserved.is_none()
            && locked.evicts(&paired, agent)?
        {
            continue;
        }
        let evicted = match &agent {
            Some(agent) => locked.admit_agent(&paired, agent, &settled)?,
            None => None,
        };
        // Unregisters when serving ends, also when the release below panics.
        let registered = locked
            .register(
                &paired,
                generation,
                connection,
                client,
                Some(outbox.clone()),
                Box::new(carrier.sever()),
            )
            .map(|()| Registered {
                state: Arc::clone(&state),
                connection,
            });
        break (evicted, registered, reserved);
    };
    drop(reserved);
    // The direct-network lock is released first, as for a revocation. The evicted identity
    // holds no outcome or run receipt, and loses its control grants.
    if let Some(evicted) = evicted {
        let _retirement = Retirement {
            state: Arc::clone(&state),
            clients: vec![evicted],
        };
        environment
            .panes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_control(evicted);
    }
    let _registered = registered?;
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

/// Serves a paired service-lane connection. It is registered for its whole life, so revoking its
/// pairing or changing Network Access severs it, and each request first checks that the pairing
/// still holds its authority. Stopping the host severs it through `stopping`.
fn serve_lane(
    mut stream: QuicStream,
    paired: &PairedDevice,
    generation: [u8; 32],
    state: &Shared,
    environment: &crate::host::Environment,
    actor: crate::service::operations::Actor,
    stopping: &LaneStop,
) -> Result<(), ServerError> {
    let lane = environment.lane.as_ref().ok_or(ServerError::Unsupported)?;
    let connection = crate::protocol::random()?;
    state.lock().unwrap_or_else(|e| e.into_inner()).register(
        paired,
        generation,
        connection,
        actor.client,
        None,
        Box::new(stream.sever()),
    )?;
    let _registered = Registered {
        state: Arc::clone(state),
        connection,
    };
    stopping.watch(Box::new(stream.sever()));
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .note_connection(paired)?;
    lane.serve(&mut stream, actor, &|| {
        state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current(paired, generation)
            .map_err(ServerError::from)
    })
}

/// How the host stops a network connection's service lane. A work connection stops on its wake
/// instead, so this severs only a lane that [`Self::watch`] registered.
#[derive(Clone, Default)]
pub(crate) struct LaneStop(Arc<Mutex<LaneStopState>>);

#[derive(Default)]
struct LaneStopState {
    stopped: bool,
    sever: Option<Box<dyn Sever>>,
}

impl LaneStop {
    /// Severs `sever` when the host stops, or at once if it already stopped.
    fn watch(&self, sever: Box<dyn Sever>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            sever.sever();
        }
        state.sever = Some(sever);
    }

    /// Severs the watched lane, and any lane watched later.
    pub(crate) fn stop(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        if let Some(sever) = &state.sever {
            sever.sever();
        }
    }
}

/// Locks `state` once no release of `client`'s control grants after an eviction is still
/// running. A returning agent's new grants therefore always follow that release.
fn after_retirement(state: &Shared, client: [u8; 32]) -> MutexGuard<'_, State> {
    let state = state.lock().unwrap_or_else(|e| e.into_inner());
    let retired = Arc::clone(&state.retired_agent);
    retired
        .wait_while(state, |state| state.retiring.contains_key(&client))
        .unwrap_or_else(|e| e.into_inner())
}

/// Agent identities held in `retiring`: the idle names an admission may evict, or an evicted
/// identity whose control grants are being released. Dropping it, also when that work panics,
/// lets those agents be admitted again.
struct Retirement {
    state: Shared,
    clients: Vec<[u8; 32]>,
}

impl Drop for Retirement {
    fn drop(&mut self) {
        if !self.clients.is_empty() {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .release_retiring(&self.clients);
        }
    }
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
mod lane_tests;

#[cfg(test)]
mod agent_tests;

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
        let outbox = crate::connection::outbox::Outbox::new(Arc::new(|| {}));
        let registered = state.register(
            &paired,
            generation,
            [5; 32],
            paired.client,
            Some(outbox),
            Box::new(super::super::tests::Quiet),
        );
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
