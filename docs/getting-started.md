# Getting started with Patch

Patch is the Omarchy audio environment: routing, mixing, metering,
analysis, recording and DJing in one application. Each Unit (Mixer,
Router, Meter, Analyzer, Recorder, DJ Booth, …) opens as its own Wayland
window, and Hyprland tiles them. The audio runs in the **Patch service**,
a separate process. Windows can close and Patch can restart or update
while the music keeps playing.

## 1. Install

On Omarchy (or any Arch system):

```sh
git clone https://github.com/novuon/Patch.git
cd Patch
./install.sh
```

It asks how to install Patch: **Minimal** (the core: Patch replaces
Omarchy's audio), **Full** (the core and every module) or **Custom** (the
core and the modules you pick: DJ, Auto Mix & stems, Visualiser, Podcast &
broadcast, Live coding, Plugins, Controllers, the themes), then your
default layout. It installs the packages Patch needs (PipeWire and its
tools, FFmpeg, Rubber Band, SQLite, fontconfig, libnotify; Rust only when
it compiles), takes the ready-made binary of a release kit (or builds
Patch), installs `omarchy-patch` into `~/.local/bin` with an app per
layout, starts the Patch service with your session and, on Omarchy,
adds Patch to the bar's audio menu. It ends with a short guide
(`omarchy-patch welcome`). Options: `--profile full|minimal`,
`--modules dj,ai_dj,…`, `--layout <id>`, `--no-stems`, `--no-deps`,
`--no-service`, `--no-bar`, and `PREFIX=/usr/local ./install.sh` for
another place.
`./uninstall.sh` removes it again (your library and settings stay;
`--purge` removes those too).

As an Arch package instead: `packaging/arch/PKGBUILD` (`makepkg -si` in
that folder), then `systemctl --user enable --now omarchy-patch`.

From the Omarchy menu (Super+Space): "Patch" opens the controller; an app
per layout your modules allow opens it: "Patch Audio" (Simple), "Patch
DJ" (Basic DJ), "Patch Auto Mix", "Patch Performance DJ", "Patch Podcast",
"Patch Live Code" and "Patch Pro" (every Unit you installed). Nothing
opens by itself at login: only the audio service starts.

The Patch service is part of PipeWire, like WirePlumber: it starts when
PipeWire starts (at login), headless and minimal (no windows, no DJ
Booth devices until a deck needs them) and restarts with PipeWire.
Real-time needs rtkit or the realtime group (docs/performance.md).
After an update, the next start of Patch hands the running engine over
to the new version without a gap (or run `omarchy-patch engine upgrade`).

For development, build in place and link the binary instead:

```sh
cargo build --release
ln -sf "$PWD/target/release/omarchy-patch" ~/.local/bin/omarchy-patch
```

## 2. Open it

| What | How |
|---|---|
| The controller: every Unit, layout and the engine state | `omarchy-patch` (or **Ctrl+K** in any Patch window) |
| Your default layout (chosen at install) | `omarchy-patch layout` |
| Simple: the Mixer, the Router and the Player | `omarchy-patch layout simple` |
| Basic DJ: decks, crossfader, library | `omarchy-patch layout basic-dj` |
| Auto Mix: the DJ Booth with Auto Mix, library, meter | `omarchy-patch layout ai-dj` |
| Performance DJ: booth, control, Visualiser (three workspaces) | `omarchy-patch layout performance-dj` |
| DJ Studio: booth, mixing desk, library (three workspaces) | `omarchy-patch layout dj-studio` |
| Podcast & Broadcast | `omarchy-patch layout podcast` |
| Pro: mixing and engineering (two workspaces) | `omarchy-patch layout pro` |
| Patch in the bar's audio dropdown | `omarchy-patch bar install` |
| A dot in the bar for Patch's buffer health | `omarchy-patch bar health on` |
| Your music: sources (folders, USB sticks) and what's analysed | `omarchy-patch library` ([library.md](library.md)) |
| Performance: what Patch costs next to PipeWire | **SUPER+ALT+P** or `omarchy-patch perf` ([performance.md](performance.md)) |
| One Unit | `omarchy-patch open mixer` (or its key in the controller) |

