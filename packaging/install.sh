#!/bin/sh
# Install SpaceTerm Core on x86_64 Linux; --desktop also installs the desktop app.
# Download install.sh from the desired tagged release, then run it with sh.
# Each published installer pins that release; --version selects another explicit version.
# Core lives in ~/.local/share/spaceterm/core, the desktop in ~/.local/lib/spaceterm,
# and their launchers in ~/.local/bin. SPACETERM_INSTALL_DIR overrides the desktop parent.
# --uninstall retains user data. --no-modify-path leaves shell profiles unchanged.
# --linger enables systemd linger without prompting. Downloads must pass both their
# SHA-256 checksum and an Ed25519 signature before extraction or execution.

set -eu

# release:assemble fills these values for this release, including dry runs.
readonly RELEASE_KIND='@RELEASE_KIND@'
readonly DEFAULT_VERSION='@DEFAULT_VERSION@'
# packaging/update-trust.json owns these keys; assembly checks them before publishing.
readonly UPDATE_KEY_CURRENT='sNR7I+ZSfwiz/hBiYA0PgrtqqOsJY53yluVe93/tATM='
readonly UPDATE_KEY_BACKUP='EprlnUGQNGvhtuT8fhIGmVMJpAm1rjGfgkmts2Beesk='

readonly RELEASES="https://github.com/sadiksaifi/SpaceTerm/releases"
readonly MINIMUM_GLIBC_MAJOR=2
readonly MINIMUM_GLIBC_MINOR=35
# Linux application ID.
readonly DESKTOP_ID=com.peblon.spaceterm
readonly PROFILE_COMMENT="# Added by the SpaceTerm installer"

work=""
legacy_launcher=""
legacy_tree=false
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
    if [ -n "$legacy_launcher" ] && [ ! -e "$bin_dir/spaceterm" ] && [ ! -L "$bin_dir/spaceterm" ]; then
        mv "$legacy_launcher" "$bin_dir/spaceterm" || true
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

require_prepared() {
    case "$RELEASE_KIND" in
        release) ;;
        dry-run) warn "this installer comes from a dry run and trusts only its test signing key" ;;
        *) die "this installer was not prepared by a release; download install.sh from $RELEASES" ;;
    esac
    valid_version "$DEFAULT_VERSION" || die "this installer has no pinned release version"
    if ! valid_key "$UPDATE_KEY_CURRENT" || ! valid_key "$UPDATE_KEY_BACKUP"; then
        die "this installer has placeholder update keys; download install.sh from $RELEASES"
    fi
}

require_linux() {
    [ "$(uname -s)" = Linux ] || die "this installer requires Linux; on macOS use the notarized DMG or Homebrew cask"
    platform=linux-x86_64
}

detect_platform() {
    case "$(uname -s)" in
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
        *) die "this installer requires Linux" ;;
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
    sha256sum "$1" | cut -d ' ' -f 1
}

# Prints the first value of a string field in a JSON document, such as build.json.
json_field() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" | head -n 1
}

# Tagged URLs work while greenfield releases are held with --latest=false.
find_release() {
    work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"
    version="${requested_version:-$DEFAULT_VERSION}"
    step "Finding SpaceTerm $version"
    download --silent --show-error "$RELEASES/download/v$version/SHA256SUMS" --output "$work/SHA256SUMS" \
        || die "could not download the release checksums"
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
    for key in "$UPDATE_KEY_CURRENT" "$UPDATE_KEY_BACKUP"; do
        valid_key "$key" || continue
        {
            printf '\060\052\060\005\006\003\053\145\160\003\041\000'
            printf '%s\n' "$key" | openssl base64 -d -A
        } >"$work/key.der"
        if openssl pkeyutl -verify -pubin -keyform DER -inkey "$work/key.der" -rawin \
            -in "$1" -sigfile "$2" >/dev/null 2>&1; then
            return
        fi
    done
    die "$(basename "$1") does not carry a valid SpaceTerm signature"
}

