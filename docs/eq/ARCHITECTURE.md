# EQ unit architecture

EQ is a Patch Unit for intentional tonal shaping. It is separate from Tune:

> EQ owns intentional tonal shaping. Tune owns physical output correction,
> calibration, optimization, and protection. Both reuse the common Patch
> DSP/filter infrastructure but remain semantically independent.

EQ describes user or subsystem intent. Tune describes what a physical output
needs to perform safely and correctly. A graph may place multiple EQ
processors at source, deck, bus, microphone, or master positions. The graph
chooses placement; EQ is not a mandatory global stage. A common playback path
is source → EQ → mixer → master EQ → Tune → Router → output.

## What runs today: the master EQ

The EQ Unit's first processor is the **master EQ**: eight parametric bands
on the program, after the mix and before Tune and the master limiter
(source → mixer → **master EQ** → Tune → limiter → output).

* Bands: bell, low/high shelf, low/high pass, notch; 20 Hz … 20 kHz,
  ±18 dB, Q 0.1 … 18. Settings persist in
  `~/.local/state/omarchy-patch/eq.json`.
* The control thread publishes band settings through a seqlock; the RT
  thread designs the filters (`patch_dsp::filter`, f64) when settings or the
  graph rate change, and interpolates the coefficients across the cycle a
  change arrives in (no clicks when dragging). No allocation or locking on
  the RT thread. Zero latency.
* Flat or bypassed, nothing is processed: the program passes bit-exact at
  no cost. Eight active bands cost ~18 µs per 1024-frame cycle.
* Control: validated commands `eq.band`, `eq.bypass`, `eq.gain`, `eq.reset` (UI, CLI
  `omarchy-patch eq …`, MIDI later); the service op `master_eq` reads or
  replaces the whole state.
* The EQ window draws the response in Rust at physical resolution, only
  when a band changes (~1.1 ms at 1200×480); the grid is static; the
  program spectrum (after the EQ) is its own half-resolution layer at
  15 fps while shown (~0.1 ms). Drag a node for frequency and gain, scroll
  for Q, double-click to switch a band; keys `1–8`, arrows, `[ ]`, `t`,
  space, `b`, `s`; `f`, `g` or `Q` (or a click on the value) types the
  selected band's frequency, gain or Q: `f 400 enter`, `f 2.5k enter`.

## Channel EQs

Every engine channel has its own EQ with the same eight bands, in the
channel strip after the DJ isolator and before the fader (the headphones
hear it too): **Apps** (the `bus` channel, every application routed
through Patch), **Deck A** and **Deck B**. Same engine: settings by
seqlock, filters designed on the RT thread, gliding changes, zero latency,
nothing processed when flat. The EQ window's tabs (or `m`) pick the
target; commands and the CLI take one (`omarchy-patch eq a band 3 …`,
default the master); `eq.json` keeps one state per target. The bands of a
channel are processed in pipelined pairs (two recursions in flight), which
keeps a stereo channel with eight active bands at ~18 µs per cycle.

## Application EQs

One application can have its own EQ: the engine has four spare channels
(`app-1` … `app-4`). `omarchy-patch eq app cliamp` (or **+ APP** in the
window, listing what plays now) gives cliamp a channel: its virtual
soundcard (`Patch App 1`) exists only while assigned, cliamp's streams are
kept on it (again after cliamp restarts), and its EQ is the target
`app:cliamp`. The EQ settings belong to the application, not the channel:
assign it again later and it gets its own curve back; a freed channel is
flat before anyone else uses it. `eq app cliamp off` sends its audio back
to the Patch bus (or the default output) and removes the soundcard.
Assignments persist in `~/.local/state/omarchy-patch/app-eq.json`.

The DJ isolator stays the DJ Booth's.

## First implementation boundary

The shared filter model supports bell, low/high shelf, low/high pass,
band-pass, and notch sections. A band has an enabled state and channel target.
The data model leaves room for automation and dynamic behavior to be added at
the parameter layer. Graphic layouts should compile into ordinary bands.
DJ 3-band/isolator curves remain a distinct semantic model that can use these
shared primitives without being treated as ordinary parametric bands.

Linear-phase FIR, dynamic EQ, mid/side matrices, and arbitrary multichannel
graph wiring are not implemented by this initial foundation. IIR latency is
zero frames; future FIR processors must publish their actual latency through
the graph timing model.

