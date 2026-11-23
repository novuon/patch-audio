# Controller Unit (MIDI & HID)

`patch-controller` is the controller service: hardware in, canonical Patch
commands out, authoritative Patch state back to the hardware. It depends only
on `patch-core`, serialization and Linux libc — no audio backend, no UI, no
DSP. Closing every window leaves controller input running, and a controller's
play light follows a deck that was started by the DJ Booth, the Auto Mix, a remote
page or the controller itself.

The Controller **unit** is what a person looks at; the service above is where
the logic lives. The unit is not a container for backend code, and the service
is not a unit.

**To set up a controller, read `docs/controllers-workflow.md`** — plug in, import a
mapping, check that it will match, and what to do when it does not. This document is
the reference behind it: the vocabulary, the measurements, and what is deliberately
not implemented.

The unit is labelled **"MIDI & HID"** (`UnitKind::Controller`, window "MIDI &
HID", key `h`) and its surface id is `controllers`. The name matters: the
*session* controller — the window `Ctrl-K` opens, titled "Patch · Controller",
which lists the units and holds Patch's own configuration — is a different
thing, and two units under the name "Controller" is a trap. The CLI follows the
same split: `omarchy-patch controller` opens the session controller,
`omarchy-patch controllers …` talks to this unit.

```
MIDI / evdev bytes
  -> identity        what the device is, stably            (identity.rs)
  -> profile         which mapping, on what evidence       (profile.rs)
  -> mapping         control -> canonical Command          (patch-core input.rs)
  -> service         validation, human takeover, execution (patch-engine)
  -> authoritative state
  -> feedback        LED / ring / display output           (feedback.rs)
```

Nothing in that path decides what a control means: a mapping does. Nothing owns
mixer, deck or AI state: the service does, and both the input path and the
feedback path read that same state.

## Open it

```sh
omarchy-patch open controllers          # the Controller unit
omarchy-patch controllers status        # devices, profiles, monitor (IPC)
omarchy-patch controllers reload        # re-read the profile file and reopen
```

The unit shows each device with its transport, its identity strength, whether a
profile matched and how it was chosen, and — for devices that have outputs —
how many messages have been sent and dropped. Below that is a monitor of the
newest events: what the control was, what it ran, or the trigger to paste into a
binding. A refused command, a device that will not open, a keyboard that was
skipped, and a control waiting to take over all appear there rather than
silently doing nothing.

## Profiles

`$XDG_CONFIG_HOME/omarchy-patch/controllers.json`, normally
`~/.config/omarchy-patch/controllers.json`. Save the file, then **Reload
profiles** in the unit or `omarchy-patch controllers reload`. A parse or
validation failure keeps the previous configuration and reports the error.
Reload while controls are released: readers are closed and reopened and stream
parsers are reset.

JSON is the format because it is already Patch's convention for every
user-editable file (`dj-keys.json`, `controllers.json`, `eq.json`) and
`serde_json` is already a dependency of this crate. TOML would mean two formats
for controller data; YAML would add a dependency for nothing this shape needs.

A profile must say how to recognise its hardware. The oldest two-field form
still works:

```json
{
  "profiles": [
    {
      "device": "/dev/snd/midiC2D0",
      "keymap": {
        "bindings": [
          {
            "context": "midi",
            "on": {"kind": "note", "channel": 0, "note": 36},
            "action": {"op": "dj.transport", "deck": "a", "action": "toggle"}
          }
        ]
      }
    }
  ]
}
```

The full form adds metadata, matching criteria and feedback:

```json
{
  "hid_devices": ["/dev/input/by-id/usb-Studio_Pad-event-joystick"],
  "profiles": [
    {
      "id": "allen-heath-xone-k2",
      "name": "Xone K2",
      "manufacturer": "Allen & Heath",
      "protocol": "midi",
      "version": 1,
      "match": {
        "names": ["XONE:K2"],
        "vendor_id": 4668,
        "product_id": 1,
        "card_id": "XONEK2"
      },
      "keymap": {"bindings": []},
      "outputs": []
    }
  ]
}
```

`protocol` is `any` (the default), `midi` or `hid`; a MIDI profile never matches
a HID device. `version` is checked against the format Patch understands, so a
profile written for a newer Patch is refused rather than half-applied. Unknown
fields are refused, so a typo (`output` for `outputs`) is reported instead of
silently ignored. Each profile exclusively supplies its device's map, with the
first matching binding winning; a device with no profile uses the legacy
`dj-keys.json` map in the matching context. Keyboard handling is unchanged, and
keyboard bindings belong in `dj-keys.json`, not here.

### Controls

Each binding listens for one control and produces one canonical command.
`context` is `midi`, `hid` or empty (any); `action` is command JSON whose
`"{value}"` and `"{deck}"` placeholders are filled in. Channels are zero-based
in profiles and one-based in the monitor.

```json
{
  "context": "midi",
  "on": {"kind": "cc", "channel": 0, "cc": 7},
  "action": {"op": "dj.fader", "deck": "a", "position": "{value}"},
  "range": [0, 1]
}
```

| `on.kind`    | fields                                       | value                                |
|--------------|----------------------------------------------|--------------------------------------|
| `note`       | `channel`, `note`, optional `release`         | none (momentary)                     |
| `cc`         | `channel`, `cc`, optional `relative`, `step`  | 0…127 scaled into `range`            |
| `cc14`       | `channel`, `cc` (the MSB; LSB is `cc + 32`)   | 0…16383 scaled into `range`          |
| `pitch_bend` | `channel`                                    | centred on 0, `range` default `[-1, 1]` |
| `program`    | `channel`, `program`                         | none                                 |
| `hidbutton`  | `code`, optional `release`                    | none (momentary)                     |
| `hidrelative`| `code`, `step`                               | movement × `step`                    |
| `hidaxis`    | `code`, `min`, `max`                         | the axis scaled into `range`         |

`range` defaults to `[0, 1]`. A reversed range mirrors a control mounted the
other way round. HID absolute axes require their actual device limits; values
are clamped to those limits before scaling. Relative values multiply `step`, so
a step command receives a delta rather than an absolute position.

A stem knob works like an EQ knob: `"parameter": "knob"` with the range −1 … 1
(fully left mutes the stem, the centre is its own level with a small detent,
fully right doubles it). One binding per stem (`drums`, `bass`, `other`,
`vocals`) and deck:

```json
{
  "context": "midi",
  "on": {"kind": "cc", "channel": 0, "cc": 20},
  "action": {"op": "dj.stem", "deck": "a", "stem": "vocals",
             "value": {"parameter": "knob", "value": "{value}"}},
  "range": [-1, 1]
}
```

Turning one hands that stem to the DJ: Auto Mix's stem moves on it stop until
**Auto** on the deck's stem row gives it back. Mute and solo take
`{"parameter": "mute", "value": true}` (or `solo`) on a note.

### 14-bit controls

A pair is declared once, on the MSB's number, and the reader assembles it from
the two messages:

```json
{
  "context": "midi",
  "on": {"kind": "cc14", "channel": 0, "cc": 16},
  "action": {"op": "dj.jump", "deck": "a", "beats": "{value}"},
  "range": [-16, 16]
}
```

Only declared pairs are assembled. Auto-pairing every CC with `cc + 32` would
invent values for the many controllers that use both numbers as independent
controls. Declaring a pair takes over that CC and its LSB, so do not also bind
the LSB separately. `cc` must be 0…111 so the LSB stays inside the 7-bit field.
The pair is held until both halves arrive, and the monitor shows the high byte
as held rather than as a command.

### Relative encoders

A hardware encoder reports *movement*, not position. Three decodings cover the
hardware in the field, and Mixxx's seven option names map onto them:

| Patch `relative`  | Mixxx option names               | reading                          |
|-------------------|----------------------------------|----------------------------------|
| `twos_complement` | `diff`, `selectknob`, `hercjog`   | 0…63 up, 64…127 down             |
| `offset_binary`   | `rot64`, `rot64fast`, `spread64`  | 64 is the detent, ± either side  |
| `signed_bit`      | (not a Mixxx MIDI option)         | bit 6 is the sign, bits 0-5 the amount |

Mixxx accumulates these into an absolute value because its control objects are
absolute. Patch's canonical commands have explicit relative forms (`dj.bend`,
`dj.crossfader_step`, `dj.jump`), so a binding yields the movement and `step`
carries any multiplier — Mixxx's `hercjogfast` is
`{"relative": "twos_complement", "step": 3}`.

