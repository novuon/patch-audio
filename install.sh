#!/usr/bin/env bash
# Install Patch, the Omarchy audio environment, for this user. On Omarchy,
# and on any other Linux with PipeWire (Ubuntu, Mint, Debian, Fedora,
# openSUSE, Arch, …): the packages come from the distribution's own
# package manager (pacman, apt, dnf or zypper).
#
#   ./install.sh                     the Patch setup screen, then install
#                                    (over an install it starts on your setup:
#                                    Enter keeps it)
#   ./install.sh --profile dj        no screen: standard | dj | podcast | live_coding | full
#                                    (a first install with no terminal needs this or
#                                    --modules: it lists the choices and stops)
#   ./install.sh --modules eq,dj     no screen: Patch core and these modules (eq mic studio
#                                    player dj ai_dj visualiser controllers plugins podcast
#                                    live_coding themes wallpapers; ai_dj is Auto Mix)
#   ./install.sh --key PATCH-…       activate a key (for a module that needs one)
#   ./install.sh --layout basic-dj   the default layout: simple basic-dj ai-dj
#                                    performance-dj dj-studio podcast live-code pro
#   ./install.sh --no-stems          don't download Auto Mix's stem separator
#   ./install.sh --no-remote         don't set up the phone remote (Tailscale)
#   ./install.sh --no-deps           skip installing packages
#   ./install.sh --print-packages    the packages this system needs, then stop
#   ./install.sh --no-bar            leave the bar's audio menu as it is
#   ./install.sh --no-service        don't start Patch with the session
#   ./install.sh --build             compile here, even with a ready-made build
#   PREFIX=/usr/local ./install.sh   another prefix (needs write access)
#
# What it does: asks how you use Patch (Standard: Patch core and the EQ;
# DJ; Podcast; Live coding; Everything; or Custom), which of that
# profile's modules you want, and which layout the Patch launcher opens,
# installs the Arch packages it needs (pacman), takes this release's
# ready-made binary for your machine (x86-64 or ARM64; compiled here only
# when there is none, which takes minutes), installs it with its
# launchers and themes, applies your setup and shows a short guide. At
# login only Patch's audio service starts; no window opens by itself.
# Change any of it later in the controller (Ctrl+K, Setup) or with
# `omarchy-patch setup`. ./uninstall.sh undoes it.
set -euo pipefail

cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
SHARE="$PREFIX/share/omarchy-patch"
# A source checkout has Cargo.toml; a binary kit has only its VERSION.
if [[ -f Cargo.toml ]]; then
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
else
  VERSION=$(cat VERSION)
fi
REPO=novuon/Patch
ARCH=$(uname -m)
DEPS=1
PRINT_PACKAGES=0
BUILD_HERE=0
PROFILE=""
MODULES_ARG=""
LAYOUT=""
STEMS=1
REMOTE=1
KEY=""
OVERRIDES=()
while (($#)); do
  case "$1" in
    --no-deps) DEPS=0 ;;
    --print-packages) PRINT_PACKAGES=1 ;;
    --no-bar) OVERRIDES+=(bar=off) ;;
    --no-service) OVERRIDES+=(start_with_session=off) ;;
    --build) BUILD_HERE=1 ;;
    --profile) PROFILE="${2:-}"; shift ;;
    --profile=*) PROFILE="${1#*=}" ;;
    --modules) MODULES_ARG="${2:-}"; PROFILE=custom; shift ;;
    --modules=*) MODULES_ARG="${1#*=}"; PROFILE=custom ;;
    --layout) LAYOUT="${2:-}"; shift ;;
    --layout=*) LAYOUT="${1#*=}" ;;
    --no-stems) STEMS=0 ;;
    --no-remote) REMOTE=0 ;;
    --key) KEY="${2:-}"; shift ;;
    --key=*) KEY="${1#*=}" ;;
    -h|--help) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '\033[1m» %s\033[0m\n' "$*"; }

# This system: its package manager (the package names below) and whether
# it is Omarchy (the bar's Patch menu, the theme menu, Hyprland's layouts).
# Anywhere else Patch installs the same, with the tray menu for the bar.
# (PATCH_INSTALL_PM, PATCH_LIMITS_DIR and PATCH_INSTALL_PLAIN are for
# integration/test-install-setup.py, which plays other distributions.)
PM=${PATCH_INSTALL_PM:-}
if [[ -z $PM ]]; then
  for p in pacman apt-get dnf zypper; do
    if command -v "$p" >/dev/null; then PM=$p; break; fi
  done
fi
OMARCHY=0
[[ -d ${XDG_DATA_HOME:-$HOME/.local/share}/omarchy ]] && OMARCHY=1
# The kit made for this system: Omarchy's, or the generic one (built on
# Ubuntu 24.04 for every distribution at least that new).
WANT_FLAVOR=generic
((OMARCHY)) && WANT_FLAVOR=omarchy
KIT_FLAVOR=$(cat FLAVOR 2>/dev/null || echo omarchy)

