#!/bin/sh
# Install the latest SpaceTerm release. Each release publishes this script as install.sh:
#
#   curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh
#
# curl does not quarantine downloads, so Gatekeeper does not block the ad hoc signed app; see
# ADR 0009. SPACETERM_INSTALL_DIR selects a directory other than /Applications.

set -eu

readonly RELEASES="https://github.com/sadiksaifi/SpaceTerm/releases"
readonly MINIMUM_MACOS_MAJOR=26
readonly NOTARIZATION_NOTICE="By using SpaceTerm, you acknowledge that it's not notarized."

work=""
volume=""
staging=""

step() {
    printf '==> %s\n' "$1"
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
        if [ -e "$staging/previous.app" ] && [ ! -e "$target" ]; then
            mv "$staging/previous.app" "$target" || true
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

# Replacing the bundle of a running application breaks it.
ensure_not_running() {
    processes="$(ps -axww -o args=)"
    # A quoted pattern matches the install path literally, whatever characters it contains.
    case "$processes" in
        *"$target/Contents/MacOS/SpaceTerm"*) die "quit SpaceTerm, then run the installer again" ;;
    esac
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ "$(uname -s)" = Darwin ] || die "SpaceTerm requires macOS"
[ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ] \
    || die "SpaceTerm requires a Mac with Apple silicon"
macos_major="$(sw_vers -productVersion | cut -d . -f 1)"
[ "$macos_major" -ge "$MINIMUM_MACOS_MAJOR" ] \
    || die "SpaceTerm requires macOS $MINIMUM_MACOS_MAJOR or newer"
for command in curl ditto hdiutil ps shasum; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done

if [ -n "${SPACETERM_INSTALL_DIR:-}" ]; then
    install_dir="$SPACETERM_INSTALL_DIR"
elif [ -w /Applications ]; then
    install_dir=/Applications
else
    install_dir="$HOME/Applications"
fi
target="$install_dir/SpaceTerm.app"
ensure_not_running

work="$(mktemp -d "${TMPDIR:-/tmp}/spaceterm-install.XXXXXX")"

step "Finding the latest release"
download --silent --show-error "$RELEASES/latest/download/SHA256SUMS" --output "$work/SHA256SUMS" \
    || die "could not download the release checksums"
entry="$(grep -E '^[0-9a-f]{64}  SpaceTerm-[0-9]+\.[0-9]+\.[0-9]+-darwin-arm64\.dmg$' "$work/SHA256SUMS")" \
    || die "the release checksums do not list a SpaceTerm disk image"
expected="${entry%%  *}"
archive="${entry#*  }"
version="${archive#SpaceTerm-}"
version="${version%-darwin-arm64.dmg}"

step "Downloading SpaceTerm $version"
# Download from the tagged release so a release published meanwhile cannot mix assets.
download --progress-bar "$RELEASES/download/v$version/$archive" --output "$work/$archive" \
    || die "could not download $archive"
actual="$(shasum -a 256 "$work/$archive" | cut -d ' ' -f 1)"
[ "$actual" = "$expected" ] || die "$archive does not match its published checksum"

step "Installing to $install_dir"
# macOS 27 warns that this hdiutil form is deprecated; its replacement may not exist on macOS 26.
# Show hdiutil's diagnostics only when attaching fails.
if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$work/volume" "$work/$archive" \
    >/dev/null 2>"$work/attach.log"; then
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
ensure_not_running
if [ -e "$target" ]; then
    mv "$target" "$staging/previous.app" || die "could not move the existing SpaceTerm.app aside"
fi
mv "$staging/SpaceTerm.app" "$target" || die "could not install SpaceTerm.app"

step "Installed SpaceTerm $version to $target"
printf '%s\n' "$NOTARIZATION_NOTICE"