```json
{
  "context": "deck",
  "on": {"kind": "cc", "channel": 0, "cc": 33,
         "relative": "twos_complement", "step": 0.01},
  "action": {"op": "dj.bend", "deck": "{deck}", "amount": "{value}"}
}
```

A relative control that reports no movement dispatches nothing, so a jog wheel
at rest produces no traffic.

### Soft takeover

A fader parked at zero must not slam the channel to silence the first time it is
touched. `soft_takeover` names the authoritative state the control drives; the
control is armed with the value Patch is already on and stays inert until the
hardware reaches it.

```json
{
  "context": "midi",
  "on": {"kind": "cc", "channel": 0, "cc": 7},
  "action": {"op": "dj.fader", "deck": "a", "position": "{value}"},
  "range": [0, 1],
  "soft_takeover": true,
  "source": "deck.a.fader"
}
```

`source` uses the same vocabulary as an output (below), so there is one way to
name a piece of state. If something else then moves that value — the Auto Mix, the
UI, a remote page — the control is re-armed and has to catch up again; a change
this controller itself caused is not treated as external. A takeover control
whose state Patch cannot report stays live, because arming against its own first
value would be a guess rather than a takeover. A relative encoder cannot
soft-take-over. While a control is waiting, the monitor says so.

### Feedback

