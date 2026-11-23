# Mixxx controller scripting: what it would take, and whether to build it

Patch has no scripting layer. This document is the investigation behind that
decision, measured against Mixxx's shipped controller scripts at commit
`44dc9ed7e8640b1d69b2d63f847d940b5ad688b1` — 152 `.js` files, 5.7 MB.

It exists because the mapping corpus forced the question. A declarative mapping
can express a fader, a knob, a button, an encoder and LED feedback, but not a
jog wheel:

| control entries | declarative | script-bound |
|---|---|---|
| jog / wheel movement | 12 | 325 |
| platter touch | 0 | 262 |

The 12 declarative movements are Mixxx's canonical `wheel` and `jog` names (2 and
10 entries); there is no declarative platter touch anywhere in the corpus. A
scratch needs a movement *and* a touch, so **no Mixxx mapping can be imported into
working platter control today**, and no amount of table work changes that: the
behaviour is in the JavaScript.

## What the scripts ask of their host

Every distinct `engine.*`, `script.*`, `midi.*`, `bpm.*` and `console.*` member
used, with the number of mapping files using it (76 members in total; a median of
10 per file; 6 files use no `engine.*` call at all):

| member | files | in Patch today |
|---|---|---|
| `engine.setValue` | 140 | canonical `Command` over `{"op":"command"}` |
| `engine.getValue` | 129 | engine state read |
| `midi.sendShortMsg` | 111 | feedback targets: note, CC, 14-bit, pitch bend |
| `engine.scratchDisable` | 98 | `dj.scratch_end` |
| `engine.scratchTick` | 98 | `dj.scratch` (the command exists; the *sequencing* is the script's) |
| `engine.scratchEnable` | 97 | `dj.scratch` |
| `engine.beginTimer` | 91 | **nothing** |
| `engine.stopTimer` | 77 | **nothing** |
| `engine.connectControl` | 69 | declarative feedback subscriptions; a script wants a callback |
| `engine.softTakeover` | 49 | native (`takeover.rs`) |
| `engine.setParameter` | 48 | **nothing** (no FX parameter commands) |
| `engine.isScratching` | 44 | engine state |
| `engine.makeConnection` | 41 | declarative feedback subscriptions |
| `script.toggleControl` | 33 | equivalent to a `dj.toggle` command |
| `midi.sendSysexMsg` | 33 | one-shot SysEx feedback target |
| `engine.trigger` | 24 | as `setValue` |
| `script.absoluteLin` / `absoluteNonLin` | 24 / 18 | a binding's `range` and scaling |
| `engine.softTakeoverIgnoreNextValue` | 23 | native |
| `script.deckFromGroup`, `script.triggerControl` | 23 / 21 | deck context, `dj.*` commands |
| `engine.getParameter` | 19 | **nothing** |
| `engine.brake` / `engine.spinback` | 18 / 9 | **nothing** (the platter has no brake/spinback) |
| `engine.getSetting` | 17 | settings read |
| `console.log` / `console.warn` | 14 / 5 | a service log line |
| `engine.makeUnbufferedConnection` | 12 | 30 ms feedback tick, already coalesced |
| `script.crossfaderCurve`, `bindConnections`, `channelRegEx`, `samplerRegEx`, `brake`, `pitch`, `posMod` | 10, 6, 6, 5, 5, 4, 4 | small pure helpers |
| `bpm.tapButton` | 9 | tap tempo |

Read that table as a cost estimate: **the primitives are mostly already here.**
Soft takeover, output, subscriptions, scratch and the command/state pair account
for the largest shares, and all five exist natively. What is missing is not
engine capability but a *runtime* — timers above all, since 91 of 152 scripts arm
one — and a narrow host API to bind it to.

## Options

**A. Embed QuickJS** (via `rquickjs`). QuickJS is MIT-licensed, as is the binding
crate, so it is licence-compatible with Patch. Small (~1 MB), fast, embeddable,
and its C dependency is build-time only. This is the smallest runtime that
executes the scripts people actually have.

**B. Embed Boa** (pure Rust JS, MIT). No C toolchain, but larger and slower, with
incomplete ES coverage; timers have to be provided either way, so the win over A
is mostly "no C".

**C. No JavaScript: grow the declarative format.** Rejected by the measurement.
Expressing 587 script-bound entries' logic declaratively means inventing a
general-purpose control language, and it would be a worse JavaScript with no
existing scripts to run.

**D. Nothing.** Keep hand-written profiles for controls that are declarative, and
accept that platter/scratch and layered controllers are out of reach. Honest, and
the current state.

Recommendation: **A, optional and off by default**, added when the work it
unblocks is being done rather than ahead of it — which means after HID or USB
bulk, since a jog wheel that cannot be read is not helped by a script that could
interpret it. It deserves its own decision because it would be the
`patch-controller` crate's first dependency beyond `patch-core`, `serde`,
`serde_json` and `libc`, and that crate's near-empty dependency set is what keeps
the service's boundary honest.

## Why the HID transport comes *after* this, not before

The obvious next build — a `/dev/hidraw` transport for vendor report protocols —
has no consumer without a runtime, and the corpus says so unambiguously. All
twenty HID and USB-bulk mappings Mixxx ships contain **zero `<control>` and zero
`<output>` elements**: each file is an `<info>` block naming vendor/product ids
and a `<scriptfiles>` list, and nothing else.

| mapping | controls | outputs | scripts |
|---|---|---|---|
| `Traktor_Kontrol_S2_Mk1.hid.xml` | 0 | 0 | 2 |
| `Traktor_Kontrol_S2_Mk2/MK3`, `S3`, `S4_MK2/MK3`, `F1`, `X1`, `Z1`, `MX2` | 0 each | 0 | 1–2 each |
| `Pioneer_CDJ_HID.hid.xml` (3 device ids) | 0 | 0 | 2 |
| `Hercules_DJ_Console_Mk1/Mk2/RMX`, `Control_MP3` | 0 each | 0 | 1 each |
| `Nintendo_Wiimote`, `Sony_SixxAxis`, `EKS_Otus`, `Dummy_Device_Screen` | 0 each | 0 | 1–2 each |
| `Traktor_Kontrol_S4_MK3.bulk.xml` | 0 | 0 | 1 |

A declarative report-parsing layer would therefore translate nothing for any real
HID device — the same trap as the declarative jog wheel, one level deeper. A
transport with nothing to drive it is exactly the "premature complexity for
hardware that is not yet supported" the project rules rule out.

Two consequences for the order of work:

1. **Scripting is the prerequisite for HID**, not a follow-on. Either the runtime
   comes first, or the two are designed together.
2. **A narrow protocol adapter is the cheap alternative for one specific device.**
   Device-specific report layouts may be implemented natively as a narrowly
   scoped adapter — the device's protocol facts, independently written, which is
   what the licensing analysis permits and what §5 asks for. For one controller
   someone actually owns (a Traktor Kontrol S2 Mk1, say) that is a few hundred
   lines against a 71 KB script, and it is the honest way to make *that* device
   work without pretending to have built the general solution. The cost is that
   it serves one device and must be labelled as such.

## The isolation contract any runtime must meet

These are requirements, not preferences; a runtime that cannot meet them should
not be added.

* **No ambient authority.** No filesystem, network, process, environment or clock
  beyond a monotonic timer. One module per profile, no `require`/`import`, no
  dynamic evaluation beyond what the host hands over.
* **A bounded, capability-checked host API.** A script may only address the decks
  and groups its profile declares; it cannot name a deck the profile does not
  cover, and cannot reach engine internals.
* **A per-call budget** (instructions or wall clock) and caps on total callbacks,
  live timers and timer rate. A script that exceeds them is stopped, its reason
  reported in the device monitor, and the device keeps working without it.
* **Its own thread.** Timers, callbacks and evaluation run on the service's
  control-plane thread. Nothing here may run in the audio callback, and a slow
  script must not delay device reads — reads are nonblocking and bounded today
  and must stay that way.
* **Feedback loop prevention.** A value a script sets must not come back to it as
  its own subscription event; this is the same guard the declarative feedback path
  already carries.
* **Deterministic testability.** The host calls a script makes must be recordable
  and replayable against fixtures, so a controller can be tested with no hardware
  and no live engine — the same technique the FIFO-backed device tests use.
* **Failure containment.** A script that throws, loops, allocates without bound or
  floods its output is disabled with a reason; the rest of the device and every
  other device is unaffected.

## Deliberately not planned

* Porting Mixxx's script helper library (`common-controller-scripts.js` and
  friends) into Patch. Those files are GPL and copying them would be a licence
  error; reimplementing the handful of pure helpers independently is cheap, and
  that is the only acceptable route. See `docs/controllers-licensing.md` §4 for
  the part that is settled: *executing* a script the user already has is not
  conveying a copy, and providing a compatible API is describing an interface.
* Emulating Mixxx's control-object namespace wholesale (`[Channel1]`,
  `[EffectRack1_EffectUnit1]`, …). Patch's canonical commands are the vocabulary;
  a compatibility layer should translate the handful of groups its deck model
  has, and refuse the rest by name rather than inventing state.
* Any suggestion that scripts exist yet. None of this is implemented, and no
  controller should be described as supported on the strength of this document.