# packages <runtime|optional|build>: what Patch needs, in this system's
# names. Runtime: PipeWire and its tools (pactl, pw-dump), FFmpeg
# (decoding and analysis), SQLite (the library), fontconfig,
# notifications, real-time scheduling (realtime-privileges: the realtime
# group's limits; never rtkit, see below), Python and uv (the stem
# separator). Rubber Band (keylock) only where Patch compiles against it:
# the ready-made builds carry their own. To compile: Rust, pkg-config, a
# C compiler and the libraries' headers.
# The generic kit's windows (Wayland or X11, the desk's EGL) load the
# windowing libraries at run time: listed, as a minimal install lacks some.
packages() {
  case $PM:$1 in
    pacman:runtime)
      echo pipewire libpulse wireplumber ffmpeg rubberband sqlite fontconfig libnotify xdg-user-dirs realtime-privileges uv python
      # The generic binary's windows outside Omarchy (Omarchy has them all).
      ((OMARCHY)) || echo libxkbcommon libxkbcommon-x11 libglvnd mesa ;;
    # JetBrains Mono for an Arch without Omarchy (Omarchy has its Nerd Font build).
    pacman:optional) echo wl-clipboard; ((OMARCHY)) || echo ttf-jetbrains-mono ;;
    pacman:build) echo rust pkgconf gcc ;;
    apt-get:runtime) echo pipewire pipewire-pulse pipewire-bin wireplumber pulseaudio-utils ffmpeg libsqlite3-0 libfontconfig1 libnotify-bin xdg-user-dirs python3 curl libegl1 libegl-mesa0 libwayland-egl1 libxkbcommon0 libxkbcommon-x11-0 libx11-xcb1 libxcursor1 libxi6 libxrandr2 ;;
    apt-get:optional) echo wl-clipboard xclip fonts-jetbrains-mono ;;
    apt-get:build) echo build-essential pkg-config libpipewire-0.3-dev librubberband-dev libsqlite3-dev libfontconfig-dev libxkbcommon-dev libwayland-dev libudev-dev libinput-dev libseat-dev libgbm-dev libegl-dev ;;
    dnf:runtime) echo pipewire pipewire-pulseaudio pipewire-utils wireplumber pulseaudio-utils "$(fedora_ffmpeg)" sqlite-libs fontconfig libnotify xdg-user-dirs python3 uv curl mesa-libEGL libwayland-egl libxkbcommon libxkbcommon-x11 libX11-xcb libXcursor libXi libXrandr ;;
    dnf:optional) echo wl-clipboard xclip gum jetbrains-mono-fonts-all ;;
    dnf:build) echo gcc gcc-c++ pkgconf-pkg-config pipewire-devel rubberband-devel sqlite-devel fontconfig-devel libxkbcommon-devel wayland-devel systemd-devel libinput-devel libseat-devel mesa-libgbm-devel mesa-libEGL-devel ;;
    zypper:runtime) echo pipewire pipewire-pulseaudio pipewire-tools wireplumber pulseaudio-utils ffmpeg libsqlite3-0 fontconfig libnotify-tools xdg-user-dirs python3 uv curl libglvnd Mesa-libEGL1 libwayland-egl1 libxkbcommon0 libxkbcommon-x11-0 libX11-xcb1 libXcursor1 libXi6 libXrandr2 ;;
    zypper:optional) echo wl-clipboard xclip gum jetbrains-mono-fonts ;;
    zypper:build) echo gcc gcc-c++ pkgconf pipewire-devel rubberband-devel sqlite3-devel fontconfig-devel libxkbcommon-devel wayland-devel libudev-devel libinput-devel libseat-devel libgbm-devel Mesa-libEGL-devel ;;
  esac
}
# Fedora: FFmpeg from RPM Fusion when it is set up (every codec), else
# Fedora's own ffmpeg-free (MP3, FLAC, Opus, Vorbis, WAV; no AAC/M4A).
fedora_ffmpeg() {
  if rpm -q ffmpeg >/dev/null 2>&1 || dnf repolist --enabled 2>/dev/null | grep -qi rpmfusion; then
    echo ffmpeg
  else
    echo ffmpeg-free
  fi
}
if ((PRINT_PACKAGES)); then
  [[ -n $PM ]] || { echo "No known package manager (pacman, apt-get, dnf, zypper)" >&2; exit 1; }
  packages runtime
  exit 0
fi

# Patch places its windows with Hyprland's Lua API (0.56+). On an older
# Hyprland the layouts don't work: windows land anywhere and the pointer
# jumps to each new one. Audio itself works; the windows don't behave.
HYPR_MIN=0.56
# No Hyprland to ask (an SSH or TTY install): no check, and no exit
# (set -e and pipefail would end the installer here without a word).
hypr=$(hyprctl version -j 2>/dev/null | sed -n 's/.*"version": *"\([0-9.]*\)".*/\1/p' | head -1) || hypr=""
if [[ -n $hypr && $(printf '%s\n' "$HYPR_MIN" "$hypr" | sort -V | head -1) != "$HYPR_MIN" ]]; then
  printf '\033[1;33m» Hyprland %s found: Patch needs %s or newer.\033[0m\n' "$hypr" "$HYPR_MIN" >&2
  echo "  Its windows won't arrange (they land anywhere, the pointer jumps to each)." >&2
  echo "  Update Omarchy first (omarchy-update), then run ./install.sh again." >&2
  if [[ -t 0 && -t 1 ]] && command -v gum >/dev/null; then
    gum confirm "Install anyway?" --default=false || exit 1
  fi
fi

# ── The Patch setup screen ─────────────────────────────────────────────
# Patch core is always in: Patch replaces Omarchy's audio (system audio,
# the service from login, the bar's audio menu, the Mixer and the Meter).
# The modules, the profiles and what each profile ticks come from the same
# catalogue Patch itself reads (crates/patch-core/modules.json), so the
# installer, Settings and `omarchy-patch modules` never disagree.
CORE=(system_audio start_with_session bar)
CAT=crates/patch-core/modules.json
[[ -r $CAT ]] || { echo "This kit has no module catalogue ($CAT)" >&2; exit 1; }
# catalogue <query> [args]: answers from the catalogue, one item a line.
catalogue() {
  python3 - "$CAT" "$@" <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))
q, args = sys.argv[2], sys.argv[3:]
mods = {m["id"]: m for m in c["modules"]}
def needs(ids):
    on, todo = [], list(ids)
    while todo:
        i = todo.pop()
        if i in mods and i not in on:
            on.append(i)
            todo += mods[i].get("needs", [])
    return [m["id"] for m in c["modules"] if m["id"] in on]
keyed = {m for l in c.get("licences", []) for m in l["modules"]}
soon = {m for l in c.get("licences", []) if l.get("coming_soon") for m in l["modules"]}
if q == "switches":        # modules with a setup switch
    print(" ".join(m["id"] for m in c["modules"] if m.get("switch")))
elif q == "all":           # what a user may choose (not the hidden ones)
    print(" ".join(m["id"] for m in c["modules"] if not m.get("hidden")))
elif q == "profiles":      # offered on the screen: id|name|about
    for p in c["profiles"]:
        if not p.get("hidden"):
            print(f'{p["id"]}|{p["name"]}|{p["about"]}')
elif q == "profile-ids":
    print(" ".join(p["id"] for p in c["profiles"]))
elif q == "default":
    print(next(p["id"] for p in c["profiles"] if p.get("default")))
elif q == "layout":
    print(next(p["layout"] for p in c["profiles"] if p["id"] == args[0]))
elif q == "ticks":         # a profile's modules (with what they need)
    print(" ".join(needs(next(p["modules"] for p in c["profiles"] if p["id"] == args[0]))))
elif q == "offer":         # a profile's modules, then what it offers
    p = next(p for p in c["profiles"] if p["id"] == args[0])
    print(" ".join(i for i in dict.fromkeys(p["modules"] + p["offer"]) if not mods[i].get("hidden")))
elif q == "needs":
    print(" ".join(needs(args)))
elif q == "item":          # the line a module gets on the screen (no comma: gum splits on them)
    m = mods[args[0]]
    extra = ""
    if m["id"] in soon:
        extra = " (coming soon)"
    elif m["id"] in keyed:
        extra = " (key needed)"
    elif m.get("step_size"):
        extra = f' (downloads ~{m["step_size"] / 1e9:.0f} GB)'
    elif m.get("preview"):
        extra = " (preview)"
    print(f'{m["name"]:<21} {m["about"]}{extra}'.replace(",", " ·"))
