# Patch

Patch is the audio environment for Omarchy: Patch becomes your system's
sound, with a mixer, EQ, meters, routing, recording, DJ decks, a
visualiser, podcast and live-coding tools. MIT licensed.

Install Patch: `curl -fsSL https://get.patchaudio.app | bash`
(https://patchaudio.app).

## Build

```sh
cargo build --release      # target/release/omarchy-patch
cargo test --workspace
```

Needs Rust (rust-toolchain.toml), pkg-config, gcc, libpipewire-0.3,
rubberband and sqlite. See docs/getting-started.md and the user manual in
docs/.

Releases and issues: https://github.com/novuon/patch-releases
