# DJ workspace

Open `omarchy-patch open dj` (controller key **j**) or the **Auto Mix** layout. Existing `light-dj` and `ai-dj` launch commands open the same workspace; Play shows the Auto Mix beside the live controls; `v` switches it between Overview and Why. For a second screen, use the Visualiser (the display-only AI Stage window was removed on 2026-11-22).

## Play a set

1. Open **Prepare**, name the set, choose collections and optionally add tracks. Collections form a union; an empty selection means the whole library. An explicitly ordered track list plays before automatic source picks.
2. Choose Suggest, Prepare or Auto. Suggest is the default. Prepare waits for approval; Auto executes transitions. Observe leaves deck operation to you.
3. Check readiness and **Save**, then **Start set**. You can prepare another draft while this set plays; draft edits never edit the current queue.
4. **Mix now / Skip** moves into the next track on a bar. **Change next** rejects that choice and chooses another. Queue arrows reorder pending tracks; Remove offers Undo. Source changes apply to future picks.
5. **Pause** freezes the set arc and both decks, including a running transition. **Resume** continues it. **End set** fades out and clears the stopped deck tracks; **Stop now** does so immediately. A separately playing manual deck keeps playing. Closing the window leaves playback running.

**Space** pauses/resumes, **N** mixes next, **1–4** choose Auto Mix mode. Text fields retain their normal typing behaviour. The master fader remains visible across tabs.

Expand **Auto Mix detail** for the existing candidate, operator, analysis and musical-field views. Approve, cancel, candidate selection/rejection, steering, cue, sync, loop and takeover remain accessible. Previous/next transition arrows browse history without seeking audio. Reporting captures the chosen transition identity when the report opens.

## Prepare future sets

Save named sets with notes, collection sources, ordered/pinned tracks, duration, energy arc, tempo corridor and Auto Mix mode. Save as/Copy create independent drafts; Trash is recoverable. Concurrent changes are rejected rather than overwritten. Stable library identities resolve relocated tracks when possible; missing files remain visible in readiness checks. Old JSON sets remain listed; incompatible formats produce a migration message.

Pins are saved editing constraints; the stored order is used at start. Automatic whole-set generation is a later extension.

## Recordings

**Record program** captures Patch's program mix before system/device volume. The recorder writes audio and a `.dj.json` sidecar, with engine sample positions mapped to recorded samples. Pausing the recorder omits paused audio and retains the mapping. Set pause/resume coordinates a program recording; ending the set finalizes it.

Performed sets list their tracks, transitions and linked takes, including incomplete takes. Choose a take to inspect/export its metadata and TXT/CUE tracklist. Chapters use completion of the crossfader handover; two tracks can be audible during the overlap. Existing recordings remain available through Recorder. Take and rehearsal playback use Player and require paused live decks.

A clock discontinuity, sample-rate change or capture overflow retains partial audio and records an error; it does not invent continuous timestamps.

## Reports and improvement

Select played transitions and report the problem, with optional notes. Reports retain the saved plan, source references, live outgoing tempo/key/grid, tuning and version. The Reports tab filters reports, selects batches and exposes replay diagnostics.

Pause or end playback before **Replay** or **Evaluate improvement**. Jobs run outside the audio callback, persist progress/results, support cancellation/retry, and label interrupted jobs after an engine restart. Missing sources or invalid old plans are shown as unreplayable. Replays reconstruct the transition; they are not recordings of the original performance.

Improvement tests three bounded tuning changes against a baseline. At least six distinct replayable unordered pairs are needed. Training and evaluation pairs are separated, known-good pairs are added, and each candidate must pass regression checks in both physical deck directions. A failed candidate cannot be activated. Review the scores, parameter changes and baseline/candidate auditions first.

**Activate evaluated candidate** writes a version with report provenance atomically. Existing plans retain their tuning; newly composed plans use the activated tuning. **Restore previous tuning** rolls back. This adjusts deterministic transition policy; it does not train a new model. Existing report-based pair feedback continues separately.

## Validation boundaries

Automated tests cover persistence/revision conflicts, legacy defaults, draft isolation, transition resume commands, mirrored rehearsal evaluation, sample-stamped capture and recorder interruption. Compact/full views are rendered headlessly. Live PipeWire listening, hardware xrun measurements and keyboard-only usability still require an operator session; metric scores alone do not prove sound quality.
