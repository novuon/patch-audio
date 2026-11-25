//! Session control protocol.
//!
//! A running Unit session listens on a Unix socket and accepts one request per
//! line. This is what the command line, an Omarchy bar item or a Hyprland
//! keybinding (`omarchy-patch controller`) talk to. The future API and a
//! detached engine can speak the same protocol.

use crate::AppId;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Ping,
    Status,
    Controller,
    Open(String),
    Close(String),
    Toggle(String),
    Layout(String),
    /// The layout on screen: go to its first workspace, put its windows
    /// back as saved, close its windows, save it (as a new layout when a
    /// name is given), or delete a saved one.
    LayoutShow,
    LayoutReset,
    LayoutClose,
    LayoutSave(Option<String>),
    LayoutDelete(String),
    /// A Patch command as JSON: `cmd cli {"op":"router.route",…}`.
    /// See [`crate::command`].
    Command {
        origin: crate::command::Origin,
        json: String,
    },
    /// Read-only state for other frontends: `query ai` (see
    /// [`crate::intent::AiView`]).
    Query(String),
    /// The layout on screen and each open window's title, for the desk
    /// these windows are inside (`docs/desk.md`; the generic kit's).
    Desk,
    Quit,
}

impl Request {
    pub fn parse(line: &str) -> Option<Request> {
        let mut parts = line.trim().splitn(2, ' ');
        let verb = parts.next()?;
        let arg = parts.next().map(|s| s.trim().to_string());
        Some(match (verb, arg) {
            ("ping", None) => Request::Ping,
            ("status", None) => Request::Status,
            ("controller", None) => Request::Controller,
            ("open", Some(a)) => Request::Open(a),
            ("close", Some(a)) => Request::Close(a),
            ("toggle", Some(a)) => Request::Toggle(a),
            ("layout", Some(a)) => Request::Layout(a),
            ("layout-show", None) => Request::LayoutShow,
            ("layout-reset", None) => Request::LayoutReset,
            ("layout-close", None) => Request::LayoutClose,
            ("layout-save", a) => Request::LayoutSave(a.filter(|a| !a.is_empty())),
            ("layout-delete", Some(a)) => Request::LayoutDelete(a),
            ("cmd", Some(a)) => {
                let (origin, json) = a.split_once(' ')?;
                let origin = match origin {
                    "user" => crate::command::Origin::User,
                    "cli" => crate::command::Origin::Cli,
                    "ai" => crate::command::Origin::Ai,
                    _ => return None,
                };
                Request::Command {
                    origin,
                    json: json.trim().to_string(),
                }
            }
            ("query", Some(a)) => Request::Query(a),
            ("desk", None) => Request::Desk,
            ("quit", None) => Request::Quit,
            _ => return None,
        })
    }

    pub fn encode(&self) -> String {
        match self {
            Request::Ping => "ping".into(),
            Request::Status => "status".into(),
            Request::Controller => "controller".into(),
            Request::Open(s) => format!("open {s}"),
            Request::Close(s) => format!("close {s}"),
            Request::Toggle(s) => format!("toggle {s}"),
            Request::Layout(s) => format!("layout {s}"),
            Request::LayoutShow => "layout-show".into(),
            Request::LayoutReset => "layout-reset".into(),
            Request::LayoutClose => "layout-close".into(),
            Request::LayoutSave(None) => "layout-save".into(),
            Request::LayoutSave(Some(name)) => format!("layout-save {name}"),
            Request::LayoutDelete(id) => format!("layout-delete {id}"),
            Request::Command { origin, json } => format!(
                "cmd {} {json}",
                match origin {
                    crate::command::Origin::User => "user",
                    crate::command::Origin::Cli => "cli",
                    crate::command::Origin::Ai => "ai",
                }
            ),
            Request::Query(q) => format!("query {q}"),
            Request::Desk => "desk".into(),
            Request::Quit => "quit".into(),
        }
    }
}

/// Reply to `status`: `ok open=graph,mixer layout=studio edited=1`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub open: Vec<String>,
    /// The layout on screen (none once its windows are closed).
    pub layout: Option<String>,
    /// Its windows no longer sit as the layout has them.
    pub edited: bool,
    /// The UI's heap (MB): in use, and freed but still held by the
    /// allocator. Informational: `heap=120/300`.
    pub heap: Option<(u64, u64)>,
}

