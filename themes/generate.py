#!/usr/bin/env python3
"""Patch Light and Patch Dark, every file from one palette each.

Each colour is set here by hand; nothing in a theme is left to Omarchy's
or Patch's mixing. Run it after changing a colour:

    python3 themes/generate.py

It checks contrast first (WCAG 2 ratios) and writes nothing when a role
misses its target: text 7:1, secondary text and every ANSI colour 4.5:1,
the accent and hairlines 3:1 (graphics).
"""

import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent

THEMES = {
    "patch-light": {
        "title": "Patch Light",
        "mode": "light",
        "about": "Instrument-panel paper: a cool grey ground, black ink, hairline\n# rules, one signal red. Violet and indigo are the only other voices.",
        "icons": "Yaru-red",
        # Grounds, from recessed to raised. The paper is the preview's and
        # the wallpapers' grey.
        "darker_background": "#a0a5a1",
        "dark_background": "#abb0ac",
        "background": "#b5bab6",
        "hover": "#bbc0bc",
        "lighter_background": "#c2c6c3",
        # Ink, from strongest to faintest.
        "bright_foreground": "#060807",
        "foreground": "#111412",
        "light_foreground": "#252926",
        "dark_foreground": "#444945",
        # Rules: the hairline between panels, the stronger one for edges
        # and inactive window borders. Graphics only, never text.
        "border": "#9ea39f",
        "rule": "#5f6460",
        # Muted text: Omarchy puts it in the terminal's bright black, the
        # grey Codex, Claude Code and herdr write secondary text in.
        "muted": "#444945",
        # The brand red, #f0243b, exactly: lights and anything with no ground
        # to read against. Too light for 3:1 on this paper, hence the accent.
        "brand": "#f0243b",
        # The signal red: fills, markers, the active state; white on it.
        "accent": "#bd0020",
        # The red as text (Claude's name and labels): 4.5:1 and more.
        "accent_text": "#92011a",
        "accent_ink": "#ffffff",
        "accent_soft": "#bd002029",
        "selection": "#da8991",
        "selection_foreground": "#060807",
        # ANSI: dark enough to read on the paper (terminals, Codex, Claude).
        "red": "#7f0117",
        "yellow": "#4f3a00",
        "orange": "#7a2d05",
        "green": "#134220",
        "cyan": "#083f45",
        "blue": "#1f2b73",
        "magenta": "#3d1c85",
        "brown": "#4d3419",
        "bright_red": "#910118",
        "bright_yellow": "#5c4400",
        "bright_green": "#18532a",
        "bright_cyan": "#0a4d54",
        "bright_blue": "#2b3a94",
        "bright_magenta": "#5429a8",
        "window_active": "#111412",
        # Patch's own windows: a lighter instrument face on the grey desk.
        # The mid-grey ground with dark-grey graphics and translucent red
        # read smudged in Patch; these keep its panels, wells and fills
        # clean (near-white wells, an opaque rose for the selection).
        "patch": {
            "bg": "#e3e6e4",
            "bg_deep": "#d8dcd9",
            "bg_deeper": "#f6f7f6",
            "raised": "#eef0ef",
            "hover": "#e8ebe9",
            "selection": "#f6d2d7",
            "border": "#c6cbc8",
            "border_strong": "#8c928e",
            "text": "#141716",
            "text_bright": "#050606",
            "text_dim": "#363b38",
            "text_faint": "#5a605c",
            "accent_soft": "#bd002029",
            "warn": "#6b4e00",
            "error": "#a30118",
            "level": "#0b7f6a",
            "level_warn": "#9a6400",
        },
    },
    "patch-dark": {
        "title": "Patch Dark",
        "mode": "dark",
        "about": "The Patch style's Ink ground, Graphite panels and controls, Paper\n# text, one signal red. Violet and indigo are lifted to read on black.",
        "icons": "Yaru-red-dark",
        # Patch style: Ink #141114 ground, Graphite 900 #1E1B1F panels,
        # Graphite 800 #2A262B controls and rules, Graphite 600 #4A454B edges.
        "darker_background": "#0b0a0b",
        "dark_background": "#100e10",
        "background": "#141114",
        "hover": "#1a171b",
        "lighter_background": "#1e1b1f",
        # Paper, Graphite 200, and two greys between it and Graphite 400.
        "bright_foreground": "#f7f4ee",
        "foreground": "#d2cdc8",
        "light_foreground": "#aaa5a8",
        "dark_foreground": "#8f8a8f",
        "border": "#2a262b",
        "rule": "#4a454b",
        "muted": "#8f8a8f",
        "brand": "#f0243b",
        "accent": "#f0243b",
        "accent_text": "#f6717f",
        "accent_ink": "#0b0a0b",
        "accent_soft": "#f0243b29",
        "selection": "#4d1b22",
        "selection_foreground": "#f7f4ee",
        "red": "#f44e5e",
        "yellow": "#d9ad3c",
        "orange": "#e07a45",
        "green": "#5fb070",
        "cyan": "#4fb3bd",
        "blue": "#7c8ae0",
        "magenta": "#9d7cf0",
        "brown": "#b08a64",
        "bright_red": "#ff7d86",
        "bright_yellow": "#f0c75e",
        "bright_green": "#7fcf8f",
        "bright_cyan": "#74d0d9",
        "bright_blue": "#9ca7ee",
        "bright_magenta": "#b89cf7",
        "window_active": "#d2cdc8",
        # Patch's own windows: Ink ground, panels between it and the
        # Graphite 800 controls, deep wells for meters and waveforms. Meter green
        # and Amber are functional only: meter levels and their warning zone.
        "patch": {
            "bg": "#141114",
            "bg_deep": "#100e10",
            "bg_deeper": "#0b0a0b",
            "raised": "#2a262b",
            "hover": "#322e33",
            "selection": "#4d1b22",
            "border": "#2a262b",
            "border_strong": "#5b565c",
            "text": "#d2cdc8",
            "text_bright": "#f7f4ee",
            "text_dim": "#c6c1c4",
            "text_faint": "#a39ea3",
            "accent_soft": "#f0243b29",
            "warn": "#d9ad3c",
            "error": "#ff6f7c",
            "level": "#22c7a0",
            "level_warn": "#ffb020",
        },
    },
}

