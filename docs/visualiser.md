# Visualiser

The Visualiser Unit (`w` in the controller, the third workspace of Live
DJ, or `omarchy-patch open visualiser`) plays its own animations to the
music in a window of any size, at the screen's own resolution.

## What it shows

Everything is drawn in character cells, as glowing glyphs in the theme's
font (block characters drawn exactly, so letters built from them tile
without seams), with a glow of the bright parts and a lift on every beat.

- **Backdrops** (26): rings, tunnel, warp, skyline, plasma, spectrum,
  horizon, lasers, checker, spiral, glyph fall, ripple, chase, fire,
  burst, scope, dots, stars, lines, grid, orbits, kaleidoscope, radial
  spectrum, rainfall, wavefront, hexes.
- **Acts on the title** (20): words, shatter, wave, glitch, zoom, type,
  neon, marquee, bounce, mirror, assemble, scan, stack, pixelate, drip,
  echo, split, credits, flip, spotlight.

The text is the playing song's title in big block letters with the
artist below (versions like "(Extended Mix)" left out); with nothing
playing, PATCH.

The motion follows [ttfx](https://github.com/omacom/ttfx)'s easing
curves (omacom, MIT; a Rust port of TerminalTextEffects by ChrisBuilds,
MIT). ttfx's own effects, the ones Omarchy's screensaver plays, are not
used.

## A look per song

When a song comes in it gets its own look: a palette (twelve schemes or
the Omarchy accent), a variable cast of backdrops and acts, a motion curve (springy
for a banger, smooth for a slow song), how hard the kick hits and how
much it glows. It comes from the title (the same song, the same look)
and never repeats the last song's. A slow song (under ~118 BPM, fully at
88) gets calm backdrops, gentle acts, a soft kick and longer scenes.

## How it follows the music

| Mood | When |
|---|---|
| reveal | a new song |
| lift | energy rising or high (big acts) |
| groove | a steady level |
| calm | a breakdown (calm backdrops) |
| handover | the Auto Mix's transition: a visual build, with no incoming title or artist |
| drop | that last beat: a visual drop; the first title comes later |
| idle | nothing plays |

Each track gets an arranged sequence rather than a fixed four-effect cycle.
Groove movements use two to six main visuals with 16-, 24- or 32-bar
passages; lifts use one to six visuals with 8- or 16-bar builds; the drop
holds 8 bars. A picture is a passage of the track, about half a minute,
never a flash: titles and text interludes play over the picture already
showing (no new scene, no wipe), a scene is never cut for a breakdown
before it has had four bars or for the first title before it has had
eight, and a scene only just begun carries on as the mix's first passage.
The automatic choreographer does not bring a scene back until about half
of what may play there (up to eight scenes) has had its turn. Supporting layers enter over the first two bars and recede at the end,
so the scene develops within its phrase. Visual scene changes unfold through
diagonal wipes, radial openings or vertical slats, timed over two beats
(four in calm sections). Quieter tracks have smaller casts
and longer holds. Aurora ribbons, a twisting depth vortex and rotating prism
facets extend the effect library.

Each track gets two or three calm variations. A sustained loss of bass relative to
the track, supported by the beat grid or a large loudness drop, eases motion
to 35% speed. Softer kicks and short dips keep normal motion; true silence
settles to 16%. The first title and artist stay hidden throughout a transition. After it
finishes, the reveal waits a track-specific two, four, six or eight bars,
then enters on a bar line. Text interludes and manual scene skips cannot
announce the track earlier. A newly started track also gets this delayed
reveal.

Windowed and fullscreen use the same logical stage, scaled to fill the
available canvas. Resizing adapts its aspect ratio without changing the
scene, palette, animation phase or effect.

Scenes last whole bars and start and end on a bar line. Backdrops and
acts react to the beat, the kick and the spectrum's bass, mids and highs.

## Transitions

An Auto Mix transition keeps the choreography going, in whole scenes: the
outgoing and the incoming track take turns with their own designs (each
in its own palette and beat feel), every passage a new scene, in a
pattern of the mix's own:

| Pattern | The turns |
|---|---|
| ping-pong | one, then the other, in equal passages |
| call and answer | the outgoing calls, the incoming answers twice as quickly; past halfway they swap |
| converge | the outgoing's passages shorten as the incoming's grow |
| mashup | between the plain turns, one track's backdrop wears the other's layer |

