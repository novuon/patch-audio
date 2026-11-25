#!/usr/bin/env bash
# Build Patch's generic Linux kit: the kit for every distribution other
# than Omarchy, built in an Ubuntu 24.04 container so it runs on any
# distribution at least that new (packaging/generic/Containerfile).
#
#   scripts/build-generic.sh            dist/patch-<v>-test-<commit>-linux-generic-<arch>.tar.gz
#   scripts/build-generic.sh release    dist/patch-<v>-linux-generic-<arch>.tar.gz
#   scripts/build-generic.sh <label>    dist/patch-<v>-<label>-linux-generic-<arch>.tar.gz
#
# Then scripts/smoke-generic.sh <kit> checks it on each distribution.
#
# Committed code only (the tree must be clean), like scripts/build-kit.sh,
# which runs inside the container with KIT_FLAVOR=generic. The Omarchy kit
# is not touched: it stays scripts/build-kit.sh on Arch.
#
# This machine plays music while it builds (docs/development.md, section
# 4): the compile takes the shared build lock, runs at the lowest CPU and
# I/O priority, CARGO_BUILD_JOBS (default 4) jobs and at most 16 GB.
# Caches (toolchain, crates, the target directory) live in
# PATCH_GENERIC_CACHE (default ~/.cache/patch-generic-build), so a second
# build takes minutes, not the ~20 of a cold one.
set -euo pipefail
cd "$(dirname "$0")/.."
label=${1:-test-$(git rev-parse --short HEAD)}
[[ -z $(git status --porcelain) ]] \
  || { echo "commit first: a kit is built from committed code only" >&2; exit 1; }

if command -v podman >/dev/null; then
  engine=podman
  # Files the build writes (dist/, the caches) stay the user's own.
  user=(--userns=keep-id)
elif command -v docker >/dev/null; then
  engine=docker
  user=(--user "$(id -u):$(id -g)")
else
  echo "podman or docker is needed to build the generic kit" >&2
  exit 1
fi

# PATCH_GENERIC_ARCH=aarch64: the 64-bit ARM kit, cross-built here
# (packaging/generic/Containerfile.aarch64).
gen_arch=${PATCH_GENERIC_ARCH:-x86_64}
# PATCH_KIT_FLAVOR=omarchy (with aarch64): the Omarchy kit for ARM (an
# Omarchy MacBook), built here too. The x86_64 Omarchy kit stays the Arch
# build (scripts/build-kit.sh), unchanged.
flavor=${PATCH_KIT_FLAVOR:-generic}
case $flavor:$gen_arch in
  generic:*) os=linux-generic ;;
  omarchy:aarch64) os=linux ;;
  *) echo "PATCH_KIT_FLAVOR=omarchy is for aarch64 only (x86_64: scripts/build-kit.sh on Arch)" >&2; exit 1 ;;
esac
case $gen_arch in
  x86_64) file=packaging/generic/Containerfile target=() ;;
  aarch64) file=packaging/generic/Containerfile.aarch64 target=(-e KIT_TARGET=aarch64-unknown-linux-gnu -e KIT_STRIP=aarch64-linux-gnu-strip) ;;
  *) echo "PATCH_GENERIC_ARCH: x86_64 or aarch64" >&2; exit 1 ;;
esac
image="localhost/patch-generic-build:$gen_arch-$(sha256sum "$file" | cut -c1-12)"
if ! "$engine" image exists "$image" 2>/dev/null && ! "$engine" image inspect "$image" >/dev/null 2>&1; then
  echo "» Preparing the build container ($image)"
  nice -n 19 ionice -c3 "$engine" build -t "$image" -f "$file" packaging/generic
fi

cache=${PATCH_GENERIC_CACHE:-$HOME/.cache/patch-generic-build}
mkdir -p "$cache/cargo/registry" "$cache/cargo/git" "$cache/tools" "$cache/rustup" "$cache/target" "$cache/home"
# The container builds a clone of this commit, always at the same path
# (/src): a worktree's .git points outside it, and the build scripts bake
# the source path into the target directory.
commit=$(git rev-parse HEAD)
[[ -d $cache/src/.git ]] || git clone -q --no-checkout "$PWD" "$cache/src"
git -C "$cache/src" fetch -q "$PWD" "$commit"
git -C "$cache/src" checkout -q --detach --force "$commit"
git -C "$cache/src" clean -qfdx
lock=$HOME/.cache/patch-build.lock
touch "$lock"
echo "» Building the generic kit ($label); waits for the build lock if another build runs"
# shellcheck disable=SC2016 # expanded inside the container
flock "$lock" bash -c 'echo "build-generic.sh $1 $(date -Is)" > "$2.owner"; shift 2; exec "$@"' _ \
  "$label" "$lock" \
  nice -n 19 ionice -c3 "$engine" run --rm "${user[@]}" \
    --memory 16g \
    -v "$cache/src:/src" \
    -v "$cache/cargo/registry:/opt/cargo/registry" -v "$cache/cargo/git:/opt/cargo/git" \
    -v "$cache/tools:/tools" -v "$cache/rustup:/opt/rustup" -v "$cache/target:/target" \
    -v "$cache/home:/home/builder" \
    -e HOME=/home/builder -e CARGO_HOME=/opt/cargo -e RUSTUP_HOME=/opt/rustup \
    -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    "${target[@]}" -e KIT_FLAVOR="$flavor" -e KIT_THEMES="${KIT_THEMES:-1}" -e PATH=/tools/bin:/opt/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    -w /src "$image" bash -euo pipefail -c '
      git config --global --add safe.directory /src
      rustup toolchain install >/dev/null   # the one rust-toolchain.toml names
      [[ -z ${KIT_TARGET:-} ]] || rustup target add "$KIT_TARGET" >/dev/null
      command -v cargo-about >/dev/null || cargo install --locked --features cli --root /tools cargo-about@0.9.3
      exec nice -n 19 scripts/build-kit.sh "$1"' _ "$label"
mkdir -p dist
cp "$cache"/src/dist/*-"$os"-"$gen_arch"* dist/
ls -la dist/*-"$os"-"$gen_arch"*
