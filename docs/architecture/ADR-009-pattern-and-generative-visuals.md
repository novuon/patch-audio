# ADR-009: Pattern, and a second engine in the Visualiser

Status: Accepted — first slice implemented (`crates/patch-pattern`, the
Pattern `pattern` op in the engine, and the reference/Strudel engines)

Related: [ADR-010](ADR-010-derived-material.md) (Derived Material, which
Pattern consumes through typed ids) and `docs/pattern-findings.md` (what
running the engines taught us), `docs/third-party-engines.md` (licensing).

## Decision

Two additions, both Patch-native, neither tied to the DJ Booth:

1. **Pattern**, a new Unit: algorithmic, temporal, live-codable control.
   A pattern is a pure function *time span → events*, built from one
   serialisable AST. Typed code, the Sequencer, the AI director, the API and future
   importers all produce that AST; nothing machine-facing goes through
   generated source text.
2. **The Visualiser becomes multi-engine**: the existing ttfx-inspired
   **choreography engine** stays as it is; a **generative engine** (a visual
   node graph compiled to one GPU shader) is added beside it; a
   **compositor** layers them. Modes: choreography, generative, hybrid.

### Engines sit behind Patch contracts (use → learn → specify → replace)

Patch owns the interface; a mature engine runs behind it as a separate,
sandboxed process while we learn what Patch needs, and a native Rust engine
replaces it later without changing the contract. Nothing machine-facing
depends on an engine.

```
Patch contract  →  engine adapter (today)          →  native (later)
```

* **Pattern**: the contract is `Backend` (`patch_pattern::backend`):
  *define events in this span, with your capabilities*. Today there are two
  implementations — the Strudel adapter (`adapters/strudel`, AGPL, installed
  separately, run under `node --permission`) and Patch's own **reference
  engine** (`pattern`/`mini`/`dsl`). `tests/engines.rs` proves they give
  identical events for deterministic programs (P7).
* **Generative visuals**: the contract is Patch's visual scene → compiled
  fragment shader + uniforms (`patch_visual`). Today Hydra is used as a
  **shader compiler** only (`adapters/hydra`, AGPL, headless, no browser);
  Patch owns rendering, feedback, compositing and timing.

No browser runtime, WebGL, or `eval` is a Patch dependency: Strudel and Hydra
run unmodified in their own processes, and their code never leaks past
`adapters/` and the adapter modules. Where an engine can't do something (e.g.
Hydra has no blur, V3), Patch does it, or the capability is reported as
`unsupported` rather than approximated silently. The reference engine is
written from the published semantics and the textbook algorithms, checked
against the engines by tests, never translated from their source.


## Modules

```
crates/patch-pattern/            NEW · the Pattern Unit's engine (no UI, no audio)
  time.rs        Time (exact rational cycles), Span
  pattern.rs     Pattern<T>: span → Vec<Hap<T>>; generic algebra
                 (seq, stack, alt, fast, slow, rev, rotate, every,
                 sometimes, degrade, euclid, polymeter, fmap, …)
  ast.rs         Pat / Transform: the serialisable graph, compiled to Pattern<Value>
  mini.rs        mini-notation ("bd*4 [hh hh] <sd cp>", "x(3,8)", "{a b c}%4")
  dsl.rs         the live-coding language: one layer per line,
                 `target("mini").method(args)…` → Program (AST); no host eval
  event.rs       the typed event contract: Target × Value, with exact time
  tempo.rs       TempoMap: cycle ↔ graph sample, piecewise, anchored (no drift)
  scheduler.rs   lookahead preparation, quantised hot swap, transport
  rt.rs          RtEvent + SPSC queue: what crosses into the audio thread
  ai.rs          describe, notation, address a node (layer + path),
                 mutate (wrap/replace/unwrap, mute, layers), structural
                 diff, compare (density, onset overlap), explain in words
  strudel.rs     the Strudel adapter (process client)
  (state lives in the service: `pattern.json`, see State)

crates/patch-visual/             EXTENDED · the Visualiser Unit
  director.rs, stage.rs, …       choreography engine (unchanged behaviour)
                 + Director::intent(): what it plays, as VisualIntent
  generative.rs  VisualScene / VisualNode / Param (const | signal), the op
                 vocabulary, validation, Signals (the bus: clock, energy,
                 bands, kick, transition, pattern:*, intent:*, stem:*),
                 Bindings (uniforms, smoothing), the GLSL ES 1.0 → 3.30
                 shim, Mode, Compositor + its CPU reference, presets
  hydra.rs       the Hydra adapter (process client) behind VisualBackend
  feed.rs        the pattern timeline placed on the graph clock; sync error

crates/patch-core/src/visual_intent.rs   the shared semantic-intent contract
                 (the AI director and Pattern write it; the Visualiser reads it;
                 `Board` is the engine's `visual_intent` op)
crates/patch-engine                      pattern_service.rs (scheduler thread,
                 `pattern` op incl. AI actions), material_service.rs,
                 sampler.rs (in the RT path, fed by the pattern queue)
crates/patch-surfaces                    pattern_view.rs + ui/pattern.slint;
                 visual_gen.rs (mode, scene, compiler worker, signals);
                 visual_gl.rs gains the generative pass (feedback
                 ping-pong) and the composite before glow/split
```

## Pattern

