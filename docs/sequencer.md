# Sequencer

Open the **Sequencer** Unit from Patch's controller, or run:

```sh
omarchy-patch open sequencer
```

The first version is a native drum machine with eight original synthesized
sounds: sub kick, snare, closed and open hats, clap, low tom, rim and shaker.
The sound bank is generated before audio processing starts. Playback uses
48-voice polyphony with interpolation at the engine's sample rate, and goes
through Patch's master volume, EQ, output correction and limiter. It respects
system volume and mute. The sequencer runs in the audio engine and keeps
playing when its window closes.

## Layout

The redesigned UI is organised into three visual bands:

**Top — Transport & Tempo.** Play/Pause/Stop/Save/Load buttons, then a
card with BPM, Swing and Level sliders, plus the three presets
(Four on the floor, Broken beat, Machine funk). Page buttons (1–16 / 17–32)
switch the grid view.

**Middle — Grid.** Eight track rows × sixteen step pads per page. Each pad
shows its velocity as a fill height (taller = louder). Active steps glow
orange; the current playhead step gets a bright accent ring; pads beyond
the track length are dimmed. Track headers on the left show the voice name,
a mute toggle, and a micro level bar.

**Bottom — Controls.** Two cards: Track (length, gain, pan, clear) and
Step (velocity, probability, repeats). A status line shows messages.

## Make a pattern

Choose **Four on the floor**, **Broken beat** or **Machine funk**, then press
**Play** (Space). **Pause** holds the current position; **Stop** (Escape)
rewinds to step 1. Presets replace the current pattern.

Click a pad to toggle it. Right-click to select a step without changing it.
The lower controls edit the selected track and step:

- **Length**: 1–32 sixteenth-note steps per track. Different lengths create
  evolving polyrhythms. Pads beyond the track length are dimmed.
- **Gain / Pan / M**: track level, stereo position and mute.
- **Velocity**: 0–127; zero turns the step off.
- **Chance**: probability of playing that step on each pass, 0–100%.
- **Repeats**: 1–4 equally spaced hits inside a step; the probability decision
  applies to the complete group of hits.
- **Copy 1–16 → 17–32**: doubles the selected track to 32 steps for variations.
- **Clear track**: clears all 32 steps of the selected track.

The two page buttons switch between steps 1–16 and 17–32. Tempo ranges from
40–240 BPM; swing lengthens alternate steps while preserving each pair's
total duration. The orange outline follows each track's own position.

**Save** stores one pattern at `$XDG_CONFIG_HOME/patch/sequencer.json`
(default `~/.config/patch/sequencer.json`). **Load** restores that file.
These controls save the pattern, tempo, swing and mixer settings. Saving
replaces the previous saved pattern. Playback state is not saved; an engine
restart starts stopped with the default groove until you load your save.

## Scope of version one

This version provides a single editable pattern and one electronic drum kit.
Sample import, pitched tracks, MIDI clock, DJ transport sync, pattern chaining
and separate track outputs are not implemented yet. To record a groove, use
Recorder with the Patch program output. The sequencer's drums reach the mixer
on their own strip, `engine.sequencer` — fader, pan, mute, EQ, PFL and meters
like any other channel; the eight voices share that one strip, they have no
individual outputs yet. The master meter includes it, and the Pattern Unit's
voices have a strip of their own (`engine.pattern`).

Timing is driven by rendered audio samples, independent of UI refresh or
buffer size. Audio processing performs no file access, allocation or locking.
The engine JSON operation is `sequencer`: read with `{"op":"sequencer"}`,
control with `playing`, `stop`, `save`, `load`, or replace a validated `pattern`.
Use `status_only: true` for a lightweight playhead query.