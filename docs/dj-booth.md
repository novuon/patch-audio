# DJ Booth — architecture, audit, plan

Patch is the application; the DJ Booth is one of its Units. This document
is the contract for turning the DJ Booth into a professional performance
system: what exists, what is missing, how it will be built, and how we
will know it works. It is kept current as work lands.

## 1. Audit (Patch v0.5.0)

What exists and is kept:

| Area | Today | Verdict |
|---|---|---|
| Real-time engine | `patch-engine`: detached daemon, one PipeWire filter node, named channels (`bus`, `deck-a`, `deck-b`), stamped `MeterBlock`s via lock-free SPSC rings, no allocation/locks in the callback, callback cost measured (~2 µs / 10.7 ms cycle) | Foundation. Keep. |
| Clock | Every cycle carries `BlockStamp` (graph sample position, CLOCK_MONOTONIC ns, rate, frames) | Authoritative audio clock. Keep; deck positions must derive from it. |
| Presentation timing | Meter shows block N at `nsec + quantum + reported device latency + trim` (OUTPUT) or unadjusted (ENGINE); latency level REPORTED | Correct model; MEASURED/TRACKED levels still missing. |
| DSP | `patch-dsp` tier dispatch; assembly kernels with Rust oracles; bench matrix | Keep. |
| Mixer controls for decks | Engine channel gain ramps, balance, 3-band LR4 isolator EQ with kills, resonant filter, PFL, cue/master blend, headphone output | Real engine features. They currently surface only in the DJ mixer window. |
| Recorder | Program source straight from the engine, bit-exact | Keep. |
| Commands | Typed, validated `Command`s over IPC; AI origin bounded | The deterministic command layer the Auto Mix will use. |
| Analysis | `patch-analysis`: content-cached tempo/grid, key (Camelot), R128 loudness/true peak, overview/detail waveforms, coarse/precise kick-grid quality, downbeat, energy and time-indexed rhythm evidence | Rhythm evidence includes tracked beat events (an exact constant grid unless the tempo really moves), alternative tempo/bar phases and 16-beat raw coverage/jitter/phase-spread regions. Saved human fixed grids and active human beatgrids are applied only to runtime copies for Booth and AI, never written into analyzer cache. A bounded background worker selectively caches local chroma/key, beat verification, onset-phase groove profiles and energy/onset structural boundaries for top AI transition candidates; unstable measured phase over a proposed overlap removes that pairing, and supported subdivision similarity contributes a small ranking term. Version-current local structure can break mix-point ties inside analyzed regions; Level 1 structure is the fallback. All these are raw evidence, not calibrated probabilities or semantic section labels. Ordinary-track vocal activity, genre embeddings and calibrated operational confidence remain planned. |
| Decks | **mpv processes** (one per deck) playing into per-deck virtual soundcards | **Main gap.** See below. |
| Library | Recursive scan of ~/Music + cliamp history; substring filter | **Gap**: no database, no identity beyond path. |
| Performance metadata | Hot cues keyed by *path* (JSON); tempo/key/EQ in `$XDG_RUNTIME_DIR` (lost on reboot) | **Gap**: identity and persistence. |

### Why mpv decks can't be professional

mpv is an excellent player, but as a deck it caps what the Booth can be:

* position is reported by IPC at a few Hz and not tied to the engine clock,
  so phase is estimated, not known;
* loops, beat jump and hot cues are *seeks*: each flushes and refills
  mpv's buffers (audible gap, not sample-accurate);
* slip mode, reverse, scratch and temporary cue are impossible;
* sync can only be a hard seek, never a gradual phase correction;
* two decks are two processes with independent buffering.

**Decision:** decks move into the engine process as native transports.
Decoded PCM lives in memory; a per-deck render worker (non-RT) reads it,
applies tempo/key with Rubber Band R3 (real-time mode) or varispeed, and
feeds a bounded ring the RT callback pulls from; every rendered chunk carries
its source position, so the engine knows exactly which source sample is
leaving at every graph cycle. Loops, jumps and cues become sample-accurate
position changes inside the worker. mpv remains for internet radio only.

## 2. Architecture

```
            ┌──────────────── patch-engine (detached, authoritative) ────────────────┐
 files ──►  │  deck workers (non-RT)             RT callback (PipeWire graph cycle)  │
 decode+SRC │  ┌ Deck N: PCM (source rate→graph │  ┌ pull deck rings (sample positions)│──► program ──► device
 (ffmpeg,   │  │  rate, soxr), transport,       │  │ EQ / filter / gain ramps         │──► cue ────► headphones
  soxr)     │  │  loops, cues, jump, slip,      │  │ PFL, meters, MeterBlock stamps   │──► patch.program (Recorder)
            │  │  Rubber Band R3 / varispeed ──►┼─►│                                  │
            │  └ ring (bounded, lock-free)      │  └ bus channel (apps via Router)    │
            │  control: validated commands ◄────┼── IPC (JSON) ◄── UI / CLI / MIDI / AI intents
            └────────────────────────────────────────────────────────────────────────┘
 patch-library (SQLite, content identity) ── tracks, analysis refs, cues, loops, grid edits, history
 patch-analysis (async, cached, versioned) ── overview + detailed waveform, beats, downbeats, key, loudness
 Patch UI units (Slint windows) read state + stamped timing; they never time audio themselves.
```