**Time** is `Time { num: i64, den: i64 }` (reduced, exact), in *cycles*. A
cycle defaults to one bar (4 beats), set per program. Haps carry `whole`
(the event's full span) and `part` (the fragment inside the queried span),
so an event split across query windows is still sent once (at its onset).

**Algebra** is generic over `T: Clone`: every operation is a function on
`Pattern<T>`, so the same `every`, `fast`, `euclid` work for notes, deck
commands or visual parameters. Randomness (`sometimes`, `degrade`, `?`) is a
hash of (seed, the event's onset, the node), never a stateful RNG: the same
pattern queried twice, or on another machine, gives the same events.

**AST.** `Pat` is a serde enum (`{"seq":[…]}`, `{"fast":{"by":"2","pat":…}}`);
`Transform` is the first-class function an `every`/`sometimes` applies. The
DSL, mini-notation and AI all build `Pat`; `compile(&Pat) → Pattern<Value>`.

**Event contract.**

```rust
struct Event { whole: Span, onset: Time, target: Target, value: Value, layer: u16 }
enum Target {
    Sound { name },                       // a Sequencer voice / sample
    Note,                                  // pitches (future synth units)
    Deck { deck, param: DeckParam },       // filter, eq, volume, fx, transport, cue
    Stem { name, action },                 // mute/solo/level
    Mixer { strip, param }, Route { … },   // validated Commands
    Midi { channel, kind }, Control { name },
    Visual { param },                      // intensity, symmetry, scene, hue, …
    Scene, Ai { instruction },
}
```

Consumers subscribe by target kind. Deck/stem/mixer/route targets compile to
`patch_core::command::Command` and go through `command::validate` with the
**origin that installed the program** (an AI-written pattern runs under AI
limits; the AI director never becomes the scheduler).

**Timing (ADR-005).** The scheduler runs in the Patch service on a control
thread. `TempoMap` anchors cycle `c₀` to graph sample `s₀` at a tempo; a
tempo change adds a new anchor at an exact cycle, so error never accumulates
(≤ one sample per tempo change). Tempo comes from the master deck's grid when
following, else the Pattern's own. Each tick (~10 ms) prepares
`[cursor, now + lookahead)` cycles:

* Sequencer voices → `RtEvent { sample, voice, velocity }` into a
  preallocated SPSC ring; the sequencer's render loop triggers them at their
  sample. The RT thread never parses, allocates or queries a pattern.
* Commands → validated and dispatched at their time by the control thread.
* Visual targets are **not** sent as events: the Visualiser holds the same
  program and the same tempo anchor (published with the service clock) and
  queries the visual layers at the frame's own cycle. Same function, same
  clock: a visual pulse lands on the audio onset, at any frame rate.

**Live coding.** Evaluate parses and compiles off the RT path; an error
(with line:column) leaves the running program in place. A new program swaps
in `now`, or at the next cycle / bar / phrase (4 cycles) boundary. The
surface shows the cycle position, the active and pending program, and the
next events.

## Visualiser

```
choreography (Director → Cue → cell grid) ─┐
generative (graph → shader → texture)  ────┼─ compositor ─ glow/split ─ output
text/image/future sources            ──────┘   (layers: opacity, blend, mask,
                                                  crossfade, scene switch)
```

**Generative graph.** Nodes form a chain/DAG: generators (osc, noise,
shape, gradient, voronoi, solid), transforms (rotate, scale, scroll,
kaleid, repeat, pixelate), modulation (modulate, modulate_rotate,
displace), colour (hue, saturate, contrast, brightness, invert, colorama,
posterize, threshold, luma), blends (add, mult, diff, layer, mask), blur and
feedback (the previous frame as a source). Every parameter is a `Param`:
a constant, or a binding to a named signal (`kick`, `bass`, `pattern:intensity`,
`intent:symmetry`) with scale, offset and smoothing. The graph compiles to one
GLSL 3.30 fragment shader; parameters are uniforms, so moving a control never
recompiles. The structure changing recompiles once (off the per-frame path).
The GL pass reuses the existing glow/Slint rendering notifier, in the same
context as the choreography passes.

**Hybrid.** The Director already decides section and timing. `Director::intent`
exports that decision as `VisualIntent` (section, intensity, motion,
symmetry, density, transition length, palette hue) without changing what it
plays. `interpret` maps intent onto a graph's bindable parameters. One
prepared choreography therefore drives varied generative pictures and stays
musically placed. Choreography never names a shader parameter.

**Signals.** One `Signals` struct, filled per frame by the surface from
generic sources (deck playheads, meters, the Pattern program, AI intent).
The engine reads names, not sources.

## AI director

The AI director writes **semantic intent** (`patch_core::visual_intent::Intent`: energy
direction, visual density, symmetry, motion, transition, focus) through the
service (`op: visual_intent`), and Pattern ASTs (`op: pattern`, origin `ai`).
`patch_pattern::ai` gives it inspect, clone, mutate-at-path, compare (event
density, onset overlap over N cycles) and explain (structural diff in words).
The AI director never runs the scheduler or the renderer.

## State

* Service: `~/.local/state/omarchy-patch/pattern.json` — source, AST,
  quantise mode, cycle length, tempo mode, seed, origin.
* Visualiser: `~/.local/state/omarchy-patch/visualiser.json` — mode, graph,
  bindings, compositor layers, preset, output.

Both are plain JSON and restore the same performance state.

## Increments

1. `patch-pattern`: time, algebra, AST, mini-notation, DSL, events, tempo
   map, scheduler, RT queue, AI ops, state. Tests for exact timing, determinism,
   hot swap; a bench for query cost.
2. `patch-visual`: signals, intent, generative graph + GLSL + CPU reference,
   compositor, presets. Tests: every shader compiles, determinism, intent
   mapping, blend maths.
3. Service: `pattern` and `visual_intent` ops, scheduler thread, sequencer
   RT ring, Command dispatch, persistence.
4. Surfaces: Pattern Unit surface; Visualiser modes and the GL generative pass.
5. Docs, CLI (`omarchy-patch pattern …`).