Every pattern starts on the outgoing track and ends on the incoming one.
The incoming track's last passage, from its lift scenes, runs into the
drop, which gives it the stage. Passages are 4 bars on mixes of 8 to 15
bars, 8 on most, 16 on mixes of 64 bars or more (half a short mix each
below 8 bars), never quick one-bar turns; the same track twice in a row is
one longer passage. The plan comes from both titles: the same mix, the same
show.

## Keys

- `space` (or a click): next scene from the chosen sources.
- `h`: hold the scene playing.
- `m`: mode — choreography → generative → hybrid.
- `g`: the next generative scene.
- `a`: Ambient on/off.
- `x`: switch the scene on screen off and move on (L → Scenes switches it on again).
- `l`: the settings panel (Scenes · Choreography · Layers).
- `f`: true fullscreen (the animation alone, no header or footer).

Title and artist are separate text passages. The artist, when included, uses
its own later main-lettering act. No act receives an artist subtitle or a
combined title/artist sequence. At least four visual-only bars separate text
passages, including when scenes are skipped manually.

## One scene library

Every visual Patch has is one named library: the original **Patch**
backgrounds, the full **Ambient** catalogue (34 scenes), the **Shaders**
(Patch's own generators — Silk, Aurora, Beams, Contours, Caustics, Orbit,
Rings, Bloom — then your installed ISF files and the older Hydra sketches,
which the choreographer only uses when you give them a weight) and the **Titles** (title/artist
effects). **Sources** switch each of the four on or off in any combination
(Patch + Ambient, Ambient + Titles, …); at least one picture source stays on.
The same choice governs Space, the scene list and the automatic
choreographer: nothing outside it plays, overlays included, and with Titles
off no title passages run. Shaders start off. Hybrid shader blending only
continues over a picked scene while Shaders are on.

The settings live in their own window, **Patch · Visualiser · Controls**
(press **L** in the Visualiser), so the picture stays clean. **L → Scenes** lists the filter's scenes by name; click one to show it. Space
steps through the filter, H holds, **Auto/Manual** switches automatic scene
changes on or off, and **Pulse now** fires one full-screen accent. Manual
choice never disables anything for the choreographer.

Each scene in the list has an **On/Off** switch (or press **X** while it
plays). A scene switched off is skipped by Space and never chosen by the
choreographer, in every section; clicking its name still shows it. Switch it
On again and it returns with its earlier settings. If every scene in the
chosen sources is off, Space still steps through them rather than show
nothing. The switches are saved with the show and in profiles.

**L → Choreography** configures the automatic Director:

