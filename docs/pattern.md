# Pattern

The Pattern Unit (`t` in the controller, the fourth workspace of the Dev
layout, or `omarchy-patch open pattern`) plays live-coded patterns against
Patch's own clock: drum voices, derived material, deck and stem moves, and
the Visualiser's parameters. The pattern runs in the engine, so it keeps
playing with every window closed.

## The window

Code on the left, the running pattern on the right.

| Key | |
|---|---|
| Ctrl+Enter | evaluate; it takes over on the next cycle |
| Ctrl+Shift+Enter | evaluate now |
| Ctrl+Space | play · stop |

Ctrl+Enter from stopped evaluates and starts playing. Code that doesn't
parse is shown as `line:col` errors and **the previous pattern keeps
playing**. The right side shows the cycle, the tempo (following a deck,
or the pattern's own BPM), the engine and what it can do, the layers, the
next events, and timing (evaluation, query, late events, swap latency,
deliveries).

The **Patch / Strudel** button chooses the language of the code you write.

## The Patch language

One layer per line: a target, its pattern in mini-notation, then
transformations.

```text
cycle 4                                  // beats per cycle (a 4/4 bar)
drums: s("bd*4 [hh hh] sd").every(4, fast(2))
hats:  s("hh*8?").sometimes(rev)
deck(a).filter("<200 800 1600 400>")
stem("vocals").sometimes(mute)           // deck A's vocals; deck(b).stem(…)
visual.intensity("<0.2 0.4 0.8 1>")
visual.hue(sine.slow(4))
scene("<tunnel nebula prism>/2")         // the Visualiser's generative scene
kit("t1", "kick ~ snare ~")              // a temporary kit's slots
sample("dm-0123456789abcdef", "1 ~ 0.5 ~")
sample("derived:deck-a:vocals:phrase-last:beats-2", "1 ~ ~ ~")
```

There is no host-language evaluation: the code becomes a Patch program
(a serialisable tree) and nothing else.

### Derived material

`sample("derived:…")` names material by what it is, and the engine cuts it
from the track on that deck when you evaluate:

`derived:<deck>:<stem>:<region>[:beats-<n>|beats-<from>-<to>][:as-<kind>]`

* deck: `a` … `d` (or `deck-a` …)
* stem: `vocals`, `drums`, `bass`, `other`, or `mix`
* region: `phrase-last`, `phrase-<n>`, `before-drop`, `after-drop`,
  `bar-<n>`, `bars-<n>-<count>`, `beat-<n>`, `beats-<n>-<count>`,
  `section-<kind>`
* kind (otherwise inferred): `as-bar_loop`, `as-vocal_chop`, …

Loops get a seamless loop point, one-shots a transient-snapped start; every
edge is faded. The address is resolved once, at evaluation: if the deck
later loads another track, the playing pattern keeps its material. Until
the material is ready its events are skipped (and counted as "not ready"),
never waited for. Material plays through Patch's sampler on its exact
sample; a loop retriggered restarts in phase (the running one fades out),
and stopping the pattern fades every sample out.

Strudel code can play material by id (`sample("dm-…")`), not by address.

## Examples

`examples/patterns/` holds ready-to-play programs (`omarchy-patch pattern
load examples/patterns/03-peak-time-techno.patch && omarchy-patch pattern play`):

| File | What it does | Touches decks |
|---|---|---|
| `01-deep-house-pulse` | four on the floor, Euclidean hats, drifting rims, flammed claps; `pulse` rings | no |
| `02-broken-polymeter` | kick in 4, toms and rims in 3 and 5 against it, an iterating snare with an echo; `lattice` | no |
| `03-peak-time-techno` | rolling 16ths, rotating Euclidean percussion, an 8-bar build into the drop; `prism`, symmetry snapping | no |
| `04-visual-score` | no sound: a 32-bar visual arrangement for any set (follow a deck) | no |
| `05-dub-filter-deck-a` | deck A's filter swept and its vocals dropped out, dub style | **yes** (deck A) |
| `06-vocal-chops-deck-a` | chops and a drum loop cut from deck A's track, played through the sampler | no (reads deck A's track) |
| `07-aurora-cathedral` | a slow 32-bar nebula bloom with a long hue drift | no |
| `08-chromatic-machine` | sharp lattice/prism geometry with a compact intensity peak | no |
| `09-tidal-glass` | spacious cells and prism forms with slow color and motion tides | no |
| `10-slow-drift` | a calm nebula drift with visible changes every 8 bars | no |

A pattern's `visual.intensity|motion|symmetry|density|feedback|hue(…)`
steers the matching part of the visual intent, so it shapes whichever
generative scene is showing. A continuous signal needs `.segment(n)` to
become events (`visual.hue(saw.slow(32).segment(4))`).

Every example is checked by a test on the reference engine and, when
installed, on Strudel.

## Strudel

With the Strudel adapter installed, Strudel code runs as Strudel
(`adapters/strudel`: `npm install` there once; it runs sandboxed under
Node's permission model). Patch still owns the clock: Strudel only answers
"which events start in this span". Without the adapter, the Patch engine
plays Patch code; **Switch engine** cycles auto → strudel → reference.
See [third-party engines](third-party-engines.md) for licences.

## Tempo

**Follow deck** locks the pattern's cycles to the lead deck's beat grid
(the one the crossfader favours among those playing); **Own tempo** keeps
the current BPM. From the command line: `omarchy-patch pattern follow a`
or `omarchy-patch pattern tempo 124`.

## Mixing

The pattern's voices reach the mixer on their own strip, `engine.pattern`
(shown as **Pattern**, after the microphones): fader, pan, mute, EQ, PFL and
meters all apply, and `omarchy-patch gain engine.pattern -6` or
`omarchy-patch mute engine.pattern` work like any other strip. The step
Sequencer's drums have theirs, `engine.sequencer`. Nothing is summed straight
into the master any more, so muting either strip really silences it.

## Command line

```sh
omarchy-patch pattern load set.patch [patch|strudel]
omarchy-patch pattern play | stop | status | describe
omarchy-patch pattern events 0 4          # what plays in cycles 0…4
omarchy-patch pattern timeline visual     # what the Visualiser reads
omarchy-patch pattern engine auto|strudel|reference
omarchy-patch material stats | cleanup | recipes | state <id>
omarchy-patch visual-intent set section=drop intensity=0.9 hold_s=32
```

## For the AI director and scripts

The engine's `pattern` op (one JSON line on
`$XDG_RUNTIME_DIR/omarchy-patch/engine.sock`) also has AI tools that work
on Patch programs, never on engine code:

| action | |
|---|---|
| `describe` | layers, notation, words, transforms, events per cycle, a text summary |
| `mutate` | `{"at":{"layer":"drums","path":[1]},"edit":{"edit":"wrap","transform":{"fast":"2"}}}`; edits: `replace`, `wrap`, `unwrap`, `mute`, `unmute`, `add_layer`, `remove_layer`; `"load": true` plays the result |
| `diff` | structural changes from the running program to `b`, with sentences |
| `compare` | density and onset/event overlap of the running program and `b` over N cycles |

A path counts children: a sequence's steps, an alternation's or stack's
members, and a transform's pattern (0). AI-written patterns must be
programs (`origin: "ai"` refuses source code) and run under Auto Mix's
command limits. `visual_intent` (`get`, `set`, `clear`) carries what the
picture should mean to the Visualiser; see [Visualiser](visualiser.md).