Boundaries:

* **DJ Booth** owns decks (transport, loops, cues, sync), the library and
  performance state.
* **Patch Mixer** owns channel strips. Deck channels are engine channels,
  so the Mixer shows and controls them (gain, EQ, filter, PFL); the DJ
  mixer window is a DJ-focused view of the same controls, not a second
  mixer.
* **Patch Meter** reads the stamped engine blocks.
* **Patch Recorder** records `patch.program`.
* **Patch Router** owns where the program and headphones go.
* **Clock**: the engine's graph clock. Deck positions, phase, waveforms and
  meters all derive from it plus explicit latency.

Timing positions, kept separate everywhere:

* *render position*: the source sample the engine is producing now;
* *output position*: render position delayed by the graph quantum;
* *presentation position*: output position plus device latency (REPORTED
  from PipeWire; MEASURED once an Analyzer loopback calibration exists;
  TRACKED drift). Bluetooth devices (AirPods Max, Bose) report a codec
  latency; Patch shows it with its level and never claims more precision.

## 3. Gap list and plan

Ordered for a vertical slice that proves the architecture (§5).

| # | Gap | Plan | Status |
|---|---|---|---|
| 1 | Native deck transport | `patch-engine::deck`: PCM store, render worker, Rubber Band R3 FFI, rings with source positions, varispeed without keylock | implemented: local files play on engine decks (mpv only for radio); positions stamped per engine cycle |
| 2 | Loops, auto loops, beat jump, quantize, slip | Worker-side, sample-accurate, grid-quantized; slip keeps a shadow position | implemented (loop move on jump); reverse/scratch later |
| 3 | Sync: tempo + beat/bar phase, master deck, gradual/hard correction, manual override | Phase from engine sample positions; gradual = bounded tempo trim; any manual touch suspends correction | implemented: tempo, beat phase and bar phase (downbeat on downbeat when both are known); measured 0.000–0.18 ms held; hard re-correction above 20 ms |
| 4 | Temporary cue, cue-play, end-of-track state, eject | Transport states | CDJ cue, end-of-track, eject implemented; temporary (held) cue planned |
| 5 | Native library/package source of truth | `patch-library`: versioned `.patchdj` manifest + SQLite, stable UUIDs, bounded fingerprints, portable audio references/embedding, integrity validation and migrations | implemented (directory package; archive form later) |
| 6 | Persistent performance metadata | Human cues, loops, grid layers, rating, tags, notes and colour are separate from versioned analysis; human records survive reanalysis | storage implemented; UI coverage partial |
| 7 | Library search and filters | Indexed text/filter query plus persisted playlists and smart rules; prepared sets have their own model | storage/query APIs implemented; Booth browsing and editing remain partial |
| 8 | Multiresolution waveform | Versioned checksummed PWF sidecar; analyzer currently persists overview bins, format permits multiple levels | overview + detailed level (150 columns/s, low/mid/high, PWD1 sidecar); scrolling deck waveform rendered in Rust at the heard playhead with beat/bar/phrase ticks, cues, loop, clip marks |
| 9 | Downbeats, phrases, structure, energy curve | Analysis v2: downbeat from bar-level onset accents, 8/16-bar phrases, energy curve, silence regions; manual grid editing overrides | downbeat (low-end accent) + per-second energy in analysis v2; structure segmentation planned |
| 10 | Deck buses in Patch Mixer | Provider exposes engine channels as Mixer strips | implemented: Deck A/B strips (engine-metered; fader = the DJ channel fader, one value in two views; mute on the engine channel) |
| 11 | Effects | Beat FX per deck channel, post-fader with spill-over: 14 grid-locked effects, chains of up to three on one depth knob (`patch-engine::fx`); engine send buses and an FX Unit later | Beat FX implemented (O(1) switching, quantum-independent output, 6 ms fades, denormal-safe); send buses planned |
| 12 | Diagnostics mode | DJ Booth engineering view: backend, rate, quantum, callback avg/worst, xruns, deck sample positions, master clock, presentation timestamp, latency and level, worker/decode queues, CPU | implemented (Performance · Diagnostics, key y, a core Unit; costs nothing while closed; developer mode opens one on every workspace) |
| 13 | Performance history | Session/event tables distinguish recommendation, plan, decision and executed events | persistence and initial Booth events implemented; full automation capture/replay planned |
| 14 | Input abstraction | Semantic `DjAction`s; keyboard map as data; MIDI/HID later bind to the same actions | implemented: `patch-core::input` keymap (data, ~/.config/omarchy-patch/dj-keys.json) maps keys and MIDI notes/CCs per context to validated commands; deck and mixer windows forward keys; MIDI controllers read from ALSA raw MIDI (hotplug, running status, SysEx/real-time skipped) and resolved in context "midi" by the engine-owned `patch-controller` worker; explicitly configured evdev HID buttons, encoders and axes use device-scoped profiles; the Controller Unit (MIDI & HID, key h) asynchronously lists devices and accepted/refused events; input continues with every window closed |
| 15 | Auto Mix boundaries | `DjIntent`, `TransitionPlan`, `DeckAction`, `MixerAction`, `EffectAction` types validated into commands; no AI dependency | implemented: AI may suggest and prepare only; a human runs the transition. Human-enabled Auto prepares the idle deck 45 s before mix-out; it never starts the mix. Alternate suggestions are selectable, and kick-led intros use cached 3-band analysis. |
| 16 | AI visualization data interface | State stream (beats, structure, compatibility, energy, plan, confidence) | implemented: `patch-core::intent::AiView` from real state (engine beat/bar/phrase phase, key, tempo, energy now + 16 s ahead, compatibility, plan progress/confidence/rationale, tension); `query ai` on the control socket; visuals later |
| 17 | Measured latency | Analyzer loopback calibration → MEASURED level | implemented (not yet run live): engine `in_measure` port, 4 log sweeps, FFT cross-correlation in graph-position lockstep, ≥ 3 agreeing sweeps or a stated failure; saved per output device; meters use it (MEASURED); Analyzer Measure (l), `omarchy-patch latency measure` |
| 18 | Reverse, scratch/jog | Worker reads backwards / jog scrub via controller abstraction | architecture, later |
| 19 | 4+ decks | Engine channels and workers are per deck; UI grows | architecture |