- Selection order (weighted variety, library order, or the track's own cast).
- Per section (Idle, Reveal, Groove, Lift, Calm, Handover, Drop): motion speed,
  scene intensity, booth/performer show/hide, layer opacity and performer
  look weights. Scene lengths stay the Director's own (see above)
  until **Custom scene lengths** is on; then minimum/maximum bars apply.
- Per scene or title effect: allowed in this section, selection weight, local
  beat response, spectrum response, motion and full-screen accent multiplier.
  Calm-incompatible scenes start excluded from Idle and Calm.
- Transitions (diagonal, radial, slats, dissolve) and their length in beats,
  supporting overlays, title gap, palette (scene original, Director, Theme).
- **Full-screen pulse:** Off, Occasional accents (default: one per 16 bars and
  on a drop), Drops only, or Every beat; strength and spacing. Bump, colour
  split and shake can join the accent. Scenes keep their own beat motion
  (jellyfish bells contract and push upward on the beat) independently of the
  full-screen pulse; both can be combined on any scene.
- **Profiles:** save and load the whole show (library, choreography, layers,
  generative settings and hold) by name.

Settings persist in `~/.local/state/omarchy-patch/visualiser-control.json`;
profiles in `visualiser-profiles/`. The earlier background choice migrates to a
selected scene in All.

## Ambient catalogue

These are native scene adaptations from **omarchy-ttfx-layer**, the renderer
used by omarchy-ambient: journeys (Sunset Drive, Neon City, Vector Highway,
Circuit Run, Neon Avenue), environments (Jellyfish, Aquarium, Cockpit, Data
Globe, Solar System, Abyss, Supernova, Event Horizon), reactive scenes (Eye,
Matrix, Tunnel, Live Terminal), fourteen sand clocks and the system scenes
(Load Canyon, Packet Grid, Silicon), which read cached telemetry off the
render path. They share Patch's glyph atlas, glow and compositor, and remain
behind the Decks & Signal and Performer layers. No desktop background service
or extra audio capture is started. CPU fallback uses the same scene cells.

World state is seeded by track identity and survives text passages and resize.
Beat events come from Patch's music clock, rather than another bass-based beat
detector. Move the pointer over the canvas to interact; opening the settings
panel suspends pointer interaction. Source revision and MIT notices are in
`crates/patch-visual/src/ambient/`. Amiga demos and terminal-hosted effects
are not included.

## Generative and hybrid modes

Beside the character-cell choreography, the Visualiser has a **generative**
engine: a scene is a small graph of operations (oscillators, noise,
Voronoi cells, shapes, the last frame for feedback; kaleidoscope, rotate,
repeat, scale; colour; blend, add, modulate…) compiled to one GPU shader.
Each argument is a constant or a **signal**: the clock (`beat`, `phase`,
`bar`, `phrase`, `bpm`), the music (`energy`, `low`, `mid`, `high`,
`kick`, `transition`), a running pattern's visual parameters
(`pattern:intensity`…) and the visual intent (`intent:intensity`,
`intent:motion`, `intent:symmetry`, `intent:density`, `intent:feedback`,
`intent:hue`, `intent:section`).

* **generative**: the generative scene alone.
* **hybrid**: the generative scene beneath, the cells over it (screen
  blend, masked under bright cells so text stays legible). Its intent is
  the AI director's when one is posted (`visual_intent`), else what the Visualiser's own
  director is playing.

Scenes: `tunnel`, `cells`, `shapes`, `nebula` (drifting noise clouds,
twisted by the mids, zoomed by the bass), `prism` (a kaleidoscope refracted
by a turning triangle, for drops), `lattice` (Voronoi circuitry pixelated on
the beat) and `pulse` (a ring per kick, with trails). A pattern can switch
them (`scene("<nebula prism>/2")`) and drive them (`visual.intensity(…)`
steers the intent every scene reads); its
events are placed on the audio clock, so a visual pulse lands within a
frame of its sound.

The shader compiler today is [Hydra](third-party-engines.md) (installed
separately: `cd adapters/hydra && npm install`), run sandboxed and used only
to generate GLSL; Patch renders, times, feeds back and composites. Without
it, or on the CPU, the Visualiser plays choreography and says why in its
caption. Mode, scene and compositor are kept in
`~/.local/state/omarchy-patch/visualiser.json`; frame rate, dropped frames,
compile time, control-to-frame latency and pattern sync error are written
to `$XDG_RUNTIME_DIR/omarchy-patch/visualiser-stats.json` every few seconds.

## AI deck mission timeline

Each deck's lower overview can switch between its ordinary waveform and an
AI mission view. The switch and mission view are available only in AI Act
mode while a deck is playing, and disappear during manual takeover or
service failure. The main scrolling waveform keeps its independent style
switch.

The compact trajectory displays preparation, 8-bar review, mix and completion.
Decision and 2-bar sync stages run in the engine without a display label.
Filled dots represent completed state; open circles are upcoming, and amber
means review is underway without a validated result.

The simplified RMS waveform uses 64 coarse, tall strokes with gaps along a
curved mission rail. Stage labels `P`, `8`, `M`, `D` sit directly above their time dots; a small
bottom legend gives their meanings.
The cue/start marker has one `C`, using the AI-prepared entry position (or
first stored cue). The playback diamond appears only while playing. The
normal deck theme remains in use. A hollow diamond and needle on the waveform
show the current deck position, using the same time fraction as seeking. Only visible waveform strokes and marker dots show a hand cursor and accept
seeking; empty space and labels keep the normal cursor. There is no separate clock.

Beat-grid projections are cached for the selected transition to avoid drift
between polls. Preparation positions are captured when the UI receives state,
rather than recorded engine timestamps. A preloaded incoming track gets a
provisional lifecycle preview, replaced by its selected outgoing engine plan
when that track leads. The heading explicitly marks it PREVIEW.
