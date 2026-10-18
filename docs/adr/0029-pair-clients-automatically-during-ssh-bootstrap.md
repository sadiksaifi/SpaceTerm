# Connect through OpenSSH account authority

SSH connections use OpenSSH host verification and account authentication under [ADR 0003](0003-let-openssh-own-remote-authentication.md), followed by a non-PTY account-local protocol bridge. SSH Bootstrap prepares or reuses the verified account server without enrolling a SpaceTerm Paired Device. Revoking a direct-network pairing does not revoke SSH account access.

Connect composes normal SSH authentication, discovery and reuse, offered verified installation when missing, permitted startup and Workspace presentation. Existing work is reused. Incompatibility does not authorize an update, and Connect never reverses deliberate Stop; only explicit Start does so. Helper preparation stays internal to Connect/Resume rather than becoming a separate public step.

Previously active connection intent and restricted pending-original intent may continue quietly after interruption, sleep or client relaunch. Saved inactive hosts are not discovery targets. Background continuation never solicits SSH passwords, host approval or credential-provider authorization and never bypasses agent, hardware-key or OS consent. Authentication or host/account/server identity changes pause continuation until requested remote work or Connect/Resume obtains authority.

Original-operation resolution keeps the validated original route, account, server, build, client and operation identities. Fresh query authority does not renew destructive approval, repeat Stop or activation, submit replacement work, redirect through a changed alias or Start a stopped server. Removing a saved SSH connection ends presentation and ordinary reconnect intent immediately while preserving restricted pending originals; resolving them never recreates that connection.

These decisions replace automatic SSH pairing. Their implementation remains with the SSH and remote recovery issues in [#520](https://github.com/sadiksaifi/SpaceTerm/issues/520).
