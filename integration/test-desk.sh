#!/usr/bin/env bash
# The desk (docs/desk.md), headless: the generic kit in a throwaway Ubuntu
# container on a virtual X11 display (Xvfb, Mesa's software GL), each layout
# opened in the desk and its picture saved (PATCH_DESK_SHOT), at 100 % and
# at the HiDPI scales of GNOME's and KDE's fractional settings.
#
#   integration/test-desk.sh dist/patch-<v>-linux-generic-x86_64.tar.gz <out dir> [layout…]
#
# Needs podman or docker and the network (Ubuntu's mirrors). Look at the
# pictures: every Unit of the page in its tile, nothing clipped or blurred.
set -euo pipefail
kit=$(realpath "${1:?usage: integration/test-desk.sh <generic kit .tar.gz> <out dir> [layout…]}")
out=$(mkdir -p "${2:?out dir}" && realpath "$2")
shift 2
layouts=("$@")
((${#layouts[@]})) || layouts=(simple basic-dj podcast live-code pro)
engine=$(command -v podman || command -v docker) || { echo "podman or docker is needed" >&2; exit 1; }
image=${PATCH_DESK_IMAGE:-docker.io/library/ubuntu:24.04}

probe='
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
cd /tmp && tar -xzf /kit.tar.gz && cd patch-*/
apt-get update -qq
apt-get install -y -qq $(./install.sh --print-packages) xvfb libegl-mesa0 libgl1-mesa-dri libgles2 xdotool xclip wl-clipboard fonts-dejavu-core fonts-jetbrains-mono >/dev/null
export HOME=/tmp/home XDG_RUNTIME_DIR=/tmp/run && mkdir -p $HOME $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
sleep 1
export DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1
fail=0
for scale in 1 1.25 1.5; do
  for layout in $LAYOUTS; do
    shot=/out/desk-$layout-$scale.png
    rm -f "$shot"
    WINIT_X11_SCALE_FACTOR=$scale PATCH_DESK_SHOT=$shot timeout 60 bin/omarchy-patch desk "$layout" >/out/desk-$layout-$scale.log 2>&1 || true
    if [[ -s $shot ]]; then echo "ok   $layout at $scale"; else echo "FAIL $layout at $scale"; tail -5 /out/desk-$layout-$scale.log; fail=1; fi
  done
done
# A divider dragged: the tiles on either side follow (and keep their minima).
first=${LAYOUTS%% *}
log=/out/desk-drag.log
PATCH_DESK_LOG=1 PATCH_DESK_SHOT=/out/desk-drag.png PATCH_DESK_SHOT_DELAY=9000 timeout 60 bin/omarchy-patch desk "$first" >$log 2>&1 &
desk=$!
for _ in $(seq 100); do grep -q "desk: divider" $log && break; sleep 0.1; done
sleep 3
read -r _ _ dir x y w h < <(grep "desk: divider" $log | tail -1)
before=$(grep "desk: tile" $log | tail -2 | tr "\n" " ")
cx=$((x + w / 2)); cy=$((y + h / 2))
if [[ $dir == across ]]; then tx=$((cx + 150)); ty=$cy; else tx=$cx; ty=$((cy - 120)); fi
xdotool mousemove $cx $cy sleep 0.3 mousedown 1 sleep 0.2 mousemove $(( (cx + tx) / 2 )) $(( (cy + ty) / 2 )) sleep 0.2 mousemove $tx $ty sleep 0.3 mouseup 1
wait $desk || true
after=$(grep "desk: tile" $log | tail -2 | tr "\n" " ")
if [[ -s /out/desk-drag.png && $before != "$after" ]]; then echo "ok   divider dragged: $after"; else echo "FAIL divider drag (before: $before after: $after)"; fail=1; fi
# The desk open: another layout goes into it; Patch quitting closes it.
log=/out/desk-life.log
PATCH_DESK_LOG=1 timeout 60 bin/omarchy-patch desk simple >$log 2>&1 &
desk=$!
for _ in $(seq 100); do grep -q "desk: tile eq" $log && break; sleep 0.1; done
sleep 2
bin/omarchy-patch layout pro >/out/desk-life-layout.log 2>&1 || true
for _ in $(seq 100); do grep -q "desk: tile inspector\|desk: tile meter" $log && break; sleep 0.1; done
if grep -q "desk: tile meter" $log; then echo "ok   a layout opens in the desk already open"; else echo "FAIL the open desk did not take the layout"; fail=1; fi
bin/omarchy-patch quit >/dev/null 2>&1 || true
for _ in $(seq 80); do kill -0 $desk 2>/dev/null || break; sleep 0.1; done
if kill -0 $desk 2>/dev/null; then echo "FAIL the desk stayed after Patch quit"; kill $desk; fail=1; else echo "ok   Patch quitting closes the desk"; fi
# The desk with Simple: Ctrl+K, Make room, the clipboard both ways.
log=/out/desk-use.log
PATCH_DESK_LOG=1 timeout 90 bin/omarchy-patch desk simple >$log 2>&1 &
desk=$!
for _ in $(seq 150); do grep -q "desk: focus Patch" $log && break; sleep 0.1; done
sleep 2
sock=$(sed -n "s/^desk: socket //p" $log | head -1)
win=$(xdotool search --name "Patch" | head -1)
xdotool windowfocus --sync $win 2>/dev/null || xdotool windowfocus $win
sleep 0.5
xdotool key --window $win ctrl+k
for _ in $(seq 50); do grep -q "desk: focus Patch · Controller" $log && break; sleep 0.1; done
if grep -q "desk: focus Patch · Controller" $log; then echo "ok   Ctrl+K: the controller opens with the keyboard"; else echo "FAIL Ctrl+K did not focus the controller"; fail=1; fi
xdotool key --window $win Escape
sleep 1
# Copied on the desktop, pasted inside.
printf "from-the-desktop" | xclip -selection clipboard
xdotool windowfocus $(xdotool search --name xclip 2>/dev/null | head -1) 2>/dev/null || true
xdotool windowfocus --sync $win 2>/dev/null || xdotool windowfocus $win
sleep 1
got=$(WAYLAND_DISPLAY=$sock timeout 5 wl-paste -n 2>/dev/null || true)
if [[ $got == from-the-desktop ]]; then echo "ok   the desktop clipboard pastes inside the desk"; else echo "FAIL paste inside the desk got \"$got\""; fail=1; fi
# Copied inside, pasted on the desktop.
printf "from-the-desk" | WAYLAND_DISPLAY=$sock timeout 5 wl-copy 2>/dev/null || true
sleep 1
got=$(timeout 5 xclip -o -selection clipboard 2>/dev/null || true)
if [[ $got == from-the-desk ]]; then echo "ok   a copy inside the desk pastes on the desktop"; else echo "FAIL the desktop got \"$got\""; fail=1; fi
# Make room on the Mixer: the desk is too small for Simple here, so the Mixer shows alone.
read -r _ _ _ x y w h < <(grep "desk: tile mixer" $log | tail -1)
xdotool mousemove $((x + w - 135)) $((y + 50)) sleep 0.3 click 1
sleep 1.5
read -r _ _ _ x2 y2 w2 h2 < <(grep "desk: tile mixer" $log | tail -1)
if (( h2 > h + 100 )) && ! tail -3 $log | grep -q "desk: tile eq"; then echo "ok   Make room: the Mixer gets the desk"; else echo "FAIL Make room: mixer $w x $h -> $w2 x $h2"; fail=1; fi
bin/omarchy-patch quit >/dev/null 2>&1 || true
wait $desk || true
kill %1 2>/dev/null || true
exit $fail
'
nice -n 19 "$engine" run --rm -e LAYOUTS="${layouts[*]}" -v "$kit:/kit.tar.gz:ro" -v "$out:/out" "$image" bash -c "$probe"
