//! An online track that is one finished file (Omarchy Radio's tracks are) is
//! no live stream: fetched, it is a file like any other, and the engine plays
//! it with its waveform, grid, key, sync and keylock. On mpv a deck has none
//! of those, and on a machine whose music is all online no deck ever had a
//! waveform. A live stream has no length, and stays on mpv.
//!
//! mpv starts at once, so a load never waits on the network; the engine takes
//! over when the file is here. Everything runs on its own thread: the windows
//! never touch the network.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const MAX_BYTES: u64 = 512 << 20;
/// Smaller than this is an error page or a stub.
const MIN_BYTES: u64 = 64 << 10;
const CACHE_BYTES: u64 = 4 << 30;
const EXTENSIONS: [&str; 7] = ["mp3", "flac", "m4a", "ogg", "opus", "wav", "aac"];

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"))
        .join("omarchy-patch/remote-tracks")
}

/// FNV-1a: stable across builds, unlike the standard library's hasher.
fn stem(url: &str) -> String {
    let hash = url.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

fn is_online(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// The file of a track already fetched: a few `stat`s, safe on the UI thread.
pub fn cached(url: &str) -> Option<PathBuf> {
    if !is_online(url) {
        return None;
    }
    let (dir, stem) = (cache_dir(), stem(url));
    EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|path| path.is_file())
}

/// From a HEAD reply (redirects included, the last answer counts): the file's
/// extension and size, if it is one finished audio file.
fn finished_file(headers: &str) -> Option<(&'static str, u64)> {
    let last = headers
        .rsplit("HTTP/")
        .find(|block| !block.trim().is_empty())?;
    let (status, rest) = last.split_once('\n')?;
    if status.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    let (mut kind, mut size) = (None, None);
    for line in rest.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        // A header *named* icy-…: a broadcast. CORS lists mention those names
        // in their values on plain files too.
        if name.starts_with("icy-") {
            return None;
        }
        match name.as_str() {
            "content-type" => kind = Some(value.trim().to_ascii_lowercase()),
            "content-length" => size = value.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    let size = size.filter(|s| (MIN_BYTES..=MAX_BYTES).contains(s))?;
    let kind = kind?;
    let ext = match kind.split(';').next()?.trim() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/aac" | "audio/x-aac" => "aac",
        _ => return None,
    };
    Some((ext, size))
}

/// Download the track if it is one finished file. Blocks: a worker's job.
fn fetch(url: &str) -> Option<PathBuf> {
    if !is_online(url) {
        return None;
    }
    if let Some(path) = cached(url) {
        return Some(path);
    }
    let curl = || {
        let mut c = Command::new("curl");
        c.args(["-fsSL", "--proto", "=https,http"]);
        c
    };
    let head = curl().args(["-I", "--max-time", "8", "--url", url]).output().ok()?;
    if !head.status.success() {
        return None;
    }
    let (ext, size) = finished_file(&String::from_utf8_lossy(&head.stdout))?;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let stem = stem(url);
    let part = dir.join(format!("{stem}.{}.part", std::process::id()));
    let got = curl()
        .args(["--max-time", "600", "--max-filesize", &MAX_BYTES.to_string(), "-o"])
        .arg(&part)
        .args(["--url", url])
        .status()
        .is_ok_and(|s| s.success())
        && std::fs::metadata(&part).is_ok_and(|m| m.len() == size);
    let path = dir.join(format!("{stem}.{ext}"));
    if !got || std::fs::rename(&part, &path).is_err() {
        let _ = std::fs::remove_file(&part);
        return None;
    }
    prune(&dir, &path);
    Some(path)
}

/// Oldest first until the folder fits; the track just fetched stays.
fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= CACHE_BYTES {
            return;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// One answer from a deck's mpv.
fn ask_player(sock: &Path, command: Value) -> Option<Value> {
    let mut stream = UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    stream.write_all(format!("{}\n", json!({"command": command})).as_bytes()).ok()?;
    for line in BufReader::new(stream).lines() {
        let reply: Value = serde_json::from_str(&line.ok()?).ok()?;
        // mpv mixes its events into the replies.
        if reply.get("error").is_some() {
            return Some(reply["data"].clone());
        }
    }
    None
}

fn deck_call(deck: &str, action: &str, mut args: Value) -> Result<Value, String> {
    args["op"] = "deck".into();
    args["deck"] = deck.into();
    args["do"] = action.into();
    patch_engine::client::call(&args)
}

/// Fetch `url` and move the deck from mpv to the engine, where it was: paused
/// stays paused, playing plays on. Nothing happens if the deck has moved on.
pub fn take_over_soon(deck: &'static str, sock: PathBuf, url: String) {
    let _ = std::thread::Builder::new()
        .name("patch-remote-track".into())
        .spawn(move || {
            let Some(path) = fetch(&url) else {
                return;
            };
            let still_here = || {
                ask_player(&sock, json!(["get_property", "path"])).is_some_and(|p| p == url.as_str())
            };
            let status = || patch_engine::client::call(&json!({"op": "deck_status", "deck": deck}));
            if !still_here() || status().is_ok_and(|s| s["loaded"] == true) {
                return;
            }
            // The engine ignores the deck's card once its track is loaded:
            // from here on it is what is heard.
            let file = path.to_string_lossy();
            if deck_call(deck, "load", json!({"path": file})).is_err() {
                return;
            }
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(10) {
                if status().is_ok_and(|s| s["decode_done"] == true) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            let at = ask_player(&sock, json!(["get_property", "time-pos"])).and_then(|t| t.as_f64());
            let paused = ask_player(&sock, json!(["get_property", "pause"])).and_then(|p| p.as_bool());
            if let Some(seconds) = at.filter(|t| *t > 0.05) {
                let _ = deck_call(deck, "seek", json!({"seconds": seconds}));
            }
            if paused == Some(false) {
                let _ = deck_call(deck, "play", json!({}));
            }
            let _ = ask_player(&sock, json!(["stop"]));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 5909250\r\n\
        Access-Control-Expose-Headers: Content-Length, Icy-Name, Icy-Br\r\n\r\n";

    #[test]
    fn a_finished_file_is_fetched_even_when_cors_names_icy_headers() {
        assert_eq!(finished_file(FILE), Some(("mp3", 5_909_250)));
    }

    #[test]
    fn the_last_answer_of_a_redirect_counts() {
        let moved = format!("HTTP/1.1 302 Found\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n{FILE}");
        assert_eq!(finished_file(&moved), Some(("mp3", 5_909_250)));
        let to_error = format!("{FILE}HTTP/1.1 404 Not Found\r\nContent-Type: audio/mpeg\r\nContent-Length: 999999\r\n\r\n");
        assert_eq!(finished_file(&to_error), None);
    }

    #[test]
    fn a_broadcast_stays_on_the_player() {
        assert_eq!(finished_file("HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\n\r\n"), None);
        assert_eq!(
            finished_file("HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 999999\r\nicy-name: Radio\r\n\r\n"),
            None
        );
    }

    #[test]
    fn neither_pages_nor_stubs_nor_giants_are_tracks() {
        for headers in [
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 999999\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 100\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 99999999999\r\n\r\n",
        ] {
            assert_eq!(finished_file(headers), None, "{headers}");
        }
    }

    #[test]
    fn the_cache_name_is_stable_and_only_online_urls_have_one() {
        assert_eq!(stem("https://a"), stem("https://a"));
        assert_ne!(stem("https://a"), stem("https://b"));
        assert_eq!(cached("/home/me/track.mp3"), None);
        assert_eq!(fetch("file:///etc/passwd"), None);
    }
}
