# Performance Unit

The Performance Unit shows what Patch costs, next to what the native
Omarchy audio layer costs: PipeWire, its PulseAudio server and
WirePlumber. It tells you where Patch spends CPU, GPU and memory, so it can
be made faster, and whether it is heavier than going without it.

```sh
omarchy-patch toggle perf      # open or close it (SUPER+ALT+P; key o in Patch)
omarchy-patch perf [seconds]   # the same figures as text, averaged (default 5 s)
omarchy-patch perf 10 baseline # … and save them as the baseline
```

The window floats over whatever you are doing, 1280 × 820, centred. If Patch
isn't running, the keybind starts it with just this window. In the Dev
layout it has workspace 4 to itself.

The keybind lives in `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + ALT + P", "Patch performance", "omarchy-patch toggle perf")
```

## A fair comparison

The native layer moves the system's audio to the output. Patch is compared
doing that same job: its **service** (the engine and its graph nodes)
against pipewire, pipewire-pulse and wireplumber. Patch's windows are its
UI; they get their own tile, next to the bar (Quickshell), the native
side's UI.

The window says whether the moment is fair. It isn't while other Patch
windows are open (they ask the service for meters, decks and analysis),
a deck plays, or the Auto Mix mixes. The banner then names the reason.

```sh
omarchy-patch perf fair [seconds]   # the honest test (default 20 s)
```

It refuses while a deck plays or the Auto Mix mixes, and never stops your
music to get a number. It closes Patch's windows for the test (the audio
keeps playing), measures, and opens them again. It also reports the
polling that loads the audio server: new PulseAudio clients per second
(whole system), engine requests and processes the engine spawned.

Measured on 2026-09-28 (Intel Arrow Lake, 20 cores; nothing playing; 30 s;
both sides real-time through rtkit):

| | Patch service | Native layer |
|---|---|---|
| CPU | 1.1 % of a core | 1.1 % |
| memory | 26 MB | 60 MB |
| new xruns | 0 | 0 |
| graph work per cycle | 43 µs | 8 µs |
| worst engine callback | 51 µs of 21.3 ms | |

## Real-time

The engine asks for real-time scheduling itself (PipeWire's rt module, in
its own context). It needs one of these:

* **rtkit** (`pacman -S rtkit`): works at once, priority up to 20;
* the **realtime group** (`pacman -S realtime-privileges`, `usermod -aG
  realtime $USER`, then log in again): higher priority and locked memory.

It never goes through the desktop's realtime portal. That path granted a
real-time CPU limit of 0 µs, and the kernel killed the engine the moment
its audio thread ran real-time. The Performance window's engine line
shows `real-time RR 20` when it works.

Patch's graph work includes its own idle devices (Deck A/B, the program
source); 56 µs is 0.26 % of a 21.3 ms cycle.

The windows are measured apart (`PATCH_TRACE_FRAMES=1` prints the UI's
update cost per second, by part). With all eleven Live DJ windows open and
nothing playing, the UI uses about 6 % of a core.

## What it measures

Everything is measured, once a second, and only while the window is open
(the window itself costs about 1 % of a core).

| Figure | Source | Patch | Native |
|---|---|---|---|
| CPU (% of one core) | `/proc/<pid>/stat` | engine + UI | pipewire, pipewire-pulse, wireplumber |
| memory (resident) | `/proc/<pid>/stat` | engine + UI | the same three |
| GPU busy %, GPU memory | NVML (`nvidia-smi pmon`), else DRM fdinfo | the UI's windows | — |
| DSP per cycle | the PipeWire graph (`pw-top`): busy time per node | `patch.*` nodes | every other node, drivers included |
| xruns | the graph's error count per node | `patch.*` nodes | the rest |
| engine callback | the engine's own timing (avg, worst, % of the cycle) | | |
| UI worst frame | the UI's frame timer | | |

The bar (Quickshell) is listed for reference and counted on neither side.
The window's own cost counts toward the Patch UI.

**Reading it.** The DSP figure compares like with like: the time the graph
spends in Patch's nodes against the time it spends in everything else, per
cycle. The CPU figure also counts the UI and Patch's background threads.
Those are what to optimize first when Patch is heavier than native. A
warning colour marks a Patch process over 20 % of a core, graph work over a
quarter of the cycle, an engine that isn't real-time, and new xruns.

## Baseline

**Set baseline** averages ten seconds and saves them
(`~/.local/state/omarchy-patch/perf-baseline.json`). From then on every
tile shows the change against it, so an optimization shows as a number.
`omarchy-patch perf` prints the change too. **Copy report** copies the
text report (last ten seconds, and the baseline) to the clipboard.

## Smooth frames, and the renderer

Measured on the Live DJ layout (4K at 60 Hz, scale 1.6, RTX 5060 Ti) with a
set playing, one minute per run, runs alternated:

| | femtovg (default) | Skia (OpenGL) |
|---|---|---|
| UI, % of a core | 30.9 | 31.4 |
| animated windows, frames per display frame missed | 0.4–0.8 % | 0.1–0.4 % |
| drawing a frame (deck / crossfader / meter) | 1.4 / 0.6 / 0.8 ms | 1.6 / 0.6 / 0.8 ms |
| memory / GPU memory | 255 / 280 MB | 300 / 436 MB |

Both draw every display frame; the few "missed" are mostly frames landing
at a frame boundary (as many count double). Skia buys nothing measurable
and costs memory and a large build dependency, so Patch stays on femtovg.
A frame takes 1–2 ms of the 16.7: the renderer is not what limits
smoothness; the UI thread's own work is.

What did help:

