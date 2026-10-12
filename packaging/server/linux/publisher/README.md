# Native publisher provisioning

The native publisher is an administrative prerequisite, not an SSH-downloaded installer. An administrator configures an APT source with a protected signing-key configuration. The repository authenticates its Release metadata, and Packages binds each exact Debian package by digest. APT verifies that chain before installing the package. Do not replace this route with `sudo` on a downloaded executable, local archive, or shell script. Do not use unauthenticated APT options.

`build.py` builds the package without installation, signing, or maintainer scripts. It uses Cargo-reported paths for the server, launcher, manager, and reusable credential-free spawner. It authenticates the pinned credential closure and copied bytes, verifies native linkage, queries the copied server's implemented protocol/store metadata, and includes the licensed sources and build material. A missing metadata composition refuses; the builder does not invent store compatibility.

Before assembly, run the real metadata prerequisite on either build host:

```sh
CARGO_BUILD_JOBS=3 mise exec -- python -I packaging/server/linux/publisher/tests.py
```

This builds the actual server and helpers without test features or metadata mocks. The server must answer the sole `--distribution-metadata` argument before native admission. Ordinary startup and a metadata query with extra arguments must still refuse while production admission is closed. Passing the injected packaging tests does not replace this check or successful candidate assembly.

Production construction requires passed Jammy, Host-key, and TPM evidence bound to the selected credential-tool construction. From the trusted repository, run:

```sh
mise exec -- python -I packaging/server/linux/publisher/build.py
```

A release uses `--release-tag` and the existing clean annotated-tag checks. The generated Debian package still needs publication through the configured authenticated APT source. The builder does not supply a repository signing identity or authorize root execution.

For the coordinator's unrun native evidence input, run:

```sh
mise exec -- python -I packaging/server/linux/publisher/build.py --evidence-candidate
```

This produces separately named Preflight archive, Debian package, and comparison receipt under `dist/`. The receipt labels runtime evidence `unrun` and binds the archive, manifest, publisher package, and credential closure. It is not an installation permit or production feed. Existing candidate outputs are refused so an interrupted or completed run cannot silently lose its original inputs. Review and preserve those inputs before making another candidate.

The Debian package contains no service unit, account registration, or credential. Its data tree has root ownership, an unreadable `0501` main server executable, and authenticated native helpers. It does not install a replacement service manager. Release uses `/usr/lib/spaceterm-native-publisher`; Preflight uses `/usr/lib/spaceterm-native-publisher-preflight`. Native management must retain and validate every object and ancestor before privileged execution. Account installation still publishes fresh protected inodes instead of adopting the package builder's staging objects.

SSH Bootstrap must detect the authenticated installed publisher before account Install or enrollment. Absence returns native result 52, `BootstrapTrustUnavailable`, with the fixed remedy from `spaceterm_host::native_refusals::remedy_for`. The client must not retry by elevating an account-writable helper. A satisfied prerequisite does not skip native account, support-policy, mount, or credential checks.
