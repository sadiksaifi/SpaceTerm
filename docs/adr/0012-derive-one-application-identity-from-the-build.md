# Derive one application identity from the build

SpaceTerm builds as one of three applications. Each has its own bundle identifier, application directories, icon, and distribution policy, so any combination can run side by side without sharing Settings, sessions, or privacy grants.

| Identity | Bundle identifier | Directory name | Updates | Microphone |
| --- | --- | --- | --- | --- |
| SpaceTerm | `io.github.sadiksaifi.spaceterm` | `spaceterm` | signed release feed | yes |
| SpaceTerm Preflight | `io.github.sadiksaifi.spaceterm-preflight` | `spaceterm-preflight` | none | yes |
| SpaceTerm Development | `io.github.sadiksaifi.spaceterm-development` | `spaceterm-development` | simulated | no |

The build selects the identity with one rule. The packaging script sets `SPACETERM_PACKAGED=1`, which leaves SpaceTerm Development. A packaged build with a validated release tag is SpaceTerm; see ADR 0009. A packaged build without a tag is SpaceTerm Preflight, an optimized build of a commit used to compare performance against the installed release. Every other build, including a bare `cargo run`, is SpaceTerm Development.

SpaceTerm Development is the default because a mistaken build must never write into a release installation's state. The build script rejects the unsafe combinations: a release tag without packaging, a release tag without the signed updater, and a packaged build with the `developer-tools` feature.

`ApplicationIdentity` owns the identity's names and distribution policy. Composition reads the policy from it rather than from Cargo features, so adding a policy means adding a field and a test, not another conditional. Developer tools remain a Cargo feature because they must be absent from packaged binaries, not merely hidden. They are the Developer Workbench and the Develop menu that opens it.

The `packaging/` directory owns everything that builds, identifies, signs, verifies, or publishes an application bundle. Each identity keeps its bundle template and icon together in `packaging/macos/<identity>/`, so adding or changing an identity touches one directory.

SpaceTerm Development has no microphone access because every build re-signs it ad hoc, and macOS would not keep the privacy grant across builds.

Every identity offers Screen Recording and Accessibility setup and recovery for terminal programs. macOS keys those grants to the bundle identifier and code signature, so each identity holds its own grants. SpaceTerm Development loses them on each build, which makes it the identity where stale-grant recovery is exercised. A permission reset names only the bundle identifier of the running identity, and only when the running bundle matches it.
