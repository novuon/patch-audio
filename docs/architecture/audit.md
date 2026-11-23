# Audit: Patch against the Omarchy audio environment architecture

The review below is the 2026-09-26 pass over v0.6.0 (`42780d3`). It stays
as the migration record. **What is true at workspace 0.7.7:**

* The DJ session lives in the engine, not in the UI. Conflict 1 is
  historical; the progress table marks it done.
* Meters travel in shared memory (`meters.shm`,
  `crates/patch-engine/src/shm.rs`), not as JSON on the control socket.
  [ADR-002](ADR-002-service-model.md) matches that.
* Deck render-ahead is `target_frames()`: 43 ms, or four graph cycles, at
  most 4,096 frames. [ADR-005](ADR-005-timing-model.md) matches that. The
  old "~32 ms" figure is retired.
* The workspace has 25 members. [The architecture README](README.md) lists
  them, including `patch-remote`, `patch-stems`, `patch-pattern`,
  `patch-material`, `patch-visual`, `patch-tune`, `patch-broadcast`,
  `patch-model`, `patch-mock`, `patch-build`, and the `app` binary.
* Source is MIT. A distributed binary links Rubber Band, FFTW
  (GPL-2.0-or-later) and Slint (GPL-3.0 option), so the binary is
  GPL-3.0. `NOTICE.md` and `packaging/arch/PKGBUILD`
  (`license=('MIT' 'GPL-3.0-only')`, `pkgver=0.7.7`) say so. Licensing was
  not changed; the package version tracks the workspace.
* Still open in the table below: item 3 (backend boundary), item 4
  (kernels not all behind the ABI), item 8 (`{"op":"graph"}` is not
  implemented).

## Original review (v0.6.0)

Date: 2026-09-26. Scope: the whole repository at `42780d3`.

## Already in place

| Requirement | Where | Notes |
|---|---|---|
| Patch is the app, Units are parts | `patch-core::units`, `spec.rs` | 14 Units; placeholders for planned ones |
| Engine outlives the UI | `patch-engine::daemon` (`omarchy-patch engine`) | detached process, own socket; UI supervises and restarts it |
| Engine updates without a gap | `client::upgrade` | measured ≤ 0.25 dB across a live handover |
| One DJ deck engine | `patch-engine::deck` | native decks run inside the engine; mpv only for radio streams |
| Validated commands for UI/CLI/keys/MIDI/AI | `patch-core::command` | AI origin has a ceiling (Prepare), can't run mixes, FX or Auto |
| Deterministic transition engine | `patch-ai` | no model, no network: tempo, key, energy, phrases → plan |
| Stamped timing | `patch-model::timing`, engine `MeterBlock` | per-cycle sample position + CLOCK_MONOTONIC; latency levels REPORTED/TRACKED/MEASURED |
| Measured output latency | `patch-engine::latency` | acoustic loop, per device |
| RT-safe data plane | engine `Rt`, `fx`, `deck::worker` | preallocated, atomics + SPSC rings; decoding at SCHED_IDLE |
| Tiered assembly with a Rust oracle | `patch-dsp` (dispatch), kernels in `patch-meter`, `patch-record`, `patch-engine`, `patch-dj` | `PATCH_DSP=reference|sse2|avx2`; the whole suite runs per tier |
| Benchmarks | `patch-bench`, `docs/bench.md` | engine cycle matrix + kernels, avg/p99/worst |
| Patch DJ format | `patch-library`, `docs/patch-dj-format` | ADR-001 |

## Conflicts and gaps (with the migration for each)

