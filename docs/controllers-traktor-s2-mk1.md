# Traktor Kontrol S2 Mk1: protocol facts

**Status: implemented, and verified by test against the protocol below — not yet
validated by a press on the hardware.**

Patch reads this device itself, through the raw HID transport, and
`patch_controller::profiles::traktor_kontrol_s2()` binds its controls and drives
its LEDs. What is verified without the unit in the loop: the profile matches the
ids below and not the Mk2's; every action parses and validates as a command a
user may send; every field resolves through the resolver; and the decoder is
tested against reports in this device's shape. What is **not** verified: the byte
offsets in reports `0x01` and `0x02` are the reference implementation's parsing,
not a capture — including the question of whether an offset counts the report-id
byte — and the jog field's encoding. A capture settles both; §*Validation* at the
end says how.

**Setting a controller up in general** is `docs/controllers-workflow.md`; this
document is the protocol reference for this one device.

**What happens if you plug one in today.** Patch's Controllers unit lists it —
HID devices are listed unfiltered, one entry per physical device, because Linux
exposes nothing that can tell a controller from a keyboard — and reports that no
profile matches it. That is the honest outcome rather than a broken one: Mixxx
drives this device entirely from HID reports through JavaScript, and every HID or
bulk mapping in Mixxx's corpus has **zero** declarative controls, so there is
nothing for the importer to translate. Driving it needs either the script runtime
or a hand-written declarative profile written against the protocol facts below.

**Where these facts come from.** Mixxx drives this controller with
`res/controllers/Traktor Kontrol S2 Mk1.hid.xml` (761 bytes: an `<info>` block and
two script files) and `Traktor-Kontrol-S2-MK1-hid-scripts.js` (71 KB), at commit
`44dc9ed7e8640b1d69b2d63f847d940b5ad688b1`. Mixxx is GPL-2.0-or-later and **no
part of those files was copied**: what follows is a device protocol — report ids,
byte offsets, bitmasks — which is what interoperation requires, written up in
Patch's own structure and for Patch's own command vocabulary. Mixxx ships no MIDI
mapping for this device: its preset is HID, and importing that preset yields a
`hid` profile that *matches* this controller and carries no bindings at all — every
one of its controls is JavaScript — which is an honest starting point rather than a
working mapping.

**What the hardware reports, measured on a real S2 Mk1** (not inferred):

    HID_NAME=Native Instruments Traktor Kontrol S2   the name has no "Mk1" — the
                                                     generation is in the product id
    HID_ID=0003:000017CC:00001101                    bus 3 (USB), vendor 0x17cc,
                                                     product 0x1101
    HID_UNIQ=522EB6F6                                the serial: the strongest criterion
    /dev/hidraw6, USB interface 3

A profile for this controller should pin it with `"vendor_id": 6092,
"product_id": 4353`, or with the serial. A *name* criterion is a case-insensitive
substring test against what the device reports, and Mixxx's preset for it ends in
"Mk1" while the hardware reports none — so the preset's own name can never match.
Importing that preset now also adds the form without the generation token, which is
why the import attaches to the device at all.

**Measured on a unit: the reference's deck A and B are the other way round.**
The left-hand deck's buttons arrive at the offsets the reference calls `B`
(`0x0C` and its family), and the right-hand deck's at `0x0D` and its family.
Driving the reference's `A` offsets as Patch's deck A therefore plays the *right*
deck: left play sent deck B's command. The profile swaps the two labels and
leaves the tables below as the reference has them. Nothing else about report
`0x01` was wrong — the bit offsets and masks drove the right controls on the
first attempt, which also settles one open question below: on this device an
offset does not count the report-id byte.

Device id: USB `0x17cc:0x1101`, protocol HID. A second unit is
`0x17cc:0x1320` (`S2 Mk2`), and the Mk2/Mk3/S3/S4 families are separate
protocols, not variations of this one.

## Reports

| report | direction | contents |
|---|---|---|
| `0x01` | in | buttons (bitmasks) and the two jog wheels |
| `0x02` | in | faders, knobs, encoders, crossfader — 12-bit values 16…4080 |
| `0x80` | out | 61 bytes, one brightness byte per LED, `0x00`–`0x1F` |
| `0xD0` | feature | fader calibration |
| `0xD1`–`0xD3` | feature | knob calibration, 0x20 bytes each |
| `0xD4` | feature | jog-wheel calibration |

Two details that are easy to get wrong:

* **The device has to be read once at startup.** The first packet of an input
  report is treated as an initial value and produces no change, so the reference
  implementation reads `0x01` and `0x02` once at init and processes `0x02` twice —
  the first copy with every byte inverted, so that the second copy differs from
  it and registers as movement.
