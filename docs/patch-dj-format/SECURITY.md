# Package security

Treat every package as untrusted. Never execute package contents, load code,
or follow an extension instruction that triggers a plugin. Reject absolute
and parent-traversing relative paths. Reject symlink package roots, manifest,
database and referenced assets; canonicalized asset paths must remain inside
the package root. Ignore unrelated files unless declared.

Bound manifest/metadata size, counts, nesting, sidecar dimensions and total
allocation before parsing. Validate integer arithmetic with checked operations
and validate finite floats/ranges. SQLite integrity and foreign-key checks are
required for validation. Corrupt derived data can be regenerated; corrupt
human data must produce an error and be preserved for recovery.

Audio content hashing is staged: startup uses size/mtime plus a bounded
fingerprint; full cryptographic hashing is explicit or cached. Integrity
checksums must describe exact bytes and must not be treated as authenticity
signatures. Archive readers, if added, must cap expanded size, entry count,
path depth and compression ratio before extracting to a temporary directory.

Crowd/environment observations store derived metrics by default. Raw
microphone or camera recordings require explicit user action, clear source
and retention metadata, and an opt-in package field.
