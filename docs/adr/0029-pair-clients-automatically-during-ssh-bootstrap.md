# Pair clients automatically during SSH Bootstrap

Successful SSH Bootstrap pairs the initiating device with the SpaceTerm Server owned by the authenticated remote account, including when setup reuses an existing compatible server. Enrollment uses the authenticated SSH connection under the host-trust policy in [ADR 0003](0003-let-openssh-own-remote-authentication.md), so setup requires no separate pairing-code step.

On Linux, setup requires the authenticated native publisher and account custody policy in [ADR 0049](0049-protect-linux-server-credentials-under-native-dac-custody.md). Missing publisher trust refuses with `BootstrapTrustUnavailable` before account installation or enrollment. SSH Bootstrap never elevates a downloaded helper or shell installer. Custody refusal reports a fixed reason and administrator remedy; it does not override policy or start a login-scoped server.

The device receives its own SpaceTerm credential with the authority and individual revocation defined in [ADR 0027](0027-trust-paired-clients-equally.md). Client credentials use the native credential facilities required by [ADR 0004](0004-resolve-application-directories-by-xdg-convention.md).

Ordinary reconnects use the existing device credential and never silently enroll a revoked device again. Deliberate SSH Bootstrap can enroll the device again while it retains SSH access to the owning account, so revoking its SpaceTerm credential does not revoke its SSH access.
