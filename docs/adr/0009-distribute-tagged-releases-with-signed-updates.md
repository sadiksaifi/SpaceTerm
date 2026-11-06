# Distribute tagged releases with signed updates

A `v*` tag publishes one GitHub release. [The release workflow](../../.github/workflows/release.yml) first validates the tagged commit on macOS and Linux. Only after both pass does it build SpaceTerm Core once per target, Apple Silicon macOS and x86_64 Linux, and sign each build once. Each desktop app embeds both Core builds, so a desktop can install SpaceTerm Core on a Machine of either platform. Jobs that hold a signing secret run in the protected `release` environment, which requires a reviewer's approval. [The release module](../../mise-tasks/lib/spaceterm_tasks/release.py) owns the steps and [the feed module](../../mise-tasks/lib/spaceterm_tasks/release_feed.py) the asset names.

## macOS code signing

macOS releases are signed with a Developer ID Application certificate under the hardened runtime, notarized with an App Store Connect API key, and stapled. The SpaceTerm Core bundle, the app and the disk image are each notarized. The app is stapled before the disk image is built, so an app copied out of the image carries its own ticket. Gatekeeper therefore opens a downloaded release without an exception.

The designated requirement names Apple's Developer ID anchor and the release Team ID, not one certificate. Every later version from that team satisfies it, including one signed by a renewed certificate, so System Permissions, Login Items and Keychain grants survive updates. [ADR 0061](0061-own-system-permissions-in-the-server-package.md) applies this to the SpaceTerm Core bundle, and [the signing module](../../mise-tasks/lib/spaceterm_tasks/macos_codesign.py) owns the requirement. The release workflow supplies the Team ID and the certificate's name as variables. The Release SpaceTerm Core compiles the Team ID in and accepts a retained runtime only if it satisfies the same requirement.

Release jobs import the certificate and its private key from the `release` environment's secrets into a temporary Keychain and delete it afterward. [The notarization module](../../mise-tasks/lib/spaceterm_tasks/macos_notary.py) writes the API key to a private temporary file for `notarytool` only. Encrypted credentials in the repository are personal recovery backups and do not participate in releases.

## Update signatures

Ed25519 signatures authenticate every update on macOS and Linux: each Core archive, the Linux desktop archive, each update feed and the Sparkle appcast. [The update trust](../../packaging/update-trust.json) holds two public keys. The `current` key's private key is a `release` secret and signs every release. The `backup` key's private key stays offline and never enters GitHub. SpaceTerm Core, the Linux desktop and [install.sh](../../packaging/install.sh) accept a signature under either key. A release signed with the backup key therefore replaces a lost or exposed current key for them without breaking installed copies. Sparkle trusts one key, so the macOS desktop's `SUPublicEDKey` pins only `current`. A macOS desktop recovers in one of two ways. While the old current key is still held, a release signed with it ships the new `SUPublicEDKey`. After that key is lost, the person reinstalls the desktop with install.sh.

install.sh verifies each download before it runs or installs it: on macOS against the Developer ID requirement, on Linux against the update keys. The release fills in its Team ID when it publishes the script. install.sh and in-app updates are the only distribution channels.

## Desktop Core resources

Desktop compilation requires no signed Core archive. It compiles the exact source commit and snapshot digest, product identity, version, protocol versions, both Update Floors, public update trust and Apple Team ID. Packaging authenticates both finalized native Core packages and signs a release-specific resource manifest with the current update key. The manifest binds those compiled inputs to each target's exact archive digest and raw signature. Both desktops ship the same finalized archive bytes. The macOS app signature seals the manifest after packaging inserts it.

Before a packaged desktop admits an embedded Core for local installation, SSH Setup or an update push, it authenticates the manifest under compiled public trust and checks every compiled build input exactly. It then verifies the selected archive's digest and signature against that manifest. A missing, unsigned, changed or substituted manifest grants no authority. An archive signed by a trusted key but absent from this release's manifest grants no authority. Native Core admission and retained immutable installation verification still apply under ADRs 0040 and 0061. Build-info reports candidate provenance only after this verification succeeds.

[The manifest module](../../mise-tasks/lib/spaceterm_tasks/desktop_manifest.py) owns packaging. [The portable verifier](../../crates/spaceterm-core/src/update/resource_manifest.rs) owns the signed schema. A dry run passes its generated public trust as a compile-time input to Core and desktop, so its manifest and archives authorize nothing in a production build. Source Development launches retain their explicit local artifact authority under ADR 0054 and have no Release authority.

## Dry runs

Pull requests that touch the release path, and `mise run release:dry-run` on a developer's computer, run the same steps as a dry run. A dry run signs with a generated update key that nothing else trusts, signs code ad hoc, skips notarization with a log line, and publishes nothing. Its install.sh is marked as a dry run, accepts ad hoc signatures, and states this when it runs.

Update admission fails open, so offline terminal access remains available.
