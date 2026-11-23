# Library

The library holds your music: the folders and drives you add (**sources**),
and for every track what Patch knows about it. A background librarian finds
new tracks and analyses them, with or without a Patch window open, at idle
priority: it never takes CPU or disk time from playing audio.

## Sources

`~/Music` is always a source. Add a folder or a USB stick:

* **In the Library window:** plug the stick in; the Sources panel says
  *Drive NAME is plugged in* with an **Add to library** button.
* **In a terminal:** `omarchy-patch library add /path/to/folder [name]`.

Each source is also a collection of its own name: click a source in the
Sources panel to browse just its tracks. A USB stick you unplug stays in
the library, marked *not connected · N tracks offline*. Plug it back in
and its tracks are there again, analyses included; anything new on it is
found at once. `omarchy-patch library remove NAME` takes a source out: its
files are never touched, and its analyses are kept should you add it
again.

### A drive that comes back under another name

Linux mounts a drive under its label (`/run/media/you/SAMSUNG_T5`), or with
a digit added when that folder is still taken (`SAMSUNG_T52`). Patch
follows: when a drive source's folder is gone and the same drive is mounted
one digit apart, the librarian checks that a sample of its tracks is there
(same files, same sizes) and moves the source and its tracks to the new
place. Nothing is analysed again and collections keep their tracks.

## Where a track stands

| Word | Means |
|---|---|
| **ready** | Tempo with a steady beat grid, the downbeat, key and loudness: everything the decks, sync and the Auto Mix need. A grid you set by hand (Club Booth · Beat grid) makes a track ready too. |
| **soft** | The tempo is confirmed by the file's own BPM (its tag, or a name ending "- 98"), but the beat is too soft for a steady grid: ballads, live drums, no kick. Nothing to check: the Auto Mix mixes it short (cuts, echoes, short blends). |
| **check** | Analysed, with a real doubt: the beat is unsteady (the row says how sure, e.g. "32 % sure") and nothing confirms the tempo, or no key or downbeat was found. It plays; check the grid, or set it by hand. |
| **waiting** | Not analysed yet (it's in the queue). |
| **failed** | The file couldn't be read. The row gives the reason; it isn't retried until a newer Patch analyses differently. |

Radio and streams have no word: they can't be analysed.

A deeper beat scan does not help the soft and check tracks: measured on 49
check tracks of a 1960s–2000s chart collection (2026-11-14), a precise grid
over the whole track at every likely tempo found a steady beat in one. Most
have no steady kick to find; for 25 of the 45 with a BPM in their tag or
name, the analysed tempo already agreed with it. Patch never writes to the
files: tags and grids you edit live in the library.

In the Library window the sidebar is a filter well, then the catalogue.
The list is the tracks. The footer says what you are looking at.

* **New collection**, above the sidebar, makes a collection: from a folder
  already in the library, from any other folder, or empty. **Add to
  collection**, on the highlighted track, puts that one track in or takes
  it out. **Make collection** is the separate button for the folder you
  are browsing;
* **Where** is each folder and drive, as a tree. The arrow opens a
  place; click that arrow again, or the open place itself, to collapse
  it. Folders next to the one you are in stay listed. **Make collection**
  turns the folder you are in into a collection. **Remove from library**
  takes a source out (the files stay on disk). The number at the right
  is how many tracks the place holds. A second line says *offline*, or
  *N ready* when some of its tracks are not ready yet;
* **Show**, the well above the folders, is what to include. The chips
  combine, and they are not places. **Fits the deck** is the tracks whose
  key is compatible and whose tempo is within 8 % of the deck that is
  playing (half and double time count). **Favourites** is the heart marks.
  **Straight grid** is the grids measured straight. **Played**, in the
  same well, is **Unplayed**, **This session**, **Last hour** or
  **Earlier**. Earlier is played, and is neither this session nor the
  last hour. The heart at the top of the list is different: it sorts
  favourites first, and does not hide the other tracks;
* **All · Ready · Soft beat · Check · Waiting · Failed** sit on the search row, with
  counts for this place. Check, Waiting and Failed hide when there are
  none. **Clear** turns every filter off;
* the line under a collection’s name says how many tracks it holds and,
  when they share one folder, that folder (`2 tracks · Music · House`);
* the line above the list is the highlighted track’s file, name first,
  then the directory (`harbour.flac  ·  ~/Music/House/2024`), and when
  that track was last played. Each row says the title, then the artist
  and the short place (`Music · House/2024`), or why it needs a check.
  Search matches a folder as well as a title or artist. **A B C D** load
  the highlighted track onto a deck;
* the footer’s left side says what this view is showing (*Showing 3 of
  8 · ready · fits*) once a filter is on. The middle is what the
  librarian does now: *analysing “Title” · 312 waiting · about 25 min
  left*, or *everything that can be analysed is analysed*. The right
  side is the whole library: *8 tracks · 7 ready · 1 to check*.

## Release years

The **YEAR** column shows when the song first came out. When this copy is
a later release (a Beatport top 100 re-release, a remix, a remaster), its
own year is under it, smaller. Search finds a year too (`1987`).

The year of this release comes from the file's own tags, offline. The
original year needs a lookup: the first time the Library opens it asks
whether Patch may look release years up at **MusicBrainz**
(musicbrainz.org, an open music encyclopedia). Allowed, the librarian asks
about each track in the background, one request a second (MusicBrainz's
rule): it sends the track's artist and title and nothing else, and keeps
the answer, so each track is asked about once. Nothing is mirrored or
downloaded beyond those answers. **Settings · DJ Booth · Look up release
years online** turns it on or off; off, only the tags' years are shown.
The footer says *looking up release years: N left* while it works.

## Genres

The GENRE column (wide windows) and the search use each track's genre.
It comes from the file's own genre tag when that names one ("Tech House",
"Deep House"). A tag that says nothing ("Top 40", "Other", "Various", a
web address) doesn't count. With lookups allowed (the same question as
the release years), the rest come from MusicBrainz: the genres people
tagged the song with, else its artist's, and only names on MusicBrainz's
own genre list. A new track's genre comes with its release-year search,
so it costs no extra request. Tracks whose years were looked up before
get one more search each, and each artist is asked about once. Discogs'
finer styles are for Auto Mix Agent unit later, as they need a token.

