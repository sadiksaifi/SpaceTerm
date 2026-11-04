#!/bin/sh
# Install SpaceTerm Core, and with --desktop the SpaceTerm desktop app. Each release publishes
# this script as install.sh:
#
#   curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --desktop
#
# SpaceTerm Core installs into ~/.local/share/spaceterm/core with its spaceterm launcher in
# ~/.local/bin. A new version installs beside the running one, which keeps running.
#
# The desktop app installs into /Applications, or ~/Applications when /Applications is not
# writable, on macOS, and into ~/.local/lib/spaceterm on Linux. SPACETERM_INSTALL_DIR selects
# another directory for the desktop app.
#
# Options:
#   --desktop          also install the desktop app
#   --version VERSION  install VERSION, such as 1.4.0, instead of the latest release
#   --uninstall        run spaceterm uninstall; with --desktop, also remove the desktop app
#   --no-modify-path   leave shell profiles unchanged
#   --linger           Linux: keep SpaceTerm Core running after logout without asking
#
# The installer never downgrades. It verifies every download before it runs or installs it:
# on macOS with codesign against the SpaceTerm Developer ID, on Linux with openssl against
# the Ed25519 update keys below.

set -eu

# The release pipeline fills RELEASE_KIND with release or dry-run, and TEAM_ID with the
# Developer ID Team ID. A dry run accepts ad hoc code signatures and placeholder update keys.
readonly RELEASE_KIND='@RELEASE_KIND@'
readonly TEAM_ID='@TEAM_ID@'
# packaging/update-trust.json holds the same two keys, and a test checks that they match.
readonly UPDATE_KEY_CURRENT='PLACEHOLDER: the release issue sets the current Ed25519 public key'
readonly UPDATE_KEY_BACKUP='PLACEHOLDER: the release issue sets the backup Ed25519 public key'

readonly RELEASES="https://github.com/sadiksaifi/SpaceTerm/releases"
readonly MINIMUM_MACOS_MAJOR=26
readonly MINIMUM_GLIBC_MAJOR=2
readonly MINIMUM_GLIBC_MINOR=35
readonly CORE_IDENTIFIER=io.github.sadiksaifi.spaceterm.core
# The desktop app's macOS bundle identifier and Linux application ID.
readonly DESKTOP_ID=io.github.sadiksaifi.spaceterm
readonly PROFILE_COMMENT="# Added by the SpaceTerm installer"

work=""
volume=""
staging=""
previous=""
target=""

step() {
    printf '==> %s\n' "$1"
}

say() {
    printf '%s\n' "$1"
}

warn() {
    printf 'warning: %s\n' "$1" >&2
}

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$volume" ]; then
        hdiutil detach "$volume" -force >/dev/null 2>&1 || true
    fi
    if [ -n "$staging" ]; then
        # Restore the previous desktop app when the replacement did not finish.
        if [ -n "$previous" ] && [ -e "$staging/$previous" ] && [ ! -e "$target" ]; then
            mv "$staging/$previous" "$target" || true
        fi
        rm -rf "$staging"
    fi
    if [ -n "$work" ]; then
        rm -rf "$work"
    fi
}

usage_error() {
    die "$1. Options: --desktop, --version VERSION, --uninstall, --no-modify-path, --linger"
}

download() {
    curl --fail --location --proto '=https' --tlsv1.2 "$@"
}

require_commands() {
    for command in "$@"; do
        command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
    done
}

# Prompts read /dev/tty, because `curl ... | sh` gives the script itself as standard input.
has_tty() {
    (: </dev/tty) 2>/dev/null
}

ask() {
    printf '%s [y/N] ' "$1" >/dev/tty
    answer=""
    IFS= read -r answer </dev/tty || true
    case "$answer" in
        y | Y | yes | Yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

# MAJOR.MINOR.PATCH with no leading zeros. Nine digits per part keep shell arithmetic exact.
valid_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$'
}

# Prints older, same or newer: how version $1 compares with version $2.
compare_versions() {
    saved_ifs="$IFS"
    IFS=.
    # shellcheck disable=SC2086 # Splitting on dots is the point.
    set -- $1 $2
    IFS="$saved_ifs"
    for _ in 1 2 3; do
        if [ "$1" -lt "$4" ]; then
            say older
            return
        elif [ "$1" -gt "$4" ]; then
            say newer
            return
        fi
        shift
    done
    say same
}