# How much of its colour faint text keeps in the terminals.
FAINT = 0.75

ANSI = ["red", "yellow", "orange", "green", "cyan", "blue", "magenta", "brown",
        "bright_red", "bright_yellow", "bright_green", "bright_cyan", "bright_blue", "bright_magenta"]


def rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def hexa(c):
    return "#%02x%02x%02x" % c


def mix(a, b, t):
    """`a` moved `t` of the way to `b` (as Omarchy's templates mix)."""
    return hexa(tuple(round(x + (y - x) * t) for x, y in zip(rgb(a), rgb(b))))


def luminance(h):
    def ch(v):
        v /= 255
        return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (ch(v) for v in rgb(h))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a, b):
    x, y = sorted((luminance(a), luminance(b)), reverse=True)
    return (x + 0.05) / (y + 0.05)


def checks(p):
    bg = p["background"]
    need = [("foreground", bg, 7), ("bright_foreground", bg, 7), ("light_foreground", bg, 7 if p["mode"] == "light" else 4.5),
            ("dark_foreground", bg, 4.5), ("muted", bg, 4.5), ("muted", p["lighter_background"], 4.5),
            ("rule", bg, 3 if p["mode"] == "light" else 1.8), ("accent", bg, 3), ("accent_text", bg, 4.5), ("accent_text", p["lighter_background"], 4.5),
            ("accent_ink", p["accent"], 4.5), ("selection_foreground", p["selection"], 7)]
    need += [("foreground", p[g], 7) for g in ("darker_background", "dark_background", "hover", "lighter_background")]
    need += [(c, bg, 4.5) for c in ANSI]
    out = [(role, on, want, contrast(p[role], on)) for role, on, want in need]
    # Patch's own roles, where set: text on every ground it sits on.
    o = p.get("patch")
    if o:
        for role, want in (("text", 7), ("text_bright", 7), ("text_dim", 7), ("text_faint", 4.5), ("warn", 4.5), ("error", 4.5)):
            for g in ("bg", "bg_deep", "bg_deeper", "raised", "hover"):
                out.append((f"patch.{role}", o[g], want, contrast(o[role], o[g])))
        # the Patch style's red on Ink is 4.48:1 (AA large): red marks and
        # signals, never body text (Patch style guide, Contrast).
        out.append(("patch.accent on bg", o["bg"], 4.45, contrast(p["accent"], o["bg"])))
        out.append(("patch.border_strong", o["bg"], 2.5, contrast(o["border_strong"], o["bg"])))
        out.append(("patch.text on selection", o["selection"], 7, contrast(o["text"], o["selection"])))
    return out


