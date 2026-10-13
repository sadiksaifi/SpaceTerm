//! Fixed, unprivileged read-only probes. No caller text is interpolated into this script.

/// Maximum bytes read from one probe.
pub const MAX_PROBE_BYTES: usize = 4096;
/// Read-only probe wall deadline.
pub const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Runs through the existing managed OpenSSH control connection with forwarding disabled.
/// This script never reads credentials, creates directories, invokes sudo, or changes services.
pub fn read_only_probe_script() -> &'static [u8] {
    br#"set -efu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
LC_ALL=C
export PATH LC_ALL
uid=$(/usr/bin/id -u)
os=$(/usr/bin/uname -s)
arch=$(/usr/bin/uname -m)
kernel=$(/usr/bin/uname -r)
version=0.0.0
libc=unknown
manager=missing
manager_version=0
manager_reachable=no
linger=unavailable
sudo=unavailable
if [ "$os" = Darwin ]; then
    version=$(/usr/bin/sw_vers -productVersion)
    libc=none
    linger=none
    if [ -x /bin/launchctl ]; then
        manager=launchagent
        manager_version=1
        if /bin/launchctl print "gui/$uid" >/dev/null 2>&1; then manager_reachable=yes; fi
    fi
elif [ "$os" = Linux ]; then
    libc=$(/usr/bin/getconf GNU_LIBC_VERSION 2>/dev/null) || libc=unknown
    if [ "$libc" = unknown ] && [ -x /usr/bin/ldd ]; then
        if /usr/bin/ldd --version 2>&1 | /usr/bin/grep -q '^musl libc'; then libc=musl; fi
    fi
    if [ -r /etc/os-release ]; then
        observed=$(/usr/bin/sed -n 's/^VERSION_ID="\([0-9][0-9.]*\)"$/\1/p' /etc/os-release)
        if [ -n "$observed" ]; then version=$observed; fi
    fi
    if [ -x /usr/bin/sudo ]; then sudo=available; fi
    if [ -x /usr/bin/loginctl ]; then
        observed=$(/usr/bin/loginctl --no-pager --no-ask-password show-user "$uid" --property=Linger --value 2>/dev/null) || observed=unavailable
        case "$observed" in yes|no) linger=$observed ;; esac
    fi
    if [ -x /usr/bin/systemctl ]; then
        manager=systemd-user
        manager_version=$(/usr/bin/systemctl --version | /usr/bin/sed -n '1s/^systemd \([0-9][0-9]*\).*$/\1/p')
        runtime=/run/user/$uid
        if [ -d "$runtime" ] && [ ! -L "$runtime" ] && [ -S "$runtime/bus" ] && [ ! -L "$runtime/bus" ] &&
            [ "$(/usr/bin/stat -c %u "$runtime")" = "$uid" ] &&
            [ "$(/usr/bin/stat -c %a "$runtime")" = 700 ] &&
            [ "$(/usr/bin/stat -c %u "$runtime/bus")" = "$uid" ]; then
            observed=$(/usr/bin/env -i PATH="$PATH" LC_ALL=C XDG_RUNTIME_DIR="$runtime" DBUS_SESSION_BUS_ADDRESS="unix:path=$runtime/bus" /usr/bin/systemctl --user --no-pager --no-ask-password show --property=Version --value 2>/dev/null) || observed=
            native_version=$(printf '%s\n' "$observed" | /usr/bin/sed -n '1s/^\([0-9][0-9]*\).*$/\1/p')
            if [ -n "$native_version" ]; then manager_reachable=yes; manager_version=$native_version; fi
        fi
    fi
fi
printf 'STBP2\nuid=%s\nos=%s\narch=%s\nversion=%s\nlibc=%s\nkernel=%s\nmanager=%s\nmanager_version=%s\nmanager_reachable=%s\nlinger=%s\nsudo=%s\n' "$uid" "$os" "$arch" "$version" "$libc" "$kernel" "$manager" "$manager_version" "$manager_reachable" "$linger" "$sudo"
"#
}