From the project checkout, use the same commands through Cargo while
developing (omit `--release` for a faster debug build):

```sh
cargo run --release -p patch --                 # controller
cargo run --release -p patch -- layout simple
cargo run --release -p patch -- layout live-dj
cargo run --release -p patch -- layout dev
cargo run --release -p patch -- open mixer
```

Layouts open on the **next empty workspace of the focused monitor**, never
beside your work. Omarchy 4.5 gives each monitor its own workspaces 1–10;
a layout stays inside that block.
What each one holds, the bar's audio menu, and what happens when a window
is too small for its Unit: [layouts.md](layouts.md).

Player is available in the Audio group for files and streams; see
[Recorder and Player](recording-playback.md) for transport and recording controls.
Broadcast is available in the DJ Booth group: it sends Patch's audio to an
Icecast/HTTP server, an RTMP endpoint or a file; see
[Broadcast](broadcast.md). Network (key Z) sends Patch's audio to other
machines on the local network and plays theirs as Mixer channels; see
[Network](network.md).

For a persistent install, `omarchy-patch hypr-rules` prints Hyprland
window rules to add to your config.

## 3. Moving around

A window's own keys come first; a letter it does not use opens that Unit.
The same in the controller (Ctrl+K), whose cursor moves with the arrows.

<!-- keys:begin (generated from crates/patch-core/src/keys.rs) -->
**Everywhere** (keys a window does not use itself)

| Key | Does |
|---|---|
| a–z | open or focus that Unit's window |
| ⇧ + letter | another window of that Unit |
| tab ⇧tab | next / previous open Patch window |
| ^k | controller (open · close) |
| ^= ^- ^0 | zoom in · out · 100 % |
| ? | help for this window |
| esc | close the help · the controller |

**Unit letters**

| Key | Opens |
|---|---|
| a | Deck A |
| b | Deck B |
| c | Broadcast |
| d | Device |
| e | Inspector |
| f | Tidal |
| g | Router |
| h | MIDI & HID |
| i | Mic |
| j | DJ Booth |
| k | EQ |
| l | Library |
| m | Mixer |
| n | Analyzer |
| o | Performance |
| p | Session |
| q | Plugins |
| r | Recorder |
| s | Sequencer |
| t | Pattern |
| u | Tune |
| v | Meter |
| w | Visualiser |
| x | Crossfader |
| y | Player |
| z | Network |
<!-- keys:end -->

