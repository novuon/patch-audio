# Versioning and compatibility

Patch DJ uses independent package-format and SQLite-schema versions.

* `manifest.format_version` currently equals `1`; an incompatible package
  semantic change increments it. Unknown versions are rejected read-only.
* `manifest.schema_version` mirrors SQLite `PRAGMA user_version` (currently 4). Additive
  changes increment it and have an explicit forward migration.
* Every analysis result carries analyzer ID/version, output schema version,
  input fingerprint, timestamp and confidence. Model output compatibility is
  distinct from package compatibility.
* Extensions and sidecars carry their own schema/format versions.

Within a major package version, readers ignore unknown optional fields and
preserve opaque extension payloads. Writers must not discard fields they do
not understand when round-tripping. A reader that cannot safely preserve an
unknown required semantic must refuse to write the package.

No analyzer version is allowed to rewrite human/performance data. Humans can
accept, reject, edit or reset a generated result; that action is stored as a
new provenance-bearing decision. Results should be retained, superseded, or
explicitly pruned by a user-visible archive operation.

Timestamps are UTC RFC 3339. IDs are UUIDs for portable entities; package-local
integer row keys are internal only. Numeric values must be finite and checked
before narrowing to binary types.
