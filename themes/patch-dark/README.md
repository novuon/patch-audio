# Patch Dark

A professional Omarchy theme made for [Patch](https://github.com/novuon/Patch),
the Omarchy audio environment, and just as usable without it. Patch Light inverted: an ink ground, paper-grey text, 1px hairline rules, square corners and the same signal red.
Its sibling is [Patch Light](../patch-light/README.md).

Every colour is defined exactly, in every file: nothing is left to mixing
by Omarchy or by apps. Contrast is checked when the theme is generated.

| Role | Colour | Use | On the ground |
|---|---|---|---|
| background | `#141114` | the ground | 1.0:1 |
| dark_background | `#100e10` | recessed: panel heads, strips | 1.0:1 |
| lighter_background | `#1e1b1f` | raised: cards, message blocks | 1.1:1 |
| foreground | `#d2cdc8` | text, the focused window border | 11.9:1 |
| light_foreground | `#aaa5a8` | secondary text | 7.7:1 |
| dark_foreground | `#8f8a8f` | faint text: captions, scales | 5.5:1 |
| muted | `#8f8a8f` | muted text: the terminal's bright black (Codex, Claude, herdr) | 5.5:1 |
| border | `#2a262b` | hairlines between panels | 1.3:1 |
| rule | `#4a454b` | edges, inactive window borders (graphics only) | 2.0:1 |
| accent | `#f0243b` | signal red: markers, fills, the active state | 4.5:1 |
| accent_text | `#f6717f` | the red as text (Claude Code) | 6.8:1 |
| selection | `#4d1b22` | selected rows | 1.3:1 |

Text reads at 4.5:1 or more everywhere: the ANSI colours (`#f44e5e` · `#d9ad3c` · `#e07a45` · `#5fb070` · `#4fb3bd` · `#7c8ae0` · `#9d7cf0` · `#b08a64`, and
their bright versions), the muted grey, and every colour Claude Code writes
text in, on the ground and on its message blocks. Only the accent and the
rules are graphics (3:1).

## Install

With Patch: `./install.sh` links both themes into `~/.config/omarchy/themes`.
On their own, from a checkout:

```sh
ln -sfn "$PWD" ~/.config/omarchy/themes/patch-dark
omarchy-theme-set patch-dark
```

A linked theme uses its `hyprland.lua` (1px borders, square corners, no
shadows). `omarchy-theme-install <git-url>` works too, but Omarchy leaves a
git-installed theme's `.lua` out, so windows keep the standard border style.

## Agents in herdr

Claude Code gets this theme's `claude.json`. Codex and herdr take the
terminal palette Omarchy generates from `colors.toml`. For herdr, follow it
in light and dark alike (its light built-ins are made for a near-white
ground):

```toml
[theme]
name = "terminal"
auto_switch = true
dark_name = "terminal"
light_name = "terminal"
```

Codex: its `ansi` syntax theme uses only the terminal palette, so it
follows the theme too (`~/.codex/config.toml`):

```toml
[tui]
theme = "ansi"
```

Codex and Claude Code read the terminal's background when they start.
Herdr answers for its panes with the colours it read last, and re-reads
them on `SIGWINCH`: after a theme change, signal the herdr clients (an
Omarchy `theme-set` hook can do it) and restart the agents started before.

## Files

- `colors.toml`: the palette; Omarchy builds terminal, editor, bar and lock
  screen configs from it.
- `patch.toml`: every Patch colour role, exactly.
- `claude.json`: Claude Code.
- `hyprland.lua`: window borders for a linked theme.
- `ghostty.conf`, `alacritty.toml`, `kitty.conf`: the terminal palette with
  faint text at 75% (Codex's secondary text stays readable). Omarchy uses
  them for a linked theme; a git-installed one gets its own templates.
- `shell.launcher.toml`, `shell.menu.toml`: opaque cards, signal-red selection.
- `btop.theme`, `icons.theme` (Yaru-red-dark), `chromium.theme`.
- `backgrounds/`: Stylus, Console, Folded Dimension, Crosspoint, Acoustic
  Core, a plain framed sheet, Signal Ribbon, Phase Orbit, Space Age, Resonance
  and a poster. The first six (01–06) are also the public theme's, under the
  same names. The nine audio and sculptural wallpapers have matching light and
  dark versions; their prompts are recorded in
  [BACKGROUND-PROMPTS.md](../BACKGROUND-PROMPTS.md).
  Each one has an intro in `backgrounds/intros/`: a six-second clip Omarchy
  plays when a theme switch lands on it, ending on the still (generators in
  `themes/intros/`).
- `unlock.png`: the lock-screen logo (`unlock.py` draws it).
- `preview.png`, `preview-unlock.png`: the theme and lock-screen pickers.

The colours come from `generate.py` in the Patch repository (`themes/`);
change them there and run it.

## Licence

MIT, see `LICENSE`.
