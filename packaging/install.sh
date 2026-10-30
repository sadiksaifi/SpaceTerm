#!/bin/sh
# Install the latest SpaceTerm release. Each release publishes this script as install.sh:
#
#   curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh
#
# macOS: curl does not quarantine downloads, so Gatekeeper does not block the self-signed app.
# SPACETERM_INSTALL_DIR selects a directory other than /Applications.
#
# Linux: SpaceTerm installs into ~/.local/lib/spaceterm with a launcher in ~/.local/bin and a
# desktop entry and icon under XDG_DATA_HOME. SPACETERM_INSTALL_DIR selects a directory other
# than ~/.local/lib. SpaceTerm updates this installation itself.
#
# Options:
#   --headless      install the headless command line without a desktop or service
#   --identity NAME select spaceterm-development or spaceterm-preflight for local archives
#   --signature FILE  detached signature for a local headless archive
#   --archive FILE  install a local archive, such as SpaceTerm Preflight, without downloading
#   --uninstall     remove the installation and leave settings and other user data in place

set -eu

readonly RELEASES="https://github.com/sadiksaifi/SpaceTerm/releases"
readonly MINIMUM_MACOS_MAJOR=26
readonly NOTARIZATION_NOTICE="By using SpaceTerm, you acknowledge that it's not notarized."
readonly MINIMUM_GLIBC_MAJOR=2
readonly MINIMUM_GLIBC_MINOR=35

work=""
volume=""
staging=""
previous=""
target=""

step() {
    printf '==> %s\n' "$1"
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
        # Restore the previous installation when the replacement did not finish.
        if [ -n "$previous" ] && [ -e "$staging/$previous" ] && [ ! -e "$target" ]; then
            mv "$staging/$previous" "$target" || true
        fi
        rm -rf "$staging"
    fi
    if [ -n "$work" ]; then
        rm -rf "$work"
    fi
}

download() {
    curl --fail --location --proto '=https' --tlsv1.2 "$@"
}

require_commands() {
    for command in "$@"; do
        command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
    done
}

# Sets archive, expected, and version from the latest release checksums. $1 is the archive
# suffix after the version, such as darwin-arm64.dmg.
latest_release() {
    work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"
    step "Finding the latest release"
    download --silent --show-error "$RELEASES/latest/download/SHA256SUMS" \
        --output "$work/SHA256SUMS" || die "could not download the release checksums"
    suffix="$(printf '%s' "$1" | sed 's/\./\\./g')"
    asset_prefix="${3:-SpaceTerm}"
    entry="$(grep -E "^[0-9a-f]{64}  $asset_prefix-[0-9]+\.[0-9]+\.[0-9]+-$suffix\$" "$work/SHA256SUMS")" \
        || die "the release checksums do not list a SpaceTerm $2"
    expected="${entry%%  *}"
    archive="${entry#*  }"
    version="${archive#"$asset_prefix"-}"
    version="${version%-"$1"}"
}

download_release() {
    step "Downloading SpaceTerm $version"
    # Download from the tagged release so a release published meanwhile cannot mix assets.
    download --progress-bar "$RELEASES/download/v$version/$archive" --output "$work/$archive" \
        || die "could not download $archive"
    actual="$("$@" "$work/$archive" | cut -d ' ' -f 1)"
    [ "$actual" = "$expected" ] || die "$archive does not match its published checksum"
}

# Replacing the bundle of a running application breaks it.
ensure_not_running_macos() {
    processes="$(ps -axww -o args=)"
    # A quoted pattern matches the install path literally, whatever characters it contains.
    case "$processes" in
        *"$target/Contents/MacOS/SpaceTerm"*) die "quit SpaceTerm, then run the installer again" ;;
    esac
}

