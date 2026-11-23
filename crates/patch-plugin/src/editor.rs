//! A plugin's own editor in a process of its own (`omarchy-patch plugins
//! editor`), started by the Patch window that opens it.
//!
//! Patch's windows draw on one busy UI thread; an editor serviced from
//! there got its mouse events and timers only as often as that thread came
//! round (every 2nd or 3rd display frame on a full layout, never while it
//! loaded something), and a plugin whose editor crashed took the windows
//! with it. Here the editor has a thread to itself: its X11 events and
//! run-loop timers are served every 2 ms.
//!
//! The window is a proper X11 top level (class `omarchy.patch.plugin`, a
//! title, its pid, a fixed size the plugin may change, and the close
//! protocol), so Hyprland floats it at the editor's size and closing it
//! closes the editor cleanly.
//!
//! The protocol is one JSON line each way. In: the [`Request`], then
//! `{"set":[id,value]}` for each change made in Patch's own face. Out:
//! `{"open":[w,h]}` once shown, `{"edit":[id,value]}` for each change made
//! in the editor. The process ends when the window is closed or stdin is.

use crate::{RATE, SavedParameter};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The class Hyprland's rules match.
pub const CLASS: &str = "omarchy.patch.plugin";
/// How often the editor's events and timers are served while it has the
/// keys (the pointer over it gives it them: focus follows the mouse), and
/// otherwise (its own meters and animation still run at 100 per second).
const SERVE: Duration = Duration::from_millis(2);
const IDLE: Duration = Duration::from_millis(10);
/// While served at full pace: how often to ask the X server whether it
/// still has the keys.
const RECHECK: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub plugin: PathBuf,
    pub parameters: Vec<SavedParameter>,
    /// The screen's scale (X11 editors under Hyprland get physical pixels).
    pub scale: f32,
    /// The size asked for on top of the screen's scale (1, 1.5, 2).
    #[serde(default = "unzoomed")]
    pub zoom: f32,
    pub title: String,
}

fn unzoomed() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum In {
    Set(u32, f64),
    Zoom(f32),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Out {
    Open(i32, i32),
    Edit(u32, f64),
    /// The zoom shown (less than asked when that would not fit the screen)
    /// and whether the plugin scales at all.
    Zoom(f32, bool),
    /// The plugin host cannot show it (not running there, no editor, no
    /// display): Patch opens it in an editor process instead.
    Refused(String),
}

/// The plugin host's socket, where Patch opens the editor of a plugin it
/// runs (`$XDG_RUNTIME_DIR/omarchy-patch/plugin-editors.sock`).
pub fn socket_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("omarchy-patch/plugin-editors.sock"))
}

/// The first line on the socket: which slot's editor, and how to show it.
#[derive(Serialize, Deserialize)]
pub(crate) struct HostOpen {
    pub slot: u32,
    pub request: Request,
}

// ---------------------------------------------------------------------------
// Patch's side.

static COMMAND: OnceLock<Vec<OsString>> = OnceLock::new();

/// The command that runs an editor (tests run one from their own binary).
/// Default: this executable's `plugins editor`.
pub fn set_command(command: Vec<OsString>) {
    let _ = COMMAND.set(command);
}

fn command() -> Vec<OsString> {
    COMMAND.get().cloned().unwrap_or_else(|| {
        let exe = std::env::current_exe().map(PathBuf::into_os_string).unwrap_or_else(|_| "omarchy-patch".into());
        vec![exe, "plugins".into(), "editor".into()]
    })
}

/// What an editor process reported since the last look.
#[derive(Default)]
pub struct News {
    pub edits: Vec<SavedParameter>,
    /// The window is up (its size), once.
    pub opened: Option<(i32, i32)>,
    /// The zoom it shows and whether the plugin scales, after each change.
    pub zoom: Option<(f32, bool)>,
    /// The editor is gone (closed, or its process ended).
    pub closed: bool,
    /// The plugin host could not show it, and why: open it in a process.
    pub refused: Option<String>,
}

/// A plugin's editor: in its own process, or shown by the plugin host on
/// the instance it runs (live meters), over its socket.
pub struct Editor {
    child: Option<Child>,
    socket: Option<UnixStream>,
    stdin: Option<Box<dyn Write + Send>>,
    rx: mpsc::Receiver<Out>,
    closed: bool,
}

/// Read an editor's lines (process or socket) into `tx` until it ends.
fn read_editor(from: impl std::io::Read + Send + 'static, tx: mpsc::Sender<Out>) -> Result<()> {
    std::thread::Builder::new()
        .name("patch-editor-in".into())
        .spawn(move || {
            // Lines that are not ours (a test harness's) are skipped.
            for line in std::io::BufReader::new(from).lines() {
                let Ok(line) = line else { break };
                let json = line.find('{').map_or("", |at| &line[at..]);
                if let Ok(out) = serde_json::from_str::<Out>(json)
                    && tx.send(out).is_err()
                {
                    break;
                }
            }
        })
        .context("read the editor")?;
    Ok(())
}

