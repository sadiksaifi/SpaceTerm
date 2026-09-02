# Remote Project Workspaces

Remote over SSH is the third SpaceTerm Workspace source. A Remote Project Workspace is pinned to
one directory on one SSH destination and remains available only for the current SpaceTerm run.

SpaceTerm validates the system OpenSSH client at startup, guides destination and directory
selection, opens remote Terminal Sessions, and owns the connection for the lifetime of the
Workspace. If that connection is lost, the Workspace remains visible with its final presentation
and an explicit Reconnect action.

## Requirements

The local Mac needs:

- `/usr/bin/ssh` from OpenSSH 8.2 or newer
- an ordinary SSH configuration, key, agent, Keychain entry, or other OpenSSH-supported
  authentication method for the destination

Check the installed client with:

```sh
/usr/bin/ssh -V
```

The remote account needs a POSIX-like Linux, macOS, or BSD environment with `/bin/sh`. Supported
login shells are Bash, Zsh, Fish, Nushell, Elvish, and POSIX `sh`. Windows OpenSSH hosts are not
supported.

SpaceTerm uses the system OpenSSH client. It does not implement SSH, replace `known_hosts`, store
credentials, or install a SpaceTerm agent, shell integration, terminfo entry, daemon, or other
component on the remote machine.

SpaceTerm checks `/usr/bin/ssh` once from its captured startup environment. When the executable is
missing, older than OpenSSH 8.2, or cannot be identified safely, **Remote over SSH** is disabled in
the New Workspace Panel with the reason shown on the row. No connection or authentication prompt
is started while this gate is closed.

## SSH host discovery

When the SSH Host Picker opens, SpaceTerm scans:

- its managed SSH configuration
- `~/.ssh/config`
- unconditional, statically resolvable `Include` files reached from those configurations

The picker discovers positive literal aliases from global `Host` declarations. It intentionally
does not list wildcard or negated patterns, `Match`-derived names, `/etc/ssh/ssh_config` entries,
DNS results, `known_hosts`, or every destination OpenSSH could resolve. System SSH configuration
participates when OpenSSH connects even though its entries are not discovery results.

Discovery runs again each time the Host Picker opens. Each configured row identifies whether it is
SpaceTerm-managed or read-only and shows its direct target or configuration provenance. Safe hosts
remain selectable when another file has a problem; a nonselectable diagnostic row explains
unreadable files, malformed entries, include cycles, or safety-limit truncation without exposing
configuration contents.

Standard user SSH entries are read-only in SpaceTerm. SpaceTerm-managed entries support Add, Edit,
and Delete and contain:

- Alias
- Host name
- optional User
- optional Port
- optional Identity file

Managed aliases are available only inside SpaceTerm, and SpaceTerm never rewrites `~/.ssh/config`
or `/etc/ssh/ssh_config`. Edit and Delete remain unavailable while a live Remote Project Workspace
uses the managed alias.

Typing `user@alias` for a configured alias creates an unsaved destination override. A query that is
neither an exact configured alias nor a valid user override offers Add SSH Host; typing alone never
connects or writes configuration.

## Open a Remote Project

The complete flow is:

1. Press Command-O and select **Remote over SSH**. If the row is disabled, follow its OpenSSH
   availability guidance before continuing.
2. Select a configured destination, enter a `user@alias` override, or choose **Add SSH Host**.
3. Complete any password, key-passphrase, keyboard-interactive, MFA, or host-confirmation
   Authentication Prompt in SpaceTerm's application-owned Dialog or Alert.
4. Browse the remote account from `~/`, enter an absolute path beginning with `/`, or enter a
   home-relative path beginning with `~/`.
5. Select **Open Remote Project**. For a missing exact path, confirm **Create Folder** first.

The Remote Workspace Picker reads one directory level at a time. It has no index, recents, fuzzy
matching, persistence, or filesystem watcher. If a listing is truncated, type the exact path to
continue.

Opening performs a final remote existence, directory-type, permission, and physical-identity
validation before SpaceTerm creates anything. A connection or validation failure keeps the
retained flow available for retry. Cancelling the flow closes its connection and writes no
Workspace state.

SpaceTerm preserves the selected SSH destination and the exact remote directory spelling for
display and shell startup. It separately resolves the physical directory for identity. Opening an
equivalent path through the same destination activates the existing Remote Project Workspace;
using another SSH alias remains distinct even when both aliases currently reach the same machine.

An unrenamed Workspace at the remote home directory uses the destination as its name. Other Remote
Projects use `<directory basename> · <destination>`. A custom Workspace name continues to override
the automatic name.

