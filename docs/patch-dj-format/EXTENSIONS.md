# Extension contract

Extension namespaces use reverse-DNS lowercase identifiers, for example
`org.patch.stems`, `org.patch.crowd`, or `com.example.analysis`. The package
manifest declares namespace and extension schema version. The database
stores opaque JSON in `extensions`; large arrays/audio use sidecars beneath a
namespace-scoped directory and must declare path, media type, byte length,
format version and checksum in the package inventory.

An extension MUST NOT redefine track identity, source/analysis/performance
authority, time units, core cue/loop/transition meaning, or package security
rules. It MUST NOT contain executable code or cause a reader to load plugins.
Core readers preserve unknown extension data and report unsupported required
capabilities. Extension-owned files must remain within the package root, use
normalized relative paths, and obey package resource limits.

Third-party implementations should publish a schema, version policy, bounds,
and at least one portable example. Namespaces are identifiers, not proof of
ownership or signatures.
