# Recorder and Player

Open the units with `omarchy-patch open recorder player`, or from the controller. Player belongs to the Audio group and remains available
when DJ features are disabled.

## Recording

Choose a source and format, then press Enter or Record. Space pauses or resumes
capture; Enter stops and saves. Source and format stay fixed for the current take.
Pausing omits audio from the take and keeps the same file. Recording continues
when windows close or the UI restarts.

24-bit PCM uses TPDF dither. Float32 preserves captured sample values. Both use
Broadcast WAV, switching to RF64 for large files. The writer updates and syncs
its header every second and drains captured frames before finalizing. Existing
files are protected from overwrite. Disk, capture, sync and finalization failures
are retained in `recorder.result.json` in Patch's private runtime directory.

The meter shows the current captured level and the take's peak hold. The view
also reports clips, dropouts, integrated loudness, file size and free space. The
last take's readouts remain available after stopping. Takes are saved under the
XDG Music directory in **Patch Recordings**. In compact windows, the Takes tab
provides the saved-file list. Play opens a take in Player; the active recording
is protected from playback until it is stopped.

```sh
omarchy-patch record start bus s24
omarchy-patch record pause
omarchy-patch record resume
omarchy-patch record stop
omarchy-patch record status
```

The recorder uses a reusable pool of up to 128 capture buffers (8 MiB). A full
pool stops capture and reports an error, protecting memory from an indefinitely
slow disk. Status, directory scans, startup, pause and stop run on background
workers in the UI. Directory scans run at most once every two seconds, separately
from status and transport. Free space comes from the filesystem directly.

## Playback v1

Use Open file, paste a file path or HTTP(S) stream URL, or play a saved take.
Player has play/pause, stop, seek, volume, repeat, time and audio-format readouts.
Space toggles playback, O opens the file chooser, arrows seek, **,** goes back
10 seconds and **.** on 30 (the buttons beside Stop), and L toggles repeat.
The time shows what is left of a timed file. Seeking and repeat are available once a timed file is ready. Stop pauses
and rewinds a timed file; it closes a stream without a known duration.

Player uses a detached mpv process with PipeWire output and a dedicated
`player.sock`. It retains playback across UI/window closure. Its **Patch Player**
audio stream can be routed with Mixer or Router. DJ decks retain their own
transport and sockets. Volume is limited to 100%.

```sh
omarchy-patch player load "/path/to/audio.wav"
omarchy-patch player pause
omarchy-patch player seek 50%
omarchy-patch player volume 75
omarchy-patch player loop on
omarchy-patch player play
omarchy-patch player status
omarchy-patch player quit
```

`load` asks mpv to open the source asynchronously. Check `status` for `ready`
before issuing a seek from scripts. `quit` stops and releases the player's
process, and the carts with it. mpv with PipeWire support must be installed.
There is one playback transport with its queue, and the carts beside it;
waveform editing and multitrack recording are future work.

## The waveform, the queue and going on where you left off

* **The waveform** shows the whole file: where the talk, the music and the
  silences are. What has played is in the accent colour. Click or drag on it
  to go there. Ticks along its top are the file's chapters and the cue point;
  an A-B loop is shaded. A file is never decoded into memory for this: it is
  read as a low-rate stream at the lowest priority, the picture fills in from
  the left while it reads, and the result is cached (an hour of audio reads
  in about 2 seconds for MP3 or 15 seconds for FLAC here, then opens from
  cache). A stream has no waveform and keeps the plain seek line.
* **The queue** plays sources in order. **Queue** (or **Q**) shows it in
  place of the pads; from 1000 px wide it sits beside them. **Add…** lines
  files up; click a line to play it, **↑** moves it up, **×** takes it out;
  **N** and **B** go to the next and back; **Shuffle** and **Clear** are
  there too. **Open file** with a queue standing plays that file now and
  keeps the queue after it. The player process holds the queue, so every
  window and the command line see the same one, and it is saved: after a
  restart the queue rests in the list and **Play** goes on with it. Local files
  show their lengths before playing; the queue shows its total when every
  length is known.
* **Played lately** lists what you opened before, under the queue. Click one
  to play it again.
* **A long file goes on where you left it** (ten minutes or longer: a
  podcast, an audiobook, a recorded set). Its place is kept when you pause,
  stop the player or load something else, and every twenty seconds while it
  plays. **Stop** means "from the start next time". Shorter files always
  start at the beginning.
* **Speed** from 0.5x to 2x with the pitch kept: the speed button steps
  through 1x, 1.25x, 1.5x, 1.75x, 2x and 0.75x; **<** and **>** step a
  quarter.
* **A-B** (A) loops a part: press at the start, press at the end, press
  again to clear. For transcribing a quote or rehearsing a passage.
* **Repeat** (L) steps through this one, the whole queue, off.

```sh
omarchy-patch player add ~/Podcasts/*.mp3   # onto the queue
omarchy-patch player next · prev · queue · queue 3 · queue shuffle · queue clear
omarchy-patch player speed 1.5 · repeat all · ab
```

## Marks and show notes

**M** marks the current place and opens **Marks**, where you can name it.
The list combines your marks with the file's own chapters. Click a timestamp
to jump there; **;** and **'** go backward and forward through the places.
**Copy** puts the list on the clipboard as timestamps for show notes.
Your marks are kept per file, separately from the audio. **Q** cycles through
Carts, Queue and Marks.

