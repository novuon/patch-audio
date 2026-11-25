# Patch architecture

Patch is the Omarchy audio environment: one application that owns audio
control, routing, DSP, timing, metering, recording and DJ performance on
the desktop, while PipeWire and WirePlumber stay the transport to
applications and hardware. Patch is the application. Mixer, Router, Meter,
Recorder, DJ Booth (and the planned Units) are its Units. Units are
functional parts of Patch. A surface is a window or frontend onto a Unit.

> Patch uses high-level languages to express architecture and semantics,
> while performance-critical kernels are replaceable by architecture-specific
> assembly or machine-generated implementations through a stable kernel ABI.

"Everything in assembly" is explicitly **not** a rule. A kernel gets an
optimised implementation only when it passes the acceptance gate in
[ADR-004](ADR-004-dsp-kernel-abi.md).

## Layers

```
 Applications (browsers, players, games, calls)
        │  PipeWire streams, routed by Patch policy
        ▼
 ┌──────────────────────── Patch ────────────────────────┐
 │  Frontends (any number, any time; none required)      │
 │    Patch UI · Unit surfaces · Light DJ · DJ Booth     │
 │    quick surface · CLI · MIDI/keys · phone remote     │
 │    AI (proposals)                                     │
 │        │ validated commands (patch-core::command)     │
 │        ▼                                              │
 │  Service: omarchy-patch engine (the "Patch service")  │
 │    control plane: graph, devices, policy, DJ session, │
 │      auto-mix, latency, analysis orchestration        │
 │    data plane (RT): mix, decks, EQ, FX, meters,       │
 │      analyzer, recorder tap, cue — one graph clock    │
 │        │ Backend boundary (I/O, clock, latency)       │
 └────────┼──────────────────────────────────────────────┘
          ▼
 PipeWire / WirePlumber (today's backend) → ALSA / Bluetooth / hardware
```

* **Service and UI lifetimes are separate** ([ADR-002](ADR-002-service-model.md)).
  The engine is a detached process with its own control socket. Windows can
  close and the UI can crash, restart or update, and the audio keeps
  playing. An engine update hands over without a gap
  (`omarchy-patch engine upgrade`).
* **One DJ engine** ([ADR-003](ADR-003-one-dj-engine.md)). Light DJ and the
  full DJ Booth are two surfaces on the same deck engine, library,
  analysis, cues, loops, grids, transport, sync, mixer, recorder and clock.
* **PipeWire is a backend, not the product.** The engine talks to it
  through a narrow boundary: node, ports, clock stamps, device latency and
  links. A direct-ALSA backend can be added behind the same boundary.
* **One timing model** ([ADR-005](ADR-005-timing-model.md)). Meters, DJ
  Booth, Light DJ, Recorder and visualisations all derive from the engine's
  stamped cycles, never from GUI timers. Meter blocks travel in shared
  memory (`meters.shm`, `crates/patch-engine/src/shm.rs`), not as JSON on
  the control socket.

## Where things live (crates)

The workspace (0.8.7) has these 27 members (`members` in the root `Cargo.toml`).

| Concern | Crate | Plane |
|---|---|---|
| Commands, validation, keymap, intents, MIDI parsing | `patch-core` | control |
| Domain model and provider traits | `patch-model` | control |
| Simulated providers | `patch-mock` | UI/test |
| MIDI/HID discovery, profiles, bounded input and monitor | `patch-controller` | engine-owned control worker |
| Engine: graph node, mix, decks, EQ, FX, sync, latency, handover, shared-memory meters | `patch-engine` | RT + control |
| Kernel dispatch (tiers), kernel ABI home | `patch-dsp` | RT |
| Meter and analyzer kernels (assembly + reference) | `patch-meter` | RT |
| Recorder conversion kernels and recorder process | `patch-record` | control/IO |
| Broadcast process | `patch-broadcast` | control/IO |
| Network Unit: audio to and from machines on the LAN (RTP with SAP, AES67-style L24, VBAN) | `patch-network` | control/IO |
| Track analysis (tempo, grid, key, loudness, energy, bands) | `patch-analysis` | offline |
| Background stem separation | `patch-stems` | offline |
| Deterministic transition engine (next track, mix points, plans) | `patch-ai` | control |
| Pattern unit (engine-owned, plays with windows closed) | `patch-pattern` | control |
| Native VST3 loading: the engine's master rack chain and the plugin windows' mirror instances | `patch-plugin` | RT + UI |
| Derived material cut from tracks the library already knows | `patch-material` | control/offline |
| Output-optimization profiles (data only) | `patch-tune` | control |
| Patch DJ format and library (SQLite, packages) | `patch-library` | control/IO |
| PipeWire/pactl integration, UI-side audio provider | `patch-pipewire` | control |
| Windows, sessions, layouts, Hyprland | `patch-host` | UI |
| Surfaces (Slint) | `patch-surfaces` | UI |
| Design-system Rust side | `patch-ui` | UI |
| Slint build helper | `patch-build` | build |
| Visualiser | `patch-visual` | UI |
| Phone and browser remote (`127.0.0.1:8124`, Tailscale Serve in front) | `patch-remote` | frontend |
| `omarchy-patch` binary | `app` | UI + service entry |
| Benchmarks | `patch-bench` | tooling |

## Real-time rules (data plane)

The engine's process callback, and every kernel it calls, must not:
do file, database or network I/O; allocate (buffers are sized at start,
for the highest supported rate); take locks other than wait-free atomics
and SPSC rings; log; call into the UI; sleep; or loop without a bound
known at build time. Control reaches the data plane through atomics and
preallocated rings, one writer and one reader each. Measurements leave it
the same way, stamped with their cycle. The control plane is everything
else: sockets, links, analysis, decoding (at idle priority), persistence.

## Documents

* [audit.md](audit.md): the state of the code against this architecture,
  conflicts and the migration plan.
* [ADR-001](ADR-001-patch-dj-format.md): the Patch DJ format is the source of truth.
* [ADR-002](ADR-002-service-model.md): service and UI lifetimes, IPC.
* [ADR-003](ADR-003-one-dj-engine.md): one DJ engine, Light DJ and DJ Booth as surfaces.
* [ADR-004](ADR-004-dsp-kernel-abi.md): DSP kernel ABI, CPU dispatch,
  assembly policy, verification, generated kernels.
* [ADR-005](ADR-005-timing-model.md): the timing model and its uncertainty.
* [ADR-006](ADR-006-audio-graph-profiles.md): the Patch audio graph and audio profiles.
* [ADR-008](ADR-008-ai-dj-visual.md): Auto Mix drawn as a decision field, driven by service state.

## Licensing

Source in this repository is MIT (`LICENSE`, `[workspace.package]` in
`Cargo.toml`). A binary built from this tree links Rubber Band and FFTW
(GPL-2.0-or-later) and Slint (taken under its GPL-3.0 option), so that
binary is a combined **GPL-3.0** work. `NOTICE.md` is the statement of
that. The Arch package (`packaging/arch/PKGBUILD`) records both:
`license=('MIT' 'GPL-3.0-only')`, and `pkgver` should follow the workspace
version. Those licence terms are unchanged.