install_macos() {
    [ "$#" -eq 0 ] || die "the macOS installer takes no options"
    [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ] \
        || die "SpaceTerm requires a Mac with Apple silicon"
    macos_major="$(sw_vers -productVersion | cut -d . -f 1)"
    [ "$macos_major" -ge "$MINIMUM_MACOS_MAJOR" ] \
        || die "SpaceTerm requires macOS $MINIMUM_MACOS_MAJOR or newer"
    require_commands curl ditto hdiutil ps shasum

    if [ -n "${SPACETERM_INSTALL_DIR:-}" ]; then
        install_dir="$SPACETERM_INSTALL_DIR"
    elif [ -w /Applications ]; then
        install_dir=/Applications
    else
        install_dir="$HOME/Applications"
    fi
    target="$install_dir/SpaceTerm.app"
    ensure_not_running_macos

    latest_release darwin-arm64.dmg "disk image"
    download_release shasum -a 256

    step "Installing to $install_dir"
    # macOS 27 warns that this hdiutil form is deprecated; its replacement may not exist on
    # macOS 26. Show hdiutil's diagnostics only when attaching fails.
    if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$work/volume" \
        "$work/$archive" >/dev/null 2>"$work/attach.log"; then
        cat "$work/attach.log" >&2
        die "could not open $archive"
    fi
    volume="$work/volume"
    [ -d "$volume/SpaceTerm.app" ] || die "$archive does not contain SpaceTerm.app"
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
    printf '%s\n' "$NOTARIZATION_NOTICE"
}

# A running process keeps its executable open, and its name in /proc/PID/exe, so the check
# matches the installed executable whichever launcher or symlink started it. A process the
# updater replaced but has not relaunched runs from the updater's staging directory.
ensure_not_running_linux() {
    executable="$target/bin/spaceterm-desktop"
    replaced="$(dirname "$target")/.$(basename "$target").update/"
    for link in /proc/[0-9]*/exe; do
        case "$(readlink "$link" 2>/dev/null || true)" in
            "$executable" | "$replaced"*) die "quit $display_name, then run the installer again" ;;
        esac
    done
}