impl Editor {
    pub fn spawn(request: &Request) -> Result<Self> {
        let command = command();
        let (program, args) = command.split_first().context("no editor command")?;
        // X windows are not scaled here: the pointer over the editor is
        // drawn at the screen's scale, the size it has everywhere else.
        let cursor = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(24.0);
        let mut child = Command::new(program)
            .args(args)
            .env("XCURSOR_SIZE", ((cursor * request.scale.max(1.0)).round() as u32).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start {}", program.to_string_lossy()))?;
        let mut stdin = child.stdin.take().context("editor stdin")?;
        let stdout = child.stdout.take().context("editor stdout")?;
        writeln!(stdin, "{}", serde_json::to_string(request)?)?;
        stdin.flush()?;
        let (tx, rx) = mpsc::channel();
        read_editor(stdout, tx)?;
        Ok(Self {
            child: Some(child),
            socket: None,
            stdin: Some(Box::new(stdin)),
            rx,
            closed: false,
        })
    }

    /// Ask the plugin host to show `slot`'s editor on the instance it runs.
    /// It answers like an editor process (or [`News::refused`]).
    pub fn connect(slot: u32, request: &Request) -> Result<Self> {
        Self::connect_at(&socket_path().context("no runtime folder")?, slot, request)
    }

    /// [`connect`](Self::connect) to the host serving editors at `path`.
    pub fn connect_at(path: &std::path::Path, slot: u32, request: &Request) -> Result<Self> {
        let socket = UnixStream::connect(path).with_context(|| format!("connect to {}", path.display()))?;
        let mut writer = socket.try_clone()?;
        writeln!(writer, "{}", serde_json::to_string(&HostOpen { slot, request: request.clone() })?)?;
        writer.flush()?;
        let (tx, rx) = mpsc::channel();
        read_editor(socket.try_clone()?, tx)?;
        Ok(Self {
            child: None,
            socket: Some(socket),
            stdin: Some(Box::new(writer)),
            rx,
            closed: false,
        })
    }

    /// A change made in Patch's face: the editor follows.
    pub fn set(&mut self, id: u32, value: f64) {
        let Some(stdin) = &mut self.stdin else { return };
        let Ok(line) = serde_json::to_string(&In::Set(id, value)) else { return };
        if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
            self.stdin = None;
        }
    }

