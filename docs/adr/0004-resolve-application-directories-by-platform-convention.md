# Resolve application directories by platform convention

One semantic application-directory interface resolves XDG roots on macOS and Linux. Filesystem
runtime roots remain optional because neither `XDG_RUNTIME_DIR` nor a usable temporary directory
is guaranteed; credentials stay in native credential facilities.
