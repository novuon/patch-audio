//! Where a track lives, said so a long list stays readable and the file
//! is still findable.
//!
//! A row carries a short place (`Music · House/2024`). The highlighted
//! track carries the file name first, then the directory, so the end of a
//! long path is what remains when the line runs out of room. Home is `~`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackPlace {
    /// Scanning label: `Music · House/2024`, or the source name at its root.
    pub short: String,
    /// `filename  ·  ~/Music/House/2024`. The file leads so an elided line
    /// keeps the name. Deep directories fold the middle.
    pub path_line: String,
}

/// `path` is a file (or a stream URL). `sources` are `(name, root)`.
pub fn track_place(path: &str, sources: &[(&str, &str)], home: &str) -> TrackPlace {
    if path.contains("://") {
        let host = path
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', ':']).next().unwrap_or(rest))
            .unwrap_or(path);
        return TrackPlace {
            short: host.to_string(),
            path_line: path.to_string(),
        };
    }
    let file = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let shown = fold_display(&display_path(&dir, home));
    let path_line = if file.is_empty() {
        shown
    } else if shown.is_empty() || shown == "~" {
        file
    } else {
        format!("{file}  ·  {shown}")
    };
    TrackPlace {
        short: short_place(path, sources, home),
        path_line,
    }
}

/// The highlighted place is this directory (a source, or a folder under one).
pub fn location_label(prefix: &str, sources: &[(&str, &str)], home: &str) -> String {
    let prefix = trim(prefix);
    if prefix.is_empty() {
        return String::new();
    }
    if let Some((name, _)) = sources.iter().find(|(_, root)| trim(root) == prefix) {
        return (*name).to_string();
    }
    if let Some((name, root)) = longest_source(prefix, sources) {
        let rel = Path::new(prefix)
            .strip_prefix(Path::new(trim(root)))
            .ok()
            .map(parts)
            .unwrap_or_default();
        return crumb(name, &rel);
    }
    display_path(prefix, home)
}

/// `"source"` when `location` is a source root, `"folder"` when it is
/// inside one, `""` when nothing is picked.
pub fn location_kind(location: &str, sources: &[(&str, &str)]) -> &'static str {
    let location = trim(location);
    if location.is_empty() {
        ""
    } else if sources.iter().any(|(_, root)| trim(root) == location) {
        "source"
    } else {
        "folder"
    }
}

/// The folder's own name: the source's name at its root, else the last part.
pub fn location_name(location: &str, sources: &[(&str, &str)]) -> String {
    let location = trim(location);
    if let Some((name, _)) = sources.iter().find(|(_, root)| trim(root) == location) {
        return (*name).to_string();
    }
    Path::new(location)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The source whose folders should open under the current place.
pub fn folder_root(location: &str, sources: &[(&str, &str)]) -> String {
    longest_source(location, sources)
        .map(|(_, root)| trim(root).to_string())
        .unwrap_or_default()
}

/// One step up, unless `location` is a source root (All tracks is that step).
/// `(path, label)`.
pub fn parent_location(location: &str, sources: &[(&str, &str)]) -> Option<(String, String)> {
    let location = trim(location);
    if location.is_empty() || sources.iter().any(|(_, root)| trim(root) == location) {
        return None;
    }
    let parent = Path::new(location).parent()?.to_string_lossy().to_string();
    let parent = trim(&parent).to_string();
    if parent.is_empty() {
        return None;
    }
    let label = sources
        .iter()
        .find(|(_, root)| trim(root) == parent)
        .map(|(name, _)| (*name).to_string())
        .or_else(|| {
            Path::new(&parent)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })?;
    Some((parent, label))
}

/// Immediate child folders of `root` that contain tracks. `(path, name, count)`.
pub fn child_folders<'a>(
    paths: impl Iterator<Item = &'a str>,
    root: &str,
) -> Vec<(String, String, usize)> {
    let root = Path::new(trim(root));
    if root.as_os_str().is_empty() {
        return vec![];
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in paths {
        if path.contains("://") {
            continue;
        }
        let Ok(rel) = Path::new(path).strip_prefix(root) else {
            continue;
        };
        let mut comps = rel.components();
        let Some(first) = comps.next() else { continue };
        if comps.next().is_none() {
            continue;
        }
        let name = first.as_os_str().to_string_lossy().into_owned();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        *counts.entry(name).or_default() += 1;
    }
    let mut out: Vec<_> = counts
        .into_iter()
        .map(|(name, count)| {
            (
                root.join(&name).to_string_lossy().into_owned(),
                name,
                count,
            )
        })
        .collect();
    out.sort_by_key(|row| row.1.to_lowercase());
    out
}

/// One folder in the source tree the sidebar draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderNode {
    pub path: String,
    pub label: String,
    pub count: usize,
    /// 0 is a folder directly inside the source.
    pub depth: usize,
    /// Its own folders are listed under it.
    pub expanded: bool,
    /// It has folders under it, so the row can open.
    pub expandable: bool,
}