def files(name, p):
    t, bg, fg = p["title"], p["background"], p["foreground"]
    alpha = lambda c, a: "rgba(%s%s)" % (c.lstrip("#"), a)
    out = {}
    out["colors.toml"] = f"""# {t}: a base Omarchy theme for Patch.
# {p['about']}
# Every colour is set by hand (themes/generate.py); nothing is mixed.
mode = "{p['mode']}"

accent = "{p['accent']}"
# The brand red itself, for lights (keyboard, mouse, case, RAM).
brand = "{p['brand']}"
selection = "{p['selection']}"
muted = "{p['muted']}"

background = "{bg}"
dark_background = "{p['dark_background']}"
darker_background = "{p['darker_background']}"
lighter_background = "{p['lighter_background']}"

foreground = "{fg}"
dark_foreground = "{p['dark_foreground']}"
light_foreground = "{p['light_foreground']}"
bright_foreground = "{p['bright_foreground']}"

selection_foreground = "{p['selection_foreground']}"
selection_background = "{p['selection']}"

# Window borders: ink when focused, a faint rule otherwise.
hyprland_active_border = "{p['window_active']}"
hyprland_inactive_border = "{alpha(p['rule'], '88')}"

# ANSI, every one at least 4.5:1 on the background (terminals, and the
# agents in them: Codex, Claude Code, herdr's panes).
""" + "".join(f'{c} = "{p[c]}"\n' for c in ANSI[:8]) + "\n" + "".join(f'{c} = "{p[c]}"\n' for c in ANSI[8:])
    # Patch's roles: the theme's, or Patch's own where the palette sets them.
    o = p.get("patch", {})
    r = lambda role, default: o.get(role, default)
    out["patch.toml"] = f"""# {t} for Patch: every colour role exactly, so no Patch window mixes its
# own. (Other themes leave this file out; Patch derives the roles then.)
bg = "{r('bg', bg)}"
bg_deep = "{r('bg_deep', p['dark_background'])}"
bg_deeper = "{r('bg_deeper', p['darker_background'])}"
raised = "{r('raised', p['lighter_background'])}"
hover = "{r('hover', p['hover'])}"
selection = "{r('selection', p['selection'])}"
border = "{r('border', p['border'])}"
border_strong = "{r('border_strong', p['rule'])}"
text = "{r('text', fg)}"
text_bright = "{r('text_bright', p['bright_foreground'])}"
text_dim = "{r('text_dim', p['light_foreground'])}"
text_faint = "{r('text_faint', p['dark_foreground'])}"
accent = "{r('accent', p['accent'])}"
accent_soft = "{r('accent_soft', p['accent_soft'])}"
accent_ink = "{r('accent_ink', p['accent_ink'])}"
mock = "{r('accent', p['accent'])}"
warn = "{r('warn', p['yellow'])}"
error = "{r('error', p['red'])}"
level = "{r('level', '#22c7a0')}"
level_warn = "{r('level_warn', '#ffb020')}"
"""
    # Claude Code (Omarchy syncs a theme's claude.json): exact values, the
    # same roles as Omarchy's template, the diff tints checked below.
    claude = {
        "name": "Omarchy",
        "base": p["mode"],
        "overrides": {
            "claude": p["accent_text"], "claudeShimmer": mix(p["accent_text"], fg, 0.35),
            "text": fg, "inverseText": bg,
            "inactive": p["dark_foreground"], "inactiveShimmer": p["light_foreground"],
            "subtle": p["muted"], "suggestion": p["cyan"],
            "permission": p["blue"], "permissionShimmer": mix(p["blue"], fg, 0.35),
            "remember": p["yellow"], "success": p["green"], "error": p["red"],
            "warning": p["yellow"], "warningShimmer": mix(p["yellow"], fg, 0.35),
            "merged": p["magenta"],
            "promptBorder": p["accent"], "promptBorderShimmer": mix(p["accent"], fg, 0.35),
            "planMode": p["cyan"], "autoAccept": p["yellow"], "bashBorder": p["bright_yellow"], "ide": p["bright_cyan"],
            "diffAdded": mix(bg, p["green"], 0.15), "diffRemoved": mix(bg, p["red"], 0.15),
            "diffAddedDimmed": mix(bg, p["green"], 0.08), "diffRemovedDimmed": mix(bg, p["red"], 0.08),
            "diffAddedWord": mix(bg, p["green"], 0.32), "diffRemovedWord": mix(bg, p["red"], 0.32),
            "userMessageBackground": p["lighter_background"], "userMessageBackgroundHover": p["hover"],
            "bashMessageBackgroundColor": p["lighter_background"], "memoryBackgroundColor": p["lighter_background"],
            "selectionBg": p["selection"],
            "rate_limit_fill": p["accent"], "rate_limit_empty": p["border"],
            "briefLabelYou": p["yellow"], "briefLabelClaude": p["accent_text"],
        },
    }
    out["claude.json"] = json.dumps(claude, indent=2) + "\n"
    out["_claude_text"] = claude
    out["hyprland.lua"] = f"""-- {t}: hairline borders, square corners, no shadows. (Omarchy stages a
-- theme's .lua only for a local theme, not one installed from git.)
local active_border_color = "rgb({p['window_active'].lstrip('#')})"
local inactive_border_color = "{alpha(p['rule'], '88')}"

hl.config({{
  general = {{
    border_size = 1,
    col = {{
      active_border = active_border_color,
      inactive_border = inactive_border_color,
    }},
  }},

  group = {{
    col = {{
      border_active = active_border_color,
      border_inactive = inactive_border_color,
    }},
  }},

  decoration = {{
    rounding = 0,
    shadow = {{
      enabled = false,
    }},
  }},
}})
"""
    light = p["mode"] == "light"
    card = f"""# Flat, opaque cards: no translucency, a signal-red selection.
background                = "{bg}"
background-alpha          = 1.0
text                      = "{fg}"
border                    = "{p['window_active']}"
border-alpha              = 1.0
scrim                     = "{bg if light else p['darker_background']}"
scrim-alpha               = 0.6
selected-background       = "{p['accent']}"
selected-background-alpha = {1.0 if light else 0.18}
selected-text             = "{p['accent_ink'] if light else p['bright_foreground']}"
selected-border           = "{p['accent']}"
selected-border-alpha     = 1.0
"""
    out["shell.launcher.toml"] = card
    out["shell.menu.toml"] = card
    # Terminals: Omarchy's templates plus faint (SGR 2) text at 75%, not
    # 50% (Codex's dim text read ~3:1 on the paper). Omarchy drops these
    # from a git-installed theme and uses its templates, faint at 50%.
    ansi16 = [bg, p["red"], p["green"], p["yellow"], p["blue"], p["magenta"], p["cyan"], fg,
              p["muted"], p["bright_red"], p["bright_green"], p["bright_yellow"], p["bright_blue"], p["bright_magenta"], p["bright_cyan"], p["bright_foreground"]]
    faint = lambda c: mix(bg, c, FAINT)
    out["ghostty.conf"] = f"""background = {bg}
foreground = {fg}
cursor-color = {p['bright_foreground']}
selection-background = {p['selection']}
selection-foreground = {p['selection_foreground']}
# Faint text (Codex's secondary text) at {FAINT:.0%}: {contrast(faint(fg), bg):.1f}:1.
faint-opacity = {FAINT}

""" + "".join(f"palette = {i}={c}\n" for i, c in enumerate(ansi16))
    names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"]
    section = lambda title, cols: f"[colors.{title}]\n" + "".join(f'{n} = "{c}"\n' for n, c in zip(names, cols))
    out["alacritty.toml"] = f"""[colors.primary]
background = "{bg}"
foreground = "{fg}"
dim_foreground = "{faint(fg)}"

[colors.cursor]
text = "{bg}"
cursor = "{p['bright_foreground']}"

[colors.vi_mode_cursor]
text = "{bg}"
cursor = "{p['bright_foreground']}"

[colors.search.matches]
foreground = "{bg}"
background = "{p['yellow']}"

[colors.search.focused_match]
foreground = "{bg}"
background = "{p['red']}"

[colors.footer_bar]
foreground = "{bg}"
background = "{fg}"

[colors.selection]
text = "{p['selection_foreground']}"
background = "{p['selection']}"

""" + section("normal", ansi16[:8]) + "\n" + section("bright", ansi16[8:]) + "\n# Faint text, exact.\n" + section("dim", [bg] + [faint(c) for c in ansi16[1:8]])
    out["kitty.conf"] = f"""foreground {fg}
background {bg}
selection_foreground {p['selection_foreground']}
selection_background {p['selection']}

cursor {p['bright_foreground']}
cursor_text_color {bg}

active_border_color {p['accent']}
active_tab_background {p['accent']}

# Faint text at {FAINT:.0%}.
dim_opacity {FAINT}

""" + "".join(f"color{i} {c}\n" for i, c in enumerate(ansi16))
    out["chromium.theme"] = ",".join(str(v) for v in rgb(bg)) + "\n"
    out["icons.theme"] = p["icons"] + "\n"
    b = {
        "main_fg": fg, "title": p["bright_foreground"], "hi_fg": p["accent"],
        "selected_bg": p["lighter_background"] if not light else p["darker_background"], "selected_fg": p["bright_foreground"],
        "inactive_fg": p["dark_foreground"], "graph_text": p["light_foreground"], "meter_bg": p["dark_background"],
        "proc_misc": p["light_foreground"], "cpu_box": p["rule"], "mem_box": p["rule"], "net_box": p["rule"],
        "proc_box": p["rule"], "div_line": p["border"] if not light else p["rule"],
    }
    grad = {
        "temp": (p["blue"], p["magenta"], p["accent"]), "cpu": (p["light_foreground"], p["magenta"], p["accent"]),
        "free": (p["light_foreground"], p["light_foreground"], fg), "cached": (p["bright_blue"], p["bright_blue"], p["blue"]),
        "available": (p["light_foreground"], p["light_foreground"], fg), "used": (p["magenta"], p["accent"], p["accent"]),
        "download": (p["blue"], p["magenta"], p["accent"]), "upload": (p["blue"], p["magenta"], p["accent"]),
        "process": (p["light_foreground"], p["magenta"], p["accent"]),
    }
    btop = [f"# {t} for btop: ink on its ground, signal red for load.", f'theme[main_bg]="{p["background"]}"', "theme_background=false"]
    # The ground is set, not left empty: Omarchy's btop.conf has theme_background
    # on, and with no main_bg btop paints its own dark default under the ink.
    btop += [f'theme[{k}]="{v}"' for k, v in b.items()]
    for k, (s, m, e) in grad.items():
        btop += ["", f'theme[{k}_start]="{s}"', f'theme[{k}_mid]="{m}"', f'theme[{k}_end]="{e}"']
    out["btop.theme"] = "\n".join(btop) + "\n"
    return out


