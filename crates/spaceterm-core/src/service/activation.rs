//! Exact, account-authorized runtime replacement and recovery of its original operation.
//!
//! The helper from the previous runtime stops the server, snapshots the retained state, moves
//! the service registration to the Pending Version and starts it. A Pending Version that does
//! not become ready before the readiness deadline is stopped; the snapshot is restored and the
//! previous version runs again. The update record then names the failure.
use super::*;
use crate::update::facts::StartFailure;

/// The snapshot of the retained state an activation may restore, beside the files it copies.
const SNAPSHOT: [(&std::ffi::CStr, &std::ffi::CStr); 2] = [
    (c"identity.json", c"activation-identity.json"),
    (c"direct.json", c"activation-direct.json"),
];

/// The result a control surface can present without interpreting retained transition steps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationPhase {
    Activating,
    Completed {
        running: bool,
    },
    /// The Pending Version did not become ready. The retained state was restored and the
    /// previous version runs again unless the Core was stopped.
    Returned {
        failure: StartFailure,
        running: bool,
    },
    Uncertain,
}

/// The original exact-target operation, retained even after a caller loses its reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationStatus {
    pub operation: [u8; 32],
    pub previous: RetainedInstallation,
    pub candidate: RetainedInstallation,
    pub stopped: bool,
    pub phase: ActivationPhase,
}

