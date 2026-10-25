# Enroll remote servers through Add Server

Settings > Servers > Add Server is the only remote enrollment journey. SSH Setup and Invitation Link methods produce the same saved paired-WSS connection and Client Identity. Application-menu, command-palette and operating-system invitation shortcuts open this journey.

SSH Setup uses [OpenSSH authority](0003-use-openssh-for-server-setup.md) temporarily to discover the approved account installation, offer verified installation when missing, obtain required operating-system consent, explicitly start the server, configure reachable WSS exposure and enroll the client. Reversing deliberate Stop requires explicit startup authorization inside setup. Incompatibility leaves the installation intact for administration on its own machine.

Installation consent binds one signed artifact's identity, target and complete digest; transfer and installation revalidate those bytes. Network exposure requires deliberate authorization for a reachable endpoint. Successful installation or startup alone never completes Add Server. Success requires persisted pairing credentials and saved connection, closed SSH resources and authenticated WSS admission. Unreachable WSS or failed credential persistence presents an actionable setup result without publishing an SSH connection.

Setup shows its current phase and yields to Authentication Prompts only while interaction is required. Authentication and transfer have separate phase limits. Cancellation releases bounded owned processes and input suppression. Admitted service effects are observed and reported truthfully, without claiming rollback; enrollment cleanup targets only the exact claim and pairing.

After setup, terminal work, directories, status, reconnect, relaunch, Pane Retry and pairing recovery use WSS exclusively. Native service administration remains on the server's machine. Saved connections retain no SSH runtime route, maintenance journal or fallback.
