# Distribute tagged releases with signed updates

This release and update policy applies to macOS. Linux currently supports source builds only, with no distribution or updates.

Annotated stable `v<version>` Git tags are SpaceTerm's only release version authority. Builds derive application identity, bundle metadata, artifact names, and update metadata from the validated tag. Development builds identify their commit and cannot update a release installation.

GitHub Releases hosts the complete Apple Silicon DMG and signed Sparkle feed. SpaceTerm uses ad hoc macOS signatures and an independent Ed25519 update key because distribution must incur no Apple membership fees. First installation therefore requires the user's Gatekeeper exception; update authenticity depends on retaining the private update key. The application owns update presentation and restart authorization, while a narrow macOS adapter lets Sparkle verify, stage, replace, and relaunch the bundle. Ordinary quit may finish an already verified installer without requesting termination or relaunch; restarting a running application requires confirmation.

Release age controls reminders and fresh-launch admission, never access to an existing Workspace.
A fresh launch checks before creating Terminal Sessions and fails open if obtaining an update is
impossible or stalls. This keeps offline terminal access available. Signed feed publication dates
start the deadline; retained observations preserve the oldest outstanding deadline and reminder
cadence. Transport and history persistence remain injected capabilities, while Settings owns user
preferences. Startup installation authorization expires as soon as Workspace access is released.