```sh
omarchy-patch player mark "Sponsor read"
omarchy-patch player marks
omarchy-patch player goto 120
```

## Carts: sounds on pads

Under the transport the Player has a cart wall for a podcast or a broadcast:
pads that start a sound at once, over whatever else plays. An intro, a
commercial, B-roll, a sting, a music bed.

* **Four banks of twelve pads** (Intros, Commercials, B-roll, Effects; rename
  them as you like). The number row, **1** to **=**, plays the pads of the
  bank shown; **[** and **]** change bank. A dot on a bank's tab says one of
  its sounds plays.
* **Press a pad to play it, press it again to stop it.** A playing pad is lit,
  counts down what is left and draws its progress along its foot. Sounds
  overlap (a sting over a bed) unless **Solo** is on: then starting a pad
  takes over from the others.
* **Fade** (F) fades every cart out over two seconds; **Stop** (Esc) cuts them.
* **Click a free pad** to give it a sound. Choose several files and they fill
  the free pads after it. **Right-click** a pad, or **Edit** (E), to change
  one: its name, its sound, **Loop** (a bed that plays round until stopped),
  **Fade out** (stopping it fades instead of cutting; a second press cuts)
  and its level, heard at once when it plays.
* **Hold** plays while the pad or its key is held. Releasing stops it (or
  fades it if Fade out is on). **Duck** lowers the main Player transport
  by 12 dB while that pad plays, then restores it gently after the last
  ducking pad ends. It keeps your volume setting and other pads unchanged.
  Click the colour swatch in Edit to cycle through five pad colours or none.
* The carts play on the Player's stream: one **Patch Player** strip in the
  Mixer for level, EQ and PFL. Sounds keep playing when the window closes;
  the bar then shows a **Carts** row with Stop.

The board is `~/.config/omarchy-patch/carts.json`; every Player window and
the command line share it. For a desktop key binding, a controller script or
a Stream Deck:

```sh
omarchy-patch player cart 3              # pad 3 of the bank shown: play, or stop it
omarchy-patch player cart commercials 1  # a pad of another bank (its number or name)
omarchy-patch player cart fade           # fade everything out · cart fade 3: one pad
omarchy-patch player cart stop           # cut everything      · cart stop 3: one pad
omarchy-patch player cart set 4 ~/ads/spot.wav "Sponsor"   # give pad 4 a sound
omarchy-patch player cart clear 4 · cart bank 2 · cart status
omarchy-patch player skip -10            # the transport: back 10 s (, and . in the window)
```

Each sound plays on its own detached mpv, which exits when the sound ends.
While a Player window is open one spare waits idle, so a pad starts in the
time mpv needs to open a file: 35 ms measured here, against 83 ms for a cold
start (the first press with every window closed). Eight sounds can play at
once.

**Cue** saves the current position and pauses. After moving elsewhere, Cue
returns to that position and pauses; pressing it at the saved position clears
the cue and rewinds. Cue positions are seconds, including positions beyond
100 seconds. The detached player holds the cue, so reopening the UI or using
another CLI process sees it too. Loading a file or stream clears the old cue.

**PFL** sends the Player's pre-fader signal to the same Headphone Cue bus as
the Mixer. Choose a Headphone Cue output first. Patch assigns the Player an
application channel if it needs one; if no channel is available, it reports an
error. PFL does not change playback volume or remove the Player from Main;
use the Player's Mixer strip to control the program mix separately. Turning PFL
off leaves the channel available for the Player's EQ and other strip controls.
The UI reads PFL state from the engine, including changes made in the Mixer.
`player cue`, `player pfl on`, and `player pfl off` also work from the CLI.

The Player worker starts on first use, sleeps when idle, and prioritizes user
commands over telemetry. Repeated polls and pending seek/volume/repeat edits
coalesce. Each status snapshot uses one IPC connection and a shared 300 ms response budget. File picking runs
separately, so leaving a chooser open keeps transport controls available.

## Validation

```sh
cargo test --offline --release --workspace
cargo build --offline --release -p patch
python3 integration/test-media.py target/release/omarchy-patch
cargo clippy --offline --workspace --all-targets -- -D warnings
PATCH_PROVIDER=mock PATCH_SHOT_SIZES=560x400,560x460 \
  target/release/omarchy-patch shots /tmp/patch-media-shots player
```

The isolated integration test uses a fake capture pipe, a private runtime
directory, and mpv's null output. It checks captured float samples byte for byte,
queued-tail preservation, final WAV sizes, overwrite protection, exclusive
capture, pause/resume, control timeouts, disk errors, playback controls, end-of-file
restart, and HTTP audio from a local server. It leaves live audio untouched.

On this machine, standalone status commands measured 2.9 ms median and
3.8 ms maximum in the final 20-call run. This is IPC/CLI latency, rather than an audio
latency or whole-application benchmark. The workspace run passed 363 tests, with 12 opt-in tests ignored. Compact and full-size layouts were rendered
for visual review. Strict Clippy passes across the workspace, including the concurrent Sequencer
work. Its swing calculation was left unchanged.
