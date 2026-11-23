# Patch's brand colour

**Patch red is `#F0243B`** (the user's choice, 2026-10-28; it replaced the
signal red `#e43055` and its light-theme partner `#b8143a`). It is Patch's one
accent. Violet `#9d7cf0` and indigo `#7c8ae0` are the only other voices, and
are unchanged.

| | sRGB | OKLCH |
|---|---|---|
| Patch red | `#f0243b` · 240 36 59 | L 0.614 · C 0.232 · h 23.0° |

## One red, two roles

The brand red is used exactly wherever nothing has to be read against it:
lights, the logo mark, deck A, the visualiser. A colour that marks UI or text
on a theme's ground must also meet that theme's contrast targets
(`themes/generate.py` checks them and refuses to write a theme that misses
one: the accent 3:1 on the background, accent text 4.5:1). On Patch Dark the
brand red passes (4.3:1 on `#141715`), so it is the accent itself. On Patch
Light's grey paper (`#b5bab6`) it reaches only 2.1:1, so the light theme's
accent is a darker red of the same hue (`#bd0020`, 3.4:1), as it was before.

Every other red in the themes keeps its old lightness, moved onto the new
hue (OKLCH h 23°) with the new chroma:

| Role | Patch Dark | Patch Light |
|---|---|---|
| `brand` (lights; Dark's mark) | `#f0243b` | `#f0243b` |
| `accent` (fills, markers, active state) | `#f0243b` | `#bd0020` |
| `accent_text` (red as text) | `#f95065` | `#92011a` |
| `accent_soft` | `#f0243b29` | `#bd002029` |
| `selection` | `#4d1b22` | `#da8991` |
| ANSI `red` / `bright_red` | `#f44e5e` / `#ff7d86` | `#7f0117` / `#910118` |
| Patch window error (selection: Light only) | `#f44e5e` | `#a30118` (selection `#f6d2d7`) |

## Where it is set

Change the red here and nowhere else, then run what regenerates the rest.

* **`themes/generate.py`**: the palettes of both themes. `python3
  themes/generate.py` writes every theme file: `colors.toml` (with `accent`
  and `brand`), `patch.toml` (Patch's own windows), `claude.json`, the
  terminals (Ghostty, Alacritty, Kitty), btop, the launcher and menu cards,
  and the README of each theme.
* **`themes/unlock.py`**: the lock-screen Module's plug-in Unit (brand red
  on both themes). Run `uv run --with pillow python themes/unlock.py`.
* **Patch's code**, which shows the red without a theme:
  * `crates/patch-visual/src/performance.rs`: `BRAND`, deck A's colour in
    the visualiser (as floats: 0.941 0.141 0.231).
  * `crates/patch-surfaces/ui/visual.slint`: `deck-colour-a`,
    `signal-colour-a`.
  * `remote/palette.css`: the phone remote's `--accent` and `--accent-soft`;
    `remote/visual-preview.js`: the preview palette.
  * `crates/patch-host/src/hypr.rs`: a border-gradient test fixture.
* **The Module** (Patch's mark, `docs/patch-style.md`): the Unit header
  (`Module` in `crates/patch-ui/ui/primitives.slint`, `Theme.brand`), the app
  icon `integration/desktop/omarchy-patch.svg` and the lock-screen logo
  (`themes/unlock.py`), both in `#f0243b` on both themes.
* **Images**: wallpapers, their intro videos, the theme previews and the
  remote's screenshots carry the red in their pixels.
  `themes/recolour.py IMAGE [VIDEO...]` moves an image's red onto the brand
  hue (and its intro with the same LUT, so the last frame still lands on the
  wallpaper). It leaves every other colour alone. Image prompts in
  `themes/BACKGROUND-PROMPTS.md` name the new red for new renders.
* **Lights**: the keyboard (Logitech G915), the mouse (PRO 2), the case and
  the RAM follow the active theme through `rgb-omarchy-sync`
  (`~/Projects/Omarchy-setup/rgb-omarchy-sync`, run by Omarchy's
  `theme-set` hook). It takes a theme's `brand` key when it has one, else
  `accent`, so both Patch themes light the hardware in `#F0243B`. The case
  and RAM keep per-theme colour corrections in
  `~/.config/rgb-omarchy-sync/profiles/patch-{dark,light}.conf` (base colour
  `brand`; the light profile uses the dark one's corrections, since both now
  drive the same red).

Not the brand: the built-in fallback accent in `crates/patch-ui/ui/theme.slint`
(amber, used only without a theme), the Club Booth's hardware LEDs
(`#ff3030`, drawn after real CDJs) and the semantic warn/error colours of
other themes.
