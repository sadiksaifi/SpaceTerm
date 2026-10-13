# Bounded Jammy evidence sequence

This is the coordinator's sequence for one owned, disposable stock Ubuntu 22.04 x86_64 guest. It is not a command to mutate penguin. Mac candidate reference: `@PAIR_DIGEST@`. Linux evidence must bind the separate authenticated Linux package and complete credential closure, not that Mac digest.

## Admission and authorization checkpoint

Use the existing ownership-receipt VM executor. The guest limit is two CPUs, 4 GiB RAM, a 24 GiB OS disk and at most 8 GiB encrypted test storage. The outer runner remains bounded to two CPUs and 6 GiB RAM including swap. No disk has a backing file or external data file. Do not use a spare-Mac route or an unguarded QEMU/Docker host script.

Before provisioning, the coordinator verifies the retained guest image, disk/seed identities, exact candidate revision and Linux package signatures through the executor. Preserve AppArmor and apport. Keep penguin's OS, systemd, authorization policy, groups and storage unchanged. If the execution-gated executor or authenticated publisher is unavailable, stop here and record the prerequisite as unrun.

The orchestration account may have guest administrator rights. It must be different from the admitted server account. The admitted account has no standing passwordless sudo/polkit, Docker/LXD, raw-disk, delegated tracing or profiling access. Ordinary password-gated sudo and transient password-authenticated caches are positive controls, not refusals. Do not modify host authorization to make a suite pass.

Use four separate new input directories: downloaded package, detached signature, retained trust receipt and trusted repository scripts. Python readers run with `-I` from the trusted checkout. Nothing extracted from the package becomes an interpreter working directory or a privileged script. The guest install path must authenticate fresh protected destination copies, their dependencies/configuration and every ancestor before root execution. It must retain descriptor-bound mount evidence and a publisher policy excluding writable aliases. Refuse unsupported 5.15 layouts rather than raising the glibc 2.35/systemd 249 baseline.

## Install and collect the custody behaviors

Connect to the same receipt-bound guest. Run the following only inside it, from the coordinator-synchronized trusted checkout. The three absolute input operands are the retained locations chosen during preparation.

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
umask 077
for body in custody_install custody_verify custody_lifecycle custody_manage custody_reboot custody_boot_check custody_cleanup; do
  test -f "mise-tasks/server/vm/linux/$body.py" || exit 2
done
run_custody server:vm:custody:install:linux --disposable-jammy \
  --artifact /absolute/package-input/server-package \
  --signature /absolute/signature-input/server-package.sig \
  --trust-receipt /absolute/authority-input/native-trust.receipt