## Authentication and host verification

Authentication and host verification remain OpenSSH policy. SpaceTerm displays OpenSSH's bounded,
control-free prompt in an application-owned Alert or Dialog and returns one answer to the waiting
OpenSSH process. Secret responses use obscured Text Input without claiming native protected
accessibility or macOS Secure Event Input semantics.

Cancel rejects the current prompt and returns to the retained SSH selection flow. SpaceTerm does
not save passwords, passphrases, MFA responses, or host-confirmation answers. OpenSSH continues to
own agent, Keychain, key-file, proxy, forwarding, keepalive, and host-verification behavior.

Before accepting a new host key, compare its fingerprint with a trusted source such as the remote
system administrator. If OpenSSH reports a changed host key, stop and resolve the `known_hosts`
entry through the normal, verified OpenSSH procedure. Do not disable strict host-key checking or
accept an unverified replacement key.

## Connections, Panes, and reconnect

Each pending Remote flow or open Remote Project Workspace owns one isolated OpenSSH control
connection. Its Tabs and Panes reuse that connection, but different Workspaces never share one,
even when they use the same destination. Each Pane still owns its own Terminal Session and SSH
channel.

New Tabs and Panes start in the Workspace's pinned remote directory. SpaceTerm revalidates the
connection and physical directory identity before creating each child. An unavailable or changed
directory blocks that child, shows an actionable alert, and leaves existing Terminal Sessions and
their focus unchanged. A Remote Project never follows a Pane's reported working directory.

A normal remote shell `exit` closes that Pane through the ordinary hierarchy rules. A shared SSH
transport loss is different. SpaceTerm preserves the Workspace, Tab and Pane layout, focused
identities, zoom state, and final terminal presentations, disables input and new child creation,
and marks the Workspace **Disconnected**. The sidebar row shows the connection state; when the
sidebar is hidden, the Workspace chip shows the same state and its tooltip remains descriptive.

Reconnect is explicit, not automatic. The Workspace menu contains one **Reconnect** action, enabled
only while the Workspace is Disconnected or a connection attempt has Failed. Only one reconnect can
run at a time. Its retained progress dialog remains below any native authentication or host-key
sheet, and Cancel safely returns to the disconnected Workspace.

A successful reconnect creates a fresh isolated connection, asks for authentication again only
when OpenSSH requires it, rediscovers the account, validates the exact pinned physical directory,
and starts a fresh shell in every preserved Pane. It preserves Workspace, Tab and Pane identity,
layout, focus, and zoom state. It does not restore remote processes, emulator state, or Scrollback.
Connection failure leaves the Workspace available for another Reconnect. If the directory is
unavailable or resolves to a different physical directory, SpaceTerm keeps the Workspace
Disconnected, preserves its final presentations, and explains that the Remote Project must be
reopened or its directory access repaired.

Closing the flow, Workspace, Operating-System Window, or application closes and reaps the owned
local SSH processes and removes their private runtime artifacts. Remote Projects and their
connections are not restored after SpaceTerm restarts.

## Local-file capability differences

Remote Panes retain terminal input, Selection, copy, ordinary text paste and drop, OSC 52
authorization, Terminal Find, web hyperlinks, accessibility, resize, rendering, and implemented
terminal protocols.

They intentionally do not offer features that would interpret a remote value as a local Mac path:

- local-path hyperlink classification or opening
- File Insertion and clipboard file-URL insertion
- Finder, Quick Look, and file-aware macOS Services
- file-path drag and drop

Remote over SSH does not include SFTP browsing, upload, download, remote preview, file
synchronization, or a remote SpaceTerm agent.

## Application paths

SpaceTerm resolves its application-owned paths once at startup. XDG values must be absolute and
nonempty; an unset, empty, or relative value uses the listed fallback.

| Purpose | Location |
| --- | --- |
| Configuration | `${XDG_CONFIG_HOME:-$HOME/.config}/spaceterm` |
| Data | `${XDG_DATA_HOME:-$HOME/.local/share}/spaceterm` |
| State | `${XDG_STATE_HOME:-$HOME/.local/state}/spaceterm` |
| Cache | `${XDG_CACHE_HOME:-$HOME/.cache}/spaceterm` |
| Runtime | `$XDG_RUNTIME_DIR/spaceterm`, or the owner-private macOS user temporary directory when unavailable or invalid |

SpaceTerm-managed hosts are stored at
`${XDG_CONFIG_HOME:-$HOME/.config}/spaceterm/ssh_config`. Configuration and runtime directories are
created lazily with owner-only access. Runtime sockets and owner directories are private and are
removed by their exact owner. Remote Workspace state, directories, and Pane processes are not
persisted under the state directory.