# Standard base64 of a raw 32-byte Ed25519 public key. Anything else is a placeholder.
valid_key() {
    printf '%s\n' "$1" | grep -Eq '^[A-Za-z0-9+/]{42}[AEIMQUYcgkosw048]=$'
}

valid_team_id() {
    printf '%s\n' "$1" | grep -Eq '^[A-Z0-9]{10}$'
}

# A release fills every signing value; only a dry run may leave the keys as placeholders.
require_prepared() {
    case "$RELEASE_KIND" in
        release)
            valid_team_id "$TEAM_ID" || die "this installer has no Developer ID Team ID; download install.sh from $RELEASES"
            if ! valid_key "$UPDATE_KEY_CURRENT" || ! valid_key "$UPDATE_KEY_BACKUP"; then
                die "this installer has placeholder update keys; download install.sh from $RELEASES"
            fi
            ;;
        dry-run)
            warn "this installer comes from a dry run: it accepts ad hoc code signatures and placeholder update keys"
            ;;
        *) die "this installer was not prepared by a release; download install.sh from $RELEASES" ;;
    esac
}

detect_platform() {
    case "$(uname -s)" in
        Darwin)
            platform=macos-arm64
            [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ] \
                || die "SpaceTerm requires a Mac with Apple silicon"
            macos_major="$(sw_vers -productVersion | cut -d . -f 1)"
            [ "$macos_major" -ge "$MINIMUM_MACOS_MAJOR" ] \
                || die "SpaceTerm requires macOS $MINIMUM_MACOS_MAJOR or newer"
            ;;
        Linux)
            platform=linux-x86_64
            [ "$(uname -m)" = x86_64 ] || die "SpaceTerm for Linux requires an x86_64 computer"
            # musl and other C libraries do not answer, so they are refused with old glibc releases.
            glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
            case "$glibc" in
                "glibc "[0-9]*.[0-9]*) glibc="${glibc#glibc }" ;;
                *) die "SpaceTerm for Linux requires glibc $MINIMUM_GLIBC_MAJOR.$MINIMUM_GLIBC_MINOR or newer" ;;
            esac
            major="${glibc%%.*}"
            minor="${glibc#*.}"
            minor="${minor%%.*}"
            if [ "$major" -lt "$MINIMUM_GLIBC_MAJOR" ] \
                || { [ "$major" -eq "$MINIMUM_GLIBC_MAJOR" ] && [ "$minor" -lt "$MINIMUM_GLIBC_MINOR" ]; }; then
                die "SpaceTerm for Linux requires glibc $MINIMUM_GLIBC_MAJOR.$MINIMUM_GLIBC_MINOR or newer"
            fi
            ;;
        *) die "SpaceTerm requires macOS or Linux" ;;
    esac
}