/// The source's folders, with the path down to `location` opened and the
/// other branches left closed. Siblings of an open folder stay in the list,
/// so collapsing a place is a matter of hiding this branch.
pub fn open_branch(paths: &[&str], source_root: &str, location: &str) -> Vec<FolderNode> {
    let mut out = Vec::new();
    grow(paths, trim(source_root), trim(location), 0, &mut out);
    out
}

fn grow(paths: &[&str], root: &str, location: &str, depth: usize, out: &mut Vec<FolderNode>) {
    if root.is_empty() || depth > 6 {
        return;
    }
    for (path, label, count) in child_folders(paths.iter().copied(), root) {
        let path = trim(&path).to_string();
        let kids = child_folders(paths.iter().copied(), &path);
        let on_path = location == path || location.starts_with(&format!("{path}/"));
        let expandable = !kids.is_empty();
        let expanded = on_path && expandable;
        out.push(FolderNode {
            path: path.clone(),
            label,
            count,
            depth,
            expanded,
            expandable,
        });
        if expanded {
            grow(paths, &path, location, depth + 1, out);
        }
    }
}

/// `path` is the file or a directory at `location`, on a component boundary.
pub fn under(path: &str, location: &str) -> bool {
    let location = trim(location);
    if location.is_empty() {
        return true;
    }
    Path::new(path).starts_with(Path::new(location))
}

/// The folder every file shares, as a short place. Empty when the tracks
/// are spread across sources, or there are none. Streams are ignored.
pub fn shared_place<'a>(
    paths: impl Iterator<Item = &'a str>,
    sources: &[(&str, &str)],
    home: &str,
) -> String {
    let files: Vec<&str> = paths
        .filter(|p| !p.is_empty() && !p.contains("://"))
        .collect();
    if files.is_empty() {
        return String::new();
    }
    let mut common = Path::new(files[0])
        .parent()
        .unwrap_or(Path::new(files[0]))
        .to_path_buf();
    for path in &files[1..] {
        let parent = Path::new(path)
            .parent()
            .unwrap_or(Path::new(path));
        while !parent.starts_with(&common) {
            match common.parent() {
                Some(up) if !up.as_os_str().is_empty() && up != common.as_path() => {
                    common = up.to_path_buf();
                }
                _ => return String::new(),
            }
        }
    }
    let dir = common.to_string_lossy().into_owned();
    let dir = trim(&dir);
    if dir.is_empty() || dir == "/" || (!home.is_empty() && dir == trim(home)) {
        return String::new();
    }
    // A file inside this directory, so the place names the directory itself.
    let probe = PathBuf::from(dir).join("track");
    short_place(&probe.to_string_lossy(), sources, home)
}

fn short_place(path: &str, sources: &[(&str, &str)], home: &str) -> String {
    let parent = Path::new(path).parent().unwrap_or(Path::new(path));
    if let Some((name, root)) = longest_source(path, sources) {
        let rel = parent
            .strip_prefix(Path::new(trim(root)))
            .ok()
            .map(parts)
            .unwrap_or_default();
        return match rel.len() {
            0 => name.to_string(),
            1 => format!("{name} · {}", rel[0]),
            n => {
                let tail = if n == 2 {
                    format!("{}/{}", rel[0], rel[1])
                } else {
                    format!("…/{}/{}", rel[n - 2], rel[n - 1])
                };
                format!("{name} · {tail}")
            }
        };
    }
    let shown = display_path(&parent.to_string_lossy(), home);
    let folders: Vec<&str> = shown
        .split('/')
        .filter(|p| !p.is_empty() && *p != "~")
        .collect();
    match folders.len() {
        0 => shown,
        1 => folders[0].to_string(),
        n => format!("{}/{}", folders[n - 2], folders[n - 1]),
    }
}

