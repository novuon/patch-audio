# Core mixer routing

The persistent engine owns playback routing and mixing. Closing a mixer or
module window does not stop the engine or bypass its processing.

Every desktop application and Patch Player feeds the core mixer. Applications
assigned their own EQ keep their dedicated mixer inputs. DJ decks feed the DJ
Booth's crossfader and group, whose output is summed once into the core mix.
Pattern, Sequencer and DJ Booth strips appear while their windows are open or
their engine transports remain active. In hardware mode, assigned inputs appear only while monitored or captured;
Patch mode also shows assigned inactive inputs. Application playback streams each get one of 32 independent native stream
lanes, including streams sharing one Chromium client or executable. The
stream's serial, client and process identify its lane; app names only label
it. Each lane has its own gain, mute, solo, pan, EQ, sends and engine meters.
The engine maintains these routes with no windows open. Requested buffer
sizes never identify or merge streams. Generic Chromium-family playback
streams stay in the Router but are omitted from the Mixer and EQ until they
have carried a second of pre-fader/pre-mute signal above −100 dBFS (a helper
that clicks once as it opens stays out). A heard stream keeps its strip
through silence and handover, except that a quiet one gives way after 60 s
when another stream of the same browser process was heard after it (X opens
a silent second stream beside a Space). A stream whose controls the user
moved, explicit caller names and kept strips always remain visible.
A waiting stream is shown within a quarter of a second of its first second
of signal: the Mixer asks the engine four times a second while one waits,
since being heard is no graph event.

Browsers close a paused output (Chromium: corked after ~10 s, closed ~5 s
later) and play into a new stream. A departed stream's strip therefore stays
in its place for 60 s, frozen at its last fader and knobs, without a meter;
the next stream of the same application takes that place
(`held_strips.rs`). A strip whose application has quit goes at once.

Browsers close an idle output after a few seconds and open a new stream on
play. Each application therefore remembers the lane settings of the stream
it was last heard on (`stream-profiles.json` in the state directory): fader,
pan, trim, strip EQ, sends and eight-band EQ. A new stream of that
application starts from them, never muted, soloed or cued. A renamed strip's
name passes to the stream that replaces it within two minutes (a kept strip's
at any time), and a Router "through Patch" choice belongs to the application. Source-provided media titles
are preserved; only indistinguishable visible sources need a number. Reservations survive an
engine handover; a departed stream releases its sink and its lane's controls
and filter/FX history are reset before reuse. Excess streams remain separate
Pulse controls and assigning native controls reports the 32-lane limit. Hardware mode shows one Main output and
hides the internal Program mix and event transport; Main shares the bar volume
and mute.

## Playback destinations

The bar calls `omarchy-patch output set-default <node-name>`:

* A hardware choice changes the core mixer's destination. It does not change
  the bus default or move applications onto the hardware.
* `patch` selects professional routing and keeps the current hardware path.
  Patch's routing controls can then change the destination.

Both choices preserve the bus listening gain and mute. The selected device
label is separate from its volume control: the bar controls the Patch bus,
not the hardware's internal unity gain. A new output must link successfully
before the engine removes its old destination. Internal Patch buses are
rejected as playback destinations to prevent feedback.

The actual PipeWire default remains `patch` in both modes. The runtime files
`output-device` and `output-selection` distinguish the physical destination
from the public bar choice. With system audio enabled, Player explicitly
requests `pipewire/patch`;
the engine also restores Player, deck and app-channel assignments after an
external routing helper moves them. While system audio is enabled, the Router
rejects direct application bypass and bus removal.

## Professional port assignments

Selecting Patch in the bar exposes routing controls and the active profiles'
actual inputs and outputs. No manufacturer rules or automatic profile changes
are used. Device inventory remains available through Inputs / Outputs; Routes
opens the graph even when the mixer is too narrow for its matrix.

Main and Headphone Cue can target actual mono/stereo ports, including different
pairs of the same device. Main supports mono; Cue requires a stereo pair.
Overlapping assignments are rejected. Configuration is stored in
`$XDG_STATE_HOME/omarchy-patch/output-routing.json`; missing ports stay missing
rather than remapping to unrelated channels. Hardware mode disables Cue routes
and uses the selected hardware's normal Main mapping, retaining the professional
assignments for reuse. AUX-labelled and unnamed audio devices use a first pair
for simple playback instead of being silent or duplicating the mix everywhere.