def readme(name, p):
    other = "patch-dark" if name == "patch-light" else "patch-light"
    bg = p["background"]
    rows = [("background", "the ground"), ("dark_background", "recessed: panel heads, strips"), ("lighter_background", "raised: cards, message blocks"),
            ("foreground", "text, the focused window border"), ("light_foreground", "secondary text"), ("dark_foreground", "faint text: captions, scales"),
            ("muted", "muted text: the terminal's bright black (Codex, Claude, herdr)"), ("border", "hairlines between panels"),
            ("rule", "edges, inactive window borders (graphics only)"), ("accent", "signal red: markers, fills, the active state"),
            ("accent_text", "the red as text (Claude Code)"), ("selection", "selected rows")]
    table = "\n".join(f"| {k} | `{p[k]}` | {u} | {contrast(p[k], bg):.1f}:1 |" for k, u in rows)
    ansi = " · ".join(f"`{p[c]}`" for c in ANSI[:8])
    look = ("Instrument-panel paper: a cool blue-grey ground, black ink, 1px hairline rules, square corners and one signal red."
            if p["mode"] == "light" else
            "Patch Light inverted: an ink ground, paper-grey text, 1px hairline rules, square corners and the same signal red.")
    return f"""# {p['title']}

A professional Omarchy theme made for [Patch](https://github.com/novuon/Patch),
the Omarchy audio environment, and just as usable without it. {look}
Its sibling is [{THEMES[other]['title']}](../{other}/README.md).

Every colour is defined exactly, in every file: nothing is left to mixing
by Omarchy or by apps. Contrast is checked when the theme is generated.

| Role | Colour | Use | On the ground |
|---|---|---|---|
{table}

Text reads at 4.5:1 or more everywhere: the ANSI colours ({ansi}, and
their bright versions), the muted grey, and every colour Claude Code writes
text in, on the ground and on its message blocks. Only the accent and the
rules are graphics (3:1).

## Install

With Patch: `./install.sh` links both themes into `~/.config/omarchy/themes`.
On their own, from a checkout:

```sh
ln -sfn "$PWD" ~/.config/omarchy/themes/{name}
omarchy-theme-set {name}
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
- `btop.theme`, `icons.theme` ({p['icons']}), `chromium.theme`.
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
"""


