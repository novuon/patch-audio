# Layouts

A layout is a set of Unit windows that Patch tiles for one job. Each opens
on the **next empty workspace of the focused monitor**, never beside your
work. Omarchy 4.5 gives each monitor its own workspaces 1–10 (Hyprland
still numbers them in one sequence, in blocks of ten); a layout stays
inside the focused monitor's block and does not walk onto the next
monitor's numbers. Windows you already have elsewhere stay where they are.
The music doesn't stop when you switch layouts: the Patch service plays
with every window closed.

```sh
omarchy-patch layout                  # your default (chosen at install)
omarchy-patch layout simple           # Patch as your audio system
omarchy-patch layout basic-dj         # two decks, the crossfader, the library
omarchy-patch layout ai-dj            # Auto Mix plays, you steer
omarchy-patch layout performance-dj   # you play: booth, control, Visualiser (three workspaces)
omarchy-patch layout dj-studio        # booth, mixing desk, library (three workspaces)
omarchy-patch layout podcast          # mic, record, broadcast
omarchy-patch layout live-code        # Tidal, and the Visualiser on a second workspace
omarchy-patch layout pro              # mixing and engineering (two workspaces)
omarchy-patch layout all-units        # development: every Unit you installed (slow)
```

The controller (`omarchy-patch`, or **Ctrl+K** in any Patch window), the
bar and the app launcher list the layouts your modules allow (a layout
whose Units are switched off isn't offered; `omarchy-patch layout` says
which module it needs). Choosing a layout in the bar first closes every
Patch window, including windows left by an older unresponsive UI session,
then opens the new layout. The audio engine keeps playing. Nothing opens
by itself: at login only the audio service starts.

| Layout | App | Needs | Workspaces | Windows |
|---|---|---|---|---|
| **Simple** | Patch Audio | the core | 1 | Mixer across the top · Router, Player |
| **Basic DJ** | Patch DJ | DJ | 1 | Deck A, Crossfader, Deck B · Library |
| **Auto Mix** | Patch Auto Mix | DJ, Auto Mix | 1 | DJ Booth (large) · Library, Meter |
| **Performance DJ** | Patch Performance DJ | DJ, Auto Mix, Visualiser | 3 | 1 Deck A, Crossfader, Deck B · Library, DJ Booth<br>2 EQ, Inspector · Mixer, Meter<br>3 the Visualiser, for a screen or a projector |
| **DJ Studio** | Patch DJ Studio | DJ, Auto Mix, Visualiser | 3 | 1 Deck A, Crossfader, Deck B · Visualiser, DJ Booth, Meter<br>2 Router, Inspector, EQ · Mixer<br>3 Library, Diagnostics (every window at full size from 2400 px wide; on 1920 px the EQ and the Meter show their compact forms) |
| **Podcast & Broadcast** | Patch Podcast | Podcast | 1 | Mic, Mixer, Meter · Player, Recorder, Broadcast |
| **Live Code** | Patch Live Code | Live coding, Visualiser | 2 | 1 Tidal · Meter, Mixer<br>2 the Visualiser |
| **Pro** | Patch Pro | the core | 2 | 1 EQ, Meter · Mixer, Inspector<br>2 Router, Analyzer · Recorder, Plugins (the rack, with the Plugins module; without it the Recorder takes the row) |
| **All Units** (developer) | — | the core | as many as needed | every Unit you installed, packed so each gets its minimum size on the screen it opens on (on 1920×1080 a Full install takes five workspaces, a Minimal one three). Dozens of windows: slow to open, meant for development, not offered as a default |

The old names still work: `minimal`, `compact` and `studio` are Simple,
`live-dj`, `dj` and `dj-only` are Performance DJ, `recording` is Podcast,
`performance` is Pro, and `dev` and `all` are All Units.

**Developer mode** (Ctrl+K → Setup, or `omarchy-patch setup dev_mode on`):
every layout you open, your own included, gets a Diagnostics window on
each of its workspaces (engine timing, clock, latency, workers). It takes
a slice of the window with the most room to spare, so every window keeps
its minimum size where the screen allows.

## Your own layouts

Arrange a layout's windows as you like (move splits, swap windows, float
one), then save it:

