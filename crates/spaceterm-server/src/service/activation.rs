//! Exact, account-authorized runtime replacement and recovery of its original operation.
use super::*;

/// The result a control surface can present without interpreting retained transition steps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationPhase {
    Activating,
    Completed { running: bool },
    Failed { can_return: bool },
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
    pub returned: bool,
}

/// One approval binds the selected service, artifact, boot and observed installation/run policy.
pub struct ActivationApproval {
    service: Service,
    state: State,
    boot: String,
    server: [u8; 32],
    running: bool,
    previous: RetainedInstallation,
    candidate: RetainedInstallation,
}
impl ActivationApproval {
    /// Nonsecret exact-observation digest for confirmation across separate CLI invocations.
    pub fn confirmation_token(&self) -> Result<String, ServerError> {
        confirmation_token(&(
            &self.service.identity,
            &self.state,
            &self.boot,
            self.server,
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

/// A separate approval to return to a verified previous runtime while keeping the server stopped.
pub struct ReturnApproval {
    service: Service,
    state: State,
    previous: RetainedInstallation,
    candidate: RetainedInstallation,
}
impl ReturnApproval {
    pub fn confirmation_token(&self) -> Result<String, ServerError> {
        confirmation_token(&(&self.service.identity, &self.state))
    }
    pub fn previous(&self) -> &RetainedInstallation {
        &self.previous
    }
    pub fn candidate(&self) -> &RetainedInstallation {
        &self.candidate
    }
    pub fn stopped(&self) -> bool {
        true
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
    ReturnAdmitted,
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
    fn from_installation(value: &RetainedInstallation) -> Result<Self, ServerError> {
        let digest = value
            .directory
            .file_name()
            .and_then(|v| v.to_str())
            .filter(|v| runtime::digest_valid(v))
            .ok_or(ServerError::RuntimeUnavailable)?;
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
    saved: Option<[u8; 32]>,
    ready: bool,
    failed: bool,
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
            Step::StartDispatched | Step::Completed | Step::Returned
        )
    }
    pub(super) fn startup_failed(&mut self) {
        if self.step == Step::Completed && !self.ready {
            self.failed = true;
        }
    }
    pub(super) fn validate(&self, state: &State) -> Result<(), ServerError> {
        let expected = if matches!(
            self.step,
            Step::Admitted
                | Step::StopDispatched
                | Step::Quiescent
                | Step::RegistrationReplaced
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
                && (!state.installed || state.removing || state.stopped != self.stopped)
                && self.step != Step::ReturnAdmitted)
            || (!matches!(self.step, Step::Admitted | Step::StopDispatched) && self.saved.is_none())
            || (self.step == Step::ReturnAdmitted
                && (!state.installed || state.removing || !state.stopped))
            || (matches!(
                self.step,
                Step::Quiescent | Step::RegistrationReplaced | Step::Approved
            ) && (state.run.is_some() || self.candidate_run.is_some()))
            || (self.step == Step::StartDispatched
                && state
                    .run
                    .as_ref()
                    .is_some_and(|run| Some(run) != self.candidate_run.as_ref()))
        {
            return Err(ServerError::UnsafeStorage);
        }
        Ok(())
    }
}

impl Service {
    /// Read-only approval preparation. Staging alone grants no runtime authority.
    pub fn prepare_activation(
        &self,
        candidate: &RetainedInstallation,
    ) -> Result<Option<ActivationApproval>, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let _intent = data.lock("service-intent.lock", false)?;
        data.verify_path(&self.paths.data)?;
        let state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        if state.activation.as_ref().is_some_and(Operation::pending) {
            return Err(ServerError::ActivationUnresolved);
        }
        if !state.installed {
            return Err(ServerError::NotInstalled);
        }
        let status = self.snapshot(&state)?;
        let installations = owned_directory(&self.paths.data.join("installations"))?;
        lock_directory(&installations)?;
        self.verify_choice(candidate)?;
        runtime::verify_candidate_installation(
            &candidate.directory,
            &self.identity,
            crate::protocol::MAJOR,
            &["workspace_policy"],
        )?;
        let previous = self.describe_approved(&state)?;
        if previous.directory == candidate.directory {
            return Ok(None);
        }
        if is_released_downgrade(&candidate.version, &previous.version) {
            return Err(ServerError::RuntimeUnavailable);
        }
        let server = data.read()?.server;
        Ok(Some(ActivationApproval {
            service: self.clone(),
            state,
            boot: boot()?,
            server,
            running: status.running,
            previous,
            candidate: candidate.clone(),
        }))
    }

    /// Admit this approval once, then resolve only its durably retained original operation.
    pub fn activate(&self, approval: ActivationApproval) -> Result<ActivationStatus, ServerError> {
        self.same_service(&approval.service)?;
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        {
            let _intent = data.lock("service-intent.lock", false)?;
            data.verify_path(&self.paths.data)?;
            let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
            if state != approval.state
                || boot()? != approval.boot
                || data.read()?.server != approval.server
                || self.snapshot(&state)?.running != approval.running
            {
                return Err(ServerError::Changed);
            }
            let installations = owned_directory(&self.paths.data.join("installations"))?;
            lock_directory(&installations)?;
            self.verify_choice(&approval.previous)?;
            self.verify_choice(&approval.candidate)?;
            runtime::verify_candidate_installation(
                &approval.candidate.directory,
                &self.identity,
                crate::protocol::MAJOR,
                &["workspace_policy"],
            )?;
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
                ready: false,
                failed: false,
            });
            data.write_json(c"service.json", &state)?;
        }
        self.resume_activation(&data)?;
        self.activation_status_locked(&data)?
            .ok_or(ServerError::ActivationUnresolved)
    }

    /// Observe the original operation without dispatching or granting any authority.
    pub fn activation_status(&self) -> Result<Option<ActivationStatus>, ServerError> {
        let Some((data, _)) = self.read()? else {
            return Ok(None);
        };
        let _operation = data.lock("service.lock", false)?;
        self.activation_status_locked(&data)
    }

    /// Finish an admitted exact target. Never resend destructive Stop or candidate Start dispatch.
    pub fn recover_activation(&self) -> Result<Option<ActivationStatus>, ServerError> {
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

    pub fn prepare_return(&self) -> Result<ReturnApproval, ServerError> {
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        let _intent = data.lock("service-intent.lock", false)?;
        let state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
        self.prove_return(&data, &state)?;
        let operation = state
            .activation
            .as_ref()
            .ok_or(ServerError::ReturnUnavailable)?;
        Ok(ReturnApproval {
            service: self.clone(),
            previous: operation.previous.installation(self),
            candidate: operation.candidate.installation(self),
            state,
        })
    }

    /// Explicit return never restores an old state snapshot or any ended Terminal Session.
    /// The previous runtime remains stopped until a later explicit Start Server.
    pub fn return_to_previous(
        &self,
        approval: ReturnApproval,
    ) -> Result<ActivationStatus, ServerError> {
        self.same_service(&approval.service)?;
        let (data, _) = self.read()?.ok_or(ServerError::NotInstalled)?;
        let _operation = data.lock("service.lock", false)?;
        {
            let _intent = data.lock("service-intent.lock", false)?;
            let mut state = Self::state(&data)?.ok_or(ServerError::NotInstalled)?;
            if state != approval.state {
                return Err(ServerError::Changed);
            }
            self.prove_return(&data, &state)?;
            let operation = state
                .activation
                .as_mut()
                .ok_or(ServerError::ReturnUnavailable)?;
            operation.step = Step::ReturnAdmitted;
            state.stopped = true;
            data.write_json(c"service.json", &state)?;
        }
        self.resume_activation(&data)?;
        self.activation_status_locked(&data)?
            .ok_or(ServerError::ActivationUnresolved)
    }

    fn same_service(&self, other: &Self) -> Result<(), ServerError> {
        if self.identity != other.identity
            || self.paths.data != other.paths.data
            || self.paths.runtime != other.paths.runtime
        {
            Err(ServerError::AuthenticationRefused)
        } else {
            Ok(())
        }
    }
    fn verify_choice(&self, choice: &RetainedInstallation) -> Result<(), ServerError> {
        let build = Build::from_installation(choice)?;
        if build.installation(self) != *choice
            || runtime::describe(&choice.directory, &self.identity)?
                != (choice.version.clone(), choice.commit.clone())
        {
            return Err(ServerError::RuntimeUnavailable);
        }
        Ok(())
    }
    pub(super) fn describe_approved(
        &self,
        state: &State,
    ) -> Result<RetainedInstallation, ServerError> {
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
    ) -> Result<Option<ActivationStatus>, ServerError> {
        let state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
        let Some(operation) = &state.activation else {
            return Ok(None);
        };
        let phase = match operation.step {
            Step::Completed if operation.failed && !operation.ready => ActivationPhase::Failed {
                can_return: self.prove_return(data, &state).is_ok(),
            },
            Step::Completed | Step::Returned => self
                .snapshot(&state)
                .map(|s| ActivationPhase::Completed { running: s.running })
                .unwrap_or(ActivationPhase::Uncertain),
            Step::StartDispatched if !self.supervisor.running().unwrap_or(true) => {
                ActivationPhase::Failed {
                    can_return: self.prove_return(data, &state).is_ok(),
                }
            }
            _ if operation.failed => ActivationPhase::Uncertain,
            _ => ActivationPhase::Activating,
        };
        Ok(Some(ActivationStatus {
            operation: operation.id,
            previous: operation.previous.installation(self),
            candidate: operation.candidate.installation(self),
            stopped: state.stopped,
            phase,
            returned: operation.step == Step::Returned,
        }))
    }

    /// Called only under service.lock. A failed write may already be durable, so each next
    /// entrypoint rereads the record instead of continuing an in-memory transition.
    pub(super) fn resume_activation(&self, data: &Directory) -> Result<(), ServerError> {
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

    fn resume_steps(&self, data: &Directory) -> Result<(), ServerError> {
        loop {
            let Some(mut state) = Self::state(data)? else {
                return Ok(());
            };
            let Some(operation) = state.activation.clone() else {
                return Ok(());
            };
            if !operation.pending() {
                return Ok(());
            }
            data.verify_path(&self.paths.data)?;
            match operation.step {
                Step::Admitted => {
                    self.verify_choice(&operation.previous.installation(self))?;
                    self.verify_choice(&operation.candidate.installation(self))?;
                    self.supervisor.check(&self.runtime(&state), false)?;
                    if !state.stopped {
                        // Durable dispatch marker precedes the native call. Interruption in this
                        // gap requires intervention if the old process remains live; never resend.
                        self.set_step(data, operation.id, Step::StopDispatched)?;
                        self.supervisor.stop()?;
                        self.wait_running(false)?;
                    } else {
                        self.set_step(data, operation.id, Step::StopDispatched)?;
                    }
                }
                Step::StopDispatched => {
                    self.supervisor.check(&self.runtime(&state), false)?;
                    if self.supervisor.running()? {
                        return Err(ServerError::ActivationUnresolved);
                    }
                    let deadline = Instant::now() + Duration::from_secs(15);
                    loop {
                        state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
                        if state
                            .run
                            .as_ref()
                            .is_some_and(|run| Some(run) != operation.original_run.as_ref())
                        {
                            return Err(ServerError::ActivationUnresolved);
                        }
                        match check_cleanup(&state).and_then(|()| self.prove_quiescent()) {
                            Ok(()) => break,
                            Err(ServerError::CleanupUnresolved)
                                if Instant::now() < deadline && !operation.failed =>
                            {
                                std::thread::sleep(Duration::from_millis(20))
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
                    let _writes = data
                        .lock("worktree.lock", true)
                        .map_err(|_| ServerError::CleanupUnresolved)?;
                    let _setup = data.lock("setup.lock", false)?;
                    let saved = saved_state(data)?;
                    let current = state
                        .activation
                        .as_mut()
                        .ok_or(ServerError::ActivationUnresolved)?;
                    current.saved = Some(saved);
                    current.step = Step::Quiescent;
                    state.run = None;
                    data.write_json(c"service.json", &state)?;
                }
                Step::Quiescent => {
                    self.require_quiescent(&state)?;
                    self.verify_choice(&operation.previous.installation(self))?;
                    self.verify_choice(&operation.candidate.installation(self))?;
                    self.supervisor.replace(
                        &operation.previous.installation(self).directory,
                        &operation.candidate.installation(self).directory,
                    )?;
                    self.set_step(data, operation.id, Step::RegistrationReplaced)?;
                }
                Step::RegistrationReplaced => {
                    self.require_quiescent(&state)?;
                    self.supervisor
                        .check(&operation.candidate.installation(self).directory, false)?;
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
                    state.installation = operation.candidate.digest.clone();
                    state
                        .activation
                        .as_mut()
                        .ok_or(ServerError::ActivationUnresolved)?
                        .step = Step::Approved;
                    data.write_json(c"service.json", &state)?;
                }
                Step::Approved => {
                    self.require_quiescent(&state)?;
                    self.verify_choice(&operation.candidate.installation(self))?;
                    if state.stopped {
                        self.set_step(data, operation.id, Step::Completed)?;
                    } else {
                        self.supervisor.check(&self.runtime(&state), false)?;
                        self.supervisor.prepare_start()?;
                        self.set_step(data, operation.id, Step::StartDispatched)?;
                        self.supervisor.start()?;
                        self.supervisor.set_automatic_start(state.automatic_start)?;
                        self.wait_running(true)?;
                    }
                }
                Step::StartDispatched => {
                    // The candidate's readiness marker is written before endpoint publication.
                    // Neither a timeout nor an absent process permits another native Start.
                    return Err(ServerError::ActivationUnresolved);
                }
                Step::ReturnAdmitted => {
                    self.require_quiescent(&state)?;
                    if operation.ready || operation.saved != Some(saved_state(data)?) {
                        return Err(ServerError::ReturnUnavailable);
                    }
                    self.verify_choice(&operation.previous.installation(self))?;
                    self.supervisor.replace(
                        &operation.candidate.installation(self).directory,
                        &operation.previous.installation(self).directory,
                    )?;
                    let _intent = data.lock("service-intent.lock", false)?;
                    state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
                    state.installation = operation.previous.digest.clone();
                    state.stopped = true;
                    state.run = None;
                    state
                        .activation
                        .as_mut()
                        .ok_or(ServerError::ActivationUnresolved)?
                        .step = Step::Returned;
                    data.write_json(c"service.json", &state)?;
                }
                Step::Completed | Step::Returned => return Ok(()),
            }
        }
    }
    fn require_quiescent(&self, state: &State) -> Result<(), ServerError> {
        if self.supervisor.running()? {
            return Err(ServerError::ActivationUnresolved);
        }
        check_cleanup(state)?;
        self.prove_quiescent()
    }
    fn set_step(&self, data: &Directory, id: [u8; 32], step: Step) -> Result<(), ServerError> {
        let _intent = data.lock("service-intent.lock", false)?;
        let mut state = Self::state(data)?.ok_or(ServerError::NotInstalled)?;
        let operation = state
            .activation
            .as_mut()
            .ok_or(ServerError::ActivationUnresolved)?;
        if operation.id != id {
            return Err(ServerError::Changed);
        }
        operation.step = step;
        operation.failed = false;
        data.write_json(c"service.json", &state)
    }
    fn prove_return(&self, data: &Directory, state: &State) -> Result<(), ServerError> {
        let operation = state
            .activation
            .as_ref()
            .ok_or(ServerError::ReturnUnavailable)?;
        if !(operation.step == Step::StartDispatched
            || (operation.step == Step::Completed && operation.failed))
            || operation.ready
            || operation.saved.is_none()
        {
            return Err(ServerError::ReturnUnavailable);
        }
        self.require_quiescent(state)
            .map_err(|_| ServerError::ReturnUnavailable)?;
        let _writes = data
            .lock("worktree.lock", true)
            .map_err(|_| ServerError::ReturnUnavailable)?;
        let _setup = data.lock("setup.lock", false)?;
        if operation.saved != Some(saved_state(data)?) {
            return Err(ServerError::ReturnUnavailable);
        }
        self.verify_choice(&operation.previous.installation(self))?;
        self.supervisor
            .check(&operation.candidate.installation(self).directory, false)
    }
}

pub(super) fn confirmation_token(value: &impl Serialize) -> Result<String, ServerError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerError::StorageUnavailable)?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect())
}

fn saved_state(data: &Directory) -> Result<[u8; 32], ServerError> {
    // The proof covers server organization and direct identity/policy/revocation together.
    // Client files are not rolled back. Presence is distinct from a JSON null document.
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(b"spaceterm-server-saved-state-2\0");
    let identity = zeroize::Zeroizing::new(
        serde_json::to_vec(&data.read()?).map_err(|_| ServerError::StorageUnavailable)?,
    );
    digest.update(b"identity.json\0present\0");
    digest.update(&(identity.len() as u64).to_be_bytes());
    digest.update(&identity);
    match data.read_json::<serde_json::Value>("direct.json")? {
        None => digest.update(b"direct.json\0absent\0"),
        Some(value) => {
            let direct = zeroize::Zeroizing::new(
                serde_json::to_vec(&value).map_err(|_| ServerError::StorageUnavailable)?,
            );
            digest.update(b"direct.json\0present\0");
            digest.update(&(direct.len() as u64).to_be_bytes());
            digest.update(&direct);
        }
    }
    digest
        .finish()
        .as_ref()
        .try_into()
        .map_err(|_| ServerError::StorageUnavailable)
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
pub(crate) fn ready(paths: &Paths) -> Result<(), ServerError> {
    let data = Directory::open(&paths.data, false)?;
    let _intent = data.lock("service-intent.lock", false)?;
    let Some(mut state) = Service::state(&data)? else {
        return Ok(());
    };
    let Some(operation) = state.activation.as_mut() else {
        return Ok(());
    };
    if matches!(operation.step, Step::StartDispatched | Step::Completed)
        && !operation.ready
        && !state.stopped
    {
        if operation.candidate_run.is_none() || operation.candidate_run != state.run {
            return Err(ServerError::ActivationUnresolved);
        }
        operation.ready = true;
        operation.failed = false;
        operation.step = Step::Completed;
        data.write_json(c"service.json", &state)?;
    } else if operation.pending() {
        return Err(ServerError::ActivationUnresolved);
    }
    Ok(())
}

pub(super) fn admit_run(state: &mut State, run: &Run) -> Result<(), ServerError> {
    if let Some(operation) = state.activation.as_mut()
        && (operation.pending() || (operation.step == Step::Completed && !operation.ready))
    {
        if !matches!(operation.step, Step::StartDispatched | Step::Completed)
            || operation.candidate_run.is_some()
        {
            return Err(ServerError::ActivationUnresolved);
        }
        operation.candidate_run = Some(run.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