* **Ranges come from the device, not from constants.** Knob and fader limits are
  read from the `0xD0`–`0xD4` feature reports as min/centre/max pairs (little
  endian) and the jog press threshold (big endian). An adapter that hard-codes
  0…4095 will be wrong on units whose stick calibration was done at the factory.

## Input report `0x01` — buttons

Each field is one bit of one byte; masks are within the byte at that offset.

**Every offset below is the reference's, and each is one more than the byte in
this device's report.** Its offsets index a buffer that *includes* the report id
as byte 0; Patch reads the payload, so the profile uses each value minus one.
Measured on the hardware: the reference's `0x0D` arrives in byte `0x0C`, its
`0x09` in `0x08`, its `0x0F` in `0x0E`, its `0x2F` in `0x2E` — and the same
shift applies to its LED table in report `0x80`. So transcribe the table, then
subtract one; or copy `DECKS` in `crates/patch-controller/src/profiles.rs`,
which holds the measured values.

| control | offset | mask |
|---|---|---|
| Deck A shift / sync / cue / play | `0x0D` | `0x80` / `0x40` / `0x20` / `0x10` |
| Deck A pads 1–4 | `0x0D` | `0x08` / `0x04` / `0x02` / `0x01` |
| Deck B shift / sync / cue / play | `0x0C` | `0x80` / `0x40` / `0x20` / `0x10` |
| Deck B pads 1–4 | `0x0C` | `0x08` / `0x04` / `0x02` / `0x01` |
| Deck A pfl / loop in / loop out / reset | `0x09` | `0x80` / `0x40` / `0x20` / `0x10` |
| Deck B pfl / loop in / loop out / reset | `0x0B` | `0x80` / `0x40` / `0x20` / `0x10` |
| Deck A load / samples | `0x0B` | `0x08` / `0x02` |
| Deck B load / samples | `0x0B` | `0x04` / `0x01` |
| FX1 focus, param 1/2/3 buttons | `0x09` | `0x08` / `0x04` / `0x02` / `0x01` |
| FX2 focus, param 1/2/3 buttons | `0x0A` | `0x80` / `0x40` / `0x20` / `0x10` |
| FX1 channel 1 / 2 enable | `0x0A` | `0x02` / `0x08` |
| FX2 channel 1 / 2 enable | `0x0A` | `0x01` / `0x04` |
| Browse encoder press | `0x0E` | `0x08` |
| Deck A gain-encoder press / left enc press / right enc press | `0x0E` | `0x01` / `0x02` / `0x04` |
| Deck B gain-encoder press / left enc press / right enc press | `0x0E` | `0x10` / `0x20` / `0x40` |
| Jog wheel A / B | `0x01` / `0x05` | 32-bit value |

## Input report `0x02` — controls

12-bit values (16…4080 after calibration) unless said otherwise.

| control | offset | notes |
|---|---|---|
| Crossfader | `0x2F` | 16-bit |
| Head mix | `0x31` | 16-bit |
| Sampler gain | `0x13` | 16-bit |
| Browse encoder turn | `0x02` | 4-bit, high nibble |
| Deck A rate / volume | `0x0F` / `0x2B` | faders |
| Deck B rate / volume | `0x1F` / `0x2D` | faders |
| Deck A gain encoder / left enc / right enc | `0x01` | low nibble / high nibble / `0x02` low nibble |
| Deck B gain encoder / left enc / right enc | `0x03` | low nibble / high nibble / `0x04` low nibble |
| Deck A EQ hi / mid / low | `0x11` / `0x25` / `0x27` | |
| Deck B EQ hi / mid / low | `0x21` / `0x23` / `0x29` | |
| Deck A / B jog press (the platter touch) | `0x0D` / `0x1D` | value, not a bit — see below |
| FX1 mix / params 1–3 | `0x0B` / `0x09` / `0x07` / `0x05` | |
| FX2 mix / params 1–3 | `0x1B` / `0x19` / `0x17` / `0x15` | |

**The platter touch is a value, not a button.** There is no touch bit: the device
sends a value per platter in report `0x02`, and the `0xD4` feature report gives
the value seen when pressed and when released. A hand is on the platter when the
value falls inside the pressed window. Jog *movement* comes from the 32-bit field
in report `0x01`; movement and touch are therefore two different reports, which
is exactly the pair `dj.scratch` / `dj.scratch_end` expects.

## Output report `0x80` — LEDs

One byte per LED; total payload 61 bytes, unused bytes sent as zero.

