# Third-party engines

Patch is MIT. During the prototype phase (ADR-009) some Units can use
mature external engines. They are **never bundled or copied into Patch**:
each runs as a separate process, is installed separately, and keeps its own
license. Patch's adapter code (the glue that speaks Patch's protocol) is MIT
and lives in `adapters/`.

| Engine | Version | Source | License | Used by | How Patch connects | Distributed by Patch? |
|---|---|---|---|---|---|---|
| Strudel (`@strudel/core`, `@strudel/mini`, `@strudel/transpiler`) | 1.2.6 (pinned) | https://codeberg.org/uzu/strudel | AGPL-3.0-or-later | Pattern (prototype engine) | `node --permission main.mjs` in `adapters/strudel`, JSON lines on stdin/stdout | No: `npm install` in the adapter directory fetches it from npm |
| hydra-synth | 1.4.0 (pinned) | https://github.com/ojack/hydra-synth | AGPL-3.0 | Visualiser (prototype generative engine: shader generation only) | Node sidecar in `adapters/hydra`, JSON lines | No: installed separately |
| Node.js | ≥ 22 (permission model) | https://nodejs.org | MIT (and bundled licenses) | runs the sidecars | `$PATCH_NODE` or `node` on `PATH` | No: a system package |

## Notes

* **AGPL**: the engines run unmodified, in their own processes, installed by
  the user from their upstream packages. If Patch ever distributes them (a
  package that installs them, a modified copy), the AGPL's source-offer and
  notice obligations apply to that distribution. Attribution alone is not
  enough. Keep them out of the Patch package until that is settled.
* `adapters/strudel/stubs/kabelsalat` is Patch's own MIT stub for a broken
  upstream dependency (see finding P5); it contains no third-party code.
* No third-party source is translated into Patch's Rust code. The reference
  engine in `patch-pattern` is written from the published semantics and the
  textbook algorithms (e.g. Bjorklund/Toussaint), and is checked against the
  engines by tests, not derived from their code.
* **TidalCycles** (GPL-3.0) is not an engine Patch runs: the Tidal Unit
  (`patch_pattern::tidal`, [Tidal](tidal.md)) is Patch's own implementation
  of the language as Tidal's documentation describes it (its syntax, its
  vocabulary's behaviour, the music-theory tables of chords and scales). No
  Tidal, SuperDirt or SuperCollider code is used, translated or installed,
  and no samples are: the sounds are Patch's own instrument. "TidalCycles"
  names the language it speaks, not something Patch ships.

## Installing (development)

```sh
cd adapters/strudel && npm install   # Strudel, pinned
```

Patch looks for an adapter in `$PATCH_STRUDEL_DIR`, then
`~/.local/share/omarchy-patch/adapters/strudel`, then the source tree.

## Native Ambient scene adaptations

The Visualiser includes selected MIT scene logic from
[avillagran/omarchy-ttfx-layer](https://github.com/avillagran/omarchy-ttfx-layer),
revision `c969b6c172108c5f6ab937d99db67c17ddd28196`: Sunset Drive, Jellyfish and
Event Horizon. These are native cell-stage modules, not an external engine
adapter. Patch supplies rendering, timing and music signals. The desktop
supervisor, GTK/Wayland application, audio capture and vendored ttfx engine
are not included. Upstream copyright and licence are preserved in
`crates/patch-visual/src/ambient/LICENSE`; adaptation notes are alongside it.
