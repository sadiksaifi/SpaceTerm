//! Bounded support-policy evaluation. Comparison observations never issue custody.

use crate::native_deployment::contracts::NativeFailure;

/// Verbose sudo listing dialects covered by the supported distribution baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SudoDialect {
    /// Ubuntu 22.04 and Debian 12 sudo 1.9.9 through 1.9.13.
    V1_9,
    /// Ubuntu 24.04 and Debian 13 sudo 1.9.15 through 1.9.16.
    V1_9Recent,
}

/// Bounded native comparison facts, not accepted provisioning or a policy lease.
/// A caller may evaluate these facts but cannot construct `NativeLinuxCustody`.
pub struct PolicySnapshot<'a> {
    /// Native root's effective verbose listing, collected with fixed arguments.
    pub sudo_listing: &'a str,
    /// Authenticated installed sudo's supported dialect.
    pub sudo_dialect: SudoDialect,
    /// Account's primary and supplementary GIDs from native lookup.
    pub account_groups: &'a [u32],
    /// Existing account processes' stale supplementary GIDs.
    pub process_groups: &'a [u32],
    /// Native resolution of the docker group, independent of its name in processes.
    pub docker_gid: Option<u32>,
    /// Native resolution of the lxd group.
    pub lxd_gid: Option<u32>,
    /// Kernel-confirmed account access to a rootful container control endpoint.
    pub container_broker_access: bool,
    /// Kernel ACL evaluation found account read or write access to raw host storage.
    pub raw_storage_access: bool,
    /// Effective host-initial-user-namespace capabilities, never child-namespace caps.
    pub initial_namespace_capabilities: u64,
    /// Independent admitted inventory found a DAC/ownership/UID/mount bypass helper.
    pub delegated_dac: bool,
    /// Relevant account-authorizing tracing helper or tracefs/debugfs access.
    pub delegated_tracing: bool,
    /// Relevant profiling helper, BPF delegation, or perf access.
    pub delegated_profiling: bool,
    /// Native kernel.perf_event_paranoid observation.
    pub perf_event_paranoid: i32,
    /// Sacrificial native perf probes confirmed denial under the admitted account.
    pub perf_denial_proven: bool,
    /// All custody-relevant authorization rules use reviewed supported templates.
    pub authorization_policy_supported: bool,
    /// All custody-relevant privileged broker grants are covered by provisioning.
    pub privileged_brokers_supported: bool,
    /// Actual rights removal since this boot leaves surviving-handle history unproven.
    pub rights_removed_since_boot: bool,
    /// Native bounds and reads completed. Partial snapshots are never positive.
    pub complete: bool,
    /// Password-authenticated transient caches are positive controls, not refusals.
    pub password_authenticated_cache: bool,
}

impl std::fmt::Debug for PolicySnapshot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PolicySnapshot(..)")
    }
}

