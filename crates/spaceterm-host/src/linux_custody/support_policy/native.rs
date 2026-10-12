//! Descriptor-bound account process observations. These are not custody authority.

#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeSet;

use crate::native_deployment::contracts::NativeFailure;
use std::fs::File;
use std::os::unix::fs::MetadataExt;

#[cfg(any(target_os = "linux", test))]
const MAX_PROCESSES: usize = 4096;
#[cfg(any(target_os = "linux", test))]
const MAX_TASKS: usize = 16_384;
#[cfg(any(target_os = "linux", test))]
const MAX_STATUS_BYTES: usize = 16 * 1024;

/// Native process observations visible through the retained procfs view.
///
/// The custody issuer must independently authenticate the procfs view and the
/// observer's user namespace before treating this as a host-wide inventory.
/// A process scan does not prove the absence of historical open bypass handles.
pub struct ProcessPolicy {
    groups: Vec<u32>,
    capabilities: u64,
    procfs: File,
    user_namespace: File,
}

impl std::fmt::Debug for ProcessPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProcessPolicy(..)")
    }
}

impl ProcessPolicy {
    /// Observes visible account threads through kernel procfs descriptors as root.
    /// No native command is executed and no host configuration is changed.
    pub fn observe(account_uid: u32) -> Result<Self, NativeFailure> {
        #[cfg(target_os = "linux")]
        {
            collect(account_uid, &os::Procfs::open()?)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = account_uid;
            Err(NativeFailure::UnsupportedHost)
        }
    }

    /// Primary, effective, saved, filesystem and supplementary GIDs of account threads.
    pub fn groups(&self) -> &[u32] {
        &self.groups
    }

    /// Permitted, effective, inheritable and ambient capabilities in the observer namespace.
    /// Capabilities in a different user namespace and bounding-set bits are excluded.
    pub fn capabilities_in_observer_namespace(&self) -> u64 {
        self.capabilities
    }

    /// Compares retained observation handles with independently trusted pins.
    /// Equality alone does not establish that either pin belongs to the host.
    pub fn matches_context(
        &self,
        procfs: &File,
        user_namespace: &File,
    ) -> Result<bool, NativeFailure> {
        let identity = |file: &File| {
            file.metadata()
                .map(|metadata| (metadata.dev(), metadata.ino()))
                .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)
        };
        Ok(identity(&self.procfs)? == identity(procfs)?
            && identity(&self.user_namespace)? == identity(user_namespace)?)
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Eq, PartialEq)]
struct NamespaceIdentity {
    device: u64,
    inode: u64,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Eq, PartialEq)]
struct TaskFacts {
    pid: u32,
    tgid: u32,
    start: u64,
    uids: [u32; 4],
    gids: [u32; 4],
    groups: Vec<u32>,
    capabilities: u64,
    user_namespace: NamespaceIdentity,
}

#[cfg(any(target_os = "linux", test))]
trait TaskPeer {
    fn id(&self) -> u32;
    fn sample(&self) -> Result<TaskFacts, NativeFailure>;
}

#[cfg(any(target_os = "linux", test))]
trait ProcessPeer {
    type Task: TaskPeer;
    fn retain_context(&self) -> Result<(File, File), NativeFailure>;
    fn namespace(&self) -> Result<NamespaceIdentity, NativeFailure>;
    fn processes(&self) -> Result<Vec<u32>, NativeFailure>;
    fn tasks(&self, pid: u32) -> Result<Vec<Self::Task>, NativeFailure>;
}

