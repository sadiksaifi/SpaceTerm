//! Transfers temporary setup authority into a saved paired connection.
use super::*;
use crate::cancellation::CancellationToken;
use std::sync::atomic::{AtomicBool, Ordering};

impl SetupEnrollment {
    pub(crate) fn operation(&self) -> JoinId {
        self.operation
    }

    pub(crate) fn public_key(&self) -> [u8; 32] {
        self.public_key
    }
}

impl Drop for SetupEnrollment {
    fn drop(&mut self) {
        if !self.settled.load(Ordering::Acquire) {
            let _ = self.owner.cancel_setup(self);
        }
    }
}

impl DirectConnections {
    /// Runs on the setup worker before admitting the SSH enrollment operation.
    pub(crate) fn prepare_setup(
        &self,
        endpoint: String,
        pin: direct::ServerPin,
        generation: u64,
    ) -> Result<SetupEnrollment, DirectError> {
        if pin.build != self.0.identity {
            return Err(DirectError::WrongServer);
        }
        let mut operation = random_id()?;
        operation[..8].copy_from_slice(&generation.to_be_bytes());
        let key = random_id()?;
        let credential = DeviceCredential::generate()?;
        // Resume validates the pinned endpoint without opening a connection.
        direct::Enrollment::resume(&endpoint, pin.clone(), operation, &credential)?;
        let public_key = credential.public_key()?;
        {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.inventory_loaded {
                return Err(DirectError::StorageUnavailable);
            }
            state.document.ensure_capacity()?;
        }
        // Native credential access may wait for account authorization. Snapshots
        // and unrelated cleanup must remain available throughout that wait.
        self.0
            .credentials
            .insert(&key, credential.expose_bytes())
            .map_err(worker::credential_error)?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = state.document.ensure_capacity() {
            drop(state);
            // This key was never offered to a server or transferred to recovery.
            let _ = self.0.credentials.remove(&key, Interaction::Quiet);
            return Err(error);
        }
        state.document.pending.push(PendingRecord::enrollment(
            operation,
            "Server setup".into(),
            endpoint.clone(),
            pin.clone(),
            operation,
            key,
            false,
        ));
        state
            .pending
            .insert(operation, Retry::new(Interaction::Quiet));
        let prepared = SetupEnrollment {
            owner: self.clone(),
            operation,
            public_key,
            endpoint,
            pin,
            settled: AtomicBool::new(false),
        };
        if !self.0.save(&mut state) {
            drop(state);
            drop(prepared);
            return Err(DirectError::StorageUnavailable);
        }
        Ok(prepared)
    }

    /// The caller closes every SSH resource before entering this QUIC admission step.
    pub(crate) fn complete_setup(
        &self,
        prepared: &SetupEnrollment,
        paired: PairedDevice,
        label: String,
        cancellation: &CancellationToken,
    ) -> Result<DirectRouteId, DirectError> {
        if !Arc::ptr_eq(&self.0, &prepared.owner.0) || paired.pin != prepared.pin {
            return Err(DirectError::WrongServer);
        }
        if cancellation.is_cancelled() {
            self.cancel_setup(prepared)?;
            return Err(DirectError::Rejected);
        }
        let saved = {
            let state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let pending = state
                .document
                .pending
                .iter()
                .find(|pending| {
                    pending.operation == prepared.operation && !pending.cancel_requested
                })
                .ok_or(DirectError::Rejected)?;
            SavedRoute {
                route: DirectRoute {
                    id: prepared.operation,
                    label,
                    endpoint: prepared.endpoint.clone(),
                    paired,
                },
                credential: pending
                    .credential_key()
                    .ok_or(DirectError::InvalidRequest)?,
                active: true,
            }
        };
        let gate = Arc::new(AtomicBool::new(true));
        let server = worker::connect(&self.0, &saved, Interaction::Requested, Arc::clone(&gate))?;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let cancelled = cancellation.is_cancelled()
            || state
                .document
                .pending
                .iter()
                .find(|pending| pending.operation == prepared.operation)
                .is_none_or(|pending| pending.cancel_requested);
        if cancelled {
            gate.store(false, Ordering::Release);
            drop(state);
            self.cancel_setup(prepared)?;
            return Err(DirectError::Rejected);
        }
        let original = state.document.clone();
        state
            .document
            .pending
            .retain(|pending| pending.operation != prepared.operation);
        state.document.routes.push(saved);
        let durability = match self.0.publish(&mut state) {
            Ok(durability) => durability,
            Err(error) => {
                gate.store(false, Ordering::Release);
                state.document = original;
                if error == SaveError::PublicationIndeterminate {
                    // The saved route may exist. Reload must establish whether the
                    // credential belongs to ordinary use or exact claim cleanup.
                    prepared.settled.store(true, Ordering::Release);
                    Self::record_setup_status(
                        &mut state,
                        prepared.operation,
                        JoinStatus::PublicationUnconfirmed,
                    );
                }
                return Err(DirectError::StorageUnavailable);
            }
        };
        Self::record_setup_status(
            &mut state,
            prepared.operation,
            match durability {
                Durability::Synchronized => JoinStatus::Approved(prepared.operation),
                Durability::Uncertain => JoinStatus::ApprovedUncertain(prepared.operation),
            },
        );
        state.pending.remove(&prepared.operation);
        state.routes.insert(
            prepared.operation,
            RouteState {
                status: DirectRouteStatus::Connected,
                server: Some(server),
                gate,
                retry: Retry {
                    due: Instant::now() + std::time::Duration::from_secs(5),
                    ..Retry::new(Interaction::Quiet)
                },
                health: None,
            },
        );
        prepared.settled.store(true, Ordering::Release);
        drop(state);
        self.start_worker();
        self.0.notify();
        Ok(prepared.operation)
    }