# Every Core command takes --machine local regardless of SPACETERM_MACHINE.
core_executable() {
    printf '%s' bin/spaceterm
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
    verify_signature "$work/$archive" "$work/$archive.sig"
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
    # The Update Floor: the oldest version that updates directly to this release.
    floor="$(json_field floor <"$runtime/build.json")"
    valid_version "$floor" || die "$archive names no valid Update Floor"
    if [ -n "$core_running" ] && [ "$(compare_versions "$core_running" "$floor")" = older ]; then
        die "SpaceTerm Core $core_running is older than $floor, the oldest version that updates directly to SpaceTerm Core $version; install SpaceTerm Core $floor first with --version $floor"
    fi
    executable="$runtime/$(core_executable)"
    [ -f "$executable" ] && [ -x "$executable" ] || die "$archive has no SpaceTerm Core executable"
    cp "$work/$archive" "$runtime/artifact.tar.gz"
    cp "$work/$archive.sig" "$runtime/artifact.sig"
    step "Installing SpaceTerm Core $version"
    # The legacy desktop occupied the CLI's launcher path. Move only its exact link.
    if is_legacy_launcher; then
        legacy_launcher="$work/legacy-launcher"
        mv "$bin_dir/spaceterm" "$legacy_launcher"
    fi
    "$executable" --machine local internal retain "$runtime" || die "SpaceTerm Core $version could not be installed"
    legacy_launcher=""
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

# Sets core_running to the version of the installation the SpaceTerm Core service record approves,
# the one that runs or starts next, or to "" when no SpaceTerm Core is registered. The launcher
# is not consulted: an activation that could not replace it leaves it naming another
# installation, possibly a pruned one. A Pending Version beside the approved one has not run, so
# it never meets an Update Floor.
running_core_version() {
    core_running=""
    record="$core_data/service.json"
    [ -e "$record" ] || [ -L "$record" ] || return 0
    unreadable="the SpaceTerm Core service record could not be read"
    [ -f "$record" ] && [ ! -L "$record" ] || die "$unreadable"
    digest="$(json_field installation <"$record")"
    [ "${#digest}" -eq 64 ] || die "$unreadable"
    case "$digest" in *[!0-9a-f]*) die "$unreadable" ;; esac
    approved="$core_data/installations/$digest"
    [ -f "$approved/build.json" ] || die "the SpaceTerm Core installation the service record approves is missing"
    core_running="$(json_field version <"$approved/build.json")"
    valid_version "$core_running" || die "the running SpaceTerm Core version could not be read"
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
        bash) profile="$HOME/.bashrc" ;;
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

