# Stems in DJ Booth

Patch separates audio as an optional preparation task. A track always remains
playable from its original mix; stems do not create extra decks or mixer
channels. The first layout provides Vocals, Drums, Bass and Other.

## Prepare a track

Load a track in a Booth deck and open **Stems**. When separation is available,
choose **Prepare stems** and wait for **Ready**. This runs in the background;
it does not stop normal playback. **Enable stems** switches that deck to the
prepared set. Each stem has a gain slider, **Mute**, **Solo**, and a meter.
Double-click a gain slider to return it to 100%. **Return to Auto** hands
controls back to AI automation; **Reset levels** restores unity gains and clears
mute/solo. Pin a prepared set to keep it out of automatic cache cleanup.

**Prepare stems** in the Booth's set options prepares the selected collections.
Pressing it again while that batch is active shows the existing progress. Patch
continues queued and interrupted preparation after a restart or upgrade and
reuses stems it already saved. The Booth shows the resumed count and any
failure. **Cancel prep** in the Stems panel also cancels the work waiting in
that batch.

## Send a clean stem to the other deck

Each stem has a **Send clean → A/B** button. For example, send Deck A's Bass to
Deck B, then mute Bass on Deck B. The incoming send remains audible because the
destination deck's stem controls affect only its own track. The send follows
Deck B's channel fader, crossfader and channel mute.

The source deck keeps its ordinary mix. Its stem gain/mute/solo controls also
shape the sent stem; the destination deck's stem mute/solo does not. Only one
stem send is active at once. Sending another stem replaces the current send.
Use **Stop send** to remove it.

Sends are clean by default. While one is active, **Add FX** applies the source
deck's configured Beat FX chain to the isolated send; it does not enable effects
on the source deck's main signal. The source deck shows an independent **Depth**
slider while FX is enabled; double-click resets it to 50%. Turn **FX on** off
again for a clean send. The optional effect follows the source deck's beat grid.
If an effect has a tail when a send stops, the tail may ring out.

The send is an additional path alongside the normal source-deck output. To
perform a handoff, adjust the two decks' stem controls and faders as needed;
the send does not automatically mute either track. Auto Mix uses the same stem
gain/mute/solo and timed-ramp contracts for its transitions; this manual send
is a Booth routing control.

## Storage

Patch prepares stems only when requested by a loaded deck, an AI transition
that needs them, or an explicit selected-track/set preparation action. Ordinary
library analysis does not create stems. Recently used cache entries may be
evicted when space is needed. **Pin cache** retains a prepared set; pinned and
currently used stems are protected from automatic cleanup.
