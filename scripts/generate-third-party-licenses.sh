#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:?usage: generate-third-party-licenses.sh <rust-target> <output-dir>}
out=${2:?usage: generate-third-party-licenses.sh <rust-target> <output-dir>}
mkdir -p "$out"
cargo about -L error generate --workspace --locked --target "$target" about.hbs \
  -o "$out/THIRD_PARTY_LICENSES.html"
cargo metadata --locked --format-version 1 --filter-platform "$target" \
  > "$out/cargo-metadata.json"
python3 scripts/write-third-party-license-notices.py \
  "$out/cargo-metadata.json" "$out/THIRD_PARTY_LICENSES-CUSTOM.txt"
rm "$out/cargo-metadata.json"
