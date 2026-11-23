# Patch user manual

Patch brings your computer's audio into one place. Use it to choose where sound
goes, balance applications and microphones, play music, mix DJ sets, record and
broadcast.

Each part of Patch is called a **Unit**. Mixer, Router, Library and Recorder are
Units. You can open one on its own or use a **layout** that opens several together.
The audio service runs separately from those windows.

**Start from the bar’s audio menu to open a layout. Press Ctrl+K in any Patch
window to open the controller and choose Units, layouts or Setup.**

**Closing a window does not stop its audio.** Decks, Player, recording and
broadcasting have their own stop controls. At login, the audio service starts;
Patch does not open windows automatically.

This first edition covers everyday use on Omarchy. Some Units depend on the
modules you selected during setup. The detailed guides linked throughout cover
advanced controls.

Patch’s main pink/red accent in Patch Dark is **`#F0243B`** (RGB **240, 36, 59**).

## Contents

1. [Open Patch and choose a workspace](#1-open-patch-and-choose-a-workspace)
2. [Get sound to your speakers or headphones](#2-get-sound-to-your-speakers-or-headphones)
3. [Balance sound in the Mixer](#3-balance-sound-in-the-mixer)
4. [Play a file or stream](#4-play-a-file-or-stream)
5. [Build your music library](#5-build-your-music-library)
6. [Mix two tracks yourself](#6-mix-two-tracks-yourself)
7. [Use the Auto Mix](#7-use-auto-mix)
8. [Record and listen back](#8-record-and-listen-back)
9. [Set up a microphone](#9-set-up-a-microphone)
10. [Show visuals and broadcast](#10-show-visuals-and-broadcast)
11. [Save your workspace and settings](#11-save-your-workspace-and-settings)
12. [Solve common problems](#12-solve-common-problems)
13. [Find more help](#13-find-more-help)
14. [Keyboard reference](#14-keyboard-reference)

## 1. Open Patch and choose a workspace

### Open a layout from the menu bar

The bar's audio menu is the everyday starting point for Patch. Its **Patch**
section, above the output devices, shows what runs and opens what you need.

1. Click the **audio/volume icon in the Omarchy bar**.
2. Type in **Open a Unit or layout…** (the menu opens with the keyboard
   there): `simple`, `ai`, `deck`, `rec`. Press **Enter**, or click one of
   the four picks below the search.
3. A layout opens on the next empty workspace. Start with **Simple** for
   everyday audio: Mixer, Router and Player open together.

Choosing a layout closes the existing Patch windows and opens the chosen
arrangement. **The audio keeps playing while you change layouts**, and with
every window closed.

What keeps playing has a row of its own in the menu, as long as it runs: the
Auto Mix's set, the decks, the Player, Tidal, a recording, a broadcast. A row
says what plays and has its buttons: **Pause** and **End set** for the Auto Mix,
**Stop** for a recording or a stream (they ask once). Click the row, or one of
the buttons under it (**Auto Mix**, **Deck A**), to bring back just that window.
While Patch records or broadcasts, the bar's audio icon has a small red dot.

**Windows** lists the Patch windows on screen; **Close all** closes them
across workspaces without stopping playback, recording or broadcasting.
Click the line for each window and the layout's **Show**, **Reset** and
**Save as…**.

If the Patch section is missing, the bar integration may not be installed.
See [the bar's audio menu](layouts.md#the-bars-audio-menu). You can still open
Patch from the launcher as described below.

### Use Ctrl+K to open the controller

With a Patch window focused, hold **Ctrl** and press **K**. This opens the
**Patch controller**, your central place to:

- Open an individual Unit, such as Mixer, Library or Recorder.
- Choose a layout for the task you want to do.
- Close Units or layouts.
- Open **Setup** to choose modules and the default layout.
- Check the audio engine's state.

For example, if you are DJing and want to check a recording, press **Ctrl+K**
and choose **Recorder**. You do not need to replace the whole layout just to
open another Unit. In the controller the arrow keys move and **Enter** opens;
each Unit's letter, shown beside it, opens it directly (**R** for Recorder).
**Ctrl+Q** closes all Patch windows; the music keeps playing. See
[Keyboard reference](#14-keyboard-reference) for every key.

Ctrl+K works inside Patch windows; it is not a desktop-wide shortcut. When no
Patch window is open, use the bar's audio menu, or press **Super+Space** and
choose **Patch** to open the controller. Layout launchers such as **Patch Audio**
and **Patch Auto Mix** open their layouts directly.

### Choose a layout

| You want to… | Choose this layout |
|---|---|
| Control everyday computer audio | Simple |
| Mix with two decks | Basic DJ |
| Order and crossfade your own tracks | Auto Mix |
| Use decks, mixing controls and visuals across workspaces | Performance DJ |
| Work with microphones, recording and streaming | Podcast & Broadcast |
| Work with detailed mixing and analysis | Pro |

Layouts open on the next empty workspace. You can also open individual Units
from the controller. A layout missing from the list may need a module enabled
in **Setup**.

Terminal equivalents:

```sh
omarchy-patch                      # open the controller
omarchy-patch layout               # open your chosen default layout
omarchy-patch layout simple
omarchy-patch open mixer
```

Press **?** for the current window's help. Shortcuts depend on which window has
focus; follow the controls and help shown there. Text fields keep their normal
typing behaviour.

If a window says it is too small, choose **Make room** to float and enlarge it,
or **Keep small** to accept its current size. Keeping it small can leave some
controls out of view.

See [Layouts](layouts.md) for the full list and custom arrangements.

## 2. Get sound to your speakers or headphones

Start with a familiar audio source, such as a music file in Player or a song in
your usual music application.

1. Open the bar's audio menu.
2. Choose the physical output you want: speakers, headphones or an audio interface.
3. Check that the main volume is raised and unmuted.
4. Start playback. Open Mixer and check for activity on the source's meter.

Patch follows the output selected in the bar. Choosing a physical device gives
you the plain mixer. Choosing **Patch** exposes the full console and professional
routing controls, retaining the current hardware output path.

Use **Router** to inspect connections. Routing means choosing a sound's source
and destination; Mixer sets how much of that sound you hear. With Patch managing
system audio, application sound continues through Patch even when a physical
output is selected in the bar.

### Main output and headphone cue

**Main** is the mix sent to your audience or speakers. **Cue** lets you check a
track in headphones before bringing it into Main.

Independent Main and Cue need separate output ports. In Patch routing mode,
assign Main and Headphone Cue to different ports or stereo pairs. They can use
different pairs on the same audio interface. Cue needs a stereo pair, and its
ports cannot overlap Main's.

Choosing a normal physical output in the bar disables the professional Cue
routes. If you have only one stereo output, do not expect an independent
headphone mix from it.

See [Core routing](core-routing.md) for port assignments and capture routing.

## 3. Balance sound in the Mixer

A **channel strip** contains the controls for one sound source. Strips appear for
actual application streams, assigned inputs and active Patch sources. A browser
can supply several separate streams; Patch can control only the streams the
browser makes available.

1. Play the source you want to adjust and find its moving meter.
2. Move that channel's fader to change its level in the mix.
3. Use **Mute** to silence it. Check for active **Solo** controls if other sources
   unexpectedly disappear.
4. Adjust Main for the overall listening level.

A meter shows signal level; a raised fader alone does not mean a source is
playing. If levels clip or sound distorted, reduce the source or channel level.
Making everything louder does not fix distortion already present in the source.

In the full console, Main, AUX and the effects returns sit at the right edge.
An effects return carries processed sound back into the mix. Start with the
source faders and Main before adjusting sends and returns.

Open **EQ** to change the tonal balance of an available channel. Select the
channel you intend to change before adjusting it. Use bypass to compare the
processed and unprocessed sound. Routing connections belong in Router.

### Reading the meters

- **Peak** shows short, high signal levels. Watch it for overloads.
- **RMS** describes average signal level over a short period.
- **LUFS** describes perceived loudness. Integrated LUFS covers the measured
  passage or take rather than just the latest instant.
- **dBFS** measures digital level, with 0 dBFS at full scale.

Meter and Analyzer provide more detail than a strip's level meter. Use them
when checking levels, stereo balance or frequency content.

## 4. Play a file or stream

Use **Player** for a file, a saved recording or an HTTP(S) audio stream. Use the
DJ decks when you need cue points, beat matching or transitions between tracks.

1. Open Player from the controller.
2. Choose **Open file**, or enter a file path or stream URL.
3. Use Play/Pause, seek and volume as needed.
4. Enable repeat if you want a timed file to loop.

Player's shortcuts include **Space** for play/pause, **O** for the file chooser
and **L** for repeat. Seeking requires a source with a known duration.

**Stop** rewinds a timed file; for a stream without a known duration, it closes
the stream. Closing the Player window leaves playback running. Reopen Player
to control it again.

The **waveform** shows the whole file; click it to go there. **Queue** lines
files up to play in order and lists what you played lately. A long file (a
podcast, an audiobook) goes on where you left it; **Stop** starts it over.
The speed button plays from 0.5x to 2x with the pitch kept, and **A-B**
loops a part.

**Carts** (for a podcast or a broadcast): the pads under the transport start
a sound at once, over whatever else plays. Click a free pad to give it a
sound, then press it, or its key on the number row, to play it and again to
stop it. Four banks hold intros, commercials, B-roll and effects; **Fade**
and **Stop** end everything that plays. Right-click a pad to name it, loop
it or set its level.

See [Recorder and Player](recording-playback.md).

## 5. Build your music library

The Library is your catalogue for DJing. Patch finds and analyses music in
`~/Music`. You can add other folders and removable drives as **sources**.

1. Open Library.
2. For a connected removable drive, use **Add drive** when the Library offers it.
3. Under **Where**, select a folder or drive to browse its tracks. The arrow opens that place and closes it again. Or browse all tracks. **New collection** makes a collection from a folder, or an empty one. **Add to collection**, on the highlighted track, puts that track into a collection. **Make collection** saves the folder you are browsing as a collection. The **Show** well above the folders is the filters, as chips, so they are not another folder or collection. They combine: what fits the playing deck, favourites, a straight grid, and when a track was played. **All**, **Ready**, **Check**, **Waiting** and **Failed** on the search row combine with those too. **Clear** turns them off. The line above the list is the highlighted track’s file, its directory, and when it was last played. The footer says what this view is showing, what the librarian is doing, and how the whole library stands.
4. Let analysis finish before relying on automatic beat matching.

To add another folder from a terminal:

```sh
omarchy-patch library add "/path/to/music" "My music"
omarchy-patch library rescan
```

| Track status | What to do |
|---|---|
| **Ready** | Ready for deck, sync and AI use; listen and check important tracks before a performance. |
| **Check** | Read the reason and check the analysis, especially the beat grid. The file can still play. |
| **Waiting** | Leave Patch time to analyse it. |
| **Failed** | Read the error and check that the file is available and readable. |

A **beat grid** marks where Patch thinks the beats and downbeats fall. Sync
follows that grid. If the grid is wrong, two synced decks can still sound out of
time. A confidence or readiness label does not replace listening.

Use Library search to narrow the list. In its keyboard controls, **A/B** load
the selected track onto a deck (**C/D** onto decks C and D), **Q** queues it
and **Shift+C** adds it to a collection. Confirm the destination deck
before loading a track during a set.

Unplugged sources remain listed with their tracks offline. Reconnecting the
drive makes those files available again. Removing a source from Library does
not delete its music files. Patch does not write its analysis into those files.

See [Library](library.md) for file moves, duplicate copies and analysis storage.

## 6. Mix two tracks yourself

Open the **Basic DJ** layout. It puts Deck A, Crossfader, Deck B and Library
together. Start at a comfortable listening level.

1. Load a track onto Deck A and another onto Deck B.
2. Set the crossfader towards A and start Deck A.
3. If you have a separate headphone Cue output, cue Deck B in headphones.
4. Find a suitable starting point on B. Check its tempo and beat grid.
5. Use Sync if appropriate, then listen to the alignment.
6. Start B at your chosen phrase and bring it into the mix with its level,
   EQ and crossfader controls.
7. When B has taken over, pause A before loading its next track.

The **crossfader** balances the decks: left favours A, centre allows both and
right favours B. Channel levels and EQ also affect what you hear.

**Keylock** keeps pitch steady while tempo changes. **Cue** returns to a saved
starting point; hot cues recall other saved positions. **Loop** repeats a section.
Check the window's help for the current controls before using them in a live set.

During a synced overlap, the incoming deck follows the outgoing deck. Once the
handover finishes, the new lead deck does not need to remain synced to the
paused deck. Its Sync button being off at that point is expected.

If Auto Mix is in Auto, moving a deck or mixing control takes over from it and
returns it to Suggest. Check the mode before expecting automatic mixing to resume.

See [DJ Booth](dj-booth.md) for cueing, loops, EQ, effects and transport details.

## 7. Use the Auto Mix

Open the **Auto Mix** layout, or open the DJ workspace from the controller. Its
main tabs are **Play**, **Prepare**, **Recordings** and **Reports**. In narrow
windows, Prepare and Recordings may be labelled Sets and Takes.

The Auto Mix orders and crossfades tracks from your library, beat-matched and in key. It does not generate, compose or create music. It uses the library's analysis to choose the order and plan each transition. You choose how much control it has.

| Mode | What happens |
|---|---|
| **Observe** | You operate the decks. Auto Mix listens and does not suggest or change playback. |
| **Suggest** | Auto Mix ranks tracks. You choose, load and mix. |
| **Prepare** | Auto Mix loads, cues and plans the next mix, then waits for your approval. |
| **Auto** | Auto Mix chooses the order and runs the transitions. Some detail views call this **ACT**. |

Start with **Suggest** to learn what it chooses and why. Expand **Auto Mix detail** to
inspect candidates, reasons and the transition plan.

### Prepare and start a set

1. Open **Prepare** and name the set.
2. Select its source collections. Selecting none makes the whole library
   available; selecting several combines them.
3. Add an ordered track list if you want those tracks to play before automatic
   selections.
4. Choose the duration, energy arc, tempo corridor and Auto Mix mode.
5. Read the readiness checks, resolve missing files and **Save**.
6. Choose **Start set** explicitly when you are ready to play.

Saving a draft does not start playback. Editing another draft while a set plays
does not change the current queue.

### Steer the set while it plays

- **Build / Hold / Release** steer the energy direction. They do not directly
  set the tempo; an active set arc also guides the journey.
- **Change next** rejects the current next choice and requests another.
- **Mix now / Skip** requests a transition into the next track on a bar.
  It is not an immediate cut.
- Queue arrows reorder pending tracks. Remove offers Undo.

Watch the actual loaded decks and next-track display. A Library row selection
is a browsing choice, not proof of what Auto Mix will play next.

### Pause, finish or take over

| Action in the DJ workspace | Result |
|---|---|
| **Pause** | Pauses both decks and freezes the set arc, including an active transition. |
| **Resume** | Continues the paused set. |
| **End set** | Fades out, stops the set decks and clears tracks from stopped decks. |
| **Stop now** | Stops immediately. |
| Close the window | Playback continues. |

Touching deck or mixer controls takes over from Auto. Select Auto again when
you want Auto Mix to resume. In Prepare, approve the prepared plan when you are
ready for that mix.

### Report a mix that went wrong

Select the transition and choose **Report**. Describe what you heard, such as
poor timing, a clash or a level change, and add a note if useful. Reports retain
information about that transition for review.

Replay and improvement evaluation belong in a review session with live playback
paused or ended. A replay reconstructs the transition; a recording preserves
what actually happened. Review and audition evaluated changes before activating
them.

See [DJ workspace](dj-workspace.md) and [Auto Mix](ai-dj.md).

## 8. Record and listen back

### Record a source with Recorder

1. Open Recorder and select the source you want to capture.
2. Choose a format. Use 24-bit PCM for a conventional high-quality WAV take;
   Float32 is also available.
3. Start recording and check that its meter responds to the intended sound.
4. Pause and resume if needed. Paused audio is omitted from the same take.
5. Stop to save and finalise the recording.
6. Open the saved take in Player and listen back.

Source and format stay fixed during a take. Closing Recorder does not stop
capture. Check the take's clips, dropouts and any reported error after stopping.
Do not assume a file is complete just because it appears in the folder.

Takes are saved in **Patch Recordings** under your Music directory, normally
`~/Music/Patch Recordings`. In a compact Recorder window, use **Takes** to find
the saved files.

### Choose the right source

| Source | Use it for |
|---|---|
| **Program · Patch engine** | Patch's program mix before device/listening volume. |
| **Patch bus** | Sound routed through the Patch bus. |
| **What you hear · output** | The selected output device's monitor. |
| **An application** | One available application's stream monitor. |
| **Processed microphones · Patch Input** | The processed microphone capture mix. |

These sources can contain different sound. Make a short test take and listen
back before recording a full session.

### Record a DJ set

Use **Record program** in the DJ workspace to capture the program mix with
linked set and transition information. Set pause/resume coordinates that
recording; ending the set finalises it. Find linked takes and export tracklists
in **Recordings**. Pause live decks before playing back a take or rehearsal
from that workspace.

See [Recorder and Player](recording-playback.md) and
[DJ workspace recordings](dj-workspace.md#recordings).

## 9. Set up a microphone

Use headphones before enabling microphone monitoring through the main mix.
Sending a microphone to nearby speakers can cause feedback.

1. Open **Mic** and assign the physical microphone or interface input.
2. New inputs start muted and unmonitored. Unmute the intended input when ready.
3. Speak at your normal distance and watch the input meter.
4. Start with a profile such as **Natural**, **Podcast** or **Broadcast**.
5. Adjust software trim and processing while listening or checking a test take.
6. In a calling or streaming application, choose **Patch Input** as its microphone.

To bring the far end of a call or an X Space into Patch, press **+ Call far end** in
**Mic** while it plays. The voice moves from its own strip into a **Remote**
lane with the **Remote voice** profile. It stays out of Patch Input and every
send (mix-minus), so the people in the call never hear themselves.

### Use a browser voice as a mic

1. Start playing the voice in Chromium.
2. In **Mic**, click **+ Voice source**. The Chromium strip in the Mixer shows which **Voice** lane now carries its sound.
3. On that lane, turn the **Aux1** send up to 0 dB.
4. In the other browser, choose **Patch AUX 1** as the microphone.

If Patch warns about a loop, use a different browser for the session that records AUX 1.


Speaker monitoring is a separate choice, **Monitor in main mix**. Leave it off
unless you want to hear the microphone through Main. Turning down the speaker
volume does not turn down Patch Input's capture mix.

The voice chain includes a high-pass filter, noise suppression, de-essing, gate,
expander and compressor, followed by channel EQ. Start gently: too much gating
or suppression can cut off quiet speech. Profiles are starting points and do
not set an unidentified microphone's hardware gain.

Use **Calibrate** for a recorded comparison. Include room silence and normal
speech, stop the take, then analyse it. Advice does not change the sound until
you apply it. Save a named profile once the result suits your voice and room.

See [Mic](mic.md) for profiles, calibration and processing controls.

## 10. Show visuals and broadcast

### Visualiser

Open **Visualiser** from the controller, or use a layout that includes it.
It follows the playing tracks, musical sections and DJ transitions.

Press **F** for fullscreen. **L** opens Layers for composable visual layers,
including Decks & Signal and Performer. Use the window's help for scene, hold
and mode controls. Visualiser generates its picture live; it is separate from
recording or broadcasting audio.

See [Visualiser](visualiser.md) and
[performance layers](visualiser-performance-layers.md).

### Broadcast

Open Broadcast to send an audio source to an Icecast server, an RTMP endpoint
or a file.

1. Select the source and destination type.
2. Enter the destination details and credentials (for RTMP, the server and
   its stream key, which never shows).
3. Choose the encoding settings. RTMP uses AAC even if another codec is selected.
4. Test with a file destination or your intended receiving service.
5. Start and check for **ON AIR** (shown once the server has taken the
   stream), signal on the meter and successful reception.
6. Stop explicitly when finished. Closing the window leaves it broadcasting.

**RECONNECTING** means Patch is trying to restore a previously working network
stream. Audio during the outage is not sent. **FAILED** requires checking the
reported error, destination and credentials.

An RTMP broadcast sends a picture with the sound: the Performer, the
Visualiser, or sound only (**Picture**). Keep that window on screen while you
stream; the Broadcast window says when it isn't and the picture is black.
Keep stream keys private; the current encoder's destination credentials can be
visible in its process arguments.

See [Broadcast](broadcast.md).

### Network

Open Network (key **Z**) to send Patch's audio to other machines on your
local network, or to play theirs through Patch.

1. To send, pick the feed (Program, AUX 1 or AUX 2) and **RTP** or **VBAN**.
   Leave **To** empty to send to every machine on this network, or type one
   machine's address. VBAN always needs an address.
2. Choose **Send to everyone** (or **Send to …**). The send shows
   **SENDING** once Patch's feed is linked to it.
3. Streams other machines announce appear under **Receive**. Choose
   **Listen**: the stream plays in the Mixer like an app.
4. Choose a higher **Latency** if a received stream crackles on Wi-Fi.

If a yellow box says the firewall drops network audio, choose
**Let it in from …** and type your password in the terminal that opens. It
opens Patch's ports to your own network only.

Sends and receives keep running when the window closes and start again with
Patch's audio service. **WAITING** means Patch's engine is not running; the
link continues when it is back. Stop a link with **Stop**, remove it with
**✕**.

See [Network](network.md).

## 11. Save your workspace and settings

To change enabled modules or your default layout, open **Ctrl+K → Setup**.

To save your window arrangement, arrange the Units first, then run:

```sh
omarchy-patch layout save Night set
```

Reopen it with:

```sh
omarchy-patch layout night-set
```

Saving a built-in layout creates your own copy. **Reset** returns an arrangement
to its saved positions. Closing a layout closes its windows and leaves audio
running. A layout stores window placement; a saved DJ set stores your set's
choices. They are separate.

Typical locations, with the default XDG directories:

| Item | Location |
|---|---|
| Recordings | `~/Music/Patch Recordings` |
| Music catalogue | `~/.local/share/omarchy-patch/library.patchdj/` |
| Analysis cache | `~/.cache/omarchy-patch/analysis/` |
| Configuration and custom layouts | `~/.config/omarchy-patch/` |
| Session state and transition reports | `~/.local/state/omarchy-patch/` |

Keep your original music and recordings backed up. The analysis cache can be
rebuilt; it is not a backup of those files.

## 12. Solve common problems

### No sound

1. Confirm that Player or a deck is actually playing.
2. Look for signal on its Mixer channel.
3. Check the channel's mute and fader, any active solos, and Main mute/volume.
4. For decks, check channel levels and the crossfader position.
5. Check the physical output selected in the bar and any professional port
   assignments in Router.
6. If there is no live state, check the service from the controller or run
   `omarchy-patch engine status`.

### A microphone works in Patch but not in a call

Choose **Patch Input** in the calling application's input settings. Check that
the assigned microphone is unmuted and the capture mode is **Inputs**, rather
than Main mix. Main listening volume and microphone capture volume are independent.

### Two synced tracks sound out of time

Check the beat grids and downbeats, especially tracks marked **Check**. During
an overlap, check Sync on the incoming deck. Listen for half- or double-tempo
analysis. Sync aligns the analysed grids; it cannot establish that the detected
grid matches the audible drums.

### Auto Mix does not mix the next track

Check its mode first. Suggest waits for you; Prepare waits for approval. Manual
deck or mixer movement may have taken over from Auto. Check for available,
analysed tracks and read the displayed plan or readiness message. Enabling
Auto-mix alone does not necessarily start a stopped deck; use Play or Start set.

### Audio continues after closing Patch

Reopen the relevant Unit and stop its transport. For a DJ set, use Pause,
End set or Stop now as appropriate. Stop Recorder and Broadcast separately
when you finish those tasks.

### Audio crackles or drops out

Check the device connection and lower unnecessary background load. Inspect
Patch's engine status and health report:

```sh
omarchy-patch engine status
omarchy-patch health
```

The optional bar health dot is green when healthy, amber after a recent skip
or a busy cycle, red after a very recent skip, and an empty ring when the engine
is not running. Its tooltip identifies the condition. Enable it with
`omarchy-patch bar health on`.

Keep the report and note what was playing, the output device and what happened
just before the fault. See [Performance](performance.md) for deeper checks.

### A controller is detected but its controls do nothing

Open **MIDI & HID**, check whether a profile matched, and inspect its event
monitor. Detection does not guarantee support for the device's protocol or
mapping. The MIDI & HID Unit is different from the Patch controller opened by
Ctrl+K.

Follow [Controller setup](controllers-workflow.md) for mapping and device checks.

## 13. Find more help

| Topic | Guide |
|---|---|
| Installation and setup | [Getting started](getting-started.md) |
| Layouts and the bar | [Layouts](layouts.md) |
| Routing and output ports | [Core routing](core-routing.md) |
| DJ controls | [DJ Booth](dj-booth.md) |
| Saved sets and reports | [DJ workspace](dj-workspace.md) |
| Auto Mix modes and transition explanations | [Auto Mix](ai-dj.md) |
| Music sources and analysis | [Library](library.md) |
| Recording and playback | [Recorder and Player](recording-playback.md) |
| Microphones | [Mic](mic.md) |
| Controller setup | [Controller workflow](controllers-workflow.md) |
| Stem separation | [Stems](stems.md) |
| Visuals | [Visualiser](visualiser.md) |
| Broadcasting | [Broadcast](broadcast.md) |
| Live coding | [Tidal](tidal.md) |
| Sequencing | [Sequencer](sequencer.md) |

Run `omarchy-patch help` for the command-line reference. For window shortcuts,
use that window's help (**?**) or the keyboard reference below rather than
assuming a key does the same thing everywhere.

## 14. Keyboard reference

A window's own keys come first. A letter the window does not use opens that
Unit, so in the Mixer **S** solos rather than opening the Sequencer. **Tab**
moves to the next open Patch window, whichever Unit it belongs to.
Diagnostics, Club Booth, the Visualiser's Controls, the Performer and decks C
and D have no letter: open them from the controller or their Unit.

<!-- keys:begin (generated from crates/patch-core/src/keys.rs) -->
**Everywhere** (keys a window does not use itself)

| Key | Does |
|---|---|
| a–z | open or focus that Unit's window |
| ⇧ + letter | another window of that Unit |
| tab ⇧tab | next / previous open Patch window |
| ^k | controller (open · close) |
| ^= ^- ^0 | zoom in · out · 100 % |
| ? | help for this window |
| esc | close the help · the controller |

**Unit letters**

| Key | Opens |
|---|---|
| a | Deck A |
| b | Deck B |
| c | Broadcast |
| d | Device |
| e | Inspector |
| f | Tidal |
| g | Router |
| h | MIDI & HID |
| i | Mic |
| j | DJ Booth |
| k | EQ |
| l | Library |
| m | Mixer |
| n | Analyzer |
| o | Performance |
| p | Session |
| q | Plugins |
| r | Recorder |
| s | Sequencer |
| t | Pattern |
| u | Tune |
| v | Meter |
| w | Visualiser |
| x | Crossfader |
| y | Player |
| z | Network |

**Controller** (Ctrl+K)

| Key | Does |
|---|---|
| ↑↓ | move |
| enter space | open · close · change |
| a–z | open that Unit |
| 1–4 | Units · Layouts · Setup · About |
| tab ⇧tab | next / previous tab |
| ^q | close all windows (audio plays on) |
| esc ^k | close the controller |

**Router**

| Key | Does |
|---|---|
| drag ▸ | patch |
| ←↑↓→ | move |
| enter | inspect |
| r | through Patch |

**Mixer**

| Key | Does |
|---|---|
| ←→ | strip |
| ↑↓ | gain |
| space | mute |
| s | solo |
| o | output device |

**Device**

| Key | Does |
|---|---|
| ↑↓ | device |
| enter | expand |

**Inspector**

| Key | Does |
|---|---|
| ↑↓ | scroll |

**Meter**

| Key | Does |
|---|---|
| x | reset I · LRA · TP |
| o | output / engine |
| , . | sync ∓10 ms |

**Analyzer**

| Key | Does |
|---|---|
| f | bars fast / slow |
| x | reset hold |
| z | scope auto-gain |
| l | measure latency |

**Session**

| Key | Does |
|---|---|
| ↑↓ | preset |
| enter | apply |

**Deck A and Deck B**

| Key | Does |
|---|---|
| space | play |
| c | cue |
| 1–8 | hot cue (⇧ clears) |
| ←→ | jump |
| o | loop |
| ↑↓ | tempo |
| s | sync |
| k | keylock |
| [ ] | key |
| , . | nudge |
| x | sample |

**Crossfader**

| Key | Does |
|---|---|
| ←→ | crossfader |
| 0 | centre |
| f | fx |
| 4 | two · four decks |

**Performance**

| Key | Does |
|---|---|
| SUPER+ALT+P | open · close (your Omarchy binding) |

**Tidal**

| Key | Does |
|---|---|
| ^⏎ | run block |
| ^⇧⏎ | run all |
| ^. | hush |
| ^␣ | play · stop |
| ^B | backdrop |
| ^H | help |

**Visualiser**

| Key | Does |
|---|---|
| space | next scene |
| h | hold this one |
| a | Ambient on · off |
| x | switch this scene off |
| m | choreography · generative · hybrid |
| g | next generative scene |
| d | DJ · booth · off |
| l | controls window |
| f | fullscreen |

**EQ**

| Key | Does |
|---|---|
| 1–8 | band |
| ←→ ↑↓ | freq · gain |
| [ ] | Q |
| f g Q | set Hz · dB · Q |
| t | type |
| space | band on/off |
| b | bypass |
| - = | input gain |
| s | spectrum on · off |
| m | next: master · apps · DJ booth · mics |
| p P | preset |

**DJ Booth**

| Key | Does |
|---|---|
| space | pause/resume |
| n | mix now |
| 1–4 | observe · suggest · prepare · act |
| a s d | pick 1–3 |
| x | drop 1 |
| enter | approve |
| esc | cancel |
| v | overview · why |

**Mini Booth**

| Key | Does |
|---|---|
| space | pause · resume Auto Mix |
| ←→ | crossfader |
| 0 | centre |
| esc | close the picker |

**Library**

| Key | Does |
|---|---|
| / | filter |
| ↑↓ | move |
| space | pre-listen |
| 1–5 | start · drop 1 · break · drop 2 · outro |
| s | set prep |
| a | load A |
| b | load B |
| c d | load C · D |
| q | queue |
| ⇧c | add to collection |
| del | out of collection |
| ← → | previous · next part |
| ⇧← ⇧→ | 8 bars back · on |
| o | plays in order · DJ decides |
| ⇧↑ ⇧↓ | move (in order) |
| h | HUD: track, waveform, set prep fit |
| esc | hide HUD · stop pre-listening |

**Pattern**

| Key | Does |
|---|---|
| ctrl+enter | evaluate · next cycle |
| ctrl+shift+enter | now |
| ctrl+space | play · stop |

**Sequencer**

| Key | Does |
|---|---|
| space | play/pause |
| esc | stop |
| right-click | select step |

**Player**

| Key | Does |
|---|---|
| space | play/pause |
| 1 … = | play a cart (again: stop it) |
| [ ] | cart bank |
| f | fade the carts out |
| esc | stop the carts |
| e | edit the pads |
| right-click | edit a pad |
| o | open file |
| c | cue |
| p | PFL |
| ←→ | seek |
| , . | back 10 s · on 30 s |
| n b | next · back in the queue |
| q | carts, queue or marks |
| m | mark this place |
| ; ' | the mark before · after |
| l | repeat: this one, all, off |
| a | A-B loop: A, B, clear |
| < > | slower · faster |

**Recorder**

| Key | Does |
|---|---|
| ↑↓ | source |
| f | format |
| enter | record / stop |
| space | pause / resume |
| t | takes |

**Performer**

| Key | Does |
|---|---|
| f | fullscreen |

**Network**

| Key | Does |
|---|---|
| ↑↓ | announced stream |
| enter | listen / stop |

**Broadcast**

| Key | Does |
|---|---|
| ↑↓ | source |
| enter | go live / stop |
| esc | stop |
<!-- keys:end -->