require_supported_linux() {
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
    if ! has_vulkan; then
        warn "no Vulkan loader (libvulkan.so.1) was found; install your GPU's Vulkan driver before opening SpaceTerm"
    fi
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

linux_locations() {
    case "${HOME:-}" in
        /*) ;;
        *) die "an absolute HOME is required" ;;
    esac
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
    bin_dir="$HOME/.local/bin"
}

# Reads the identity of an installed tree from its desktop entry.
read_identity() {
    set -- "$1"/share/applications/*.desktop
    [ "$#" -eq 1 ] && [ -f "$1" ] || die "the archive does not contain one desktop entry"
    application_id="$(basename "$1" .desktop)"
    display_name="$(sed -n 's/^Name=//p' "$1" | head -n 1)"
    [ -n "$display_name" ] || die "the archive desktop entry has no name"
}

uninstall_linux() {
    linux_locations
    name=spaceterm
    application_id=io.github.sadiksaifi.spaceterm
    display_name=SpaceTerm
    if [ -d "$install_dir" ]; then
        target="$(cd "$install_dir" && pwd -P)/$name"
    else
        target="$install_dir/$name"
    fi
    ensure_not_running_linux
    step "Removing $display_name"
    launcher="$bin_dir/$name-desktop"
    if [ -L "$launcher" ] && [ "$(readlink "$launcher")" = "$target/bin/spaceterm-desktop" ]; then
        rm -f "$launcher"
    fi
    icon="$data_home/icons/hicolor/scalable/apps/$application_id.svg"
    if [ -L "$icon" ]; then
        rm -f "$icon"
    fi
    rm -f "$data_home/applications/$application_id.desktop"
    refresh_desktop_database
    rm -rf "$target" "$(dirname "$target")/.$name.update"
    step "Removed $display_name. Settings and other user data remain."
}

refresh_desktop_database() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
    fi
}

install_linux() {
    archive_file=""
    case "${1:-}" in
        "") ;;
        --uninstall)
            [ "$#" -eq 1 ] || die "--uninstall takes no arguments"
            uninstall_linux
            return
            ;;
        --archive)
            [ "$#" -eq 2 ] || die "--archive takes one archive"
            archive_file="$2"
            ;;
        *) die "unknown option: $1" ;;
    esac
    require_supported_linux
    require_commands tar gzip readlink sed
    linux_locations

    if [ -n "$archive_file" ]; then
        [ -f "$archive_file" ] || die "the archive does not exist"
        work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"
        cp "$archive_file" "$work/archive.tar.gz"
        archive="archive.tar.gz"
        version="from the local archive"
    else
        require_commands curl sha256sum
        # Refuse before downloading; the unpacked identity is checked again before replacement.
        display_name=SpaceTerm
        target="$install_dir/spaceterm"
        if [ -d "$install_dir" ]; then
            target="$(cd "$install_dir" && pwd -P)/spaceterm"
        fi
        ensure_not_running_linux
        latest_release linux-x86_64.tar.gz "Linux archive"
        download_release sha256sum
    fi

    mkdir -p "$install_dir" || die "could not create $install_dir"
    install_dir="$(cd "$install_dir" && pwd -P)"
    require_launchable "$install_dir"
    # Stage beside the destination so the final replacement is a rename on one filesystem.
    staging="$(mktemp -d "$install_dir/.spaceterm.install.XXXXXX")" \
        || die "could not write to $install_dir"
    tar -xzf "$work/$archive" -C "$staging" --no-same-owner --no-same-permissions \
        || die "could not unpack $archive"
    set -- "$staging"/*
    [ "$#" -eq 1 ] && [ -x "$1/bin/spaceterm-desktop" ] && [ -d "$1/share/spaceterm" ] \
        || die "the archive does not contain one SpaceTerm installation"
    name="$(basename "$1")"
    case "$name" in
        spaceterm | spaceterm-preflight) ;;
        *) die "the archive does not contain one SpaceTerm installation" ;;
    esac
    mv "$1" "$staging/new"
    read_identity "$staging/new"
    target="$install_dir/$name"

    step "Installing $display_name $version to $target"
    ensure_not_running_linux
    if [ -e "$target" ]; then
        previous=previous
        mv "$target" "$staging/$previous" || die "could not move the existing installation aside"
    fi
    mv "$staging/new" "$target" || die "could not install $display_name"
    # The updater's leftovers belong to the replaced installation.
    rm -rf "$install_dir/.$name.update"

    mkdir -p "$bin_dir" "$data_home/applications" "$data_home/icons/hicolor/scalable/apps"
    launcher="$bin_dir/$name-desktop"
    if [ -e "$launcher" ] && [ ! -L "$launcher" ]; then
        warn "$launcher exists and is not a link, so it was left in place"
    else
        ln -sfn "$target/bin/spaceterm-desktop" "$launcher"
    fi
    # The icon follows the installation, which updates replace.
    ln -sfn "$target/share/icons/hicolor/scalable/apps/$application_id.svg" \
        "$data_home/icons/hicolor/scalable/apps/$application_id.svg"
    exec="$(exec_value "$target/bin/spaceterm-desktop")"
    entry="$data_home/applications/$application_id.desktop"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            Exec=*) printf 'Exec=%s %%u\n' "$exec" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <"$target/share/applications/$application_id.desktop" >"$staging/entry"
    mv "$staging/entry" "$entry" || die "could not install the desktop entry"
    refresh_desktop_database

    step "Installed $display_name to $target"
    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *) printf 'Add %s to PATH to run %s from a terminal.\n' "$bin_dir" "$name" ;;
    esac
}

# Checksums arrive over HTTPS before any downloaded executable runs. The retained CLI then
# authenticates the original Ed25519 signature and every resource before publishing a runtime.
# [Signing] Add the Developer ID requirement check with the signing and notarization rollout.
retain_headless() {
    mkdir "$work/runtime" 2>/dev/null || die "could not prepare the CLI installation"
    # Refuse links and parent paths before extraction. The Rust verifier enforces the complete
    # signed layout before retention; tar never writes outside this private staging directory.
    tar -tzf "$headless_archive" > "$work/members" 2>/dev/null || die "could not read the CLI archive"
    if grep -Ev '^server(/|$)' "$work/members" >/dev/null \
        || grep -E '(^|/)\.\.?(/|$)' "$work/members" >/dev/null; then
        die "the CLI archive layout is invalid"
    fi
    tar -tvzf "$headless_archive" > "$work/kinds" 2>/dev/null || die "could not read the CLI archive"
    if cut -c 1 "$work/kinds" | grep -Ev '^[-d]$' >/dev/null; then
        die "the CLI archive must contain only files and directories"
    fi
    tar -xzf "$headless_archive" -C "$work/runtime" 2>/dev/null || die "could not unpack the CLI archive"
    runtime="$work/runtime/server"
    [ -x "$runtime/bin/spaceterm" ] || die "the CLI executable is missing"
    if [ -n "${headless_identity:-}" ]; then
        compiled_identity="$("$runtime/bin/spaceterm" server provision build-info | sed -n 's/.*"identity":"\([a-z-]*\)".*/\1/p')"
        [ "$compiled_identity" = "$headless_identity" ] || die "the CLI archive has a different application identity"
    fi
    cp "$headless_archive" "$runtime/artifact.tar.gz" 2>/dev/null || die "could not stage the CLI archive"
    cp "$headless_signature" "$runtime/artifact.sig" 2>/dev/null || die "could not stage the CLI signature"
    "$runtime/bin/spaceterm" server provision retain "$runtime" \
        || die "the signed CLI installation could not be retained"
}

headless_uninstall() {
    root="$HOME/.local/share/$headless_identity/server"
    launcher="$HOME/.local/bin/$headless_identity"
    runtime=""
    if [ -L "$launcher" ]; then
        linked="$(readlink "$launcher")"
        case "$linked" in
            "$root/installations/"*/bin/spaceterm) runtime="${linked%/bin/spaceterm}" ;;
        esac
    fi
    if [ -z "$runtime" ] && [ -f "$root/service.json" ] && [ ! -L "$root/service.json" ]; then
        digest="$(sed -n 's/.*"installation"[[:space:]]*:[[:space:]]*"\([0-9a-f]\{64\}\)".*/\1/p' "$root/service.json")"
        [ "${#digest}" -eq 64 ] || die "the service installation could not be read"
        runtime="$root/installations/$digest"
    fi
    if [ -z "$runtime" ]; then
        step "The CLI launcher and service are already uninstalled"
        return
    fi
    digest="$(basename "$runtime")"
    [ "${#digest}" -eq 64 ] || die "the CLI launcher is unmanaged"
    case "$digest" in *[!0-9a-f]*) die "the CLI launcher is unmanaged" ;; esac
    for directory in "$HOME/.local" "$HOME/.local/share" "${root%/server}" "$root" "$root/installations" "$runtime" "$runtime/bin" "$runtime/bin/spaceterm"; do
        [ ! -L "$directory" ] || die "the CLI installation has unsafe links"
    done
    if [ -n "$headless_confirm" ]; then
        "$runtime/bin/spaceterm" server provision uninstall --confirm "$headless_confirm"
        return
    fi
    status=0
    preview="$("$runtime/bin/spaceterm" server provision uninstall)" || status=$?
    printf '%s\n' "$preview"
    case "$status" in
        0) return ;;
        3) ;;
        *) die "the service could not be uninstalled" ;;
    esac
    token="$(printf '%s\n' "$preview" | sed -n 's/.*--confirm \(c1-[0-9a-f]\{64\}\).*/\1/p')"
    [ -n "$token" ] || die "the uninstall preview has no confirmation token"
    if [ -t 0 ]; then
        printf 'Uninstall this server? [y/N] ' >&2
        IFS= read -r answer
    elif ( : </dev/tty ) 2>/dev/null; then
        printf 'Uninstall this server? [y/N] ' >/dev/tty
        IFS= read -r answer </dev/tty
    else
        die "review the preview, then run this installer with --headless --uninstall --confirm $token"
    fi
    case "$answer" in
        y|Y|yes|YES) "$runtime/bin/spaceterm" server provision uninstall --confirm "$token" ;;
        *) die "uninstall cancelled" ;;
    esac
}

