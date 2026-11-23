# The Patch style

How Patch's windows apply Patch's own style guide (*Patch House Style 2.0*,
November 2026, in Dutch a *huisstijl*). It is not a re-layout: every Unit
keeps its controls, its responsive layouts and the rules in
`docs/decisions.md`.

Settings · Style offers three styles, and **all three follow the guide's
chrome and type**: the Unit header, the Module, Space Grotesk titles and
JetBrains Mono for every label and value.

* **Classic** (the default): the Omarchy layouts with flat, square
  controls in the theme's colours.
* **Patch**: the same layouts with the guide's panels, controls, corners
  and meter colours. In code `Theme.patch-style` (Slint) and `Style::Patch`
  (`patch_ui::style`).
* **Pro gear**: the Patch look, laid out like club gear.

## The idea

*Plug in a Unit. It's live.* Every tool is a Unit in its own tile, plugged
into one engine that keeps playing. The mark is exactly that, and red is
where you work: one red element per Unit, for what is live or in focus.

## The Module (the mark)

A socket Unit with a red Unit plugged into it. Built on x = mark height
/ 23: the socket Unit 23x square, a 2x gap, the plug-in Unit 15x square on
the same centre; the socket 7x deep and 8x tall, the plug 8x long and 6x
tall, 1x of air on its three sides. Outer corners 1.2x, socket and plug
0.4x. Paper socket on Ink, Ink socket on Paper; the plug-in Unit is always
Patch Red. Never stretched, rotated, recoloured, separated or shadowed.

Where it is drawn:

* `Module` in `crates/patch-ui/ui/primitives.slint`: every Unit header,
  on whole pixels (the air never drops under a pixel, as the guide's pixel
  masters do below 48 px);
* `integration/desktop/omarchy-patch.svg`: the app icon (Ink tile, the mark
  62 % of its width, corners 22.5 %), installed to
  `share/icons/hicolor/scalable/apps/`; also the notifications' icon;
* `themes/unlock.py`: the lock-screen logo of Patch Dark and Patch Light.

## Colour (Patch Dark)

| Role | Value | Use |
| --- | --- | --- |
| `bg` | Ink `#141114` | the window's ground |
| `panel` | ≈ Graphite 900 `#1E1B1F` | cards: a strip, a section (`Panel`) |
| `raised` | Graphite 800 `#2A262B` | controls: buttons, pads, knob bodies |
| `border_strong` | `#5B565C` | control edges, knob tracks |
| `text_bright` | Paper `#F7F4EE` | names, values, selections |
| `accent` | Patch Red `#F0243B` | the one red: the LIVE dot, CUE on, mute, record, the playing deck, the primary action |
| `level`, `level_warn` | Meter green `#22C7A0`, Meter amber `#FFB020` | inside meters only (the Patch meter style) |

Set in `themes/generate.py` (contrast-checked; the desktop's Patch Dark uses
the same Ink/Graphite/Paper). Red is never tinted, darkened or swapped for a
close red in Patch Dark; a changed value (an EQ cut, a send, pan) is Paper,
not red. Patch Light keeps its darker accent `#bd0020` on its grey paper
(`docs/decisions.md`).

## Type (every style)

* **Space Grotesk 700**: Unit titles, headlines, track titles.
* **JetBrains Mono** (the Omarchy font): every value, label and key. Labels
  600 in caps, tracked +14 % (`Caption`, `Theme.caps-track`); names in
  strips, pads and lists 600 (`Theme.font-ui`); large readouts such as BPM
  and key 500, tabular (`Theme.font-data`), so numbers never jump.

Inter, used by the first version of the style, is no longer bundled.

## Unit chrome (every style)

The header is 48 px at Patch's 13 px body in Patch and Pro gear, 34 px in
Classic, whose layouts are fitted to it (`Theme.header-h`): the Module,
PATCH (Mono 600 caps, quiet), the Unit's title (Space Grotesk 700) and its
state (Mono) on the left; on the right **LIVE** as a small red dot and
faint Mono caps, no fill: a layout shows it in every window, so it must
not shout (the user, 2026-11-14). A Unit on mock data or a planned one
says MOCK or PLANNED the same way, without the dot. Clicking PATCH or the Module opens the
controller. A narrow window keeps the Module, the title and the badge.

The footer keeps its key hints and the version on the right.

## Shapes (Patch and Pro gear)

`Theme.r-panel` 6 px for cards, `Theme.r-control` 4 px for controls.

## Knobs (Patch and Pro gear)

A Graphite body, a 270° track and the value as an arc, so a knob that
hides its pointer still shows its value.

## Meters

Classic, Banded and LED are unchanged; **Patch** (Meter green, Amber from
−6 dB, red only on clipping) is a fourth style. Settings · Meters picks the
default.

## Left out on purpose

The user declined these parts of the guide (2026-11-12); don't add them:

* the 2 px red focus border on the focused Unit (Patch's themes keep
  Hyprland's focus highlight off by default, `docs/decisions.md`);
* the 48 px footer (it stays 26 px, so no Unit loses room);
* Patch Light on Paper with `#F0243B` (it keeps its grey paper and
  `#bd0020`);
* Focus Grid icons for the layout launchers, and the Focus Grid loading
  animation.

## Checking

`omarchy-patch shots <dir> <surface>` with `UNIT_THEME_DIR=themes/patch-dark`
and `PATCH_SHOT_SIZES=WxH,…` (include the live window sizes), and
`tools/style-shots.sh <dir> [deck|crossfader4|…]` for four decks, stems and
Auto Mix timeline. Budgets that pick a layout include the panels' padding.
