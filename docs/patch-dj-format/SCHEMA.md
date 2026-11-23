# SQLite schema, version 4

The exact schema is created by `patch-library/src/schema.rs`; migrations are
append-only. These table families define semantics, not a requirement that an
implementation use Patch's Rust types.

| Family | Tables | Authority |
|---|---|---|
| Track/source | `tracks`, `source_metadata` | stable UUID, source locators and imported facts |
| Analysis | `analysis`, `analysis_results`, `beatgrids`, `models`, `embeddings` | replaceable derived results with provenance/history |
| Human prep | `performance`, `cues`, `loops`, `playlists`, `playlist_entries`, `playlists_v2`, `sets` | DJ-created choices; analyzer updates cannot overwrite |
| Mixing | `transitions` | structured transition plans and provenance |
| Performance | `sessions`, `performance_events`, `automation_curves`, `history` | actual actions, recorded separately from recommendations |
| Package | `package_meta`, `extensions`, `assets`, `package_files` | declared package metadata and integrity inventory |

## Track identity and source

`tracks.id` is a package-local integer foreign key. `tracks.track_uuid` is the
portable stable identity and MUST remain unchanged when source paths change.
`content_id` is a fast, non-cryptographic fingerprint. `path` is the active
locator, not identity. `source_metadata` stores original/import provenance and
additional source facts as JSON; analysis belongs elsewhere. Implementations
should retain container, codec, sample rate, bit depth, channel count, sample
count/duration, tags, artwork references, file size and optional full hash.

## Analysis provenance

`analysis_results` rows are append-only. `kind`, analyzer/model identity and
version, output schema, timestamp, confidence, inputs, data, and supersession
are explicit. A reanalysis inserts a new row and links the old result through
`supersedes`; it never mutates cues, loops, human beatgrid layers, notes,
ratings or performance history. Low-confidence or conflicting analyses are
valid and must not be presented as facts.

`beatgrids.layer` distinguishes machine base data from human correction data.
`segments` describes piecewise tempo: start/end samples, starting/ending BPM,
first beat, bar grouping/downbeat phase, and confidence. A partial human layer
overrides only its covered regions. Preserve the machine layer for later
comparison and reanalysis.

## Cues and loops

Cue `kind` is semantic text (`temporary`, `memory`, `hot`, `load`, `mix_in`,
`mix_out`, or a namespaced custom kind). Each row has stable ID, exact sample
position/rate, optional beat position, label/color/comment, origin,
confidence, timestamps, and generated/human state. There is no format-level
cue count limit. Loop rows store exact start and end samples independently of
BPM, with optional beat length, quantization, temporary/saved state and
origin. `end_sample` is exclusive and strictly greater than start.

## Playlists, sets and transitions

Playlists have ordered entries and may carry JSON query rules. Rule vocabulary
can grow; unknown rules should be reported and preserved. Sets are separate
entities, stored as versioned JSON in `sets.data`, with ordered track refs,
alternates/branches, transitions, notes, expected timing, energy intent,
rehearsals and human overrides.

Transition `data` is a structured plan: source/destination track, sample
positions, duration/rate, phrase alignment, tempo ratio, key relationship,
energy delta and time/value curves for faders/EQ/effects/stems. Free text is
optional rationale only. Recommendation, plan, performer decision and
executed action are separate states/events.

## Sessions, AI and extensions

Sessions group discrete performance events and optional sampled automation
curves. Do not log each audio frame; use discrete actions and bounded-rate
control samples encoded as a curve. Event time is UTC, with audio sample
position and graph clock metadata when available. Crowds/environment data is
optional derived metrics with source, confidence, privacy and retention
metadata; raw audio/video is never implicit.

Embedding rows identify model, dimension, dtype, normalization, input kind,
sidecar path, offset/length, creation time and confidence. Dimensions and
dtypes are per-row; implementations must not assume a universal vector size.
Model registry rows define analyzer/model capabilities and output schemas.

## Binary sidecars (reference encodings)

`PWF1` is the multiresolution waveform encoding. Header is 24 bytes: magic
`PWF1`; little-endian u16 version `1`; u16 flags `0`; u32 sample rate; u16
channels; u16 level count; u32 total-bin count; u32 CRC-32/ISO-HDLC of the
payload. Each level is u32 bin count followed by `[min,max,rms]` IEEE-754
float32 triples in little endian. Values are normalized to `[-1,1]` (RMS is
nonnegative). A reader checks dimensions, finite values, CRC, counts, exact
length and no trailing bytes. Min/max retain transients that an average-only
overview would hide. Levels progress from overview to finer resolution.

`PEV1` is a Patch embedding vector: a 16-byte little-endian header (magic,
u16 version `1`, u16 dtype `1=f32` or `2=i8`, u32 dimensions, f32 quantization
scale), packed vector values, then u32 CRC-32/ISO-HDLC over header and values.
F32 values are finite little-endian float32. I8 values are signed bytes and
decode as `value * scale`. Model/version, normalization, source input and
timestamp live in SQLite. SHA-256 in the package inventory protects the full
sidecar bytes as well.

Extensions use reverse-DNS lowercase namespaces and isolated versioned JSON
payloads or namespaced sidecars. Extensions cannot redefine core fields.
Readers that do not understand an extension preserve it opaquely and never
execute it. See [EXTENSIONS.md](EXTENSIONS.md).