fn longest_source<'a>(path: &str, sources: &'a [(&str, &str)]) -> Option<(&'a str, &'a str)> {
    let path = Path::new(path);
    sources
        .iter()
        .filter_map(|(name, root)| {
            let root = trim(root);
            if root.is_empty() {
                return None;
            }
            path.starts_with(Path::new(root)).then_some((*name, root))
        })
        .max_by_key(|(_, root)| root.len())
}

fn crumb(name: &str, parts: &[String]) -> String {
    match parts.len() {
        0 => name.to_string(),
        1..=3 => format!("{name} / {}", parts.join(" / ")),
        n => format!("{name} / … / {}", parts[n - 2..].join(" / ")),
    }
}

fn parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn display_path(path: &str, home: &str) -> String {
    let path = trim(path);
    let home = trim(home);
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".into();
    }
    path.strip_prefix(&format!("{home}/"))
        .map(|rest| format!("~/{rest}"))
        .unwrap_or_else(|| path.to_string())
}

/// Fold the middle of a displayed directory once it is deeper than
/// `~/Music/House/2024/May`. The last two folders stay.
fn fold_display(shown: &str) -> String {
    let abs = shown.starts_with('/');
    let parts: Vec<&str> = shown.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 5 {
        return shown.to_string();
    }
    let head = if abs {
        format!("/{}/{}", parts[0], parts[1])
    } else {
        format!("{}/{}", parts[0], parts[1])
    };
    format!(
        "{head}/…/{}/{}",
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

fn trim(path: &str) -> &str {
    path.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<(&'static str, &'static str)> {
        vec![
            ("Music", "/home/user/Music"),
            ("Stick", "/run/media/randy/STICK"),
        ]
    }

    #[test]
    fn a_track_in_a_source_names_the_source_and_its_folders() {
        let p = track_place(
            "/home/user/Music/House/2024/harbour.flac",
            &sources(),
            "/home/user",
        );
        assert_eq!(p.short, "Music · House/2024");
        assert_eq!(p.path_line, "harbour.flac  ·  ~/Music/House/2024");
    }

    #[test]
    fn a_file_at_the_source_root_is_just_the_source() {
        let p = track_place("/home/user/Music/harbour.flac", &sources(), "/home/user");
        assert_eq!(p.short, "Music");
        assert_eq!(p.path_line, "harbour.flac  ·  ~/Music");
    }

    #[test]
    fn a_deeper_path_keeps_the_file_and_folds_the_middle() {
        let p = track_place(
            "/home/user/Music/House/2024/May/Week/Sets/harbour.flac",
            &sources(),
            "/home/user",
        );
        assert_eq!(p.short, "Music · …/Week/Sets");
        assert_eq!(
            p.path_line,
            "harbour.flac  ·  ~/Music/…/Week/Sets"
        );
    }

    #[test]
    fn the_longest_source_wins_and_a_lookalike_prefix_does_not() {
        let sources = [
            ("Music", "/home/user/Music"),
            ("House", "/home/user/Music/House"),
        ];
        let p = track_place("/home/user/Music/House/2024/a.flac", &sources, "/home/user");
        assert_eq!(p.short, "House · 2024");
        let other = track_place(
            "/home/user/Music-videos/a.flac",
            &sources,
            "/home/user",
        );
        assert_eq!(other.short, "Music-videos");
        assert!(!under("/home/user/Music-videos/a.flac", "/home/user/Music"));
        assert!(under("/home/user/Music/House/a.flac", "/home/user/Music"));
    }

    #[test]
    fn a_stick_and_a_stream_say_where_they_are() {
        let stick = track_place(
            "/run/media/randy/STICK/Techno/a.mp3",
            &sources(),
            "/home/user",
        );
        assert_eq!(stick.short, "Stick · Techno");
        assert_eq!(stick.path_line, "a.mp3  ·  /run/media/randy/STICK/Techno");
        let radio = track_place("https://ice.somafm.com/groovesalad", &sources(), "/home/user");
        assert_eq!(radio.short, "ice.somafm.com");
        assert_eq!(radio.path_line, "https://ice.somafm.com/groovesalad");
    }

    #[test]
    fn folders_under_a_place_are_the_next_level_only() {
        let paths = [
            "/home/user/Music/House/2024/a.flac",
            "/home/user/Music/House/2024/b.flac",
            "/home/user/Music/House/2023/c.flac",
            "/home/user/Music/Techno/d.flac",
            "/home/user/Music/loose.flac",
            "/run/media/randy/STICK/Techno/e.mp3",
            "https://example.com/live",
        ];
        let folders = child_folders(paths.into_iter(), "/home/user/Music");
        assert_eq!(
            folders,
            vec![
                ("/home/user/Music/House".into(), "House".into(), 3),
                ("/home/user/Music/Techno".into(), "Techno".into(), 1),
            ]
        );
        let deeper = child_folders(paths.into_iter(), "/home/user/Music/House");
        assert_eq!(deeper.len(), 2);
        assert_eq!(deeper[0].1, "2023");
        assert_eq!(deeper[1].1, "2024");
        assert_eq!(deeper[1].2, 2);
    }

    #[test]
    fn the_open_branch_keeps_siblings_and_expands_only_the_path() {
        let paths = [
            "/home/user/Music/House/2024/a.flac",
            "/home/user/Music/House/2024/b.flac",
            "/home/user/Music/House/2023/c.flac",
            "/home/user/Music/Techno/d.flac",
            "/home/user/Music/loose.flac",
        ];
        let top = open_branch(&paths, "/home/user/Music", "/home/user/Music");
        assert_eq!(top.len(), 2);
        assert!(top.iter().all(|n| n.depth == 0 && !n.expanded));
        assert_eq!(top[0].label, "House");
        assert!(top[0].expandable);
        assert_eq!(top[1].label, "Techno");
        assert!(!top[1].expandable);

        let branch = open_branch(&paths, "/home/user/Music", "/home/user/Music/House/2024");
        let shown: Vec<_> = branch
            .iter()
            .map(|n| (n.label.as_str(), n.depth, n.expanded, n.expandable))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("House", 0, true, true),
                ("2023", 1, false, false),
                ("2024", 1, false, false),
                ("Techno", 0, false, false),
            ]
        );
    }

    #[test]
    fn browsing_a_place_names_it_and_knows_the_step_up() {
        let s = sources();
        assert_eq!(location_kind("/home/user/Music", &s), "source");
        assert_eq!(
            location_kind("/home/user/Music/House/2024", &s),
            "folder"
        );
        assert_eq!(location_kind("", &s), "");
        assert_eq!(location_label("/home/user/Music", &s, "/home/user"), "Music");
        assert_eq!(
            location_label("/home/user/Music/House/2024", &s, "/home/user"),
            "Music / House / 2024"
        );
        assert_eq!(parent_location("/home/user/Music", &s), None);
        assert_eq!(
            parent_location("/home/user/Music/House", &s),
            Some(("/home/user/Music".into(), "Music".into()))
        );
        assert_eq!(
            parent_location("/home/user/Music/House/2024", &s),
            Some(("/home/user/Music/House".into(), "House".into()))
        );
        assert_eq!(folder_root("/home/user/Music/House/2024", &s), "/home/user/Music");
        assert_eq!(location_name("/home/user/Music", &s), "Music");
        assert_eq!(location_name("/home/user/Music/House/2024", &s), "2024");
    }

    #[test]
    fn a_collection_shows_the_folder_its_tracks_share() {
        let s = sources();
        let house = [
            "/home/user/Music/House/2024/a.flac",
            "/home/user/Music/House/2023/b.flac",
        ];
        assert_eq!(
            shared_place(house.into_iter(), &s, "/home/user"),
            "Music · House"
        );
        let one = ["/home/user/Music/House/2024/a.flac"];
        assert_eq!(
            shared_place(one.into_iter(), &s, "/home/user"),
            "Music · House/2024"
        );
        let mixed = [
            "/home/user/Music/House/a.flac",
            "/run/media/randy/STICK/Techno/b.mp3",
        ];
        assert_eq!(shared_place(mixed.into_iter(), &s, "/home/user"), "");
        let streams = ["https://example.com/a"];
        assert_eq!(shared_place(streams.into_iter(), &s, "/home/user"), "");
    }

    #[test]
    fn a_very_deep_crumb_keeps_the_source_and_the_last_folders() {
        assert_eq!(
            location_label(
                "/home/user/Music/House/2024/May/Week",
                &sources(),
                "/home/user"
            ),
            "Music / … / May / Week"
        );
    }
}