## Privacy and security

- Credentials and authentication responses remain memory-only, are released promptly, and never
  enter history, logs, diagnostics, accessibility values, or application events.
- The flow retains only a bounded, control-free tail of OpenSSH error output for a
  transient error alert; it does not persist or export that output.
- Exact destinations and remote directories are runtime Workspace data and are not restored after
  application restart.
- Utility operations are bounded and use `/bin/sh` without installing remote files.
- OpenSSH owns keys, agents, Keychain use, `known_hosts`, proxies, forwarding, keepalives, and
  transport security.
- SpaceTerm does not add automatic network telemetry or credential storage.

## Troubleshooting

| Message or symptom | What to do |
| --- | --- |
| **OpenSSH was not found at /usr/bin/ssh** | Verify that the system SSH client exists at that exact path. SpaceTerm does not use a replacement from `PATH`. |
| **OpenSSH 8.2 or newer is required** | Run `/usr/bin/ssh -V` and update macOS to a release with a supported system OpenSSH client before retrying. |
| **The installed SSH client could not be checked** | Confirm that `/usr/bin/ssh -V` runs successfully and that the executable is readable and executable. |
| A known host is missing from the picker | Confirm it is a positive literal alias in global `Host` context in `~/.ssh/config` or an unconditional included file. Wildcards, negations, `Match`, system config, and dynamic resolution are intentionally excluded. |
| A managed host cannot be edited or deleted | Close every live Remote Project Workspace using that alias, then reopen the Host Picker. Standard user-config entries are always read-only. |
| A managed SSH config error appears | Use SpaceTerm's Add, Edit, and Delete actions instead of hand-editing the managed file. Check that the XDG configuration path is absolute and owner-writable without broadening its permissions. |
| Authentication fails or is cancelled | Retry and answer SpaceTerm's Authentication Prompts. For a user-config alias, test the same destination with `/usr/bin/ssh destination` to diagnose agent, key, proxy, or server policy outside SpaceTerm. |
| OpenSSH reports a new or changed host key | Verify the fingerprint with a trusted administrator and repair `known_hosts` through the normal OpenSSH workflow. Never bypass host-key verification. |
| **Enter an absolute path beginning with / or ~/.** | Enter `/path/to/project` or `~/path/to/project`; use `~/` rather than a bare `~`. |
| **No such remote folder** | Enter the correct path or use **Create Folder** after confirming that its parent is writable. |
| **Not a remote folder** | Select or enter a directory rather than a regular file or another remote object. |
| **Permission denied for this remote folder** | Ask the remote administrator for directory traversal and read access, or select a directory the account can use. |
| **SSH connection was lost** | Open the Workspace menu and choose **Reconnect**. Expect fresh shells in the preserved Pane layout; remote processes and Scrollback are not restored. |
| **Remote Directory Unavailable** during reconnect | Restore traversal and read access to the pinned directory, then choose **Reconnect** again. If the directory was intentionally moved or removed, close and reopen the Remote Project at its new path. |
| **Remote Directory Changed** during reconnect | The selected spelling now resolves to a different physical directory. Close and reopen the Remote Project so SpaceTerm can validate and accept the new identity. |
| **Reconnect** is disabled | Wait for the active connection or reconnect attempt to finish. Reconnect is enabled only after a Disconnected or Failed status. |
| Only the first 1024 folders are shown | Type the complete absolute or `~/` path directly. |
| The remote shell cannot start | Confirm that the remote account has `/bin/sh` and one supported login shell, and that the pinned directory still exists and resolves to the original physical directory. |
| A local file action is unavailable in a Remote Pane | This is intentional. Use remote-native tools or an explicitly separate transfer workflow; SpaceTerm does not treat remote paths as local files. |
| A runtime-path or socket error appears | Unset an empty or relative `XDG_RUNTIME_DIR`, or set it to an absolute owner-writable directory. SpaceTerm falls back to the private macOS user temporary directory and never requires broad permissions. |

For a SpaceTerm-managed alias, advanced OpenSSH diagnosis can use the managed file explicitly:

```sh
/usr/bin/ssh -F "$HOME/.config/spaceterm/ssh_config" my-alias
```

Replace `my-alias` with the managed alias. If `XDG_CONFIG_HOME` is set to a valid absolute
directory, substitute its `spaceterm/ssh_config` path. This diagnostic still uses ordinary OpenSSH
host verification and should not be combined with options that bypass it.