impl Status {
    pub fn encode(&self) -> String {
        let mut line = format!(
            "ok open={} layout={} edited={}",
            self.open.join(","),
            self.layout.as_deref().unwrap_or("-"),
            u8::from(self.edited)
        );
        if let Some((used, free)) = self.heap {
            line.push_str(&format!(" heap={used}/{free}"));
        }
        line
    }

    pub fn parse(line: &str) -> Option<Status> {
        let rest = line.trim().strip_prefix("ok")?;
        let mut status = Status::default();
        for field in rest.split_whitespace() {
            if let Some(v) = field.strip_prefix("open=") {
                status.open = v
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            } else if let Some(v) = field.strip_prefix("layout=") {
                status.layout = (v != "-").then(|| v.to_string());
            } else if let Some(v) = field.strip_prefix("edited=") {
                status.edited = v == "1";
            }
        }
        Some(status)
    }
}

/// Where Patch keeps its sockets and live state: `$XDG_RUNTIME_DIR/
/// omarchy-patch`, or without one `/tmp/omarchy-patch-<uid>` (a shared,
/// predictable `/tmp/omarchy-patch` could be made first by another user).
/// Made private (0700) the first time; one that belongs to someone else
/// is not used.
pub fn runtime_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let uid = std::fs::metadata("/proc/self")
            .map(|m| m.uid())
            .unwrap_or(0);
        let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(base) => PathBuf::from(base).join("omarchy-patch"),
            None => std::env::temp_dir().join(format!("omarchy-patch-{uid}")),
        };
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() && m.uid() == uid => {
                if m.mode() & 0o077 != 0 {
                    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                }
                dir
            }
            // Someone else's (or not a directory): a fresh private one.
            _ => {
                let own = std::env::temp_dir()
                    .join(format!("omarchy-patch-{uid}-{}", std::process::id()));
                let _ = std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&own);
                own
            }
        }
    })
    .clone()
}

/// While it lives, `<runtime>/<name>.json` holds `{"at": <unix seconds>}`,
/// rewritten once a second; it is removed when the guard drops. The bar's
/// icon reads it (a file, nothing spawned) to show that Patch records or
/// broadcasts with every window closed; a file older than a few seconds is
/// a process that died without cleaning up.
pub struct Heartbeat {
    path: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn heartbeat(name: &str) -> Heartbeat {
    use std::sync::atomic::{AtomicBool, Ordering};
    let path = runtime_dir().join(format!("{name}.json"));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let (file, flag) = (path.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name(format!("patch-{name}-beat"))
        .spawn(move || {
            let tmp = file.with_extension("json.tmp");
            while !flag.load(Ordering::Relaxed) {
                let at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                if std::fs::write(&tmp, format!("{{\"at\":{at}}}\n")).is_ok() {
                    let _ = std::fs::rename(&tmp, &file);
                }
                std::thread::park_timeout(std::time::Duration::from_secs(1));
            }
        })
        .ok();
    Heartbeat { path, stop, thread }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A socket just bound: for its owner only. Same-uid peers are the trust
/// boundary; the engine records `SO_PEERCRED` and does not require a
/// second credential.
pub fn private_socket(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

pub fn socket_path(unit: AppId) -> PathBuf {
    runtime_dir().join(format!("{}.sock", unit.slug()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let cmd = crate::command::Command::DeckTransport {
            deck: crate::command::Deck::A,
            action: crate::command::Transport::Cue,
        };
        let c = Request::Command {
            origin: crate::command::Origin::Ai,
            json: cmd.to_json(),
        };
        for r in [
            Request::Ping,
            Request::Open("graph".into()),
            Request::Layout("studio".into()),
            Request::LayoutSave(None),
            Request::LayoutSave(Some("Live DJ (mine)".into())),
            Request::LayoutDelete("my-set".into()),
            c,
        ] {
            assert_eq!(Request::parse(&r.encode()), Some(r));
        }
        let s = Status {
            open: vec!["graph".into(), "mixer".into()],
            layout: Some("studio".into()),
            edited: true,
            ..Default::default()
        };
        assert_eq!(Status::parse(&s.encode()), Some(s));
        assert_eq!(Status::parse("ok open= layout=-"), Some(Status::default()));
    }
}