    fn record_setup_status(state: &mut State, id: JoinId, status: JoinStatus) {
        if state.joins.len() >= 16 {
            let finished = state
                .joins
                .iter()
                .find(|(_, join)| {
                    matches!(
                        join.status,
                        JoinStatus::Approved(_)
                            | JoinStatus::ApprovedUncertain(_)
                            | JoinStatus::PublicationUnconfirmed
                            | JoinStatus::Rejected
                            | JoinStatus::Expired
                            | JoinStatus::Failed(_)
                    )
                })
                .map(|(id, _)| *id);
            if let Some(finished) = finished {
                state.joins.remove(&finished);
            }
        }
        state.joins.insert(
            id,
            JoinState {
                status,
                cancelled: false,
                retry_requested: false,
                deadline: Instant::now(),
            },
        );
    }

    /// Accepts the owning setup's validated account-local receipt for this exact claim.
    /// The caller verifies the receipt's server pin and operation before calling.
    pub(crate) fn confirm_setup_cancelled(
        &self,
        prepared: &SetupEnrollment,
    ) -> Result<(), DirectError> {
        if !Arc::ptr_eq(&self.0, &prepared.owner.0) {
            return Err(DirectError::WrongServer);
        }
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state
            .document
            .pending
            .iter_mut()
            .find(|pending| pending.operation == prepared.operation)
            .ok_or(DirectError::InvalidRequest)?;
        if !matches!(&pending.authority, records::Authority::Enrollment { endpoint, pin, claim, .. }
            if endpoint == &prepared.endpoint && pin == &prepared.pin && *claim == prepared.operation)
        {
            return Err(DirectError::WrongServer);
        }
        pending.confirmed = true;
        pending.cancel_requested = true;
        // Keep the confirmed receipt in memory even when synchronization fails.
        // Recovery retries retention before retiring the exact native credential.
        prepared.settled.store(true, Ordering::Release);
        let retained = self.0.save(&mut state);
        state
            .pending
            .insert(prepared.operation, Retry::new(Interaction::Quiet));
        drop(state);
        self.start_worker();
        self.0.notify();
        if retained {
            Ok(())
        } else {
            Err(DirectError::StorageUnavailable)
        }
    }

    pub(crate) fn cancel_setup(&self, prepared: &SetupEnrollment) -> Result<(), DirectError> {
        if !Arc::ptr_eq(&self.0, &prepared.owner.0) {
            return Err(DirectError::WrongServer);
        }
        self.cancel_join(prepared.operation)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Credentials, MemoryRecords, saved};
    use super::super::worker::DirectNetwork;
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Network {
        fail: bool,
        cancellation: CancellationToken,
        connections: AtomicUsize,
        cancellations: Mutex<Vec<[u8; 32]>>,
    }

