# ADR-010: Derived Material

Status: Accepted — first slice implemented (`crates/patch-material`, the
engine's material service and sampler)

## Decision

Patch derives temporary musical material from tracks it already knows,
on demand: a clean two-beat vocal from deck A's phrase before the drop, a
one-bar percussion loop, a kick one-shot, a texture to stretch under a
transition. **Derived Material is Patch infrastructure** (`patch-material`,
served by the Patch service). Pattern, Deck, the DJ Booth, the AI director, the
Sequencer and future units consume it through typed ids
(`patch_core::material::{AssetId, KitId}`); none of them cuts audio.

```
track / stem ─ analysis + stem metadata ─ Derived Material ─ asset cache
                                                │
                       Pattern · Deck · DJ Booth · Sampler · AI director · …
```

## Model

* **Request (musical, not seconds).** `Request { source, stem, kind,
  region, requirements, limit }`. `region` is musical: `Phrase { index |
  Last | BeforeDrop | AfterDrop }`, `Bars { bar, count }`, `Beats { beat,
  count }`, `Section { kind }`, with an optional beat range inside it
  ("phrase 47, beats 3–4"). Seconds exist only as an explicit escape hatch.
* **Resolution.** The resolver turns a request into exact source frames
  from the track's beat events (bar and beat-in-bar), downbeat, structure
  sections and energy, and records *why* and *how sure*: grid support,
  downbeat confidence, boundary salience. A phrase is 32 beats (8 bars),
  the convention already used by the Auto Mix.
* **Recipe = identity.** `Recipe { version, source: SourceRef { track,
  path, fingerprint (SHA-256 of the file, the stem cache's source hash),
  stem, stem_set }, region: Resolved { start/end frame at a rate, first
  beat, beats, bar, phrase, source BPM, key }, kind, transforms,
  analysis_version }`. `AssetId = "dm-" + 64-bit hash of the recipe's
  canonical JSON`: the same recipe is the same asset on any machine.
* **Transforms are data**, applied in order when rendering, never to a
  stored intermediate: trim, transient snap, fades, reverse, gain,
  normalize, loop seam, filter, envelope, repitch, time-stretch, pitch-shift,
  freeze, stutter, slices. Stretch and pitch use the engine's Rubber Band
  (high quality, offline); without it, the transform is refused rather than
  faked.
* **Asset** = id + recipe + measured metadata: frames, duration, peak, RMS,
  transient positions, loop seam discontinuity → `loop_suitability`,
  estimated bleed for stem material, a `quality` score with reasons, the
  source and resulting BPM/key, creation time, cost, cache policy, consumers.
  A poor asset says so; nothing is reported clean that isn't.

## Cache

`~/.cache/omarchy-patch/material/<id>.f32` (interleaved stereo f32, the
graph rate) and `<id>.json` (the asset). Owned by the service.

* **Policies**: `while_referenced`, `session`, `reuse { seconds }`,
  `keep` (expensive to regenerate). Consumers hold references (a Pattern
  layer, a kit, a deck, an AI plan); released references make an asset
  evictable under its policy.
* **Budget** (`PATCH_MATERIAL_CACHE_BYTES`, default 512 MB): above it, the
  least useful unreferenced assets go first (policy, then last use). Every
  cleanup returns a report (removed, freed, kept, pressure); `stats` shows
  size and pressure at any time. Nothing grows silently.
* **Regeneration**: a missing file is re-derived from its recipe. If the
  source's fingerprint no longer matches (the file changed or moved), the
  asset is `stale` and says so; it is never silently rebuilt from a different
  file.

## Preparation and readiness

Extraction, decoding, stem loading and stretching run on background
workers, never on the audio thread. A request returns an id and a state:
`queued → preparing → ready | degraded | failed`. With a deadline, the
service degrades in order: **cached → clean stem extraction → full-mix
extraction (flagged) → skip**. The reason is part of the state.
Prediction (`prepare` for a likely next track) uses the same queue at low
priority and only for the regions a transition can use.

## Real-time handle

A ready asset is loaded into the engine's **sampler**: an immutable buffer
in a slot of a fixed table. Pattern's `Sample` events become `RtEvent {
kind: Sample, index: slot }` in the same queue as Sequencer voices; the
sampler starts the voice on its sample with a short raised-cosine fade
(no clicks), loops when the asset is a loop, and retires slots only when no
voice uses them. The audio thread never decodes, allocates or frees.

## AI

The AI director asks semantically (`kind`, `stem`, musical region, requirements
like `clean`, `loopable`, `beats: 2`, target BPM/key, deadline) and gets
ranked candidates: suitability, confidence, musical position, BPM/key fit,
estimated cost, whether cached, and reasons. It never needs sample offsets
(they are in the recipe if it wants them).

## Provenance

Every asset, kit slot and Pattern sample event resolves back to its recipe:
which file (fingerprint), which stem set, which frames, which transforms.
Sessions persist recipes and kits, not audio.
