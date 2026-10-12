use spaceterm_host::{
    linux_custody::support_policy::{PolicySnapshot, SudoDialect, evaluate},
    native_deployment::contracts::NativeFailure,
};

const AUTHENTICATED: &str = "Matching Defaults entries for account on host:\n    env_reset, authenticate\n\nUser account may run the following commands on host:\n\nSudoers entry:\n    RunAsUsers: ALL\n    RunAsGroups: ALL\n    Options: authenticate\n    Commands:\n        ALL\n";
const NOPASSWD: &str = "Matching Defaults entries for account on host:\n    env_reset\n\nUser account may run the following commands on host:\n\nSudoers entry:\n    RunAsUsers: root\n    Options: !authenticate\n    Commands:\n        /usr/bin/systemctl restart nginx\n";
fn admitted() -> PolicySnapshot<'static> {
    PolicySnapshot {
        sudo_listing: AUTHENTICATED,
        sudo_dialect: SudoDialect::V1_9,
        account_groups: &[1000, 27],
        process_groups: &[1000, 27],
        docker_gid: Some(999),
        lxd_gid: Some(998),
        container_broker_access: false,
        raw_storage_access: false,
        initial_namespace_capabilities: 0,
        delegated_dac: false,
        delegated_tracing: false,
        delegated_profiling: false,
        perf_event_paranoid: 2,
        perf_denial_proven: true,
        authorization_policy_supported: true,
        privileged_brokers_supported: true,
        rights_removed_since_boot: false,
        complete: true,
        password_authenticated_cache: false,
    }
}
#[test]
fn support_policy_refuses_nopasswd_but_allows_password_gated_sudo_membership() {
    assert_eq!(evaluate(&admitted()), Ok(()));
    let mut snapshot = admitted();
    snapshot.sudo_listing = NOPASSWD;
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxPasswordlessSudo)
    );
}
#[test]
fn password_authenticated_transient_caches_are_positive_controls() {
    let mut snapshot = admitted();
    snapshot.password_authenticated_cache = true;
    assert_eq!(evaluate(&snapshot), Ok(()));
}
#[test]
fn support_policy_uses_gids_and_detects_stale_container_groups() {
    let mut snapshot = admitted();
    snapshot.process_groups = &[999];
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxDockerMembership)
    );
    snapshot.process_groups = &[998];
    assert_eq!(evaluate(&snapshot), Err(NativeFailure::LinuxLxdMembership));
}
#[test]
fn support_policy_detects_raw_device_acl_and_capability_bypasses() {
    let mut snapshot = admitted();
    snapshot.raw_storage_access = true;
    assert_eq!(evaluate(&snapshot), Err(NativeFailure::LinuxRawDiskAccess));
    snapshot.raw_storage_access = false;
    snapshot.initial_namespace_capabilities = 1 << 1; // CAP_DAC_OVERRIDE
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxDelegatedDacAuthority)
    );
    snapshot.initial_namespace_capabilities = 1 << 19; // CAP_SYS_PTRACE
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxDelegatedTracing)
    );
    snapshot.initial_namespace_capabilities = 1 << 38; // CAP_PERFMON
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxDelegatedProfiling)
    );
}
#[test]
fn unsupported_relevant_broker_is_not_silently_admitted() {
    let mut snapshot = admitted();
    snapshot.privileged_brokers_supported = false;
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxUnsupportedPrivilegedBroker)
    );
}
#[test]
fn truncated_failed_and_unsupported_sudo_listings_are_unavailable() {
    for listing in [
        "",
        "Sudoers entry:\n    Options: !authenticate\n",
        "Sudoers entry:\n    RunAsUsers: ALL\n    Options: unknown\n    Commands:\n        ALL\n",
    ] {
        let mut snapshot = admitted();
        snapshot.sudo_listing = listing;
        assert_eq!(
            evaluate(&snapshot),
            Err(NativeFailure::LinuxPolicyCheckUnavailable)
        );
    }
}
#[test]
fn rights_removal_requires_reboot_only_for_an_actual_unproven_transition() {
    let mut snapshot = admitted();
    assert_eq!(evaluate(&snapshot), Ok(()));
    snapshot.rights_removed_since_boot = true;
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxPriorBypassHistoryUnproven)
    );
}
#[test]
fn partial_policy_and_unproven_native_perf_probes_never_admit() {
    let mut snapshot = admitted();
    snapshot.complete = false;
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxPolicyCheckUnavailable)
    );
    snapshot.complete = true;
    snapshot.perf_denial_proven = false;
    assert_eq!(
        evaluate(&snapshot),
        Err(NativeFailure::LinuxPolicyCheckUnavailable)
    );
}
