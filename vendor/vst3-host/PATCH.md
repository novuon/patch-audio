# vst3-host 0.10.0, patched for Patch

The crates.io release (MIT, Helge Sverre), with one addition:

* `RealtimePluginRunner::editor()` / `EditorAccess` (`src/realtime.rs`): the
  plugin's own editor, served on the control thread while the runner
  processes the same instance on the audio thread. Upstream, the runner owns
  the whole `Plugin`, so an editor could only be opened on a second instance,
  whose meters never move (no audio reaches it). The handle reaches only the
  controller domain (view, frame, run loop, scale, edit feedback queues),
  which `process` never touches.

Everything else is the published source. To update: copy the new release
over this folder and re-apply the `EditorAccess` block (and its `pub use` in
`src/lib.rs`).
