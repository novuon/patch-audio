# ADR-006: The Patch audio graph and audio profiles

Status: Proposed

## Graph

Patch's graph is Patch's own model of what it routes and processes,
distinct from PipeWire's graph, which is the transport. Nodes have stable
identifiers that survive restarts, handovers and reconfiguration:

| Kind | Id form | Today |
|---|---|---|
| application stream | `app:<name>` | PipeWire streams routed through the bus |
| physical input / output | `in:<device>` / `out:<device>` | PipeWire devices |
| bus | `bus:<name>` | `bus:patch` (the Patch bus), `bus:program`, `bus:cue` |
| mixer channel | `ch:<name>` | `ch:bus`, `ch:deck-a`, `ch:deck-b` |
| deck | `deck:<id>` | `deck:a`, `deck:b` |
| DSP | `dsp:<ch>/<kind>` | EQ, filter, Beat FX per channel |
| tap | `tap:<point>` | meters (per channel, program), analyzer, recorder (`patch.program`) |
| send / return | `send:<from>/<to>` | planned |
| plugin, AI processor | `plugin:<id>` | planned |

Edges are `(from, to)` pairs of these ids. The service exposes the graph
(`{"op":"graph"}`) so the Router, diagnostics and the CLI read one truth.
A change to one path (a new app routed, a device switched) never restarts
the engine or touches the other paths: links are added and removed per
port, the engine node stays, and the handover covers engine updates.

## Profiles

Profiles are named sets of capabilities and settings, not hard-coded
modes: quantum and rate requests, which services run (DJ session,
analyzer, recorder, cue), DSP quality choices (resampler), routing presets
and the scheduling requests (RT priority when the system allows it).
Starting set: NORMAL, LOW LATENCY, DJ, STUDIO, STREAMING, GAMING. A
profile is applied by the service. It may ask PipeWire for a quantum and
rate, but it never forces system settings silently. If a profile needs
privileges it doesn't have (RT scheduling), it says so.

## Consequences

The Router and the quick surface become views of this graph. The ids are
the vocabulary for commands, presets, recordings and diagnostics.