run_custody server:vm:custody:verify:linux handoff
run_custody server:vm:custody:verify:linux memory
run_custody server:vm:custody:verify:linux aliases
run_custody server:vm:custody:verify:linux publication
run_custody server:vm:custody:verify:linux mount-evidence
run_custody server:vm:custody:verify:linux support-policy
run_custody server:vm:custody:verify:linux packaged-execution
run_custody server:vm:custody:verify:linux durability
run_custody server:vm:custody:lifecycle:linux
)
```

Each block runs in a subshell. Its guard stops at the first nonzero task result, preserves that exit status and leaves the original executor receipt and evidence untouched. Do not run the next block after a failure. Stop at the cleanup and recovery checkpoint on a refusal, missing record, nonworking negative control or cleanup uncertainty. A task name is not a runnable implementation. In the current integration snapshot these custody bodies and the complete pinned credential-tool closure are missing. Do not run the earlier credential-free Linux prototype, SELinux experiment or apport-masking scripts as replacements.

Each driver keeps its bounded structured output in the same executor-owned evidence state. Keep that state outside the guest before any reboot or abrupt-power-off case. Bind every record to the exact revision, package/closure digest, guest baseline and native observed operation. Record Unrun, denial and uncertainty separately. Never log plaintext credentials, memory samples, environment values, native item references, terminal/clipboard contents or raw command output.

The drivers must cover these behaviors with actual positive and negative controls:

- Exact startup, unreadable approved ELF, pre-secret dumpable 0 set/readback, malformed readiness and zero release on every pre-release failure. AppArmor and apport remain enabled; a piped core handler is not bypassed with `LimitCORE=0`.
- Working readable-memory controls followed by denied ptrace, `process_vm_readv`, proc-memory/environment/descriptor and perf access. Include pre-opened handles and crashes of the server, retriever and tool. Temporary Yama changes are guest-scoped and restored on every exit. No secret-bearing core or apport report is accepted.
- Namespace, bind/idmapped/detached/overlay aliases, writable-map and old-writer controls. A `chown` or `chmod` adoption control must still demonstrate its old writer. Publication must create new protected inodes and reject account-backed storage and publisher/broker exposures, not rely on an alias-absence scan.
- Descriptor-bound original-mount evidence and the actual required native witness on 5.15, with lost/mismatched originals, idmapped views, missing creation provenance and unsupported storage refused. Preserve native attempt and entry bounds.
- Standing passwordless grants, daemon/socket ACL access, stale groups, raw-device grants and delegated tracing/profiling refused with typed remedies. Unknown relevant policy is unavailable. Preserve ordinary password-gated sudo and do not automatically clear password caches.
- Exact launcher/server/spawner/tool/interpreter/library/provider/configuration closure on glibc 2.35 and systemd 249, without build-host libraries, display, desktop or source checkout. Include Host and HostAndTpm2, wrong mode/machine, missing Host key and no startup provisioning. Missing valid TPM evidence is Unrun, never a downgrade or Passed.
- One credential-free pre-secret spawner for Terminal and tool children, ordinary nonroot groups/sudo, no inherited credential channels or memory, and exact status/signal/reaping behavior. NoNewPrivs and server guards do not propagate to terminals.
- Both Stop/release orders, approval/head crash boundaries, uncertainty recovery, Stage then crash, stopped activation, old Running replay and ancestor replacement. Locked/unmounted storage waits without underlay creation or replacement identity, then resumes against the original registered roots after unlock.

## Running across reboot

Authorization checkpoint: the coordinator explicitly authorizes reboot of this exact owned disposable guest. Reboot is a separate step, not part of a background controller loop.

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
run_custody server:vm:custody:manage:linux start
run_custody server:vm:custody:reboot:linux --disposable-jammy
)
```

Reconnect to the same guest using its original executor receipt. Do not provision another VM or reinstall the package. Keep all prior records. Observe boot before account login, fresh approved startup and native identity retention. Then run:

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
run_custody server:vm:custody:boot-check:linux running
run_custody server:vm:custody:manage:linux stop
)
```

Observe durable Stopped and release fencing before supervision effects, settled started Git writes, exact previous process/Session/retriever retirement and endpoint absence. Explicitly authorize the second guest reboot:

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
run_custody server:vm:custody:reboot:linux --disposable-jammy
)
```

Reconnect through the original receipt. Do not restart from Install. Observe Stopped after boot and client launch:

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
run_custody server:vm:custody:boot-check:linux stopped
)
```

The lifecycle driver separately tests logout survival and explicit Start. A cached service-status record or a successful reboot command is not that evidence. No GUI or physical-hardware case is credited from this headless guest.

## Cleanup and recovery checkpoint

First collect and validate all bounded records into the executor-owned state outside the guest. Request fixed inventory-bound guest cleanup:

```sh
(
run_custody() {
  mise run "$@" || {
    status=$?
    printf '%s\n' 'classification=recovery-required; keep the original executor receipt and evidence; stop at the cleanup and recovery checkpoint' >&2
    exit "$status"
  }
}
run_custody server:vm:custody:cleanup:linux --disposable-jammy
)
```

The native cleanup operation drains work and removes only retained exact resources after proving prior-instance absence. It preserves replaced objects and uncertain inventory. It does not reset global authorization, AppArmor, apport or host policy.

Only after collection and guest cleanup have known outcomes may the ownership-receipt executor stop and remove its exact outer container, disks, seed and private access material. It must revalidate the original container and file identities through removal. Do not use host `rm -rf`, delete all containers or follow a pathname from guest output.

On interrupted provisioning, power-off, lost output, identity-publication failure or uncertain cleanup, retain the original receipt and candidate. Stop further effects. Have the coordinator review the executor's recorded native identities, then resume its exact recovery/cleanup operation. If that operation is unavailable, preserve the scope for reviewed manual recovery. A recreated VM or a second successful fsync does not resolve the original uncertainty.
