# Protect Linux server credentials under native DAC custody

Linux server installations use root-owned native publication under stock AppArmor and apport. The server runs as the owning account from a main executable with mode `0501`, without set-ID bits. It sets and verifies dumpable 0 before credential delivery. The launcher and credential retriever are short-lived; no privileged SpaceTerm listener remains.

The launcher, manager, credential-free spawner, credential tool, dependencies, providers, configuration, resources, and their ancestors must be protected against account writes. Publication creates fresh destination inodes, authenticates their exact bytes, closes staging writers and mappings, and retains the execution bindings through use. Changing ownership or renaming an account-created inode does not establish custody.

Mount evidence is bound to retained descriptors. A trusted publication policy must exclude writable aliases, including views not visible in the selected mount namespace. The supported baseline remains glibc 2.35, systemd 249, and Linux 5.15. Layouts that cannot supply the required mount evidence or protected host-storage binding are refused rather than admitted by pathname, ownership, or an uncontextualized mount query.

Install, startup, credential release, and activation refuse accounts with effective passwordless sudo grants, including restricted grants; docker or lxd membership; rootful container-broker access; raw-device access; or delegated DAC, tracing, or profiling bypass authority. Persistent passwordless polkit grants also refuse admission. Unsupported relevant authorization or broker policy is refused. Unavailable or incomplete policy checks are not success. SpaceTerm does not edit host policy to make an account admissible.

A transient sudo or polkit cache created by password authentication is not a refusal. While that cache is usable, owning-account code may be able to become root and read server memory. SpaceTerm's own scoped sudo management exchange uses `sudo -k` and leaves no reusable timestamp. It does not clear another application's caches.

A reboot may be required once after removal of bypass rights when provisioning cannot exclude surviving handles. This is not a routine startup requirement. Stop, uninstall, and authorized recovery remain available when the account no longer satisfies admission policy.

Linux account incarnation is the home directory object's inode number and statx birth time, observed by root underneath stacked mounts. Reassigning a retained home to the same UID also transfers that server registration. An administrator must retire the registration before such reassignment. Cross-filesystem home moves and backup restoration require explicit account repair or reset. Network and automounted homes are unsupported in this phase.

## Authenticated provisioning

The first privileged execution requires the native publisher package from an administrator-configured trusted APT source. APT authenticates repository metadata and package hashes before the installed code executes. A downloaded archive, a helper's self-check, or client-side verification alone cannot establish this prerequisite. Missing trusted publisher provisioning produces `BootstrapTrustUnavailable` before account Install or device enrollment.

The package builder in `packaging/server/linux/publisher/build.py` constructs the native prerequisite without maintainer scripts, service installation, or credential creation. Release code lives under `/usr/lib/spaceterm-native-publisher`; Preflight code uses `/usr/lib/spaceterm-native-publisher-preflight`. Account publication and privileged invocation must still validate these objects and all ancestors through native custody. Package construction does not authorize execution or adopt its staging inodes as an account installation.

The pinned credential tool and licensed dependency closure are separate from the host service manager. No packaged PID 1 or systemctl replaces systemd 249. Native startup decrypts directly and does not depend on `LoadCredentialEncrypted`. Credential-tool rebuilds invalidate runtime evidence. A separately named unrun evidence candidate is not production approval; the production source gate remains closed until the required native evidence passes.

## Preserved behavior

The account and application identity still select one server under [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md). [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) still controls boot, logout independence, and deliberate Stopped state. [ADR 0034](0034-confirm-server-update-restarts-and-explain-incompatibility.md) and [ADR 0040](0040-keep-one-server-runtime-version-active.md) still control staging, confirmation, activation, and stopped updates. Custody refusal never selects a login-scoped fallback or overrides Stopped.