elif q == "keyed":         # the chosen modules that need a key
    print(" ".join(m for m in args if m in keyed))
PY
}
SWITCH_IDS=($(catalogue switches))
ALL_IDS=($(catalogue all))
# Layouts: id, name, what it opens, the modules it needs (as
# DefaultLayout::needs in crates/patch-core/src/setup.rs).
LAYOUT_IDS=(simple basic-dj ai-dj performance-dj dj-studio podcast live-code pro)
declare -A LAYOUT_NAME=(
  [simple]="Simple" [basic-dj]="Basic DJ" [ai-dj]="AI DJ" [performance-dj]="Performance DJ" [dj-studio]="DJ Studio"
  [podcast]="Podcast" [live-code]="Live Code" [pro]="Pro"
)
declare -A LAYOUT_ABOUT=(
  [simple]="Mixer and EQ · 1 workspace"
  [basic-dj]="Decks, crossfader, library · 1 workspace"
  [ai-dj]="DJ Booth with the AI DJ, library, meter · 1 workspace"
  [performance-dj]="Booth · control · Visualiser · 3 workspaces"
  [dj-studio]="Booth · mixing desk (Router, EQ, Mixer) · library · 3 workspaces"
  [podcast]="Mic, Mixer, meter, Player, Recorder, Broadcast · 1 workspace"
  [live-code]="Tidal, meter, Mixer · Visualiser · 2 workspaces"
  [pro]="EQ, meter, Mixer · Router, Analyzer, Recorder, plugins · 2 workspaces"
)
declare -A LAYOUT_NEEDS=(
  [simple]="" [basic-dj]="dj" [ai-dj]="dj ai_dj" [performance-dj]="dj ai_dj visualiser eq"
  [dj-studio]="dj ai_dj visualiser eq studio" [podcast]="podcast" [live-code]="live_coding visualiser"
  [pro]="eq studio"
)
CHOSEN=""   # the modules on (switches and downloads)
has() { [[ " $CHOSEN " == *" $1 "* ]]; }
offered() {
  local m
  for m in ${LAYOUT_NEEDS[$1]}; do has "$m" || return 1; done
}
# The layout that suits the modules best, when none is chosen: on an update
# the one set up now, while the modules still offer it; else the profile's.
best_layout() {
  local l
  for l in "$SAVED_LAYOUT" "${PROFILE_LAYOUT:-}"; do
    if [[ -n $l && -n ${LAYOUT_NAME[$l]:-} ]] && offered "$l"; then echo "$l"; return; fi
  done
  for l in performance-dj ai-dj basic-dj podcast live-code simple; do
    offered "$l" && { echo "$l"; return; }
  done
}

# Installed over an earlier Patch (or after an uninstall that kept the
# settings): what is set up now. An update must keep it unless the user
# changes it, whether they press Enter through the screens or run from a
# script. A switch the file doesn't name (newer than that install) is on:
# Patch reads it so, an older setup keeps the Units it showed.
SAVED_SETUP="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy-patch/setup.toml"
STEMS_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy-patch/stems.json"
SAVED_PROFILE=""
SAVED_MODULES=""
SAVED_LAYOUT=""
if [[ -r $SAVED_SETUP ]]; then
  for id in "${SWITCH_IDS[@]}"; do
    grep -qx "$id = false" "$SAVED_SETUP" || SAVED_MODULES+="$id "
  done
  # The downloads: there or not.
  [[ -e $STEMS_FILE ]] && SAVED_MODULES+="stems "
  [[ -n $(ls -A "$SHARE/themes/patch-dark/backgrounds/intros" 2>/dev/null) ]] && SAVED_MODULES+="wallpapers "
  SAVED_MODULES=$(catalogue needs $SAVED_MODULES)" "
  SAVED_LAYOUT=$(sed -n 's/^layout = "\(.*\)"$/\1/p' "$SAVED_SETUP")
  SAVED_PROFILE=custom
  for p in $(catalogue profile-ids); do
    ticks=$(catalogue ticks "$p")
    # A profile is the same set of switches (downloads don't decide it).
    same=1
    for id in "${SWITCH_IDS[@]}"; do
      [[ " $ticks " == *" $id "* ]] && on=1 || on=0
      [[ " $SAVED_MODULES " == *" $id "* ]] && was=1 || was=0
      ((on == was)) || { same=0; break; }
    done
    if ((same)); then SAVED_PROFILE=$p; break; fi
  done
  # Minimal is no longer on the screen: the same modules, as Custom.
  [[ $SAVED_PROFILE == minimal ]] && SAVED_PROFILE=custom
fi

TAGLINE="The Omarchy audio environment"
((OMARCHY)) || TAGLINE="The audio environment for Linux"

