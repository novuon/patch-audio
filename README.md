<p align="center">
  <a href="https://patchaudio.app"><img src="docs/readme/banner.png" alt="Patch: the Omarchy &amp; Linux audio environment" width="100%"></a>
</p>

<p align="center">
  <a href="https://github.com/novuon/patch-releases/releases/latest"><img src="https://img.shields.io/github/v/release/novuon/patch-releases?style=flat-square&label=release&color=F0243B" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/novuon/patch-audio?style=flat-square&color=141114" alt="MIT licence"></a>
  <a href="https://patchaudio.app/download"><img src="https://img.shields.io/badge/Omarchy-also%20Ubuntu%20%C2%B7%20Fedora%20%C2%B7%20Debian%20%C2%B7%20openSUSE%20%C2%B7%20Arch-141114?style=flat-square" alt="Omarchy, and Ubuntu, Fedora, Debian, openSUSE and Arch"></a>
  <img src="https://img.shields.io/badge/x86__64%20%C2%B7%20ARM64-141114?style=flat-square" alt="x86_64 and ARM64">
  <img src="https://img.shields.io/badge/Rust-141114?style=flat-square&logo=rust&logoColor=white" alt="Written in Rust">
  <a href="https://patchaudio.app"><img src="https://img.shields.io/badge/website-patchaudio.app-F0243B?style=flat-square" alt="Website: patchaudio.app"></a>
</p>

<p align="center">
  <a href="https://patchaudio.app"><b>Website</b></a> ·
  <a href="https://patchaudio.app/download"><b>Download</b></a> ·
  <a href="https://patchaudio.app/docs"><b>Documentation</b></a> ·
  <a href="https://github.com/novuon/patch-releases/releases"><b>Releases</b></a> ·
  <a href="https://github.com/novuon/patch-releases/issues/new?template=bug_report.yml"><b>Report a bug</b></a>
</p>

**Patch is the audio environment for Omarchy**, and it runs on other Linux distributions too. It becomes your system's sound: one real-time engine with a mixer, EQ, meters, routing, recording, DJ decks, a visualiser, and podcast and live-coding tools. The engine keeps playing when windows close or Patch updates.

<p align="center"><img src="docs/readme/screenshot.png" alt="Patch: the Mixer, the Mixer Inspector and the Router" width="100%"></p>

## Install

```bash
curl -fsSL https://get.patchaudio.app | bash
```

The installer downloads the newest release, checks its SHA-256 and asks how you use Patch: Standard (your sound, the Mixer and the EQ), DJ, Podcast, Live coding, Everything or Custom. Updating is the same line, and keeps your setup, library and settings. Add or remove modules later with <kbd>Ctrl</kbd>+<kbd>K</kbd> › Setup › Modules.

## What's inside

A selection of the units in Patch; there are more, and the [documentation](https://patchaudio.app/docs) covers them all.

| Unit | What it does |
|---|---|
| **Mixer** | Every app on its own channel strip, with sends to AUX 1 and 2, FX returns and post-fader meters |
| **Router** | Patch any source through processing (gate, EQ, compressor, limiter, VST3 plugins) to any output |
| **Meter** | Loudness in LUFS, true peak, VU and a live spectrum |
| **Recorder** | 24-bit with dither or 32-bit float, takes kept in your music folder |
| **DJ** | Decks, a crossfader, the Mini Booth and a library that analyses tempo, key and beat grid |
| **Mic and Podcast** | A voice chain per mic, remote guests with mix-minus, and Broadcast to Icecast or RTMP |
| **Network** | Send and receive audio over your local network: RTP, AES67 and VBAN |
| **Live coding** | Tidal, Pattern and the Sequencer, with the Visualiser on a second screen |

Keyboard first, with a phone remote. Made for Omarchy (Arch Linux with Hyprland 0.56 or newer), and it runs on other Linux distributions with PipeWire too: Ubuntu 24.04 and newer, Linux Mint 22, Debian 13, Fedora 40 and newer, openSUSE Tumbleweed and Arch. On PCs (x86_64) and 64-bit ARM, including Omarchy on Apple Silicon MacBooks.

## Build from source

```sh
cargo build --release --locked      # target/release/omarchy-patch
cargo test --workspace
```

Needs Rust (pinned in `rust-toolchain.toml`), pkg-config, gcc, libpipewire-0.3, Rubber Band and SQLite. The binary is called `omarchy-patch`, so it never shadows `/usr/bin/patch`. Start with [docs/getting-started.md](docs/getting-started.md); the user manual is in [docs/](docs/).

| Crate | What it does |
|---|---|
| `patch-engine` | The real-time PipeWire engine, with AVX2 assembly DSP |
| `patch-surfaces` | Every window, written in Slint |
| `patch-core` | Commands, layouts, keys and the IPC protocol |
| `patch-host` | Windows, Hyprland rules and the session |
| `patch-analysis`, `patch-library` | Tempo, key and beat-grid analysis, and the music library |

## Releases and issues

Ready-made builds, the release history and bug reports live in [novuon/patch-releases](https://github.com/novuon/patch-releases). Each release here carries the same version tag.

## Licence

Patch is [MIT licensed](LICENSE). Third-party notices are in [NOTICE.md](NOTICE.md) and [licenses/](licenses/).

[<img src="https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-dark.svg" alt="#MadeWithSlint" height="48">](https://slint.dev)
