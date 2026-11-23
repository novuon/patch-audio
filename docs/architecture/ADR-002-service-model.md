# ADR-002: The Patch service and its frontends

Status: Accepted (the engine owns the DJ session; meters are shared memory)

## Decision

The audio of Patch runs in one long-lived process, the **Patch service**:
today `omarchy-patch engine`, a detached process with a Unix control
socket at `$XDG_RUNTIME_DIR/omarchy-patch/engine.sock`. It owns:

* the data plane: the PipeWire filter node, the mix, the decks, EQ, FX,
  meters, analyzer, recorder tap and cue output, on one graph clock;
* the control plane: links and device policy, the DJ session (crossfader,
  faders, queue, transition scheduling, auto-mix), latency measurement and
  engine handover.

Every other part of Patch is a **frontend** and may come and go: the Patch
UI and its Unit windows, Light DJ, the DJ Booth, the quick surface, the
CLI, MIDI/keyboard input, the phone remote and Auto Mix layer. Frontends send validated
commands (`patch-core::command`) and read state and stamped measurements.
No frontend is required for audio to keep running. Closing a window never
stops a Unit that carries audio (see "Units never cut live audio").

We keep the binary name `omarchy-patch` and call the process "the Patch
service" in documentation. We don't introduce a `patchd` name.

## IPC

Control uses JSON lines on the Unix socket: human-readable, cheap at
control rates, trivial for the CLI and tests. Meter blocks do not cross
that socket. The engine publishes them in a shared-memory seqlock file,
`meters.shm`, under the private runtime directory
(`crates/patch-engine/src/shm.rs`). Surfaces map the file and read the
newest block. Sample buffers never cross IPC: audio stays in the
engine's real-time path; recording reads the engine's program source
(`patch.program`) through PipeWire.

The phone remote (`crates/patch-remote`, binary `patch-remote`) is another
frontend. It binds `127.0.0.1:8124` only and is meant to sit behind
Tailscale Serve. It talks to the engine on the same Unix socket and does
not own audio.

The socket directory is mode 0700 and the socket itself is mode 0600.
A peer that can connect runs as the same user; that is the trust
boundary. The engine records the peer's pid and uid and does not ask
for a second credential. A request line is at most 16 MB and must
finish within 800 ms of its first byte, so a slow trickle cannot hold
a connection thread. An idle connection a window keeps open is not on
that clock. A client refuses a reply line longer than 1 MB, and that
read is one deadline for the whole line.

## Lifetime

* Started by the UI when missing (and restarted by the UI's supervisor if
  it dies, with a loopback fallback so the bus never goes silent).
* Replaced without a gap by `omarchy-patch engine upgrade`. The UI does
  this automatically for an older build, never while a deck plays.
* Stopped only explicitly (`omarchy-patch engine quit`, refused while
  audio flows unless forced).
* Next: a systemd user unit so the service can start with the session,
  independent of any window (Omarchy integration).

## Consequences

State that must survive the UI belongs in the service. The DJ session
(crossfader, faders, queue, plan scheduler, auto-mix) lives in the engine.
