# Distribute tagged releases with signed updates

macOS distribution uses ad hoc signatures and an independent Ed25519 update key because it must
incur no Apple membership fees. Browser downloads therefore require a Gatekeeper exception, and
update authenticity depends on retaining the private update key. The application owns restart
authorization while Sparkle verifies and installs updates.

Release age governs fresh-launch admission and reminders. Admission fails open when an update
cannot be obtained so offline terminal access remains available; an existing Workspace retains
access regardless of release age.
