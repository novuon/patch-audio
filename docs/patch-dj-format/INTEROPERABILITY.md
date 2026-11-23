# Interoperability and adapters

Patch DJ is the internal and interchange source of truth. Importers translate
external concepts into Patch's track, playlist, set, cue, grid, transition,
analysis and event models. Exporters translate out and report every lossy or
unsupported field. No external DJ application is required at runtime.

An adapter records source ecosystem, adapter ID/version, import time,
external object IDs and original fields in provenance. Original audio is
unchanged. Import is transactional: malformed records are isolated with
diagnostics; they do not partially overwrite human data. Reimport updates
only the adapter-owned/imported namespace unless a person explicitly chooses
to replace a Patch field.

Potential adapters include Rekordbox XML/device libraries, Serato, Mixxx,
Engine DJ, M3U/M3U8, and ordinary audio folders. This repository currently
implements native package/library storage only; these external adapters are
specified as future work. M3U is a playlist adapter, never Patch's library
model. Proprietary format behavior may be tested from public documentation;
GPL/proprietary implementation code must not be copied into this MIT project.