require_home() {
    case "${HOME:-}" in
        /*) ;;
        *) die "an absolute HOME is required" ;;
    esac
    core_data="$HOME/.local/share/spaceterm/core"
    bin_dir="$HOME/.local/bin"
}

file_sha256() {
    if [ "$platform" = macos-arm64 ]; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        sha256sum "$1" | cut -d ' ' -f 1
    fi
}

# Prints the first value of a string field in a JSON document, such as build.json.
json_field() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -n 1
}

# Sets version and downloads its SHA256SUMS. Latest asks the latest release for its checksums;
# every archive then comes from the tagged release, so a release published meanwhile cannot mix
# assets: its archives would not match these checksums.
find_release() {
    work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"
    if [ -n "$requested_version" ]; then
        step "Finding SpaceTerm $requested_version"
        sums="$RELEASES/download/v$requested_version/SHA256SUMS"
    else
        step "Finding the latest release"
        sums="$RELEASES/latest/download/SHA256SUMS"
    fi
    download --silent --show-error "$sums" --output "$work/SHA256SUMS" \
        || die "could not download the release checksums"
    if [ -n "$requested_version" ]; then
        version="$requested_version"
    else
        entry="$(grep -E "^[0-9a-f]{64}  spaceterm-core-[0-9]+\.[0-9]+\.[0-9]+-$platform\.tar\.gz\$" "$work/SHA256SUMS" | head -n 1)"
        [ -n "$entry" ] || die "the release checksums do not list SpaceTerm Core for $platform"
        version="${entry#*  spaceterm-core-}"
        version="${version%-"$platform".tar.gz}"
        valid_version "$version" || die "the release checksums name an invalid version"
    fi
}

# Downloads release asset $1 into the work directory and checks it against SHA256SUMS.
fetch() {
    pattern="$(printf '%s' "$1" | sed 's/\./\\./g')"
    entry="$(grep -E "^[0-9a-f]{64}  $pattern\$" "$work/SHA256SUMS")" \
        || die "the release checksums do not list $1"
    if [ "${2:-}" = quiet ]; then
        download --silent --show-error "$RELEASES/download/v$version/$1" --output "$work/$1" \
            || die "could not download $1"
    else
        download --progress-bar "$RELEASES/download/v$version/$1" --output "$work/$1" \
            || die "could not download $1"
    fi
    [ "$(file_sha256 "$work/$1")" = "${entry%%  *}" ] || die "$1 does not match its published checksum"
}

# Verifies file $1 against its raw 64-byte Ed25519 signature $2 under the current or the backup
# key. openssl reads the key as DER: the fixed Ed25519 SubjectPublicKeyInfo prefix, then the key.
verify_signature() {
    [ "$(wc -c <"$2" | tr -d ' ')" = 64 ] || die "$(basename "$1") has a malformed signature"
    checked=0
    for key in "$UPDATE_KEY_CURRENT" "$UPDATE_KEY_BACKUP"; do
        valid_key "$key" || continue
        checked=1
        {
            printf '\060\052\060\005\006\003\053\145\160\003\041\000'
            printf '%s\n' "$key" | openssl base64 -d -A
        } >"$work/key.der"
        if openssl pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin \
            -in "$1" -sigfile "$2" >/dev/null 2>&1; then
            return
        fi
    done
    if [ "$checked" = 0 ]; then
        # require_prepared lets placeholder keys through only in a dry run.
        warn "dry run: skipped the Ed25519 check of $(basename "$1") because the update keys are placeholders"
        return
    fi
    die "$(basename "$1") does not carry a valid SpaceTerm signature"
}

# Verifies bundle $1 against the SpaceTerm Developer ID with identifier $2.
verify_code() {
    if [ "$RELEASE_KIND" = dry-run ]; then
        codesign --verify --deep --strict "$1" >/dev/null 2>&1 \
            || die "$(basename "$1") is not code signed"
        return
    fi
    requirement="identifier \"$2\" and anchor apple generic"
    requirement="$requirement and certificate 1[field.1.2.840.113635.100.6.2.6] exists"
    requirement="$requirement and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
    requirement="$requirement and certificate leaf[subject.OU] = \"$TEAM_ID\""
    codesign --verify --deep --strict --test-requirement="=$requirement" "$1" >/dev/null 2>&1 \
        || die "$(basename "$1") is not signed by the SpaceTerm Developer ID"
}

# Every SpaceTerm Core command the installer runs takes --machine local, because it acts on this
# account's Core whatever SPACETERM_MACHINE the shell selects.

# The SpaceTerm Core executable within an installation, in this machine's layout. A macOS
# installation is an app-like bundle, so System Permissions follow the bundle.
core_executable() {
    if [ "$platform" = macos-arm64 ]; then
        printf '%s' 'SpaceTerm Core.app/Contents/MacOS/spaceterm'
    else
        printf '%s' bin/spaceterm
    fi
}

# Prints the newest installed SpaceTerm Core version, or nothing. Each version lives in its own
# installations/<sha256> directory beside the others.
installed_core_version() {
    newest=""
    for build in "$core_data"/installations/*/build.json; do
        [ -f "$build" ] || continue
        digest="$(basename "$(dirname "$build")")"
        [ "${#digest}" -eq 64 ] || continue
        case "$digest" in *[!0-9a-f]*) continue ;; esac
        found="$(json_field version <"$build")"
        valid_version "$found" || die "the installed SpaceTerm Core version could not be read"
        if [ -z "$newest" ] || [ "$(compare_versions "$found" "$newest")" = newer ]; then
            newest="$found"
        fi
    done
    printf '%s' "$newest"
}

# Refuses a downgrade of product $1 from installed version $2 to version $3. Prints install or
# skip.
plan_product() {
    if [ -z "$2" ]; then
        say install
        return
    fi
    case "$(compare_versions "$3" "$2")" in
        newer) say install ;;
        same) say skip ;;
        older) die "$1 $2 is installed, which is newer than $3; the installer never downgrades" ;;
    esac
}

