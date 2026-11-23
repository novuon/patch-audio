# Mic: native microphone processing

Mic is an Audio Unit (`omarchy-patch open mic`, key `i`), sharing the persistent
Patch service, sixteen input lanes, Meter, channel EQ, Router, Recorder and
Player. Closing Mic does not close its signal path. The Linux backend exposes
USB, interface, built-in, Bluetooth and virtual capture sources through the
existing source discovery; a device must actually be available to PipeWire.
Stereo and multichannel interfaces expose independent capture ports; stereo
devices also retain their whole-device choice. Automatic input addition avoids
adding a port already covered by a whole-device assignment. Mic does not claim
hardware gain or onboard DSP control for unidentified devices.

## Signal path

Raw input → software input trim → high-pass → optional four-band suppression
→ de-esser → noise gate → downward expander → compressor → software output
trim → existing eight-band channel EQ → channel fader/mute → capture sum →
existing zero-latency peak guard → `patch.input`.

Speaker monitoring is an explicit independent send into the main mix. New UI
inputs are muted and unmonitored. Use headphones before enabling monitoring.
Patch capture buses are rejected as inputs to their own capture lanes.
Select **Patch Input** in a conference or streaming application. Router/Mixer
continue to control the same lanes; this is not a separate engine or stream
copy. Recorder now offers **Processed microphones · Patch Input**.

The four-band suppression is deterministic downward expansion, with up to
20 dB attenuation in quiet bands. It is not neural denoising, echo cancellation,
or a claim to remove every noise during speech. Thresholds need adjustment
for each room. Gate, de-esser and compressor reuse Patch's existing voice
algorithms. The standalone expander uses a continuous stereo-linked 2:1 curve.
Compression and de-essing currently expose the established amount controls.
Input/output trims glide over approximately 3 ms.

Voice DSP lives in `patch-dsp`, with its previous engine API re-exported for
compatibility. Channel EQ and the capture limiter remain their existing
implementations. Settings publication is a bounded seqlock read; the callback
retains its last complete configuration if a control-plane writer is active.
The callback allocates no memory, takes no locks and performs no filesystem,
process, inference, or GUI operations. Rate changes rebuild fixed filter state.

## Use a browser voice as a mic

To pass a voice playing in Chromium (an X Space, a call) to another browser
session as its microphone, processed like a mic:

1. Let the voice play in Chromium.
2. In **Mic**, press **+ Voice source**. A **Voice N** lane takes that stream;
   its Chromium strip in the Mixer now reads "Sound now on Voice N".
3. Pick a starting profile (Remote voice is set) and adjust as for a mic.
4. In the Mixer, turn the **Voice N** lane's **Aux1** knob up (0 dB).
5. In the other browser, choose **Patch AUX 1** as the microphone.

If that other browser is Chromium too and records AUX 1 in the same process
the voice comes from, the lane warns of a loop: use another browser (Firefox)
for that session. To hear the voices of a call you are in yourself, use
**+ Call far end** instead: they are never sent back to the call.

## Remote voices (mix-minus)

A remote voice is the far end of a call or an X Space, taken into a Mic lane
like a remote guest on a broadcast console. **+ Call far end** takes the stream
that is playing into Patch now. Any lane's Input list also offers **Remote voices**:
each playing application stream (`stream:<lane key>`, shown as
"Chromium · stream 3 · playing") and each application channel in use
(`app-1`…`app-4`). Browsers name every stream "Playback", so the lane number
and "playing" tell them apart. The lane is then named **Remote N**.

* The stream leaves its own strip: its soundcard's monitor feeds the Mic
  lane's input instead, never both. No device opens and nothing is resampled.
* A remote lane starts with the **Remote voice** profile: high-pass 100 Hz,
  no gate or expander (the call has already processed the voice: codec, auto
  gain, noise suppression), compressor 60 % to level loud and quiet speakers,
  de-esser 30 %, +1 dB presence. It starts unmuted and monitored, because it
  was audible before.
* **Call far end** (mix-minus, the default for **+ Call far end**): the lane plays
  and records in the program but stays out of Patch Input, and it feeds no
  send (AUX 1/2, FX 1/2) whatever its send knobs say. AUX buses leave Patch for
  browsers and streams, and an FX return could carry the voice there. So the
  people in the call never hear themselves back. When Patch Input carries the
  whole program (`omarchy-patch input main`), it is N−1: the program before
  the master EQ, Tune and limiter, without the remote lanes, then the master
  fader and Patch Input's peak guard.
