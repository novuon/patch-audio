# ADR-003: One DJ engine, two DJ surfaces

Status: Accepted

## Decision

There is exactly one DJ engine: the Patch service's decks (transport,
loops, cues, grids, sync, keylock, EQ, FX), the Patch DJ format and library
(ADR-001), `patch-analysis`, the deterministic transition engine
(`patch-ai`) and the DJ session (crossfader, faders, queue, auto-mix).

The **DJ Booth** (Deck A, Deck B, Crossfader, Library, Diagnostics) and
**Light DJ** are both surfaces of the DJ Booth Unit onto that engine. Light
DJ isn't a Unit and has no state of its own:

* now playing, next, the queue;
* play/pause, next, transition now;
* a small waveform, BPM, key, a basic cue and loop, sync;
* auto-mix on/off and a direction (build / hold / release);
* master volume, quick queue editing.

Switching between them is only a change of window. Light DJ playing Deck A
with Deck B prepared looks exactly the same in the Booth: no reload, no
interruption, no copy of state. Closing either surface leaves the session
running.

## Auto-mix

Auto-mix is the deterministic transition engine running on the service's
DJ session. It uses BPM, grid, downbeats, phrases, intro/outro energy, key
and loudness (all from analysis) to pick the next queued or library track,
prepare it and execute the transition on the beat clock. A human turns it
on, and any human touch takes over. It works without any AI.

An AI layer (optional, off by default) may only propose: ranked tracks, a
direction ("keep the energy up", "stay around 125 BPM", "surprise me") or a
transition plan. Proposals become structured intents that the
deterministic engine validates and executes. No free-form model output
reaches the real-time path or issues commands directly.

## Consequences

* The Booth isn't simplified for Light DJ. Light DJ is a convenience view.
* Tests must show both surfaces reading the same engine state, and that
  switching surfaces doesn't touch the audio.

## Unified DJ workspace

The `dj` surface combines live session operation, preparation, recording history and report processing. `light-dj` and `ai-dj` remain launch aliases; Play shows the Auto Mix permanently beside the live controls (Overview / Why), and the `dj` window alone takes Auto Mix keys. A second screen shows the Visualiser; the display-only `ai-stage` view was removed (2026-11-22). Offline report jobs never run on the audio callback. See [DJ workspace](../dj-workspace.md).
