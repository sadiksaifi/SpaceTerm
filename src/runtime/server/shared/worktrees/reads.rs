//! Connection-scoped Git reads. Polling never waits for a Git process.
use super::*;
use std::collections::BTreeMap;

pub(super) struct PreparedRead {
    pub reply: ReadReply,
    pub approval: Option<Approval>,
}
struct Job {
    token: SshCancellationToken,
    worker: std::thread::JoinHandle<Result<PreparedRead, Error>>,
    deadline: Instant,
}
#[derive(Default)]
pub(super) struct Reads {
    next: u64,
    jobs: BTreeMap<(Id, u64), Job>,
    retiring: Vec<Job>,
}
impl Reads {
    fn cancel(&mut self, key: (Id, u64)) {
        if let Some(job) = self.jobs.remove(&key) {
            job.token.cancel();
            self.retiring.push(job);
        }
    }
    pub fn cancel_connection(&mut self, connection: Id) {
        let keys: Vec<_> = self
            .jobs
            .keys()
            .filter(|(c, _)| *c == connection)
            .copied()
            .collect();
        for key in keys {
            self.cancel(key);
        }
    }
    pub fn stop(&mut self) {
        let keys: Vec<_> = self.jobs.keys().copied().collect();
        for key in keys {
            self.cancel(key);
        }
    }
    fn tick(&mut self) {
        let expired: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.deadline <= Instant::now())
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.cancel(key);
        }
        self.retiring.retain(|job| !job.worker.is_finished());
    }
    pub fn begin(
        &mut self,
        connection: Id,
        device: Id,
        read: Read,
        inventory: Inventory,
        settings: Settings,
    ) -> Result<ReadReply, Error> {
        self.tick();
        if self.jobs.len() + self.retiring.len() >= 32
            || self.jobs.keys().filter(|(c, _)| *c == connection).count() >= 2
        {
            return Err(Error::Unavailable);
        }
        let id = self.next.checked_add(1).ok_or(Error::Unavailable)?;
        self.next = id;
        let token = SshCancellationToken::default();
        let cancellation = token.clone();
        let worker = std::thread::Builder::new()
            .name("spaceterm-worktree-read".into())
            .spawn(move || read_git(device, read, inventory, settings, &cancellation))
            .map_err(|_| Error::Unavailable)?;
        self.jobs.insert(
            (connection, id),
            Job {
                token,
                worker,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );
        Ok(ReadReply::Pending { id })
    }
    pub fn poll(&mut self, connection: Id, id: u64) -> Result<PreparedRead, Error> {
        self.tick();
        let key = (connection, id);
        let job = self.jobs.get(&key).ok_or(Error::Unavailable)?;
        if !job.worker.is_finished() {
            return Ok(PreparedRead {
                reply: ReadReply::Pending { id },
                approval: None,
            });
        }
        self.jobs
            .remove(&key)
            .ok_or(Error::Unavailable)?
            .worker
            .join()
            .unwrap_or(Err(Error::Unavailable))
    }
}
impl Drop for Reads {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_git(
    device: Id,
    read: Read,
    inventory: Inventory,
    settings: Settings,
    token: &SshCancellationToken,
) -> Result<PreparedRead, Error> {
    let (git, home) = git(None, token)?;
    let mut approval = None;
    let reply = match read {
        Read::Context { root } => {
            list(&git, &root, token)?;
            let branches = git.branches(&root, token).map_err(|_| Error::Unavailable)?;
            if branches.local.len() + branches.remote.len() > 4096 {
                return Err(Error::Unavailable);
            }
            ReadReply::Context {
                settings,
                home,
                branches,
            }
        }
        Read::Location { root, location } => {
            list(&git, &root, token)?;
            let location = location
                .strip_prefix("~/")
                .map_or_else(|| PathBuf::from(&location), |suffix| home.join(suffix));
            if !location.is_absolute() {
                return Err(Error::Location);
            }
            match local_filesystem().probe_new_directory(&location) {
                Ok(crate::platform::local_filesystem::NewDirectoryTarget::Free) => {
                    ReadReply::Location(location)
                }
                _ => return Err(Error::Location),
            }
        }
        Read::CheckRemoval { root, path } => {
            let list = list(&git, &root, token)?;
            let record = list
                .worktrees
                .iter()
                .find(|r| r.root == path)
                .ok_or(Error::Missing)?
                .clone();
            if path == root {
                return Err(Error::Main);
            }
            if record.locked {
                return Err(Error::Locked);
            }
            if pinned_in_worktree(&inventory, &path)? {
                return Err(Error::Pinned);
            }
            let (expected, changes, submodules) = if matches!(
                local_filesystem().validate_directory(&path),
                Err(LocalFilesystemError::Missing)
            ) {
                (RemovalExpectation::Absent(local_filesystem()), false, false)
            } else {
                let check = git
                    .check_removal(&path, &local_filesystem(), token)
                    .map_err(|_| Error::Unavailable)?;
                (
                    RemovalExpectation::Worktree {
                        identity: check.identity,
                        require_clean: !check.changes,
                    },
                    check.changes,
                    check.submodules,
                )
            };
            let review = Removal {
                boot: inventory.close.as_ref().ok_or(Error::Unavailable)?.boot,
                confirmation: nonce().map_err(|_| Error::Unavailable)?,
                tabs: affected_worktree_tabs(&inventory, &root, &path),
                root,
                path,
                changes,
                submodules,
            };
            approval = Some(Approval {
                device,
                review: review.clone(),
                expected,
                record,
                expires: Instant::now() + Duration::from_secs(300),
            });
            ReadReply::Removal(review)
        }
        _ => return Err(Error::Unavailable),
    };
    Ok(PreparedRead { reply, approval })
}