* **Voice source** (**+ Voice source**, or **Call far end**
  off in the lane's Inspector): the lane is named **Voice N**. An app's voice
  goes through the voice chain like a microphone, into Patch Input and
  sendable: send it to AUX 1 and choose **Patch AUX 1** as the microphone in
  another browser. The Mic unit warns of a loop when the browser this voice
  comes from records the AUX it is sent to itself.
* The Mixer replaces the far-end lane's send controls with **Sends off to
  prevent echo in the call**. The original stream strip reads **Sound now on
  Remote N** or **Sound now on Voice N**.
* A lane keeps its stream and role when the browser reopens its playback
  (see [Core routing](core-routing.md)).
* When the stream ends (a browser closes a paused stream after a few
  seconds), the lane goes silent and says so. Choose the playing stream again.
  Choosing your microphone in the bar keeps remote lanes.

Browsers process what they capture for a call (WebRTC echo cancellation,
noise suppression, auto gain). That suits voice; music sent into a call or a
Space through Patch Input can pump or duck. Use **tools/input-test.html** to
hear what the browser receives, with each of these switched on or off.

## Meters and latency

A bounded history retains the latest 128 engine-cycle raw peaks. Bars use a
cycle axis, and their actual stamped time span is shown; sample-rate changes
reset the history. This is level history, not a reconstructed waveform.

The compact surface reuses the Inspector's live post-fader meter. Raw input
peak/RMS and gate, compressor and de-esser detector gains come from a bounded
engine-cycle snapshot stamped with graph sample position, monotonic time and
sample rate. Reduction excludes compressor makeup. Missing/disconnected or
stale snapshots are withheld, rather than presented as current audio.

These processors add **zero samples of buffering latency**. This does not mean
zero device/backend latency, nor zero frequency-dependent IIR group delay.
The EQ response view uses the existing EQ renderer and commands, with Mic
lanes as valid targets. Program-spectrum overlays are hidden for a Mic EQ,
since that spectrum would not represent microphone audio.

## Portable profiles

Nine factory starting profiles: Natural, Broadcast, Podcast, Streaming, Warm
voice, Clear voice, Vocal recording, Noise reduction and Custom. These are
conservative starting points, not microphone-independent sound guarantees.
Profiles contain voice settings and eight-band EQ, with a versioned `patch-mic`
JSON format. Source assignment, monitoring, channel fader/mute, hardware gain
and calibration measurements are deliberately excluded.

The native surface supports load, save and a previous-settings comparison/
restore. Saving under another name duplicates the current settings. Factory
profiles are read-only. CLI import/export, duplicate and explicit replacement
support interchange and edits; existing exports and comparison WAVs are never
overwritten. Service writes use a temporary file, fsync and atomic rename.

```sh
omarchy-patch mic profiles
omarchy-patch mic select mic-1 <PipeWire-source-or-port>
omarchy-patch mic profile mic-1 Podcast
omarchy-patch mic set mic-1 input_gain_db 3
omarchy-patch mic set mic-1 expander -50
omarchy-patch mic set mic-1 noise -55
omarchy-patch mic bypass mic-1 on
omarchy-patch mic bypass mic-1 off
omarchy-patch eq mic-1 band 3 bell 250 -2 1
omarchy-patch mic save mic-1 'My voice'
omarchy-patch mic save mic-1 'My voice' --replace
omarchy-patch mic duplicate Podcast 'Podcast variant'
omarchy-patch mic export 'My voice' /tmp/my-voice.json
omarchy-patch mic import /tmp/my-voice.json
omarchy-patch mic restore mic-1
```

Voice bypass covers the voice stages and their trims; EQ and the capture peak
guard retain their independent bypass/protection states. The chain has a fixed,
documented order. Arbitrary native stage reordering is not implemented.

## Recorded calibration and comparison

Open Calibrate, record an original take, then stop and save it. The recording
uses Recorder's device capture, retaining the original. Processed takes capture
the shared Patch Input sum. Analyze a local original take of 1–60 seconds,
including room silence and normal speech. A longer take is rejected rather than
silently truncated. ffmpeg decodes only on a worker; live audio is unaffected.
Stop a recording before analyzing or playing its file.

Measurements include sample peak/RMS, clipped sample count, headroom, window
level range, a quiet-window percentile, approximate frequency energy and
low/high-frequency activity indicators. Quiet-window estimates require an
actual quiet section. Band indicators are heuristics for possible plosives and
sibilance, not a validated speech classifier. Silence produces no gain boost,
and clipped recordings cannot be repaired by a software gain recommendation.

Advice is shown without changing sound. Apply advice is explicit, preserves
channel EQ and gain, and checks the source still matches the analyzed input.
Restore returns to the previous voice/EQ settings. No hardware gain is changed.
The optional `RecommendationProvider` trait accepts measurements and returns
advice; inference is not part of the normal audio path.

Render comparison processes the same take through the actual shared voice,
EQ and capture limiter implementations, using Recorder's WAV writer. Listen
original/processed uses Player. The render excludes the channel fader and
captures no live mix, so other playing sources cannot contaminate it.

```sh
omarchy-patch mic calibrate '/path/to/original.wav'
omarchy-patch mic compare '/path/to/original.wav' /tmp/my-voice.json /tmp/processed.wav
```

## Extension boundaries and current limits

`patch_core::mic::InsertionPoint` reserves before-voice, before-EQ and after-EQ
positions for shared future processor hosting. Mic has no plugin manager.
Standalone device applications belong to application adapters, not the DSP
path. `HardwareAdapter` requires detection and verified gain writes; there is
no manufacturer adapter or required hardware DSP. The macOS branch and its
backend are untouched; domain, analysis and DSP code are portable Rust.

Raw recording of an individual multichannel port is not implemented by the
existing stereo device Recorder. Mic reports that limitation; processed
capture of those lanes still works through Patch Input. Independent advanced dynamics timing controls are not yet implemented. Calibration records currently live in the UI session;
portable profiles and current device assignments persist in the service.
Physical hot-plug/Bluetooth testing and blind listening remain release gates.

## Reference review

Reviewed Podtune commit `e3555d5eebb5a9fe3e4abfbd5a06c28b7d4cb23e`, including
`src/protocol.cpp`, `src/audio.cpp`, `src/backend.cpp`, `src/presets.json`, QML
controls and website sources. Useful concepts are conservative presets,
compact/advanced controls, explicit signal measurements and recorded listening
comparisons. Its HID register map, firmware assumptions, ALSA control names,
APHEX effects and headphone-output selection are specific to the PodMic USB
and were not imported. Its acknowledged write/read-back approach informs the
optional hardware adapter contract.

Sources: [Podtune repository](https://github.com/bjarneo/podtune) and
[Podtune website](https://bjarneo.github.io/podtune/).

## Validation

```sh
cargo test --offline --workspace
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo test --offline --release -p patch-dsp voice_cost -- --ignored --nocapture
python3 integration/test-core-routing.py target/debug/omarchy-patch --mic
cargo run --offline -p patch-surfaces --example mic-shot -- /tmp/patch-mic-shots
```

The routing integration runs private PipeWire/Pulse/WirePlumber servers with
hardware/Bluetooth monitors disabled and temporary XDG directories. It never
opens a physical device or changes the installed Patch. DSP tests cover linked
channels, filter/gate/de-esser/compression behavior, band noise attenuation,
rate changes, zero callback allocations and coherent parameter publication.
Calibration tests cover silence, clipping, measurable levels and quiet sections.

Development validation on 2026-10-09: the workspace suite passed 695 tests
(17 ignored benchmarks/integration cases), the development build and strict
workspace Clippy check passed. The private routing test passed with zero
reported engine drops and observed switching times of 36–58 ms. The offline
file test verified calibration, exact bypass rendering, duration and original
file/overwrite protection. Headless simulated surfaces were inspected at
560×460 and 900×600; these are layout checks, not hardware validation.

An optimized 1024-frame stereo benchmark measured the current four-stage
voice chain at 21.2 µs and optional band suppression at 40.1 µs on this machine
(48 kHz, approximately 0.10% and 0.19% of the block duration respectively).
These are kernel timings under development load, not whole-engine CPU claims.
No assembly was added. The installed release and the macOS branch were not
changed.
