# Examples

## Manifest

```json
{
  "format": "patch-dj",
  "format_version": 1,
  "schema_version": 4,
  "package_id": "491dfd6a-fd80-4b8b-a5f4-9f08e653a862",
  "mode": "set",
  "name": "Friday night",
  "created_at": "2026-09-26T19:00:00Z",
  "audio_policy": "relative_or_embedded",
  "extensions": [{"namespace": "org.patch.stems", "schema_version": 1}]
}
```

## Structured transition plan

```json
{
  "source_track": 17,
  "destination_track": 23,
  "source_mix_out_sample": 13230000,
  "destination_mix_in_sample": 0,
  "duration_samples": 3528000,
  "sample_rate": 44100,
  "strategy": {
    "phrase_alignment_beats": 32,
    "tempo_ratio": 1.016,
    "key_relation": "relative_minor",
    "energy_delta": 0.08,
    "fader_curve": [[0.0, 0.0], [8.0, 0.5], [32.0, 1.0]],
    "eq_moves": [{"target":"deck-a.low","at_seconds":12.0,"value":-0.5}]
  },
  "confidence": 0.82,
  "notes": "DJ approved"
}
```

The transition is deterministic structured intent. AI rationale is separate
provenance; a recommendation does not become a plan or an executed action
until accepted and logged.

## Waveform sidecar

See the byte-level PWF1 definition in [SCHEMA.md](SCHEMA.md). The reader
rejects non-finite values, invalid dimensions, checksum errors, truncation,
count mismatch and trailing bytes. A waveform is derived and disposable; the
original audio is not.

The checked-in `fixtures/reference.patchdj` is generated using
`cargo run -p patch-library --example build_reference -- /tmp/reference.patchdj`.
Its one-second stereo
PCM tracks are deterministic synthetic tones; it contains a variable-tempo
track, a corrected grid alongside the automatic grid, two cue types, a saved
loop, waveform pyramid, quantized embedding, playlist, prepared set,
transition plan, session events and a non-executable extension payload.