## 4. Rules (kept by review and tests)

* Nothing in the RT callback allocates, locks, touches files, the database,
  the network or the UI. Workers and the analyst do.
* No hidden resampling: decode + SRC is explicit (soxr, precision 28 bit),
  once, at load; the track's native rate/bit depth is kept in its metadata.
* Source files are never modified; DJ metadata lives in the library DB.
* AI never touches the engine directly: it emits intents, validated into
  commands. A human DJ needs no AI.
* A playing deck survives UI, library, analysis and AI failures (they live
  outside the engine or on isolated threads).
* No fake controls: a control either works or is absent / marked planned.

## 5. Vertical slice — acceptance criteria

Each is checked by a test, a benchmark or a live measurement, recorded in
the CHANGELOG / docs/bench.md when it lands.

1. Browse and search the library (≥ 10 000 tracks: query < 5 ms).
2. Import WAV, AIFF, FLAC, ALAC, MP3: decode tests per format; native rate
   and bit depth recorded.
3. Analyse tracks off the playback path; cached, versioned; re-analysis
   when the version changes.
4. Load Deck A and B; load-to-play < 300 ms for a 6-minute FLAC (cached
   decode) — measured.
5. Overview + scrolling waveform with beat and bar markers; render cost per
   frame < 1 ms — measured.
6. Both decks play from the engine; their positions are sample positions
   of the engine clock (test: position advances exactly `frames` per cycle
   at speed 1).
7. Headphone cue with PFL and blend (done).
8. Tempo ±8/16/50 %, keylock (Rubber Band R3), key shift.
9. Sync: tempo and beat phase; after sync, measured phase error < 1 ms at
   the engine; gradual correction keeps it there within ±2 ms under tempo
   drift; manual nudge suspends correction.
10. Hot cues (done) and loops: loop boundaries sample-accurate (test: the
    sample after loop end is the loop start sample; no gap, no click).
11. Beat jump ±1/4/16 beats, quantized.
12. Mix through engine channels, visible and controllable in Patch Mixer.
13. Meter shows the mix in sync (done: OUTPUT timing).
14. Record the program (done: bit-exact).
15. Keyboard operation of all of the above.
16. Diagnostics view with the timing figures listed in §3 #12.
17. Close and reopen Patch: analysis, cues, loops, grid edits, tempo/key/EQ
    per deck restored.

## 6. Known limitations (honest list)

* Output latency is REPORTED by PipeWire; Bluetooth devices may report less
  than they add. Until the Analyzer's loopback calibration exists, sync to
  the ear on Bluetooth is only as good as the report plus the manual trim.
* Headphone cue on a second device runs on that device's clock; PipeWire
  resamples adaptively, so cue and program drift by a few samples. Cue on
  the same interface (e.g. a 4-output card) avoids it.
* Real-time scheduling is unavailable on this system until the user is in
  the `realtime` group; the engine reports it.
* Internet radio stays on mpv (a live stream has no grid, loops or cues).