`outputs` drive LEDs, rings, ladders and displays from authoritative state. Each
output follows one source and writes one hardware target:

```json
{
  "outputs": [
    {"target": {"kind": "note", "channel": 0, "note": 11,
                "on_value": 127, "off_value": 0},
     "source": "deck.a.play"},
    {"target": {"kind": "cc", "channel": 0, "cc": 20, "max": 127},
     "source": "deck.a.vu"},
    {"target": {"kind": "cc14", "channel": 0, "cc": 16},
     "source": "deck.a.progress"},
    {"target": {"kind": "pitch_bend", "channel": 0},
     "source": "deck.a.tempo", "range": [-16, 16]},
    {"target": {"kind": "sysex", "payload": [126, 0, 6, 1]},
     "source": "deck.a.loaded"}
  ]
}
```

| `source`                                  | value                                   |
|-------------------------------------------|-----------------------------------------|
| `crossfader`                              | 0…1                                     |
| `master.vu`                               | pre-master level, 0…1                    |
| `deck.<a\|b\|c\|d>.fader`                         | channel fader, 0…1                       |
| `deck.<a\|b\|c\|d>.loaded` / `.play` / `.pfl`       | on/off                                  |
| `deck.<a\|b\|c\|d>.loop` / `.keylock` / `.slip` / `.quantize` | on/off                        |
| `deck.<a\|b\|c\|d>.tempo`                         | percent, as the tempo fader commands it  |
| `deck.<a\|b\|c\|d>.progress`                      | playhead position, 0…1                   |
| `deck.<a\|b\|c\|d>.vu`                            | deck level, 0…1                          |
| `deck.<a\|b\|c\|d>.cue1 … cue8`                     | 1 when that hot cue is set, 0 when not   |
| `deck.<a\|b\|c\|d>.cue`                            | 1 when the main cue is set               |
| `deck.<a\|b\|c\|d>.beatloop.<1\|2\|4\|8\|16\|32>`    | 1 when the loop of that length is on     |
| `deck.<a\|b\|c\|d>.kill.<low\|mid\|high>`          | 1 when that EQ band is killed            |

A `note` target is a two-state indicator. `cc` and `cc14` take the source mapped
over the target's full range (`max` limits a `cc`). `pitch_bend` is symmetric
about its centre. `sysex` is a fixed sequence sent once per connection, in
declaration order — put an initialisation handshake first and it goes first.
`range` maps the source's own span onto the target, so a `deck.<x>.tempo` output
needs one (e.g. `[-16, 16]`); a reversed range mirrors it. `min_interval_ms`
overrides the default rate for a level output.

The snapshot publishes **every deck the engine runs**, up to four, so a four-deck
controller's C and D indicators follow real state; a source naming a deck the
audio graph does not have resolves to nothing rather than silently to deck A,
which is why the count is read from the graph instead of assumed.

The cue, loop and kill indicators are all read from state the deck already holds,
which is why binding them needed no new bookkeeping: the main cue and the hot
cues come from the cue table, a kill is the band whose own gain is null — the
same value `dj.eq` writes when it is given no dB — and the loop's length in beats
is *derived*, because Patch stores a loop as a sample range rather than a length.
A loop that is not a whole number of beats (a hand-placed in/out pair) therefore
lights none of the length indicators, which is the honest answer, and a deck with
no grid reports no length at all.

**Why feedback cannot loop.** Three independent guards, each tested:

1. Direction — output bytes are written to the device and never fed back into
   the input parser.
2. Change detection — a value equal to the last one sent produces no bytes at
   all, so a 30 ms poll does not become thirty messages a second per light.
3. Reflection suppression — when a control's new value arrived from the hardware
   itself, the device already knows it, so the next emission for exactly that
   source is dropped once.

