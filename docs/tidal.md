# Tidal

The Tidal Unit is [TidalCycles](https://tidalcycles.org) native to Patch:
you write Tidal the way Tidal is written (`d1 $ every 4 (fast 2) $ s
"bd*2 [~ sn]" # room 0.3`), and Patch plays it on its own clock and its own
instrument. Nothing else to install: no Haskell, no GHCi, no SuperCollider,
no SuperDirt. It plays with Patch's own sounds out of the box, and with
any sample folders you give it (Tidal's Dirt-Samples layout). The music
keeps playing with every window closed.

Open it with `f` in the controller, `omarchy-patch open tidal`, or the
**Live Code** layout (`omarchy-patch layout live-code`: Tidal, the meter and
the Mixer on one workspace, the Visualiser on the next for a projector).

## The window

Code on the left, what plays on the right.

| Key | |
|---|---|
| Ctrl+Enter | run the block under the cursor (or the selection) |
| Ctrl+Shift+Enter | run everything |
| Ctrl+. | hush (silence every channel) |
| Ctrl+Space | play · stop |
| Ctrl+B | the backdrop: the Visualiser under the code (off · dim · bright) |
| Ctrl+H | show or hide the reference (narrow windows) |
| Tab | complete the word being typed (else indent) |
| Ctrl+/ | comment the line or selection out, or back in |
| Ctrl+Z · Ctrl+Shift+Z | undo · redo (typing, completions, inserted examples alike) |

**The code lights up as it plays.** Every word of the mini-notation an
event comes from is boxed while the event sounds: `bd` and `sn` in
`s "bd*2 sn"`, the `0` of `# n "<0 3>"` with them, a `let`'s words where the
`let` is written, a `0 .. 7` range whole. It follows what the engine plays,
so a block run from the CLI lights up too; a line you have edited since it
ran stays dark (that isn't what plays).

The code is coloured as you type: Tidal's words, controls, strings and
numbers in the theme's own colours, and a word Patch's Tidal doesn't know in
the error colour before you run it. The line under the code says what the
word at the cursor does (`every 4 (fast 2) $ p — Apply a function every n
cycles`) and what Tab would complete it to. Enter keeps the line's indent
(one step more after a line ending in `$`, `#` or an open bracket);
brackets and quotes close themselves.

A **block** is lines without a blank line between them, as in Tidal's own
editors. Running a block changes only the channels it names: `d2 $ …`
replaces d2, d1 plays on. The lines you ran flash in the gutter. Code with an
error is refused as a whole: its lines get a red dot, the errors say where
and why (`3:24: 'gian' isn't a word Patch's Tidal knows (did you mean
gain?)`), and **what was playing keeps playing**.

The code is the window's own, comments and all, saved as you type
(`~/.local/state/omarchy-patch/tidal-editor.tidal`). What plays is the
engine's (`tidal.json`): reopening the window, upgrading the engine or
restarting the computer brings both back.

**Channels** lists what plays: a light that flashes on each of the
channel's events, what it plays, and **M**ute, **S**olo and **✕** (silence)
buttons, which run `mute 1`, `solo 1`, `d1 silence` for you.

The gutter is a live playhead: `▌` flashes on the lines you ran, `▸` stays on
the lines whose channels are hitting right now (so a block sent from the
CLI or another window shows up too), and `●` marks an error. Each channel in
the list carries its source line (`L7`), so code and playing channels can be
read together. Running a block from this window still flashes exactly the
lines it ran.

**Reference** is every word with its usage and a line that plays it: search
it (`jux`, `scale`, `room`), click an entry and its example is added after
the block you're in, ready for Ctrl+Enter. **Start** has six whole tracks
(Night Drive, Broken Gravity, Acid Chapel, Glass Rain, Hyperglass, Slow
Orbit): each is added at the end of your code and played from a hush;
nothing of yours is replaced.

### The backdrop

Ctrl+B draws the Visualiser under the code, the code on a veil you can see
through (dim, then bright). It is the Visualiser's own show (the mode and
scene you chose there), moving to the music: the decks when they play,
otherwise your Tidal session's tempo, beat and the output's spectrum. On the
GPU it is drawn beneath the window, so it costs what a Visualiser window
costs. A Visualiser window elsewhere (a projector) keeps its own show.

## The language

What runs is Tidal as documented by the TidalCycles project. The whole
vocabulary is in the window's reference; in short:

```haskell
setcps (128/60/4)                       -- tempo: cycles a second (setbpm 128 too)

d1 $ s "bd*4"                           -- channels d1 … d16, or p "name"
d2 $ every 4 (fast 2) $ s "~ cp" # room 0.3
d3 $ n "0 .. 7" # s "superpiano" # octave 4
d4 $ jux rev $ n (scale "minor" "0 2 4 <7 9>") # s "supermandolin"

let hats = s "hh*8" # gain "1 0.7"      -- a named pattern
d5 $ sometimes (# speed 2) hats
let wob n = (# pan sine) . fast n       -- a function of your own
d6 $ wob 2 $ s "cp*2"

do d1 $ s "bd*2"                        -- several statements as one block
   d2 $ s "~ cp"

once $ s "cp*4"                         -- one cycle, now (a fill), beside what plays
xfadeIn 1 8 $ s "bd*2 [~ sn]"           -- crossfade d1 over 8 cycles (xfade: 4)
clutchIn 1 4 $ s "hh*8"                 -- event by event, at random, over 4 (clutch: 2)
jumpIn 1 2 $ …  ·  jumpIn' 1 4 $ …      -- after 2 cycles · at the next multiple of 4
d1 $ every 2 (\x -> x # speed 2) $ s "bd sn"   -- a function written in place

mute 2 · solo 3 · unmute 2 · unsolo 3 · unmuteAll · unsoloAll
d1 silence
hush
```

* **Mini-notation**: `~ [ ] < > * / ! @ _ ? | , {}%`, Euclid `bd(3,8,2)`
  (rotated earlier, as in Tidal), ranges `0 .. 7`, kit slots `drum:3`.
* **Functions**: application, `$`, `.` composition, lambdas (`\x -> …`),
  sections `(# speed 2)`,
  `(0.25 <~)`, `(+ n 12)`, lists `[a, b]`, pairs `(0, 0.5)`, patterned
  arguments (`fast "<1 2>"`).
* **Operators**: `#`, `|+ |- |* |/ |% |< |>` (structure from the left),
  `|+|` (both), `+|` (right), `+ - * /` on patterns, `<~ ~>`, `<>`.
* **Time and change**: fast, slow, hurry, rev, iter, iter', palindrome, ply,
  press, pressBy, swingBy, swing, linger, trunc, zoom, compress, fastGap,
  segment, inside, outside, every, every', whenmod, foldEvery, within,
  within', chunk, chunk', while, sometimes(By), often, rarely, almostNever,
  almostAlways, always, never, someCycles(By), degrade(By), unDegradeBy,
  brak, rot, shuffle, scramble.
* **Layers and structure**: jux, juxBy, off, superimpose, layer, stut, echo,
  echoWith (stutWith), stutter, ghost, spread, fastspread, stack, overlay,
  cat, fastcat, randcat, wrandcat, timeCat, wedge, append, fastAppend,
  squeeze, bite, chew, fit, plyWith, soak, select, pickF, fix, unfix, contrast,
  struct, mask, sew, stitch, inv, euclid, euclidInv, euclidFull, euclidOff,
  euclidOffFull, fadeIn, fadeOut.
* **Samples**: chop, striate, striateBy, slice, splice, randslice, loopAt,
  and the controls `begin end speed (below 0: backwards) unit accelerate
  cut loop coarse` (see [Samples](#samples)).
* **Harmony and signals**: note names (`c e g`, `fs4`), chords (`c'maj7`,
  `e'min7'ii`, `c'major'5`), scale (55 scales), toScale, arp (up, down,
  updown, downup, converge, diverge, pinkyup, thumbup …), rolled, rolledBy
  (strummed chords), sine, cosine, saw, isaw, tri, square, rand, irand,
  perlin, brand, brandBy, envL, envR, envLR, envRR, choose, chooseBy,
  wchoose, binary, binaryN, range, rangex, quantise, run, scan.

Not there yet: `anticipate` and `interpolate`, `ur`, `nTake`, and Haskell
beyond expressions (`where`, type annotations, `import`).
They answer with a message saying so, never with a wrong sound.

## Sounds

Patch's instrument plays the events: its own drum bank (the Sequencer's
designs) and nine synths, each voice with an envelope, filters, shaping,
crushing and pan, and a shared delay and room.

| Name | Plays |
|---|---|
| `bd kick 808bd 909` | kick |
| `sn sd snare 808sd` | snare |
| `hh hc ch hat hh27 808hc` · `oh ho 808oh` · `cr crash` · `ride` | hats and cymbals |
| `cp clap realclaps 808cp` | clap |
| `lt mt ht tom 808lt 808mt 808ht` | toms |
| `rim rs` · `cb cowbell` · `sh shaker perc` | rim, cowbell, shaker |
| `drum:0 … drum:7` (also `808`, `kit`) | the kit by slot: kick, snare, hat, open hat, clap, tom, rim, shaker |
| `superpiano piano` | struck piano tone |
| `supersaw saw superzow` | three detuned saws |
| `supersquare square superchip superpwm` | square |
| `supersine sine` | sine |
| `superfm fm` | two-operator FM, a bright attack |
| `superhammond organ` | drawbar organ |
| `supermandolin pluck arpy` | plucked string |
| `superreese reese bass` | detuned bass |
| `superpad pad` | slow, soft pad |

A name Patch has no sound for shows in the error colour as you type, is
said when you run the block (`d2: Patch has no sound 'arpeggio'`) and stays
silent; it is never played as something else. The same goes for controls
that change nothing yet (`vowel`, `orbit`, `squiz`): listed when you run,
carried along, ignored.

Each sound has its own trim so a hat sits under the kick, noisy voices are
high-passed by default and a dense chord does not push the master limiter;
`gain` remains the player's squared control. Drums have short natural
decays instead of ringing at full level for the whole sample.

**Controls**: `s n note octave gain amp velocity pan speed cutoff (lpf)
resonance (lpq) hcutoff (hpf) attack release sustain legato shape crush
delay delaytime delayfeedback room size`. `n` and `note` count from middle C
(`n "0"` is C5, MIDI 60), on a drum `note` retunes it; `gain` is squared, as
in Tidal's feel; `legato` sets a synth note's length in steps, `sustain` in
seconds; without either a synth note lasts its step. `delaytime`,
`delayfeedback` and `size` set the shared delay and room (the last event to
set them wins, as with SuperDirt's orbits).

## Samples

A folder of audio files is a sound. Put them in
`~/.local/share/omarchy-patch/samples/<name>/` (WAV, FLAC, AIFF, Ogg, MP3,
Opus): `s "name"` plays the first file, `name:3` or `# n 3` the fourth, in
name order, wrapping, as SuperDirt does. A SuperDirt install's Dirt-Samples
(`~/.local/share/SuperCollider/downloaded-quarks/Dirt-Samples`) are read
too, after Patch's own folder, so code written for SuperDirt plays as
written. A folder wins over a built-in sound of the same name: with a `bd`
folder, `bd` is that kick; without one, Patch's. `OMARCHY_PATCH_SAMPLES`
(paths with `:`) replaces the list of places looked in.

```sh
omarchy-patch tidal samples                              # what is there, what is loaded
omarchy-patch tidal sample add vox ~/Music/song.flac 61.2 63.4   # a new sample of vox, cut from a track
```

### From a deck

Press **x** on a deck (or click its sample key) and its window opens a
sample panel above the loop buttons. Choose what to cut (the deck's loop,
or the beat, bar, 2, 4 or 8 bars that just played, on the track's grid),
from the mix or one of the track's stems when they are prepared, as one
file or 4, 8 or 16 slices, and a name (the track's first word unless you
give one); **enter** saves it into that folder. The cut is the track as it
is, not as the deck plays it (no tempo, key, EQ or effects), so Tidal's
`loopAt` fits it to any tempo. On short decks, the sample controls take
the place of the scrolling waveform. On taller decks, the panel lists
what was made from the track with code that plays it (`loopAt 1 $ chop 8 $ s "midnight:2"`,
`n "4 .. 11" # s "kit"`) to copy. `omarchy-patch deck a sample` (or a
controller button mapped to `dj.sample` with action `save`) saves at once with the panel's
last choices.

New files are found the next time you run code; no restart. A block's
samples load before it plays, so its first hits sound; a sample a pattern
reaches only later (`n` from a signal, say) loads in the background and
skips that one hit. Samples stay loaded up to 1 GB; past it, those unused
for a while are let go. In the editor, Tab completes folder names inside
`s "…"` and the hint line says how many files a folder has.

Samples play in stereo through the same voice as everything else
(filters, shape, crush, pan, room, delay), with a 1 ms fade at each edge so
cuts don't click. The controls: `begin`/`end` (the part played, 0 … 1),
`speed` (below 0: backwards), `unit "c"` (speed in cycles: the sample fits
the cycle at any tempo), `accelerate` (the rate glides), `cut` (a cut group:
a new sound in it ends the last), `loop` (play the part n times), `coarse`
(sample-rate reduction), plus `legato`/`sustain` to cut a sample short.

```haskell
d1 $ loopAt 2 $ chop 8 $ s "break"            -- a two-cycle break, in time at any tempo
d2 $ splice 8 "0 [2 3] 4 <6 1>" $ s "break"   -- its slices re-ordered, each fitting its step
d3 $ striate 4 $ s "bd sn" # cut 1
```

## Changing a channel

`d1 $ …` replaces d1 at once. The transitions change it over time from the
cycle playing when you run them: `xfadeIn 1 8` crossfades at equal power
over 8 cycles, `clutchIn 1 4` hands it over event by event, `jumpIn 1 2`
switches after 2 cycles and `jumpIn' 1 4` at the next multiple of 4. The
channel list shows what is happening (`xfade · s "sn*4"`), the words that
play still light up, and the next block you run after it ends tidies the
channel back to its new code. `once $ …` plays one cycle on a channel of
its own (`once`, up to four at a time) and is gone after. Not playing,
there is nothing to change from: the new code takes over when you play.

## Tempo

Tidal starts at its own 0.5625 cycles a second (135 BPM, four beats a
cycle). `setcps` or `setbpm` in your code sets it; **+ / −** and **Own
tempo** in the window do too. **Follow deck** locks the cycle to the lead
deck's beat grid (four beats a cycle), so Tidal plays in time with a DJ set.

## Mixing

The Tidal Unit has its own mixer strip, `engine.tidal` (**Tidal**): fader,
pan, mute, EQ, PFL and meters, like any other. `omarchy-patch gain
engine.tidal -6`, `omarchy-patch mute engine.tidal`. These go through the
Mixer, which lists the strip when Tidal is among the modules you
installed; the engine op `{"op":"mute","channel":"tidal","muted":true}`
works either way.

## From the command line (and your own editor)

```sh
omarchy-patch tidal eval 'd1 $ s "bd*4"'     # run a block; prints the channels it changed
omarchy-patch tidal eval - < block.tidal     # the block on stdin
omarchy-patch tidal run examples/tidal/03-acid.tidal
omarchy-patch tidal hush | play | stop | status | events 0 2
omarchy-patch tidal tempo 128 | follow a
```

`tidal eval -` makes any editor a Tidal editor: send the block under the
cursor to it (in Neovim, `:'<,'>w !omarchy-patch tidal eval -`). Errors go
to stderr as `line:col: message` with exit status 1. The engine op is
`{"op":"tidal","action":"eval","code":"…"}` on the engine socket.

`examples/tidal/` has six sets (a first beat, broken techno, acid, chords
and arps, polymeter, ambient); a test plays every one.

## How it works

* `crates/patch-pattern/src/tidal/`: the language. `lex` and `parse` read
  Haskell's shapes (application, operators with Tidal's fixities, sections,
  lists, layout); `eval` gives every word its meaning on Patch's pattern
  algebra (`Pattern`, exact rational time); `notes` has note names, chords
  and scales; `Session` is what plays, channel by channel, and `Tidal` is
  the engine behind the scheduler. `help` is the window's reference; every
  example in it is run by a test.
* `crates/patch-pattern/src/instrument.rs`: sound names and controls as
  what the engine plays. `crates/patch-pattern/src/samples.rs`: the sample
  folders (an index, rescanned when a folder changes).
* `crates/patch-engine/src/instrument.rs`: the instrument (real-time safe:
  everything allocated up front, events on their exact sample from their
  own queue, a full voice table steals the quietest voice).
  `crates/patch-engine/src/sample_bank.rs` decodes sample files off the
  audio thread (WAV itself, other formats through ffmpeg) into a fixed
  table of slots; buffers move to the audio thread and back through
  mailboxes, so it never allocates or frees.
* `crates/patch-engine/src/pattern_service.rs`: the Tidal Unit runs its own
  pattern service (`Service::tidal`), on the same clock as the Pattern Unit
  but its own engine, queue, state and mixer strip, so the two play
  together.
* `crates/patch-pattern/src/tidal/editor.rs`: what the editor knows of
  the code (each character's colour, the word at the cursor, completion,
  indenting, pairs, comments) and `SourceMap`, which finds the code the
  engine keeps in the editor's text. Mini-notation words keep their place
  (`mini::parse_located`), values carry it (`eval::Loc`) and events report
  it (`Event::locs`, counted in `Session::sources`).
* `crates/patch-surfaces/src/tidal_view.rs`, `ui/tidal.slint`: the window.
  The text is drawn as one layer per colour over a transparent input
  (Slint's input has one colour), with the window's own cursor and its own
  undo (the input's forgets programmatic edits).

Patch's Tidal is written from Tidal's documentation; none of TidalCycles'
source (GPL-3.0) is used or translated, as with the Pattern Unit's reference
engine. See [third-party engines](third-party-engines.md).
