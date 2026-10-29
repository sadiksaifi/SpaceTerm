# Install and pair servers from their own command line

The primary remote setup path runs on the server's own machine: `install.sh --headless`, `spaceterm server setup`, then `spaceterm server pair`. Another device joins the Invitation Link through the desktop's Add Server or `spaceterm connection add`. [ADR 0029](0029-enroll-remote-servers-through-add-server.md) owns enrollment trust and the optional desktop SSH Setup method.

Setup installs the service from the launcher's installation, obtains Linux linger authorization when needed, starts the server unless deliberately stopped, and sets Network Access. It is idempotent and changes only preferences its flags name. Without a terminal, Network Access must be explicit and linger authorization must already exist. [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md) owns startup policy and the Mac GUI-session requirement. The installer also owns uninstallation; server help supplies its invocation.

`server pair` prints only the Invitation Link on stdout and waits for its exact claim. The person types the Comparison Code displayed on the joining device. A wrong code rejects the claim. EOF, interruption, expiry or failure withdraws that invitation. JSON, no-input mode or absence of a terminal fails before creating an invitation.

`connection add` reads the link from stdin or a hidden prompt, never from arguments. Its address override preserves the server pin. CLI Saved Connections and credentials stay separate from the desktop's. Connection removal removes the local record immediately; pending self-revocation is retried only by later connection commands. [ADR 0038](0038-unpair-devices-without-blocking-local-connection-removal.md) owns exact revocation and enrollment cleanup.

Pairing credentials use Keychain on macOS or Secret Service on Linux through a shared native-store interface. An unavailable store refuses remote client use; it never creates a plaintext fallback under [ADR 0004](0004-resolve-application-directories-by-xdg-convention.md). Server-role and local work commands need no client credential store.
