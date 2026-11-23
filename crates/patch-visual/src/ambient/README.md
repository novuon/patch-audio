Scene logic adapted from avillagran/omarchy-ttfx-layer, commit
c969b6c172108c5f6ab937d99db67c17ddd28196 (MIT, copyright 2026 avillagran).
https://github.com/avillagran/omarchy-ttfx-layer

All 34 ASCII scenes are exposed (`stage::Backdrop::AMBIENT`). `journey.rs`
serves Sunset Drive, Neon City, Vector Highway and Circuit Run; `upstream/`
holds the remaining scene sources (showcase scenes, sand clocks, 3D scenes,
eye, matrix, tunnel) with only their entry points adapted. `horizon.rs`
retains the shared camera and software raster helpers. System scenes read
`telemetry.rs`, sampled off the render path.
`raster.rs` and `text.rs` retain the upstream cell raster and pointer label font.
GTK, Wayland surfaces, producer threads, desktop pointer polling and audio
capture are not imported. Constructors use Patch's scene seed. Patch owns
motion time, audio bands, composition and transitions. The upstream licence is
in LICENSE. The enclosing Rust modules adapt these cells to Patch's Grid.
