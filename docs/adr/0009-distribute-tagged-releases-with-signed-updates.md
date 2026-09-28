# Distribute tagged releases with signed updates

Annotated stable `v<version>` Git tags are SpaceTerm's only release version authority. Builds derive application identity, bundle metadata, artifact names, and update metadata from the validated tag. Development builds identify their commit and cannot update a release installation.

GitHub Releases hosts the complete Apple Silicon DMG and signed Sparkle feed. SpaceTerm uses ad hoc macOS signatures and an independent Ed25519 update key because distribution must incur no Apple membership fees. First installation therefore requires the user's Gatekeeper exception; update authenticity depends on retaining the private update key. The application owns update presentation and restart authorization, while a narrow macOS adapter lets Sparkle verify, stage, replace, and relaunch the bundle. Ordinary quit cancels an unconfirmed staged installer before termination.