# A running process keeps its executable open, and its name in /proc/PID/exe, so the check
# matches the installed executable whichever launcher or symlink started it. A process the
# updater replaced but has not relaunched runs from the updater's staging directory.
ensure_not_running_linux() {
    executable="$target/bin/spaceterm-desktop"
    replaced="$(dirname "$target")/.$(basename "$target").update/"
    for link in /proc/[0-9]*/exe; do
        case "$(readlink "$link" 2>/dev/null || true)" in
            "$executable" | "$target/bin/spaceterm" | "$replaced"*) die "quit SpaceTerm, then run the installer again" ;;
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
    # A custom parent must not turn program replacement into deletion of retained data.
    resolved="$(readlink -m "$target")"
    config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
    state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
    cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"
    case "$config_home" in /*) ;; *) config_home="$HOME/.config" ;; esac
    case "$state_home" in /*) ;; *) state_home="$HOME/.local/state" ;; esac
    case "$cache_home" in /*) ;; *) cache_home="$HOME/.cache" ;; esac
    for root in "$config_home/spaceterm" "$state_home/spaceterm" "$cache_home/spaceterm" \
        "$data_home/spaceterm" "$HOME/.local/share/spaceterm"; do
        retained="$(readlink -m "$root")"
        case "$resolved/" in
            "$retained/"*) die "the install path overlaps retained SpaceTerm data" ;;
        esac
        case "$retained/" in
            "$resolved/"*) die "the install path overlaps retained SpaceTerm data" ;;
        esac
    done
}

# The installed desktop app was verified when it was installed or updated, so it may answer.
installed_desktop_version_linux() {
    [ -e "$target" ] || return 0
    [ "$legacy_tree" = false ] || return 0
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

    mkdir -p "$bin_dir" "$data_home/applications"
    launcher="$bin_dir/spaceterm-desktop"
    if [ -e "$launcher" ] && [ ! -L "$launcher" ]; then
        warn "$launcher exists and is not a link, so it was left in place"
    else
        ln -sfn "$target/bin/spaceterm-desktop" "$launcher"
    fi
    exec="$(exec_value "$target/bin/spaceterm-desktop")"
    # The entry names the installation's own icon, so it follows every update without links.
    icon="$target/share/icons/hicolor/scalable/apps/$DESKTOP_ID.svg"
    entry="$data_home/applications/$DESKTOP_ID.desktop"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            Exec=*) printf 'Exec=%s %%u\n' "$exec" ;;
            Icon=*) printf 'Icon=%s\n' "$(printf '%s' "$icon" | sed 's/\\/\\\\/g')" ;;
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
    rm -f "$data_home/applications/$DESKTOP_ID.desktop"
    refresh_desktop_database
    rm -rf "$target" "$(dirname "$target")/.spaceterm.update"
    step "Removed SpaceTerm. Settings and other user data remain."
}

# Legacy releases installed no Core. Recognize their packaged desktop entry without
# executing the old desktop as a CLI or reading any retained application data.
locate_legacy_desktop() {
    legacy_tree=false
    if [ -f "$target/share/applications/io.github.sadiksaifi.spaceterm.desktop" ] \
        && [ -f "$target/bin/spaceterm" ] && [ ! -e "$target/bin/spaceterm-desktop" ]; then
        legacy_tree=true
    fi
}

is_legacy_launcher() {
    [ "$legacy_tree" = true ] && [ -L "$bin_dir/spaceterm" ] \
        && [ "$(readlink -f "$bin_dir/spaceterm")" = "$(readlink -f "$target/bin/spaceterm")" ]
}

clean_legacy_launchers() {
    rm -f "$data_home/applications/io.github.sadiksaifi.spaceterm.desktop"
    for icon in "$data_home"/icons/hicolor/*/apps/io.github.sadiksaifi.spaceterm.png \
        "$data_home"/icons/hicolor/*/apps/io.github.sadiksaifi.spaceterm.svg \
        "$data_home"/icons/hicolor/*/apps/io.github.sadiksaifi.spaceterm-symbolic.svg \
        "$data_home"/icons/hicolor/*/apps/io.github.sadiksaifi.spaceterm-attention-symbolic.svg; do
        [ -e "$icon" ] || [ -L "$icon" ] || continue
        rm -f "$icon"
    done
    refresh_desktop_database
}

uninstall() {
    require_linux
    require_home
    if [ "$desktop" = true ]; then
        linux_desktop_locations
        locate_legacy_desktop
        ensure_not_running_linux
    fi
    uninstall_core
    if [ "$desktop" = true ]; then
        if is_legacy_launcher; then
            rm -f "$bin_dir/spaceterm"
        fi
        uninstall_desktop_linux
        clean_legacy_launchers
    fi
}

install() {
    require_linux
    require_prepared
    detect_platform
    require_home
    require_commands curl tar gzip sed grep mktemp sha256sum openssl readlink
    if [ "$desktop" = true ] && ! has_vulkan; then
        warn "no Vulkan loader (libvulkan.so.1) was found; install your GPU's Vulkan driver before opening SpaceTerm"
    fi

    core_before="$(installed_core_version)"
    running_core_version
    desktop_before=""
    linux_desktop_locations
    locate_legacy_desktop
    if [ "$legacy_tree" = true ]; then
        ensure_not_running_linux
    fi
    if [ "$desktop" = true ]; then
        desktop_before="$(installed_desktop_version_linux)"
    fi
    find_release
    core_plan="$(plan_product "SpaceTerm Core" "$core_before" "$version")"
    # Re-admit the verified Core when an obsolete desktop still owns its launcher.
    if is_legacy_launcher; then
        core_plan=install
    fi
    desktop_plan=skip
    if [ "$desktop" = true ]; then
        desktop_plan="$(plan_product SpaceTerm "$desktop_before" "$version")"
        if [ "$desktop_plan" = install ]; then
            ensure_not_running_linux
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
        else
            install_desktop_linux
        fi
        clean_legacy_launchers
    elif [ "$legacy_tree" = true ]; then
        # A Core-only installation replaces the obsolete desktop with the Core CLI.
        ensure_not_running_linux
        rm -rf "$target" "$install_dir/.spaceterm.update"
        clean_legacy_launchers
    fi
    offer_linger
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