/// One approval binds the selected service, artifact, boot and observed installation/run policy.
pub struct ActivationApproval {
    service: Service,
    state: State,
    boot: String,
    core: [u8; 32],
    running: bool,
    previous: RetainedInstallation,
    candidate: RetainedInstallation,
}
impl ActivationApproval {
    /// Nonsecret exact-observation digest for confirmation across separate CLI invocations.
    pub fn confirmation_token(&self) -> Result<String, CoreError> {
        confirmation_token(&(
            &self.service.identity,
            &self.state,
            &self.boot,
            self.core,
            self.running,
            Build::from_installation(&self.previous)?,
            Build::from_installation(&self.candidate)?,
        ))
    }
    pub fn candidate(&self) -> &RetainedInstallation {
        &self.candidate
    }
    pub fn previous(&self) -> &RetainedInstallation {
        &self.previous
    }
    pub fn running(&self) -> bool {
        self.running
    }
    pub fn stopped(&self) -> bool {
        self.state.stopped
    }
    pub fn identity(&self) -> &str {
        &self.service.identity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    Admitted,
    StopDispatched,
    Quiescent,
    RegistrationReplaced,
    Approved,
    StartDispatched,
    Completed,
    /// The candidate's start failed or missed the readiness deadline.
    StartFailed,
    ReturnStopDispatched,
    /// The retained state is restored and the registration names the previous version again.
    ReturnApproved,
    ReturnStartDispatched,
    Returned,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    digest: String,
    version: String,
    commit: String,
}
impl Build {
    fn from_installation(value: &RetainedInstallation) -> Result<Self, CoreError> {
        let digest = value
            .directory
            .file_name()
            .and_then(|v| v.to_str())
            .filter(|v| runtime::digest_valid(v))
            .ok_or(CoreError::RuntimeUnavailable)?;
        Ok(Self {
            digest: digest.into(),
            version: value.version.clone(),
            commit: value.commit.clone(),
        })
    }
    fn installation(&self, service: &Service) -> RetainedInstallation {
        RetainedInstallation {
            directory: service.paths.data.join("installations").join(&self.digest),
            version: self.version.clone(),
            commit: self.commit.clone(),
        }
    }
    fn valid(&self) -> bool {
        let version = self.version.strip_prefix("dev.").is_some_and(|suffix| {
            // Signed runtime admission owns identity policy; this record checks syntax.
            let revision = suffix.strip_suffix(".dirty").unwrap_or(suffix);
            revision.len() == 12 && self.commit.get(..12) == Some(revision)
        }) || (self.version.split('.').count() == 3
            && self.version.split('.').all(|part| {
                !part.is_empty()
                    && (part == "0" || !part.starts_with('0'))
                    && part.bytes().all(|v| v.is_ascii_digit())
            }));
        runtime::digest_valid(&self.digest)
            && self.version.len() <= 64
            && version
            && self.commit.len() == 40
            && self
                .commit
                .bytes()
                .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
    }
}

/// Safety intent shares the strict service record so older strict readers cannot erase it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    schema: u32,
    id: [u8; 32],
    previous: Build,
    candidate: Build,
    stopped: bool,
    original_run: Option<Run>,
    candidate_run: Option<Run>,
    step: Step,
    /// The digest of the retained-state snapshot taken once the server was quiescent.
    saved: Option<[u8; 32]>,
    /// Unix seconds when the candidate's Start was dispatched, for the readiness deadline.
    started_at: Option<u64>,
    ready: bool,
    failed: bool,
    /// Why the candidate did not become ready, once the return began.
    failure: Option<StartFailure>,
}
impl Operation {
    pub(super) fn pending(&self) -> bool {
        !matches!(self.step, Step::Completed | Step::Returned)
    }
    pub(super) fn settled(&self) -> bool {
        matches!(self.step, Step::Returned) || (!self.pending() && !self.failed)
    }
    pub(super) fn permits_start(&self) -> bool {
        matches!(
            self.step,
            Step::StartDispatched | Step::Completed | Step::ReturnStartDispatched | Step::Returned
        )
    }
    /// A later Start of a stopped update's candidate failed before it was ready, so the Core
    /// returns to the previous version. That Start ended the Stop, which is now `stopped`.
    /// Returns whether the return begins.
    pub(super) fn startup_failed(&mut self, stopped: bool) -> bool {
        let returning = self.step == Step::Completed && !self.ready;
        if returning {
            self.step = Step::StartFailed;
            self.stopped = stopped;
        }
        returning
    }
    pub(super) fn validate(&self, state: &State) -> Result<(), CoreError> {
        let expected = if matches!(
            self.step,
            Step::Admitted
                | Step::StopDispatched
                | Step::Quiescent
                | Step::RegistrationReplaced
                | Step::ReturnApproved
                | Step::ReturnStartDispatched
                | Step::Returned
        ) {
            &self.previous.digest
        } else {
            &self.candidate.digest
        };
        if self.schema != 1
            || self.id == [0; 32]
            || !self.previous.valid()
            || !self.candidate.valid()
            || self.previous.digest == self.candidate.digest
            || &state.installation != expected
            || (self.ready && !matches!(self.step, Step::Completed))
            || (self.pending()
                && (!state.installed || state.removing || state.stopped != self.stopped))
            || (!matches!(self.step, Step::Admitted | Step::StopDispatched) && self.saved.is_none())
            || (matches!(
                self.step,
                Step::ReturnApproved | Step::ReturnStartDispatched | Step::Returned
            ) && self.failure.is_none())
            || (matches!(
                self.step,
                Step::Quiescent | Step::RegistrationReplaced | Step::Approved
            ) && (state.run.is_some() || self.candidate_run.is_some()))
            || (self.step == Step::ReturnApproved && state.run.is_some())
            || (self.step == Step::StartDispatched
                && state
                    .run
                    .as_ref()
                    .is_some_and(|run| Some(run) != self.candidate_run.as_ref()))
        {
            return Err(CoreError::UnsafeStorage);
        }
        Ok(())
    }
}

