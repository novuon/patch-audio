//! Native `.patchdj` directory packages. The package is a portable directory
//! with a manifest and an ordinary SQLite database; it never loads code.

use crate::{Change, Facts, Library, schema};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const FORMAT_NAME: &str = "patch-dj";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageMode {
    Library,
    Portable,
    Set,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub schema_version: u32,
    pub package_id: String,
    pub mode: PackageMode,
    pub name: String,
    pub created_at: String,
    pub audio_policy: String,
    #[serde(default)]
    pub extensions: Vec<ExtensionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRef {
    pub namespace: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub tracks: usize,
    pub missing_audio: usize,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

pub struct Package {
    root: PathBuf,
    manifest: Manifest,
    pub library: Library,
}

impl Package {
    /// Attach a manifest and standard directories to an existing local
    /// database. Used for crash recovery and the pre-package library upgrade.
    pub fn adopt(root: &Path, name: &str, mode: PackageMode) -> Result<Self, String> {
        if name.trim().is_empty() || name.len() > 512 {
            return Err("invalid package name".into());
        }
        let md = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
        if md.file_type().is_symlink() || !md.is_dir() {
            return Err("package root must be a real directory".into());
        }
        let db_path = root.join("library.db");
        let dbmd =
            std::fs::symlink_metadata(&db_path).map_err(|e| format!("missing library.db: {e}"))?;
        if dbmd.file_type().is_symlink() || !dbmd.is_file() {
            return Err("library.db must be a regular file".into());
        }
        // The schema version first; the WAL is folded in only when a
        // migration needs a consistent backup. (A FULL checkpoint on every
        // open, without waiting for locks, failed whenever the librarian
        // was writing: the AI DJ then saw an empty library.)
        let observed: i64 = {
            let c = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
            let _ = c.busy_timeout(std::time::Duration::from_secs(5));
            let v: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if v < schema::SCHEMA_VERSION as i64 {
                c.execute_batch("PRAGMA wal_checkpoint(FULL);")
                    .map_err(|e| e.to_string())?;
            }
            v
        };
        if observed < schema::SCHEMA_VERSION as i64 {
            let backup = root.join(format!("library.db.pre-schema-{observed}.bak"));
            if backup.exists() {
                return Err(format!(
                    "migration backup already exists: {}",
                    backup.display()
                ));
            }
            std::fs::copy(&db_path, &backup)
                .map_err(|e| format!("could not create migration backup: {e}"))?;
        }
        for sub in [
            "tracks",
            "playlists",
            "sets",
            "history",
            "transitions",
            "models",
            "assets",
            "audio",
            "analysis",
        ] {
            let p = root.join(sub);
            if p.exists() && std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(format!("package directory is a symlink: {sub}"));
            }
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let library = Library::open(&db_path)?;
        let manifest = Manifest {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            schema_version: schema::SCHEMA_VERSION,
            package_id: package_id(),
            mode,
            name: name.into(),
            created_at: timestamp(),
            audio_policy: if mode == PackageMode::Library {
                "external_references"
            } else {
                "relative_or_embedded"
            }
            .into(),
            extensions: vec![],
        };
        write_manifest(root, &manifest)?;
        Ok(Package {
            root: root.to_path_buf(),
            manifest,
            library,
        })
    }

    /// Create a package in an empty/nonexistent directory. For reference
    /// libraries, callers may continue adding external source references;
    /// Portable and Set modes may additionally include `audio/` assets.
    pub fn create(root: &Path, name: &str, mode: PackageMode) -> Result<Self, String> {
        if name.trim().is_empty() || name.len() > 512 {
            return Err("package name must contain 1 to 512 bytes".into());
        }
        if root.exists() {
            let md = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
            if md.file_type().is_symlink() || !md.is_dir() {
                return Err("package destination must be a real directory".into());
            }
            if std::fs::read_dir(root)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
            {
                return Err("package destination is not empty".into());
            }
        }
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        for sub in [
            "tracks",
            "playlists",
            "sets",
            "history",
            "transitions",
            "models",
            "assets",
            "audio",
            "analysis",
        ] {
            std::fs::create_dir(root.join(sub)).map_err(|e| e.to_string())?;
        }
        let manifest = Manifest {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            schema_version: schema::SCHEMA_VERSION,
            package_id: package_id(),
            mode,
            name: name.into(),
            created_at: timestamp(),
            audio_policy: match mode {
                PackageMode::Library => "external_references".into(),
                PackageMode::Portable | PackageMode::Set => "relative_or_embedded".into(),
                PackageMode::Archive => "embedded_or_verified_references".into(),
            },
            extensions: Vec::new(),
        };
        let db_path = root.join("library.db");
        let library = Library::open(&db_path)?;
        write_manifest(root, &manifest)?;
        Ok(Package {
            root: root.to_path_buf(),
            manifest,
            library,
        })
    }

    pub fn open(root: &Path) -> Result<Self, String> {
        let md = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
        if md.file_type().is_symlink() || !md.is_dir() {
            return Err("package root must be a real directory".into());
        }
        let manifest_path = root.join("manifest.json");
        let manifest_md = std::fs::symlink_metadata(&manifest_path)
            .map_err(|e| format!("missing manifest.json: {e}"))?;
        if manifest_md.file_type().is_symlink()
            || !manifest_md.is_file()
            || manifest_md.len() > 1024 * 1024
        {
            return Err("manifest.json must be a regular file no larger than 1 MiB".into());
        }
        let bytes = std::fs::read(&manifest_path).map_err(|e| e.to_string())?;
        let mut manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid manifest.json: {e}"))?;
        if manifest.format != FORMAT_NAME {
            return Err(format!("not a {FORMAT_NAME} package"));
        }
        if manifest.format_version != FORMAT_VERSION {
            return Err(format!(
                "unsupported package format version {}; supported version is {FORMAT_VERSION}",
                manifest.format_version
            ));
        }
        if manifest.schema_version > schema::SCHEMA_VERSION {
            return Err(format!(
                "package requires schema {}; this build supports through {}",
                manifest.schema_version,
                schema::SCHEMA_VERSION
            ));
        }
        validate_manifest(&manifest)?;
        let db_path = root.join("library.db");
        if std::fs::symlink_metadata(&db_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("library.db must not be a symlink".into());
        }
        if !db_path.is_file() {
            return Err("package has no regular library.db".into());
        }
        // The schema version first; the WAL is folded in only when a
        // migration needs a consistent backup. (A FULL checkpoint on every
        // open, without waiting for locks, failed whenever the librarian
        // was writing: the AI DJ then saw an empty library.)
        let observed: i64 = {
            let c = rusqlite::Connection::open(&db_path).map_err(|e| e.to_string())?;
            let _ = c.busy_timeout(std::time::Duration::from_secs(5));
            let v: i64 = c
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if v < schema::SCHEMA_VERSION as i64 {
                c.execute_batch("PRAGMA wal_checkpoint(FULL);")
                    .map_err(|e| e.to_string())?;
            }
            v
        };
        if observed < schema::SCHEMA_VERSION as i64 {
            let backup = root.join(format!("library.db.pre-schema-{observed}.bak"));
            if backup.exists() {
                return Err(format!(
                    "migration backup already exists: {}",
                    backup.display()
                ));
            }
            std::fs::copy(&db_path, &backup)
                .map_err(|e| format!("could not create migration backup: {e}"))?;
        }
        let library = Library::open(&db_path)?;
        if manifest.schema_version < schema::SCHEMA_VERSION {
            manifest.schema_version = schema::SCHEMA_VERSION;
            write_manifest(root, &manifest)?;
        }
        Ok(Package {
            root: root.to_path_buf(),
            manifest,
            library,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Add a source without touching it. Embedded copies use deterministic
    /// package-relative names and are atomically installed before the DB row
    /// points at them.
    pub fn add_audio(
        &self,
        source: &Path,
        facts: impl Fn(&Path) -> Option<Facts>,
        embed: bool,
    ) -> Result<(i64, Change), String> {
        if !source
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| crate::EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(x)))
        {
            return Err("unsupported audio extension".into());
        }
        if embed && matches!(self.manifest.mode, PackageMode::Library) {
            return Err("library mode does not embed source audio".into());
        }
        let (track, change) = self.library.import(source, &facts)?;
        if embed {
            let track_uuid: String = self
                .library
                .db
                .query_row("SELECT track_uuid FROM tracks WHERE id=?1", [track], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?;
            let ext = source
                .extension()
                .and_then(|s| s.to_str())
                .filter(|s| s.len() <= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
                .unwrap_or("audio");
            let rel = PathBuf::from("audio").join(format!("{track_uuid}.{ext}"));
            safe_package_target(&self.root, &rel)?;
            let dest = self.root.join(&rel);
            let temp = dest.with_extension("part");
            let mut input = std::fs::File::open(source).map_err(|e| e.to_string())?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut input, &mut output)
                .and_then(|_| output.sync_all())
                .map_err(|e| e.to_string())?;
            std::fs::rename(&temp, &dest).map_err(|e| e.to_string())?;
            let size = std::fs::metadata(&dest).map_err(|e| e.to_string())?.len();
            let digest = crate::sha256_file(&dest)?;
            let tx = self
                .library
                .db
                .unchecked_transaction()
                .map_err(|e| e.to_string())?;
            let mut source_meta: serde_json::Value = tx
                .query_row(
                    "SELECT data FROM source_metadata WHERE track_id=?1",
                    [track],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let source_meta = source_meta
                .as_object_mut()
                .ok_or_else(|| "source metadata must be a JSON object".to_string())?;
            source_meta.insert(
                "source_uri".into(),
                serde_json::Value::String(rel.to_string_lossy().into_owned()),
            );
            source_meta.insert(
                "original_path".into(),
                serde_json::Value::String(source.to_string_lossy().into_owned()),
            );
            source_meta.insert("embedded".into(), serde_json::Value::Bool(true));
            tx.execute(
                "UPDATE tracks SET path=?1 WHERE id=?2",
                rusqlite::params![rel.to_string_lossy(), track],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("INSERT INTO source_metadata(track_id,data) VALUES(?1,?2) ON CONFLICT(track_id) DO UPDATE SET data=excluded.data",rusqlite::params![track,serde_json::Value::Object(source_meta.clone()).to_string()]).map_err(|e|e.to_string())?;
            let asset_id = format!("audio-{track}");
            tx.execute("INSERT INTO assets(id,track_id,kind,relative_path,media_type,size,sha256) VALUES(?1,?2,'audio',?3,'application/octet-stream',?4,?5) ON CONFLICT(id) DO UPDATE SET relative_path=excluded.relative_path,size=excluded.size,sha256=excluded.sha256",rusqlite::params![asset_id,track,rel.to_string_lossy(),size as i64,digest]).map_err(|e|e.to_string())?;
            tx.execute("INSERT INTO package_files(relative_path,size,sha256,role) VALUES(?1,?2,?3,'audio') ON CONFLICT(relative_path) DO UPDATE SET size=excluded.size,sha256=excluded.sha256",rusqlite::params![rel.to_string_lossy(),size as i64,digest]).map_err(|e|e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        } else {
            let mut source_meta: serde_json::Value = self
                .library
                .db
                .query_row(
                    "SELECT data FROM source_metadata WHERE track_id=?1",
                    [track],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let source_meta = source_meta
                .as_object_mut()
                .ok_or_else(|| "source metadata must be a JSON object".to_string())?;
            source_meta.insert(
                "source_uri".into(),
                serde_json::Value::String(source.to_string_lossy().into_owned()),
            );
            source_meta.insert(
                "original_path".into(),
                serde_json::Value::String(source.to_string_lossy().into_owned()),
            );
            source_meta.insert("embedded".into(), serde_json::Value::Bool(false));
            self.library.db.execute("INSERT INTO source_metadata(track_id,data) VALUES(?1,?2) ON CONFLICT(track_id) DO UPDATE SET data=excluded.data",rusqlite::params![track,serde_json::Value::Object(source_meta.clone()).to_string()]).map_err(|e|e.to_string())?;
        }
        Ok((track, change))
    }

    pub fn resolve_audio(&self, track_id: i64) -> Result<Option<PathBuf>, String> {
        let path: String = self
            .library
            .db
            .query_row("SELECT path FROM tracks WHERE id=?1", [track_id], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        let p = Path::new(&path);
        if p.is_absolute() {
            return Ok(p.is_file().then(|| p.to_path_buf()));
        }
        if !safe_relative_path(p) {
            return Err("unsafe relative audio path".into());
        }
        let full = self.root.join(p);
        let mut cursor = self.root.clone();
        for component in p.components() {
            cursor.push(component.as_os_str());
            if std::fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("package audio path traverses a symlink".into());
            }
        }
        let root = std::fs::canonicalize(&self.root).map_err(|e| e.to_string())?;
        let resolved = match std::fs::canonicalize(&full) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if !resolved.starts_with(root) {
            return Err("package audio path escapes package root".into());
        }
        Ok(resolved.is_file().then_some(resolved))
    }

    pub fn checkpoint(&self) -> Result<(), String> {
        self.library
            .db
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn write_waveform(
        &self,
        track: i64,
        waveform: &crate::waveform::Waveform,
    ) -> Result<PathBuf, String> {
        self.library.store_waveform(track, waveform)
    }

    /// Validate SQLite consistency, manifest/schema compatibility and safe
    /// relative asset paths. External source files may be missing; they are
    /// reported as warnings so packages can still be opened and relinked.
    pub fn validate(&self) -> ValidationReport {
        let mut report = ValidationReport::default();
        let integrity: Result<String, _> =
            self.library
                .db
                .pragma_query_value(None, "quick_check", |r| r.get(0));
        match integrity {
            Ok(v) if v == "ok" => {}
            Ok(v) => report.errors.push(format!("SQLite quick_check: {v}")),
            Err(e) => report
                .errors
                .push(format!("SQLite quick_check failed: {e}")),
        }
        let version: Result<i64, _> =
            self.library
                .db
                .pragma_query_value(None, "user_version", |r| r.get(0));
        match version {
            Ok(v) if v == self.manifest.schema_version as i64 => {}
            Ok(v) => report.errors.push(format!(
                "manifest schema {} differs from database schema {v}",
                self.manifest.schema_version
            )),
            Err(e) => report
                .errors
                .push(format!("cannot read database schema version: {e}")),
        }
        match self.library.tracks() {
            Ok(tracks) => {
                report.tracks = tracks.len();
                for t in tracks {
                    if t.content_id.is_empty() {
                        report
                            .errors
                            .push(format!("track {} has empty content identity", t.id));
                    }
                    if !t.duration.is_finite() || t.duration < 0.0 {
                        report
                            .errors
                            .push(format!("track {} has invalid duration", t.id));
                    }
                    if !t.missing {
                        match self.resolve_audio(t.id) {
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                report.missing_audio += 1;
                                report.warnings.push(format!(
                                    "track {} audio is unavailable: {}",
                                    t.id, t.path
                                ));
                            }
                            Err(e) => report
                                .errors
                                .push(format!("track {} has unsafe audio reference: {e}", t.id)),
                        }
                    }
                }
            }
            Err(e) => report.errors.push(format!("cannot read tracks: {e}")),
        }
        let fk: i64 = self
            .library
            .db
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap_or(-1);
        if fk != 0 {
            report
                .errors
                .push(format!("database has {fk} foreign key violations"));
        }
        let bad_cues: i64=self.library.db.query_row("SELECT COUNT(*) FROM cues WHERE sample_position<0 OR sample_rate<=0 OR (confidence IS NOT NULL AND (confidence<0 OR confidence>1))",[],|r|r.get(0)).unwrap_or(1);
        if bad_cues > 0 {
            report
                .errors
                .push(format!("{bad_cues} invalid cue records"));
        }
        let bad_loops: i64=self.library.db.query_row("SELECT COUNT(*) FROM loops WHERE start_sample<0 OR end_sample<=start_sample OR sample_rate<=0",[],|r|r.get(0)).unwrap_or(1);
        if bad_loops > 0 {
            report
                .errors
                .push(format!("{bad_loops} invalid loop records"));
        }
        let mut stmt = match self.library.db.prepare("SELECT relative_path FROM assets") {
            Ok(s) => s,
            Err(e) => {
                report.errors.push(format!("cannot inspect assets: {e}"));
                return report;
            }
        };
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for path in rows.flatten() {
                if !safe_relative_path(Path::new(&path)) {
                    report.errors.push(format!("unsafe asset path: {path}"));
                } else if std::fs::symlink_metadata(self.root.join(&path))
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    report
                        .errors
                        .push(format!("asset must not be a symlink: {path}"));
                }
            }
        }
        let files: Result<Vec<(String, i64, String, String)>, String> = (|| {
            let mut st = self
                .library
                .db
                .prepare("SELECT relative_path,size,sha256,role FROM package_files")
                .map_err(|e| e.to_string())?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())
        })();
        match files {
            Ok(rows) => {
                for (rel, size, hash, role) in rows {
                    let relp = Path::new(&rel);
                    if !safe_relative_path(relp) {
                        report
                            .errors
                            .push(format!("unsafe package file path: {rel}"));
                        continue;
                    }
                    if let Err(e) = safe_existing_package_path(&self.root, relp) {
                        report
                            .errors
                            .push(format!("unsafe package path {rel}: {e}"));
                        continue;
                    }
                    let path = self.root.join(relp);
                    let meta = match std::fs::symlink_metadata(&path) {
                        Ok(m) => m,
                        Err(e) => {
                            report
                                .errors
                                .push(format!("missing {role} file {rel}: {e}"));
                            continue;
                        }
                    };
                    if meta.file_type().is_symlink() || !meta.is_file() {
                        report
                            .errors
                            .push(format!("package file is not a regular file: {rel}"));
                        continue;
                    }
                    if meta.len() != size as u64 {
                        report.errors.push(format!("size mismatch for {rel}"));
                        continue;
                    }
                    match crate::sha256_file(&path) {
                        Ok(actual) if actual == hash => {}
                        Ok(_) => report.errors.push(format!("SHA-256 mismatch for {rel}")),
                        Err(e) => report.errors.push(format!("cannot hash {rel}: {e}")),
                    }
                    if rel.ends_with(".pwf") {
                        match std::fs::read(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|b| crate::waveform::Waveform::decode(&b))
                        {
                            Ok(_) => {}
                            Err(e) => report.errors.push(format!("invalid waveform {rel}: {e}")),
                        }
                    }
                    if rel.ends_with(".pev") {
                        match std::fs::read(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|b| crate::EmbeddingVector::decode(&b))
                        {
                            Ok(_) => {}
                            Err(e) => report.errors.push(format!("invalid embedding {rel}: {e}")),
                        }
                    }
                }
            }
            Err(e) => report
                .errors
                .push(format!("cannot inspect package files: {e}")),
        }
        report
    }
}

fn validate_manifest(m: &Manifest) -> Result<(), String> {
    if m.name.trim().is_empty() || m.name.len() > 512 {
        return Err("invalid package name".into());
    }
    if m.package_id.len() != 36
        || !m
            .package_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return Err("package_id must be a UUID string".into());
    }
    let mut seen = std::collections::HashSet::new();
    for ext in &m.extensions {
        if !valid_namespace(&ext.namespace) || !seen.insert(ext.namespace.as_str()) {
            return Err(format!(
                "invalid or duplicate extension namespace: {}",
                ext.namespace
            ));
        }
    }
    Ok(())
}

pub fn valid_namespace(s: &str) -> bool {
    s.split('.').count() >= 2
        && s.len() <= 255
        && s.split('.').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

pub fn safe_relative_path(p: &Path) -> bool {
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

pub(crate) fn safe_package_target(root: &Path, rel: &Path) -> Result<(), String> {
    if !safe_relative_path(rel) {
        return Err("unsafe relative package path".into());
    }
    let mut cursor = root.to_path_buf();
    for c in rel.components() {
        cursor.push(c.as_os_str());
        if std::fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("package path traverses a symlink".into());
        }
        if cursor != root.join(rel) && !cursor.exists() {
            std::fs::create_dir(&cursor).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn safe_existing_package_path(root: &Path, rel: &Path) -> Result<(), String> {
    if !safe_relative_path(rel) {
        return Err("unsafe relative path".into());
    }
    let mut cursor = root.to_path_buf();
    for c in rel.components() {
        cursor.push(c.as_os_str());
        if std::fs::symlink_metadata(&cursor).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("path traverses a symlink".into());
        }
    }
    Ok(())
}

fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), String> {
    let path = root.join("manifest.json");
    let temp = root.join("manifest.json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    f.write_all(&bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())?;
    if let Ok(dir) = std::fs::File::open(root) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn timestamp() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", n.as_secs(), n.subsec_nanos())
}

fn package_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_create_reopen_and_validate() {
        let root = std::env::temp_dir().join(format!("patchdj-package-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let p = Package::create(&root, "Reference set", PackageMode::Set).unwrap();
        assert_eq!(p.manifest().format, "patch-dj");
        assert_eq!(p.manifest().format_version, 1);
        drop(p);
        let p = Package::open(&root).unwrap();
        assert!(p.validate().is_valid(), "{:?}", p.validate().errors);
        assert!(root.join("library.db").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_traversal_and_invalid_namespaces() {
        assert!(!safe_relative_path(Path::new("../outside")));
        assert!(!safe_relative_path(Path::new("/etc/passwd")));
        assert!(safe_relative_path(Path::new("audio/track.flac")));
        assert!(valid_namespace("org.patch.analysis"));
        assert!(!valid_namespace("Patch.foo"));
    }

    #[test]
    fn embedded_audio_is_copied_and_relocatable() {
        let base = std::env::temp_dir().join(format!("patchdj-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let src = base.join("source.flac");
        let original = vec![0x5a; 128_000];
        std::fs::write(&src, &original).unwrap();
        let root = base.join("set.patchdj");
        let p = Package::create(&root, "Set", PackageMode::Set).unwrap();
        let (track, _) = p
            .add_audio(
                &src,
                |_| {
                    Some(Facts {
                        title: "Test track".into(),
                        rate: 44100,
                        ..Default::default()
                    })
                },
                true,
            )
            .unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), original);
        let embedded = p.resolve_audio(track).unwrap().unwrap();
        assert!(embedded.starts_with(&root));
        assert_eq!(std::fs::read(&embedded).unwrap(), original);
        assert!(p.validate().is_valid(), "{:?}", p.validate());
        std::fs::write(&embedded, b"tampered").unwrap();
        assert!(!p.validate().is_valid(), "integrity corruption is detected");
        std::fs::write(&embedded, &original).unwrap();
        assert!(p.validate().is_valid());
        drop(p);
        let moved = base.join("moved");
        std::fs::rename(&root, &moved).unwrap();
        let p = Package::open(&moved).unwrap();
        assert!(p.resolve_audio(track).unwrap().unwrap().starts_with(&moved));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn relink_requires_matching_content() {
        let base = std::env::temp_dir().join(format!("patchdj-relink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let root = base.join("lib.patchdj");
        let p = Package::create(&root, "Relink", PackageMode::Library).unwrap();
        let a = base.join("a.flac");
        let b = base.join("moved.flac");
        std::fs::write(&a, vec![3; 200_000]).unwrap();
        std::fs::write(&b, vec![3; 200_000]).unwrap();
        let (id, _) = p.add_audio(&a, |_| None, false).unwrap();
        let uuid: String = p
            .library
            .db
            .query_row("SELECT track_uuid FROM tracks WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(p.library.relink(&uuid, &b).unwrap(), id);
        assert_eq!(p.library.tracks().unwrap()[0].path, b.to_string_lossy());
        std::fs::write(&a, vec![4; 200_000]).unwrap();
        assert!(p.library.relink(&uuid, &a).is_err());
        drop(p);
        let _ = std::fs::remove_dir_all(base);
    }
}