```sh
omarchy-patch layout save Night set   # save what's on screen as "Night set"
omarchy-patch layout night-set        # open it again, exactly like that
omarchy-patch layout reset            # the layout on screen back as saved
omarchy-patch layout show             # go to its first workspace
omarchy-patch layout close            # close its windows (audio keeps playing)
omarchy-patch layout delete night-set
```

* A built-in layout is never changed: saving it asks for a name and makes
  your own copy. Saving your own layout again (without a name) writes over
  it.
* Floating windows are saved floating, at their place and size.
* Patch notices when the windows on screen no longer sit as the layout has
  them: the bar shows it as changed, and **Reset** puts them back.
* Saved layouts are JSON files in `~/.config/omarchy-patch/layouts/`.

While a layout opens, a cover hides the workspace switching and
splitting. For those seconds animations are off, and neither the mouse
nor focus follow each other (a moved mouse would otherwise make the next
window split the wrong one). All three come back afterwards; if Patch
stopped answering, a watchdog restores them and moves the cover away after
20 seconds. Omarchy can leave animations off already (its no-animations
toggle); Patch only restores a setting it changed. A theme switch still
reloads Hyprland, and Patch puts its focus border and window rules back
when that reload finishes.

Omarchy 4.5's **Super+L** cycles the focused workspace through tiling,
scrolling and floating (**Super+Alt+L** cycles back). A tiled layout puts
the workspaces it fills back on tiling, so its splits land, and remembers
that so a theme reload does not float them again. Windows the layout saved
as floating stay floating. The controller, the performance window and the
opening cover float on their own rules; those rules are registered with
Omarchy, so a floating workspace does not resize them into its cascade.

## The bar's audio menu

With Patch as your audio system, the audio menu in the Omarchy bar can be
Patch's:

```sh
omarchy-patch bar install     # Patch in the bar's audio dropdown
omarchy-patch bar uninstall   # the old one back
```

The dropdown stays what it was: volume, outputs, inputs and the per-app
mixer. Your own changes stay as well: Patch is built from the audio widget
your bar shows now, whether that is Omarchy's or your own clone. The
**Patch** section is added above the outputs. It grows with what runs, not
with options: with nothing running and no window open it is a search, four
picks and the EQ.

* **What runs**, one row each, only while it runs (Patch keeps all of it
  going with every window closed):

  | Row | Shows | Buttons | Its windows |
  |---|---|---|---|
  | **Broadcast** (red dot) | live, the time, where to | Stop (asks once) | Broadcast |
  | **Recording** (red dot) | the time, paused | Stop (asks once) | Recorder |
  | **Auto Mix** | the tracks playing, the set's minutes | Pause / Resume, End set (asks once) | Auto Mix, the decks it plays |
  | **Decks** (by hand) | the tracks playing | Pause, then Play | the decks playing, Crossfader |
  | **Player** | the title | Pause, then Play | Player |
  | **Tidal** | its tempo | Stop, then Play | Tidal |
  | **Sequencer** | its tempo | Stop, then Play | Sequencer |
  | **Pattern** | its layers | Stop, then Play | Pattern |

  Click a row for its first window; a row with more windows shows them as
  buttons below it (an Auto Mix set: **Auto Mix**, **Deck A**, **Deck B**). A
  window that is open already comes forward. Something the menu paused
  keeps its row, with Play, for an hour.
* **In the mix**: one line, only while something processes the sound with
  no window open: each plugin in the rack that is on (**Box Of Bass**,
  **Gluon**; one the plugin host held out after a crash says "held out")
  and a mic whose monitor is on. A click opens just that plugin's window,
  to change it, and nothing else. Past five plugins the last button opens
  the rack. Hover one for where it runs ("Gluon on Master").
* **Windows**: the Patch windows on screen on one line, with **Close all**
  (across every workspace; the audio keeps playing). Click the line for a
  button per window and the layout on screen: Show, Reset, Save, Save as…
  and Delete (your own layouts).
