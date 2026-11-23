//! Adapter boundary: foreign catalogs terminate here and produce native
//! values plus provenance. No adapter is required to open/play a package.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProvenance {
    pub ecosystem: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub imported_at: String,
    pub external_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportRecord {
    pub record_type: String,
    pub external_id: Option<String>,
    pub original_fields: Value,
    pub provenance: ImportProvenance,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportBatch {
    pub records: Vec<ImportRecord>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub written: usize,
    pub lossy_fields: Vec<String>,
    pub warnings: Vec<String>,
}

pub trait Importer {
    fn id(&self) -> &'static str;
    fn import(&self, input: &Path) -> Result<ImportBatch, String>;
}

pub trait Exporter {
    fn id(&self) -> &'static str;
    fn export(&self, package: &Path, output: &Path) -> Result<ExportReport, String>;
}

/// Names deliberately reserve the ecosystem boundary without making vendor
/// libraries or proprietary applications runtime dependencies.
pub const ADAPTER_IDS: &[&str] = &[
    "rekordbox",
    "serato",
    "mixxx",
    "engine-dj",
    "m3u",
    "audio-folder",
];