Level outputs are additionally rate-limited and coalesced: the newest value lands
when the interval allows and older pending values are discarded rather than
queued, so a fast jog cannot build a backlog. A replug re-sends every output
once, in declaration order, so the visible state is restored.

## Automatic recognition

A device is offered to the profiles and the best available evidence wins:

| Evidence      | Criterion                                            |
|---------------|------------------------------------------------------|
| `path`        | the exact device path, or the evdev node             |
| `hardware id` | vendor/product, serial, card id or interface number  |
| `name`        | a case-insensitive substring of the device name      |

Every criterion a profile states must hold: a profile naming a vendor the device
is not can never match, however similar the name looks. If one profile has the
best evidence it is loaded. If several tie, **Patch reports them and loads
none** — a wrong preset must not be able to move a fader nobody touched. A
device with no matching profile is still usable, and the unit distinguishes it
from a device that was refused.

This is where Patch improves on Mixxx rather than copying it: Mixxx parses
mapping headers but does **not** match MIDI devices automatically —
`MidiController::matchMapping()` returns false with a "not implemented for MIDI
devices yet" comment
(`src/controllers/midi/midicontroller.cpp`), and the parsed product info only
preselects an entry in the Preferences combo box.

## Device identity

A path is not an identity: ALSA renumbers cards across reboots and a USB device
can move bus. Identity comes from sysfs, strongest first — USB vendor/product
plus serial, then vendor/product plus interface, then the ALSA card's stable
`id`, then the path. The unit shows which rung was used, because a match resting
on a path is a weaker claim than one resting on a serial. A profile that must
find one specific unit among two identical models should therefore state the
serial or the interface, not only a name.