The footer of every window lists that window's keys, and **?** shows them
all; the [user manual](user-manual.md#14-keyboard-reference) lists every
window's keys. These are keys the
Patch window receives. They do not use Super or Right Alt, so they do not
take Omarchy 4.5's shortcuts: Super+Q closes a window, Super+A selects
all, Super+M minimizes, Super+` toggles the Quake console, Super+Alt+L
cycles the workspace layout backwards, Super+Shift+Ctrl+arrows moves the
window to another monitor, and holding Right Alt dictates. Patch's global
shortcut is still **Super+Alt+P** (Performance).

## 4. Play something through Patch

```sh
omarchy-patch route cliamp      # cliamp now plays through the Patch bus
omarchy-patch send master bose  # move everything to another output, live
omarchy-patch restore           # undo all Patch routing
```

The Meter shows what you hear, when you hear it: meters are delayed by the
output's latency (Bluetooth included). To measure that latency instead of
trusting the driver, hold your headphones near a microphone and run
`omarchy-patch latency measure` (or press `l` in the Analyzer). The music
pauses for about 5 s while test sweeps play.

## 5. DJ

**Library** (`l`): `/` filters, `↑↓` moves, `a`/`b` load a track on a deck,
`q` queues it. Tracks under `~/Music` are found and analysed in the
background (tempo, grid, key, loudness, energy).

**Decks** (`a`, `b`): `space` play, `c` cue (hold it on the cue point to
preview), `1–4` hot cues, `←→` jump, `o` loop, `↑↓` tempo, `s` sync,
`k` keylock, `[ ]` key, hold `, .` to nudge.

**Crossfader** (`x`): EQ and filter per deck, headphone cue, Beat FX (Echo,
Reverb, Flanger, Trans, Crush, tempo-locked; `f` toggles), crossfader.

**Light DJ** (`j`): the everyday view of the same DJ session: now playing,
next, the queue, auto-mix. `space` play (starts the queue when nothing is
loaded), `n` next (mix at the next bar), `m` auto-mix, `q` queue the
Library's selection.

### Auto Mix: suggestion, preparation, or full automatic mixing

The Library's Auto Mix card and Light DJ's **Auto-mix** are two different ways
to use the same deterministic, analysis based mix planner; it does not need
a network model:

| Library Auto Mix control | What it does |
|---|---|
| **Build / Hold / Release** | Changes the preferred energy of the next suggestion: rise, stay similar, or ease down. It does not set BPM. |
| **Suggest / Suggest again** | Selects and explains a suitable next track. It does not load or play it. The alternate buttons choose a runner-up. |
| **Prepare** | Loads the suggested track on the idle deck, cues it at its planned intro point, and enables beat sync. It stays paused; no transition starts. |
| **Mix** | Schedules the suggested transition at its planned phrase. This is the manual “go” button. |
| **Auto prep** | Automatically prepares the idle deck about 45 seconds before mix-out. You still press **Mix** to start the transition. |
| **Off** | Turns off this suggestion/preparation assistant. It does not turn off Light DJ Auto-mix. |

For **hands-free mixing**, use Light DJ (`omarchy-patch open light-dj`) and
turn on **Auto-mix**. This is the control that chooses, loads, cues, syncs,
and starts transitions without waiting for a Mix click. `Build`, `Hold`, and
`Release` there steer track energy when no set arc is running. Starting a set
also enables Auto-mix. If a deck is already playing, the set continues from
that track; if both decks are stopped, it chooses and starts an opener.

The Library remains a catalogue, so its rows do not change as Auto Mix picks
tracks. When Auto-mix is on, the **Auto-mix · Service choice** box above the
catalogue shows the service's actual next selection and transition countdown.
The A/B line beside it shows what is really loaded on each deck and whether
it is playing or cued. Light DJ shows the same next selection and plan; the
set log records tracks once they begin playing.

### The Auto Mix Unit

`omarchy-patch layout ai-dj` shows which tracks Auto Mix considers next, and why: candidates
and why, how sure it is, the planned transition. It also sets how much it
may do: `1` OBSERVE, `2` SUGGEST, `3` PREPARE (it waits for your
**enter**), `4` ACT (Auto-mix). See [ai-dj.md](ai-dj.md).

### What the mixer uses

Auto-mix takes the head of your queue first. With an empty queue, it chooses
from the analysed library, avoiding tracks played earlier in the session.
It needs a confident tempo analysis and a sufficiently long track. The
selector scores tempo fit, Camelot key compatibility, rating, and energy
direction. Candidates within 8% tempo reach (including half/double-time
matches) can blend; a farther-tempo candidate is cut-only and receives no
tempo-fit score, so other fit factors can still affect its ranking.

The engine loads and cues the incoming deck, then syncs its beat grid to the
playing deck. During an overlap, the incoming deck follows the outgoing
deck's effective tempo and its beat phase is corrected. The sync button is
shown on the follower; the master deck's button is off. Once a transition
finishes, Patch clears the old follower lock so the new lead deck is not
slaved to a paused deck. Therefore a dark Sync button on the only playing
deck is expected; during the actual overlap, the incoming deck should show
Sync enabled.

Auto-mix yields to manual control: touching a deck or mixer control while a
transition is scheduled cancels that plan and turns Auto-mix off. Turn
Auto-mix on again when you want it to resume choosing.

Mix length and the incoming cue point vary by track pairing and intro
analysis. The planner picks phrase-aligned lengths and intro cue bars, then
uses the detected outro to schedule the transition. Build/Hold/Release guide
energy selection, not beat matching. When a set is active, its **journey**
arc moves through warm-up, build, peak, and release; **warmup** stays relaxed
most of the set, and **peak** holds energy before a release. “Tell the DJ…”
can steer the next two picks (`keep the energy up`, `more relaxed`), constrain
the desired range (`stay around 126`), request variety (`surprise me`), or
ask for a similar track.

No mode can promise 100% correct musical beat grids: automatic sync can lock
perfectly to an analysis that misread a track's tempo or downbeat. For the
most dependable results, scan and analyse the tracks first, use files whose
tempo grids you have checked, and avoid queuing unverified tracks. Auto-mix
only picks tracks with BPM confidence of at least 0.5; candidates beyond 8%
tempo reach have no tempo-fit score and use a cut instead of a blend. The
incoming deck's Sync should be on while it is being mixed in. The sync
reading measures Patch's deck playheads; it cannot certify that every
track's detected grid matches the audible drums.

```sh
omarchy-patch open light-dj
# Full automatic 60-minute set; uses the current track or starts an opener:
omarchy-patch dj set start 60 journey
# Alternative: auto-mix from the current playing track without a set arc:
omarchy-patch dj automix on hold
# Optional steering and library maintenance:
omarchy-patch dj say stay around 126
omarchy-patch dj queue add "$HOME/Music/track.mp3"
omarchy-patch library scan
omarchy-patch analyze "$HOME/Music/track.mp3"
```

`dj automix on` starts autonomous transitions but does not start a stopped
deck; press Play or start a set. `dj set stop` stops future choices and leaves
the current track playing. Auto-mix runs in the Patch service, so it continues
if the windows close.

The DJ session lives in the Patch service, so it keeps playing with every
window closed. Opening the Booth or Light DJ shows exactly where it is.

## 6. The engine and your machine

```sh
omarchy-patch engine status      # rate, quantum, load, kernels in use, …
omarchy-patch engine upgrade     # swap to a new build without a gap
omarchy-patch optimize           # measure DSP kernels on this CPU, keep the best verified
omarchy-patch service install    # start the Patch service with your session (optional)
```

For the steadiest audio, give PipeWire real-time priority (for example,
install `realtime-privileges` and add yourself to the `realtime` group).
`engine status` shows `"realtime": false` when it doesn't have it.

## Where things are kept

| What | Where |
|---|---|
| DJ session (mixer, EQ, queue, auto-mix) | `~/.local/state/omarchy-patch/dj-session.json` |
| Analysis cache | `~/.cache/omarchy-patch/analysis/` |
| Library | `~/.local/share/omarchy-patch/library.patchdj` (Patch DJ format, see `docs/patch-dj-format/`) |
| Measured latency, kernel profile, keymap | `~/.config/omarchy-patch/` (`latency.json`, `kernels.json`, `dj-keys.json`) |
| Recordings | `~/Music/Patch Recordings` |

## More

* `omarchy-patch help`: every command.
* `docs/architecture/`: how Patch is built (service, one DJ engine,
  timing, the kernel ABI).
* `docs/dj-booth.md`: the DJ Booth in depth. `docs/bench.md`: measurements.
* `CHANGELOG.md`: what changed in each version.

## Unified DJ workspace

Open `omarchy-patch open dj` for Play, Prepare, Recordings and Reports in one window. Start with Suggest mode, save a future set, then start it explicitly. See [DJ workspace](dj-workspace.md) for the complete workflow and report-driven tuning review.

Hardware MIDI/HID input runs in the engine even with its windows closed.
See [Controller Unit](controllers.md) for per-device profiles, monitoring
and `omarchy-patch controllers status|reload`.
