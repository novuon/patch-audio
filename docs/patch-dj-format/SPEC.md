# Patch DJ package format

Status: **format version 1, schema version 4, implemented in `patch-library`**. This document defines
the interoperable package contract. Patch is the reference implementation;
the format is independent of any DJ application or service.

## Terms and authority

* A **track** is an identity plus one or more source locations.
* A **package** is a `.patchdj` directory containing `manifest.json`,
  `library.db`, and optional assets/sidecars/audio.
* A **library** is a durable catalog, normally with external audio references.
* A **portable package** carries relative references and may embed audio.
* A **set package** carries the subset and preparation data intended for one
  performance. It can be partially planned.
* An **archive** is intended for preservation and may contain embedded audio.
* **Source** records describe imported material. **Analysis** records are
  versioned machine outputs. **Performance** records are human decisions and
  executed history. Analysis replacement MUST NOT mutate performance rows.

`manifest.json` is UTF-8 JSON. Its required fields are `format` (`patch-dj`),
`format_version` (currently integer `1`), `schema_version`, `package_id`
(UUID text), `mode`, `name`, `created_at` (UTC RFC 3339), and `audio_policy`.
`extensions` is an optional array of `{namespace,schema_version}` declarations.
Readers MUST reject unknown major format versions and MUST NOT silently
rewrite a package they cannot understand. Unknown JSON fields are ignored and
preserved by writers where practical.

Package modes are `library`, `portable`, `set`, and `archive`. A library may
reference absolute local files. Portable/set packages should use package
relative source paths; audio may be embedded under `audio/`. A reference-only
portable package remains valid while audio is unavailable and reports paths
that need relinking.

## Layout

```text
name.patchdj/
  manifest.json
  library.db                 SQLite, schema version in PRAGMA user_version
  tracks/                     optional per-track documents/sidecars
  playlists/                  optional human-readable exports
  sets/                       optional human-readable exports
  history/                    optional exports
  transitions/                optional exports
  models/                     optional model manifests, never executable code
  assets/                     artwork and other referenced media
  analysis/                   derived sidecars
  audio/                      optional embedded original audio
```

SQLite is the authoritative editable store. JSON files in the directories
above are interchange or cache artifacts and MUST NOT supersede database
records unless a future specification version says so. No executable content
is loaded from a package.

## SQLite contract

SQLite tables are documented in [SCHEMA.md](SCHEMA.md). `user_version` is the
schema version. Implementations MUST use transactions for related edits,
foreign keys, and crash-safe commits. The real-time audio callback MUST NOT
open the database; a loader compiles persistent rows into runtime state.
WAL is an implementation choice while editing. Before copying a live package,
checkpoint the WAL and close or snapshot the database.

Primary track identity is a persistent UUID. Content identity is a quick
fingerprint used to suggest a match after relocation; it is not an identity
and is not cryptographic proof. Paths are locators only. A reader should try
the package-relative URI, saved external URI, and relocation hints, then ask
the user to relink. Full audio SHA-256 may be computed lazily or on explicit
validation, never as a startup requirement.

## Time and numeric data

Times in interoperable track data are nonnegative integer sample positions
paired with their sample rate. Conversion to seconds is `sample / rate`.
Sample positions refer to decoded source timeline, before playback tempo or
key changes. UTC timestamps use RFC 3339 with `Z`. Floating-point values MUST
be finite; confidence values are in `[0,1]`. Unknown values are absent/null,
not invented defaults. Multichannel layouts and arbitrary time signatures
are allowed; no 4/4 assumption is implied.

## Audio, integrity, and portability

Original audio is never edited by Patch analysis. Embedded audio is copied
byte-for-byte. Relative paths MUST be normalized and MUST NOT contain parent
traversal, absolute roots, or symlink escapes. Missing external audio is a
warning with relink support, not grounds to discard metadata. Sidecar writes
use a temporary sibling followed by flush/sync and atomic rename. SQLite
transactions protect authoritative human metadata.

An implementation may verify package-file hashes on demand. Opening a
package must not hash multi-gigabyte sources synchronously. Missing or
corrupt derived sidecars may be regenerated; missing/corrupt human data is an
error and must never be silently replaced.

## Compatibility

The format and SQLite schema are separate version axes. A reader may support
the package version but not its database schema. It must report that case.
Additive schema changes increment `schema_version`; incompatible semantic
changes require a documented migration and version increment. See
[VERSIONING.md](VERSIONING.md) and [MIGRATIONS.md](MIGRATIONS.md).

External DJ formats are adapters. Their fields are mapped into Patch types
with import provenance. Lossy export is explicitly reported. They do not
define Patch's track, set, transition, cue, or session semantics.