/// Evaluates comparison observations. Success does not attest their native provenance.
pub fn evaluate(snapshot: &PolicySnapshot<'_>) -> Result<(), NativeFailure> {
    if !snapshot.complete
        || snapshot.account_groups.len() > 4096
        || snapshot.process_groups.len() > 16_384
    {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    check_sudo(snapshot.sudo_listing, snapshot.sudo_dialect)?;
    if !snapshot.authorization_policy_supported {
        return Err(NativeFailure::LinuxUnsupportedAuthorizationPolicy);
    }
    for (group, refusal) in [
        (snapshot.docker_gid, NativeFailure::LinuxDockerMembership),
        (snapshot.lxd_gid, NativeFailure::LinuxLxdMembership),
    ] {
        if group.is_some_and(|gid| {
            snapshot.account_groups.contains(&gid) || snapshot.process_groups.contains(&gid)
        }) {
            return Err(refusal);
        }
    }
    if snapshot.container_broker_access {
        return Err(NativeFailure::LinuxContainerBrokerAccess);
    }
    if snapshot.raw_storage_access {
        return Err(NativeFailure::LinuxRawDiskAccess);
    }
    let capabilities = snapshot.initial_namespace_capabilities;
    // Host rights only. Capabilities in a child user namespace are not these bits.
    const DAC: u64 = (1 << 0)
        | (1 << 1)
        | (1 << 2)
        | (1 << 3)
        | (1 << 6)
        | (1 << 7)
        | (1 << 8)
        | (1 << 16)
        | (1 << 17)
        | (1 << 21);
    if snapshot.delegated_dac || capabilities & DAC != 0 {
        return Err(NativeFailure::LinuxDelegatedDacAuthority);
    }
    if snapshot.delegated_tracing || capabilities & (1 << 19) != 0 {
        return Err(NativeFailure::LinuxDelegatedTracing);
    }
    if snapshot.delegated_profiling || capabilities & ((1 << 38) | (1 << 39)) != 0 {
        return Err(NativeFailure::LinuxDelegatedProfiling);
    }
    if snapshot.perf_event_paranoid < 1 {
        return Err(NativeFailure::LinuxSystemWideProfiling);
    }
    if !snapshot.perf_denial_proven {
        return Err(NativeFailure::LinuxPolicyCheckUnavailable);
    }
    if !snapshot.privileged_brokers_supported {
        return Err(NativeFailure::LinuxUnsupportedPrivilegedBroker);
    }
    if snapshot.rights_removed_since_boot {
        return Err(NativeFailure::LinuxPriorBypassHistoryUnproven);
    }
    // A password-authenticated sudo/polkit cache is intentionally not inspected
    // here. The owner accepted that transient exposure; value 47 stays unused.
    Ok(())
}

#[derive(Default)]
struct SudoEntry {
    run_as: bool,
    options: Option<bool>,
    commands: bool,
    grants: bool,
}
impl SudoEntry {
    fn finish(&self, default_authentication: bool) -> Result<bool, NativeFailure> {
        if !self.run_as || !self.commands || !self.grants {
            return Err(NativeFailure::LinuxPolicyCheckUnavailable);
        }
        Ok(!self.options.unwrap_or(default_authentication))
    }
}
fn check_sudo(text: &str, dialect: SudoDialect) -> Result<(), NativeFailure> {
    let unavailable = NativeFailure::LinuxPolicyCheckUnavailable;
    if text.is_empty()
        || text.len() > 2 * 1024 * 1024
        || !text.ends_with('\n')
        || text.contains('\0')
    {
        return Err(unavailable);
    }
    // Both supported verbose dialects have the same record grammar. A native
    // collector must authenticate the installed version before selecting either.
    match dialect {
        SudoDialect::V1_9 | SudoDialect::V1_9Recent => {}
    }
    let mut entry: Option<SudoEntry> = None;
    let mut entries = 0;
    let mut defaults = false;
    let mut header = false;
    let mut authentication = true;
    let mut passwordless = false;
    for line in text.lines() {
        if line.len() > 16 * 1024 {
            return Err(unavailable);
        }
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Matching Defaults entries for ")
            && line.ends_with(':')
            && entry.is_none()
            && !header
        {
            defaults = true;
        } else if line.starts_with("User ")
            && line.contains(" may run the following commands on ")
            && line.ends_with(':')
            && entry.is_none()
            && !header
        {
            header = true;
            defaults = false;
        } else if line == "Sudoers entry:" && header {
            if let Some(previous) = entry.take() {
                passwordless |= previous.finish(authentication)?;
            }
            entries += 1;
            if entries > 512 {
                return Err(NativeFailure::LimitExceeded);
            }
            entry = Some(SudoEntry::default());
        } else if line.starts_with("    ") {
            let value = line.trim();
            if let Some(entry) = &mut entry {
                if let Some(users) = value.strip_prefix("RunAsUsers: ") {
                    if entry.run_as || entry.commands || users.is_empty() {
                        return Err(unavailable);
                    }
                    entry.run_as = true;
                } else if value.starts_with("RunAsGroups: ") && !entry.commands {
                    if value == "RunAsGroups: " {
                        return Err(unavailable);
                    }
                } else if let Some(options) = value.strip_prefix("Options: ") {
                    if entry.options.is_some() || entry.commands {
                        return Err(unavailable);
                    }
                    let mut required = authentication;
                    for option in options.split(',').map(str::trim) {
                        match option {
                            "authenticate" => required = true,
                            "!authenticate" => required = false,
                            "setenv" | "!setenv" | "noexec" | "!noexec" | "log_input"
                            | "!log_input" | "log_output" | "!log_output" | "intercept"
                            | "!intercept" => {}
                            _ => return Err(unavailable),
                        }
                    }
                    entry.options = Some(required);
                } else if value == "Commands:" && !entry.commands {
                    entry.commands = true;
                } else if entry.commands && line.starts_with("        ") && !value.is_empty() {
                    entry.grants |= !value.starts_with('!');
                } else {
                    return Err(unavailable);
                }
            } else if defaults {
                for option in value.split(',').map(str::trim) {
                    match option {
                        "authenticate" => authentication = true,
                        "!authenticate" => authentication = false,
                        // Exempt-group and unfamiliar authentication plugins need
                        // native effective-policy support; do not infer admission.
                        option
                            if option.starts_with("exempt_group")
                                || option.starts_with("!exempt_group") =>
                        {
                            return Err(unavailable);
                        }
                        "env_reset" | "!env_reset" | "mail_badpass" | "use_pty" | "!use_pty"
                        | "lecture" | "!lecture" | "rootpw" | "targetpw" | "runaspw"
                        | "!rootpw" | "!targetpw" | "!runaspw" => {}
                        option
                            if [
                                "secure_path=",
                                "timestamp_timeout=",
                                "passwd_timeout=",
                                "env_keep+=",
                                "env_keep=",
                                "env_check=",
                                "env_delete=",
                                "lecture=",
                                "timestamp_type=",
                            ]
                            .iter()
                            .any(|prefix| option.starts_with(prefix)) => {}
                        _ => return Err(unavailable),
                    }
                }
            } else {
                return Err(unavailable);
            }
        } else {
            return Err(unavailable);
        }
    }
    passwordless |= entry.ok_or(unavailable)?.finish(authentication)?;
    if passwordless {
        Err(NativeFailure::LinuxPasswordlessSudo)
    } else {
        Ok(())
    }
}