#[cfg(any(target_os = "linux", test))]
fn collect<P: ProcessPeer>(account_uid: u32, peer: &P) -> Result<ProcessPolicy, NativeFailure> {
    if matches!(account_uid, 0 | u32::MAX) {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    let (procfs, user_namespace) = peer.retain_context()?;
    let namespace = peer.namespace()?;
    let processes = peer.processes()?;
    if processes.is_empty() || processes.len() > MAX_PROCESSES {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    let mut seen_processes = BTreeSet::new();
    let mut status_records = 0usize;
    let mut groups = BTreeSet::new();
    let mut capabilities = 0;
    for pid in &processes {
        if *pid == 0 || !seen_processes.insert(*pid) {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        let tasks = peer.tasks(*pid)?;
        // Each task supplies two status records. Reserve both before reading;
        // the 16384-record bound is not a 16384-task allowance.
        status_records = status_records
            .checked_add(
                tasks
                    .len()
                    .checked_mul(2)
                    .ok_or(NativeFailure::LimitExceeded)?,
            )
            .ok_or(NativeFailure::LimitExceeded)?;
        if status_records > MAX_TASKS {
            return Err(NativeFailure::LimitExceeded);
        }
        if tasks.is_empty() {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        let mut seen_tasks = BTreeSet::new();
        for task in tasks {
            let before = task.sample()?;
            let after = task.sample()?;
            if before != after
                || before.tgid != *pid
                || before.pid != task.id()
                || before.pid == 0
                || !seen_tasks.insert(before.pid)
                || before.groups.len() > MAX_TASKS
            {
                return Err(NativeFailure::LinuxPolicyCheckUnavailable);
            }
            if !before.uids.contains(&account_uid) {
                continue;
            }
            groups.extend(before.gids);
            groups.extend(before.groups);
            if groups.len() > MAX_TASKS {
                return Err(NativeFailure::LimitExceeded);
            }
            if before.user_namespace == namespace {
                capabilities |= before.capabilities;
            }
        }
        let current_tasks = peer.tasks(*pid)?;
        if current_tasks.len() != seen_tasks.len()
            || current_tasks
                .into_iter()
                .map(|task| task.id())
                .collect::<BTreeSet<_>>()
                != seen_tasks
        {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
    }
    let after = peer.processes()?;
    if peer.namespace()? != namespace
        || after.len() != processes.len()
        || after.into_iter().collect::<BTreeSet<_>>() != seen_processes
    {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    Ok(ProcessPolicy {
        groups: groups.into_iter().collect(),
        capabilities,
        procfs,
        user_namespace,
    })
}

#[cfg(any(target_os = "linux", test))]
fn decode_task(
    status: &[u8],
    stat: &[u8],
    user_namespace: NamespaceIdentity,
) -> Result<TaskFacts, NativeFailure> {
    let unavailable = NativeFailure::LinuxPolicyCheckUnavailable;
    if status.len() > MAX_STATUS_BYTES || stat.len() > MAX_STATUS_BYTES {
        return Err(NativeFailure::LimitExceeded);
    }
    if !status.ends_with(b"\n") || !stat.ends_with(b"\n") {
        return Err(unavailable);
    }
    let mut pid = None;
    let mut tgid = None;
    let mut uids = None;
    let mut gids = None;
    let mut groups = None;
    let mut caps = [None; 5];
    for line in status.split(|byte| *byte == b'\n') {
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        let key = &line[..colon];
        // Only policy fields are decoded. Kernel task names and other status
        // text are neither interpreted as policy nor retained in the result.
        if !matches!(
            key,
            b"Pid"
                | b"Tgid"
                | b"Uid"
                | b"Gid"
                | b"Groups"
                | b"CapInh"
                | b"CapPrm"
                | b"CapEff"
                | b"CapBnd"
                | b"CapAmb"
        ) {
            continue;
        }
        let value = std::str::from_utf8(&line[colon + 1..])
            .map_err(|_| unavailable)?
            .trim();
        let duplicate = match key {
            b"Pid" => pid.replace(decimal(value)?).is_some(),
            b"Tgid" => tgid.replace(decimal(value)?).is_some(),
            b"Uid" => uids.replace(four_ids(value)?).is_some(),
            b"Gid" => gids.replace(four_ids(value)?).is_some(),
            b"Groups" => {
                let mut parsed = Vec::new();
                for group in value.split_ascii_whitespace() {
                    if parsed.len() >= MAX_TASKS {
                        return Err(NativeFailure::LimitExceeded);
                    }
                    parsed.push(decimal(group)?);
                }
                groups.replace(parsed).is_some()
            }
            _ => {
                let index = match key {
                    b"CapInh" => 0,
                    b"CapPrm" => 1,
                    b"CapEff" => 2,
                    b"CapBnd" => 3,
                    _ => 4,
                };
                if value.len() != 16 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(unavailable);
                }
                caps[index]
                    .replace(u64::from_str_radix(value, 16).map_err(|_| unavailable)?)
                    .is_some()
            }
        };
        if duplicate {
            return Err(unavailable);
        }
    }
    let pid = pid.ok_or(unavailable)?;
    let prefix = stat
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or(unavailable)?;
    let closing = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or(unavailable)?;
    if stat.get(prefix + 1) != Some(&b'(') || closing <= prefix + 1 {
        return Err(unavailable);
    }
    let stat_pid = std::str::from_utf8(&stat[..prefix]).map_err(|_| unavailable)?;
    let fields = std::str::from_utf8(&stat[closing + 1..]).map_err(|_| unavailable)?;
    let start = fields.split_ascii_whitespace().nth(19).ok_or(unavailable)?;
    if decimal::<u32>(stat_pid)? != pid {
        return Err(unavailable);
    }
    let [inheritable, permitted, effective, bounding, ambient] =
        caps.map(|cap| cap.ok_or(unavailable));
    bounding?;
    Ok(TaskFacts {
        pid,
        tgid: tgid.ok_or(unavailable)?,
        start: decimal(start)?,
        uids: uids.ok_or(unavailable)?,
        gids: gids.ok_or(unavailable)?,
        groups: groups.ok_or(unavailable)?,
        capabilities: inheritable? | permitted? | effective? | ambient?,
        user_namespace,
    })
}

#[cfg(any(target_os = "linux", test))]
fn decimal<T: std::str::FromStr>(value: &str) -> Result<T, NativeFailure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    value
        .parse()
        .map_err(|_| NativeFailure::LinuxPolicyCheckUnavailable)
}

#[cfg(any(target_os = "linux", test))]
fn four_ids(value: &str) -> Result<[u32; 4], NativeFailure> {
    let mut values = value.split_ascii_whitespace();
    let mut ids = [0; 4];
    for id in &mut ids {
        *id = decimal(
            values
                .next()
                .ok_or(NativeFailure::LinuxPolicyCheckUnavailable)?,
        )?;
    }
    if values.next().is_some() {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    Ok(ids)
}

#[cfg(target_os = "linux")]
mod os;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const HOST: NamespaceIdentity = NamespaceIdentity {
        device: 4,
        inode: 7,
    };
    const CHILD: NamespaceIdentity = NamespaceIdentity {
        device: 4,
        inode: 8,
    };

    struct Task {
        before: TaskFacts,
        after: TaskFacts,
        reads: Cell<usize>,
    }
    impl TaskPeer for Task {
        fn id(&self) -> u32 {
            self.before.pid
        }
        fn sample(&self) -> Result<TaskFacts, NativeFailure> {
            let read = self.reads.replace(self.reads.get() + 1);
            Ok(if read == 0 { &self.before } else { &self.after }.clone())
        }
    }
    struct Peer(Vec<TaskFacts>);
    impl ProcessPeer for Peer {
        type Task = Task;
        fn retain_context(&self) -> Result<(File, File), NativeFailure> {
            Ok((tempfile::tempfile().unwrap(), tempfile::tempfile().unwrap()))
        }
        fn namespace(&self) -> Result<NamespaceIdentity, NativeFailure> {
            Ok(HOST)
        }
        fn processes(&self) -> Result<Vec<u32>, NativeFailure> {
            Ok(vec![17])
        }
        fn tasks(&self, _: u32) -> Result<Vec<Task>, NativeFailure> {
            Ok(self
                .0
                .iter()
                .map(|facts| Task {
                    before: facts.clone(),
                    after: facts.clone(),
                    reads: Cell::new(0),
                })
                .collect())
        }
    }
    fn facts(tid: u32, namespace: NamespaceIdentity, cap: u64) -> TaskFacts {
        TaskFacts {
            pid: tid,
            tgid: 17,
            start: 31,
            uids: [1000; 4],
            gids: [1000; 4],
            groups: vec![1000, 999],
            capabilities: cap,
            user_namespace: namespace,
        }
    }
    const STATUS: &[u8] = b"Name:\tfixture\nTgid:\t17\nPid:\t18\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nGroups:\t1000 999 \nCapInh:\t0000000000000000\nCapPrm:\t0000000000080000\nCapEff:\t0000000000000000\nCapBnd:\t000000ffffffffff\nCapAmb:\t0000000000000000\n";
    // Fields 3 through 21 follow comm. The parser must use the final closing
    // parenthesis because task names can contain spaces and parentheses.
    const STAT: &[u8] = b"18 (fixture ) thread) S 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 31 0\n";
    #[test]
    fn native_status_decoder_keeps_permitted_caps_but_not_the_bounding_set() {
        let observed = decode_task(STATUS, STAT, HOST).unwrap();
        assert!(observed == facts(18, HOST, 1 << 19));
    }
    #[test]
    fn native_process_collector_observes_nonleader_groups_and_host_capabilities() {
        let mut leader = facts(17, HOST, 0);
        leader.groups = vec![1000];
        let peer = Peer(vec![leader, facts(18, HOST, 1 << 19)]);
        let observed = collect(1000, &peer).unwrap();
        assert_eq!(observed.groups(), &[999, 1000]);
        assert_eq!(observed.capabilities_in_observer_namespace(), 1 << 19);
    }
    #[test]
    fn child_user_namespace_capabilities_are_not_host_capabilities() {
        let observed = collect(1000, &Peer(vec![facts(17, CHILD, u64::MAX)])).unwrap();
        assert_eq!(observed.capabilities_in_observer_namespace(), 0);
        assert_eq!(observed.groups(), &[999, 1000]);
    }

    #[derive(Clone, Copy)]
    enum Fault {
        Groups,
        Start,
        TaskNamespace,
        ObserverNamespace,
        Tasks,
        Processes,
        Duplicate,
        Unreadable,
    }
    struct FaultPeer {
        fault: Fault,
        namespace_reads: Cell<usize>,
        process_reads: Cell<usize>,
        task_reads: Cell<usize>,
    }
    impl ProcessPeer for FaultPeer {
        type Task = Task;
        fn retain_context(&self) -> Result<(File, File), NativeFailure> {
            Peer(vec![]).retain_context()
        }
        fn namespace(&self) -> Result<NamespaceIdentity, NativeFailure> {
            let reads = self.namespace_reads.replace(self.namespace_reads.get() + 1);
            Ok(
                if reads > 0 && matches!(self.fault, Fault::ObserverNamespace) {
                    CHILD
                } else {
                    HOST
                },
            )
        }
        fn processes(&self) -> Result<Vec<u32>, NativeFailure> {
            let reads = self.process_reads.replace(self.process_reads.get() + 1);
            Ok(if reads > 0 && matches!(self.fault, Fault::Processes) {
                vec![17, 27]
            } else {
                vec![17]
            })
        }
        fn tasks(&self, _: u32) -> Result<Vec<Task>, NativeFailure> {
            if matches!(self.fault, Fault::Unreadable) {
                return Err(NativeFailure::LinuxPolicyCheckUnavailable);
            }
            let reads = self.task_reads.replace(self.task_reads.get() + 1);
            let mut tasks = Peer(vec![facts(17, HOST, 0), facts(18, HOST, 0)]).tasks(17)?;
            match self.fault {
                Fault::Groups => tasks[1].after.groups.clear(),
                Fault::Start => tasks[1].after.start += 1,
                Fault::TaskNamespace => tasks[1].after.user_namespace = CHILD,
                Fault::Tasks if reads > 0 => {
                    tasks.pop();
                }
                Fault::Duplicate => {
                    tasks[1].before.pid = 17;
                    tasks[1].after.pid = 17;
                }
                _ => {}
            }
            Ok(tasks)
        }
    }
    #[test]
    fn changed_incomplete_or_duplicated_process_observations_are_unavailable() {
        for fault in [
            Fault::Groups,
            Fault::Start,
            Fault::TaskNamespace,
            Fault::ObserverNamespace,
            Fault::Tasks,
            Fault::Processes,
            Fault::Duplicate,
            Fault::Unreadable,
        ] {
            let peer = FaultPeer {
                fault,
                namespace_reads: Cell::new(0),
                process_reads: Cell::new(0),
                task_reads: Cell::new(0),
            };
            assert!(matches!(
                collect(1000, &peer),
                Err(NativeFailure::LinuxPolicyCheckUnavailable)
            ));
        }
    }
    #[test]
    fn two_samples_share_the_task_status_record_limit() {
        let at_limit = (17..17 + (MAX_TASKS / 2) as u32)
            .map(|id| facts(id, HOST, 0))
            .collect();
        assert!(collect(1000, &Peer(at_limit)).is_ok());
        let over_limit = (17..18 + (MAX_TASKS / 2) as u32)
            .map(|id| facts(id, HOST, 0))
            .collect();
        assert!(matches!(
            collect(1000, &Peer(over_limit)),
            Err(NativeFailure::LimitExceeded)
        ));
    }
    #[test]
    fn native_status_decoder_refuses_missing_duplicate_and_malformed_fields() {
        let status = std::str::from_utf8(STATUS).unwrap();
        for field in [
            "Pid", "Tgid", "Uid", "Gid", "Groups", "CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb",
        ] {
            let key = format!("{field}:");
            let missing = status
                .lines()
                .filter(|line| !line.starts_with(&key))
                .map(|line| format!("{line}\n"))
                .collect::<String>();
            assert!(matches!(
                decode_task(missing.as_bytes(), STAT, HOST),
                Err(NativeFailure::LinuxPolicyCheckUnavailable)
            ));
            let duplicate = format!(
                "{status}{}\n",
                status.lines().find(|line| line.starts_with(&key)).unwrap()
            );
            assert!(matches!(
                decode_task(duplicate.as_bytes(), STAT, HOST),
                Err(NativeFailure::LinuxPolicyCheckUnavailable)
            ));
        }
        for malformed in [
            status.replace("1000\t1000\t1000\t1000", "1000\t1000\t1000"),
            status.replace("1000\t1000\t1000\t1000", "1000\t1000\t1000\t1000\t1000"),
            status.replace("Groups:\t1000 999", "Groups:\t-1 999"),
            status.replace("0000000000080000", "000000000008000g"),
            status.trim_end().to_owned(),
        ] {
            assert!(matches!(
                decode_task(malformed.as_bytes(), STAT, HOST),
                Err(NativeFailure::LinuxPolicyCheckUnavailable)
            ));
        }
        assert!(matches!(
            decode_task(&vec![b'x'; MAX_STATUS_BYTES + 1], STAT, HOST),
            Err(NativeFailure::LimitExceeded)
        ));
        assert!(matches!(
            decode_task(STATUS, b"18 (truncated) S\n", HOST),
            Err(NativeFailure::LinuxPolicyCheckUnavailable)
        ));
        assert!(matches!(
            decode_task(
                STATUS,
                &STAT
                    .iter()
                    .copied()
                    .map(|byte| if byte == b'8' { b'9' } else { byte })
                    .collect::<Vec<_>>(),
                HOST
            ),
            Err(NativeFailure::LinuxPolicyCheckUnavailable)
        ));
    }
    #[test]
    fn process_observations_pin_their_context_and_refuse_substitution() {
        let observed = collect(1000, &Peer(vec![facts(17, HOST, 0)])).unwrap();
        assert!(
            observed
                .matches_context(&observed.procfs, &observed.user_namespace)
                .unwrap()
        );
        let substituted = tempfile::tempfile().unwrap();
        assert!(
            !observed
                .matches_context(&substituted, &observed.user_namespace)
                .unwrap()
        );
        assert!(
            !observed
                .matches_context(&observed.procfs, &substituted)
                .unwrap()
        );
        assert_eq!(format!("{observed:?}"), "ProcessPolicy(..)");
    }
    #[test]
    fn collected_stale_groups_and_capabilities_feed_the_support_policy_evaluator() {
        use super::super::{PolicySnapshot, SudoDialect, evaluate};
        let observed = collect(1000, &Peer(vec![facts(17, HOST, 1 << 19)])).unwrap();
        let mut snapshot = PolicySnapshot {
            sudo_listing: include_str!(
                "../../../../../tests/server_native/linux/policy_fixtures/sudo-1.9.9-ubuntu-22.04-password.listing"
            ),
            sudo_dialect: SudoDialect::V1_9,
            account_groups: &[1000],
            process_groups: observed.groups(),
            docker_gid: Some(999),
            lxd_gid: None,
            container_broker_access: false,
            raw_storage_access: false,
            initial_namespace_capabilities: observed.capabilities_in_observer_namespace(),
            delegated_dac: false,
            delegated_tracing: false,
            delegated_profiling: false,
            perf_event_paranoid: 2,
            perf_denial_proven: true,
            authorization_policy_supported: true,
            privileged_brokers_supported: true,
            rights_removed_since_boot: false,
            complete: true,
            password_authenticated_cache: true,
        };
        assert_eq!(
            evaluate(&snapshot),
            Err(NativeFailure::LinuxDockerMembership)
        );
        snapshot.docker_gid = None;
        assert_eq!(
            evaluate(&snapshot),
            Err(NativeFailure::LinuxDelegatedTracing)
        );
        // This exercises policy integration through an injected native boundary;
        // it never constructs custody or authenticates the remaining facts.
    }
}
