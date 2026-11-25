#!/usr/bin/env bash
# Check the generic Linux kit on each distribution it is for, in throwaway
# containers: the packages install.sh asks for exist there, the binary
# starts, measures its DSP kernels and draws a Unit (headless, no sound
# card, no display).
#
#   scripts/smoke-generic.sh dist/patch-<v>-…-linux-generic-x86_64.tar.gz [image…]
#
# Images default to the oldest and newest of each family. Needs podman or
# docker and the network (each distribution's own package mirrors).
set -euo pipefail
kit=$(realpath "${1:?usage: scripts/smoke-generic.sh <generic kit .tar.gz> [image…]}")
shift
images=("$@")
((${#images[@]})) || images=(
  docker.io/library/ubuntu:24.04
  docker.io/library/ubuntu:latest
  docker.io/library/debian:trixie
  registry.fedoraproject.org/fedora:latest
  registry.opensuse.org/opensuse/tumbleweed:latest
  docker.io/library/archlinux:latest
)
engine=$(command -v podman || command -v docker) || { echo "podman or docker is needed" >&2; exit 1; }
# PATCH_SMOKE_PLATFORM=linux/arm64: the ARM kit, in the images' arm64 builds
# under qemu (binfmt; slow, minutes per image).
platform=(--platform "${PATCH_SMOKE_PLATFORM:-linux/amd64}")
# PATCH_SMOKE_FLAVOR=omarchy: the Omarchy ARM kit (its image: Arch Linux ARM).
flavor=${PATCH_SMOKE_FLAVOR:-generic}
# Arch Linux's images are x86_64 only (Arch Linux ARM is another project).
if [[ ${PATCH_SMOKE_PLATFORM:-} == linux/arm64 ]]; then
  images=("${images[@]/docker.io\/library\/archlinux:latest}")
fi

# Runs as root inside each container: the packages from install.sh's own
# table (--print-packages), then the binary.
probe='
set -euo pipefail
cd /tmp && tar -xzf /kit.tar.gz && cd patch-*/
[[ $(cat FLAVOR) == "$FLAVOR" ]] || { echo "not the $FLAVOR kit"; exit 1; }
pkgs=$(./install.sh --print-packages)
fonts=""
if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq && apt-get install -y -qq $pkgs fonts-dejavu-core >/dev/null
elif command -v dnf >/dev/null; then
  dnf install -y -q --skip-unavailable $pkgs dejavu-sans-fonts >/dev/null
elif command -v zypper >/dev/null; then
  zypper -n -q install --no-recommends $pkgs dejavu-fonts >/dev/null
elif command -v pacman >/dev/null; then
  # pacman 7 sandboxes its downloads with Landlock, which qemu-user lacks.
  grep -q "^DisableSandbox" /etc/pacman.conf || sed -i "/^\[options\]/a DisableSandbox" /etc/pacman.conf
  pacman -Syu --noconfirm --needed -q $pkgs ttf-dejavu >/dev/null
fi
for tool in pactl pw-dump ffmpeg ffprobe notify-send python3; do
  command -v $tool >/dev/null || { echo "missing after install: $tool"; exit 1; }
done
export HOME=/tmp/home XDG_RUNTIME_DIR=/tmp/run && mkdir -p $HOME $XDG_RUNTIME_DIR
bin/omarchy-patch --version
bin/omarchy-patch optimize --quick >/dev/null
bin/omarchy-patch shots /tmp/shots mixer >/dev/null
find /tmp/shots -name "mixer-*.png" | grep -q . || { echo "no Mixer shot"; exit 1; }
echo "glibc $(ldd --version | head -1 | grep -o "[0-9.]*$")"
'
fail=0
for image in "${images[@]}"; do
  [[ -n $image ]] || continue
  printf '» %-50s ' "$image"
  if out=$(nice -n 19 "$engine" run --rm "${platform[@]}" -e FLAVOR="$flavor" -v "$kit:/kit.tar.gz:ro" "$image" bash -c "$probe" 2>&1); then
    echo "ok · $(tail -1 <<<"$out")"
  else
    echo "FAILED"
    sed 's/^/    /' <<<"$out" | tail -15
    fail=1
  fi
done
exit $fail
