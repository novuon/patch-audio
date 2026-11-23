# Plugins Unit

Patch hosts VST3 effects as a rack. Each plugin runs where its row says:

* **Master**: the whole mix, after the master EQ, before Tune, the main
  fader and the limiter.
* **One channel**: a mic, an app, the engine's voices (any Mixer strip with
  an engine channel). The plugin takes the channel's input (after a mic's
  voice processing) before its EQ, fader and sends, so what the channel
  sends to AUX and FX is processed too. An app strip without a channel of
  its own is given one when you pick it, as its knobs would (four apps at
  once). Six channels can have plugins at once.
* **AUX 1, AUX 2**: what is sent there, after the bus's level, as it leaves
  Patch.
* **FX 1, FX 2**: after the bus's built-in effect (reverb, delay), before
  its return fader.

A podcast, say: a voice plugin on each mic, another on the caller's app,
a reverb plugin on FX 1 fed by the channels' sends. Several plugins at one
place run in rack order.

Plugins are not bundled. Download the vendor's Linux VST3 and press
**Install…** above the Installed list (or `omarchy-patch plugins install
<file>`): pick the `.zip` or `.tar.gz` (or, for a plugin already unpacked,
any file inside its `.vst3` folder) and its bundle goes into `~/.vst3`.
Patch also finds bundles put in `~/.vst3`, `/usr/lib/vst3` or
`/usr/local/lib/vst3` by hand (**⟳** looks again).

To uninstall one, point at its row in the Installed list and press
**Uninstall…**; the row asks again (**Keep** is the default) before the
bundle moves to the Trash, where a file manager restores it. Only plugins
in `~/.vst3` can be uninstalled, and not while the rack has them: remove
the plugin from the rack first (`omarchy-patch plugins uninstall <name>`
does the same).

A download is untrusted, and installing it runs nothing of it:

* Patch unpacks it itself, with no shell and no outside unpacker, and
  takes only the `.vst3` bundles in it (a readme, a CLAP or a Windows build
  beside them stays behind).
* Every entry must be a plain file or folder inside its bundle: a path
  out of it (`..`, `/…`), a link or a device file refuses the whole
  install. At most 20 000 entries, 2 GB unpacked (counted as it unpacks,
  not as the archive claims), 8 bundles.
* A bundle must hold a Linux library for this computer
  (`Contents/x86_64-linux/*.so`), checked from its ELF header without
  loading it; a Windows or macOS download is refused.
* It is unpacked into a private folder in `~/.vst3` and moves into place
  in one rename; one of the same name is swapped out whole (the line says
  Updated). Files get mode 644, folders 755.
* Installed is not added: **Add** first loads it in a probe process, as
  below.

## The rack

Open **Plugins** (`omarchy-patch open plugins`). The top panel is the
signal path as it runs now, a line for the master and one for each other
place with plugins, left to right in the engine's order:

* **Master**: **IN** (the Mixer's channels that reach MAIN and are not
  muted, those playing first; the dot is lit while one plays; the FX
  returns join here too), **EQ**, the master's plugins, **Tune** (room
  correction), **Fader** (MAIN's level), **Limiter**, **OUT** (the device
  MAIN plays to).
* **A channel**: its input, the plugins, its strip (EQ, fader, sends), the
  mix.
* **AUX**: its sends, its level, the plugins, out of Patch.
* **FX**: its sends, the built-in effect, the plugins, the return, the mix.

A plugin that is off is shown faded, "passed by"; click one to open its
window. Each plugin's window shows its own line, with itself outlined.
Below the path:

* **Installed**: every VST3 bundle found. **Add** first loads the plugin in a
  separate process (`omarchy-patch plugins probe`): a plugin that crashes or
  hangs on load is refused and never reaches the engine or a window. An
  added plugin starts off.
* **Rack**: the plugins in signal order. The round switch puts one on or off
  **while the music plays**: the plugin host loads it and it fades in over
  one block; off fades it out and unloads it. The place button (Master,
  Mic 1, FX 1 …) moves it elsewhere in the mix, live. **Open** shows its Patch window, **Original** opens the plugin's own
  editor floating (lit while it is open; again closes it), and 1× 1.5× 2×
  set that editor's size. ↑ ↓ reorder the chain, × removes the plugin. Each row shows its output level
  and how much of each block it takes.

Patch saves the rack in `$XDG_CONFIG_HOME/omarchy-patch/plugins.json` (or
`~/.config/omarchy-patch`). The 0.7.8 single-insert file is read as slot 1.

## A plugin's window

**Open** shows the plugin in its own window, `plugin#N` for slot N
(`plugin` for slot 1), titled with its name: Patch's face for the plugin.
Its header switches between two views, and the plugin remembers which it
opens with:

* **Patch**: Patch's face for any plugin. Its controls as knobs (drag up or
  down, Shift for fine, the wheel a step a notch, double-click for the
  default), switches and choices, with the plugin's own readouts ("−3.5
  dB"), grouped into sections by name (a strip's controls, "FX 57 …").
  Meters show the level into and out of the plugin on the master.
