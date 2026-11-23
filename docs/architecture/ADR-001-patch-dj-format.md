# ADR-001: Patch DJ is the native DJ source of truth

Status: Accepted

## Decision

Patch defines and owns the native Patch DJ package and library. DJ Booth reads
and writes that model. External ecosystems (including Rekordbox, Serato,
Mixxx, Engine DJ and M3U) are import/export adapters only. Patch tracks use
stable UUID identity; a filesystem path is a relocatable source locator.

Persistent data is separated into source facts, versioned derived analysis,
and human/performance records. Reanalysis adds or supersedes analysis results
without modifying cues, human grid corrections, loops, notes, ratings,
prepared sets, decisions or actual performance history. SQLite is the
transactional metadata store; large numeric data uses versioned sidecars;
audio stays external by default and can be embedded for portable packages.

The playback callback consumes precompiled runtime state and never accesses
SQLite, files, JSON, UI or network services. AI is optional and its results
are recommendations/provenance, not implicit decisions or actions.

## Rationale

Patch needs richer native semantics than any one third-party catalog format
and must work without proprietary DJ software. A native contract can model
variable tempo, arbitrary meters, human corrections, structured transitions,
portable sets and optional AI without inheriting another application's
limitations. SQLite is mature, inspectable, widely implemented and suited to
concurrent metadata access; versioned sidecars keep waveform/vector payloads
efficient without making a custom container the first requirement.

## Consequences

Patch must publish schema, sidecar and migration contracts and maintain
fixtures/validators. Adapters must preserve source provenance and report loss.
Package updates need transactional/crash recovery and explicit migration
behavior. Existing path-keyed cues and ephemeral deck settings require
identity-based migration before they can be considered fully native.

## Scope of this first implementation

Implemented: SQLite library, stable track UUIDs, fast relocation fingerprints,
separate analysis/performance stores, additive migrations, package directory
manifest, package validation, structured cue/grid/set/transition/session
records, and PWF1 waveform encoding. The complete independent interchange
contract is documented. Compressed archive packing, full external adapters,
all analysis algorithms, complete set editing UI, rich tag extraction,
and migration of every legacy preference remain planned.