    impl DirectNetwork for Network {
        fn connect(
            &self,
            identity: &str,
            route: &DirectRoute,
            _: DeviceCredential,
            _: Arc<AtomicBool>,
        ) -> Result<ServerConnection, DirectError> {
            self.connections.fetch_add(1, Ordering::AcqRel);
            if self.fail {
                return Err(DirectError::Transport);
            }
            struct Offline;
            impl direct::DirectConnector for Offline {
                fn connect(&self) -> Result<direct::DirectConnection, DirectError> {
                    Err(DirectError::Transport)
                }
            }
            self.cancellation.cancel();
            ServerConnection::direct(identity, route.id, route.paired.clone(), Arc::new(Offline))
                .map_err(DirectError::from)
        }

        fn revoke(
            &self,
            _: &str,
            _: &PairedDevice,
            _: &DeviceCredential,
            _: UnpairKey,
            _: PairingId,
        ) -> Result<direct::RevocationReceipt, DirectError> {
            panic!("unfinished setup uses its exact enrollment claim")
        }

        fn cancel_enrollment(
            &self,
            _: &str,
            _: &direct::ServerPin,
            claim: [u8; 32],
            _: &DeviceCredential,
        ) -> Result<Option<direct::RevocationReceipt>, DirectError> {
            self.cancellations.lock().unwrap().push(claim);
            Ok(None)
        }
    }

    fn fixture(fail: bool) -> (DirectConnections, Arc<MemoryRecords>, Arc<Network>) {
        let records = Arc::new(MemoryRecords::default());
        let network = Arc::new(Network {
            fail,
            cancellation: CancellationToken::default(),
            connections: AtomicUsize::new(0),
            cancellations: Mutex::default(),
        });
        let owner = DirectConnections::new(
            "spaceterm-development",
            records.clone(),
            Arc::new(Credentials::default()),
            network.clone(),
        );
        // Load through the public startup operation. No saved route opens a connection.
        owner.resume_retained();
        (owner, records, network)
    }

    #[derive(Default)]
    struct PublicationRecords {
        document: Mutex<Document>,
        mode: AtomicUsize,
        writes: AtomicUsize,
    }

    impl Records for PublicationRecords {
        fn load(&self) -> Result<Document, ()> {
            Ok(self.document.lock().unwrap().clone())
        }

        fn save(&self, document: &Document) -> Result<Durability, SaveError> {
            self.writes.fetch_add(1, Ordering::AcqRel);
            let mode = self.mode.load(Ordering::Acquire);
            if mode != 3 {
                *self.document.lock().unwrap() = document.clone();
            }
            match mode {
                0 => Ok(Durability::Synchronized),
                1 => Ok(Durability::Uncertain),
                _ => Err(SaveError::PublicationIndeterminate),
            }
        }
    }

    fn publication_fixture() -> (DirectConnections, Arc<PublicationRecords>, Arc<Network>) {
        let records = Arc::new(PublicationRecords::default());
        let network = Arc::new(Network {
            fail: false,
            cancellation: CancellationToken::default(),
            connections: AtomicUsize::new(0),
            cancellations: Mutex::default(),
        });
        let owner = DirectConnections::new(
            "spaceterm-development",
            records.clone(),
            Arc::new(Credentials::default()),
            network.clone(),
        );
        owner.resume_retained();
        (owner, records, network)
    }

    fn wait_for_cleanup(owner: &DirectConnections, operation: JoinId) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let retained = owner.0.records.load().unwrap();
            if !retained
                .pending
                .iter()
                .any(|pending| pending.operation == operation)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "confirmed setup cleanup did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_waiting_native_credential_insert_does_not_block_registry_snapshots() {
        struct WaitingCredentials {
            store: Credentials,
            entered: std::sync::mpsc::Sender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl CredentialStore for WaitingCredentials {
            fn read(
                &self,
                key: &[u8; 32],
                interaction: Interaction,
            ) -> Result<Option<Zeroizing<Vec<u8>>>, credentials::CredentialError> {
                self.store.read(key, interaction)
            }
            fn insert(
                &self,
                key: &[u8; 32],
                secret: &[u8],
            ) -> Result<(), credentials::CredentialError> {
                self.entered
                    .send(())
                    .map_err(|_| credentials::CredentialError::Unavailable)?;
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| credentials::CredentialError::Unavailable)?;
                self.store.insert(key, secret)
            }
            fn remove(
                &self,
                key: &[u8; 32],
                interaction: Interaction,
            ) -> Result<(), credentials::CredentialError> {
                self.store.remove(key, interaction)
            }
        }
        let (entered, inserting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let owner = DirectConnections::new(
            "spaceterm-development",
            Arc::new(MemoryRecords::default()),
            Arc::new(WaitingCredentials {
                store: Credentials::default(),
                entered,
                release: Mutex::new(released),
            }),
            Arc::new(Network {
                fail: true,
                cancellation: CancellationToken::default(),
                connections: AtomicUsize::new(0),
                cancellations: Mutex::default(),
            }),
        );
        owner.resume_retained();
        let setup = owner.clone();
        let preparation = std::thread::spawn(move || {
            setup.prepare_setup(
                "quic://127.0.0.1:9231".into(),
                saved(1, 2).route.paired.pin,
                0,
            )
        });
        inserting
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let reader = owner.clone();
        let (read, snapshot) = std::sync::mpsc::channel();
        let observation = std::thread::spawn(move || read.send(reader.routes()).unwrap());
        let observed_while_waiting = snapshot.recv_timeout(std::time::Duration::from_secs(3));
        // Release the boundary before asserting, including when the regression is present.
        release.send(()).unwrap();
        let prepared = preparation.join().unwrap().unwrap();
        observation.join().unwrap();
        assert_eq!(observed_while_waiting.unwrap(), Vec::new());
        drop(prepared);
    }