* **Original**: the plugin's own editor, exactly as its maker drew it, in a
  window of its own beside Patch's (Hyprland floats it at the editor's
  size, class `omarchy.patch.plugin`, titled "<plugin> · Original"). Close
  that window (or press Esc in it), or pick **Patch**, and the plugin is
  back on Patch's face.
  **Original** again brings an open editor forward.
  1× 1.5× 2× beside it zoom the editor on top of the screen's scale, in
  place, and the plugin remembers it. A zoom that would not fit the screen
  is shown as large as fits (Box Of Bass at 2× on a 4K screen at 1.6:
  1.86×). A plugin that does not scale its editor but lets the host size
  it (Graillon, Dplug's) is zoomed by size: its own size times the zoom.
  One that does neither says so and stays as it is.

Every change is live: a knob in Patch's face or in the plugin's own editor
reaches the plugin on the master at once, and the other view follows.
`On`/`Off` in the window is the rack's switch.

A plugin that is on shows its own editor from the plugin host, on the
instance that plays the audio, so its meters, spectrum and scopes move
(Patch asks over `$XDG_RUNTIME_DIR/omarchy-patch/plugin-editors.sock`).
The host serves the editor on its main thread, beside the audio worker:
VST3 keeps the editor (controller) and the audio (processor) apart, and
vst3-host is vendored (`vendor/vst3-host/PATCH.md`) to reach the
editor of a plugin the audio thread owns. An editor that crashes the host
takes the plugins out for a moment, as a plugin crashing on load does:
the master plays straight through, that plugin is held out, the host
starts again with the rest. A plugin that is off (nothing plays it) opens
its editor in a process of its own (`omarchy-patch plugins editor`), on
an instance of its own, as does one the host cannot show.

Either way the editor's mouse, keys and timers are served every 2 ms
while it has the keys (the pointer over it gives it them) and every 10 ms
otherwise, never from Patch's windows: they share one busy UI thread, and
an editor served from there got its events only as often as that thread
came round. The window carries the desktop's arrow pointer at the
screen's scale, which the editor's own windows inherit: under XWayland a
window without one shows no pointer. A window the plugin opens on its own
(MiniMeters' Debug window) is given the same arrow when it appears.

Each Patch plugin window holds a second instance of the plugin that never
processes audio and shows no editor; it names the controls and writes
their values. Settings travel as parameter values, so a plugin's state
that is not a parameter (some plugins' internal preset browser) stays in
the editor until it is saved as parameters.

## Audio

Plugins never run in the engine. When a plugin in the rack is on, the engine
starts a plugin host (`omarchy-patch plugins host`, one process for the
rack), and each place with plugins crosses to it and back through shared
memory without a lock: this adds one graph block of latency at that place
(none where no plugin is on), about 5 ms at 256 frames. On one deck that
puts it a block behind the other deck; elsewhere it only delays that
place. The host's real-time worker (`patch-vst3`, FIFO 75)
runs the chain; if it misses a block Patch plays the block dry instead of
waiting. A plugin that returns invalid samples is replaced by its input for
that block.

If a plugin crashes, the host goes down with it and the engine does not:
the master goes straight through at once (the music never stops), and the
plugin that was running is held out of the chain and shown as crashed in
the rack, while a new host starts with the others. Switch the crashed
plugin off and on to try it again. A host that stops answering for a
second (a plugin stuck in a loop) is killed and treated the same way. If
the host keeps dying without a plugin to blame, three times in a minute,
the rack stays off until a plugin is switched.

Plugins are prepared for 256-frame blocks; longer graph blocks reach them in
256-frame pieces. Some plugins do their prepared maximum's work on every
call (Box Of Bass at 8192: 8.4 ms per 5 ms quantum, so the master stayed
dry). The engine runs plugins at 48 kHz; at other graph rates the rack is
bypassed.

## CLI

```sh
omarchy-patch plugins status                 # the rack, and what the engine runs
omarchy-patch plugins installed              # VST3 bundles on this machine
omarchy-patch plugins uninstall gluon        # ~/.vst3 → the Trash (not while in the rack)
omarchy-patch plugins add graillon           # by name or bundle path (probed first)
omarchy-patch plugins on 2 | off 2           # live
omarchy-patch plugins move 2 up|down
omarchy-patch plugins place 2 mic-1          # master | mic-N | app-N | aux-1 | fx-2 …
omarchy-patch plugins view 2 patch|original  # what Open shows
omarchy-patch plugins parameters 2
omarchy-patch plugins set 2 <parameter-id> <0..1>   # live
omarchy-patch plugins remove 2
omarchy-patch open plugin#2                  # slot 2's window
```

## Plugins tried

| Plugin | Linux VST3 | Notes |
|---|---|---|
| Box Of Bass 1.2.2 (Box of Rules) | yes | 113 controls in channel strips |
| Graillon 3 FREE 3.2 (Auburn Sounds) | yes | free download, direct |
| Fifth Order 1.0.2 (m.media) | yes | free, download link by email |
| Voxengo OldSkoolVerb, Boogex | no | Windows and macOS only |