- Windows out of sight (a hidden workspace draws nothing) update a few
  times a second instead of every frame, enough to draw at once when they
  come into view: UI work 128 → 70 ms a second.
- No blocking work in a frame: the EQ window's list of playing apps made
  the engine run `pactl` (7 ms) inside a UI frame every few seconds; it is
  now refreshed in the background.
- Updating from the display's frame callbacks instead of the 60 Hz timer
  measured worse (11 % of frames missed): each window already draws on its
  frame callback, so the timer's drift costs nothing.

Later, on the same layout (September 30):

- Windows on a hidden workspace draw nothing at all now: Patch knows from
  Hyprland's events which workspaces are on screen (before, a hidden
  window was still drawn a few times a second: a hidden 4K Visualiser
  cost ~40 ms a second).
- The decks' track overview is two images drawn once per track instead of
  600 rectangles a deck, each coloured by the playhead and all of them
  re-evaluated every frame: UI on the DJ workspace 39.8 → 27.5 % of a core.
- Layers (`cache-rendering-hint`) on the decks' static sections measured
  *worse* (27.2 against 26.0 %, three runs each): their text changes
  often (time, bar), so the layers are redrawn and then blended too.
- Meters come from shared memory (`meters.shm`): no request per frame.
- The Visualiser, when on screen at 4K, was the largest single cost:
  about 7 ms a frame (435 ms a second), every pixel drawn on the CPU and
  uploaded as a 12 MB image. It now draws on the GPU the way a terminal
  draws the Omarchy screensaver: the CPU decides each cell's glyph and
  colours, a shader draws them from a glyph atlas with the glow and the
  colour split (`visual_gl.rs`, beneath Slint's items, through the
  window's one rendering notifier). UI with it on screen: 43.5 → 11.5 %
  of a core; the Omarchy screensaver (ttfx 0.5 in Ghostty) measured ~19 %
  (terminal +17 %, compositor +1.5 %, ttfx 0.5 %). Without OpenGL
  (headless shots, the software renderer) it still draws on the CPU.

Tools: `PATCH_RENDERER=femtovg|software` (a machine's own choice, for
every window however it starts: `~/.config/omarchy-patch/renderer`;
`software` where the GPU driver draws nothing, such as an M3 on Asahi's
experimental G15 driver), `PATCH_TRACE_RENDER=<file>` (per
window every 10 s: frame rate, draw time, gaps between frames),
`PATCH_TRACE_LOOP=1` (every update and draw, timestamped),
`PATCH_TRACE_FRAMES=1` (the UI thread's work per second, by section).

Memory (October 1):

- The UI's heap grew ~3–6 MB a minute while windows drew (3.3 MB a
  minute with the meters animating, 6 with the Visualiser on screen;
  nothing while idle). Not Patch's Rust allocations (dhat: flat), not
  GPU memory (flat): the OpenGL path (the software renderer didn't grow).
  The Visualiser's GPU renderer handed glyphs atlas slots per frame in
  order of first use, so the atlas was remade (glTexImage2D) nearly
  every frame and the NVIDIA driver's heap kept growing. Glyphs now keep
  their slot; new ones are added with glTexSubImage2D, and the atlas is
  remade only for a new cell size: +18 → +3 MB in 3 minutes (the +3 is
  warm-up after a start, the same on every renderer).
- Fonts went through fontdue, which parses every glyph up front (a Nerd
  Font: 48.6 MB, loaded twice; the title faces 73.6 MB); ab_glyph parses
  a glyph when it's drawn: 2.5 and 3.7 MB. UI heap after start 252 → 86 MB.
- Every track is kept as 16-bit by default (High-quality decks keeps
  lossless ones as float): engine with two decks 291 → 171 MB.

Finding a leak: `omarchy-patch status` shows the UI's heap (in use /
held free); PATCH_TRACE_HEAP=<file> logs every 10 s each update step's
heap growth; `cargo build --release --no-default-features --features dhat-heap` (with
PATCH_DHAT_SECS=<s>) profiles Rust allocations by call site, live ones
at that moment; in that build PATCH_MTRACE=<from>,<to> with
LD_PRELOAD=libc_malloc_debug.so.0 and MALLOC_TRACE=<fifo> traces C
libraries' mallocs too.

Normal builds use jemalloc for Rust and linked native libraries, including
the engine daemon, to reduce memory retained by glibc's thread arenas.
DHAT builds disable jemalloc so profiling and glibc's malloc tracing keep
working. For a controlled allocator comparison, build with
`--no-default-features` to use glibc; compare RSS under the same workload.
An allocator change does not eliminate live audio buffers or guarantee a
particular RSS reduction. Existing processes keep their allocator until
they are upgraded to a committed release through the development workflow.

Idle, as the system's audio (October 1, this machine; decks empty, no
windows, nothing playing; PipeWire + WirePlumber + the engine):

| | CPU | wake-ups/s | memory |
|---|---|---|---|
| plain PipeWire (no Patch) | 0.35 % | 33 | 89 MB |
| Patch, before | ~8 % | ~1,360 | 283 MB (two tracks loaded) |
| Patch, now | 0.60 % | 100 | 137 MB (engine 27 MB) |
| Patch, now, an app playing | 1.6 % | 426 | 140 MB |

What it took: the engine drives the graph only while it makes sound itself
(a deck plays; PipeWire fixes a link's passivity from node.passive when
it's made, so the outputs are made again on a change), so an idle card
suspends; the program source and the fallback loopback are passive; a
paused deck renders nothing; idle threads poll 4–10 times a second, not
200; the system volume is read on events, not every 2 s with pactl.
`omarchy-patch engine status` says why the engine drives (graph_reason).