install_core() {
    archive="spaceterm-core-$version-$platform.tar.gz"
    step "Downloading SpaceTerm Core $version"
    fetch "$archive"
    fetch "$archive.sig" quiet
    if [ "$platform" = linux-x86_64 ]; then
        verify_signature "$work/$archive" "$work/$archive.sig"
    fi
    # Refuse links and parent paths before extraction. SpaceTerm Core verifies the complete
    # signed layout again before it retains the installation.
    tar -tzf "$work/$archive" >"$work/members" 2>/dev/null || die "could not read $archive"
    if grep -Ev '^core(/|$)' "$work/members" >/dev/null \
        || grep -E '(^|/)\.\.?(/|$)' "$work/members" >/dev/null; then
        die "$archive has an invalid layout"
    fi
    tar -tvzf "$work/$archive" >"$work/kinds" 2>/dev/null || die "could not read $archive"
    if cut -c 1 "$work/kinds" | grep -Ev '^[-d]$' >/dev/null; then
        die "$archive must contain only files and directories"
    fi
    mkdir "$work/runtime"
    tar -xzf "$work/$archive" -C "$work/runtime" 2>/dev/null || die "could not unpack $archive"
    runtime="$work/runtime/core"
    [ -f "$runtime/build.json" ] || die "$archive has no build document"
    [ "$(json_field identity <"$runtime/build.json")" = spaceterm ] \
        && [ "$(json_field version <"$runtime/build.json")" = "$version" ] \
        || die "$archive does not hold the SpaceTerm Core $version release"
    if [ "$platform" = macos-arm64 ]; then
        verify_code "$runtime/SpaceTerm Core.app" "$CORE_IDENTIFIER"
    fi
    executable="$runtime/$(core_executable)"
    [ -f "$executable" ] && [ -x "$executable" ] || die "$archive has no SpaceTerm Core executable"
    cp "$work/$archive" "$runtime/artifact.tar.gz"
    cp "$work/$archive.sig" "$runtime/artifact.sig"
    step "Installing SpaceTerm Core $version"
    "$executable" --machine local internal retain "$runtime" || die "SpaceTerm Core $version could not be installed"
}

# Resolves the installed SpaceTerm Core executable through its launcher, or through the service
# record when another tool owns the launcher path. Sets core_installed to the executable or "".
locate_installed_core() {
    core_installed=""
    relative="$(core_executable)"
    root="$core_data"
    launcher="$bin_dir/spaceterm"
    runtime=""
    if [ -L "$launcher" ]; then
        linked="$(readlink "$launcher")"
        case "$linked" in
            "$root/installations/"*/"$relative") runtime="${linked%/"$relative"}" ;;
        esac
    fi
    if [ -z "$runtime" ] && [ -f "$root/service.json" ] && [ ! -L "$root/service.json" ]; then
        digest="$(json_field installation <"$root/service.json")"
        [ "${#digest}" -eq 64 ] || die "the SpaceTerm Core service record could not be read"
        runtime="$root/installations/$digest"
    fi
    [ -n "$runtime" ] || return 0
    digest="$(basename "$runtime")"
    [ "${#digest}" -eq 64 ] || die "the spaceterm launcher is unmanaged"
    case "$digest" in *[!0-9a-f]*) die "the spaceterm launcher is unmanaged" ;; esac
    for directory in "$HOME/.local" "$HOME/.local/share" "${root%/core}" "$root" "$root/installations" "$runtime"; do
        [ ! -L "$directory" ] || die "the SpaceTerm Core installation has unsafe links"
    done
    executable="$runtime"
    rest="$relative"
    while [ -n "$rest" ]; do
        executable="$executable/${rest%%/*}"
        [ ! -L "$executable" ] || die "the SpaceTerm Core installation has unsafe links"
        case "$rest" in */*) rest="${rest#*/}" ;; *) rest="" ;; esac
    done
    [ -f "$executable" ] && [ -x "$executable" ] || die "the installed SpaceTerm Core executable is missing"
    core_installed="$executable"
}