**The device takes the full byte, and the reference's 0–31 scale must be scaled
up.** Measured: `0xFF` lights an LED at full, while `0x1F` — the reference's own
"on" value, written into the byte unchanged — is not visibly lit at all. The
reference's script says so in its own comment ("a scale from 0 to 0x1F"), and it
writes through a packet layer that scales. So a brightness of 0…31 has to be
multiplied into 0…255; `max: 0xFF` in the profile.

**Every offset below is one *less* than the reference's table**, because its
offsets index a buffer that includes the output report id — the same id-inclusive
convention its input offsets use. Those input offsets were measured on the
hardware (its deck 0 play at `0x0D` arrives in byte `0x0C`), and the outputs come
from the same `HIDPacket` class, so the shift is the same.

| LED | Deck A | Deck B |
|---|---|---|
| play / cue / sync / shift | `0x02` / `0x06` / `0x03` / `0x07` | `0x22` / `0x26` / `0x23` / `0x27` |
| pfl | `0x1F` | `0x1C` |
| loop in / loop out / reset | `0x01` / `0x04` / `0x05` | `0x21` / `0x20` / `0x25` |
| track loaded | `0x1E` | `0x1D` |
| VU meter / peak | `0x14` / `0x00` | `0x10` / `0x24` |
| samples | `0x34` | `0x33` |
| pad 1–4 green | `0x0B` / `0x0A` / `0x09` / `0x08` | `0x2B` / `0x2A` / `0x29` / `0x28` |
| pad 1–4 blue | `0x0F` / `0x0E` / `0x0D` / `0x0C` | `0x2F` / `0x2E` / `0x2D` / `0x2C` |

| LED | offset |
|---|---|
| FX1 focus / params 1–3 / channel 1 / channel 2 | `0x1C` / `0x1B` / `0x1A` / `0x19` / `0x3D` / `0x3B` |
| FX2 focus / params 1–3 / channel 1 / channel 2 | `0x39` / `0x38` / `0x37` / `0x36` / `0x3C` / `0x3A` |
| Warning light (master) | `0x33` |

Each pad has separate green and blue bytes, so a colour is the pair; a brightness
of `0x00` turns an LED off.

## What this maps onto in Patch

