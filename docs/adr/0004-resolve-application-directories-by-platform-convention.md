# Resolve application directories by platform convention

One semantic application-directory interface supports XDG roots on macOS and Linux and Known
Folders on Windows. Windows keeps managed SSH metadata machine-local because it can contain local
identity-file paths. Filesystem runtime roots remain optional because native IPC security cannot
be represented by a directory on every platform; credentials stay in native credential facilities.