Additional independent Booth/monitor buses and arbitrary per-source sends are
not implemented. Available physical ports must not be mistaken for additional
independent DSP mixes. See `contextual-mixer-design.md` for scope and checks.

## Capture inputs

The bar calls `omarchy-patch input set-default <node-name>`. A physical source
feeds the core input channels and applications capture `patch.input` (Patch
Input). Multichannel devices expose individual ports, with up to sixteen
independent input channels. Choosing Patch Input keeps the configured inputs.

The capture mix has its own signal path and peak guard, independent of speaker
volume, mute and the core main fader. Input gain, pan, mute, EQ and voice
processing apply before the capture sum. Normal physical-input selection
disables speaker monitoring. The microphone inspector's **Monitor in main
mix** toggle enables it explicitly, with a gain ramp. Unmonitored inputs are
linked only while a capture application needs them; merely playing music does
not activate a selected microphone.

With Patch Input selected, the bar can switch between **Inputs** and **Main
mix**. The main mix tap is before system listening attenuation. CLI equivalents:

```sh
omarchy-patch input mix inputs
omarchy-patch input mix main
omarchy-patch input monitor mic-1 on
omarchy-patch input monitor mic-1 off
```

Internal meter captures, system-sounds loopbacks and explicit output-monitor
recordings stay on their own sources. Patch capture buses cannot be assigned
as microphone inputs. Monitoring choices and capture mode survive engine
restarts. The existing `patch.program` source remains an internal program tap.

## Verification

`integration/test-core-routing.py <binary>` starts private PipeWire, Pulse and
WirePlumber instances. Every hardware monitor is disabled and both playback
destinations are null sinks. It checks destination selection, Player routing,
gain and mute preservation, the signal ceiling, capture modes, monitoring,
sixteen distinct virtual input lanes, microphone link release and module
presence without opening any Patch UI.

The playback test measured 34–63 ms per selection and zero engine drops, with
the signal peak matching the configured listening gain. The old helper's loud
transient was not reproduced with virtual devices; the static bypass and
volume-control defects were confirmed. Real hardware switching and a physical
sixteen-input interface still require live validation after shipping.

Engine tests cover sixteen independent input lanes, input mixing without
speaker feedback, capture independence from system attenuation, output-bus
feedback rejection, DJ grouping and limiter ceilings. Rust and C now provide 64 channel slots (60 configured: the existing 28
plus 32 native playback lanes). Shared-memory meter format version 8 and a
larger channel-name table prevent old readers from misinterpreting the layout.
Unconnected stream lanes skip audio DSP work.

`integration/test-browser-streams.py <binary> --provider` opens twelve
playback streams on one libpulse client in a private graph. It requests two
buffer sizes and checks distinct native routes, real stereo meters, transfer
of an existing Pulse listening level, independent fader/mute/solo/EQ/send
behavior, surviving stream identities, clean lane reuse and caller naming.

A browser reopens its playback streams (Chromium does when a page starts an
echo-cancelling capture, or the output changes). Two cases keep the sound on
its lane:

* A stream that closes leaves its lane held for 10 s; the next stream from
  the same client process takes it back. Other processes never take a held
  lane while a free one exists.
* A stream heard for the first time takes the lane of a sibling (same process
  and application) that fell quiet just then: heard within 3 s, silent for
  the last 0.4 s, and adjusted, heard before or bound as a Remote voice. The
  two lanes swap, so the strip, its fader and sends stay with the sound and
  the quiet stream waits hidden. If both streams play 2–6 s later (a new tab
  after all), the swap is undone. A stream already heard before an engine
  upgrade is never a successor.

Whatever named the old stream (a Remote voice lane) follows a succession
chain to the stream playing now. The integration test covers both cases and
the undo with one libpulse client.
Build `patch-pipewire`'s `contextual-check` example alongside the binary for
its provider checks. No browser, physical device or desktop graph is used.

A web app may mix several callers before exposing one playback stream. Patch
can separate only the streams the browser supplies. Separate app playback
lanes do not automatically configure call return/mix-minus feeds; input mix
and AUX routing remain explicit.

The private browser test also replaces the running private engine and checks
that lane identities, levels and per-stream EQ survive. A handover successor
keeps the existing input topology through the crossfade and enables new lane
allocation only once the retiring engine has finished. Legacy application
lane controls seed their new per-stream lanes during that migration.
