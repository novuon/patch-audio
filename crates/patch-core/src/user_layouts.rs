//! Layouts you saved (the bar's Save and Save as…, `omarchy-patch layout
//! save`): one JSON file each in `~/.config/omarchy-patch/layouts/`, named
//! by the layout's id. The built-in layouts are never written: saving one
//! you changed makes a named copy.

use crate::layout::{LayoutRecipe, LayoutStatus, Tile, intern};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct File {
    name: String,
    #[serde(default)]
    summary: String,
    tiles: Vec<TileFile>,
}

#[derive(Serialize, Deserialize)]
struct TileFile {
    ws: u8,
    surface: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    float: bool,
}

pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch/layouts"))
}

/// The id for a name: lower case, words joined by `-` ("My Set 2" → `my-set-2`).
pub fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Every saved layout, by name.
pub fn load() -> Vec<LayoutRecipe> {
    let Some(dir) = dir() else { return vec![] };
    let mut out: Vec<LayoutRecipe> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            if path.extension()? != "json" || slug(&id) != id {
                return None;
            }
            let file: File = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            Some(recipe(&id, file))
        })
        .filter(|r| !r.tiles.is_empty())
        .collect();
    out.sort_by_key(|r| r.name.to_lowercase());
    out
}

fn recipe(id: &str, file: File) -> LayoutRecipe {
    LayoutRecipe {
        id: intern(id),
        name: intern(&file.name),
        summary: intern(&file.summary),
        status: LayoutStatus::Available,
        tiles: file
            .tiles
            .iter()
            .map(|t| Tile {
                ws: t.ws,
                surface: intern(&t.surface),
                x: t.x,
                y: t.y,
                w: t.w,
                h: t.h,
                float: t.float,
            })
            .collect(),
    }
}

/// Write a layout (replacing one with the same id) and return it as loaded.
pub fn save(name: &str, summary: &str, tiles: &[Tile]) -> std::io::Result<LayoutRecipe> {
    let id = slug(name);
    let dir = dir().ok_or_else(|| std::io::Error::other("no home directory"))?;
    if id.is_empty() {
        return Err(std::io::Error::other("the name needs a letter or a digit"));
    }
    std::fs::create_dir_all(&dir)?;
    let file = File {
        name: name.trim().to_string(),
        summary: summary.to_string(),
        tiles: tiles
            .iter()
            .map(|t| TileFile {
                ws: t.ws,
                surface: t.surface.to_string(),
                x: t.x,
                y: t.y,
                w: t.w,
                h: t.h,
                float: t.float,
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)? + "\n";
    // Write next to it, then rename: a crash never leaves half a layout.
    let tmp = dir.join(format!(".{id}.json.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, dir.join(format!("{id}.json")))?;
    Ok(recipe(&id, file))
}

/// Remove a saved layout; `false` if there was none with this id.
pub fn delete(id: &str) -> std::io::Result<bool> {
    let Some(path) = dir().map(|d| d.join(format!("{id}.json"))) else {
        return Ok(false);
    };
    if slug(id) != id || !path.is_file() {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_ids() {
        assert_eq!(slug("Live DJ (mine)"), "live-dj-mine");
        assert_eq!(slug("  My Set 2 "), "my-set-2");
        assert_eq!(slug("!!"), "");
    }
}
