#!/usr/bin/env bash
# Install Patch in one line (served at https://get.patchaudio.app), on
# Omarchy and on any other Linux with PipeWire (Ubuntu 24.04+, Mint 22,
# Debian 13, Fedora 40+, openSUSE Tumbleweed, Arch, …):
#
#   curl -fsSL https://get.patchaudio.app | bash
#
# Downloads the newest Patch release for this computer (x86_64 or aarch64;
# the Omarchy kit on Omarchy, the generic kit anywhere else) from GitHub (novuon/patch-releases), checks its SHA-256, unpacks it in a
# temporary folder and runs its installer, which asks how you use Patch.
# Arguments go to the installer: ... | bash -s -- --profile dj
# AI agents and scripts (no terminal): a first install needs --profile
# <standard|dj|podcast|live_coding|full> or --modules a,b,…; without one
# the installer lists the choices and stops. Ask the user which they want.
# PATCH_VERSION=v0.8.0 picks a release instead of the newest.
set -euo pipefail

REPO=novuon/patch-releases
ARCH=$(uname -m)
say() { printf '\033[1m» %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m» %s\033[0m\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is needed"
command -v python3 >/dev/null || die "python3 is needed (sudo apt install python3, or your distribution's package)"
# Omarchy gets its own kit; every other distribution the generic one.
os=linux-generic
[[ -d ${XDG_DATA_HOME:-$HOME/.local/share}/omarchy ]] && os=linux
[[ $(id -u) != 0 ]] || die "Run this as your own user, not as root (the installer asks for sudo itself)"

api="https://api.github.com/repos/$REPO/releases"
if [[ -n ${PATCH_VERSION:-} ]]; then api+="/tags/$PATCH_VERSION"; else api+="/latest"; fi
release=$(curl -fsSL -H "Accept: application/vnd.github+json" "$api" 2>/dev/null) \
  || die "Patch isn't released yet: https://patchaudio.app tells you when it is"
pick='
import json, sys
arch, os = sys.argv[1], sys.argv[2]
for a in json.load(sys.stdin).get("assets", []):
    n = a["name"]
    if n.endswith(f"-{os}-{arch}.tar.gz"):
        print(a["browser_download_url"])
        break
'
url=$(python3 -c "$pick" "$ARCH" "$os" <<<"$release")
if [[ -z $url && $os == linux-generic ]] && command -v pacman >/dev/null; then
  # An older release without a generic kit: Arch runs the Omarchy one.
  os=linux
  url=$(python3 -c "$pick" "$ARCH" "$os" <<<"$release")
fi
if [[ -z $url && $os == linux-generic ]]; then
  die "This release has no build for your distribution yet ($ARCH): https://patchaudio.app tells you when it has"
fi
[[ -n $url ]] || die "This release has no build for $ARCH"
kit=${url##*/}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Work from the download's folder: the folder this ran from may not be
# readable (sudo -u, a service, another user's home).
cd "$tmp"
say "Downloading ${kit%.tar.gz}"
curl -fL --progress-bar -o "$tmp/$kit" "$url"
curl -fsSL -o "$tmp/$kit.sha256" "$url.sha256"
(cd "$tmp" && sha256sum -c --quiet "$kit.sha256") || die "The download doesn't match its checksum"
tar -xzf "$tmp/$kit" -C "$tmp"
dirs=("$tmp"/patch-*/)
dir=${dirs[0]%/}
[[ -x $dir/install.sh ]] || die "The download has no installer"

# Piped into bash, the installer's screens need the terminal itself.
cd "$dir"
if [[ -t 1 && -r /dev/tty ]]; then
  ./install.sh "$@" </dev/tty
else
  ./install.sh "$@"
fi