impl Service {
    /// Read-only approval preparation. Staging alone grants no runtime authority.
    pub fn prepare_activation(
        &self,
        candidate: &RetainedInstallation,
    ) -> Result<Option<ActivationApproval>, CoreError> {
        let (data, _) = self.read()?.ok_or(CoreError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let _intent = data.lock("service-intent.lock", false)?;
        data.verify_path(&self.paths.data)?;
        let state = Self::state(&data)?.ok_or(CoreError::NotInstalled)?;
        if state.activation.as_ref().is_some_and(Operation::pending) {
            return Err(CoreError::ActivationUnresolved);
        }
        if !state.installed {
            return Err(CoreError::NotInstalled);
        }
        let status = self.snapshot(&state)?;
        let installations = owned_directory(&self.paths.data.join("installations"))?;
        let _installations = lock_directory(&installations)?;
        self.verify_choice(candidate)?;
        runtime::verify(&candidate.directory, &self.identity)?;
        let previous = self.describe_approved(&state)?;
        if previous.directory == candidate.directory {
            return Ok(None);
        }
        if is_released_downgrade(&candidate.version, &previous.version) {
            return Err(CoreError::RuntimeUnavailable);
        }
        let core = data.read()?.core;
        Ok(Some(ActivationApproval {
            service: self.clone(),
            state,
            boot: boot()?,
            core,
            running: status.running,
            previous,
            candidate: candidate.clone(),
        }))
    }

    /// Admit this approval once, then resolve only its durably retained original operation.
    pub fn activate(&self, approval: ActivationApproval) -> Result<ActivationStatus, CoreError> {
        self.same_service(&approval.service)?;
        let (data, _) = self.read()?.ok_or(CoreError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        {
            let _intent = data.lock("service-intent.lock", false)?;
            data.verify_path(&self.paths.data)?;
            let mut state = Self::state(&data)?.ok_or(CoreError::NotInstalled)?;
            if state != approval.state
                || boot()? != approval.boot
                || data.read()?.core != approval.core
                || self.snapshot(&state)?.running != approval.running
            {
                return Err(CoreError::Changed);
            }
            let installations = owned_directory(&self.paths.data.join("installations"))?;
            let _installations = lock_directory(&installations)?;
            self.verify_choice(&approval.previous)?;
            self.verify_choice(&approval.candidate)?;
            runtime::verify(&approval.candidate.directory, &self.identity)?;
            state.activation = Some(Operation {
                schema: 1,
                id: crate::protocol::random()?,
                previous: Build::from_installation(&approval.previous)?,
                candidate: Build::from_installation(&approval.candidate)?,
                stopped: state.stopped,
                original_run: state.run.clone(),
                candidate_run: None,
                step: Step::Admitted,
                saved: None,
                started_at: None,
                ready: false,
                failed: false,
                failure: None,
            });
            data.write_json(c"service.json", &state)?;
        }
        self.resume_activation(&data)?;
        self.activation_status_locked(&data)?
            .ok_or(CoreError::ActivationUnresolved)
    }

    /// Observe the original operation without dispatching or granting any authority.
    pub fn activation_status(&self) -> Result<Option<ActivationStatus>, CoreError> {
        let Some((data, _)) = self.read()? else {
            return Ok(None);
        };
        let _operation = data.lock("service.lock", false)?;
        self.activation_status_locked(&data)
    }

    /// Finish an admitted exact target. Never resend destructive Stop or candidate Start dispatch.
    pub fn recover_activation(&self) -> Result<Option<ActivationStatus>, CoreError> {
        let Some((data, state)) = self.read()? else {
            return Ok(None);
        };
        if state.activation.is_none() {
            return Ok(None);
        }
        let _operation = data.lock("service.lock", false)?;
        self.resume_activation(&data)?;
        self.activation_status_locked(&data)
    }

    fn same_service(&self, other: &Self) -> Result<(), CoreError> {
        if self.identity != other.identity
            || self.paths.data != other.paths.data
            || self.paths.runtime != other.paths.runtime
        {
            Err(CoreError::AuthenticationRefused)
        } else {
            Ok(())
        }
    }
    fn verify_choice(&self, choice: &RetainedInstallation) -> Result<(), CoreError> {
        let build = Build::from_installation(choice)?;
        if build.installation(self) != *choice
            || runtime::describe(&choice.directory, &self.identity)?
                != (choice.version.clone(), choice.commit.clone())
        {
            return Err(CoreError::RuntimeUnavailable);
        }
        Ok(())
    }
    pub(super) fn describe_approved(
        &self,
        state: &State,
    ) -> Result<RetainedInstallation, CoreError> {
        let directory = self.runtime(state);
        let (version, commit) = runtime::describe(&directory, &self.identity)?;
        Ok(RetainedInstallation {
            directory,
            version,
            commit,
        })
    }
    pub(super) fn activation_status_locked(
        &self,
        data: &Directory,
    ) -> Result<Option<ActivationStatus>, CoreError> {
        let state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
        let Some(operation) = &state.activation else {
            return Ok(None);
        };
        let running = || self.snapshot(&state).map(|s| s.running);
        let phase = match (operation.step, operation.failure) {
            (Step::Completed, _) => running()
                .map(|running| ActivationPhase::Completed { running })
                .unwrap_or(ActivationPhase::Uncertain),
            (Step::Returned, Some(failure)) => running()
                .map(|running| ActivationPhase::Returned { failure, running })
                .unwrap_or(ActivationPhase::Uncertain),
            _ if operation.failed => ActivationPhase::Uncertain,
            _ => ActivationPhase::Activating,
        };
        Ok(Some(ActivationStatus {
            operation: operation.id,
            previous: operation.previous.installation(self),
            candidate: operation.candidate.installation(self),
            stopped: state.stopped,
            phase,
        }))
    }

    /// Called only under service.lock. A failed write may already be durable, so each next
    /// entrypoint rereads the record instead of continuing an in-memory transition.
    pub(super) fn resume_activation(&self, data: &Directory) -> Result<(), CoreError> {
        let result = self.resume_steps(data);
        if result.is_err() {
            let _intent = data.lock("service-intent.lock", false)?;
            let Some(mut state) = Self::state(data)? else {
                return Ok(());
            };
            if let Some(operation) = state.activation.as_mut()
                && operation.pending()
            {
                operation.failed = true;
                data.write_json(c"service.json", &state)?;
            }
        }
        result
    }

    fn resume_steps(&self, data: &Directory) -> Result<(), CoreError> {
        loop {
            let Some(mut state) = Self::state(data)? else {
                return Ok(());
            };
            let Some(operation) = state.activation.clone() else {
                return Ok(());
            };
            data.verify_path(&self.paths.data)?;
            let previous = operation.previous.installation(self);
            let candidate = operation.candidate.installation(self);
            match operation.step {
                Step::Admitted => {
                    self.verify_choice(&previous)?;
                    self.verify_choice(&candidate)?;
                    self.supervisor.check(&self.runtime(&state), false)?;
                    // Durable dispatch marker precedes the native call. Interruption in this
                    // gap requires intervention if the old process remains live; never resend.
                    self.set_step(data, operation.id, Step::StopDispatched)?;
                    if !state.stopped {
                        self.supervisor.stop()?;
                        self.wait_running(false)?;
                    }
                }
                Step::StopDispatched => {
                    self.supervisor.check(&self.runtime(&state), false)?;
                    self.wait_quiescent(data, &operation, operation.original_run.as_ref())?;
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
                    let _writes = data
                        .lock("worktree.lock", true)
                        .map_err(|_| CoreError::CleanupUnresolved)?;
                    let _setup = data.lock("setup.lock", false)?;
                    let saved = take_snapshot(data)?;
                    let current = state
                        .activation
                        .as_mut()
                        .ok_or(CoreError::ActivationUnresolved)?;
                    current.saved = Some(saved);
                    current.step = Step::Quiescent;
                    state.run = None;
                    data.write_json(c"service.json", &state)?;
                }
                Step::Quiescent => {
                    self.require_quiescent(&state)?;
                    self.verify_choice(&previous)?;
                    self.verify_choice(&candidate)?;
                    self.supervisor
                        .replace(&previous.directory, &candidate.directory)?;
                    self.set_step(data, operation.id, Step::RegistrationReplaced)?;
                }
                Step::RegistrationReplaced => {
                    self.require_quiescent(&state)?;
                    self.supervisor.check(&candidate.directory, false)?;
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
                    state.installation = operation.candidate.digest.clone();
                    state
                        .activation
                        .as_mut()
                        .ok_or(CoreError::ActivationUnresolved)?
                        .step = Step::Approved;
                    data.write_json(c"service.json", &state)?;
                }
                Step::Approved => {
                    // A launcher failure never stops the candidate; clients read it as Outdated.
                    self.repoint_cli_locked(&self.runtime(&state));
                    self.require_quiescent(&state)?;
                    self.verify_choice(&candidate)?;
                    if state.stopped {
                        self.set_step(data, operation.id, Step::Completed)?;
                        continue;
                    }
                    self.supervisor.check(&self.runtime(&state), false)?;
                    self.supervisor.prepare_start()?;
                    self.change(data, operation.id, |operation| {
                        operation.step = Step::StartDispatched;
                        operation.started_at = Some(unix_now());
                    })?;
                    let started = self
                        .supervisor
                        .start()
                        .and_then(|()| self.supervisor.set_automatic_start(state.automatic_start));
                    if started.is_err() {
                        self.set_step(data, operation.id, Step::StartFailed)?;
                    }
                }
                Step::StartDispatched => {
                    // The candidate marks readiness durably before it publishes its endpoint.
                    // Neither a timeout nor an absent process permits another native Start.
                    if !self.await_step(data, &operation, Step::StartDispatched)? {
                        self.set_step(data, operation.id, Step::StartFailed)?;
                    }
                }
                Step::Completed if operation.ready || state.stopped => {
                    self.repoint_cli_locked(&self.runtime(&state));
                    if operation.ready {
                        runtime::updates::record_completed(data, &operation.candidate.digest)?;
                        remove_snapshot(data)?;
                    }
                    return Ok(());
                }
                Step::Completed => {
                    // Ready is recorded by the candidate's own run. A running candidate that
                    // is not ready yet stays Activating until its deadline passes.
                    return Ok(());
                }
                Step::StartFailed => {
                    // Durable dispatch marker precedes the Stop of the candidate.
                    self.set_step(data, operation.id, Step::ReturnStopDispatched)?;
                    if self.supervisor.running()? {
                        self.supervisor.stop()?;
                        self.wait_running(false)?;
                    }
                }
                Step::ReturnStopDispatched => {
                    self.wait_quiescent(data, &operation, operation.candidate_run.as_ref())?;
                    let failure = self.classify();
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
                    let _writes = data
                        .lock("worktree.lock", true)
                        .map_err(|_| CoreError::CleanupUnresolved)?;
                    let _setup = data.lock("setup.lock", false)?;
                    restore_snapshot(data, operation.saved.ok_or(CoreError::UnsafeStorage)?)?;
                    self.verify_choice(&previous)?;
                    self.supervisor
                        .replace(&candidate.directory, &previous.directory)?;
                    state.installation = operation.previous.digest.clone();
                    state.run = None;
                    let current = state
                        .activation
                        .as_mut()
                        .ok_or(CoreError::ActivationUnresolved)?;
                    current.step = Step::ReturnApproved;
                    current.failure = Some(failure);
                    data.write_json(c"service.json", &state)?;
                    runtime::updates::record_start_failure(
                        data,
                        operation.id,
                        &operation.candidate.digest,
                        &operation.candidate.version,
                        failure,
                    )?;
                }
                Step::ReturnApproved => {
                    self.repoint_cli_locked(&self.runtime(&state));
                    self.require_quiescent(&state)?;
                    if state.stopped {
                        self.set_step(data, operation.id, Step::Returned)?;
                        continue;
                    }
                    self.supervisor.check(&self.runtime(&state), false)?;
                    self.supervisor.prepare_start()?;
                    self.change(data, operation.id, |operation| {
                        operation.step = Step::ReturnStartDispatched;
                        operation.started_at = Some(unix_now());
                    })?;
                    self.supervisor.start()?;
                    self.supervisor.set_automatic_start(state.automatic_start)?;
                }
                Step::ReturnStartDispatched => {
                    // The previous version's own run records Returned once it is ready. One
                    // that misses the deadline leaves the outcome unresolved.
                    if !self.await_step(data, &operation, Step::ReturnStartDispatched)? {
                        return Err(CoreError::ActivationUnresolved);
                    }
                }
                Step::Returned => {
                    self.repoint_cli_locked(&self.runtime(&state));
                    remove_snapshot(data)?;
                    return Ok(());
                }
            }
        }
    }
    /// Waits until the started server's own run moves `operation` on from `step`, or until the
    /// readiness deadline from its dispatch. Returns whether it moved on.
    fn await_step(
        &self,
        data: &Directory,
        operation: &Operation,
        step: Step,
    ) -> Result<bool, CoreError> {
        let deadline = operation
            .started_at
            .unwrap_or(0)
            .saturating_add(self.supervisor.ready_deadline().as_secs());
        loop {
            let current = Self::state(data)?
                .and_then(|state| state.activation)
                .filter(|current| current.id == operation.id)
                .map(|current| current.step);
            if current != Some(step) {
                return Ok(true);
            }
            if unix_now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    /// Waits for the stopped server's cleanup, refusing a run other than `expected`.
    fn wait_quiescent(
        &self,
        data: &Directory,
        operation: &Operation,
        expected: Option<&Run>,
    ) -> Result<(), CoreError> {
        if self.supervisor.running()? {
            return Err(CoreError::ActivationUnresolved);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
            if state.run.as_ref().is_some_and(|run| Some(run) != expected) {
                return Err(CoreError::ActivationUnresolved);
            }
            match check_cleanup(&state).and_then(|()| self.prove_quiescent()) {
                Ok(()) => return Ok(()),
                Err(CoreError::CleanupUnresolved)
                    if Instant::now() < deadline && !operation.failed =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// Why the candidate did not become ready. Disk full and a network port another program
    /// holds are transient: they say nothing about the version.
    fn classify(&self) -> StartFailure {
        if runtime::updates::storage_full(&self.paths.data, 64 * 1024 * 1024) {
            return StartFailure::DiskFull;
        }
        if let Ok((Some(policy), true, _)) = crate::direct::observe(&self.paths)
            && std::net::UdpSocket::bind(policy.bind)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::AddrInUse)
        {
            return StartFailure::PortInUse;
        }
        StartFailure::NotReady
    }
    fn require_quiescent(&self, state: &State) -> Result<(), CoreError> {
        if self.supervisor.running()? {
            return Err(CoreError::ActivationUnresolved);
        }
        check_cleanup(state)?;
        self.prove_quiescent()
    }
    fn set_step(&self, data: &Directory, id: [u8; 32], step: Step) -> Result<(), CoreError> {
        self.change(data, id, |operation| operation.step = step)
    }
    fn change(
        &self,
        data: &Directory,
        id: [u8; 32],
        edit: impl FnOnce(&mut Operation),
    ) -> Result<(), CoreError> {
        let _intent = data.lock("service-intent.lock", false)?;
        let mut state = Self::state(data)?.ok_or(CoreError::NotInstalled)?;
        let operation = state
            .activation
            .as_mut()
            .ok_or(CoreError::ActivationUnresolved)?;
        if operation.id != id {
            return Err(CoreError::Changed);
        }
        edit(operation);
        operation.failed = false;
        data.write_json(c"service.json", &state)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Copies the retained state beside itself and returns the copy's digest.
fn take_snapshot(data: &Directory) -> Result<[u8; 32], CoreError> {
    for (live, copy) in SNAPSHOT {
        data.copy_record(live, copy)?;
    }
    snapshot_digest(data, 1)
}

/// Restores the retained state from the snapshot whose digest is `saved`.
fn restore_snapshot(data: &Directory, saved: [u8; 32]) -> Result<(), CoreError> {
    if snapshot_digest(data, 1)? != saved {
        return Err(CoreError::UnsafeStorage);
    }
    for (live, copy) in SNAPSHOT {
        data.copy_record(copy, live)?;
    }
    Ok(())
}

fn remove_snapshot(data: &Directory) -> Result<(), CoreError> {
    for (_, copy) in SNAPSHOT {
        data.remove_record(copy)?;
    }
    Ok(())
}

/// The digest of the retained-state files at `side` of [`SNAPSHOT`]: 0 live, 1 the copy.
/// Presence is distinct from a JSON null document.
fn snapshot_digest(data: &Directory, side: usize) -> Result<[u8; 32], CoreError> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(b"spaceterm-core-retained-state-1\0");
    for pair in SNAPSHOT {
        let name = [pair.0, pair.1][side]
            .to_str()
            .map_err(|_| CoreError::UnsafeStorage)?;
        digest.update(pair.0.to_bytes_with_nul());
        match data.read_json::<serde_json::Value>(name)? {
            None => digest.update(b"absent\0"),
            Some(value) => {
                let bytes = zeroize::Zeroizing::new(
                    serde_json::to_vec(&value).map_err(|_| CoreError::StorageUnavailable)?,
                );
                digest.update(b"present\0");
                digest.update(&(bytes.len() as u64).to_be_bytes());
                digest.update(&bytes);
            }
        }
    }
    digest
        .finish()
        .as_ref()
        .try_into()
        .map_err(|_| CoreError::StorageUnavailable)
}

pub(super) fn confirmation_token(value: &impl Serialize) -> Result<String, CoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| CoreError::StorageUnavailable)?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect())
}

/// Whether two verified release versions identify an ordinary update as a downgrade.
/// Development commits have no inferred ordering. This says nothing about protocol compatibility.
pub fn is_released_downgrade(candidate: &str, approved: &str) -> bool {
    let candidate: Vec<_> = candidate.split('.').collect();
    let approved: Vec<_> = approved.split('.').collect();
    if candidate.len() != 3
        || approved.len() != 3
        || candidate
            .iter()
            .chain(&approved)
            .any(|part| part.is_empty() || !part.bytes().all(|v| v.is_ascii_digit()))
    {
        return false;
    }
    candidate
        .iter()
        .map(|v| (v.len(), *v))
        .cmp(approved.iter().map(|v| (v.len(), *v)))
        .is_lt()
}

/// Mark readiness durably before publishing an endpoint or admitting any client work.
pub(crate) fn ready(paths: &Paths) -> Result<(), CoreError> {
    let data = Directory::open(&paths.data, false)?;
    let _intent = data.lock("service-intent.lock", false)?;
    let Some(mut state) = Service::state(&data)? else {
        return Ok(());
    };
    let Some(operation) = state.activation.as_mut() else {
        return Ok(());
    };
    if operation.step == Step::ReturnStartDispatched && !state.stopped {
        operation.step = Step::Returned;
        data.write_json(c"service.json", &state)?;
    } else if matches!(operation.step, Step::StartDispatched | Step::Completed)
        && !operation.ready
        && !state.stopped
    {
        if operation.candidate_run.is_none() || operation.candidate_run != state.run {
            return Err(CoreError::ActivationUnresolved);
        }
        operation.ready = true;
        operation.failed = false;
        operation.step = Step::Completed;
        data.write_json(c"service.json", &state)?;
    } else if operation.pending() {
        return Err(CoreError::ActivationUnresolved);
    }
    Ok(())
}

pub(super) fn admit_run(state: &mut State, run: &Run) -> Result<(), CoreError> {
    // The previous version starting again after a return is no candidate run.
    if state
        .activation
        .as_ref()
        .is_some_and(|operation| operation.step == Step::ReturnStartDispatched)
    {
        return Ok(());
    }
    if let Some(operation) = state.activation.as_mut()
        && (operation.pending() || (operation.step == Step::Completed && !operation.ready))
    {
        if !matches!(operation.step, Step::StartDispatched | Step::Completed)
            || operation.candidate_run.is_some()
        {
            return Err(CoreError::ActivationUnresolved);
        }
        operation.candidate_run = Some(run.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
