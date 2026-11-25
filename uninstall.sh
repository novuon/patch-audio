#!/usr/bin/env bash
# Remove Patch installed by ./install.sh.
#
#   ./uninstall.sh          remove Patch; keep your library, analyses and settings
#   ./uninstall.sh --purge  remove those too
#   PREFIX=/usr/local ./uninstall.sh   when installed with another prefix
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/omarchy-patch"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
PURGE=0
[[ "${1:-}" == "--purge" ]] && PURGE=1

say() { printf '\033[1m» %s\033[0m\n' "$*"; }

if [[ -x "$BIN" ]]; then
  # System audio off first: the default output goes back to your device.
  "$BIN" setup system_audio off >/dev/null 2>&1 || true
  say "Taking Patch out of the bar and the session"
  "$BIN" bar uninstall >/dev/null 2>&1 || true
  "$BIN" service uninstall >/dev/null 2>&1 || true
  # Its unit file is gone, but systemd keeps it "active" until told.
  systemctl --user stop omarchy-patch.service >/dev/null 2>&1 || true
  # Windows, the recorder and the engine (audio goes back to PipeWire).
  "$BIN" quit >/dev/null 2>&1 || true
  "$BIN" engine quit --force >/dev/null 2>&1 || true
  # The librarian (the DJ module's background analysis) runs on its own:
  # stopped by its exact command line, never by a pattern.
  for pid in $(pgrep -u "$(id -u)" -x omarchy-patch); do
    if tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -qx "$BIN librarian "; then
      kill "$pid" 2>/dev/null || true
    fi
  done
fi
# The phone remote (its Tailscale Serve route on 8124 stays: `tailscale
# serve --https=8124 off` removes it).
UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
if [[ -f $UNITS/patch-remote.service ]]; then
  say "Removing the phone remote"
  systemctl --user disable --now patch-remote.service patch-remote-follow.path >/dev/null 2>&1 || true
  rm -f "$UNITS"/patch-remote.service "$UNITS"/patch-remote-follow.path "$UNITS"/patch-remote-follow.service
  systemctl --user daemon-reload >/dev/null 2>&1 || true
fi
rm -f "$PREFIX/bin/patch-remote" "$PREFIX/share/omarchy-patch/patch-remote-launch"

# Patch's own devices (Patch, Program, Input, System Sounds and its
# loopback) would otherwise stay until the next login.
if command -v pactl >/dev/null; then
  pactl list modules short 2>/dev/null | while IFS=$'\t' read -r index name args; do
    case "$name $args" in
      "module-null-sink "*sink_name=patch\ *|"module-null-sink "*sink_name=patch.*|"module-loopback "*source=patch.*)
        pactl unload-module "$index" 2>/dev/null || true ;;
    esac
  done
fi

# A download in progress stops (`omarchy-patch modules`).
systemctl --user stop omarchy-patch-modules.service >/dev/null 2>&1 || true

# Omarchy off a Patch theme first, onto its default: not onto a theme
# that is no longer in its menu.
current="${XDG_STATE_HOME:-$HOME/.local/state}/omarchy/current/theme.name"
if [[ -f $current && $(<"$current") == patch-* ]] && command -v omarchy-theme-set >/dev/null; then
  omarchy-theme-set tokyo-night >/dev/null 2>&1 || true
fi

# The theme links install.sh made (a theme copied there by hand stays).
for t in patch-light patch-dark; do
  link="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/themes/$t"
  [[ -L $link ]] && rm -f "$link"
done

say "Removing the program and its desktop entries"
[[ -L $PREFIX/bin/patch-audio ]] && rm -f "$PREFIX/bin/patch-audio"
rm -f "$BIN" "$APPS"/omarchy-patch.desktop "$APPS"/omarchy-patch-*.desktop  # the controller and the layout apps
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/omarchy-patch.svg" \
      "$PREFIX/share/icons/hicolor/scalable/apps/omarchy-patch-red.svg" \
      "$PREFIX/share/icons/hicolor/scalable/apps/omarchy-patch-amber.svg"
# The tray (outside Omarchy): no longer started with the session.
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/omarchy-patch-tray.desktop"
rm -rf "$PREFIX/share/omarchy-patch/themes"  # only the themes: your library lives beside them
# What `omarchy-patch modules` downloaded and kept track of (the stem
# separator stays with your library; --purge takes it).
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/omarchy-patch/modules" \
       "${XDG_STATE_HOME:-$HOME/.local/state}/omarchy-patch/modules" \
       "${XDG_CACHE_HOME:-$HOME/.cache}/omarchy-patch/downloads"

if [[ $PURGE == 1 ]]; then
  say "Removing your Patch library, analyses and settings"
  rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/omarchy-patch" \
         "${XDG_CONFIG_HOME:-$HOME/.config}/patch-remote" \
         "${XDG_DATA_HOME:-$HOME/.local/share}/omarchy-patch" \
         "${XDG_STATE_HOME:-$HOME/.local/state}/omarchy-patch" \
         "${XDG_CACHE_HOME:-$HOME/.cache}/omarchy-patch"
else
  echo "Kept: your library, analyses and settings (./uninstall.sh --purge removes them)."
fi
say "Patch is removed."
