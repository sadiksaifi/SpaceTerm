#!/bin/sh
set -eu

# Run in a dedicated SpaceTerm Pane. A scenario name selects one visual page;
# the default walks through all pages interactively.
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$script_dir/kitty-graphics-smoke.py" "$@"