## Pre-listening and set prep

Highlight a track and press **space**: it plays from where you last
listened (the start at first). The keys jump to the track's major
moments, worked out from its analysis: **1** the start, **2** drop 1,
**3** the main break, **4** drop 2, **5** the outro. **← →** step through
the parts, **⇧← ⇧→** go 8 bars back or on, **esc** stops. Move to
another track while it plays and that one starts at the same moment: at
drop 1, the next track's drop 1.

A slim strip above the list shows the track, the moment it is in, and
where the five keys land. **Space** plays and pauses; the strip stays
while you do. The pre-listen ends, and the strip with it, on **esc**, when
the track has played to its end, after a minute paused, and when the last
Library window closes.

**h** opens the **HUD**, a panel over the list (**h** or **esc** hides it).
It only shows; the keys do the work. It shows the pre-listen, or the
highlighted track when nothing is pre-listened to:
- the track: artist, genre, year (and this release's, when later), BPM,
  key, loudness and length; playing or paused, where it plays (AUX 1,
  HEADPHONES) and the time;
- a large waveform: the played part in the accent, the playhead, a line
  where each cue key lands (the one you used last named and lit), and
  the track's parts (intro, build, drop, break, outro) under it;
- **how it sits in the set prep** that **s** adds to. A track already in
  it is shown beside the tracks before and after it. Any other track is
  shown as the next one after the last, plus the set track it mixes best
  with. Each pair shows its key move on the Camelot wheel (✓ fits, ✗
  clashes), the tempo change (half and double time count), whether energy
  goes up or down, and a verdict: *mixes well* (key and tempo fit),
  *workable* (one of them), *hard mix*.

In short windows the HUD leaves out the set prep part.

The preview is its own stream (*Patch Preview* in the Mixer), so the
Player keeps what it plays. While a deck or the Player is playing, the
preview goes to **AUX 1**, kept off main: give AUX 1 an output in the
Mixer (your headphones) to hear it. If AUX 1 has no output and a
Headphone Cue output is chosen, it plays in the headphones instead. When
nothing else plays, it plays in the headphones when a Headphone Cue
output is chosen, else on main. The strip says where (AUX 1,
HEADPHONES). The first time each run that it moves to AUX 1 the Library
says so; **Don't tell me again** stops that (Settings · DJ Booth turns it
back on).

A **set prep** is a second kind of collection: the set you are putting
together while you listen, in the order you add the tracks. **s** puts
the highlighted track in it, or takes it out (the first **s** makes one,
named by the day). **Set prep** in the sidebar lists them, with **New set
prep**; the one marked *s adds here* is where **s** puts tracks (the one
you viewed last). In a set prep, **⇧↑ ⇧↓** move the highlighted track up
or down. A set prep is a collection too: the Auto Mix can play from it.

### Play in order, or let the DJ decide

Each collection, a set prep too, has the Auto Mix decide the order by
default: it picks the next track that mixes best. **Plays: In order**
beside a collection's title (or **o**) makes the Auto Mix play it in its
listed order instead. The list then shows that order, and **⇧↑ ⇧↓** move
the highlighted track. The Auto Mix still plans each transition, and a track
you queue (**q**) still comes first. A set starts on the first listed
track (or the highlighted one with **Start Auto Mix**) and goes on from
whichever track plays. Tracks not analysed yet are skipped. After the
last one the DJ decides again. **DJ decides** (or **o** again) goes back.

In a terminal:

```sh
omarchy-patch library           # the overview: sources, counts, what it does now
omarchy-patch library failed    # every failed track and why
omarchy-patch library rescan    # look for new music in every source now
```

## Moving, renaming and copying files

Patch knows a track by its **content** (its size and a fingerprint of its
start, middle and end), not by its name or folder. So:

* **Moved or renamed**, inside a source or to another source: the next scan
  finds it and it is the same track. Its analysis, waveform, cues, loops,
  history and collections come along; nothing is analysed again. A scan
  runs at once when a drive is plugged in, every 15 minutes otherwise, or
  with `omarchy-patch library rescan`.
* **Copied** (the same file in two places, say on the stick and in
  `~/Music`): one track in the library, where it was first found. The
  other copy takes over only when the first is gone (deleted, or its stick
  unplugged), analysis and all.
* **Changed** (re-encoded, or new tags written by a tag editor): new
  content, so a new track, analysed again. Cues and history stay with the
  old one.

Where it is kept:

| What | Where |
|---|---|
| The library: tracks, analyses, waveforms, cues, loops, beat grids, history, collections, sources | `~/.local/share/omarchy-patch/library.patchdj/library.db` |
| The analysis cache (by content) and each track's detailed waveform (`.wfd`) | `~/.cache/omarchy-patch/analysis/` (bounded to 8 GiB and one year; entries can be rebuilt) |

Nothing is written to your music files or to the drive they are on.

## How long it takes

About a second and a half per track (measured on this machine: 1.4–1.6 s
for a 320 kbps MP3 on the internal disk), one track at a time, whenever
the machine has idle time; a USB stick adds its read time. A 13 GB stick
with 2,400 tracks takes about an hour and a quarter. It picks up where it
left off after a reboot, and each track is analysed once. The Library window shows the time left.