install_headless() {
    # Installer steps always act on this account, independently of CLI work preferences.
    SPACETERM_SERVER=local
    export SPACETERM_SERVER
    headless_archive=""
    headless_signature=""
    headless_identity=spaceterm
    headless_confirm=""
    headless_remove=false
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --headless) shift ;;
            --uninstall) headless_remove=true; shift ;;
            --archive) [ "$#" -ge 2 ] || die "--archive takes one file"; headless_archive="$2"; shift 2 ;;
            --signature) [ "$#" -ge 2 ] || die "--signature takes one file"; headless_signature="$2"; shift 2 ;;
            --identity) [ "$#" -ge 2 ] || die "--identity takes one name"; headless_identity="$2"; shift 2 ;;
            --confirm) [ "$#" -ge 2 ] || die "--confirm takes one token"; headless_confirm="$2"; shift 2 ;;
            *) die "unknown option" ;;
        esac
    done
    case "$headless_identity" in spaceterm|spaceterm-preflight|spaceterm-development) ;; *) die "unsupported CLI identity" ;; esac
    case "${HOME:-}" in /*) ;; *) die "an absolute HOME is required" ;; esac
    if "$headless_remove"; then
        [ -z "$headless_archive$headless_signature" ] || die "uninstall takes no archive"
        headless_uninstall
        return
    fi
    [ -z "$headless_confirm" ] || die "--confirm requires --uninstall"
    case "$(uname -s):$(uname -m)" in
        Darwin:arm64)
            [ "$(sw_vers -productVersion | cut -d . -f 1)" -ge "$MINIMUM_MACOS_MAJOR" ] || die "SpaceTerm requires macOS $MINIMUM_MACOS_MAJOR or newer"
            headless_target=darwin-arm64 ;;
        Linux:x86_64)
            # Headless installs need no Vulkan loader, GPU or display server.
            glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
            case "$glibc" in "glibc "*) ;; *) die "SpaceTerm requires glibc 2.35 or newer" ;; esac
            glibc="${glibc#glibc }"; major="${glibc%%.*}"; minor="${glibc#*.}"; minor="${minor%%.*}"
            if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then die "SpaceTerm requires glibc 2.35 or newer"; fi
            headless_target=linux-x86_64 ;;
        *) die "SpaceTerm requires Apple silicon macOS or x86_64 glibc Linux" ;;
    esac
    require_commands tar gzip readlink sed
    if [ -n "$headless_archive" ]; then
        [ -f "$headless_archive" ] && [ -f "$headless_signature" ] || die "a local CLI archive requires its detached --signature"
        work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"
        cp "$headless_archive" "$work/artifact.tar.gz"
        cp "$headless_signature" "$work/artifact.sig"
        headless_archive="$work/artifact.tar.gz"
        headless_signature="$work/artifact.sig"
    else
        [ "$headless_identity" = spaceterm ] && [ -z "$headless_signature" ] || die "nonproduction CLI installs require a local archive and signature"
        require_commands curl
        latest_release "$headless_target.tar.gz" "CLI archive" spaceterm
        if [ "$(uname -s)" = Darwin ]; then download_release shasum -a 256; else download_release sha256sum; fi
        headless_archive="$work/$archive"
        headless_signature="$work/$archive.sig"
        download --silent --show-error "$RELEASES/download/v$version/$archive.sig" --output "$headless_signature" || die "could not download the CLI signature"
        expected_signature="$(sed -n "s/^\([0-9a-f]\{64\}\)  $archive.sig$/\1/p" "$work/SHA256SUMS")"
        if [ "$(uname -s)" = Darwin ]; then actual_signature="$(shasum -a 256 "$headless_signature" | cut -d ' ' -f 1)"; else actual_signature="$(sha256sum "$headless_signature" | cut -d ' ' -f 1)"; fi
        [ "$actual_signature" = "$expected_signature" ] || die "the CLI signature differs from the published checksum"
    fi
    retain_headless
    step "Installed the $headless_identity CLI. Run $headless_identity server setup to configure the server."
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for option in "$@"; do
    if [ "$option" = --headless ]; then
        install_headless "$@"
        exit 0
    fi
done

case "$(uname -s)" in
    Darwin) install_macos "$@" ;;
    Linux) install_linux "$@" ;;
    *) die "SpaceTerm requires macOS or Linux" ;;
esac
