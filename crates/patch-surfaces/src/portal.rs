//! The desktop's own folder chooser, through the XDG desktop portal: the
//! dialog the user set as their default (on Omarchy e.g. flea, or GTK's),
//! themed like the rest of the desktop. No helper program of our own.

use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// Ask for a folder. Blocks until the user picks one (Some) or cancels
/// (None); Err when there is no portal to ask.
pub fn pick_folder(title: &str) -> Result<Option<std::path::PathBuf>, String> {
    Ok(pick(title, true, false, Some(crate::dj::music_dir()), &[])?.into_iter().next())
}

pub fn pick_file(title: &str) -> Result<Option<std::path::PathBuf>, String> {
    Ok(pick(title, false, false, Some(crate::dj::music_dir()), &[])?.into_iter().next())
}

/// Ask for any number of files (none: cancelled).
pub fn pick_files(title: &str) -> Result<Vec<std::path::PathBuf>, String> {
    pick(title, false, true, Some(crate::dj::music_dir()), &[])
}

/// A file, starting in `folder`, offered by filters: (label, ["*.zip", …]),
/// the first one chosen.
pub fn pick_file_in(
    title: &str,
    folder: std::path::PathBuf,
    filters: &[(&str, &[&str])],
) -> Result<Option<std::path::PathBuf>, String> {
    Ok(pick(title, false, false, Some(folder), filters)?.into_iter().next())
}

fn pick(
    title: &str,
    directory: bool,
    multiple: bool,
    folder: Option<std::path::PathBuf>,
    filters: &[(&str, &[&str])],
) -> Result<Vec<std::path::PathBuf>, String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| format!("no session bus: {e}"))?;
    // The request's object path is known up front: listen before asking,
    // so a fast answer isn't missed.
    let token = format!("patch{}", std::process::id());
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request = format!("{PATH}/request/{sender}/{token}");
    let req = zbus::blocking::Proxy::new(
        &conn,
        DEST,
        request.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| e.to_string())?;
    let mut answers = req.receive_signal("Response").map_err(|e| e.to_string())?;

    let chooser =
        zbus::blocking::Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.FileChooser")
            .map_err(|e| e.to_string())?;
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("directory", Value::from(directory));
    options.insert("multiple", Value::from(multiple));
    options.insert("modal", Value::from(true));
    if let Some(folder) = folder.filter(|p| p.is_dir()) {
        // A NUL-terminated byte string, as the portal wants it.
        let mut bytes = folder.as_os_str().as_encoded_bytes().to_vec();
        bytes.push(0);
        options.insert("current_folder", Value::from(bytes));
    }
    // a(sa(us)): each filter a label and its glob patterns (kind 0).
    let filters: Vec<(&str, Vec<(u32, &str)>)> = filters
        .iter()
        .map(|(label, globs)| (*label, globs.iter().map(|g| (0, *g)).collect()))
        .collect();
    if let Some(first) = filters.first() {
        options.insert("current_filter", Value::from(first.clone()));
        options.insert("filters", Value::from(filters));
    }
    chooser
        .call_method("OpenFile", &("", title, options))
        .map_err(|e| format!("no folder chooser (desktop portal): {e}"))?;

    let Some(msg) = answers.next() else {
        return Ok(vec![]);
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        msg.body().deserialize().map_err(|e| e.to_string())?;
    if code != 0 {
        return Ok(vec![]); // cancelled
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .unwrap_or_default();
    Ok(uris.iter().filter_map(|u| path_of_uri(u)).collect())
}

/// `file:///home/me/My%20Music` → /home/me/My Music.
fn path_of_uri(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_become_paths() {
        assert_eq!(
            path_of_uri("file:///home/me/My%20Music").unwrap(),
            std::path::Path::new("/home/me/My Music")
        );
        assert_eq!(
            path_of_uri("file:///run/media/me/DJ%20STICK/Caf%C3%A9").unwrap(),
            std::path::Path::new("/run/media/me/DJ STICK/Café")
        );
        assert_eq!(
            path_of_uri("file:///tmp/100%").unwrap(),
            std::path::Path::new("/tmp/100%")
        );
        assert!(path_of_uri("https://example.com").is_none());
    }
}
