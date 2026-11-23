# Patch EQ preset format (version 1 proposal)

Preset files are UTF-8 JSON so users and third-party tools can create them.
Patch reads and writes this format (`patch_core::eq::Preset`): built-in
presets ship with Patch; saved ones live in
`~/.config/omarchy-patch/eq-presets/<name>.json`, and any valid file dropped
there appears in the list. Load or save on any EQ target:

```sh
omarchy-patch eq presets                  # built-in and saved
omarchy-patch eq preset warm              # the master
omarchy-patch eq a preset "voice clarity" # deck A
omarchy-patch eq apps save "Late night"   # the apps EQ as a preset
```

In the EQ window: the preset button (or `p` / `P`) steps through them;
Save stores the current EQ (overwriting the loaded preset when it is one
of yours, else as "Custom n"). A loaded preset shows `*` once edited.

Optional `gain_db` (±18, default 0) is the input gain before the bands: a
correction's preamp, so its boosts don't reach the limiter. `omarchy-patch
eq autoeq <headphone>` writes presets with one (an AutoEq profile refitted
onto the 8 bands, its source in `provenance`).

Version 1 as implemented: `parametric`, `linked`, up to 8 bands, band
`channel` `all`, types `bell`, `low_shelf`, `high_shelf`, `low_pass`,
`high_pass`, `notch`. Files that don't validate are skipped, never
half-applied. Unknown fields round-trip.

```json
{
  "format": "patch-eq",
  "version": 1,
  "id": "org.example.voice-cleanup",
  "name": "Voice cleanup",
  "description": "Gentle low cut and presence shaping",
  "created": "2026-09-27T00:00:00Z",
  "provenance": { "kind": "user", "tool": "Patch" },
  "mode": "parametric",
  "channel_mode": "linked",
  "bands": [
    { "id": "high-pass", "enabled": true, "type": "high_pass", "frequency_hz": 75.0, "q": 0.707, "channel": "all" },
    { "id": "presence", "enabled": true, "type": "bell", "frequency_hz": 3200.0, "gain_db": 1.5, "q": 1.1, "channel": "all" }
  ]
}
```

Unknown fields should be preserved by tooling where possible. Importers must
validate version, ranges, band count, channel target, and filter type before
preparing DSP state. Tune calibration and protection data are explicitly out
of scope and must never be embedded in an EQ preset.