| device control | canonical Patch command |
|---|---|
| play | `dj.transport { action: toggle }` |
| cue | `dj.transport { action: cue }` (and `cue_release` on release) |
| sync | `dj.sync` |
| loop in / loop out | `dj.loop_in` / `dj.loop_out` — the two-button loop the panel labels, with `dj.loop { beats: null }` on RESET leaving it |
| pfl | `dj.toggle { what: pfl }` — the panel labels this button **CUE**, right above the channel fader. It is the *headphone* cue, not the deck's cue point on the row above, and the two are easy to confuse when reading a capture: both report on report `0x01`, three bytes apart. Until the engine arm used the flag it already had (`Params::pfl_of`), every press was answered `refused` and the button did nothing at all. |
| deck volume fader | `dj.fader { position }`, mapped to the fader positions 0…60/66 so its top is **unity** (0 dB): the fader law `session::fader_db` runs −60…+6 dB over 0…1, and a deck fader must not reach the master limiter |
| EQ hi·mid·low | `dj.eq_knob { band, value }` with `range = (-0.97, 1.0)` — the command's own domain is −1 (kill) … 0 … +1, so the knob's travel avoids the kill at the bottom |
| gain encoder (endless) | `dj.filter_step { deck, amount }` — an endless knob sends movement, so it drives the filter, and its centre press returns the filter to off with `dj.filter { amount: 0.0 }`. Patch's own gain has no control here. |
| FX unit (x2) | `dj.fx { kind, on, depth }` — the three effect buttons choose the deck's effect and the dry/wet knob is its depth. Patch holds one effect per deck with one depth and one length, so the unit's second and third effect knobs have no command to reach |
| gain knob press | `mixer.set_gain { db: 0 }` on that deck's strip |
| rate fader | `dj.tempo { percent }` (percent, not Mixxx's normalised rate) |
| crossfader | `dj.crossfader { position }` |
| load | `dj.load_path` / `dj.load_track` |
| browse encoder | **nothing yet** — Patch's library browsing has no canonical command |
| pads | `dj.hot_cue { n }` — see the warning below |
| jog movement (report `0x01`) | `dj.scratch { delta, velocity }` |
| platter touch (report `0x02`, pressed window) | `dj.scratch_end` on release; the first movement is the press |

**Hot cues work from hardware, with one caveat.** `dj.hot_cue { n }` jumps to a
stored cue or sets one where the deck is, and `{ clear: true }` removes it. The
engine executes it on the daemon's command path over the library's hot-cue table
— the same store the DJ Booth reads and writes, so there is no second copy of the
state — and the importer translates Mixxx's `hotcue_N_activate` and
`hotcue_N_clear` onto it (443 entries across the corpus). Two things are still
missing: a DJ Booth that is already open will not *show* a cue set from a pad
until it reloads cues, and the pads' LEDs cannot follow hot-cue state, because
the daemon's feedback snapshot carries deck status and meters but not cue
positions. Both have the same fix — publish cue positions in that snapshot.

Feedback follows the same table in reverse through the LED offsets above: play
LED follows deck state, cue and loop LEDs their flags, pads their hot-cue or mode
colour, VU from the meter ring that the Mixer, DJ Booth and remote already read.

## What Patch provides for it

1. **A `hidraw` transport** (`patch-controller/src/hid.rs`): opens only a device
   the user named — a node or an identity such as `usb:17cc:1101` — read
   nonblocking with a fixed per-read budget on the control-plane thread, and
   writes report `0x80` for feedback. The controller's own poll set drives it,
   exactly as it drives a MIDI port.
2. **The bindings are the field table** (`Trigger::HidField`): report id, offset,
   width and mask, so the protocol stays data in a profile rather than code in
   the service. The tables above are that data, written out in
   `patch-controller/src/profiles.rs`.
3. **Feature-report reads at init** for calibration (`0xD0`, and `0xD4` for the
   platter), and the startup read: the device's first packet is the current state
   of every control, so it is treated as an initial value and fires nothing.
4. **A HID output target** (`OutputTarget::HidReport`): one brightness byte of a
   report that goes out whole, assembled from every output sharing that report,
   so lighting a pad does not blank the meter beside it.
5. **Both reports on one device**, with the last value of every field kept, so a
   scratch pairs its movement with the platter's hand — which is a *value* in a
   window, not a bit, and is derived accordingly.

## The browser: what is still missing, and how to add it

The browse encoder (`0x01` in report `0x02`, mask `0xF0`), its press (`0x0D`,
mask `0x08`) and LOAD A / LOAD B (`0x0A`, masks `0x08` / `0x04`) are the only
controls on this panel with nothing behind them. The reference's load button
means *LoadSelectedTrack*, and Patch has no selection a command can address:
`dj.load_track` takes a title query, `dj.load_path` a path.

The engine can already enumerate what that selection would be over —
`C::LoadTrack` reads `patch_library::open_quick().tracks()` and filters with
`patch_library::title_matches`, the same rule the DJ Booth uses, and the code
comment beside it names the gap: *"the candidates are the whole library here,
because the engine has no view, where the booth searches the list it is
showing."*

So the work is a cursor plus two commands — move it by rows, load it into a
deck — and it belongs in **one** owner. Two options, and they differ in who
holds the selection:

1. **The engine** holds the cursor over `tracks()`, publishes it in the `dj`
   status the Booth already polls, and the Booth highlights that row. The
   controller works immediately, but until the Booth follows it the DJ scrolls
   blind.
2. **The Booth** holds the cursor, over the list it is actually showing, so the
   DJ scrolls what is on screen — but a controller's command is handled by the
   *engine* today, so this needs a path for a browse command to reach the app.

Option 2 is what a DJ wants; option 1 is what the current plumbing supports
without a new route between the processes. Either way the profile gains three
bindings and the change must be one commit: a new command variant is refused at
compile time until `validate`, the engine's handler and the Booth's handler all
know about it.

## Open questions

* ~~Does Linux expose this as `hidraw` at all on the user's machine?~~ **Answered:
  yes.** `/dev/hidraw6`, USB interface 3, on the machine these facts were measured
  on. Its node is `root:root`, so reading it needs the udev rule Patch ships —
  see `docs/controllers-workflow.md` §10.
* The jog field's encoding is treated as an unsigned 32-bit delta by the reference
  implementation; whether the two half-turns wrap, and at what modulus, is not
  established here.
* Pad modes (hot cue / intro-outro / sampler) and the shift layer are *behaviour*
  layered on these controls; the tables give the inputs, not the modes.
* The VU meter and peak LEDs are driven by metering, which suggests a rate-limited
  path rather than one message per state change.

## Validation, when there is a unit

Record reports while moving one control at a time, then assert that the field
table above maps each recording to the intended control: that is testable with no
Patch build and no hardware in the loop, from captures. Everything above is
unvalidated until such captures exist, and no controller should be called
supported on the strength of this document.