uninstall_core() {
    locate_installed_core
    if [ -z "$core_installed" ]; then
        step "SpaceTerm Core is already uninstalled"
        return
    fi
    status=0
    preview="$("$core_installed" --machine local uninstall)" || status=$?
    case "$status" in
        0)
            say "$preview"
            return
            ;;
        3) ;;
        *)
            say "$preview"
            die "SpaceTerm Core could not be uninstalled"
            ;;
    esac
    token="$(printf '%s\n' "$preview" | sed -n 's/.*--confirm \(c1-[0-9a-f]\{1,\}\).*/\1/p' | head -n 1)"
    [ -n "$token" ] || die "the uninstall preview has no confirmation token"
    if ! has_tty; then
        say "$preview"
        die "no terminal is attached to confirm; review the preview, then run: spaceterm uninstall --confirm $token"
    fi
    printf '%s\n' "$preview" | sed -n '1p' >/dev/tty
    ask "Uninstall SpaceTerm Core?" || die "uninstall cancelled"
    "$core_installed" --machine local uninstall --confirm "$token" || die "SpaceTerm Core could not be uninstalled"
}

# Linux only. Without linger, systemd stops the account's SpaceTerm Core at logout.
offer_linger() {
    command -v loginctl >/dev/null 2>&1 || {
        say "This computer has no systemd login manager, so SpaceTerm Core stops when you log out."
        return
    }
    uid="$(id -u)"
    [ "$(loginctl show-user "$uid" --property=Linger --value 2>/dev/null || true)" != yes ] || return 0
    if [ "$linger" = false ]; then
        if ! has_tty; then
            say "No terminal is attached, so the installer did not ask to keep SpaceTerm Core running after you log out. Run the installer again with --linger to keep it running."
            return
        fi
        if ! ask "Keep SpaceTerm Core running after you log out? This runs sudo loginctl enable-linger."; then
            say "SpaceTerm Core stops when you log out. Run the installer again with --linger to keep it running."
            return
        fi
    fi
    step "Keeping SpaceTerm Core running after you log out"
    if has_tty; then
        # sudo asks for the password on the terminal itself.
        sudo loginctl enable-linger "$uid" || die "could not keep SpaceTerm Core running after you log out"
    else
        sudo -n loginctl enable-linger "$uid" </dev/null \
            || die "could not keep SpaceTerm Core running after you log out: sudo needs a terminal to ask for your password"
    fi
}

# Adds ~/.local/bin to the profile of the login shell, once.
modify_path() {
    case ":${PATH:-}:" in
        *":$bin_dir:"*) return ;;
    esac
    advice="Add $bin_dir to PATH to run spaceterm from a terminal."
    if [ "$modify_path" = false ]; then
        say "$advice"
        return
    fi
    # shellcheck disable=SC2016 # The profile expands $HOME and $PATH when the shell starts.
    line='export PATH="$HOME/.local/bin:$PATH"'
    case "$(basename "${SHELL:-}")" in
        zsh) profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash)
            if [ "$platform" = macos-arm64 ]; then
                profile="$HOME/.bash_profile"
            else
                profile="$HOME/.bashrc"
            fi
            ;;
        fish)
            profile="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/spaceterm.fish"
            # shellcheck disable=SC2016 # fish expands $HOME when it starts.
            line='fish_add_path --path $HOME/.local/bin'
            ;;
        *)
            say "$advice"
            return
            ;;
    esac
    if [ -f "$profile" ] && grep -qxF "$line" "$profile"; then
        return
    fi
    mkdir -p "$(dirname "$profile")" || die "could not create the directory of $profile"
    if [ -s "$profile" ]; then
        printf '\n' >>"$profile" || die "could not write $profile"
    fi
    printf '%s\n%s\n' "$PROFILE_COMMENT" "$line" >>"$profile" || die "could not write $profile"
    say "Added $bin_dir to PATH in $profile. Open a new terminal to run spaceterm."
}

# Replacing the bundle of a running application breaks it.
ensure_not_running_macos() {
    processes="$(ps -axww -o args=)"
    # A quoted pattern matches the install path literally, whatever characters it contains.
    case "$processes" in
        *"$target/Contents/MacOS/SpaceTerm"*) die "quit SpaceTerm, then run the installer again" ;;
    esac
}

macos_desktop_locations() {
    if [ -n "${SPACETERM_INSTALL_DIR:-}" ]; then
        install_dir="$SPACETERM_INSTALL_DIR"
    elif [ -w /Applications ]; then
        install_dir=/Applications
    else
        install_dir="$HOME/Applications"
    fi
    target="$install_dir/SpaceTerm.app"
}

