# ADR-005: One timing model, with its uncertainty

Status: Accepted (graph stamps, deck stamps and latency levels implemented)

## Decision

All of Patch derives time from the service's graph cycles. Each cycle is
stamped with its **graph position** (samples since the clock started) and
the **monotonic time** of its first frame (`BlockStamp`: `sample_pos`,
`nsec`, `rate`, `frames`). No surface has its own timer for audio events.
Surfaces interpolate between stamps, or delay them to when they're heard.

A sound passes through these positions. Each is named in code and docs, and
none is faked:

| Position | Meaning | Source today | Certainty |
|---|---|---|---|
| source | frame in the file/track | deck transport (`pos`, source frames) | exact |
| decoder | frames decoded so far | `Pcm::ready` | exact |
| render | frames rendered ahead by the deck worker | worker ring, `target_frames()`: 43 ms, or four graph cycles, at most 4,096 frames | exact |
| graph | the cycle the engine processes | `BlockStamp` (+ deck position stamped per cycle) | exact |
| output buffer | when the device consumes that cycle | + one quantum | exact for the graph |
| hardware presentation | when the DAC/radio plays it | + device latency: PipeWire `Latency` (REPORTED) | driver's word |
| acoustic presentation | when it reaches the ear | + MEASURED (Analyzer acoustic loop), else REPORTED + trim | measured ± spread, or uncertain |

Latency figures carry their level: **REPORTED** (driver or PipeWire;
Bluetooth often under-reports), **TRACKED** (from stamp drift over time) and
**MEASURED** (acoustic loop, with its spread across sweeps). The best
available level is used, and the level is always shown with the number.
When nothing better than REPORTED exists for Bluetooth, the UI shows
REPORTED rather than presenting it as exact.

## Consumers

* Meter and Analyzer: blocks shown at `nsec + output latency` (OUTPUT
  reference) or at `nsec` (ENGINE reference).
* DJ Booth and Light DJ: deck positions come from stamped cycles; beat
  phase, sync and the plan scheduler use the same positions.
* Recorder: records `patch.program`, fed from the engine output in the same
  cycle; takes line up with the graph position.

## Consequences

A new feature that needs "now" uses the service's stamps. Anything that
can't be known (e.g. a Bluetooth sink's true presentation time without a
measurement) is represented as uncertain, not guessed.
