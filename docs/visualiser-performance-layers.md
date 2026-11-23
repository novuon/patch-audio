# Visualiser performance layers

## Integration plan

The existing `patch_visual::director::Director` remains the choreography authority.
`visual.rs` assembles one performance snapshot at the same point it publishes the
current grid and generative frame. Deck values come from the existing DjState
mirror (engine meters/EQ/mixer plus existing metadata/playhead polling), not a
new audio-analysis or IPC service. The output spectrum is explicitly shared;
per-deck spectrum and stem telemetry are not inferred.

The GL compositor draws ordered procedural layers into its existing scene or
composite target, before existing glow/final processing. It does not allocate a
target per layer or use another context. Fixed shaders/programs are prepared
with the existing renderer setup; no layer shader compilation occurs in draw.
The existing Performer program supplies the point appearance; a shared procedural
operator surface supplies the complete body in solid and point appearances.
The user's latest reference supersedes the earlier nonhuman appearance for
these new Visualiser layers: restrained human-like silhouette, dark contours.
The separate Broadcast Performer remains supported.

Incremental vertical slice:
1. Bounded extensible source snapshot and layer settings; missing data explicit.
2. Deck ribbons driven by live level, beat, EQ, blend and shared spectrum.
3. Existing procedural face extended with a composed dark/point appearance.
4. Shared scene timing and intent, deterministic appearance changes, independent
   toggles/opacity/order and palette-linked configurable deck colours.
5. Existing Slint controls, tests, shader validation, GPU replay measurements.

No full-body animation, stem-specific visuals without telemetry, additional
Director, video playback, per-frame inference, or new rendering framework.

## Verification

Results are recorded below after the checks. Production audio and the installed
release are not changed by development builds. The initial settings keep both
layers off so the existing Visualiser remains unchanged until enabled.

## Implemented foundation

`patch-visual/src/performance.rs` owns plain scene/source/settings data, bounded
at eight input sources, appearance interpretation, cadence-pressure detail
reduction, and the existing graph-stamped handover conversion. It owns no
Director or audio analysis. Track/artist/BPM/key/progress text uses the existing
DjState mirror; the GPU packet contains only numeric drawing inputs.

`patch-surfaces/src/visual_layers.rs` adapts that mirror to the packet and
persists controls in `~/.local/state/omarchy-patch/visualiser-layers.json`.
The settings include custom RGB source colours and optional Director colours.
The existing application mirror now includes Visualiser windows, including
engine-rate levels/EQ/faders/beat positions. No second poller is introduced.

`visual.rs` attaches the packet to the exact frame that carries the current
choreography/generative composition. Remote AI/pattern intent feeds the layers
in all three modes. Track arrival, build/drop/quiet sections, current palette,
scene progress and handover are interpreted deterministically. Missing grid or
transition is represented by None, not a made-up clock/progress value.

`visual_gl/layers.rs` holds four fixed programs prepared with renderer setup:
deck ribbons, solid booth, solid operator, and the extended Performer points.
All draw into the parent's scene/composite target before bloom/final output.
Solid and point bodies share one articulated surface so proportions match
during morphs. Source/point buffers are implicit in gl_VertexID; no mesh uploads.
Uniform locations are cached at setup. No layer programs/targets are created in
the draw path. As with Patch's existing GL pipeline, program linking requires
the current GL context during setup; it is not per-frame compilation. Existing
Hydra/ISF hot compilation behavior is unchanged.

Text ink is marked in the existing cell flags and redrawn above the layers
when a title is present, without another texture or framebuffer. Source labels
and the settings panel remain normal Slint items above the GL canvas.

Press **L** or click **Layers** in the Visualiser. Toggle the layers separately,
use their opacity sliders, select dark silhouette/points, enable automatic
appearance, select the Director palette, and choose which new layer is in
front. The L key works in fullscreen too. Both layers default off to preserve
existing sessions. Appearance transitions are smoothed; opacity is explicit.

## Scope and next milestone

The initial adapter presents A/B while the packet/shader supports up to eight
sources. Additional-source discovery and labels are a later UI task. Signal
ribbons are procedural representations of actual state, not sampled audio
waveforms. Spectrum input is the existing mixed-output spectrum, not separate
deck spectra. EQ and enabled beat-FX depth affect only their corresponding
source. There are no invented stem components; stem-specific visuals await a
verified activity/mute feed. Effect response currently reflects enabled depth,
not a distinct visual algorithm per effect type.

The head-only first slice was rejected. The current operator has 30 articulated
parts: head, neck, torso, pelvis, shoulder panels, upper arms, forearms, elbows,
palms, ten fingers, thighs, shins and boots. Both appearances use the same
parametric geometry and a two-bone elbow solution. The restrained dark suit has
a chest P mark. Actual deck balance steers posture; shared beat/section/drop
state drives weight shifts and head motion. EQ, FX and handover state direct
hands toward the real control banks; fingers remain part of the articulated rig.

Decks & Signal now includes a procedural booth: two platters, mixer chassis,
channel-fader rails/caps, crossfader, six EQ knobs, filters, enabled-FX indicators,
play indicators and actual VU strips. Actual normalized fader positions and the
crossfader position are passed explicitly alongside the existing signal packet;
they are not guessed from audible weights. Ribbons originate at the platters
and rise/meet over the mixer during real handovers. The initial presentation
is A/B; the shared numeric source contract remains bounded and extensible.