    #[test]
    fn setup_retains_the_observed_generation_in_its_exact_cleanup_claim_before_dispatch() {
        let (owner, records, network) = fixture(true);
        let prepared = owner
            .prepare_setup(
                "quic://127.0.0.1:9231".into(),
                saved(1, 2).route.paired.pin,
                0x0102_0304_0506_0708,
            )
            .unwrap();
        let retained = records.load().unwrap();
        let pending = &retained.pending[0];
        assert!(
            matches!(&pending.authority, records::Authority::Enrollment { claim, .. }
            if claim == &prepared.operation() && claim[..8] == [1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(pending.operation, prepared.operation());
        assert_eq!(network.connections.load(Ordering::Acquire), 0);
        assert!(network.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn account_confirmed_setup_cancellation_retires_only_its_credential_without_quic() {
        let (owner, records, network) = fixture(true);
        let pin = saved(1, 2).route.paired.pin;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), pin.clone(), 0)
            .unwrap();
        let other = owner
            .prepare_setup("quic://127.0.0.1:9232".into(), pin, 0)
            .unwrap();
        let original = records.load().unwrap();
        let key = original
            .pending
            .iter()
            .find(|pending| pending.operation == prepared.operation())
            .unwrap()
            .credential_key()
            .unwrap();
        let other_key = original
            .pending
            .iter()
            .find(|pending| pending.operation == other.operation())
            .unwrap()
            .credential_key()
            .unwrap();
        owner.confirm_setup_cancelled(&prepared).unwrap();
        let operation = prepared.operation();
        drop(prepared);
        wait_for_cleanup(&owner, operation);
        assert!(
            owner
                .0
                .credentials
                .read(&key, Interaction::Quiet)
                .unwrap()
                .is_none()
        );
        assert!(
            owner
                .0
                .credentials
                .read(&other_key, Interaction::Quiet)
                .unwrap()
                .is_some()
        );
        assert!(network.cancellations.lock().unwrap().is_empty());
        assert_eq!(network.connections.load(Ordering::Acquire), 0);
    }

    #[test]
    fn confirmed_setup_cleanup_waits_for_receipt_retention_before_retiring_the_credential() {
        let (owner, records, network) = publication_fixture();
        let pin = saved(1, 2).route.paired.pin;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), pin, 0)
            .unwrap();
        let operation = prepared.operation();
        let key = records.load().unwrap().pending[0].credential_key().unwrap();
        records.mode.store(3, Ordering::Release);
        assert_eq!(
            owner.confirm_setup_cancelled(&prepared),
            Err(DirectError::StorageUnavailable)
        );
        drop(prepared);
        assert!(
            owner
                .0
                .credentials
                .read(&key, Interaction::Quiet)
                .unwrap()
                .is_some()
        );
        assert!(!records.load().unwrap().pending[0].confirmed);
        records.mode.store(0, Ordering::Release);
        wait_for_cleanup(&owner, operation);
        assert!(
            owner
                .0
                .credentials
                .read(&key, Interaction::Quiet)
                .unwrap()
                .is_none()
        );
        assert!(network.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn a_different_registry_cannot_confirm_setup_cancellation() {
        let (owner, records, _) = fixture(true);
        let (other, _, _) = fixture(true);
        let prepared = owner
            .prepare_setup(
                "quic://127.0.0.1:9231".into(),
                saved(1, 2).route.paired.pin,
                0,
            )
            .unwrap();
        assert_eq!(
            other.confirm_setup_cancelled(&prepared),
            Err(DirectError::WrongServer)
        );
        let retained = records.load().unwrap();
        assert!(!retained.pending[0].confirmed);
        assert!(!retained.pending[0].cancel_requested);
    }

    #[test]
    fn unsynchronized_setup_publication_keeps_the_admitted_route_and_never_requests_cancellation() {
        let (owner, records, network) = publication_fixture();
        let paired = saved(1, 2).route.paired;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), paired.pin.clone(), 0)
            .unwrap();
        records.mode.store(1, Ordering::Release);
        let id = owner
            .complete_setup(
                &prepared,
                paired.clone(),
                "Remote server".into(),
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(
            matches!(owner.join_status(id), Some(JoinStatus::ApprovedUncertain(route)) if route == id)
        );
        let writes = records.writes.load(Ordering::Acquire);
        drop(prepared);
        assert_eq!(records.writes.load(Ordering::Acquire), writes);
        let retained = records.load().unwrap();
        assert!(retained.pending.is_empty());
        assert_eq!(retained.routes[0].route.paired, paired);
        assert!(network.cancellations.lock().unwrap().is_empty());
    }

    #[test]
    fn indeterminate_setup_publication_preserves_both_possible_disk_outcomes_without_guessing_cleanup()
     {
        for mode in [2, 3] {
            let (owner, records, network) = publication_fixture();
            let paired = saved(1, 2).route.paired;
            let prepared = owner
                .prepare_setup("quic://127.0.0.1:9231".into(), paired.pin.clone(), 0)
                .unwrap();
            let id = prepared.operation();
            records.mode.store(mode, Ordering::Release);
            assert_eq!(
                owner.complete_setup(
                    &prepared,
                    paired,
                    "Remote server".into(),
                    &CancellationToken::default(),
                ),
                Err(DirectError::StorageUnavailable)
            );
            assert!(matches!(
                owner.join_status(id),
                Some(JoinStatus::PublicationUnconfirmed)
            ));
            let writes = records.writes.load(Ordering::Acquire);
            drop(prepared);
            assert_eq!(records.writes.load(Ordering::Acquire), writes);
            let retained = records.load().unwrap();
            assert_eq!(retained.routes.len(), usize::from(mode == 2));
            assert_eq!(retained.pending.len(), usize::from(mode == 3));
            assert!(network.cancellations.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn successful_setup_retains_the_paired_client_identity_and_retires_claim_cleanup() {
        let (owner, records, _) = fixture(false);
        let paired = saved(1, 2).route.paired;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), paired.pin.clone(), 0)
            .unwrap();
        let id = owner
            .complete_setup(
                &prepared,
                paired.clone(),
                "Remote server".into(),
                &CancellationToken::default(),
            )
            .unwrap();
        drop(prepared);
        let retained = records.load().unwrap();
        assert!(retained.pending.is_empty());
        assert_eq!(retained.routes[0].route.paired, paired);
        assert_eq!(owner.status(&id), Some(DirectRouteStatus::Connected));
    }

    #[test]
    fn unreachable_server_never_publishes_a_setup_route() {
        let (owner, records, network) = fixture(true);
        let paired = saved(1, 2).route.paired;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), paired.pin.clone(), 0)
            .unwrap();
        let original = records.load().unwrap();
        assert!(matches!(&original.pending[0].authority,
            records::Authority::Enrollment { claim, .. } if *claim == prepared.operation()));
        assert_eq!(
            owner.complete_setup(
                &prepared,
                paired,
                "Remote server".into(),
                &CancellationToken::default(),
            ),
            Err(DirectError::Transport)
        );
        assert!(owner.routes().is_empty());
        assert!(records.load().unwrap().routes.is_empty());
        assert_eq!(network.connections.load(Ordering::Acquire), 1);
    }

    #[test]
    fn cancellation_during_admission_never_publishes_the_pairing() {
        let (owner, records, network) = fixture(false);
        let paired = saved(1, 2).route.paired;
        let prepared = owner
            .prepare_setup("quic://127.0.0.1:9231".into(), paired.pin.clone(), 0)
            .unwrap();
        assert_eq!(
            owner.complete_setup(
                &prepared,
                paired,
                "Remote server".into(),
                &network.cancellation,
            ),
            Err(DirectError::Rejected)
        );
        assert!(owner.routes().is_empty());
        assert!(records.load().unwrap().routes.is_empty());
    }
}
