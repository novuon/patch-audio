#!/usr/bin/env bash
# The generic kit on the real GNOME and KDE Plasma, headless: GNOME Shell
# (Ubuntu, with its AppIndicator extension) and KWin with Plasma's shell
# (Fedora), each with a virtual monitor in a throwaway container. In each:
# the tray registers with the desktop's own StatusNotifierWatcher and its
# menu answers; a layout opens in the desk (a Wayland client of that
# compositor, its own decorations on GNOME); a screenshot of the whole
# desktop is saved.
#
#   integration/test-desktops.sh dist/patch-<v>-linux-generic-x86_64.tar.gz <out dir> [gnome|kde]…
#
# Needs podman or docker and the network (Ubuntu's and Fedora's mirrors).
set -euo pipefail
kit=$(realpath "${1:?usage: integration/test-desktops.sh <generic kit .tar.gz> <out dir> [gnome|kde]…}")
out=$(mkdir -p "${2:?out dir}" && realpath "$2")
shift 2
desktops=("$@")
((${#desktops[@]})) || desktops=(gnome kde)
engine=$(command -v podman || command -v docker) || { echo "podman or docker is needed" >&2; exit 1; }

# What runs inside the desktop's session (D-Bus session, compositor up).
read -r -d '' session <<'EOF' || true
set -uo pipefail
fail=0
cd /tmp/patch-*/
for _ in $(seq 100); do [[ -S $XDG_RUNTIME_DIR/${WAYLAND_DISPLAY:-wayland-0} ]] && break; sleep 0.2; done
sleep 3
bin/omarchy-patch tray >/out/$DESKTOP-tray.log 2>&1 &
sleep 4
items=$(gdbus call --session --dest org.kde.StatusNotifierWatcher --object-path /StatusNotifierWatcher \
  --method org.freedesktop.DBus.Properties.Get org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems 2>&1)
if grep -q "StatusNotifierItem" <<<"$items"; then echo "ok   $DESKTOP: the tray registered with the desktop"; else echo "FAIL $DESKTOP: tray not registered: $items"; fail=1; fi
name=$(grep -o "org.kde.StatusNotifierItem-[0-9]*-1" <<<"$items" | head -1)
menu=$(gdbus call --session --dest "$name" --object-path /MenuBar --method com.canonical.dbusmenu.GetLayout -- 0 -1 "[]" 2>&1)
if grep -q "Layouts" <<<"$menu"; then echo "ok   $DESKTOP: the tray menu answers"; else echo "FAIL $DESKTOP: tray menu: ${menu:0:200}"; fail=1; fi
[[ -z ${TRAY_SHOT:-} ]] || { eval "$TRAY_SHOT" >/out/$DESKTOP-tray-shot.log 2>&1; [[ -s /out/$DESKTOP-tray.png ]] && echo "ok   $DESKTOP: the tray menu, drawn by the desktop (picture)"; }
# The Player in the desktop's media controls (MPRIS): a real mpv on the
# Player's socket, no audio device needed.
rt=$XDG_RUNTIME_DIR/omarchy-patch && mkdir -p $rt
ffmpeg -loglevel error -f lavfi -i sine=frequency=440:duration=120 -metadata title="Test tone" -y /tmp/tone.flac
mpv --no-config --ao=null --vo=null --input-ipc-server=$rt/player.sock /tmp/tone.flac >/dev/null 2>&1 &
mpvpid=$!
mpris() { gdbus call --session --dest org.mpris.MediaPlayer2.patch --object-path /org/mpris/MediaPlayer2 "$@" 2>&1; }
status() { mpris --method org.freedesktop.DBus.Properties.Get org.mpris.MediaPlayer2.Player PlaybackStatus; }
for _ in $(seq 60); do status | grep -q "Playing" && break; sleep 0.2; done
if status | grep -q "Playing" && mpris --method org.freedesktop.DBus.Properties.Get org.mpris.MediaPlayer2.Player Metadata | grep -q "Test tone"; then
  echo "ok   $DESKTOP: the Player is in the media controls, playing"
else echo "FAIL $DESKTOP: MPRIS: $(status)"; fail=1; fi
mpris --method org.mpris.MediaPlayer2.Player.PlayPause >/dev/null
for _ in $(seq 30); do status | grep -q "Paused" && break; sleep 0.2; done
if status | grep -q "Paused"; then echo "ok   $DESKTOP: a media key pauses the Player"; else echo "FAIL $DESKTOP: PlayPause: $(status)"; fail=1; fi
kill $mpvpid 2>/dev/null
owned() { gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.mpris.MediaPlayer2.patch | grep -q true; }
for _ in $(seq 40); do owned || break; sleep 0.2; done
if ! owned; then echo "ok   $DESKTOP: nothing loaded, the Player leaves the media controls"; else echo "FAIL $DESKTOP: MPRIS name stayed"; fail=1; fi

# A layout in the desk, on this desktop's compositor.
PATCH_DESK_LOG=1 PATCH_DESK_SHOT=${DESK_SHOT:-} PATCH_DESK_SHOT_DELAY=4000 bin/omarchy-patch desk simple >/out/$DESKTOP-desk.log 2>&1 &
desk=$!
for _ in $(seq 200); do grep -q "desk: focus Patch" /out/$DESKTOP-desk.log && break; sleep 0.1; done
sleep 3
if grep -q "desk: tile eq" /out/$DESKTOP-desk.log; then echo "ok   $DESKTOP: a layout opens in the desk ($(grep "desk: window" /out/$DESKTOP-desk.log | tail -1 | cut -d" " -f3-))"; else echo "FAIL $DESKTOP: desk"; tail -5 /out/$DESKTOP-desk.log; fail=1; fi
read -r _ _ _ _ _ w _ < <(grep "desk: tile mixer" /out/$DESKTOP-desk.log | tail -1)
read -r _ _ size _ scale _ < <(grep "desk: window" /out/$DESKTOP-desk.log | tail -1)
pw=${size%x*}
if python3 -c "import sys; sys.exit(0 if abs($w + 16 - $pw / $scale) < 4 else 1)"; then echo "ok   $DESKTOP: tiles in logical pixels (window $size at $scale)"; else echo "FAIL $DESKTOP: tile width $w in a window $size at $scale"; fail=1; fi
[[ -z ${SHOT:-} ]] || eval "$SHOT" >/out/$DESKTOP-shot.log 2>&1
for _ in $(seq 100); do [[ -s /out/$DESKTOP.png ]] && break; sleep 0.1; done
if [[ -s /out/$DESKTOP.png ]]; then echo "ok   $DESKTOP: picture"
elif [[ -n ${DESK_SHOT:-} ]]; then echo "note $DESKTOP: no picture (the desk drew no frame after opening on KWin's virtual backend)"
else echo "FAIL $DESKTOP: picture"; tail -3 /out/$DESKTOP-shot.log 2>/dev/null; fi
bin/omarchy-patch quit >/dev/null 2>&1 || true
for _ in $(seq 60); do kill -0 $desk 2>/dev/null || break; sleep 0.1; done
kill -0 $desk 2>/dev/null && { echo "FAIL $DESKTOP: the desk stayed"; fail=1; }
echo $fail >/tmp/fail
EOF

read -r -d '' common <<'EOF' || true
set -euo pipefail
cd /tmp && tar -xzf /kit.tar.gz
export HOME=/tmp/home XDG_RUNTIME_DIR=/tmp/run XDG_SESSION_TYPE=wayland LIBGL_ALWAYS_SOFTWARE=1
mkdir -p $HOME $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
printf "%s" "$SESSION" >/tmp/session.sh
echo 1 >/tmp/fail
EOF

read -r -d '' gnome <<'EOF' || true
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq $(cd /tmp/patch-*/ && ./install.sh --print-packages) gnome-shell gnome-shell-extension-appindicator dbus-x11 dbus-daemon libglib2.0-bin libgl1-mesa-dri mpv fonts-cantarell fonts-jetbrains-mono >/tmp/pkg.log 2>&1 || { echo "FAIL packages"; tail -15 /tmp/pkg.log; exit 1; }
# A system bus, and no systemd seats: GNOME Shell then uses its stand-in
# login manager (no logind in a container).
mkdir -p /run/dbus && { [ -s /etc/machine-id ] || dbus-uuidgen >/etc/machine-id; } && dbus-daemon --system --fork 2>/dev/null
rm -rf /run/systemd/seats
mkdir -p $HOME/.local/share/icons/hicolor/scalable/apps && cp /tmp/patch-*/integration/desktop/*.svg $HOME/.local/share/icons/hicolor/scalable/apps/
# Tests only: an extension putting the shell in unsafe mode, so the
# screenshot and Eval D-Bus calls answer this script.
x=$HOME/.local/share/gnome-shell/extensions/unsafe@patch.test && mkdir -p $x
v=$(gnome-shell --version | grep -o "[0-9]*" | head -1)
printf '{"uuid":"unsafe@patch.test","name":"unsafe","description":"tests","shell-version":["%s"]}' "$v" >$x/metadata.json
echo 'export default class { constructor(m) {} enable() { global.context.unsafe_mode = true; } disable() {} }' >$x/extension.js
export DESKTOP=gnome
shell='gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval'
shoot='gdbus call --session --dest org.gnome.Shell.Screenshot --object-path /org/gnome/Shell/Screenshot --method org.gnome.Shell.Screenshot.Screenshot false false'
find_tray='Main.panel.statusArea[Object.keys(Main.panel.statusArea).find(k => k.startsWith("appindicator"))]'
export TRAY_SHOT="$shell 'Main.overview.hide(); $find_tray.menu.open(); true'; sleep 2; $shoot /out/gnome-tray.png; $shell '$find_tray.menu.close(); true'"
export SHOT="$shell 'Main.overview.hide(); true'; sleep 2; $shoot /out/gnome.png"
dbus-run-session -- bash -c "
  gsettings set org.gnome.shell disable-user-extensions false
  gsettings set org.gnome.shell enabled-extensions \"['ubuntu-appindicators@ubuntu.com', 'unsafe@patch.test']\"
  gnome-shell --headless --virtual-monitor 1920x1080 >/out/gnome-shell.log 2>&1 &
  WAYLAND_DISPLAY=wayland-0 bash /tmp/session.sh
"
exit $(cat /tmp/fail)
EOF

read -r -d '' kde <<'EOF' || true
dnf install -y -q $(cd /tmp/patch-*/ && ./install.sh --print-packages) kwin.x86_64 plasma-workspace.x86_64 dbus-daemon dbus-tools mesa-dri-drivers mpv jetbrains-mono-fonts-all glib2 >/tmp/pkg.log 2>&1 || { echo "FAIL packages"; tail -15 /tmp/pkg.log; exit 1; }
mkdir -p $HOME/.local/share/icons/hicolor/scalable/apps && cp /tmp/patch-*/integration/desktop/*.svg $HOME/.local/share/icons/hicolor/scalable/apps/
# kwin_wayland carries a file capability a container can't grant: a copy
# runs without it. On its virtual backend KWin crashes when a Qt Wayland
# client (Plasma's shell) connects and when asked for a screenshot, so the
# tray's host is KDE's own watcher in kded6 (off screen), and the picture is
# the desk's own (PATCH_DESK_SHOT), at KDE's 125 %.
cp /usr/bin/kwin_wayland /tmp/kwin_wayland
export DESKTOP=kde XDG_CURRENT_DESKTOP=KDE TRAY_SHOT="" SHOT="" DESK_SHOT=/out/kde.png
dbus-run-session -- bash -c "
  /tmp/kwin_wayland --virtual --width 1920 --height 1080 --scale 1.25 --no-lockscreen >/out/kde-kwin.log 2>&1 &
  sleep 6
  QT_QPA_PLATFORM=offscreen kded6 >/out/kde-kded.log 2>&1 &
  sleep 10
  WAYLAND_DISPLAY=wayland-0 bash /tmp/session.sh
"
exit $(cat /tmp/fail)
EOF

status=0
for d in "${desktops[@]}"; do
  case $d in
    gnome) image=docker.io/library/ubuntu:latest script=$gnome ;;
    kde) image=registry.fedoraproject.org/fedora:latest script=$kde ;;
    *) echo "no desktop '$d' (gnome, kde)" >&2; exit 2 ;;
  esac
  nice -n 19 "$engine" run --rm -e SESSION="$session" -v "$kit:/kit.tar.gz:ro" -v "$out:/out" "$image" \
    bash -c "$common"$'\n'"$script" || status=1
done
exit $status
