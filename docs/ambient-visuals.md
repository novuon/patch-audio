# Ambient scenes in the Visualiser

The initial Ambient collection contains Sunset Drive, Jellyfish and Event
Horizon. It extends the existing background role, with no new independent
compositing layer or choreography engine. `docs/visualiser.md` describes the
controls and remaining catalogue scope.

Scene logic comes from omarchy-ttfx-layer revision
`c969b6c172108c5f6ab937d99db67c17ddd28196`, the renderer used by
omarchy-ambient revision `b4780c71fea079a98d2a3169fe48f92e6210688b` at review.
The selected native code, attribution and adaptation notes live in
`crates/patch-visual/src/ambient/`. No GTK, desktop supervisor, separate capture
or external rendering engine is required.

## Implementation

- The Director retains its track arrangements, title rules, passage lengths
  and actual Auto Mix handover clock. Ambient recipes select appropriate worlds
  for groove, lift/drop and calm passages. Complete environments never stack
  over each other as sparse signal effects do.
- `Cue::world_seed` identifies the leading track, including each side of a
  handover. World state survives title/artist passages and canvas resize.
- The state cache is bounded to four worlds with least-recently-used eviction.
  Raster buffers are reused and resize does not rebuild simulations.
- Patch supplies bands, music beat events and its existing motion clock.
  Upstream bass-based beat detection is removed. The pointer is local to the
  canvas, eased, and inactive behind the settings panel.
- Original scene colours feed Patch's existing glyph atlas and bloom passes.
  The CPU fallback receives the same cells. The horizontal bar used by Sunset
  is drawn geometrically because the default font lacks that glyph.
- Existing Patch backgrounds retain their original context/scene seeds and
  remain the default. Background selection has its own small persisted state;
  selecting Ambient brings the choreography into view. The existing M key can
  still select generative/hybrid afterwards. No login behaviour changes.

## Verification (2026-11-08)

`cargo test --release -p patch-visual`: **74 passed, 0 failed, 1 ignored**.
`cargo clippy --release --workspace --all-targets -- -D warnings`: clean.

Coverage includes seeded replay, animation, resize continuity, audio and
pointer response, bounded cache, invalid pointer/band input, shared beat
triggers, collection arrangements, switching collection mid-handover without
changing drop/title timing, and font-independent horizontal bars.

The opt-in production EGL compositor test passed at 1920×1080, with 180 frames
per world, frame-change assertions and each background composed with the
existing booth and performer:

```sh
EGL_PLATFORM=surfaceless \
PATCH_VISUAL_GPU_SHOTS=/tmp/patch-ambient-review \
PATCH_VISUAL_AMBIENT_VIDEO=/tmp/patch-ambient-review/ambient-collection.mp4 \
cargo test --profile dev-fast -p patch-surfaces --lib ambient_collection_gpu \
  -- --ignored --nocapture --test-threads=1
```

Measured compositor wall time, synchronised with `glFinish` in the test only:
Sunset median 0.276 ms / p95 0.387 ms; Jellyfish 0.269 / 0.341 ms; Event Horizon
0.256 / 0.337 ms. These exclude CPU scene generation, Slint and the full app,
and are not a guarantee for other GPUs. The recording uses repeatable test
music signals at 124 BPM, not a live audio recording. It is a review artifact,
never an application media input.

`cargo test --profile dev-fast -p patch-surfaces --lib`:
**118 passed, 3 failed, 8 ignored**. Failures outside Ambient:

- `scope::tests::renders_fast`: 6.02 ms against 3 ms, in the unoptimised
  surfaces profile.
- `analyzer::tests::renders_fast`: 24.19 ms, in the same profile. These two
  profile-sensitive timing checks are also discussed in the existing
  performance-layer handover.
- `app::output_strip_tests::the_top_of_the_fader_is_what_the_strip_can_do`:
  the browser application at position 0.5 uses `output_db`, while the assertion
  expects -30 dB. `app.rs` is byte-for-byte identical to HEAD
  (`714ae0026c2ffc53dd4efaded7b05b22a5b21b29`). No fader code or test changed.

Review media on this machine is in `~/.cache/patch-ambient-review/`: the
18-second MP4, original 1080p PNGs, and small GIF excerpts. Development code
has not been installed over the user's release and playback was not changed.

The booth/performer example also has a dedicated moving capture (rather than
only the composition still). `ambient_performer_gpu` drives the production
Groove with a verse and drop, active source meters and crossfader movement.
It checks that the body's pose changes, not just its timestamp, and passed.
`PATCH_VISUAL_AMBIENT_PERFORMER_VIDEO` writes its 12-second 1080p MP4.
The review recording is `~/.cache/patch-ambient-review/ambient-performer.mp4`.

## Release preparation

For live installation the remaining browser-fader assertion was corrected to
the existing Pulse cubic law (`60 * log10(position)`), completing the earlier
`f0f7997` test correction. This changes only a test; fader behaviour and sound
are unchanged. Release workspace results are recorded in the installed
release manifest.
