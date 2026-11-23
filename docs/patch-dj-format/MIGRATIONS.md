# Migrations and recovery

`patch-library::schema::migrate` is the reference migration runner. It reads
SQLite `user_version`, applies each migration under `BEGIN IMMEDIATE`, updates
the version in the same transaction, and rejects a database newer than the
reader. Migration failure rolls back the current migration. It does not run
down migrations.

Before a package schema migration, Patch checkpoints WAL and copies the
database to `library.db.pre-schema-N.bak`. The backup is retained; cleanup is
an explicit user operation. The current migrations are additive. If a future
migration changes or removes data, it must first create and verify a backup,
provide a dry-run/report, and require an explicit migration command. Never
silently apply destructive transforms.

If a process stops during a SQLite transaction, SQLite recovery is used on
open. Derived sidecars are written to temporary siblings and atomically
renamed only after complete validation; incomplete temporary files are
unreferenced and safe to remove after validation. Human edits commit in one
SQLite transaction. A broken database produces a diagnostic and is not
replaced by a blank database.

Schema tests must open each supported prior version, migrate, run
`PRAGMA integrity_check` and foreign-key checks, and prove that human tables
survive. Unknown future versions must fail without modifying bytes.

Schema 4 stores model identities as `(model ID, model version)` and copies
each embedding's exact model version into its own row. The v3→v4 migration
retains old model and embedding records while rebuilding those relations.