macos_app_version() {
    plutil -extract CFBundleShortVersionString raw -o - "$1/Contents/Info.plist" 2>/dev/null || true
}

installed_desktop_version_macos() {
    [ -e "$target" ] || return 0
    found="$(macos_app_version "$target")"
    valid_version "$found" || die "the version of $target could not be read"
    printf '%s' "$found"
}

install_desktop_macos() {
    image="SpaceTerm-$version-macos-arm64.dmg"
    step "Downloading SpaceTerm $version"
    fetch "$image"
    # macOS 27 warns that this hdiutil form is deprecated; its replacement may not exist on
    # macOS 26. Show hdiutil's diagnostics only when attaching fails.
    if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$work/volume" \
        "$work/$image" >/dev/null 2>"$work/attach.log"; then
        cat "$work/attach.log" >&2
        die "could not open $image"
    fi
    volume="$work/volume"
    [ -d "$volume/SpaceTerm.app" ] || die "$image does not contain SpaceTerm.app"
    verify_code "$volume/SpaceTerm.app" "$DESKTOP_ID"
    [ "$(macos_app_version "$volume/SpaceTerm.app")" = "$version" ] \
        || die "$image does not hold SpaceTerm $version"
    step "Installing SpaceTerm $version to $install_dir"
    mkdir -p "$install_dir" || die "could not create $install_dir"
    # Stage beside the destination so the final replacement is a rename on one volume.
    staging="$(mktemp -d "$install_dir/.SpaceTerm.install.XXXXXX")" \
        || die "could not write to $install_dir"
    ditto "$volume/SpaceTerm.app" "$staging/SpaceTerm.app"
    # SpaceTerm may have started during the download.
    ensure_not_running_macos
    if [ -e "$target" ]; then
        previous=previous.app
        mv "$target" "$staging/$previous" || die "could not move the existing SpaceTerm.app aside"
    fi
    mv "$staging/SpaceTerm.app" "$target" || die "could not install SpaceTerm.app"
    step "Installed SpaceTerm $version to $target"
}

uninstall_desktop_macos() {
    macos_desktop_locations
    ensure_not_running_macos
    if [ ! -e "$target" ]; then
        step "SpaceTerm is already removed"
        return
    fi
    rm -rf "$target" || die "could not remove $target"
    step "Removed SpaceTerm. Settings and other user data remain."
}

# A running process keeps its executable open, and its name in /proc/PID/exe, so the check
# matches the installed executable whichever launcher or symlink started it. A process the
# updater replaced but has not relaunched runs from the updater's staging directory.
ensure_not_running_linux() {
    executable="$target/bin/spaceterm-desktop"
    replaced="$(dirname "$target")/.$(basename "$target").update/"
    for link in /proc/[0-9]*/exe; do
        case "$(readlink "$link" 2>/dev/null || true)" in
            "$executable" | "$replaced"*) die "quit SpaceTerm, then run the installer again" ;;
        esac
    done
}

has_vulkan() {
    for ldconfig in ldconfig /sbin/ldconfig /usr/sbin/ldconfig; do
        if command -v "$ldconfig" >/dev/null 2>&1; then
            "$ldconfig" -p 2>/dev/null | grep -q 'libvulkan\.so\.1 '
            return
        fi
    done
    return 1
}

# GIO looks up a launcher's program before it expands field codes, so an escaped percent sign
# in the path never matches the file.
require_launchable() {
    case "$1" in
        *[[:cntrl:]%]*) die "the install path contains characters desktop launchers cannot run" ;;
    esac
}

