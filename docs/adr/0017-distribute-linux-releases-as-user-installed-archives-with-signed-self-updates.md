# Distribute Linux releases as user-installed archives with signed self-updates

Linux releases ship as one x86_64 desktop archive and one SpaceTerm Core archive, unpacked into the user's own directories, so SpaceTerm can replace itself without root and without a distribution package for each desktop. The archives are built against glibc 2.35, which excludes musl and older distributions. Updates verify a signed per-platform feed and archive with the update keys of [ADR 0009](0009-distribute-tagged-releases-with-signed-updates.md), keeping one trust root across platforms.

The desktop archive starts the GUI through `bin/spaceterm-desktop`. SpaceTerm Core, which contains the `spaceterm` CLI, installs from its own archive under [ADR 0054](0054-use-one-headless-executable.md).

## Installer

[install.sh](../../packaging/install.sh) runs only on Linux. Each release publishes its own copy, pinned to that release's version, and the copy downloads only from that release's tag. `--version` selects another explicit version. Before extracting or running a download, the installer checks its SHA-256 checksum and its Ed25519 signature under the current or backup key pinned in the script. It installs SpaceTerm Core alone by default, and the desktop too with `--desktop`. The script lists its options and directories.

A v0.6.x install left a desktop tree without SpaceTerm Core, a `~/.local/bin/spaceterm` link into that tree, and a desktop entry and icon under the old application ID. The installer recognizes that exact tree instead of refusing it, and stops at any tree it does not recognize. With `--desktop` it replaces the tree. Without `--desktop` it removes the tree, because the new SpaceTerm Core now provides the `spaceterm` CLI. It moves only the exact old link out of the launcher path, and puts it back if the installation fails. After that, it removes the old updater's leftover directory, and the old desktop entry and icon only where they match what the old installer wrote. It keeps every retained XDG data root and refuses a custom install path that overlaps one.
