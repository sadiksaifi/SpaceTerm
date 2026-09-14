#!/bin/bash
set -euo pipefail

# A distinct development identity keeps source-build inspection separate from installed apps.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_dir="$(cd -- "$script_dir/.." && pwd -P)"
artifact_path=$(mktemp "${TMPDIR:-/tmp}/spaceterm-appearance-executable.XXXXXX")
trap 'rm -f -- "$artifact_path"' EXIT HUP INT TERM

build_and_package() {
    "$script_dir/cargo-artifacts.sh" run -- python3 "$script_dir/cargo-build-executable.py" \
        --output "$artifact_path" --bin spaceterm -- \
        --manifest-path "$repo_dir/Cargo.toml" --features appearance-exerciser --locked
    executable=$(cat "$artifact_path")
    bundle="$(dirname -- "$(dirname -- "$executable")")/appearance-exerciser/SpaceTerm Appearance.app"
    mkdir -p "$bundle/Contents/MacOS"
    install -m 0644 "$repo_dir/packaging/macos/AppearanceExerciser-Info.plist" "$bundle/Contents/Info.plist"
    install -m 0755 "$executable" "$bundle/Contents/MacOS/SpaceTerm Appearance"
}

if [ "$#" -ne 0 ]; then
    echo "usage: dev-appearance-macos.sh" >&2
    exit 2
fi

build_and_package
rm -f -- "$artifact_path"

exec env SPACETERM_APPEARANCE_EXERCISER=1 "$bundle/Contents/MacOS/SpaceTerm Appearance"