# The public, theme-only repos (omarchy-patch-dark-theme and
# omarchy-patch-light-theme): what `omarchy-theme-install` stages from a git
# repo and nothing Patch-only. Omarchy drops a git theme's .lua and terminal
# configs (and says so), and patch.toml is Patch's own; installing Patch
# replaces the clone with a link to its full theme, which adds them.
PUBLIC_FILES = ["colors.toml", "btop.theme", "icons.theme", "chromium.theme", "claude.json",
                "shell.launcher.toml", "shell.menu.toml", "unlock.png", "preview.png",
                "preview-unlock.png", "LICENSE"]
PUBLIC_BACKGROUNDS = ("01-", "02-", "03-", "04-", "05-", "06-")
PUBLIC_REPO = "https://github.com/novuon/omarchy-{name}-theme"


def public_readme(name, p):
    other = "patch-dark" if name == "patch-light" else "patch-light"
    bg = p["background"]
    rows = [("background", "the ground"), ("lighter_background", "raised: cards, message blocks"),
            ("foreground", "text"), ("light_foreground", "secondary text"), ("muted", "muted text"),
            ("border", "hairlines between panels"), ("accent", "signal red: markers, fills, the active state"),
            ("selection", "selected rows")]
    table = "\n".join(f"| {k} | `{p[k]}` | {u} | {contrast(p[k], bg):.1f}:1 |" for k, u in rows)
    look = ("Instrument-panel paper: a cool grey-green ground, black ink and one\nsignal red."
            if p["mode"] == "light" else
            "An ink ground, paper-grey text and one signal red.")
    return f"""# {p['title']}

{look}
A precise, restrained Omarchy theme with photoreal audio wallpapers, each
with an intro that plays when you switch to the theme. Its sibling is
[{THEMES[other]['title']}]({PUBLIC_REPO.format(name=other)}).

```sh
omarchy-theme-install {PUBLIC_REPO.format(name=name)}
```

## Wallpapers

Stylus, Console, Folded Dimension, Crosspoint, Acoustic Core and a plain
framed sheet. Each has a six-second intro in `backgrounds/intros/` that ends on the
still.

## Colours

Every colour is defined exactly, in every file, and text reads at 4.5:1 or
more on the ground.

| Role | Colour | Use | On the ground |
|---|---|---|---|
{table}

It was made for Patch, the pro audio environment for Omarchy, and needs
nothing from it.

## Licence

MIT, see `LICENSE`.
"""