# Desktop Entry Exec quoting, then string value escaping.
exec_value() {
    quoted="$(printf '%s' "$1" | sed -e 's/[\\"`$]/\\&/g' -e 's/\\/\\\\/g')"
    printf '"%s"' "$quoted"
}

linux_desktop_locations() {
    install_dir="${SPACETERM_INSTALL_DIR:-$HOME/.local/lib}"
    case "$install_dir" in
        /*) ;;
        *) die "SPACETERM_INSTALL_DIR must be an absolute path" ;;
    esac
    require_launchable "$install_dir"
    case "${XDG_DATA_HOME:-}" in
        /*) data_home="$XDG_DATA_HOME" ;;
        *) data_home="$HOME/.local/share" ;;
    esac
    if [ -d "$install_dir" ]; then
        target="$(cd "$install_dir" && pwd -P)/spaceterm"
    else
        target="$install_dir/spaceterm"
    fi
}

# The installed desktop app was verified when it was installed or updated, so it may answer.
installed_desktop_version_linux() {
    [ -e "$target" ] || return 0
    found="$("$target/bin/spaceterm-desktop" --build-info 2>/dev/null </dev/null | json_field version)" || true
    valid_version "$found" || die "the version of the SpaceTerm desktop app in $target could not be read"
    printf '%s' "$found"
}

refresh_desktop_database() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
    fi
}

install_desktop_linux() {
    archive="spaceterm-desktop-$version-linux-x86_64.tar.gz"
    step "Downloading SpaceTerm $version"
    fetch "$archive"
    fetch "$archive.sig" quiet
    verify_signature "$work/$archive" "$work/$archive.sig"

    mkdir -p "$install_dir" || die "could not create $install_dir"
    install_dir="$(cd "$install_dir" && pwd -P)"
    require_launchable "$install_dir"
    target="$install_dir/spaceterm"
    # Stage beside the destination so the final replacement is a rename on one filesystem.
    staging="$(mktemp -d "$install_dir/.spaceterm.install.XXXXXX")" \
        || die "could not write to $install_dir"
    tar -xzf "$work/$archive" -C "$staging" --no-same-owner --no-same-permissions \
        || die "could not unpack $archive"
    set -- "$staging"/*
    [ "$#" -eq 1 ] && [ "$(basename "$1")" = spaceterm ] && [ -x "$1/bin/spaceterm-desktop" ] \
        && [ -f "$1/share/applications/$DESKTOP_ID.desktop" ] \
        || die "$archive does not contain one SpaceTerm installation"
    mv "$1" "$staging/new"
    # The signature authenticates these bytes, not the version their asset name claims, so the
    # verified build itself must be the selected release. An older signed archive served under a
    # newer name is a downgrade.
    candidate="$("$staging/new/bin/spaceterm-desktop" --build-info 2>/dev/null </dev/null)" \
        || die "$archive does not report its build"
    candidate_version="$(printf '%s\n' "$candidate" | json_field version)"
    if ! valid_version "$candidate_version" \
        || [ "$(printf '%s\n' "$candidate" | json_field identity)" != spaceterm ] \
        || [ "$(printf '%s\n' "$candidate" | json_field target)" != x86_64-unknown-linux-gnu ]; then
        die "$archive does not hold a SpaceTerm release for this computer"
    fi
    candidate_plan="$(plan_product SpaceTerm "$desktop_before" "$candidate_version")"
    if [ "$candidate_plan" != install ] || [ "$candidate_version" != "$version" ]; then
        die "$archive holds SpaceTerm $candidate_version, not the SpaceTerm $version release"
    fi

    step "Installing SpaceTerm $version to $target"
    ensure_not_running_linux
    if [ -e "$target" ]; then
        previous=previous
        mv "$target" "$staging/$previous" || die "could not move the existing installation aside"
    fi
    mv "$staging/new" "$target" || die "could not install SpaceTerm"
    # The updater's leftovers belong to the replaced installation.
    rm -rf "$install_dir/.spaceterm.update"

    mkdir -p "$bin_dir" "$data_home/applications" "$data_home/icons/hicolor/scalable/apps"
    launcher="$bin_dir/spaceterm-desktop"
    if [ -e "$launcher" ] && [ ! -L "$launcher" ]; then
        warn "$launcher exists and is not a link, so it was left in place"
    else
        ln -sfn "$target/bin/spaceterm-desktop" "$launcher"
    fi
    # The icon follows the installation, which updates replace.
    ln -sfn "$target/share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg" \
        "$data_home/icons/hicolor/scalable/apps/$DESKTOP_ID.svg"
    exec="$(exec_value "$target/bin/spaceterm-desktop")"
    entry="$data_home/applications/$DESKTOP_ID.desktop"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            Exec=*) printf 'Exec=%s %%u\n' "$exec" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$target/share/applications/$DESKTOP_ID.desktop" >"$staging/entry"
    mv "$staging/entry" "$entry" || die "could not install the desktop entry"
    refresh_desktop_database
    step "Installed SpaceTerm $version to $target"
}

uninstall_desktop_linux() {
    linux_desktop_locations
    ensure_not_running_linux
    launcher="$bin_dir/spaceterm-desktop"
    if [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$target/bin/spaceterm-desktop" ]; then
        rm -f "$launcher"
    fi
    icon="$data_home/icons/hicolor/scalable/apps/$DESKTOP_ID.svg"
    if [ -L "$icon" ]; then
        rm -f "$icon"
    fi
    rm -f "$data_home/applications/$DESKTOP_ID.desktop"
    refresh_desktop_database
    rm -rf "$target" "$(dirname "$target")/.spaceterm.update"
    step "Removed SpaceTerm. Settings and other user data remain."
}

uninstall() {
    case "$(uname -s)" in
        Darwin) platform=macos-arm64 ;;
        Linux) platform=linux-x86_64 ;;
        *) die "SpaceTerm requires macOS or Linux" ;;
    esac
    require_home
    # Desktop checks come first, so a running desktop app stops the uninstall before Core goes.
    if [ "$desktop" = true ]; then
        if [ "$platform" = macos-arm64 ]; then
            macos_desktop_locations
            ensure_not_running_macos
        else
            linux_desktop_locations
            ensure_not_running_linux
        fi
    fi
    uninstall_core
    if [ "$desktop" = true ]; then
        if [ "$platform" = macos-arm64 ]; then
            uninstall_desktop_macos
        else
            uninstall_desktop_linux
        fi
    fi
}

install() {
    require_prepared
    detect_platform
    require_home
    require_commands curl tar gzip sed grep mktemp
    if [ "$platform" = macos-arm64 ]; then
        [ "$linger" = false ] || die "--linger applies only to Linux"
        require_commands shasum codesign
        if [ "$desktop" = true ]; then
            require_commands ditto hdiutil plutil ps
        fi
    else
        require_commands sha256sum openssl readlink
        if [ "$desktop" = true ] && ! has_vulkan; then
            warn "no Vulkan loader (libvulkan.so.1) was found; install your GPU's Vulkan driver before opening SpaceTerm"
        fi
    fi

    # Every refusal that needs no download comes before the first archive download.
    core_before="$(installed_core_version)"
    desktop_before=""
    if [ "$desktop" = true ]; then
        if [ "$platform" = macos-arm64 ]; then
            macos_desktop_locations
            desktop_before="$(installed_desktop_version_macos)"
        else
            linux_desktop_locations
            desktop_before="$(installed_desktop_version_linux)"
        fi
    fi
    find_release
    core_plan="$(plan_product "SpaceTerm Core" "$core_before" "$version")"
    desktop_plan=skip
    if [ "$desktop" = true ]; then
        desktop_plan="$(plan_product SpaceTerm "$desktop_before" "$version")"
        if [ "$desktop_plan" = install ]; then
            if [ "$platform" = macos-arm64 ]; then
                ensure_not_running_macos
            else
                ensure_not_running_linux
            fi
        fi
    fi

    if [ "$core_plan" = install ]; then
        install_core
    else
        step "SpaceTerm Core $version is already installed"
    fi
    if [ "$desktop" = true ]; then
        if [ "$desktop_plan" = skip ]; then
            step "SpaceTerm $version is already installed"
        elif [ "$platform" = macos-arm64 ]; then
            install_desktop_macos
        else
            install_desktop_linux
        fi
    fi
    if [ "$platform" = linux-x86_64 ]; then
        offer_linger
    fi
    modify_path

    if [ -z "$core_before" ]; then
        say "Run spaceterm setup to start SpaceTerm Core."
    elif [ "$core_plan" = install ]; then
        say "SpaceTerm Core $version is installed beside SpaceTerm Core $core_before. A running SpaceTerm Core keeps its version until it restarts."
    fi
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

desktop=false
requested_version=""
remove=false
modify_path=true
linger=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --desktop) desktop=true ;;
        --version)
            [ "$#" -ge 2 ] || usage_error "--version takes a version, such as 1.4.0"
            valid_version "$2" || usage_error "--version takes a version, such as 1.4.0"
            requested_version="$2"
            shift
            ;;
        --uninstall) remove=true ;;
        --no-modify-path) modify_path=false ;;
        --linger) linger=true ;;
        *) usage_error "unknown option: $1" ;;
    esac
    shift
done

if [ "$remove" = true ]; then
    [ -z "$requested_version" ] && [ "$modify_path" = true ] && [ "$linger" = false ] \
        || usage_error "--uninstall takes only --desktop"
    uninstall
else
    install
fi