    /// Another size for the open editor (1, 1.5, 2): it changes in place.
    pub fn zoom(&mut self, zoom: f32) {
        let Some(stdin) = &mut self.stdin else { return };
        let Ok(line) = serde_json::to_string(&In::Zoom(zoom)) else { return };
        if writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
            self.stdin = None;
        }
    }

    /// Edits made in the editor, and whether it is still there.
    pub fn poll(&mut self) -> News {
        let mut news = News::default();
        loop {
            match self.rx.try_recv() {
                Ok(Out::Edit(id, value)) => news.edits.push(SavedParameter { id, value }),
                Ok(Out::Open(w, h)) => news.opened = Some((w, h)),
                Ok(Out::Zoom(z, scales)) => news.zoom = Some((z, scales)),
                Ok(Out::Refused(why)) => news.refused = Some(why),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        // An editor that ended closed its output: the channel says so once
        // everything it said is read (no process check every frame).
        news.closed = self.closed;
        news
    }

    pub fn is_open(&self) -> bool {
        !self.closed
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
}

impl Drop for Editor {
    /// Close it: stdin ends and the editor leaves on its own; one that does
    /// not within a second is killed (off this thread: never wait on it).
    fn drop(&mut self) {
        self.stdin = None;
        // Shown by the plugin host: it closes the window when the socket does.
        if let Some(socket) = self.socket.take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        let Some(mut child) = self.child.take() else { return };
        let _ = std::thread::Builder::new().name("patch-editor-end".into()).spawn(move || {
            for _ in 0..100 {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

// ---------------------------------------------------------------------------
// The editor process.

xcb::atoms_struct! {
    struct Atoms {
        wm_protocols => b"WM_PROTOCOLS",
        wm_delete_window => b"WM_DELETE_WINDOW",
        net_wm_name => b"_NET_WM_NAME",
        net_wm_pid => b"_NET_WM_PID",
        utf8_string => b"UTF8_STRING",
    }
}

struct Window {
    conn: xcb::Connection,
    window: xcb::x::Window,
    atoms: Atoms,
    escape: Option<xcb::x::Keycode>,
    /// The screen's size, which a zoomed editor must fit.
    screen: (i32, i32),
    /// It (or one of the plugin's windows inside it) has the keyboard:
    /// served at full pace. Not the pointer: XWayland keeps the pointer
    /// where it last saw it, so an editor that opens there is entered with
    /// the real pointer elsewhere.
    focused: std::cell::Cell<bool>,
    /// The desktop's arrow, also given to the plugin's own top-level
    /// windows (a debug or settings window), which often define none: the
    /// pointer vanishes over them under XWayland.
    cursor: xcb::x::Cursor,
    root: xcb::x::Window,
    /// The bits of a resource id that are the client's: windows the same
    /// X connection made share the rest.
    client: u32,
}

const ESCAPE: u32 = 0xff1b;

/// The keycode that types `keysym` unshifted.
fn keycode(conn: &xcb::Connection, setup: &xcb::x::Setup, keysym: u32) -> Option<xcb::x::Keycode> {
    let (first, last) = (setup.min_keycode(), setup.max_keycode());
    let map = conn
        .wait_for_reply(conn.send_request(&xcb::x::GetKeyboardMapping { first_keycode: first, count: last - first + 1 }))
        .ok()?;
    let per = map.keysyms_per_keycode().max(1) as usize;
    let at = map.keysyms().chunks(per).position(|syms| syms.first() == Some(&keysym))?;
    Some(first + at as u8)
}

/// The desktop's arrow pointer, from its cursor theme (libxcb-cursor, the
/// way Qt and GTK load it); the X server's own arrow if that is missing.
fn cursor(conn: &xcb::Connection, screen: &xcb::x::Screen) -> xcb::x::Cursor {
    type New = unsafe extern "C" fn(*mut xcb::ffi::xcb_connection_t, *const u8, *mut *mut libc::c_void) -> i32;
    type Load = unsafe extern "C" fn(*mut libc::c_void, *const libc::c_char) -> u32;
    type Free = unsafe extern "C" fn(*mut libc::c_void);
    // SAFETY: libxcb-cursor's C API as declared in xcb/xcb_cursor.h; the
    // screen is the setup's own wire data (an xcb_screen_t); the context
    // lives only here and the cursor it made outlives it.
    let themed = unsafe {
        let lib = libc::dlopen(c"libxcb-cursor.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            0
        } else {
            let new = libc::dlsym(lib, c"xcb_cursor_context_new".as_ptr());
            let load = libc::dlsym(lib, c"xcb_cursor_load_cursor".as_ptr());
            let free = libc::dlsym(lib, c"xcb_cursor_context_free".as_ptr());
            let mut id = 0;
            if !new.is_null() && !load.is_null() && !free.is_null() {
                let new = std::mem::transmute::<*mut libc::c_void, New>(new);
                let load = std::mem::transmute::<*mut libc::c_void, Load>(load);
                let free = std::mem::transmute::<*mut libc::c_void, Free>(free);
                let mut ctx = std::ptr::null_mut();
                if new(conn.get_raw_conn(), (screen as *const xcb::x::Screen).cast(), &mut ctx) == 0 {
                    id = load(ctx, c"left_ptr".as_ptr());
                    free(ctx);
                }
            }
            id
        }
    };
    if themed != 0 {
        return xcb::XidNew::new(themed);
    }
    // The cursor font's left_ptr (glyph 68, its mask 69).
    let font: xcb::x::Font = conn.generate_id();
    let cursor: xcb::x::Cursor = conn.generate_id();
    conn.send_request(&xcb::x::OpenFont { fid: font, name: b"cursor" });
    conn.send_request(&xcb::x::CreateGlyphCursor {
        cid: cursor,
        source_font: font,
        mask_font: font,
        source_char: 68,
        mask_char: 69,
        fore_red: 0,
        fore_green: 0,
        fore_blue: 0,
        back_red: 0xffff,
        back_green: 0xffff,
        back_blue: 0xffff,
    });
    conn.send_request(&xcb::x::CloseFont { font });
    cursor
}

impl Window {
    fn new(title: &str) -> Result<Self> {
        let (conn, screen_number) = xcb::Connection::connect(None).context("connect to the X server")?;
        let setup = conn.get_setup();
        let screen = setup.roots().nth(screen_number as usize).context("no X11 screen")?;
        let cursor = cursor(&conn, screen);
        let client = !setup.resource_id_mask();
        let screen = screen.to_owned();
        let screen_size = (i32::from(screen.width_in_pixels()), i32::from(screen.height_in_pixels()));
        let atoms = Atoms::intern_all(&conn).context("X11 atoms")?;
        let window = conn.generate_id();
        conn.send_and_check_request(&xcb::x::CreateWindow {
            depth: xcb::x::COPY_FROM_PARENT as u8,
            wid: window,
            parent: screen.root(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            border_width: 0,
            class: xcb::x::WindowClass::InputOutput,
            visual: screen.root_visual(),
            value_list: &[
                xcb::x::Cw::BackPixel(screen.black_pixel()),
                xcb::x::Cw::EventMask(
                    xcb::x::EventMask::STRUCTURE_NOTIFY
                        | xcb::x::EventMask::KEY_PRESS
                        | xcb::x::EventMask::FOCUS_CHANGE,
                ),
                // The plugin's own windows inherit it unless they set their
                // own; without one the pointer vanishes over the editor.
                xcb::x::Cw::Cursor(cursor),
            ],
        })
        .context("create the editor window")?;
        // Esc closes the editor wherever its focus is inside the window.
        let escape = keycode(&conn, setup, ESCAPE);
        if let Some(key) = escape {
            conn.send_request(&xcb::x::GrabKey {
                owner_events: true,
                grab_window: window,
                modifiers: xcb::x::ModMask::ANY,
                key,
                pointer_mode: xcb::x::GrabMode::Async,
                keyboard_mode: xcb::x::GrabMode::Async,
            });
        }
        let set = |property, r#type, data: &[u8]| {
            conn.send_request(&xcb::x::ChangeProperty {
                mode: xcb::x::PropMode::Replace,
                window,
                property,
                r#type,
                data,
            });
        };
        set(xcb::x::ATOM_WM_NAME, xcb::x::ATOM_STRING, title.as_bytes());
        set(atoms.net_wm_name, atoms.utf8_string, title.as_bytes());
        let class = format!("{CLASS}\0{CLASS}\0");
        set(xcb::x::ATOM_WM_CLASS, xcb::x::ATOM_STRING, class.as_bytes());
        conn.send_request(&xcb::x::ChangeProperty {
            mode: xcb::x::PropMode::Replace,
            window,
            property: atoms.wm_protocols,
            r#type: xcb::x::ATOM_ATOM,
            data: &[atoms.wm_delete_window],
        });
        conn.send_request(&xcb::x::ChangeProperty {
            mode: xcb::x::PropMode::Replace,
            window,
            property: atoms.net_wm_pid,
            r#type: xcb::x::ATOM_CARDINAL,
            data: &[std::process::id()],
        });
        // Top-level windows mapped from now on (the plugin's own among
        // them), to give the pointer to.
        conn.send_request(&xcb::x::ChangeWindowAttributes {
            window: screen.root(),
            value_list: &[xcb::x::Cw::EventMask(xcb::x::EventMask::SUBSTRUCTURE_NOTIFY)],
        });
        conn.flush()?;
        Ok(Self {
            conn,
            window,
            atoms,
            escape,
            screen: screen_size,
            focused: std::cell::Cell::new(false),
            cursor,
            root: screen.root(),
            client,
        })
    }

    fn id(&self) -> u32 {
        use xcb::Xid;
        self.window.resource_id()
    }

    /// The editor's size, fixed: the window manager keeps it (the plugin
    /// may ask for another, which comes here again).
    fn size(&self, width: i32, height: i32) -> Result<()> {
        let (w, h) = (width.max(1) as u32, height.max(1) as u32);
        // WM_NORMAL_HINTS: flags (PMinSize | PMaxSize), 4 pads, min, max,
        // then the rest unused (18 words in all).
        let mut hints = [0u32; 18];
        hints[0] = (1 << 4) | (1 << 5);
        hints[5] = w;
        hints[6] = h;
        hints[7] = w;
        hints[8] = h;
        self.conn.send_request(&xcb::x::ChangeProperty {
            mode: xcb::x::PropMode::Replace,
            window: self.window,
            property: xcb::x::ATOM_WM_NORMAL_HINTS,
            r#type: xcb::x::ATOM_WM_SIZE_HINTS,
            data: &hints,
        });
        self.conn.send_request(&xcb::x::ConfigureWindow {
            window: self.window,
            value_list: &[xcb::x::ConfigWindow::Width(w), xcb::x::ConfigWindow::Height(h)],
        });
        self.conn.flush()?;
        Ok(())
    }

    fn show(&self) -> Result<()> {
        self.conn.send_request(&xcb::x::MapWindow { window: self.window });
        self.conn.flush()?;
        Ok(())
    }

    /// Focus as the X server has it now, in case a focus-out went missing.
    fn recheck(&self) {
        use xcb::Xid;
        let Ok(focus) = self.conn.wait_for_reply(self.conn.send_request(&xcb::x::GetInputFocus {})) else {
            return;
        };
        // Up from the focused window to this one; None and PointerRoot are
        // not windows, and plugins nest a few deep.
        let mut window = focus.focus();
        for _ in 0..8 {
            if window == self.window {
                return;
            }
            if window.resource_id() <= 1 {
                break;
            }
            match self.conn.wait_for_reply(self.conn.send_request(&xcb::x::QueryTree { window })) {
                Ok(tree) if tree.parent() != tree.root() => window = tree.parent(),
                _ => break,
            }
        }
        self.focused.set(false);
    }

    /// A top-level window just mapped: if the plugin made it (the same X
    /// client as a window inside the editor), it shows the arrow.
    fn lend_cursor(&self, top: xcb::x::Window) {
        use xcb::Xid;
        let base = |w: xcb::x::Window| w.resource_id() & self.client;
        let Ok(tree) = self.conn.wait_for_reply(self.conn.send_request(&xcb::x::QueryTree { window: self.window })) else {
            return;
        };
        if tree.children().iter().any(|&c| base(c) == base(top)) {
            self.conn.send_request(&xcb::x::ChangeWindowAttributes {
                window: top,
                value_list: &[xcb::x::Cw::Cursor(self.cursor)],
            });
            let _ = self.conn.flush();
        }
    }

    /// Serve the window's own events: false once it should close.
    fn events(&self) -> bool {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(xcb::Event::X(xcb::x::Event::ClientMessage(m)))) => {
                    if let xcb::x::ClientMessageData::Data32([atom, ..]) = m.data()
                        && atom == xcb::Xid::resource_id(&self.atoms.wm_delete_window)
                    {
                        return false;
                    }
                }
                Ok(Some(xcb::Event::X(xcb::x::Event::DestroyNotify(e)))) if e.window() == self.window => return false,
                Ok(Some(xcb::Event::X(xcb::x::Event::MapNotify(e)))) if e.event() == self.root && e.window() != self.window => {
                    self.lend_cursor(e.window());
                }
                Ok(Some(xcb::Event::X(xcb::x::Event::FocusIn(_)))) => self.focused.set(true),
                Ok(Some(xcb::Event::X(xcb::x::Event::FocusOut(e)))) => {
                    if e.detail() != xcb::x::NotifyDetail::Inferior {
                        self.focused.set(false);
                    }
                }
                Ok(Some(xcb::Event::X(xcb::x::Event::KeyPress(k)))) if Some(k.detail()) == self.escape => return false,
                Ok(Some(_)) => {}
                Ok(None) => return true,
                // The X server is gone.
                Err(xcb::Error::Connection(_)) => return false,
                Err(_) => {}
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.conn.send_request(&xcb::x::DestroyWindow { window: self.window });
        let _ = self.conn.flush();
    }
}

fn scaled((w, h): (i32, i32), by: f32) -> (i32, i32) {
    ((w as f32 * by).round() as i32, (h as f32 * by).round() as i32)
}

/// The largest scale up to `want` at which an editor `unit` big at scale 1
/// fits the screen (never below the screen's own scale, `base`).
fn fit(want: f32, base: f32, unit: (i32, i32), screen: (i32, i32)) -> f32 {
    // Room for the bar and the window's border.
    let room = (screen.0 as f32 * 0.96, screen.1 as f32 * 0.92);
    let (w, h) = scaled(unit, want);
    if w as f32 <= room.0 && h as f32 <= room.1 {
        return want;
    }
    (want * (room.0 / w as f32).min(room.1 / h as f32)).max(base).min(want)
}

/// What an editor window serves: the editor process's own instance of the
/// plugin, or the plugin host's running one ([`vst3_host::EditorAccess`]:
/// the instance that processes the audio, so its meters move).
pub(crate) trait Target {
    fn has_editor(&self) -> bool;
    fn open(&mut self, window: u32) -> Result<()>;
    fn close(&mut self);
    fn service(&mut self);
    fn attached_size(&self) -> Option<(i32, i32)>;
    fn can_resize(&self) -> Option<bool>;
    fn resize(&mut self, width: i32, height: i32) -> Option<(i32, i32)>;
    fn set_scale(&mut self, factor: f32) -> bool;
    fn take_resize(&self) -> Option<(i32, i32)>;
    /// Edits made in the editor since the last look.
    fn changes(&mut self) -> Vec<(u32, f64)>;
    /// A change made in Patch's own face, for the editor to show.
    fn follow(&mut self, id: u32, value: f64);
}

impl Target for vst3_host::Plugin {
    fn has_editor(&self) -> bool {
        vst3_host::Plugin::has_editor(self)
    }
    fn open(&mut self, window: u32) -> Result<()> {
        self.open_editor(vst3_host::WindowHandle::from_x11(window)).context("open the plugin's editor")
    }
    fn close(&mut self) {
        let _ = self.close_editor();
    }
    fn service(&mut self) {
        self.service_run_loop();
    }
    fn attached_size(&self) -> Option<(i32, i32)> {
        self.attached_editor_size().ok().flatten()
    }
    fn can_resize(&self) -> Option<bool> {
        self.attached_editor_can_resize()
    }
    fn resize(&mut self, width: i32, height: i32) -> Option<(i32, i32)> {
        self.resize_editor(width, height).ok()
    }
    fn set_scale(&mut self, factor: f32) -> bool {
        self.set_editor_scale_factor(factor).unwrap_or(false)
    }
    fn take_resize(&self) -> Option<(i32, i32)> {
        self.take_editor_resize_request()
    }
    fn changes(&mut self) -> Vec<(u32, f64)> {
        self.get_parameter_changes()
    }
    fn follow(&mut self, id: u32, value: f64) {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            let _ = self.set_parameter(id, value);
            // Patch's own change: not an edit to send back.
            let _ = self.get_parameter_changes();
        }
    }
}

impl Target for vst3_host::EditorAccess {
    fn has_editor(&self) -> bool {
        vst3_host::EditorAccess::has_editor(self)
    }
    fn open(&mut self, window: u32) -> Result<()> {
        vst3_host::EditorAccess::open(self, vst3_host::WindowHandle::from_x11(window)).context("open the plugin's editor")
    }
    fn close(&mut self) {
        let _ = vst3_host::EditorAccess::close(self);
    }
    fn service(&mut self) {
        self.service_run_loop();
    }
    fn attached_size(&self) -> Option<(i32, i32)> {
        vst3_host::EditorAccess::attached_size(self)
    }
    fn can_resize(&self) -> Option<bool> {
        vst3_host::EditorAccess::can_resize(self)
    }
    fn resize(&mut self, width: i32, height: i32) -> Option<(i32, i32)> {
        vst3_host::EditorAccess::resize(self, width, height).ok()
    }
    fn set_scale(&mut self, factor: f32) -> bool {
        self.set_scale_factor(factor).unwrap_or(false)
    }
    fn take_resize(&self) -> Option<(i32, i32)> {
        self.take_resize_request()
    }
    fn changes(&mut self) -> Vec<(u32, f64)> {
        self.parameter_changes()
    }
    // The engine gives the processor the value; draining the changes
    // brings the controller, and so the editor, up to it.
    fn follow(&mut self, _id: u32, _value: f64) {}
}

/// Zoom a plugin that sizes but does not scale: its own size at open
/// (`natural`) times `zoom`, fitted to the screen. The size it took and
/// the zoom that is.
fn resize_zoom(plugin: &mut impl Target, natural: (i32, i32), zoom: f32, screen: (i32, i32)) -> Option<((i32, i32), f32)> {
    let zoom = fit(zoom.clamp(1.0, 3.0), 1.0, natural, screen);
    let (w, h) = plugin.resize(scaled(natural, zoom).0, scaled(natural, zoom).1)?;
    Some(((w, h), w as f32 / natural.0.max(1) as f32))
}

fn say(out: &Out) -> bool {
    let Ok(line) = serde_json::to_string(out) else { return true };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}").and_then(|()| stdout.flush()).is_ok()
}

/// One open editor window: its plugin, its size and zoom.
pub(crate) struct Session<T: Target> {
    target: T,
    window: Window,
    base: f32,
    scale: f32,
    size: (i32, i32),
    natural: (i32, i32),
    scales: bool,
    resizes: bool,
    shown: f32,
    checked: Instant,
}

impl<T: Target> Session<T> {
    /// Open `target`'s editor in a window of its own, at the request's
    /// scale and zoom. What to tell Patch first comes back with it; on
    /// failure, the target with why.
    pub(crate) fn open(mut target: T, request: &Request) -> std::result::Result<(Self, Vec<Out>), (T, anyhow::Error)> {
        if !target.has_editor() {
            return Err((target, anyhow::anyhow!("{} has no editor of its own", request.title)));
        }
        // Before the editor attaches: it opens at the screen's scale and
        // zoom, where it supports one (JUCE, VSTGUI); others keep their size.
        let base = request.scale.max(0.5);
        let mut scale = base * request.zoom.clamp(1.0, 3.0);
        if scale > 1.01 {
            target.set_scale(scale);
        }
        let window = match Window::new(&request.title) {
            Ok(window) => window,
            Err(e) => return Err((target, e)),
        };
        if let Err(e) = target.open(window.id()) {
            return Err((target, e));
        }
        let mut size = target.attached_size().unwrap_or((640, 480));
        // Asked again with the view attached: whether it scales at all.
        let scales = target.set_scale(scale);
        // One that does not scale may still let the host size it (Dplug's,
        // Graillon's): zoom is then its own size times the zoom.
        let natural = size;
        let resizes = !scales && target.can_resize() == Some(true);
        let mut shown = 1.0;
        if scales {
            let fitted = fit(scale, base, scaled(size, 1.0 / scale), window.screen);
            if fitted < scale {
                target.set_scale(fitted);
                size = target.take_resize().or_else(|| target.attached_size()).unwrap_or(size);
                scale = fitted;
            }
            shown = scale / base;
        } else if resizes && request.zoom > 1.01 {
            (size, shown) = resize_zoom(&mut target, natural, request.zoom, window.screen).unwrap_or((size, 1.0));
        }
        let shown_ok = window.size(size.0, size.1).and_then(|()| window.show());
        if let Err(e) = shown_ok {
            target.close();
            return Err((target, e));
        }
        // Restoring and opening are not edits.
        let _ = target.changes();
        let first = vec![Out::Open(size.0, size.1), Out::Zoom(shown, scales || resizes)];
        let session = Self { target, window, base, scale, size, natural, scales, resizes, shown, checked: Instant::now() };
        Ok((session, first))
    }

    /// Serve it once: its window's events, Patch's messages, the plugin's
    /// timers, a size it asks for, its edits (into `out`). False once its
    /// window is closed.
    pub(crate) fn step(&mut self, input: impl IntoIterator<Item = In>, out: &mut Vec<Out>) -> bool {
        if !self.window.events() {
            return false;
        }
        for message in input {
            match message {
                In::Set(id, value) => self.target.follow(id, value),
                In::Zoom(zoom) => {
                    if self.scales {
                        let want = fit(self.base * zoom.clamp(1.0, 3.0), self.base, scaled(self.size, 1.0 / self.scale), self.window.screen);
                        if (want - self.scale).abs() > 0.001 {
                            self.target.set_scale(want);
                            self.scale = want;
                        }
                        self.shown = self.scale / self.base;
                    } else if self.resizes
                        && let Some((resized, z)) = resize_zoom(&mut self.target, self.natural, zoom, self.window.screen)
                    {
                        self.size = resized;
                        self.shown = z;
                        let _ = self.window.size(resized.0, resized.1);
                    }
                    out.push(Out::Zoom(self.shown, self.scales || self.resizes));
                }
            }
        }
        self.target.service();
        if let Some((w, h)) = self.target.take_resize() {
            self.size = (w, h);
            let _ = self.window.size(w, h);
        }
        out.extend(self.target.changes().into_iter().map(|(id, value)| Out::Edit(id, value)));
        if self.window.focused.get() && self.checked.elapsed() >= RECHECK {
            self.window.recheck();
            self.checked = Instant::now();
        }
        true
    }

    /// It has the keys: served at full pace.
    pub(crate) fn focused(&self) -> bool {
        self.window.focused.get()
    }

    /// How long to wait before serving it again.
    pub(crate) fn pace(&self) -> Duration {
        if self.focused() { SERVE } else { IDLE }
    }

    /// Close the editor, then its window; the plugin comes back.
    pub(crate) fn close(mut self) -> T {
        self.target.close();
        drop(self.window);
        self.target
    }
}

/// Run one plugin's editor until its window or stdin closes.
/// `omarchy-patch plugins editor`.
pub fn serve() -> Result<()> {
    let mut lines = std::io::BufReader::new(std::io::stdin()).lines();
    let request: Request = serde_json::from_str(&lines.next().context("no editor request")??)
        .context("read the editor request")?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("patch-editor-in".into())
        .spawn(move || {
            for line in lines {
                let Ok(line) = line else { break };
                if let Ok(message) = serde_json::from_str::<In>(&line)
                    && tx.send(message).is_err()
                {
                    break;
                }
            }
        })?;

    let mut plugin = crate::host(RATE, 1024)?
        .load_plugin(&request.plugin)
        .with_context(|| format!("load {}", request.plugin.display()))?;
    for p in &request.parameters {
        if p.value.is_finite() && (0.0..=1.0).contains(&p.value) {
            let _ = plugin.set_parameter(p.id, p.value);
        }
    }
    let (mut session, first) = Session::open(plugin, &request).map_err(|(_, e)| e)?;
    for out in &first {
        say(out);
    }
    let mut out = Vec::new();
    loop {
        let mut input = Vec::new();
        let mut gone = false;
        loop {
            match rx.try_recv() {
                Ok(message) => input.push(message),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        if gone || !session.step(input, &mut out) {
            break;
        }
        if !out.drain(..).all(|o| say(&o)) {
            break;
        }
        std::thread::sleep(session.pace());
    }
    drop(session.close());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb::Xid;

    #[test]
    fn a_zoom_that_would_not_fit_the_screen_shrinks_to_fit() {
        // Box Of Bass: 1003×666 at scale 1, on a 3840×2160 screen at 1.6.
        let (unit, screen) = ((1003, 666), (3840, 2160));
        assert_eq!(fit(1.6, 1.6, unit, screen), 1.6);
        assert_eq!(fit(2.4, 1.6, unit, screen), 2.4, "1.5× fits");
        let two = fit(3.2, 1.6, unit, screen);
        let (w, h) = scaled(unit, two);
        assert!(two < 3.2 && two > 2.8, "2× fitted: {two}");
        assert!(w <= 3686 && h <= 1988, "{w}×{h} on the screen");
        // Never below the screen's own scale, even when that is too big.
        assert_eq!(fit(1.6, 1.6, (4000, 3000), screen), 1.6);
    }

    /// Not a test: this binary is the tests' editor process.
    #[test]
    fn plugin_editor_entry_for_tests() {
        if std::env::var_os("PATCH_TEST_EDITOR").is_some() {
            serve().unwrap();
            std::process::exit(0);
        }
    }

    fn property(conn: &xcb::Connection, window: xcb::x::Window, property: xcb::x::Atom) -> Vec<u8> {
        conn.wait_for_reply(conn.send_request(&xcb::x::GetProperty {
            delete: false,
            window,
            property,
            r#type: xcb::x::ATOM_ANY,
            long_offset: 0,
            long_length: 1024,
        }))
        .map(|r| match r.format() {
            32 => r.value::<u32>().iter().flat_map(|v| v.to_ne_bytes()).collect(),
            8 => r.value::<u8>().to_vec(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
    }

    /// The top-level window an editor process shows.
    fn window_of(conn: &xcb::Connection, root: xcb::x::Window, pid: u32) -> Option<xcb::x::Window> {
        let pid_atom = conn
            .wait_for_reply(conn.send_request(&xcb::x::InternAtom { only_if_exists: true, name: b"_NET_WM_PID" }))
            .ok()?
            .atom();
        let tree = conn.wait_for_reply(conn.send_request(&xcb::x::QueryTree { window: root })).ok()?;
        tree.children().iter().copied().find(|w| {
            let v = property(conn, *w, pid_atom);
            v.len() == 4 && u32::from_ne_bytes([v[0], v[1], v[2], v[3]]) == pid
        })
    }

    // Core event codes for XTest.
    const MOTION: u8 = 6;
    const PRESS: u8 = 4;
    const RELEASE: u8 = 5;
    const KEY_PRESS: u8 = 2;
    const KEY_RELEASE: u8 = 3;

    fn fake(conn: &xcb::Connection, root: xcb::x::Window, kind: u8, detail: u8, x: i16, y: i16) {
        conn.send_request(&xcb::xtest::FakeInput {
            r#type: kind,
            detail,
            time: xcb::x::CURRENT_TIME,
            root,
            root_x: x,
            root_y: y,
            deviceid: 0,
        });
        conn.flush().unwrap();
    }

    /// Opens Box Of Bass's own editor on this display, drags across it as
    /// a hand would (XTest: the X server's pointer, not the desktop's),
    /// checks the pointer shows over it, closes it
    /// with Esc, then the way the window manager does. Run by hand:
    /// `cargo test -p patch-plugin --release editor_ -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens a window on the desktop"]
    fn editor_answers_the_mouse_and_closes_like_a_window() {
        let path = crate::bundle_path(&PathBuf::from(std::env::var_os("HOME").unwrap_or_default()));
        if crate::scan::binary(&path).is_none() || std::env::var_os("DISPLAY").is_none() {
            eprintln!("no Box Of Bass or no X display; skipping");
            return;
        }
        // SAFETY: before the editor process starts; this test alone sets it.
        unsafe { std::env::set_var("PATCH_TEST_EDITOR", "1") };
        set_command(vec![
            std::env::current_exe().unwrap().into_os_string(),
            "plugin_editor_entry_for_tests".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]);
        let open = || {
            let mut editor = Editor::spawn(&Request {
                plugin: path.clone(),
                parameters: Vec::new(),
                scale: 1.6,
                zoom: 1.0,
                title: "Box Of Bass — Original".into(),
            })
            .unwrap();
            let t0 = Instant::now();
            let size = loop {
                let news = editor.poll();
                assert!(!news.closed, "the editor closed by itself");
                if let Some(size) = news.opened {
                    break size;
                }
                assert!(t0.elapsed() < Duration::from_secs(10), "the editor never opened");
                std::thread::sleep(Duration::from_millis(20));
            };
            eprintln!("opened {}x{} in {:?}", size.0, size.1, t0.elapsed());
            (editor, size)
        };
        let closes = |editor: &mut Editor, how: &str| {
            let t1 = Instant::now();
            while !editor.poll().closed {
                assert!(t1.elapsed() < Duration::from_secs(3), "{how} did not close the editor");
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!("{how} closed it in {:?}", t1.elapsed());
        };
        let (mut editor, size) = open();
        assert!(size.0 > 800, "not drawn at the screen's scale: {size:?}");

        let (conn, screen) = xcb::Connection::connect(None).unwrap();
        let root = conn.get_setup().roots().nth(screen as usize).unwrap().root();
        std::thread::sleep(Duration::from_millis(500));
        let window = window_of(&conn, root, editor.pid().unwrap()).expect("no window with the editor's pid");
        assert_eq!(property(&conn, window, xcb::x::ATOM_WM_CLASS), format!("{CLASS}\0{CLASS}\0").into_bytes());
        let geometry = conn
            .wait_for_reply(conn.send_request(&xcb::x::GetGeometry { drawable: xcb::x::Drawable::Window(window) }))
            .unwrap();
        // Hyprland rounds through its logical size at a fractional scale.
        let shown = (i32::from(geometry.width()), i32::from(geometry.height()));
        assert!((shown.0 - size.0).abs() <= 2 && (shown.1 - size.1).abs() <= 2, "shown at {shown:?}, not {size:?}");
        let at = conn
            .wait_for_reply(conn.send_request(&xcb::x::TranslateCoordinates {
                src_window: window,
                dst_window: root,
                src_x: 0,
                src_y: 0,
            }))
            .unwrap();
        let (ox, oy) = (at.dst_x(), at.dst_y());

        // Drag up from a grid of points until one moves a control; time how
        // long the edit takes to come back.
        let mut answered = None;
        'grid: for gy in 1..6 {
            for gx in 1..8 {
                let x = ox + (size.0 * gx / 8) as i16;
                let y = oy + (size.1 * gy / 6) as i16;
                fake(&conn, root, MOTION, 0, x, y);
                std::thread::sleep(Duration::from_millis(30));
                fake(&conn, root, PRESS, 1, x, y);
                let mut first = None;
                for step in 1..=12 {
                    std::thread::sleep(Duration::from_millis(8));
                    let sent = Instant::now();
                    fake(&conn, root, MOTION, 0, x, y - step * 4);
                    let deadline = sent + Duration::from_millis(150);
                    while Instant::now() < deadline {
                        if !editor.poll().edits.is_empty() {
                            first.get_or_insert(sent.elapsed());
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                fake(&conn, root, RELEASE, 1, x, y - 48);
                std::thread::sleep(Duration::from_millis(50));
                if let Some(latency) = first {
                    eprintln!("a drag at ({gx}/8, {gy}/6) moved a control; first edit back in {latency:?}");
                    answered = Some(latency);
                    break 'grid;
                }
            }
        }
        let latency = answered.expect("no drag moved any control");
        assert!(latency < Duration::from_millis(40), "slow to answer: {latency:?}");

        // The pointer stays visible over the editor (a window without a
        // cursor of its own shows none under XWayland).
        conn.wait_for_reply(conn.send_request(&xcb::xfixes::QueryVersion { client_major_version: 4, client_minor_version: 0 }))
            .unwrap();
        for (x, y) in [(ox + 4, oy + 4), (ox + (size.0 / 2) as i16, oy + (size.1 / 2) as i16)] {
            fake(&conn, root, MOTION, 0, x, y);
            std::thread::sleep(Duration::from_millis(100));
            let image = conn.wait_for_reply(conn.send_request(&xcb::xfixes::GetCursorImage {})).unwrap();
            let seen = image.cursor_image().iter().filter(|p| *p >> 24 != 0).count();
            eprintln!("pointer at ({x}, {y}): {}x{}, {seen} pixels seen", image.width(), image.height());
            assert!(seen > 20, "the pointer is invisible over the editor");
        }

        // Esc closes it.
        let escape = keycode(&conn, conn.get_setup(), ESCAPE).unwrap();
        conn.send_request(&xcb::x::SetInputFocus {
            revert_to: xcb::x::InputFocus::PointerRoot,
            focus: window,
            time: xcb::x::CURRENT_TIME,
        });
        fake(&conn, root, KEY_PRESS, escape, 0, 0);
        fake(&conn, root, KEY_RELEASE, escape, 0, 0);
        closes(&mut editor, "Esc");

        let (mut editor, unzoomed) = open();
        std::thread::sleep(Duration::from_millis(500));
        let window = window_of(&conn, root, editor.pid().unwrap()).expect("no window with the editor's pid");
        let shown = |conn: &xcb::Connection| {
            let g = conn
                .wait_for_reply(conn.send_request(&xcb::x::GetGeometry { drawable: xcb::x::Drawable::Window(window) }))
                .unwrap();
            (i32::from(g.width()), i32::from(g.height()))
        };
        // Zoom in place: the plugin redraws larger and the window follows;
        // 2× would pass the screen's height, so it is fitted.
        for asked in [1.5f32, 2.0] {
            editor.zoom(asked);
            let t = Instant::now();
            let got = loop {
                if let Some((z, scales)) = editor.poll().zoom {
                    assert!(scales, "Box Of Bass scales its editor");
                    break z;
                }
                assert!(t.elapsed() < Duration::from_secs(3), "no answer to the zoom");
                std::thread::sleep(Duration::from_millis(10));
            };
            std::thread::sleep(Duration::from_millis(400));
            let size = shown(&conn);
            eprintln!("zoom {asked}: shown at {got:.2}×, {}×{}", size.0, size.1);
            let ratio = size.0 as f32 / unzoomed.0 as f32;
            assert!((ratio - got).abs() < 0.05, "window {size:?} not at {got}× of {unzoomed:?}");
            let screen = conn.get_setup().roots().next().unwrap().height_in_pixels();
            assert!(size.1 <= i32::from(screen), "taller than the screen: {size:?}");
        }

        // Closed the way Hyprland closes a window: WM_DELETE_WINDOW.
        let atom = |name: &[u8]| {
            conn.wait_for_reply(conn.send_request(&xcb::x::InternAtom { only_if_exists: false, name }))
                .unwrap()
                .atom()
        };
        conn.send_request(&xcb::x::SendEvent {
            propagate: false,
            destination: xcb::x::SendEventDest::Window(window),
            event_mask: xcb::x::EventMask::NO_EVENT,
            event: &xcb::x::ClientMessageEvent::new(
                window,
                atom(b"WM_PROTOCOLS"),
                xcb::x::ClientMessageData::Data32([atom(b"WM_DELETE_WINDOW").resource_id(), 0, 0, 0, 0]),
            ),
        });
        conn.flush().unwrap();
        closes(&mut editor, "WM_DELETE_WINDOW");
    }

    /// Graillon (Dplug) does not scale its editor but lets the host size
    /// it: zoom grows the window by its own size. Run by hand:
    /// `cargo test -p patch-plugin --release graillon_ -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens a window on the desktop"]
    fn graillon_zooms_by_its_size() {
        let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".vst3/Auburn Sounds Graillon 3.vst3");
        if crate::scan::binary(&path).is_none() || std::env::var_os("DISPLAY").is_none() {
            eprintln!("no Graillon or no X display; skipping");
            return;
        }
        // SAFETY: before the editor process starts; this test alone sets it.
        unsafe { std::env::set_var("PATCH_TEST_EDITOR", "1") };
        set_command(vec![
            std::env::current_exe().unwrap().into_os_string(),
            "plugin_editor_entry_for_tests".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ]);
        let mut editor = Editor::spawn(&Request {
            plugin: path,
            parameters: Vec::new(),
            scale: 1.6,
            zoom: 1.0,
            title: "Graillon 3 — Original".into(),
        })
        .unwrap();
        let t0 = Instant::now();
        let (natural, scales) = loop {
            let news = editor.poll();
            assert!(!news.closed, "the editor closed by itself");
            if let (Some(size), Some((_, scales))) = (news.opened, news.zoom) {
                break (size, scales);
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "the editor never opened");
            std::thread::sleep(Duration::from_millis(20));
        };
        eprintln!("opened {}x{}, zooms: {scales}", natural.0, natural.1);
        assert!(scales, "Graillon's editor can be sized");
        let (conn, _) = xcb::Connection::connect(None).unwrap();
        let root = conn.get_setup().roots().next().unwrap().root();
        std::thread::sleep(Duration::from_millis(300));
        let window = window_of(&conn, root, editor.pid().unwrap()).expect("no window with the editor's pid");
        for asked in [1.5f32, 2.0, 1.0] {
            editor.zoom(asked);
            let t = Instant::now();
            let got = loop {
                if let Some((z, _)) = editor.poll().zoom {
                    break z;
                }
                assert!(t.elapsed() < Duration::from_secs(3), "no answer to the zoom");
                std::thread::sleep(Duration::from_millis(10));
            };
            std::thread::sleep(Duration::from_millis(400));
            let g = conn
                .wait_for_reply(conn.send_request(&xcb::x::GetGeometry { drawable: xcb::x::Drawable::Window(window) }))
                .unwrap();
            let size = (i32::from(g.width()), i32::from(g.height()));
            eprintln!("zoom {asked}: {got:.2}×, {}×{}", size.0, size.1);
            let ratio = size.0 as f32 / natural.0 as f32;
            assert!((ratio - got).abs() < 0.05 && got > asked * 0.8, "window {size:?} at {ratio:.2}, said {got}");
        }
        drop(editor);
    }
}