One depth renderbuffer is shared by the existing scene/composite targets. Its
storage changes only on resize. Solid operator/booth and additive points use
the same depth attachment so hands, controls and body occlude in 3D. Renderbuffer
binding, depth function/write mask/clear value are restored for Slint. No extra
framebuffer or graphics context is added. A 24×16 grid per body segment gives
73,728 generated triangle vertices, with 30,720 optional points; the 27 booth
parts use 10,368 vertices. Particle decimation remains bounded.

More sophisticated finger contact, richer gesture intentions and more sculpted
surfaces are further work; no prerecorded body animation is used.
No LLM call is needed to animate. The existing shared intent already supplies
section/intensity/motion/density/colour for this slice, so its schema did not
need a speculative gesture API yet.

GPU failure leaves the original grid/CPU fallback working and shows the layer
availability in the panel. Slower frame cadence lowers optional point density
to half/quarter with slow recovery; scene/beat clocks are unchanged. Existing
resolution limits and feedback targets are preserved. Adaptive density is a
cadence-based safeguard, not a claim to measure GPU utilization.

## Reproducible checks

- `cargo test --release --workspace`
- `cargo clippy --release --workspace --all-targets -- -D warnings`
- `EGL_PLATFORM=surfaceless cargo test --release -p patch-surfaces performance_layers_gpu -- --ignored --nocapture --test-threads=1`

The opt-in EGL test exercises the production Renderer, including hybrid,
choreography, bloom, disabled layers, independent visibility, opacity zero,
resize/fullscreen-sized targets and empty sources. It checks stable target and
program handles across the timed sequence. It requires an EGL desktop GL
driver; it never opens the user's Patch or an audio device.

Optional `PATCH_VISUAL_GPU_SHOTS=<directory>` writes PPM captures. Optional
`PATCH_VISUAL_AI_REPLAY=<json>` replays a JSON array of the existing `ai_state`
contract through the real Director and compositor. Capture is read-only; the
replay never sends commands to the engine. Absent spectrum/EQ/stem values in a
capture stay absent/default. Replay uses the capture's source analysis energy;
production uses the existing live deck meters.

Read-only capture command:

```sh
python3 integration/capture-visual-transition.py /tmp/visual-handover.json
EGL_PLATFORM=surfaceless PATCH_VISUAL_AI_REPLAY=/tmp/visual-handover.json cargo test --release -p patch-surfaces performance_layers_gpu -- --ignored --nocapture --test-threads=1
```

## Initial head-only slice results (superseded by full operator)

- Release workspace suite: **1,207 passed, 0 failed, 33 ignored**.
- Release workspace Clippy, all targets, warnings denied: clean.
- Desktop GL 3.30 shader validation and linking: clean. All five new/extended
  shader sources also validate under GLES 3.0.
- Opt-in production-compositor EGL test: passed on NVIDIA GeForce RTX 5060 Ti.
  At 1920×1080, both layers + hybrid background + existing bloom, over 180
  frames: **0.200 ms median / 0.469 ms p95** synchronized wall time (includes
  `glFinish` in the test only). This excludes Slint, application polling and
  unrelated Units; it is not a full-application FPS or slower-GPU guarantee.
- Read-only capture of an actual **11-bar** Auto Mix transition: **223 stamped
  samples**, **70 executing handover frames**, **one existing-Director drop cue**.
  Capture/replay never altered the engine. Source weights transferred from B to
  A; outgoing stamped beats remained the transition timing authority.
- GPU assertions cover independent visibility, zero and partial opacity,
  source bass kill, enabled FX depth, empty sources, hybrid composition,
  canvas resize/fullscreen-sized targets and stable target/program handles.
- The user's running engine reported zero timing overruns and zero telemetry
  drops before/after the isolated rendering/replay checks. These checks ran
  before installing the new UI into that audio session.
- After a final intent-priority correction (reuse the existing smoothed
  Steering intent before raw posted intent), the focused dev-fast functional
  suite passed **83 tests**. Its two existing `renders_fast` CPU timing tests
  fail in that unoptimised-UI profile (scope 6.27 ms, analyzer 24.76 ms); both
  pass in the release workspace suite. Their guards were not changed.

Artifacts on this development machine:
`~/.cache/patch-visual-layers/ai-handover.json` and `shots/` beneath the same
folder. These are review/test captures, never application media inputs.

EGL verifies the actual compositor and canvas resizing, not the window manager
or human interaction with the controls. Native deployment verification uses a
clean committed release, with executable hashes and checks recorded in that
release's manifest. Development restarts are authorized by the standing rule in
AGENTS.md; restore playback and the existing layout after verification. No login
configuration is changed for this feature.

## Full-operator verification

The opt-in GPU test now additionally checks body motion from a changed shared
beat and booth-control geometry from changed actual fader/crossfader values.
`PATCH_VISUAL_TEST_SHADERS=<ui-directory>` is available **only in the test
binary** for rapid optical iteration through the production compositor. Release
builds always use embedded sources; there is no production file-watching or
alternative renderer. New body/booth shaders validate and link on desktop GL
3.30 and GLES 3.0. Final release measurements are recorded at deployment.
