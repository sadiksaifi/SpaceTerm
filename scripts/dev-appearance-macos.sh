#!/bin/bash
set -euo pipefail

# A distinct development identity keeps source-build inspection separate from installed apps.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_dir="$(cd -- "$script_dir/.." && pwd -P)"
bundle="$repo_dir/target/appearance-exerciser/SpaceTerm Appearance.app"

cargo build --manifest-path "$repo_dir/Cargo.toml" --features appearance-exerciser --locked
mkdir -p "$bundle/Contents/MacOS"
install -m 0644 "$repo_dir/packaging/macos/AppearanceExerciser-Info.plist" "$bundle/Contents/Info.plist"
install -m 0755 "$repo_dir/target/debug/spaceterm" "$bundle/Contents/MacOS/SpaceTerm Appearance"
exec env SPACETERM_APPEARANCE_EXERCISER=1 "$bundle/Contents/MacOS/SpaceTerm Appearance"