# The screens use gum where it is (Omarchy has it) and plain numbered
# menus on any other terminal, with the same choices and defaults.
GUM=0
command -v gum >/dev/null && [[ -z ${PATCH_INSTALL_PLAIN:-} ]] && GUM=1
# pick_one <header> <selected> <item>…: the item chosen, on stdout.
pick_one() {
  local header=$1 selected=$2 i n=0 d=1 a
  shift 2
  printf '\n%s\n' "$header" >&2
  for i in "$@"; do
    n=$((n + 1))
    [[ $i == "$selected" ]] && d=$n
    printf '  %2d  %s\n' "$n" "$i" >&2
  done
  while :; do
    read -rp "  Your choice (1-$n, Enter: $d): " a  || return 1
    a=${a:-$d}
    if [[ $a =~ ^[0-9]+$ ]] && ((a >= 1 && a <= n)); then
      printf '%s\n' "${!a}"
      return 0
    fi
  done
}
# pick_many <header> <selected lines> <item>…: the items chosen, a line each.
pick_many() {
  local header=$1 selected=$2 i n a k
  shift 2
  local on=()
  for i in "$@"; do
    if grep -qxF -- "$i" <<<"$selected"; then on+=(1); else on+=(0); fi
  done
  while :; do
    printf '\n%s\n' "$header" >&2
    n=0
    for i in "$@"; do
      printf '  %2d  [%s] %s\n' "$((n + 1))" "$( ((on[n])) && echo x || echo ' ')" "$i" >&2
      n=$((n + 1))
    done
    read -rp "  Numbers to switch (e.g. 2 5), Enter when done: " a  || return 1
    [[ -z $a ]] && break
    for k in ${a//,/ }; do
      if [[ $k =~ ^[0-9]+$ ]] && ((k >= 1 && k <= n)); then on[k - 1]=$((1 - on[k - 1])); fi
    done
  done
  n=0
  for i in "$@"; do
    ((on[n])) && printf '%s\n' "$i"
    n=$((n + 1))
  done
  return 0
}

logo() {
  cat <<'LOGO'
█▀█ ▄▀█ ▀█▀ █▀▀ █ █
█▀▀ █▀█  █  █▄▄ █▀█
LOGO
}

# The modules screen: `offer` listed, `ticked` preselected.
modules_screen() {
  local header=$1 offer=$2 ticked=$3 items=() on=() id item out
  for id in $offer; do
    item=$(catalogue item "$id")
    items+=("$item")
    [[ " $ticked " == *" $id "* ]] && on+=(--selected "$item")
  done
  if ((GUM)); then
    out=$(gum choose --no-limit --height 16 "${on[@]}" --header "$header" \
      --cursor "  › " "${items[@]}") || exit 1
  else
    local sel="" j
    for ((j = 1; j < ${#on[@]}; j += 2)); do sel+="${on[j]}"$'\n'; done
    out=$(pick_many "$header" "$sel" "${items[@]}") || exit 1
  fi
  CHOSEN=""
  for id in $offer; do
    item=$(catalogue item "$id")
    if grep -qxF "$item" <<<"$out"; then CHOSEN+="$id "; fi
  done
}

SCREEN_DONE=0
setup_screen() {
  SCREEN_DONE=1
  clear
  if ((!GUM)); then
    logo
    printf '\n%s\nv%s · routing, mixing, metering, recording, DJ\n' "$TAGLINE" "$VERSION"
  else
  gum style --border rounded --border-foreground 1 --padding "1 4" --margin "1 2" \
    "$(gum style --foreground 1 --bold "$(logo)")" "" \
    "$(gum style --bold "$TAGLINE")" \
    "$(gum style --faint "v$VERSION · routing, mixing, metering, recording, DJ")"
  fi
  local header="  How do you use Patch? Patch core (your sound, volume and outputs, the Mixer) is always in" lines=() start=() line p name about pick
  local default=${SAVED_PROFILE:-$(catalogue default)}
  while IFS='|' read -r p name about; do
    line=$(printf '%-13s %s' "$name" "$about")
    line=${line//,/ ·}
    lines+=("$line")
    [[ $p == "$default" ]] && start=(--selected "$line")
  done < <(catalogue profiles)
  local custom="Custom        Patch core and the modules you choose"
  lines+=("$custom")
  [[ $default == custom ]] && start=(--selected "$custom")
  [[ -n $SAVED_PROFILE ]] && header="  How do you use Patch? (your setup now is selected: Enter keeps it)"
  if ((GUM)); then
    pick=$(gum choose --header "$header" --cursor "  › " --height 9 "${start[@]}" "${lines[@]}") || exit 1
  else
    pick=$(pick_one "$header" "${start[1]:-}" "${lines[@]}") || exit 1
  fi
  PROFILE=custom
  while IFS='|' read -r p name about; do
    [[ $pick == "$name "* ]] && PROFILE=$p
  done < <(catalogue profiles)
  # What comes with it: ticked as the profile has it (on an update, as it
  # is set up now while the profile is unchanged); the rest it offers.
  local ticked offer
  if [[ $PROFILE == custom ]]; then
    offer="${ALL_IDS[*]}"
    ticked=${SAVED_MODULES:-$(catalogue ticks "$(catalogue default)")}
    modules_screen "  Patch core is always in. Choose the modules (x toggles, enter confirms)" "$offer" "$ticked"
  else
    offer=$(catalogue offer "$PROFILE")
    ticked=$(catalogue ticks "$PROFILE")
    [[ $PROFILE == "$SAVED_PROFILE" ]] && ticked=$SAVED_MODULES
    modules_screen "  ${pick%% *} comes with these ticked; add or remove any (x toggles, enter confirms)" "$offer" "$ticked"
    # A profile's modules that the screen doesn't list stay as they are.
  fi
}

layout_screen() {
  local items=() l out first
  first=$(best_layout)
  items+=("$(printf '%-16s %s' "${LAYOUT_NAME[$first]}" "${LAYOUT_ABOUT[$first]}")")
  for l in "${LAYOUT_IDS[@]}"; do
    [[ $l == "$first" ]] && continue
    offered "$l" && items+=("$(printf '%-16s %s' "${LAYOUT_NAME[$l]}" "${LAYOUT_ABOUT[$l]}")")
  done
  local header="  Your default layout: what the Patch launcher opens (nothing opens at login)"
  if ((GUM)); then
    out=$(gum choose --height 10 --cursor "  › " --header "$header" "${items[@]}") || exit 1
  else
    out=$(pick_one "$header" "${items[0]}" "${items[@]}") || exit 1
  fi
  for l in "${LAYOUT_IDS[@]}"; do
    if [[ $out == "${LAYOUT_NAME[$l]} "* ]]; then LAYOUT=$l; fi
  done
}

# Without a terminal: the profile or modules given, else what is set up
# now. A first install with neither stops before it changes anything and
# lists the choices: an AI agent or a script runs it with no screen to
# show, and 2026-11-25 an agent's install quietly became Standard without
# asking the user. Every choice replaces the desktop's own audio.
INTERACTIVE=0
[[ -t 0 && -t 1 ]] && INTERACTIVE=1
ask_for_setup() {
  local p name about
  {
    printf '\n» Patch needs a setup choice. There is no terminal here to show the\n'
    printf '  setup screen (an AI agent, a script, or ssh without -t).\n\n'
    printf '  AI agent: ask the user which setup they want, then run the install\n'
    printf '  again with it. Nothing has been installed or changed yet.\n\n'
    printf '  How do you use Patch? (Patch core: your sound, volume and outputs, the Mixer)\n'
    while IFS='|' read -r p name about; do
      printf '    --profile %-12s %s\n' "$p" "${about//,/ ·}"
    done < <(catalogue profiles)
    printf '    --modules a,b,…        Patch core and only these modules:\n'
    printf '                           %s\n' "${ALL_IDS[*]}"
    printf '\n  Every profile includes the Patch themes (four wallpapers each); the\n'
    printf '  module wallpapers adds the other wallpapers and the intros.\n'
    printf '  Optional: --layout <%s>\n' "${LAYOUT_IDS[*]}"
    printf '\n  For example:\n'
    printf '    curl -fsSL https://get.patchaudio.app | bash -s -- --profile dj\n'
    printf '    curl -fsSL https://get.patchaudio.app | bash -s -- --modules eq,dj,ai_dj,themes,wallpapers\n'
    printf '    ./install.sh --profile %s    (from an unpacked kit)\n\n' "$(catalogue default)"
  } >&2
}
if [[ -z $PROFILE ]]; then
  if ((INTERACTIVE)); then
    setup_screen
  elif [[ -n $SAVED_PROFILE ]]; then
    PROFILE=custom
    CHOSEN=$SAVED_MODULES
  else
    ask_for_setup
    exit 2
  fi
elif [[ $PROFILE == full* ]]; then
  PROFILE=full
fi
if [[ $PROFILE == custom && -n $MODULES_ARG ]]; then
  CHOSEN=""
  for id in ${MODULES_ARG//,/ }; do
    [[ " ${ALL_IDS[*]} " == *" $id "* ]] || { echo "modules: ${ALL_IDS[*]}" >&2; exit 2; }
    CHOSEN+="$id "
  done
elif [[ $PROFILE != custom ]]; then
  [[ " $(catalogue profile-ids) " == *" $PROFILE "* ]] || { echo "profiles: $(catalogue profile-ids) (or --modules …)" >&2; exit 2; }
  PROFILE_LAYOUT=$(catalogue layout "$PROFILE")
  # Chosen on the screen already; from a script, the profile's modules.
  ((SCREEN_DONE)) || CHOSEN=$(catalogue ticks "$PROFILE")
fi
# What the chosen modules need comes with them (the AI DJ plays on the
# decks, the wallpapers belong to the themes).
CHOSEN=$(catalogue needs $CHOSEN)
((STEMS)) || CHOSEN=${CHOSEN//stems/}
if [[ -z $LAYOUT ]]; then
  if ((INTERACTIVE)); then layout_screen; else LAYOUT=$(best_layout); fi
fi
[[ -n ${LAYOUT_NAME[$LAYOUT]:-} ]] || { echo "layouts: ${LAYOUT_IDS[*]}" >&2; exit 2; }
offered "$LAYOUT" || { echo "the $LAYOUT layout needs: ${LAYOUT_NEEDS[$LAYOUT]}" >&2; exit 2; }

# A module that needs a key (modules.json `licences`): the key now, or later in Settings.
LICENCES="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy-patch/licences.json"
KEYED=$(catalogue keyed $CHOSEN)
if [[ -n $KEYED && -z $KEY ]] && ((INTERACTIVE)) && ! grep -q '"activation_id"' "$LICENCES" 2>/dev/null; then
  # shellcheck source=install-key.sh
  source ./install-key.sh
  key_prompt
fi

SETUP=()
# The core is on, except what the user switched off since installing: an
# update keeps that. Only over a binary that is here: uninstall.sh switches
# system audio off and keeps the file, and a reinstall has to turn it back on.
for id in "${CORE[@]}"; do
  if [[ -x $BIN/omarchy-patch && -r $SAVED_SETUP ]] && grep -qx "$id = false" "$SAVED_SETUP"; then
    SETUP+=("$id=off")
  else
    SETUP+=("$id=on")
  fi
done
for id in "${SWITCH_IDS[@]}"; do
  if has "$id"; then SETUP+=("$id=on"); else SETUP+=("$id=off"); fi
done
SETUP+=("${OVERRIDES[@]}" "layout=$LAYOUT")
CHOSEN_LIST=$(echo $CHOSEN)
say "Setup: ${PROFILE} · modules: ${CHOSEN_LIST:-none (Patch core)} · default layout: $LAYOUT"

# 1 · The binary. A release kit (the download for your machine) carries
# it in bin/; a checkout of a release tag fetches the kit's binary; any
# other checkout (work in progress) is compiled here.
PREBUILT=""
REMOTE_PREBUILT=""
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
release_checkout() {
  if ! git rev-parse --git-dir >/dev/null 2>&1; then return 0; fi
  [[ $(git describe --exact-match --tags HEAD 2>/dev/null) == "v$VERSION" ]] && git diff --quiet HEAD 2>/dev/null
}
# fetch_prebuilt [repo]: this version's ready-made binaries for this system
# (the Omarchy or the generic kit), from the release.
fetch_prebuilt() {
  local repo=${1:-$REPO} os=linux
  [[ $WANT_FLAVOR == generic ]] && os=linux-generic
  local kit="patch-$VERSION-$os-$ARCH.tar.gz"
  local url="https://github.com/$repo/releases/download/v$VERSION"
  say "Fetching the ready-made Patch $VERSION for $ARCH ($WANT_FLAVOR)"
  if ! { curl -fsSL -o "$TMP/$kit" "$url/$kit" && curl -fsSL -o "$TMP/$kit.sha256" "$url/$kit.sha256"; } 2>/dev/null; then
    # A private repository: through the GitHub CLI when it's signed in.
    command -v gh >/dev/null && gh auth status >/dev/null 2>&1 \
      && gh release download "v$VERSION" -R "$repo" -D "$TMP" -p "$kit" -p "$kit.sha256" --clobber >/dev/null 2>&1 \
      || return 1
  fi
  (cd "$TMP" && sha256sum -c --quiet "$kit.sha256") || { echo "The download doesn't match its checksum" >&2; return 1; }
  tar -xzf "$TMP/$kit" -C "$TMP" "patch-$VERSION/bin/omarchy-patch" && PREBUILT="$TMP/patch-$VERSION/bin/omarchy-patch"
  tar -xzf "$TMP/$kit" -C "$TMP" "patch-$VERSION/bin/patch-remote" 2>/dev/null \
    && REMOTE_PREBUILT="$TMP/patch-$VERSION/bin/patch-remote"
  return 0
}
if [[ $BUILD_HERE == 0 ]]; then
  if [[ -x bin/omarchy-patch && $KIT_FLAVOR == "$WANT_FLAVOR" ]]; then
    PREBUILT=bin/omarchy-patch
  elif [[ -x bin/omarchy-patch ]]; then
    # The other system's kit (the Omarchy kit on Ubuntu, say): this
    # version's kit for this system, from the public releases.
    fetch_prebuilt novuon/patch-releases || {
      echo "No $WANT_FLAVOR build of Patch $VERSION to fetch: trying this kit's own" >&2
      PREBUILT=bin/omarchy-patch
    }
  elif release_checkout; then
    fetch_prebuilt || echo "No ready-made build for $ARCH: compiling Patch here instead (a few minutes)" >&2
  fi
fi

# 2 · Packages. Runtime: PipeWire and its tools, FFmpeg (decoding and
# analysis), Rubber Band (keylock), SQLite (the library), fontconfig,
# notifications, real-time scheduling for the audio thread (the realtime
# group). To compile: Rust, pkg-config, a C compiler.
RUNTIME=($(packages runtime))
BUILD=($(packages build))
OPTIONAL=($(packages optional))
# installed <package>: whether this system has it (by name, or for rpm
# systems by what a package provides: openSUSE's ffmpeg is ffmpeg-7).
installed() {
  case $PM in
    pacman) pacman -Qq "$1" >/dev/null 2>&1 ;;
    apt-get) [[ $(dpkg-query -W -f '${Status}' "$1" 2>/dev/null) == "install ok installed" ]] ;;
    dnf|zypper) rpm -q --whatprovides "$1" >/dev/null 2>&1 ;;
    *) return 1 ;;
  esac
}
# Each of these at once, else one by one: a name this release of the
# distribution lacks (uv on older ones) must not stop the rest.
pm_install() {
  local cmd
  case $PM in
    apt-get) cmd=(sudo apt-get install -y) ;;
    dnf) cmd=(sudo dnf install -y) ;;
    zypper) cmd=(sudo zypper --non-interactive install --no-recommends) ;;
  esac
  "${cmd[@]}" "$@" && return 0
  local p failed=()
  for p in "$@"; do "${cmd[@]}" "$p" >/dev/null 2>&1 || failed+=("$p"); done
  ((${#failed[@]} == 0)) || echo "Not installed (not in this distribution's repositories?): ${failed[*]}" >&2
}
install_packages() {
  [[ $DEPS == 1 ]] || return 0
  if [[ $PM == apt-get || $PM == dnf || $PM == zypper ]]; then
    missing=()
    for p in "$@"; do
      installed "$p" || missing+=("$p")
    done
    if command -v cargo >/dev/null; then
      missing=("${missing[@]/rust}")
    fi
    missing=($(printf '%s\n' "${missing[@]}" | sed '/^$/d'))
    if ((${#missing[@]})); then
      say "Installing packages: ${missing[*]}"
      [[ $PM == apt-get ]] && { sudo apt-get update -qq || true; }
      pm_install "${missing[@]}"
    fi
  elif [[ $PM == pacman ]]; then
    missing=()
    for p in "$@"; do
      pacman -Qq "$p" >/dev/null 2>&1 || missing+=("$p")
    done
    # rustup provides cargo too: don't ask for the rust package then.
    if command -v cargo >/dev/null; then
      missing=("${missing[@]/rust}")
    fi
    missing=($(printf '%s\n' "${missing[@]}" | sed '/^$/d'))
    if ((${#missing[@]})); then
      say "Installing packages: ${missing[*]}"
      sudo pacman -S --needed --noconfirm "${missing[@]}"
    fi
  else
    echo "No pacman, apt, dnf or zypper: install these yourself if missing: $*" >&2
  fi
}
# Patch is a PipeWire application: on a system still running PulseAudio
# itself, switching the sound server is the user's step, not ours.
if pgrep -u "$(id -u)" -x pulseaudio >/dev/null; then
  echo "This system's sound server is PulseAudio; Patch needs PipeWire (the default on every current distribution)." >&2
  echo "Switch to PipeWire first (your distribution's guide: pipewire-pulse), log in again, then run ./install.sh again." >&2
  exit 1
fi
install_packages "${RUNTIME[@]}" "${OPTIONAL[@]}"
if [[ $PM == dnf && $(fedora_ffmpeg) == ffmpeg-free ]]; then
  echo "Note: Fedora's ffmpeg-free plays MP3, FLAC, Opus, Vorbis and WAV, not AAC/M4A. For every format: enable RPM Fusion, then sudo dnf swap ffmpeg-free ffmpeg --allowerasing" >&2
fi
# The ready-made binary has to run against this system's libraries.
if [[ -n $PREBUILT ]] && ! "$PREBUILT" --version >/dev/null 2>&1; then
  echo "The ready-made build doesn't run on this system: compiling Patch here instead" >&2
  PREBUILT=""
fi
[[ -z $PREBUILT && -f Cargo.toml ]] && install_packages "${BUILD[@]}"
# Real time: without it the engine's audio thread can be pre-empted and
# the sound drops out under load. The realtime group's limits apply from
# the next login; until then the engine runs without real-time, as the
# rest of PipeWire does.
# Never rtkit: installed, it makes every PipeWire app's audio thread
# real-time with rtkit's 200 ms CPU limit (soft = hard), and the kernel
# SIGKILLs an app whose audio thread runs 200 ms without blocking (cliamp
# died a second after it started, 2026-11-25). With the realtime group
# PipeWire uses the group's limits instead: no CPU limit, nothing killed.
# integration/test-rt-clients.py checks it.
RELOGIN=0
# Other distributions have no realtime-privileges: the same group and the
# same limits it sets (real-time priority, and locked memory for the
# decks' audio), from the next login.
LIMITS_DIR=${PATCH_LIMITS_DIR:-/etc/security}
LIMITS=$LIMITS_DIR/limits.d/95-patch-realtime.conf
if ((DEPS)) && [[ $PM != pacman ]] && [[ ! -f $LIMITS ]] \
   && ! grep -qs '^@realtime' "$LIMITS_DIR/limits.conf" "$LIMITS_DIR"/limits.d/*.conf; then
  say "Setting up real-time audio (the realtime group)"
  if { getent group realtime >/dev/null || sudo groupadd -r realtime; } \
     && printf '%s\n' "# Patch: real-time audio for the realtime group (as Arch's realtime-privileges)" \
          "@realtime - rtprio 98" "@realtime - memlock unlimited" | sudo tee "$LIMITS" >/dev/null; then
    :
  else
    echo "Could not set up the realtime group: the audio thread runs without real-time priority" >&2
  fi
fi
if getent group realtime >/dev/null && ! id -nG "$USER" | tr ' ' '\n' | grep -qx realtime; then
  say "Adding $USER to the realtime group (real-time audio)"
  sudo usermod -aG realtime "$USER" || echo "Could not add you to 'realtime': sudo usermod -aG realtime $USER" >&2
fi
# In the group, but this session started before: real-time (and the decks'
# locked memory) only from the next login. Checked on every run, so running
# the installer again before logging out still says so.
if id -nG "$USER" 2>/dev/null | tr ' ' '\n' | grep -qx realtime \
   && ! id -nG | tr ' ' '\n' | grep -qx realtime; then
  RELOGIN=1
fi

# Raw HID: a controller speaking a vendor protocol has no evdev node in the
# seat, so logind's uaccess tag never reaches its /dev/hidraw node and the
# controller service cannot open it. The rule grants the active seat access.
RULES=/etc/udev/rules.d/70-omarchy-patch-controllers.rules
if has controllers && [[ ! -f $RULES ]]; then
  say "Granting raw-HID access for controllers (udev rule)"
  if sudo install -Dm644 packaging/70-omarchy-patch-controllers.rules "$RULES" \
     && sudo udevadm control --reload \
     && sudo udevadm trigger --subsystem-match=hidraw; then
    :
  else
    echo "Could not install $RULES: controllers that use a raw HID protocol will list but not open." >&2
  fi
fi

# 3 · Compile, when there's no ready-made build.
if [[ -z $PREBUILT && ! -f Cargo.toml ]]; then
  echo "This kit holds only the compiled Patch, and it can't run here (or --build was given)." >&2
  if [[ $WANT_FLAVOR == generic ]]; then
    echo "Patch needs Ubuntu 24.04, Debian 13, Fedora 40 or newer (glibc 2.39 and PipeWire 1.0 at least)." >&2
  fi
  echo "Install from the source instead: https://github.com/$REPO" >&2
  exit 1
fi
if [[ -z $PREBUILT ]]; then
  command -v cargo >/dev/null || { echo "cargo not found: install rust (or rustup) first" >&2; exit 1; }
  say "Compiling Patch (release; a few minutes the first time)…"
  features=()
  [[ $WANT_FLAVOR == generic ]] && features=(--features generic)
  cargo build --release --locked -p patch -p patch-remote "${features[@]}"
  PREBUILT=target/release/omarchy-patch
fi

# 4 · Install: the binary, the desktop entries (their Exec uses the name,
# found on PATH).
say "Installing into $PREFIX"
install -Dm755 "$PREBUILT" "$BIN/omarchy-patch"
# Outside Omarchy the command has its plain name too.
((OMARCHY)) || ln -sfn omarchy-patch "$BIN/patch-audio"
# Installed over a running Patch: the engine hands over to this build now,
# without a gap (otherwise the old one plays on until the next login).
# Then the librarian stops: the new engine starts the new one on its next
# pass (within 30 s), in the session's environment; it analyses in the
# background and picks up where it stopped. After a failed handover (from
# SSH, without the session) both stay as they are. The patterns match only
# those processes' own command lines.
if pgrep -u "$(id -u)" -f -x '.*omarchy-patch engine' >/dev/null; then
  say "Handing the running engine over to this version"
  if "$BIN/omarchy-patch" engine upgrade >/dev/null; then
    pkill -u "$(id -u)" -f -x '.*omarchy-patch librarian' || true
  else
    echo "The running engine didn't hand over: pause the deck and run omarchy-patch engine upgrade, or the new one takes over at your next login" >&2
  fi
fi
# And the windows: closed now (the audio plays on), reopened at the end as
# they were, the layout or the windows that were open. Only in the desktop
# session itself: from SSH or a TTY they can't reopen, so they stay.
REOPEN=""
if [[ -n ${WAYLAND_DISPLAY:-} ]]; then
  windows=$("$BIN/omarchy-patch" status 2>/dev/null) || windows=""
  if [[ $windows == ok* ]]; then
    # `ok open=mixer,meters layout=simple edited=0`; no layout is `-`.
    layout=$(sed -n 's/.* layout=\([^ ]*\).*/\1/p' <<<"$windows")
    open=$(sed -n 's/.*open=\([^ ]*\).*/\1/p' <<<"$windows")
    if [[ -n $layout && $layout != - ]]; then
      REOPEN="layout $layout"
    elif [[ -n $open ]]; then
      REOPEN="open ${open//,/ }"
    fi
    say "Closing Patch's windows: they reopen with this version at the end"
    "$BIN/omarchy-patch" quit >/dev/null 2>&1 || true
    for _ in $(seq 40); do
      [[ $("$BIN/omarchy-patch" status 2>/dev/null) == closed ]] && break
      sleep 0.25
    done
  fi
fi
mkdir -p "$APPS"
install -m644 integration/desktop/*.desktop "$APPS/"
# The app icon: the Module on an Ink tile (docs/patch-style.md).
install -Dm644 integration/desktop/omarchy-patch.svg "$PREFIX/share/icons/hicolor/scalable/apps/omarchy-patch.svg"
# The tray's alert icons (outside Omarchy: a take, a broadcast, a skip).
install -m644 integration/desktop/omarchy-patch-red.svg integration/desktop/omarchy-patch-amber.svg \
  "$PREFIX/share/icons/hicolor/scalable/apps/"
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" >/dev/null 2>&1 || true
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "Note: $BIN is not on your PATH; add it to use omarchy-patch from a terminal." >&2 ;;
esac

# The themes ship with Patch (setup links them into Omarchy's theme menu).
mkdir -p "$SHARE/stems"
install -m644 tools/stems/setup.py tools/stems/demucs_adapter.py "$SHARE/stems/"
# Copied over what is there, never deleted first: wallpapers added to the
# installed themes (backgrounds/12-… and their intros) are the user's and
# are not in the repository; `rm -rf` here removed them on every install.
mkdir -p "$SHARE/themes"
# The wallpapers Patch ships are compressed 4K WebPs (themes/media keeps the
# 8K masters). A wallpaper Patch ships owns its name: the same picture in
# another format (an older install's PNG, a lossless WebP) goes, so
# Omarchy's picker lists it once. 2026-11-13: 12–20 showed twice, a 4K and
# a 1672×941 copy. Wallpapers of other names are the user's and stay.
# Without the Wallpapers module a theme keeps four: Stylus (01) and the
# three newest (18–20); keep in step with KEEP in scripts/build-modules.py.
install_theme() {
  local from=$1 to=$2 f name stem old
  mkdir -p "$to/backgrounds"
  for f in "$from"/*; do
    [[ ${f##*/} == backgrounds ]] && continue
    cp -rT "$f" "$to/${f##*/}"
  done
  for f in "$from"/backgrounds/*; do
    name=${f##*/}
    has wallpapers || [[ $name =~ ^(01|18|19|20)- ]] || continue
    [[ -d $f ]] && { cp -rT "$f" "$to/backgrounds/$name"; continue; }
    stem=${name%.*}
    for old in "$to/backgrounds/$stem".*; do
      [[ -f $old && ${old##*/} != "$name" ]] && rm -f "$old"
    done
    cp "$f" "$to/backgrounds/"
  done
}
# A kit that carries the themes installs them from here; one that doesn't
# downloads them (`omarchy-patch modules`, below), the wallpapers' intros
# always so.
DOWNLOAD=""
if has themes; then
  if [[ -d themes/patch-light ]]; then
    install_theme themes/patch-light "$SHARE/themes/patch-light"
    install_theme themes/patch-dark "$SHARE/themes/patch-dark"
  else
    DOWNLOAD+="themes "
  fi
  has wallpapers && DOWNLOAD+="wallpapers "
fi

# This machine, measured once (under a second): the fastest DSP kernels
# for its CPU, and the drawing tier its windows start at.
say "Measuring this machine (audio kernels, drawing)"
"$BIN/omarchy-patch" optimize --quick >/dev/null 2>&1 || true

# 5 · The setup: system audio, DJ Booth, service, bar menu, themes.
say "Applying your setup"
# The bar's main output starts at Patch itself: the full Mixer console.
# A reinstall keeps the choice made since.
MAIN_OUTPUT="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy-patch/main-output"
[[ -s $MAIN_OUTPUT ]] || { mkdir -p "${MAIN_OUTPUT%/*}" && echo patch > "$MAIN_OUTPUT"; }
"$BIN/omarchy-patch" setup set --all "${SETUP[@]}"
# Omarchy applies a theme from its own copy, and its wallpaper picker and
# intros read that copy: a Patch theme in use is staged again, so the
# wallpapers that arrived after it was applied show (before 0.8.8's fix the
# Wallpapers download didn't, and a reinstall downloads nothing new). The
# wallpaper on screen stays.
theme_name=$(cat "$HOME/.local/state/omarchy/current/theme.name" 2>/dev/null || true)
if [[ $theme_name == patch-* ]] && command -v omarchy-theme-set >/dev/null; then
  OMARCHY_THEME_SKIP_BACKGROUND=1 omarchy-theme-set "$theme_name" </dev/null >/dev/null 2>&1 || true
fi

# A module key, then what downloads: small ones now, big ones
# (the wallpapers, the stem separator) in the background, with a
# notification when ready. An installed stem separator is refreshed.
if [[ -n $KEY ]]; then
  "$BIN/omarchy-patch" licence activate "$KEY" || echo "The key didn't activate: Settings › Modules, or omarchy-patch licence activate <key>" >&2
fi
# Auto Mix brings its stem separator: refreshed when it is here, else
# downloaded with Auto Mix once its key is active (later, when
# the key comes, Patch adds it by itself).
# The stem separator prepares its runtime with uv: where the distribution
# has no uv package (Debian, Ubuntu), uv's own installer puts it in
# ~/.local/bin.
if has ai_dj && ((STEMS)) && ((DEPS)) && ! command -v uv >/dev/null && [[ ! -x $HOME/.local/bin/uv ]]; then
  say "Installing uv (the stem separator's Python runtime)"
  curl -LsSf https://astral.sh/uv/install.sh | env UV_NO_MODIFY_PATH=1 sh >/dev/null \
    || echo "uv didn't install: the stem separator needs it (https://docs.astral.sh/uv/)" >&2
fi
if has ai_dj && ((STEMS)); then
  if [[ -e $STEMS_FILE ]]; then
    say "Refreshing the stem separator"
    "$BIN/omarchy-patch" setup stems install || echo "The stem separator didn't refresh: it is tried again with Auto Mix" >&2
  else
    DOWNLOAD+="ai_dj "
  fi
fi
if [[ -n $DOWNLOAD ]]; then
  say "Adding: $DOWNLOAD"
  "$BIN/omarchy-patch" modules add $DOWNLOAD --yes || echo "Not all could be added now: Settings › Modules, or omarchy-patch modules add $DOWNLOAD" >&2
fi

# 6 · The phone remote (with the DJ): the Booth, Mixer and library in a
# phone's browser, over Tailscale only (https://<this machine>.ts.net:8124,
# for your own Tailscale account). It runs once this machine is on
# Tailscale; without it the remote is installed but stays off.
if has dj && ((REMOTE)) && [[ -x $REMOTE_PREBUILT || -x bin/patch-remote || -x target/release/patch-remote ]]; then
  say "Installing the phone remote"
  remote_bin=${REMOTE_PREBUILT:-bin/patch-remote}
  [[ -x $remote_bin ]] || remote_bin=bin/patch-remote
  [[ -x $remote_bin ]] || remote_bin=target/release/patch-remote
  install -Dm755 "$remote_bin" "$BIN/patch-remote"
  install -Dm755 remote/patch-remote-launch "$SHARE/patch-remote-launch"
  UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
  mkdir -p "$UNITS"
  install -m644 remote/patch-remote.service remote/patch-remote-follow.path \
    remote/patch-remote-follow.service "$UNITS/"
  REMOTE_ENV="${XDG_CONFIG_HOME:-$HOME/.config}/patch-remote/environment"
  # This machine's Tailscale name and account: the only origin and the
  # only login the remote accepts.
  tailnet=$(tailscale status --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); s = d["Self"]
print(s["DNSName"].rstrip("."), d["User"][str(s["UserID"])]["LoginName"])' 2>/dev/null) || tailnet=""
  if [[ ! -s $REMOTE_ENV && -n $tailnet ]]; then
    mkdir -p "${REMOTE_ENV%/*}" && chmod 700 "${REMOTE_ENV%/*}"
    printf 'PATCH_REMOTE_ORIGIN="https://%s:8124"\nPATCH_REMOTE_USER="%s"\n' $tailnet > "$REMOTE_ENV"
  fi
  if [[ -s $REMOTE_ENV ]]; then
    # Tailscale Serve carries the phone's HTTPS to the remote on
    # 127.0.0.1:8124; a route already on that port is left alone.
    if ! tailscale serve status --json 2>/dev/null | grep -q '"8124"'; then
      tailscale serve --bg --https=8124 http://127.0.0.1:8124 >/dev/null 2>&1 \
        || sudo tailscale serve --bg --https=8124 http://127.0.0.1:8124 >/dev/null \
        || echo "Tailscale Serve didn't take port 8124 (HTTPS certificates on in the Tailscale admin?): tailscale serve --bg --https=8124 http://127.0.0.1:8124" >&2
    fi
    systemctl --user daemon-reload
    systemctl --user enable patch-remote.service patch-remote-follow.path >/dev/null 2>&1
    systemctl --user restart patch-remote.service patch-remote-follow.path
    say "Phone remote: $(sed -n 's/^PATCH_REMOTE_ORIGIN="\(.*\)"/\1/p' "$REMOTE_ENV")/ (on Tailscale, your account)"
  else
    systemctl --user daemon-reload
    echo "Phone remote installed, off: sign this machine in to Tailscale (tailscale up), then run ./install.sh again" >&2
  fi
fi

if [[ -n $REOPEN ]]; then
  say "Reopening Patch's windows ($REOPEN)"
  # shellcheck disable=SC2086 # the surfaces are separate words
  setsid -f "$BIN/omarchy-patch" $REOPEN </dev/null >/dev/null 2>&1 || true
fi
say "Done"
"$BIN/omarchy-patch" welcome
# The last step, last on screen (after the guide, which would push it out
# of sight).
if [[ $RELOGIN == 1 ]]; then
  printf '\n'
  say "One step left: log out and back in (or restart)"
  cat <<'TXT'
  Patch works now. Real-time audio, which keeps the sound steady when the
  computer is busy, starts with your next login: until then Patch runs as
  the rest of your system's audio does. Once is enough.
TXT
fi
true
