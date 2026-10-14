use super::*;
use crate::{
    account_service::StoppedOwnerAdmission, native_deployment::contracts::NativeRequestId,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoppedEpoch {
    request: NativeRequestId,
    previous: ServerEpoch,
    next: ServerEpoch,
    pub(super) pending: bool,
}
impl StoppedEpoch {
    pub(super) fn validate(&self) -> bool {
        self.previous.checked_successor() == Some(self.next)
    }
}

/// Request-bound custody for an exclusive actor that cannot create listeners or
/// Terminal Sessions. Pending epoch publication survives disposal of that actor.
pub struct StoppedRuntimeCustody {
    admission: Arc<StoppedOwnerAdmission>,
    signer: Arc<dyn SigningKey>,
    restored: bool,
}
impl StoppedRuntimeCustody {
    /// Normal startup may finish an interrupted runtime-free owner epoch only
    /// under its new exclusive native instance admission, never from a dead PID.
    pub fn resume_start(
        admission: &crate::account_service::AccountStartup,
        epoch: ServerEpoch,
        sessions: &[SessionKey],
        signer: &dyn SigningKey,
    ) -> Result<(), ServiceFailure> {
        admission.revalidate()?;
        let registration = admission.registration();
        let (mut state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        let Some(owner) = &state.stopped_owner else {
            return Ok(());
        };
        if !owner.pending {
            return Ok(());
        }
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        if (epoch != owner.previous && epoch != owner.next)
            || (epoch == owner.next && !sessions.is_empty())
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        if epoch == owner.next {
            state.epoch = Some(epoch);
            state.cleanup = None;
            state.graceful = false;
        }
        state.clean = true;
        state
            .stopped_owner
            .as_mut()
            .ok_or(ServiceFailure::InvalidRecord)?
            .pending = false;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(ServiceFailure::LimitExceeded)?;
        publish(registration, &state, signer)
    }

    /// Only the retained stopped-owner admission can construct this lifecycle.
    pub fn new(
        admission: Arc<StoppedOwnerAdmission>,
        signer: Arc<dyn SigningKey>,
    ) -> Result<Self, ServiceFailure> {
        admission.revalidate()?;
        if signer.public_key() != admission.registration().server_pin().public_key {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(Self {
            admission,
            signer,
            restored: false,
        })
    }
    /// Reconciles only an epoch covered by this original stopped request. A
    /// successor with old Session custody is not a runtime-free recovery.
    pub fn recover(
        &mut self,
        epoch: ServerEpoch,
        sessions: &[SessionKey],
    ) -> Result<(), ServiceFailure> {
        if self.restored {
            return Ok(());
        }
        self.admission.revalidate()?;
        let registration = self.admission.registration();
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        let (mut state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if let Some(owner) = &state.stopped_owner
            && owner.pending
        {
            if owner.request != self.admission.request()
                || (epoch != owner.previous && epoch != owner.next)
                || (epoch == owner.next && !sessions.is_empty())
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            if epoch == owner.next {
                state.epoch = Some(epoch);
                state.cleanup = None;
            state.graceful = false;
            }
            state.clean = true;
            state
                .stopped_owner
                .as_mut()
                .ok_or(ServiceFailure::InvalidRecord)?
                .pending = false;
            publish(registration, &state, self.signer.as_ref())?;
        }
        self.restored = true;
        Ok(())
    }
    /// Records the original epoch transition before the physical SQLite commit.
    pub fn prepare_epoch(
        &self,
        previous: ServerEpoch,
        next: ServerEpoch,
    ) -> Result<(), ServiceFailure> {
        self.admission.revalidate()?;
        let registration = self.admission.registration();
        let (mut state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        if previous.checked_successor() != Some(next)
            || !state.clean
            || state.epoch.unwrap_or(ServerEpoch::new(0)) != previous
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        state.stopped_owner = Some(StoppedEpoch {
            request: self.admission.request(),
            previous,
            next,
            pending: true,
        });
        publish(registration, &state, self.signer.as_ref())
    }
    /// Tracks a durable epoch without claiming joined completion. Interruption
    /// between SQLite commit and this publication is covered by prepare_epoch.
    pub fn committed_epoch(&self, epoch: ServerEpoch) -> Result<(), ServiceFailure> {
        self.admission.verify_instance_custody()?;
        let registration = self.admission.registration();
        let (mut state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        let owner = state
            .stopped_owner
            .as_ref()
            .ok_or(ServiceFailure::CleanupUnresolved)?;
        if owner.request != self.admission.request() || owner.next != epoch || !owner.pending {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        state.epoch = Some(epoch);
        state.cleanup = None;
            state.graceful = false;
        state.clean = false;
        publish(registration, &state, self.signer.as_ref())
    }
    /// Publishes completion after the server actor and store worker have joined.
    /// This grants neither Stop nor Start and leaves deliberate Stopped intact.
    pub fn finish(&self, epoch: ServerEpoch) -> Result<(), ServiceFailure> {
        self.admission.verify_instance_custody()?;
        let registration = self.admission.registration();
        registration
            .endpoint()?
            .prove_no_listener()
            .map_err(|_| ServiceFailure::CleanupUnresolved)?;
        let (mut state, _) = read(registration)?.ok_or(ServiceFailure::CleanupUnresolved)?;
        let owner = state
            .stopped_owner
            .as_mut()
            .ok_or(ServiceFailure::CleanupUnresolved)?;
        if owner.request != self.admission.request()
            || owner.next != epoch
            || state.epoch != Some(epoch)
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        owner.pending = false;
        state.clean = true;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(ServiceFailure::LimitExceeded)?;
        publish(registration, &state, self.signer.as_ref())
    }
}
