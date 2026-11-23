# Performer · Patch Auto Mix

The approved Unit structure remains: real deck and mix controls, waveforms,
transport, tempo, key, FX, crossfader and live information about the set.
Open with `omarchy-patch open performer`; **f** toggles fullscreen.
The surface is read-only and opening it does not start music or a broadcast.
An RTMP broadcast from the Broadcast Unit sends this picture with the sound
(see [Broadcast](broadcast.md#the-picture-rtmp)); OBS can still capture it.

## A generated identity, not media playback

Patch Auto Mix is a nonhuman point-field presence. A vertex program generates
144,000 three-dimensional points from their IDs on each frame. Its paired
apertures, branching crown and converging signal wakes form a stable identity;
flow, expression, density, perspective and topology evolve through the set.
There is no image input, mesh/video asset, MP4 decoder, media player or stored
frame sequence. The earlier image-warp performer was rejected and removed.
The fragment program shades those points; the existing native compositor
adds restrained bloom. Geometry generation happens entirely on the GPU.

## Actual choreography and signals

The existing Visualiser director publishes its current scene seed, scene
progress, palette, section and AI handover progress to Performer. Arrivals
reveal the field; builds unfold it, drops send a spatial impulse through it,
and handovers weave the two signal streams. The same director leads both
windows, without starting a second choreography engine.

Live engine beat phase, levels, low EQ and the crossfader alter its expression
and flow. Scene parameters are smoothed across changes. The signature stays
recognisable as the set changes. No clip timeline drives the performance.
The face's motion is expressive; the controls and waveforms are exact engine
state. Native uniforms update on the UI frame clock; internal rendering is
capped at 1280 pixels on the longest side, while the UI stays native-resolution.
No live-coding session starts here. If live coding is added, Auto Mix must
drive it; there is no operator editor or pretend code stream in the Unit.

## Validation

The standalone geometry draw measured 0.078 ms median and 0.160 ms p95 at
1280×720 on the development RTX 5060 Ti across 1,440 generated frames. This
excludes Slint and post-processing. A review MP4 is an offscreen recording of
the generator with sample signals, never an input asset used by the Unit.

The geometry and fragment programs are compiled and linked by the shader
test, and a decision guard rejects texture sampler inputs. The renderer saves
and restores blend state and point sizing around the procedural draw.
Headless shots show controls/data only; a GPU failure is labelled rather than
showing an animated picture in its place. Audio/AI mixing logic is unchanged.
