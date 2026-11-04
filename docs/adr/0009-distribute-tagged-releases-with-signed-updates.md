# Distribute tagged releases with signed updates

A `v*` tag publishes one GitHub release. [The release workflow](../../.github/workflows/release.yml) first validates the tagged commit on macOS and Linux. Only after both pass does it build SpaceTerm Core once per target, Apple Silicon macOS and x86_64 Linux, and sign each build once. Each desktop app embeds both Core builds, so a desktop can install SpaceTerm Core on a Machine of either platform. Jobs that hold a signing secret run in the protected `release` environment, which requires a reviewer's approval. [The release module](../../mise-tasks/lib/spaceterm_tasks/release.py) owns the steps and [the feed module](../../mise-tasks/lib/spaceterm_tasks/release_feed.py) the asset names.

## macOS code signing

macOS releases are signed with a Developer ID Application certificate under the hardened runtime, notarized with an App Store Connect API key, and stapled. The SpaceTerm Core bundle, the app and the disk image are each notarized. The app is stapled before the disk image is built, so an app copied out of the image carries its own ticket. Gatekeeper therefore opens a downloaded release without an exception.

The designated requirement names Apple's Developer ID anchor and the release Team ID, not one certificate. Every later version from that team satisfies it, including one signed by a renewed certificate, so System Permissions, Login Items and Keychain grants survive updates. [ADR 0061](0061-own-system-permissions-in-the-server-package.md) applies this to the SpaceTerm Core bundle, and [the signing module](../../mise-tasks/lib/spaceterm_tasks/macos_codesign.py) owns the requirement. The release workflow supplies the Team ID and the certificate's name as variables. The Release SpaceTerm Core compiles the Team ID in and accepts a retained runtime only if it satisfies the same requirement.

Release jobs import the certificate and its private key from the `release` environment's secrets into a temporary Keychain and delete it afterward. [The notarization module](../../mise-tasks/lib/spaceterm_tasks/macos_notary.py) writes the API key to a private temporary file for `notarytool` only. Encrypted credentials in the repository are personal recovery backups and do not participate in releases.

## Update signatures

Ed25519 signatures authenticate every update on macOS and Linux: each Core archive, the Linux desktop archive, each update feed and the Sparkle appcast. [The update trust](../../packaging/update-trust.json) holds two public keys. The `current` key's private key is a `release` secret and signs every release. The `backup` key's private key stays offline and never enters GitHub. Every reader accepts a signature under either key, so a release signed with the backup key can replace a lost or exposed current key without breaking installed copies. The desktop's `SUPublicEDKey` and [install.sh](../../packaging/install.sh) pin the same keys.

install.sh verifies each download before it runs or installs it: on macOS against the Developer ID requirement, on Linux against the update keys. The release fills in its Team ID when it publishes the script. install.sh and in-app updates are the only distribution channels.

## Dry runs

Pull requests that touch the release path, and `mise run release:dry-run` on a developer's computer, run the same steps as a dry run. A dry run signs with a generated update key that nothing else trusts, signs code ad hoc, skips notarization with a log line, and publishes nothing. Its install.sh is marked as a dry run, accepts ad hoc signatures, and states this when it runs.

Update admission fails open, so offline terminal access remains available.