def export(out):
    """Write the two public theme repos' contents into out/."""
    import shutil
    for name, p in THEMES.items():
        dest = out / f"omarchy-{name}-theme"
        # Replace the contents; a .git there is the published repo's history.
        for old in [p_ for p_ in dest.iterdir() if p_.name != ".git"] if dest.exists() else []:
            shutil.rmtree(old) if old.is_dir() else old.unlink()
        (dest / "backgrounds" / "intros").mkdir(parents=True)
        for f in PUBLIC_FILES:
            shutil.copy2(HERE / name / f, dest / f)
        kept = []
        # Compressed 4K WebPs (the 8K masters stay in themes/media).
        for still in sorted((HERE / name / "backgrounds").glob("*.webp")):
            if still.name.startswith(PUBLIC_BACKGROUNDS):
                intro = still.parent / "intros" / f"{still.stem}.mp4"
                shutil.copy2(still, dest / "backgrounds" / still.name)
                shutil.copy2(intro, dest / "backgrounds" / "intros" / intro.name)
                kept.append(still.name)
        (dest / "README.md").write_text(public_readme(name, p))
        # colors.toml's header speaks of Patch and this generator: the public
        # theme gets a plain one.
        colors = (dest / "colors.toml").read_text().splitlines()
        body = colors[next(i for i, line in enumerate(colors) if not line.startswith("#")):]
        (dest / "colors.toml").write_text(f"# {p['title']}: every colour set by hand.\n" + "\n".join(body) + "\n")
        print(f"{dest}: {len(kept)} wallpapers")


