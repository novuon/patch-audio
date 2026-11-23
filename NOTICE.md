# Third-party software

Patch's own source code is under the [MIT License](LICENSE). It builds on
the following work.

## Linked system libraries

| Library | Licence | Used for |
|---|---|---|
| [PipeWire](https://pipewire.org) | MIT | the audio graph (the engine is a pw_filter) |
| [Rubber Band Library](https://breakfastquay.com/rubberband/) | GPL-2.0-or-later (or commercial) | keylock and key shift in normal builds |
| [FFTW](https://www.fftw.org) (through Rubber Band) | GPL-2.0-or-later | Rubber Band's FFTs in normal builds |
| [libsamplerate](https://libsndfile.github.io/libsamplerate/) (through Rubber Band) | BSD-2-Clause | resampling in normal builds |
| [SQLite](https://sqlite.org) | public domain | the library database |
| fontconfig, FreeType | MIT-style, FTL | finding and drawing fonts |

Public tester kits are built with `PATCH_KIT=1`. That build selects
[Signalsmith Stretch](https://github.com/Signalsmith-Audio/signalsmith-stretch)
and [Signalsmith Linear](https://github.com/Signalsmith-Audio/linear) (MIT,
compiled in from `crates/patch-engine/vendor/signalsmith`) and does not link
Rubber Band or FFTW. The public-kit workflow checks the built binaries for
those dynamic dependencies before packaging.

## Programs Patch runs

FFmpeg and ffprobe (decoding, analysis; LGPL/GPL, used as separate
programs), PipeWire's and PulseAudio's tools, notify-send, fc-match,
zenity, hyprctl. They are called as programs, not linked or included in the
tester kit.

## Rust crates

The Rust dependency graph uses permissive licences (MIT, Apache-2.0, BSD,
Zlib, ISC, Unicode, BSL, CC0, Unlicense) except for Slint, which offers a
royalty-free desktop license alongside GPL and commercial options:

- **[Slint](https://slint.dev)**, the UI toolkit: GPL-3.0-only, or the
  [Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0](https://slint.dev/terms-and-conditions#slint-royalty-free-license-20)
  (with attribution), or a commercial licence.

Notable others: [ttfx](https://github.com/omacom/ttfx) (MIT; the
Visualiser's easing curves; a Rust port of
[TerminalTextEffects](https://github.com/ChrisBuilds/terminaltexteffects)
by ChrisBuilds, MIT) and [fontdue](https://github.com/mooman219/fontdue)
(MIT/Apache-2.0/Zlib). `cargo metadata` lists them all with their
licences.

## What this means for a binary

A normal build links Rubber Band and FFTW (GPL-2.0-or-later) and Slint,
so distributing it under the GPL-3.0 option for Slint means distributing
the combined work under GPL-3.0. The Arch package built from this
repository uses that route; its source stays MIT, and corresponding source
must accompany any distribution of the combined binary.

The public tester-kit build does not link Rubber Band or FFTW. It uses
Slint's royalty-free license for desktop applications, with Slint's
attribution badge on the public release page it is downloaded from
(github.com/novuon/patch-releases; the license's section 2(b)). Each kit includes the
generated Rust crate license inventory and texts, the full Slint license,
and both vendored Signalsmith MIT licenses. This license path applies to
the tester kit; it does not change the license or linking of normal builds.

Built with [Slint](https://slint.dev).
