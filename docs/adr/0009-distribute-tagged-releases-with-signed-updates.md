# Distribute tagged releases with signed updates

macOS releases use ad hoc signatures and an independent Ed25519 update key because distribution must incur no Apple membership fees. Browser downloads therefore need a Gatekeeper exception, and update authenticity depends on keeping the private key secret. Update admission fails open so offline terminal access remains available.
