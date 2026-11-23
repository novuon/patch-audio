//! Additive SQLite schema migrations. `user_version` is the schema version.
//! Every migration is one transaction and refuses databases newer than this
//! reader. Existing human data is never rewritten by a migration.

use rusqlite::Connection;

const MIGRATIONS: [&str; 6] = [
    "CREATE TABLE tracks (
        id INTEGER PRIMARY KEY,
        content_id TEXT NOT NULL UNIQUE,
        path TEXT NOT NULL,
        size INTEGER NOT NULL,
        mtime INTEGER NOT NULL DEFAULT 0,
        title TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '',
        album TEXT NOT NULL DEFAULT '', genre TEXT NOT NULL DEFAULT '',
        codec TEXT NOT NULL DEFAULT '', rate INTEGER NOT NULL DEFAULT 0,
        bits INTEGER NOT NULL DEFAULT 0, channels INTEGER NOT NULL DEFAULT 0,
        duration REAL NOT NULL DEFAULT 0, added_at INTEGER NOT NULL DEFAULT 0,
        last_played INTEGER, plays INTEGER NOT NULL DEFAULT 0,
        missing INTEGER NOT NULL DEFAULT 0, bpm REAL, key TEXT, lufs REAL,
        energy REAL, rating INTEGER NOT NULL DEFAULT 0, tags TEXT, color TEXT
    );
    CREATE INDEX tracks_path ON tracks(path);
    CREATE TABLE analysis (track_id INTEGER PRIMARY KEY REFERENCES tracks(id), version INTEGER NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
    CREATE TABLE performance (track_id INTEGER PRIMARY KEY REFERENCES tracks(id), data TEXT NOT NULL);
    CREATE TABLE history (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, event TEXT NOT NULL, deck TEXT, track_id INTEGER, data TEXT);
    CREATE TABLE playlists (id INTEGER PRIMARY KEY, name TEXT NOT NULL, smart TEXT);
    CREATE TABLE playlist_items (playlist_id INTEGER NOT NULL REFERENCES playlists(id), pos INTEGER NOT NULL, track_id INTEGER NOT NULL REFERENCES tracks(id));",
    "CREATE TABLE package_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE source_metadata (track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE, data TEXT NOT NULL);
    CREATE TABLE analysis_results (
        id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        kind TEXT NOT NULL, analyzer_id TEXT NOT NULL, analyzer_version TEXT NOT NULL,
        schema_version INTEGER NOT NULL, created_at TEXT NOT NULL,
        confidence REAL, inputs TEXT NOT NULL, data TEXT NOT NULL,
        supersedes TEXT REFERENCES analysis_results(id), status TEXT NOT NULL DEFAULT 'current'
    );
    CREATE INDEX analysis_track_kind ON analysis_results(track_id, kind, created_at);
    CREATE TABLE cues (
        id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        kind TEXT NOT NULL, sample_position INTEGER NOT NULL, sample_rate INTEGER NOT NULL,
        beat_position REAL, label TEXT NOT NULL DEFAULT '', color TEXT, comment TEXT NOT NULL DEFAULT '',
        origin TEXT NOT NULL, confidence REAL, created_at TEXT NOT NULL, modified_at TEXT NOT NULL,
        generated INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX cues_track ON cues(track_id);
    CREATE TABLE loops (
        id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        start_sample INTEGER NOT NULL, end_sample INTEGER NOT NULL, sample_rate INTEGER NOT NULL,
        beat_length REAL, label TEXT NOT NULL DEFAULT '', quantized INTEGER NOT NULL DEFAULT 0,
        temporary INTEGER NOT NULL DEFAULT 0, origin TEXT NOT NULL, created_at TEXT NOT NULL,
        CHECK(start_sample >= 0 AND end_sample > start_sample)
    );
    CREATE INDEX loops_track ON loops(track_id);
    CREATE TABLE beatgrids (
        id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
        layer TEXT NOT NULL, analyzer_id TEXT, analyzer_version TEXT, created_at TEXT NOT NULL,
        confidence REAL, time_signature_num INTEGER, time_signature_den INTEGER,
        segments TEXT NOT NULL, provenance TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX beatgrids_track_layer ON beatgrids(track_id, layer);
    CREATE TABLE playlists_v2 (id TEXT PRIMARY KEY, name TEXT NOT NULL, parent_id TEXT REFERENCES playlists_v2(id), kind TEXT NOT NULL, rules TEXT, created_at TEXT NOT NULL);
    CREATE TABLE playlist_entries (playlist_id TEXT NOT NULL REFERENCES playlists_v2(id) ON DELETE CASCADE, position INTEGER NOT NULL, track_id INTEGER NOT NULL REFERENCES tracks(id), PRIMARY KEY(playlist_id, position));
    CREATE TABLE sets (id TEXT PRIMARY KEY, name TEXT NOT NULL, data TEXT NOT NULL, created_at TEXT NOT NULL, modified_at TEXT NOT NULL);
    CREATE TABLE transitions (id TEXT PRIMARY KEY, source_track INTEGER NOT NULL REFERENCES tracks(id), destination_track INTEGER NOT NULL REFERENCES tracks(id), kind TEXT NOT NULL, status TEXT NOT NULL, data TEXT NOT NULL, provenance TEXT NOT NULL, created_at TEXT NOT NULL);
    CREATE TABLE sessions (id TEXT PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT, data TEXT NOT NULL);
    CREATE TABLE performance_events (id INTEGER PRIMARY KEY, session_id TEXT REFERENCES sessions(id), occurred_at TEXT NOT NULL, kind TEXT NOT NULL, track_id INTEGER REFERENCES tracks(id), deck TEXT, sample_position INTEGER, data TEXT NOT NULL);
    CREATE INDEX performance_events_session ON performance_events(session_id, id);
    CREATE TABLE automation_curves (id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, target TEXT NOT NULL, encoding TEXT NOT NULL, sample_rate REAL NOT NULL, points BLOB NOT NULL);
    CREATE TABLE models (id TEXT PRIMARY KEY, version TEXT NOT NULL, capability TEXT NOT NULL, output_schema INTEGER NOT NULL, metadata TEXT NOT NULL);
    CREATE TABLE embeddings (id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE, model_id TEXT NOT NULL REFERENCES models(id), dimension INTEGER NOT NULL, dtype TEXT NOT NULL, normalization TEXT NOT NULL, source_input TEXT NOT NULL, sidecar TEXT NOT NULL, byte_offset INTEGER NOT NULL, byte_length INTEGER NOT NULL, created_at TEXT NOT NULL, confidence REAL);
    CREATE TABLE extensions (namespace TEXT PRIMARY KEY, schema_version INTEGER NOT NULL, data TEXT NOT NULL);
    CREATE TABLE assets (id TEXT PRIMARY KEY, track_id INTEGER REFERENCES tracks(id), kind TEXT NOT NULL, relative_path TEXT NOT NULL, media_type TEXT NOT NULL, size INTEGER NOT NULL, sha256 TEXT NOT NULL);
    CREATE TABLE package_files (relative_path TEXT PRIMARY KEY, size INTEGER NOT NULL, sha256 TEXT NOT NULL, role TEXT NOT NULL);",
    "ALTER TABLE tracks ADD COLUMN track_uuid TEXT;
    CREATE UNIQUE INDEX tracks_uuid ON tracks(track_uuid);",
    "CREATE TABLE models_v4 (id TEXT NOT NULL, version TEXT NOT NULL, capability TEXT NOT NULL, output_schema INTEGER NOT NULL, metadata TEXT NOT NULL, PRIMARY KEY(id,version));
    INSERT INTO models_v4 SELECT id,version,capability,output_schema,metadata FROM models;
    CREATE TABLE embeddings_v4 (id TEXT PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE, model_id TEXT NOT NULL, model_version TEXT NOT NULL, dimension INTEGER NOT NULL, dtype TEXT NOT NULL, normalization TEXT NOT NULL, source_input TEXT NOT NULL, sidecar TEXT NOT NULL, byte_offset INTEGER NOT NULL, byte_length INTEGER NOT NULL, created_at TEXT NOT NULL, confidence REAL, FOREIGN KEY(model_id,model_version) REFERENCES models_v4(id,version));
    INSERT INTO embeddings_v4 (id,track_id,model_id,model_version,dimension,dtype,normalization,source_input,sidecar,byte_offset,byte_length,created_at,confidence)
        SELECT e.id,e.track_id,e.model_id,m.version,e.dimension,e.dtype,e.normalization,e.source_input,e.sidecar,e.byte_offset,e.byte_length,e.created_at,e.confidence FROM embeddings e JOIN models m ON m.id=e.model_id;
    DROP TABLE embeddings;
    DROP TABLE models;
    ALTER TABLE models_v4 RENAME TO models;
    ALTER TABLE embeddings_v4 RENAME TO embeddings;",
    // Sources (folders and drives in the library), failed analyses (kept so
    // a broken file isn't retried forever, and shown with the reason), and
    // the analysis figures a status needs, next to BPM and key.
    "CREATE TABLE sources (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, name TEXT NOT NULL UNIQUE, added_at INTEGER NOT NULL, scanned_at INTEGER);
    CREATE TABLE analysis_failures (track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE, version INTEGER NOT NULL, error TEXT NOT NULL, at INTEGER NOT NULL);
    ALTER TABLE tracks ADD COLUMN grid_confidence REAL;
    ALTER TABLE tracks ADD COLUMN downbeat INTEGER;
    UPDATE tracks SET
        grid_confidence = (SELECT json_extract(a.data, '$.bpm_confidence') FROM analysis a WHERE a.track_id = tracks.id),
        downbeat = (SELECT json_extract(a.data, '$.downbeat') FROM analysis a WHERE a.track_id = tracks.id),
        energy = (SELECT (SELECT avg(value) FROM json_each(json_extract(a.data, '$.energy'))) / 255.0 FROM analysis a WHERE a.track_id = tracks.id);",
    // Analysis history: one result per track, kind and analyzer version
    // (the current one, else the newest). Every store used to add a copy.
    "CREATE TEMP TABLE keep_results AS
        SELECT id FROM analysis_results WHERE status = 'current'
        UNION SELECT (SELECT b.id FROM analysis_results b WHERE b.track_id = a.track_id AND b.kind = a.kind AND b.analyzer_version = a.analyzer_version ORDER BY b.created_at DESC, b.rowid DESC LIMIT 1)
        FROM analysis_results a GROUP BY a.track_id, a.kind, a.analyzer_version;
    UPDATE analysis_results SET supersedes = NULL WHERE supersedes IS NOT NULL AND supersedes NOT IN (SELECT id FROM keep_results);
    DELETE FROM analysis_results WHERE id NOT IN (SELECT id FROM keep_results);
    DROP TABLE keep_results;",
];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub fn migrate(db: &Connection) -> Result<(), String> {
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version < 0 || version as usize > MIGRATIONS.len() {
        return Err(format!(
            "unsupported Patch DJ schema version {version}; this build supports through {SCHEMA_VERSION}"
        ));
    }
    db.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    db.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    // Migration 6 frees most of a grown library: give the space back.
    let compact = (version as usize) < 6 && MIGRATIONS.len() >= 6;
    for i in version as usize..MIGRATIONS.len() {
        let next = i + 1;
        db.execute_batch(&format!(
            "BEGIN IMMEDIATE;\n{}\nPRAGMA user_version = {next};\nCOMMIT;",
            MIGRATIONS[i]
        ))
        .map_err(|e| {
            let _ = db.execute_batch("ROLLBACK;");
            format!("schema migration {next} failed; transaction rolled back: {e}")
        })?;
    }
    if compact && version > 0 {
        let _ = db.execute_batch("VACUUM;");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_v1_transactionally_and_rejects_future_versions() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(MIGRATIONS[0]).unwrap();
        db.execute_batch("PRAGMA user_version=1; INSERT INTO tracks(content_id,path,size,title) VALUES('cid','/music/a.flac',8,'A'); INSERT INTO performance(track_id,data) VALUES(1,'{\"notes\":\"keep me\"}');").unwrap();
        migrate(&db).unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        let note: String = db
            .query_row("SELECT data FROM performance WHERE track_id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION as i64);
        assert_eq!(note, r#"{"notes":"keep me"}"#);
        db.execute_batch("PRAGMA user_version=999;").unwrap();
        assert!(migrate(&db).unwrap_err().contains("unsupported"));
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            999
        );
    }

    #[test]
    fn v3_to_v4_preserves_embedding_and_model_provenance() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(MIGRATIONS[0]).unwrap();
        db.execute_batch(MIGRATIONS[1]).unwrap();
        db.execute_batch(MIGRATIONS[2]).unwrap();
        db.execute_batch("INSERT INTO tracks(content_id,path,size,title,track_uuid) VALUES('cid','/a',4,'A','12345678-1234-4234-a234-123456789abc'); INSERT INTO models(id,version,capability,output_schema,metadata) VALUES('org.test.embed','model-7','semantic',1,'{}'); INSERT INTO embeddings(id,track_id,model_id,dimension,dtype,normalization,source_input,sidecar,byte_offset,byte_length,created_at) VALUES('e1',1,'org.test.embed',3,'f32','l2','pcm','analysis/e1.pev',0,28,'2026-01-01T00:00:00Z'); PRAGMA user_version=3;").unwrap();
        migrate(&db).unwrap();
        let version: String = db
            .query_row(
                "SELECT model_version FROM embeddings WHERE id='e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "model-7");
        let fk: i64 = db
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fk, 0);
    }
}
