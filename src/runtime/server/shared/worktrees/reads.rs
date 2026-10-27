//! Connection-scoped Git reads. Each runs on its own thread, which wakes the host when it settles;
//! nothing waits for a Git process.
use super::*;
use std::collections::BTreeMap;

pub(super) struct PreparedRead {
    pub reply: ReadReply,
    pub approval: Option<Approval>,
}
/// Reads of one kind a connection runs at once. Every client reader shares one connection, so
/// each kind of read has its own bound and a slow kind cannot starve another.
const READS_PER_KIND: usize = 2;
/// Reads the server runs at once across every connection.
const READS: usize = 32;

/// A read of one connection, by its request id.
type Key = (Id, RequestId);
struct Job {
    kind: std::mem::Discriminant<Read>,
    token: CancellationToken,
    result: std::sync::mpsc::Receiver<Result<PreparedRead, Error>>,
    worker: std::thread::JoinHandle<()>,
    deadline: Instant,
}
#[derive(Default)]
pub(super) struct Reads {
    jobs: BTreeMap<Key, Job>,
    retiring: Vec<Job>,
    wake: Option<Arc<dyn spaceterm_server::connection::Wake>>,
}
impl Reads {
    /// Every read started from now on wakes `wake` when it settles.
    pub fn set_wake(&mut self, wake: Arc<dyn spaceterm_server::connection::Wake>) {
        self.wake = Some(wake);
    }
    /// Stops a running read. It never settles.
    pub fn cancel(&mut self, key: Key) {
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
    /// When the earliest running read outlasts its deadline.
    pub fn due(&self) -> Option<Instant> {
        self.jobs.values().map(|job| job.deadline).min()
    }
    pub fn begin(
        &mut self,
        key: Key,
        device: Id,
        read: Read,
        inventory: Inventory,
        settings: Settings,
    ) -> Result<(), Error> {
        self.retiring.retain(|job| !job.worker.is_finished());
        let (connection, _) = key;
        let kind = std::mem::discriminant(&read);
        if self.jobs.len() + self.retiring.len() >= READS
            || self
                .jobs
                .iter()
                .filter(|((c, _), job)| *c == connection && job.kind == kind)
                .count()
                >= READS_PER_KIND
        {
            return Err(Error::Unavailable);
        }
        if self.jobs.contains_key(&key) {
            return Err(Error::Unavailable);
        }
        let token = CancellationToken::default();
        let cancellation = token.clone();
        let wake = self.wake.clone();
        let (sender, result) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("spaceterm-worktree-read".into())
            .spawn(move || {
                let _ = sender.send(read_git(device, read, inventory, settings, &cancellation));
                if let Some(wake) = wake {
                    wake.wake();
                }
            })
            .map_err(|_| Error::Unavailable)?;
        self.jobs.insert(
            key,
            Job {
                kind,
                token,
                result,
                worker,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        );
        Ok(())
    }
    /// The reads that finished, and those that outlasted their deadline as
    /// [`Error::Unavailable`].
    pub fn settled(&mut self) -> Vec<(Key, Result<PreparedRead, Error>)> {
        use std::sync::mpsc::TryRecvError;
        self.retiring.retain(|job| !job.worker.is_finished());
        let now = Instant::now();
        let mut settled = Vec::new();
        let mut expired = Vec::new();
        self.jobs.retain(|key, job| match job.result.try_recv() {
            Ok(prepared) => {
                settled.push((*key, prepared));
                false
            }
            Err(TryRecvError::Empty) if now < job.deadline => true,
            Err(TryRecvError::Empty) => {
                expired.push(*key);
                true
            }
            Err(TryRecvError::Disconnected) => {
                settled.push((*key, Err(Error::Unavailable)));
                false
            }
        });
        for key in expired {
            self.cancel(key);
            settled.push((key, Err(Error::Unavailable)));
        }
        settled
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
    token: &CancellationToken,
) -> Result<PreparedRead, Error> {
    let reply = match &read {
        Read::ServerFacts => Some(ReadReply::ServerFacts(
            crate::server_settings::server_facts()?,
        )),
        Read::Git => Some(ReadReply::Git(
            crate::runtime::server::repositories::git_status(token),
        )),
        Read::ValidateLocationTemplate { template } => {
            crate::server_settings::validate_location_template(template)?;
            Some(ReadReply::LocationTemplate)
        }
        _ => None,
    };
    if let Some(reply) = reply {
        return Ok(PreparedRead {
            reply,
            approval: None,
        });
    }
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
            if pinned_in_worktree(
                inventory
                    .workspaces
                    .iter()
                    .filter_map(|w| w.pinned_directory.as_deref()),
                &path,
            )? {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings' slow reads cannot take the slots of a validation or removal check on the same
    /// connection.
    #[test]
    fn each_kind_of_read_has_its_own_bound_per_connection() {
        let mut reads = Reads::default();
        let inventory = State::empty().inventory;
        let mut next = 0;
        let mut begin = |reads: &mut Reads, read: Read| {
            next += 1;
            reads.begin(
                ([1; 32], RequestId(next)),
                [2; 32],
                read,
                inventory.clone(),
                Settings::default(),
            )
        };
        for _ in 0..READS_PER_KIND {
            assert!(begin(&mut reads, Read::ServerFacts).is_ok());
            assert!(begin(&mut reads, Read::Git).is_ok());
        }
        assert_eq!(
            begin(&mut reads, Read::ServerFacts).err(),
            Some(Error::Unavailable)
        );
        assert!(
            begin(
                &mut reads,
                Read::ValidateLocationTemplate {
                    template: "{repository}".into()
                }
            )
            .is_ok()
        );
        reads.stop();
    }
}