HID is opt-in and safe. A path is opened only if it is an event device that is
neither a keyboard (it reports the letter keys) nor a pointing device (relative
axes plus pointer buttons); a path that is refused is listed with its reason and
never retried. Naming a path explicitly in `hid_devices` is the one case where
the intent is unambiguous, so it is honoured. HID uses the Linux
[evdev event protocol](https://www.kernel.org/doc/html/latest/input/event-codes.html)
for buttons, relative controls and absolute axes; listed paths are opened
read-only, without an exclusive grab, and keyboard repeat events are ignored.
Device permissions remain the desktop session's responsibility, and open
failures are visible in the unit.

## Bounds and operational limits

Input dispatch is independent of GUI timers. `poll` wakes on readable input with
a 5 ms timeout for host lifecycle checks. Poll descriptors and read buffers are
fixed-size; at most 32 devices, four 256-byte reads per device per iteration,
1,024 bindings per profile, 256 outputs per profile, 32 monitor entries, 1,024
takeover entries and 32 feedback messages per poll. There is no application
input queue that can grow without a bound.

Bindings are indexed by control identity at profile load, preserving context,
order and release semantics. Snapshots publish at 10 Hz; visible Controller
surfaces read at most four times a second. Hidden and absent Controller surfaces
make no status requests.

Parsing does not allocate; mapping, dispatch and feedback are control-plane
operations and can allocate or acquire service locks. They never run in the
audio callback — the controller worker is its own thread. Slow commands can
delay subsequent controller events. Writes are nonblocking and a full device
buffer drops the message and counts it rather than stalling the worker, so a
slow or half-dead device cannot block input. These bounds are architectural
safeguards, not a measured latency guarantee.

Event times are host monotonic observation times, not audio-frame stamps.
`dispatch_us` measures mapping-result command execution and monitor-label work,
not hardware-to-audio latency; no latency figure is claimed here because none
has been measured.

On HID `SYN_DROPPED`, dispatch for that device stops until reconnect or profile
reload and the monitor reports an overrun: held controls cannot be safely
reconstructed without querying kernel state, so stale transitions are never
dispatched. State resynchronisation without a replug, and automatic release of
held controls on unplug, are not implemented. Engine retirement closes readers
and checks retirement before dispatch; rollback reopens readers. There is no
exactly-once input guarantee during engine handover.

## Verification

`cargo test -p patch-controller -p patch-core` covers MIDI stream fragments,
running status, real-time bytes inside a message, pitch bend, program change,
14-bit assembly from two halves, every relative encoding, HID repeat/release and
overrun handling, axis calibration, mapped values and ranges, soft takeover from
both directions and its re-arm on an external change, feedback change detection,
rate limiting and coalescing, the per-poll bound, reflection suppression,
re-send after reconnect, profile parsing and validation, identity matching and
ambiguity, refusal of keyboards and mistyped paths, duplicate profiles, device
misidentification, command refusal, monitor bounds, nonblocking FIFO reads,
disconnects and descriptor shutdown.

Every device test is a FIFO or a unit fixture. **No physical controller has been
used to validate any of this**, and none of it should be described as
hardware-tested. Hardware latency and real controller compatibility remain an
acceptance step.

Running the whole workspace suite serially (`cargo test --release --workspace
-- --test-threads=1`, as the project's shared workflow asks, to avoid competing
deadline tests) passes 981 tests with 25 ignored and none failing; strict
workspace Clippy is clean at the same commit.

The importer has a corpus test, which is skipped unless you point it at a
directory of Mixxx mappings:

```sh
PATCH_MIXXX_CORPUS=~/mixxx-controllers cargo test --release -p patch-controller -- --nocapture
```

It holds the importer to invariants that must not break on any mapping file:
every imported profile is one Patch accepts by its own validation, no command
retains a `{deck}` placeholder, and every control and output entry in the file is
accounted for exactly once. Running it over Mixxx's 164 mappings produces the
figures in the section above.

The CPU-only benchmark (`cargo bench -p patch-controller --bench input`)
compares the legacy linear keymap with the indexed resolver at 1,024 bindings
and measures MIDI parsing. It excludes hardware, service dispatch, contention
and audio-cycle latency; the worst batch average is not per-event tail latency.

The Controller unit uses the existing bounded asynchronous IPC poller. Neither
telemetry reads nor profile reload waits on the drawing thread. Closing or
hiding the unit stops its telemetry requests, while the engine retains hardware
input ownership. GUI-only commands are refused by the engine's semantic command
executor and shown as refusals in the monitor.

## Importing a Mixxx mapping

Patch ships no Mixxx mapping files. It can, however, read one you obtained
yourself and translate the declarative part of it into a native profile:

```sh
omarchy-patch controllers import ~/mixxx-controllers/Numark-Mixtrack-Pro.midi.xml
omarchy-patch controllers import <file> --write     # append it to controllers.json
```

Without `--write` the profile goes to standard output for you to inspect or
paste; with it, the profile is appended to `controllers.json` and the whole file
is validated before anything is written (staged and renamed, so a failure cannot
leave a half-written list behind). A profile whose id is already present is
refused rather than overwritten. `--write` never installs anything else: no
script, no asset, and no Mixxx file is copied into Patch.

Every run prints what it could **not** translate, by name and reason, and every
file's entries are accounted for exactly: translated, script-bound, merged into
a 14-bit pair, or refused with a reason. The measured result over Mixxx's whole
shipped corpus at commit `44dc9ed7` — 164 mappings, 17,295 control entries:

| | entries |
|---|---|
| translated into Patch bindings | 3,274 |
| script-bound (Mixxx hands them to JavaScript) | 11,588 (67 %) |
| no Patch equivalent yet (reason reported) | 2,372 |
| low bytes merged into their 14-bit pair | 61 |
| outputs: translated / no state to follow | 1,392 / 926 |

That is the honest picture of what a declarative translation can reach: **two
thirds of Mixxx's control entries are JavaScript**, which is why the importer
reports them as a category rather than pretending to convert them, and why
2,940 entries name a control object Patch has no command for yet. Of the 5,707
entries that do bind a control object directly, 2,706 translate — the rest are
mostly samplers, effect parameters, library browsing and `pregain`, each with its
own reason in the report.

The entries that moved from "no equivalent" to translated across these
measurements are hot cues, loop pads, manual loop points and reverse:
`hotcue_N_activate` and `hotcue_N_clear` map onto `dj.hot_cue`; `beatloop_N`,
`beatloop_N_toggle` and `beatloop_N_activate` all mean "a loop of N beats,
toggled" and map onto `dj.loop_toggle`; `loop_in`/`loop_out` map onto two new
commands over the engine's own pending in-point; and `reverse` maps onto a new
`dj.reverse` whose release ends it. Each needed the engine to learn the command
before the importer could emit it. Then the cue limit itself: Patch held four hot
cues per deck and the corpus names cues 1 to 8, so all 142 entries above four were
refused for that reason alone. The limit is now eight — in the command, in the
library and in the DJ Booth's storage, which are the three places that decide it —
because eight is also what DJ hardware provides. Still refused, each for a stated reason:
`hotcue_N_set` and `hotcue_N_goto` (Patch's command jumps to a stored cue or sets
an empty one, so it can neither overwrite a cue nor promise not to set one),
`beatlooproll_*` (a loop that exists only while the pad is held — Patch has no
roll), the unsized `beatloop_activate` (Mixxx re-uses the size chosen last),
`loop_in_goto`/`loop_out_goto` (a mapping cannot name a loop point to seek to),
`loop_enabled` (no separate enable — a loop is set or left) and `loop_scale` (a
knob whose multiplier range is the mapping's own).

Kill switches are mapped too: `filterLowKill`/`filterMidKill`/`filterHighKill` on
a deck become the EQ command with `db: null`, which is Patch's own word for
silence, and the release lifts the band to unity. No floor is invented, because
the command already carries the kill.

**What is left is mostly a feature gap, not a mapping gap.** Running the importer
with `PATCH_MIXXX_REFUSALS=1` ranks the refusals by control-object family, and the
top of that list says what Patch would have to *gain* rather than translate:

| family | entries | what it would need |
|---|---|---|
| effect parameters and enable | 548 | effect-unit slots and per-parameter state Patch does not keep |
| sampler transport | 226 | a sampler (the groups are `[SamplerN]`, not decks) |
| library selection load/eject | 213 | the booth's selection, which a controller cannot see |
| `pregain`, master `volume` | 198 | a normalised-knob-to-dB curve, which the mapping does not state |
| headphone mix | 88 | a monitor-mix control; Patch cues per deck instead |

That ranking is how the next target gets chosen — by count, not by guess — and it
is why the work above went in the order it did.

An imported profile is a starting point and is labelled as such in its own
`attribution` field, which preserves the mapping file's name, author and forum
link. Patch does not claim authorship of it, and no converted mapping is
described here as working, because none has been tried on hardware.

## Mixxx compatibility and licensing

`docs/controllers-mixxx-research.md` documents how Mixxx discovers devices,
parses mappings, runs JavaScript and drives LEDs, with source references at
commit `44dc9ed7`; `docs/controllers-licensing.md` analyses what an MIT project
may reuse. In short: Patch's native profiles are the authoritative format, no
Mixxx XML, JavaScript or C++ is bundled or copied, and Mixxx's option names are
mapped into Patch's own vocabulary so a user can convert a mapping file they
obtained themselves. No converted mapping is named as supported here, because
none has been tested.

Where a controller genuinely needs scripting, Patch has no scripting layer, and
none has been added speculatively. That decision now has a measurement behind it
rather than an assumption, and the measurement says scripting is not the edge
case — it is the main road. Across Mixxx's mappings: **325 of 337 jog/wheel
movement entries and 262 of 262 platter touch entries are script-bound**, leaving
12 declarative movements (the canonical `wheel`/`jog` names) and no declarative
touch at all. So no imported mapping can drive a real jog wheel, because a
scratch needs a movement *and* a touch, and the pair only exists in JavaScript.
`docs/controllers-scripting.md` sets out the surface those scripts need and what
a runtime would cost.

## Which commands work from hardware

A canonical command is only as universal as its executors. Patch has two: the
engine's daemon (which serves hardware, the CLI, Auto Mix and remote pages) and the
surface host (which serves the DJ Booth's own keys and pads). Commands implemented
in the surface alone are refused with `not a service command` when they arrive
from a controller, while the same action from the booth works — the invariant this
work exists to hold, and it was broken for a third of the vocabulary.

**The signal, and how to keep it.** The engine's command executor no longer ends
in a catch-all. Every command it cannot run is listed by name with the state it
would need, which makes that `match` exhaustive: adding a `Command` variant to
`patch-core` now fails to compile until someone decides here whether the engine can
run it. Coverage is therefore checked by the compiler rather than by a grep — and
the first pass at this was a grep, which reported a dozen false positives because
one match arm's patterns span several lines (`C::A { .. } | C::B { .. } =>`). The
same mistake is available in `matches!` lists, which mention variants without
implementing them.

That compiler pass found **20 of the 56 commands then in the enum** with no engine
path. Three commands have since been added to the vocabulary for hardware's sake
(`dj.loop_in`, `dj.loop_out`, `dj.reverse`), so what remains is **12 of 59**. Of the
20, these now have an engine path, each over state the engine already holds and
mirroring the booth's semantics:

| command | engine path |
|---|---|
| `dj.hot_cue` | the library's cue table (verified by test, not yet on hardware) |
| `dj.tempo`, `dj.tempo_step` | the transport's tempo — percent converted to a fraction, the step clamped to the widest fader range (±50 %), since the fader's ±8/±16 is the booth's model |
| `dj.eq` | the channel's EQ, over the same `low_db`/`mid_db`/`high_db` the EQ op takes |
| `dj.pfl` | the monitor mix, whose op already existed |
| `dj.bend` | the transport's bend rate |
| `mixer.set_gain` | master and the engine's channel names — before this, a fader bound to it did nothing at all |
| `dj.crossfader_step` | the session's own xfader, read and written back through the op an absolute move uses |
| `dj.load_track` | the library itself, over a matching rule shared with the booth (`patch_library::title_matches`), then the same load path the UI uses with its load lock and grid |
| `dj.loop_in`, `dj.loop_out`, `dj.reverse` | **new commands**, added because 225 corpus entries name exactly these and the engine already had the ops: a manual loop is the engine's own pending in-point plus an out-point that engages it, and reverse ends on a release |
| `dj.loop_toggle`, `dj.loop_scale`, `dj.keylock`, `dj.key_step`, `dj.quantize`, `dj.slip`, `dj.toggle` | the transport's own flags and loop, with the loop toggle exiting only the loop running at that length and the key nudge clamped at twelve semitones |

**Twelve still have no engine path**, and each says why when asked:

| command | why |
|---|---|
| `mixer.connect`, `mixer.route`, `mixer.restore` | the audio graph belongs to the host's routing provider, not the engine's session |
| `mixer.set_mute` | the command names a *mixer strip*, and resolving a strip name to an engine channel is the host's strip graph |
| `dj.tempo_range` | it changes what the tempo fader means (±8/16/50 %), which is the booth's fader model |
| `dj.plan`, `dj.plan_cancel`, `dj.stop`, `dj.start` | a plan is built, held and ticked by the DJ Booth; the engine only ever sees the commands a plan emits |
| `dj.ai` | the Auto Mix's suggest and hand-over cycle is orchestrated by the booth, which holds the plan and the selection |
| `record.start`, `record.stop` | the recorder is its own process with its own socket, and the surface starts it |

`dj.load_track` was the one that mattered most for hardware — the load button — and
it is done: the *rule* for what a query means is shared with the booth so a pad and
a click cannot land on different tracks, while the candidates differ by design (the
booth searches the list it is showing, the engine searches the whole library,
because it has no view). An empty query matches nothing rather than everything,
which `contains("")` would have done.

## Not implemented

* A HID transport for vendor report protocols. Patch reads HID through Linux
  evdev codes; a device that speaks its own report format over `/dev/hidraw`
  (the Traktor Kontrol S2 Mk1 is one — Mixxx drives it with a 71 KB script and a
  packet parser, and ships no MIDI mapping for it at all) cannot be read or
  written yet. It is no longer *invisible*: every HID device the kernel lists is
  enumerated from `/sys/class/hidraw` (sysfs only, never opened), grouped so one
  piece of hardware is one entry however many interfaces it exposes, and shown in
  the device list with its vendor/product identity and the state
  `detected · HID device Patch cannot read yet`.

  That list is deliberately **unfiltered**, because Linux cannot classify these
  devices and the measurements say so: on a test machine the keyboard and the
  mouse both report USB interface protocol 0 ("generic HID"), a keyboard's key
  bitmap is not reachable from its own hidraw node, and the leading HID usage
  page is vendor-defined `0xff00` for a keyboard, a USB receiver and a pair of
  headphones alike. A filter built on any of those would hide a real controller
  before it hid a keyboard, so a headset or an RGB controller appearing in the
  list is the acceptable side of that trade — every entry carries the same honest
  reason, and the human recognises their own hardware by name. The refusal that
  matters, never *opening* a keyboard or a mouse, is on the evdev path, where the
  kernel reports real capabilities.

  Nor can this transport be built usefully before the script runtime:
  all twenty HID and USB-bulk mappings Mixxx ships contain zero declarative
  controls — they are device ids plus script files — so a report parser driven by
  mapping data would translate nothing for any real HID controller.
  `docs/controllers-scripting.md` has the measurement and the order of work it
  implies, including the narrow per-device adapter as the cheap alternative.

* Vinyl-mode scratching from an imported mapping. The canonical commands exist
  (`dj.scratch`, `dj.scratch_end`), the engine glides the platter toward the
  hand, and the importer emits the pair when a mapping has both a declarative
  jog and a declarative platter touch in the same deck group. On Mixxx's corpus
  that condition never holds — see the measurement above — so in practice a real
  controller's jog still needs the scripting layer. The DJ Booth's own
  scrubbing is the only thing using these commands today.
* Hot cues. `dj.hot_cue` works from hardware, the CLI, Auto Mix and the remote (the
  engine implements it on the daemon's command path, over the library's own
  hot-cue table — the one store the DJ Booth also writes, so there is no second
  copy), the importer translates `hotcue_N_activate` and `hotcue_N_clear`, and
  pad LEDs follow the cues: the daemon's feedback snapshot carries which cues are
  set, and `hotcue_N_enabled`/`hotcue_N_status` bind to it (531 output entries
  across 25 mappings). That snapshot is a *read* of the library guarded by its
  modification stamp, so any writer — the booth, the CLI, this daemon — invalidates
  it rather than letting a pad go stale, and the DJ Booth reads the same stamp once
  a second, so a cue set from a controller appears in an open window within that
  second instead of waiting for a reload. What is still missing there is a per-cue
  colour, which Mixxx has and Patch does not.
* Samplers, library browsing, effect parameters, `pregain`, and Mixxx's loop-point
  jumps: the Mixxx control objects for these have no Patch command or state, so the
  importer reports them rather than guessing.
* USB bulk devices. Mixxx hard-codes five vendor/product pairs with endpoint
  definitions, including the Traktor Kontrol S4 MK3; this is deliberately not
  attempted, and stays out of the MIDI and HID paths when it is.
* HID report-descriptor parsing. HID is addressed through Linux evdev codes, so
  a device with no evdev node (a vendor hidraw protocol) is out of scope.
* A graphical learn/editor flow: recording the next event and choosing a command
  is done by reading the monitor and writing the JSON.
* Velocity-sensitive pads, sampler triggering, library browsing and hot-cue
  colour feedback: the canonical commands or state those need do not exist in
  Patch yet, and belong there rather than here. On velocity, a measurement rather
  than an intention: **no mapping in Mixxx's corpus declares a `<velocity/>`
  option**, so nothing is waiting on it — the pads in the corpus send a plain note.
  How every option tag in that corpus is disposed of is in
  `docs/controllers-mixxx-research.md`.
* Vendor-specific protocols, MIDI 2.0, and sample-stamped event scheduling.

## Checkpoint reconciliation (2026-10-12)

The older checkpoint combined controller input with a local Broadcast feed.
Current `patch-broadcast` already has detached streaming, capture, Icecast,
RTMP and file output. Its daemon, CLI and surface remain the Broadcast
implementation. OBS can select the existing Patch Program capture source.
Only the missing controller unit and its HID/profile contract were integrated
into the current workspace. The old combined checkpoint is superseded.

## Integration measurements (2026-10-12)

At 1,024 MIDI bindings on this host, the CPU-only comparison measured
1,113 ns/event for the legacy linear resolver and 168 ns/event median
batch average for the indexed resolver (168 ns/event worst of ten batches).
MIDI parsing measured 6 ns/event. The linear baseline is one batch; these
are processing costs, not physical input-to-audio latency guarantees.

The release workspace suite passed 736 tests (19 ignored), run serially
to avoid competing adapter deadline tests. Strict release workspace Clippy
passed. Five focused controller checks include native event packet fragments,
HID repeat/release and overrun handling, calibrated axes and encoders, indexed
binding semantics, nonblocking FIFO mapping, command refusal, bounded reads
and monitoring, disconnects, and descriptor shutdown. Physical MIDI/HID
controller compatibility remains a hardware acceptance step.

## Integration measurements (2026-10-17)

The same corpus measured again after the vocabulary work above — Mixxx at commit
`44dc9ed7e8640b1d69b2d63f847d940b5ad688b1`, 164 mappings, 17,295 `<control>` and
2,318 `<output>` entries:

| | entries |
|---|---|
| translated into Patch bindings | 3,278 |
| script-bound (Mixxx hands them to JavaScript) | 11,588 (67 %) |
| no Patch equivalent, each with its own reason | 2,368 |
| low bytes merged into their 14-bit pair | 61 |
| outputs followed / with no state to follow | 1,392 / 926 |

The release workspace suite passed **1,012 tests with none failing**, run serially,
and strict release Clippy over the workspace and all targets was clean. Both are
reproducible: the summary with
`PATCH_MIXXX_CORPUS=<dir> cargo test -p patch-controller a_mapping_corpus -- --nocapture`,
and the ranking of what is refused, with the reason each family gives, by adding
`PATCH_MIXXX_REFUSALS=1`.

The refusals are a shape worth keeping in view: 129 distinct control-object
families, of which every *large* one now answers for itself — effects, samplers,
the master strip, beatgrid editing, tapped tempo, the library selection — and 50
small ones (217 entries between them) still share one generic sentence.

**No physical controller has been exercised at any point in this work.** Every
device path is a FIFO-backed simulation and unit tests, so these numbers measure
translation rather than hardware, and the hardware acceptance step remains
outstanding.