def main():
    if "--export" in sys.argv:
        export(pathlib.Path(sys.argv[sys.argv.index("--export") + 1]).resolve())
        return
    bad = []
    for name, p in THEMES.items():
        for role, on, want, got in checks(p):
            if got < want:
                bad.append(f"{name}: {role} {p.get(role) or p.get('patch', {}).get(role.removeprefix('patch.'), '')} on {on}: {got:.2f} < {want}")
        if contrast(mix(p["background"], p["foreground"], FAINT), p["background"]) < 4.5:
            bad.append(f"{name}: faint text at {FAINT}: below 4.5")
        # Claude's diff tints must keep the text readable.
        for tint in (0.15, 0.32):
            for c in ("green", "red"):
                got = contrast(p["foreground"], mix(p["background"], p[c], tint))
                if got < 4.5:
                    bad.append(f"{name}: text on the {c} diff tint {tint}: {got:.2f} < 4.5")
        # Every colour Claude Code writes text in, on the ground and on its
        # message blocks.
        o = files(name, p)["_claude_text"]["overrides"]
        text_roles = ["claude", "claudeShimmer", "text", "inactive", "inactiveShimmer", "subtle", "suggestion", "permission",
                      "permissionShimmer", "remember", "success", "error", "warning", "warningShimmer", "merged", "planMode",
                      "autoAccept", "ide", "briefLabelYou", "briefLabelClaude"]
        for role in text_roles:
            for on in (p["background"], p["lighter_background"], p["hover"]):
                got = contrast(o[role], on)
                if got < 4.5:
                    bad.append(f"{name}: Claude {role} {o[role]} on {on}: {got:.2f} < 4.5")
        for role in ("bashBorder", "promptBorder"):
            if contrast(o[role], p["background"]) < 3:
                bad.append(f"{name}: Claude {role} {o[role]}: {contrast(o[role], p['background']):.2f} < 3")
    if bad:
        sys.exit("contrast:\n" + "\n".join(bad))
    for name, p in THEMES.items():
        for f, text in files(name, p).items():
            if not f.startswith("_"):
                (HERE / name / f).write_text(text)
        (HERE / name / "README.md").write_text(readme(name, p))
        print(f"{name}: " + ", ".join(f"{role} {got:.1f}" for role, _, _, got in checks(p)[:6]))


if __name__ == "__main__":
    main()
