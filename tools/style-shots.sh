#!/usr/bin/env bash
# Patch style fixtures, headless: four decks, stems, the AI timeline. Never
# starts the engine or changes the desktop. Usage:
#   PATCH_SHOT_SIZES=647x712,857x712 tools/style-shots.sh /tmp/shots [deck]
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"
export UNIT_THEME_DIR="$repo_dir/themes/patch-dark"
export PATCH_SHOT_MISSION=1
export PATCH_PROVIDER=mock
shot_config=$(mktemp -d)
trap 'rm -rf -- "$shot_config"' EXIT
export XDG_CONFIG_HOME="$shot_config"
export PATCH_STYLE_SHOTS=1
cargo run --release --locked -q -p patch-surfaces --example style-shots -- "${1:-/tmp/patch-style-shots}" "${2:-}"