1. **DJ session logic runs in the UI process.** *(Resolved: the engine
   owns the session. The original gap:)* Crossfader position and
   law, deck fader law, auto-gain trims, the transition scheduler
   (`run_plan`) and the Auto Mix state machine live in `AudioApp`. Closing the
   UI keeps the decks playing but freezes a running transition and loses the
   crossfader state.
   *Migration:* move the DJ session into the service. The engine holds the
   crossfader, faders and trims and computes channel gains; the plan
   scheduler runs on the engine's control thread against the decks' stamped
   positions (more precise than the UI's extrapolation); the queue and
   auto-mix live there too. UI surfaces become views that send commands.
   The UI-side code is removed, not kept beside it.
2. **No Light DJ surface, no queue.** *Migration:* a queue in the service's
   DJ session; Light DJ is a surface of the DJ Booth plugin over the same
   state (ADR-003).
3. **No explicit backend boundary in code.** The engine calls PipeWire
   through the C shim and `pw-link`/`pactl` in `links.rs`.
   *Migration:* name the boundary (`Backend`: node creation, clock, device
   latency, links, device list) with PipeWire as its one implementation;
   no ALSA backend yet.
4. **Assembly lives beside its consumers.** Kernels are in `patch-meter`,
   `patch-record`, `patch-engine` and `patch-dj` (legacy), each with its own
   `x86_64.s`. `patch-dsp` only holds dispatch.
   *Migration:* move kernels into `patch-dsp` behind the kernel ABI
   (ADR-004): `reference/`, `x86_64/{sse2,avx2}/`, a dispatch table and a
   correctness/benchmark harness shared by all kernels. `omarchy-patch
   optimize` measures and persists the selection per installation.
5. **`patch-dj` is legacy.** Its parec → mix → pacat process was replaced
   by the engine. Only `crossfade` and `fader_db` (the laws) are still used.
   *Migration:* move the laws into the engine's DJ session; delete the rest.
6. **Timing positions are partly implicit.** We stamp graph positions and
   deck source positions, and we know REPORTED/MEASURED latency. The
   position chain (source → decoder → render → graph → output buffer →
   hardware → acoustic) isn't named in code.
   *Migration:* ADR-005 names each position and its uncertainty; the
   `Timing` shown to surfaces carries the level and an uncertainty.
7. **No audio profiles, no quick surface, no Omarchy integration
   (install, systemd user service, keybindings).** *Migration:* ADR-006
   (profiles as capability sets); a systemd user unit for the service; the
   quick surface is a later increment.
8. **Graph identity.** Engine channels have stable names (`bus`,
   `deck-a`, …), but there's no single inspectable graph of Patch nodes,
   buses, taps and connections. *Migration:* ADR-006.

## Progress (2026-09-27)

| # | Item | Status |
|---|---|---|
| 1 | DJ session out of the UI | **done**: crossfader, faders, auto-gain, EQ knobs, plan scheduler, queue and auto-mix live in the service; the UI mirrors it; a blend ran to completion with no UI process |
| 2 | Light DJ, queue | **done**: Light DJ (key `j`) over the same session; shared state shown live against the Booth |
| 3 | Backend boundary in code | open: the PipeWire calls are still direct (`links.rs`, C shim) |
| 4 | Kernels behind the ABI | **started**: ABI, registry, dispatch, harnesses and `optimize` in `patch-dsp`; the engine's mix and peak moved. The meter/analyzer and recorder kernels are still in their crates |
| 5 | Legacy `patch-dj` | **done**: removed; its laws live in the session |
| 6 | Timing positions | **done** in code (`patch_model::timing::Position`, `Presentation` with uncertainty) |
| 7 | Profiles, quick surface, service unit | service unit: `omarchy-patch service install` (not auto-installed); profiles and the quick surface open |
| 8 | Inspectable graph | open (`{"op":"graph"}` per ADR-006) |

## Real hot paths (measured, `docs/bench.md`, Core Ultra 7 265K)

* Engine cycle, 3 channels at 48 kHz / 512 frames: ~2 µs average on the
  AVX2 tier (10.7 ms budget, < 0.1 %). The gain-ramp mix and per-channel
  peak are the per-sample work. The EQ runs only when not flat.
* Analysis thread: K-weighting, true peak (4× polyphase) and the analyzer
  FFTs. These were the reason for the existing assembly.
* Deck worker: sinc varispeed and Rubber Band (in a library we don't
  replace).
* New since the bench: Beat FX (reverb is 16 combs + 8 all-passes per
  sample when on). It's a candidate for a kernel only if profiling shows it
  matters.

The first kernels to move behind the ABI are the ones already in
assembly with an oracle: mix (gain ramp), peak/energy stats, stereo sums.
The next candidates (interleave, waveform min/max, crossfade) only after a
profile shows them.