* **Open a Unit or layout…**: the dropdown opens with the keyboard in this
  search. Type part of a name or of what a Unit does (`rec`, `stream`,
  `deck`, `ai`), or a plugin in your rack, on or off (`graillon`, `vst`),
  and press Enter, or use the arrows. Empty, it offers four
  picks: what you opened lately that isn't on screen, then your default
  layout and the everyday Units. Down from the empty search (or Tab) hands
  the keyboard back to the dropdown. A layout first closes the Patch
  windows on screen, as before.
* **The master EQ**: step through the presets (‹ ›), bypass it, or click to
  open the EQ.

While Patch records or broadcasts, the audio icon in the bar has a small
red dot, so a take or a stream is never running out of sight. The recorder
and the stream each rewrite a small file (`recording.json`,
`broadcasting.json` in Patch's runtime folder) once a second; the bar reads
them, nothing is spawned.

Recent picks are what you open from the bar, the launchers and
`omarchy-patch open`/`layout`, kept in
`~/.local/state/omarchy-patch/recent`. The row buttons run
`omarchy-patch bar do …` (`set pause`, `set resume`, `set end`,
`decks pause`, `record stop`, …).

Patch follows the system output, so the dropdown's output picker chooses
where Patch plays. Patch's own devices (the decks, the program, app
channels) are left out of the output and input lists, and the dropdown may
grow taller than Omarchy's, so Input stays in view. The plugin is called `patch.audio` and is a clone of
`omarchy.audio`, so it keeps the built-in widget's media and OSD hooks.
Install saves `shell.json` once as `shell.json.before-patch`. Run
`bar install` again after you change your own audio widget.

### Buffer health

An optional dot left of the audio widget appears when Patch's audio doesn't
keep up, for a DJ set or a machine near its limit. While the buffers are
healthy, or the engine isn't running, it stays out of sight:

```sh
omarchy-patch bar health on    # the dot in the bar
omarchy-patch bar health off   # out again
omarchy-patch health           # what it shows, for a report
```

* **amber**: a skip in the last 5 minutes, or a cycle over 80 % of its time.
* **red**: a skip in the last 15 seconds.

`omarchy-patch health` reports green too, with how much of each cycle the
busiest one took.

A skip is a second in which the audio device ran dry (an xrun PipeWire
reports, or the graph woke over two cycles late, after its first 30 seconds),
Patch's processing missed
its cycle, or a playing deck ran out of audio. Hover the dot for which.
The engine writes `health.json` in its runtime folder once a second and the
dot reads it; nothing else runs. It is a plugin of its own, `patch.health`,
so the shell's own bar settings can move or remove it too.

## Room for every window

Every Unit has a minimum size, where all its controls fit. The layouts
give every window at least that on a 1920 × 1080 screen and on a 4K
screen at scale 1.6 (2400 × 1350), less Omarchy's bar (the tests check
this). A window can still end up smaller: on a smaller screen, when you
open more windows, or when you tile it by hand.

A window below its minimum shows a bar at the top: "EQ shows everything
from 860×460 · this window is 700×400". It offers two choices:

* **Make room**: the window floats, grows to its minimum (a little more)
  and is centred.
* **Keep small**: you accept that window small. Patch remembers this per
  Unit, in `~/.config/omarchy-patch/small-windows`, and doesn't ask again.
  Delete the Unit's line (its id, such as `eq`) to be asked again.

Patch never resizes a window without your say.

| Unit | Minimum (logical px) |
|---|---|
| Auto Mix | 760 × 440 |
| Analyzer | 720 × 400 |
| EQ | 860 × 460 |
| Deck A / B | 520 × 440 |
| Crossfader | 380 × 520 |
| Router | 640 × 420 |
| Mixer | 640 × 380 |
| Meter | 560 × 460 |
| Broadcast | 560 × 420 |
| Recorder | 560 × 380 |
| Library | 520 × 360 |
| Light DJ | 460 × 420 |
| Diagnostics | 480 × 360 |
| Tune | 480 × 360 |
| Device, Inspector, Session | 420 × 300 |
| MIDI & HID | 480 × 320 |
| Sequencer | 960 × 560 |
| Planned Units | 320 × 200 |

The Mixer console keeps its faders usable in compact windows by collapsing
the tone and send banks. Below 640 × 480 it shows the same size warning as
other Units; **Make room** grows it to the full-detail console size.
