# Broadcast

The Broadcast Unit (**c** in the controller, `omarchy-patch open broadcast`)
sends Patch's audio somewhere else: to an **Icecast/HTTP** server, to an
**RTMP** endpoint (YouTube, Twitch, a local nginx-rtmp) with Patch's picture,
or to a **file** on this machine.

Capture and encoding run in a **detached process**, so a stream keeps running
when the window closes, when Patch's UI restarts, and across a Patch update.
Stopping it is always explicit: from the window, the controller, or the CLI.

## What goes out

The same source list the Recorder offers:

* **Program · Patch engine** — the engine's own output, before the device.
* **Patch bus** — everything routed through the Patch bus.
* **What you hear · \<output\>** — the monitor of the current output device.
* **An application** — one stream's monitor.

`‹ ›` (or `↑ ↓`) moves the selection, and it is locked while a broadcast runs.

## Where it goes

| Destination | What to enter | Notes |
|---|---|---|
| **Icecast** | Server (`http://host:8000`), Mount, User, Password | Speaks Icecast/HTTP (`icecast://…` under the hood). `https://` connects over TLS. A password with no user is sent as Icecast's `source`; any character works in either. |
| **RTMP** | Server (`rtmp://host/app`) and Stream key | **H.264 video and AAC sound in an FLV stream** (Opus/MP3 selected here are sent as AAC); the picture is below. A whole ingest URL with the key in it works too, and `rtmps://`. |
| **File** | A path (`~/Music/Patch Broadcast/set.ogg`) | A local recording of the same encode; useful to test a setup without a server. An existing file is never overwritten: the next free name is used (`set-2.ogg`), and the window shows it. |

Codec (Opus, MP3, AAC), bitrate (96–256 kbps) and sample rate (44.1/48 kHz)
are chosen below. The destination is shown back with any credentials masked
(an RTMP stream key shows as `••••`, in the window, the bar and the status),
and the password and stream key are entered in fields that never show them;
the encoder's error messages are shown with them taken out. Patch hands the
broadcast process its settings in the environment, not on its command line;
`ffmpeg` still receives the Icecast password (and an RTMP stream key) inside
its destination URL, so they are visible in the process list while a
broadcast runs.

A limiter keeps the coded audio just under full scale (peaks at −0.26 dBFS)
so the encoder never clips; it never raises the level.

## The picture (RTMP)

YouTube and Twitch need a picture with the sound. An RTMP broadcast carries
the **Performer** (the Broadcast DJ display, the audience picture) or the
**Visualiser**, chosen under **Picture**, or **Sound only** for a server that
takes that.

* It is the window as drawn — its generated stage with the controls on top,
  what the screen shows — scaled on the GPU to **1280×720**, letterboxed when
  the window has another shape, and sent at **30 fps**.
* The broadcast process paces it onto the sound: 30 pictures for every second
  of audio, so picture and sound stay in step for the whole set, whatever rate
  the window draws at.
* Coded as **H.264** (4 Mb/s, a keyframe every 2 s, BT.709 colours) on the
  GPU with NVENC when this machine has it, else with x264.
* The window must be **on screen**: a closed, minimised or hidden window draws
  nothing, and then the picture is black (after 1.5 s; a short hitch keeps the
  last picture). The window says so; **Show Performer** opens or raises it.
  Put it on a second screen or a projector, or keep it in view.
* Taking the picture costs the window's UI thread about a copy of one frame
  30 times a second; nothing at all is taken while no broadcast wants it.

## When the connection drops

A network stream that reached its server comes back by itself: the capture
keeps running, the encoder reconnects after 1, 2, 4, 8, then every 15
seconds (counted again from 1 s for each outage), and the window shows
**RECONNECTING** with the reason and the attempt. A server that stops
answering for 10 s counts as lost. Audio during the gap is not sent (and not
counted as sent); each outage counts once as a dropout. It keeps trying
until you stop it. A broadcast that never reached its server (5 s for an
unreachable host), a server that refuses the credentials or the mount (401,
403), and a file destination end with **FAILED** and the encoder's own
message instead. The window keeps showing FAILED and why until the next
start, and `omarchy-patch broadcast status` says it too.

## The window

* **OFFLINE / CONNECTING / ON AIR / RECONNECTING / FAILED**, with uptime and how much audio
  has been sent. **ON AIR** only once the destination has taken the stream
  (the server accepted it, or the file is being written); until then it
  says CONNECTING.
* A **level meter** of what is going out, its peak, a **dropout** count
  (each time the source delivered well under what the clock expected, and
  each outage), and how much the server or the file has taken.
* **Enter** goes live; **Esc** stops. While a broadcast runs the form is
  locked — change a destination by stopping first. "Sends to" shows where
  the running broadcast goes.
* Stopping waits for the encoder to finish (a file gets its last seconds and
  its end); one that cannot finish within 5 s (a stalled server) is ended.

## The CLI

```sh
omarchy-patch broadcast start icecast http://host:8000 --mount patch \
    --user source --password … --codec opus --bitrate 128
omarchy-patch broadcast start rtmp rtmp://live.example/app --key STREAMKEY --name "Patch"
omarchy-patch broadcast start file ~/Music/Patch\ Broadcast/set.ogg
omarchy-patch broadcast status        # one JSON line: state, uptime, level, …
omarchy-patch broadcast stop          # finalize and print the outcome
```

`start` returns once the destination has taken the stream ("broadcasting
to …"), fails with the server's reason, or says it is still connecting
after 12 s; for RTMP it also says when no window is sending the picture.
`--picture performer|visualiser|none` chooses the picture (Performer by
default). `status` prints the last broadcast's failure when none runs.
`--source device:<node>` / `--source stream:<index>` chooses what goes out
(default: the engine's program node). `--rate`, `--bitrate`, `--mount`,
`--user`, `--password`, `--key`, `--codec` and `--name` are the rest of the
form. A password given on the command line is in your shell's history; the
window's field is not.

## Requirements

`parec` (PipeWire tools) captures; `ffmpeg` encodes and pushes. Without them
the window says so and refuses to start. The stream is controlled over
`$XDG_RUNTIME_DIR/omarchy-patch/broadcast.sock`; the last outcome is written
beside it, so a stopped broadcast still reports what it sent.

## Broadcast DJ display

[Broadcast DJ · PATCH / SIGNAL](broadcast-dj.md) is the separate audience
display (`open performer`): a scalable portrait of both decks and their
actual mix controls. Use **f** for fullscreen. An RTMP broadcast sends its
picture with the sound (above); OBS can still capture it for anything else.
