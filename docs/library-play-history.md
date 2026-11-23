# Library play history and changing sets

The library labels tracks as **Unplayed**, **Last hour**, or an age in hours
or days. **This session** is added when the last play falls within the active
Auto Mix set, including sets longer than an hour. The history buttons filter the
current search and collection; clicking an active history filter clears it.
Select a row with the keyboard or mouse, or hover it, to see its last-played
local date, time, time zone and play count.

The engine records playback in the library after ten seconds of playback
with the deck reaching the main mix: channel mute/solo, crossfader/fader
and DJ group mute/level all apply. Output-device volume is excluded.
Pausing and resuming a loaded track does not create another play. Loading
a track again, including the same file, resets the observer. This runs on
a dedicated background thread, not
the audio callback or transition planner. Existing stored plays are retained;
previous playback that was never recorded cannot be reconstructed here.
Open library windows notice recorded plays within a few seconds; ages and
session labels refresh at least once a minute.

To replace a running AI set, select the new set and its collections in the
DJ workspace, click **Switch to this set**, then **Confirm switch**. The
Library's **Switch AI collection…** also asks for a second click and uses the
highlighted track as the next queued track. The current deck keeps playing.
An unstarted transition is discarded and the normal AI selection, rehearsal
and mixing system prepares the replacement. A transition already audible
finishes first; its incoming track is already playing. The new set keeps its
selected assistance mode: Act runs transitions, Prepare waits for approval.
