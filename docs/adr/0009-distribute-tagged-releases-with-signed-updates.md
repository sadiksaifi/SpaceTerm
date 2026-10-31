# Distribute tagged releases with signed updates

macOS releases reuse one self-signed code-signing certificate, pinned in the release Packager configuration, so rebuilt apps retain the same designated requirement for System Permissions without Apple membership fees. The first update from an ad hoc release changes that identity and may require users to grant permissions once more. The server package's bundle is signed with the same certificate under its own identifier, and it holds the System Permissions of Terminal Sessions under [ADR 0061](0061-own-system-permissions-in-the-server-package.md). Permission retention across certificate-signed server updates must be verified for microphone, Screen Recording, and Accessibility access on supported macOS versions.

An independent Ed25519 key authenticates updates on macOS and Linux. macOS release packaging imports the code-signing certificate and its private key from GitHub Actions Secrets into a temporary Keychain, rejects missing or unexpected signing material, and cleans up the import. Encrypted credentials in the repository are personal recovery backups and do not participate in releases.

Self-signing does not provide notarization. Browser downloads therefore need a Gatekeeper exception. Update admission fails open so offline terminal access remains available.
