//! The desk (generic kit only): a layout's Units in one window, on desktops
//! that don't tile them (GNOME, KDE Plasma, Cinnamon, any X11 desktop).
//!
//! A small Wayland compositor (Smithay) whose output is one ordinary window
//! on the user's desktop, Wayland or X11. Patch's UI session runs inside it
//! (`WAYLAND_DISPLAY` set to the desk's socket), so every Unit is still its
//! own unchanged Wayland window: input, cursors, popups and text work as
//! anywhere. The desk only places them, with the geometry the Mac desk
//! shares (`patch_core::desk`), and draws the header and the dividers.
//! See `docs/desk.md`.
//!
//! An idle desk does no work: it draws when a Unit commits a frame or the
//! pointer moves over its own parts, and sends frame callbacks at the
//! monitor's rate at most.

mod header;

use header::{Action, Header, Model};
use patch_core::desk::{Desk as Geometry, Rect};
use patch_core::ipc::Request;
use smithay::{
    backend::{
        allocator::{Fourcc, dmabuf::Dmabuf},
        egl::EGLDevice,
        input::{
            AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent,
            PointerAxisEvent, PointerButtonEvent,
        },
        renderer::{
            ExportMem, ImportDma, ImportMemWl,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::WaylandSurfaceRenderElement,
                utils::CropRenderElement,
            },
            gles::GlesRenderer,
        },
        winit::{self as backend_winit, WinitEvent, WinitGraphicsBackend},
    },
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{PopupKind, PopupManager, Space, Window, WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, Keysym, XkbConfig},
        pointer::{AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, MotionEvent},
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction,
            channel::{self, Sender},
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        wayland_protocols::xdg::{decoration::zv1::server::zxdg_toplevel_decoration_v1, shell::server::xdg_toplevel},
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
        winit::{
            dpi::LogicalSize,
            platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11},
            window::{Theme as HostTheme, Window as HostWindow},
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Scale, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale},
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                request_data_device_client_selection, set_data_device_focus, set_data_device_selection,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
        tablet_manager::TabletSeatHandler,
        viewporter::ViewporterState,
    },
};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Space between Units, and around them (logical px): a divider sits in it.
const GAP: f32 = 8.0;

/// A layout opens in a desk here (`patch_host::session::desk_wanted`).
/// Without EGL (`libEGL.so.1`) the desk can't draw: each Unit its own
/// window then (Smithay would abort the process).
pub fn wanted() -> bool {
    patch_host::session::desk_wanted() && egl_loads()
}

fn egl_loads() -> bool {
    // SAFETY: dlopen and dlclose of a system library by name.
    unsafe {
        let h = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if h.is_null() {
            eprintln!("patch: no libEGL.so.1, so no desk: each Unit opens as its own window");
            return false;
        }
        libc::dlclose(h);
        true
    }
}

fn lock_path() -> std::path::PathBuf {
    patch_core::ipc::runtime_dir().join("desk.lock")
}

fn try_lock() -> Option<std::fs::File> {
    let path = lock_path();
    let _ = std::fs::create_dir_all(path.parent()?);
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    // SAFETY: flock on a file this process owns.
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(f)
}

/// A desk is open (its lock is held): the running session is inside it.
pub fn running() -> bool {
    try_lock().is_none() && lock_path().exists()
}

fn send(req: Request) -> Option<String> {
    patch_host::ipc::send(crate::APP, &req)
}

/// Open `layout` in the desk: in the one already open, or a new desk.
pub fn open(layout: &str) -> anyhow::Result<()> {
    if !egl_loads() {
        anyhow::bail!("the desk needs libEGL.so.1 (libegl1, mesa-libEGL or libEGL1)");
    }
    let Some(lock) = try_lock() else {
        // A desk runs: its session takes the layout.
        return match send(Request::Layout(layout.to_string())).as_deref() {
            Some("ok") => Ok(()),
            _ => anyhow::bail!("Patch's desk did not open layout '{layout}'"),
        };
    };
    // A session outside a desk (Units opened on their own) closes first:
    // the desk's own session takes its place.
    if patch_host::ipc::is_running(crate::APP) {
        send(Request::Quit);
        let until = Instant::now() + Duration::from_secs(4);
        while patch_host::ipc::is_running(crate::APP) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let exe = std::env::current_exe()?;
    // The engine starts out here, with the desktop's own environment (it
    // outlives the desk).
    if let Err(e) = patch_engine::client::ensure_running(&exe) {
        eprintln!("desk: engine: {e}");
    }
    run(layout, &exe, lock)
}

/// What the session inside said about its windows (`Request::Desk`).
#[derive(serde::Deserialize, Default)]
struct Report {
    layout: Option<String>,
    windows: Vec<(String, String)>,
}

enum Msg {
    Report(Option<Report>),
    SessionEnded,
    /// The desktop's clipboard text, read when the desk got focus.
    HostClipboard(Option<String>),
    /// Text a Unit inside copied.
    DeskClipboard(String),
}

/// The desktop's clipboard, on its own thread (arboard: Wayland's
/// data-control where the desktop has it, else X11, which GNOME's
/// XWayland keeps in step with its Wayland clipboard).
enum Clip {
    Set(String),
    Get,
    /// A Unit pastes the desktop's clipboard: its text into this pipe.
    Paste(OwnedFd),
}

/// Text mime types, best first, as Wayland and X11 apps offer them.
const TEXT: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

fn clipboard_thread(tx: Sender<Msg>) -> std::sync::mpsc::Sender<Clip> {
    let (ctx, crx) = std::sync::mpsc::channel::<Clip>();
    let _ = std::thread::Builder::new().name("desk-clipboard".into()).spawn(move || {
        let mut board = arboard::Clipboard::new().ok();
        for cmd in crx {
            let Some(b) = board.as_mut() else {
                // No clipboard reachable: a paste gets nothing (the pipe closes).
                continue;
            };
            match cmd {
                Clip::Set(text) => {
                    let _ = b.set_text(text);
                }
                Clip::Get => {
                    let _ = tx.send(Msg::HostClipboard(b.get_text().ok()));
                }
                Clip::Paste(fd) => {
                    use std::io::Write;
                    let text = b.get_text().unwrap_or_default();
                    let _ = std::fs::File::from(fd).write_all(text.as_bytes());
                }
            }
        }
    });
    ctx
}

/// A layout on the desk.
struct Laid {
    id: String,
    name: String,
    geometry: Geometry,
    /// As the layout has it, for Reset and a divider's double-click.
    original: Geometry,
    pages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Hover {
    None,
    Header(Action),
    Divider(Vec<bool>, bool),
    Client,
}

struct Unit {
    window: Window,
    /// The Unit's surface id (`mixer`, `meters#2`), once the session said.
    surface: Option<String>,
    /// Where it sits now, when tiled (logical px).
    tile: Option<Rect>,
}

pub struct State {
    dh: DisplayHandle,
    handle: LoopHandle<'static, State>,
    signal: LoopSignal,
    backend: WinitGraphicsBackend<GlesRenderer>,
    damage: OutputDamageTracker,
    output: Output,
    space: Space<Window>,
    popups: PopupManager,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<State>,
    data_device_state: DataDeviceState,
    dmabuf_state: DmabufState,
    _dmabuf_global: DmabufGlobal,
    _output_manager: OutputManagerState,
    _xdg_decoration: XdgDecorationState,
    _fractional_scale: FractionalScaleManagerState,
    _viewporter: ViewporterState,
    _cursor_shape: CursorShapeManagerState,
    seat: Seat<State>,
    start: Instant,

    units: Vec<Unit>,
    laid: Option<Laid>,
    page: usize,
    zoom: Option<&'static str>,
    edited: bool,
    titles: HashMap<String, String>,
    /// The layout's windows aren't all there yet: the page shows once they
    /// are (or after a moment), as Hyprland's curtain hides the opening.
    opening_until: Option<Instant>,

    size: Size<i32, Logical>,
    scale: f64,
    frame_interval: Duration,
    palette: patch_ui::Palette,
    theme: Option<std::time::SystemTime>,
    header: Header,
    header_model: Option<(Model, Option<Action>, i32, u64)>,
    header_buffer: Option<(MemoryRenderBuffer, (u32, u32))>,
    dividers: Vec<(Rect, Vec<bool>, bool)>,
    divider_buffers: Vec<SolidColorBuffer>,

    pointer: Point<f64, Logical>,
    hover: Hover,
    drag: Option<(Vec<bool>, bool)>,
    last_press: Option<(Instant, Vec<bool>)>,
    cursor: CursorImageStatus,
    host_cursor: Option<CursorIcon>,
    focus_before_blur: Option<WlSurface>,
    /// Windows that had the keyboard, latest last (a closed one gives it back).
    focus_history: Vec<WlSurface>,

    redraw_due: bool,
    frame_timer: bool,
    last_frame: Instant,
    sync_due: bool,
    sync_tries: u8,
    tx: Sender<Msg>,
    session: u32,
    clipboard: std::sync::mpsc::Sender<Clip>,
    /// The text last carried across, either way (no echo back).
    clip_last: Option<String>,
    /// Tests: the desk saves what it shows here once the layout is open,
    /// then closes (`PATCH_DESK_SHOT=<file.png>`).
    shot: Option<(std::path::PathBuf, Option<Instant>)>,
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

/// The keyboard layout the desktop uses (`/etc/default/keyboard` on Debian
/// and Ubuntu, `localectl`'s X11 file elsewhere), as `XKB_DEFAULT_*` would.
fn xkb_names() -> (String, String, String) {
    let mut layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_default();
    let mut variant = std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default();
    let mut options = std::env::var("XKB_DEFAULT_OPTIONS").unwrap_or_default();
    if layout.is_empty() {
        if let Ok(t) = std::fs::read_to_string("/etc/default/keyboard") {
            for line in t.lines() {
                let v = |k: &str| line.strip_prefix(k).map(|v| v.trim().trim_matches('"').to_string());
                if let Some(x) = v("XKBLAYOUT=") {
                    layout = x;
                } else if let Some(x) = v("XKBVARIANT=") {
                    variant = x;
                } else if let Some(x) = v("XKBOPTIONS=") {
                    options = x;
                }
            }
        } else if let Ok(t) = std::fs::read_to_string("/etc/X11/xorg.conf.d/00-keyboard.conf") {
            for line in t.lines() {
                let mut w = line.split_whitespace();
                if w.next() != Some("Option") {
                    continue;
                }
                let (Some(k), Some(v)) = (w.next(), w.next()) else { continue };
                let v = v.trim_matches('"').to_string();
                match k.trim_matches('"') {
                    "XkbLayout" => layout = v,
                    "XkbVariant" => variant = v,
                    "XkbOptions" => options = v,
                    _ => {}
                }
            }
        }
    }
    (layout, variant, options)
}

/// The desk window's own frame (where the desktop has it drawn by the app,
/// as GNOME does) dark or light as Patch's theme is.
fn host_theme(p: &patch_ui::Palette) -> HostTheme {
    let c = p.bg;
    let luma = 0.2126 * c.red() as f32 + 0.7152 * c.green() as f32 + 0.0722 * c.blue() as f32;
    if luma < 128.0 { HostTheme::Dark } else { HostTheme::Light }
}

fn toplevel_title(w: &Window) -> Option<String> {
    let tl = w.toplevel()?;
    with_states(tl.wl_surface(), |s| s.data_map.get::<XdgToplevelSurfaceData>()?.lock().ok()?.title.clone())
}

/// A Unit's full size, as its size note counts it.
fn spec_min(surface: &str) -> (f32, f32) {
    patch_surfaces::spec::spec().instance(surface).map_or((320.0, 200.0), |(s, _)| {
        let (w, h) = patch_host::session::minimum_size(surface, s.min_size);
        (w as f32, h as f32)
    })
}

fn run(layout: &str, exe: &std::path::Path, _lock: std::fs::File) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let dh = display.handle();

    let app_id = crate::APP.app_id();
    let attributes = HostWindow::default_attributes()
        .with_theme(Some(host_theme(&patch_ui::load_palette())))
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(640.0, 400.0))
        .with_maximized(true)
        .with_title("Patch");
    let attributes = WindowAttributesExtWayland::with_name(attributes, app_id.clone(), app_id.clone());
    let attributes = WindowAttributesExtX11::with_name(attributes, app_id.clone(), app_id);
    let (mut backend, winit) = backend_winit::init_from_attributes_with_gl_attr::<GlesRenderer>(
        attributes,
        smithay::backend::egl::context::GlAttributes { version: (3, 0), profile: None, debug: false, vsync: false },
    )
    .map_err(|e| anyhow::anyhow!("the desk window: {e}"))?;

    let scale = backend.window().scale_factor();
    let phys = backend.window_size();
    if std::env::var_os("PATCH_DESK_LOG").is_some() {
        eprintln!("desk: window {}x{} at {scale} (start)", phys.w, phys.h);
    }
    let refresh = backend
        .window()
        .current_monitor()
        .and_then(|m| m.refresh_rate_millihertz())
        .filter(|r| *r >= 30_000)
        .unwrap_or(60_000);
    let output = Output::new(
        "desk".into(),
        PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Patch".into(), model: "Desk".into() },
    );
    let _output_global = output.create_global::<State>(&dh);
    let mode = OutputMode { size: phys, refresh: refresh as i32 };
    output.change_current_state(Some(mode), Some(Transform::Flipped180), Some(OutputScale::Fractional(scale)), Some((0, 0).into()));
    output.set_preferred(mode);

    // Units' buffers: shared memory, or GPU buffers the desk's GL imports.
    let formats = backend.renderer().dmabuf_formats();
    // The GPU's render node, for dmabuf feedback (none on a software GL).
    let node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .ok()
        .and_then(|d| d.render_device_path().ok())
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| std::os::unix::fs::MetadataExt::rdev(&m) as libc::dev_t);
    let mut dmabuf_state = DmabufState::new();
    let dmabuf_global = match node.and_then(|n| DmabufFeedbackBuilder::new(n, formats.clone()).build().ok()) {
        Some(feedback) => dmabuf_state.create_global_with_default_feedback::<State>(&dh, &feedback),
        None => dmabuf_state.create_global::<State>(&dh, formats),
    };
    let mut shm_state = ShmState::new::<State>(&dh, vec![]);
    shm_state.update_formats(backend.renderer().shm_formats());

    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(&dh, "desk");
    let (layout_names, variant, options) = xkb_names();
    let xkb = XkbConfig {
        layout: &layout_names,
        variant: &variant,
        options: (!options.is_empty()).then_some(options.clone()),
        ..Default::default()
    };
    if seat.add_keyboard(xkb, 400, 30).is_err() {
        seat.add_keyboard(XkbConfig::default(), 400, 30).map_err(|e| anyhow::anyhow!("keyboard: {e:?}"))?;
    }
    seat.add_pointer();

    let mut space = Space::default();
    space.map_output(&output, (0, 0));

    let socket = ListeningSocketSource::new_auto()?;
    let socket_name = socket.socket_name().to_os_string();
    if std::env::var_os("PATCH_DESK_LOG").is_some() {
        eprintln!("desk: socket {}", socket_name.to_string_lossy());
    }
    let handle = event_loop.handle();
    handle
        .insert_source(socket, |stream, _, state: &mut State| {
            let _ = state.dh.insert_client(stream, Arc::new(ClientState::default()));
        })
        .map_err(|e| anyhow::anyhow!("desk socket: {e}"))?;
    handle
        .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
            // SAFETY: the display lives as long as this source.
            unsafe {
                let _ = display.get_mut().dispatch_clients(state);
            }
            state.flush();
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("desk display: {e}"))?;

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, state| {
            if let channel::Event::Msg(msg) = event {
                match msg {
                    Msg::Report(r) => state.apply(r),
                    Msg::SessionEnded => state.signal.stop(),
                    Msg::HostClipboard(text) => state.host_clipboard(text),
                    Msg::DeskClipboard(text) => {
                        if state.clip_last.as_ref() != Some(&text) {
                            state.clip_last = Some(text.clone());
                            let _ = state.clipboard.send(Clip::Set(text));
                        }
                    }
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("desk channel: {e}"))?;

    // The theme: the desk's own parts follow it as the Units do.
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            let stamp = patch_ui::theme_stamp();
            if stamp != state.theme {
                state.theme = stamp;
                state.palette = patch_ui::load_palette();
                state.backend.window().set_theme(Some(host_theme(&state.palette)));
                state.header_model = None;
                state.redraw();
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow::anyhow!("desk timer: {e}"))?;

    // The session inside: Patch's own windows, in this desk.
    let mut child = std::process::Command::new(exe)
        .args(["layout", layout])
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("PATCH_DESK", "1")
        .env("XDG_SESSION_TYPE", "wayland")
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .spawn()?;
    let session = child.id();
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("desk-session".into()).spawn(move || {
            let _ = child.wait();
            let _ = tx.send(Msg::SessionEnded);
        })?;
    }

    let size = phys.to_f64().to_logical(scale).to_i32_round();
    let mut state = State {
        dh: dh.clone(),
        handle: handle.clone(),
        signal: event_loop.get_signal(),
        damage: OutputDamageTracker::from_output(&output),
        output,
        space,
        popups: PopupManager::default(),
        compositor_state: CompositorState::new::<State>(&dh),
        xdg_shell_state: XdgShellState::new::<State>(&dh),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<State>(&dh),
        dmabuf_state,
        _dmabuf_global: dmabuf_global,
        _output_manager: OutputManagerState::new_with_xdg_output::<State>(&dh),
        _xdg_decoration: XdgDecorationState::new::<State>(&dh),
        _fractional_scale: FractionalScaleManagerState::new::<State>(&dh),
        _viewporter: ViewporterState::new::<State>(&dh),
        _cursor_shape: CursorShapeManagerState::new::<State>(&dh),
        seat,
        start: Instant::now(),
        units: Vec::new(),
        laid: None,
        page: 0,
        zoom: None,
        edited: false,
        titles: HashMap::new(),
        opening_until: Some(Instant::now() + Duration::from_millis(2500)),
        size,
        scale,
        frame_interval: Duration::from_micros(1_000_000_000 / refresh as u64),
        palette: patch_ui::load_palette(),
        theme: patch_ui::theme_stamp(),
        header: Header::new(),
        header_model: None,
        header_buffer: None,
        dividers: Vec::new(),
        divider_buffers: Vec::new(),
        pointer: (0.0, 0.0).into(),
        hover: Hover::None,
        drag: None,
        last_press: None,
        cursor: CursorImageStatus::default_named(),
        host_cursor: None,
        focus_before_blur: None,
        focus_history: Vec::new(),
        redraw_due: false,
        frame_timer: false,
        last_frame: Instant::now(),
        sync_due: false,
        sync_tries: 0,
        clipboard: clipboard_thread(tx.clone()),
        clip_last: None,
        tx,
        session,
        shot: std::env::var_os("PATCH_DESK_SHOT").map(|p| (p.into(), None)),
        backend,
    };
    // The layout's name in the header at once, before its windows map.
    state.laid_for(layout);
    state.offer_host_clipboard();

    handle
        .insert_source(winit, |event, _, state| state.winit_event(event))
        .map_err(|e| anyhow::anyhow!("desk window events: {e}"))?;
    state.redraw();
    event_loop.run(None, &mut state, |_| {})?;
    // The session ended, or the desk closed: a session still inside goes too.
    // SAFETY: a signal to the child this desk started.
    unsafe {
        libc::kill(state.session as i32, libc::SIGTERM);
    }
    Ok(())
}

impl State {
    fn flush(&mut self) {
        let _ = self.dh.flush_clients();
    }

    /// The part of the desk the Units share (logical px).
    fn area(&self) -> Rect {
        let top = header::HEIGHT + GAP;
        Rect::new(GAP, top, (self.size.w as f32 - 2.0 * GAP).max(1.0), (self.size.h as f32 - top - GAP).max(1.0))
    }

    fn min_of(&self, surface: &str) -> (f32, f32) {
        let client = self.units.iter().find(|u| u.surface.as_deref() == Some(surface)).and_then(|u| {
            let tl = u.window.toplevel()?;
            let m = with_states(tl.wl_surface(), |s| s.cached_state.get::<SurfaceCachedState>().current().min_size);
            (m.w > 0 && m.h > 0).then_some((m.w as f32, m.h as f32))
        });
        client.unwrap_or_else(|| spec_min(surface))
    }

    /// Set up the desk for layout `id` (its pages and their shares).
    fn laid_for(&mut self, id: &str) {
        if self.laid.as_ref().is_some_and(|l| l.id == id) {
            return;
        }
        let spec = patch_surfaces::spec::spec();
        let Some(recipe) = spec.layout(id).cloned() else {
            self.laid = None;
            return;
        };
        let recipe = patch_surfaces::spec::arrange(&patch_core::setup::load(), recipe, None);
        let openable = |s: &str| spec.instance(s).is_some_and(|(x, _)| x.origin.can_open());
        let Some(geometry) = Geometry::from_recipe(&recipe, openable, GAP) else {
            self.laid = None;
            return;
        };
        let name_of = |s: &str| spec.instance(s).map_or(s.to_string(), |(x, _)| x.title.to_string());
        let pages = geometry
            .pages
            .iter()
            .enumerate()
            .map(|(n, p)| {
                let units = p.tiles.as_ref().map(|t| t.units()).unwrap_or_default();
                let names: Vec<String> = units.iter().take(2).map(|u| name_of(u)).collect();
                let more = if units.len() > 2 { " …" } else { "" };
                format!("{} {}{more}", n + 1, names.join(" · "))
            })
            .collect();
        self.laid = Some(Laid { id: id.to_string(), name: recipe.name.to_string(), original: geometry.clone(), geometry, pages });
        self.page = 0;
        self.zoom = None;
        self.edited = false;
        self.opening_until = Some(Instant::now() + Duration::from_millis(2500));
        self.backend.window().set_title(&format!("Patch · {}", recipe.name));
        self.header_model = None;
    }

    /// Ask the session inside which windows are which (off the desk's
    /// thread: the session may be waiting on the desk meanwhile).
    fn sync(&mut self) {
        if self.sync_due {
            return;
        }
        self.sync_due = true;
        let _ = self.handle.insert_source(Timer::from_duration(Duration::from_millis(30)), |_, _, state| {
            let tx = state.tx.clone();
            let _ = std::thread::Builder::new().name("desk-sync".into()).spawn(move || {
                let r = send(Request::Desk).and_then(|j| serde_json::from_str::<Report>(&j).ok());
                let _ = tx.send(Msg::Report(r));
            });
            TimeoutAction::Drop
        });
    }

    fn apply(&mut self, report: Option<Report>) {
        self.sync_due = false;
        let Some(report) = report else {
            // The session isn't listening yet: again shortly.
            if self.sync_tries < 40 {
                self.sync_tries += 1;
                self.sync();
            }
            return;
        };
        match report.layout.as_deref() {
            Some(id) => self.laid_for(id),
            None => {
                if self.laid.is_some() && !report.windows.is_empty() {
                    // The layout's windows closed (others stay, floating).
                    self.laid = None;
                    self.backend.window().set_title("Patch");
                    self.header_model = None;
                }
            }
        }
        self.titles = report.windows.into_iter().map(|(s, t)| (t, s)).collect();
        let mut unknown = false;
        for u in self.units.iter_mut() {
            let title = toplevel_title(&u.window);
            u.surface = title.as_ref().and_then(|t| self.titles.get(t).cloned());
            unknown |= u.surface.is_none();
        }
        if unknown && self.sync_tries < 40 {
            self.sync_tries += 1;
            self.sync();
        } else {
            self.sync_tries = 0;
        }
        self.arrange();
    }

    /// Place every window: the page's Units in their tiles, others floating.
    fn arrange(&mut self) {
        let area = self.area();
        let min = |s: &str| self.min_of(s);
        let (placed, dividers, layout_units): (Vec<(&'static str, Rect)>, _, Vec<(usize, &'static str)>) = match &self.laid {
            Some(l) => {
                let all = l
                    .geometry
                    .pages
                    .iter()
                    .enumerate()
                    .flat_map(|(n, p)| {
                        let mut v: Vec<(usize, &'static str)> =
                            p.tiles.as_ref().map(|t| t.units()).unwrap_or_default().into_iter().map(|u| (n, u)).collect();
                        v.extend(p.floating.iter().map(|t| (n, t.surface)));
                        v
                    })
                    .collect();
                match self.zoom {
                    Some(z) => (vec![(z, area)], Vec::new(), all),
                    None => (l.geometry.place(self.page, area, &min), l.geometry.dividers(self.page, area, &min), all),
                }
            }
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        self.dividers = dividers.into_iter().map(|d| (d.rect, d.path, d.across)).collect();
        if std::env::var_os("PATCH_DESK_LOG").is_some() {
            // Tests (integration/test-desk.sh) read where things are.
            for (s, r) in &placed {
                eprintln!("desk: tile {s} {:.0} {:.0} {:.0} {:.0}", r.x, r.y, r.w, r.h);
            }
            for (r, _, across) in &self.dividers {
                eprintln!("desk: divider {} {:.0} {:.0} {:.0} {:.0}", if *across { "across" } else { "down" }, r.x, r.y, r.w, r.h);
            }
        }

        // The layout's windows all there: the opening is over.
        if let Some(until) = self.opening_until {
            let page_units: Vec<&str> = placed.iter().map(|(s, _)| *s).collect();
            let all_in = !page_units.is_empty()
                && page_units.iter().all(|s| self.units.iter().any(|u| u.surface.as_deref() == Some(*s) && u.window.geometry().size.w > 0));
            if all_in || Instant::now() >= until {
                self.opening_until = None;
                self.header_model = None;
                self.focus_first(&placed);
            }
        }

        let scale = self.scale;
        let mut new_float = None;
        for u in self.units.iter_mut() {
            let Some(tl) = u.window.toplevel().cloned() else { continue };
            let tiled = u.surface.as_deref().and_then(|s| placed.iter().find(|(p, _)| *p == s)).map(|(_, r)| *r);
            let elsewhere = u.surface.as_deref().and_then(|s| layout_units.iter().find(|(_, p)| *p == s)).map(|(n, _)| *n);
            let floating_spec = self.laid.as_ref().zip(u.surface.as_deref()).is_some_and(|(l, s)| {
                l.geometry.pages.get(self.page).is_some_and(|p| p.floating.iter().any(|t| t.surface == s))
            });
            match (tiled, elsewhere) {
                (Some(r), _) => {
                    // Whole pixels at the desk's scale, so Units stay crisp.
                    let snap = |v: f32| ((v as f64 * scale).round() / scale) as f32;
                    let (x, y) = (snap(r.x), snap(r.y));
                    let (w, h) = (snap(r.x + r.w) - x, snap(r.y + r.h) - y);
                    let r = Rect::new(x, y, w, h);
                    tl.with_pending_state(|s| {
                        s.size = Some((w.round() as i32, h.round() as i32).into());
                        if !floating_spec {
                            for t in [xdg_toplevel::State::TiledLeft, xdg_toplevel::State::TiledRight, xdg_toplevel::State::TiledTop, xdg_toplevel::State::TiledBottom] {
                                s.states.set(t);
                            }
                        }
                    });
                    if tl.is_initial_configure_sent() {
                        tl.send_pending_configure();
                    }
                    u.tile = (!floating_spec).then_some(r);
                    self.space.map_element(u.window.clone(), (x.round() as i32, y.round() as i32), false);
                }
                (None, Some(_)) => {
                    // On another page (or hidden behind a zoomed Unit).
                    u.tile = None;
                    self.space.unmap_elem(&u.window);
                }
                (None, None) => {
                    // Not in the layout: floating, centred, at its own size,
                    // and it has the keyboard (the controller, a dialog).
                    u.tile = None;
                    if self.space.element_location(&u.window).is_none() && u.window.geometry().size.w > 0 {
                        new_float = Some(tl.wl_surface().clone());
                        let g = u.window.geometry().size;
                        let x = area.x + (area.w - g.w as f32).max(0.0) / 2.0;
                        let y = area.y + (area.h - g.h as f32).max(0.0) / 3.0;
                        self.space.map_element(u.window.clone(), (x.round() as i32, y.round() as i32), true);
                    }
                }
            }
        }
        if new_float.is_some() && self.opening_until.is_none() {
            self.focus(new_float);
        }
        // Floating windows over the tiles.
        let floats: Vec<Window> = self.units.iter().filter(|u| u.tile.is_none()).map(|u| u.window.clone()).collect();
        for w in floats {
            if self.space.element_location(&w).is_some() {
                self.space.raise_element(&w, false);
            }
        }
        if let Some(l) = &self.laid {
            let (mw, mh) = l.geometry.minimum(self.page, &|s: &str| self.min_of(s));
            // Never more than the monitor: a page too big for it shares
            // the space by the minima (each Unit shows its size note).
            let screen = self
                .backend
                .window()
                .current_monitor()
                .map(|m| m.size().to_logical::<f64>(self.scale))
                .map_or((f64::MAX, f64::MAX), |s| (s.width * 0.95, s.height * 0.9));
            let w = ((mw + 2.0 * GAP) as f64).min(screen.0).max(640.0);
            let h = ((mh + header::HEIGHT + 2.0 * GAP) as f64).min(screen.1).max(400.0);
            self.backend.window().set_min_inner_size(Some(LogicalSize::new(w, h)));
        }
        self.header_model = None;
        self.redraw();
    }

    fn focus_first(&mut self, placed: &[(&'static str, Rect)]) {
        let Some((first, _)) = placed.first() else { return };
        let surface = self
            .units
            .iter()
            .find(|u| u.surface.as_deref() == Some(*first))
            .and_then(|u| u.window.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(s) = surface {
            self.focus(Some(s));
        }
    }

    fn focus(&mut self, surface: Option<WlSurface>) {
        if std::env::var_os("PATCH_DESK_LOG").is_some() {
            let title = surface.as_ref().and_then(|s| self.unit_for(s)).and_then(|i| toplevel_title(&self.units[i].window));
            eprintln!("desk: focus {}", title.as_deref().unwrap_or("-"));
        }
        if let Some(s) = &surface {
            self.focus_history.retain(|h| h != s && h.is_alive());
            self.focus_history.push(s.clone());
        }
        let keyboard = self.seat.get_keyboard().expect("keyboard");
        keyboard.set_focus(self, surface.clone(), SERIAL_COUNTER.next_serial());
        for u in &self.units {
            if let Some(tl) = u.window.toplevel() {
                let on = surface.as_ref() == Some(tl.wl_surface());
                u.window.set_activated(on);
                if tl.is_initial_configure_sent() {
                    tl.send_pending_configure();
                }
            }
        }
    }

    fn model(&self) -> Model {
        let spec = patch_surfaces::spec::spec();
        let (layout, pages, units) = match &self.laid {
            Some(l) => {
                let units = l
                    .geometry
                    .pages
                    .get(self.page)
                    .and_then(|p| p.tiles.as_ref())
                    .map(|t| t.units())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|u| (u, spec.instance(u).map_or(u.to_string(), |(x, _)| x.title.to_string())))
                    .collect();
                (l.name.clone(), l.pages.clone(), units)
            }
            None => (String::new(), Vec::new(), Vec::new()),
        };
        Model {
            layout,
            pages,
            page: self.page,
            units,
            zoom: self.zoom,
            edited: self.edited,
            opening: self.opening_until.is_some(),
        }
    }

    fn redraw(&mut self) {
        if self.redraw_due {
            return;
        }
        self.redraw_due = true;
        if self.frame_timer {
            return;
        }
        // At most one frame per refresh of the monitor.
        let wait = (self.last_frame + self.frame_interval).saturating_duration_since(Instant::now());
        if wait.is_zero() {
            self.backend.window().request_redraw();
        } else {
            self.frame_timer = true;
            let _ = self.handle.insert_source(Timer::from_duration(wait), |_, _, state| {
                state.frame_timer = false;
                state.backend.window().request_redraw();
                TimeoutAction::Drop
            });
        }
    }

    fn winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, scale_factor } => {
                if std::env::var_os("PATCH_DESK_LOG").is_some() {
                    eprintln!("desk: window {}x{} at {scale_factor}", size.w, size.h);
                }
                self.scale = scale_factor;
                self.size = size.to_f64().to_logical(scale_factor).to_i32_round();
                let mode = OutputMode { size, refresh: (1_000_000.0 / self.frame_interval.as_micros() as f64 * 1000.0) as i32 };
                self.output.change_current_state(Some(mode), None, Some(OutputScale::Fractional(scale_factor)), None);
                self.output.set_preferred(mode);
                for u in &self.units {
                    u.window.with_surfaces(|_, states| {
                        with_fractional_scale(states, |f| f.set_preferred_scale(scale_factor));
                    });
                }
                self.arrange();
            }
            WinitEvent::Focus(on) => {
                let keyboard = self.seat.get_keyboard().expect("keyboard");
                if on {
                    // What was copied on the desktop meanwhile pastes inside.
                    let _ = self.clipboard.send(Clip::Get);
                    if let Some(s) = self.focus_before_blur.take() {
                        self.focus(Some(s));
                    }
                } else {
                    self.focus_before_blur = keyboard.current_focus();
                    keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
                }
            }
            WinitEvent::Input(event) => self.input(event),
            WinitEvent::Redraw => self.render(),
            WinitEvent::CloseRequested => {
                // The session closes its windows; the desk ends when it has.
                let tx = self.tx.clone();
                let _ = std::thread::Builder::new().name("desk-quit".into()).spawn(move || {
                    send(Request::Quit);
                    std::thread::sleep(Duration::from_secs(3));
                    let _ = tx.send(Msg::SessionEnded);
                });
            }
        }
    }

    fn render(&mut self) {
        self.redraw_due = false;
        self.last_frame = Instant::now();
        let scale = Scale::from(self.scale);
        // The header, drawn again only when what it shows changed.
        let model = self.model();
        let hover = match &self.hover {
            Hover::Header(a) => Some(a.clone()),
            _ => None,
        };
        let key = (model.clone(), hover.clone(), self.size.w, self.scale.to_bits());
        if self.header_model.as_ref() != Some(&key) || self.header_buffer.is_none() {
            if let Some((px, w, h)) = self.header.draw(&model, &self.palette, self.size.w as f32, self.scale, hover.as_ref()) {
                let buffer = MemoryRenderBuffer::from_slice(&px, Fourcc::Abgr8888, (w as i32, h as i32), 1, Transform::Normal, None);
                self.header_buffer = Some((buffer, (w, h)));
            }
            self.header_model = Some(key);
        }
        // Dividers: a hairline in the gap, the accent under the pointer.
        let p = &self.palette;
        let rgba = |c: slint::Color| [c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0, c.alpha() as f32 / 255.0];
        while self.divider_buffers.len() < self.dividers.len() {
            self.divider_buffers.push(SolidColorBuffer::default());
        }
        let mut lines = Vec::new();
        for (i, (r, path, across)) in self.dividers.iter().enumerate() {
            let active = matches!(&self.hover, Hover::Divider(h, _) if h == path) || self.drag.as_ref().is_some_and(|(d, _)| d == path);
            let (thick, color) = if active { (3.0, rgba(p.accent)) } else { (1.0, rgba(p.border)) };
            let line = if *across {
                Rect::new(r.x + (r.w - thick) / 2.0, r.y, thick, r.h)
            } else {
                Rect::new(r.x, r.y + (r.h - thick) / 2.0, r.w, thick)
            };
            let buf = &mut self.divider_buffers[i];
            buf.update((line.w.round().max(1.0) as i32, line.h.round().max(1.0) as i32), color);
            lines.push((i, Point::<f64, Logical>::from((line.x as f64, line.y as f64))));
        }
        let bg = rgba(p.bg_deeper);
        let opening = self.opening_until.is_some();
        let age = self.backend.buffer_age().unwrap_or(0);
        let window_size = self.backend.window_size();

        let (renderer, mut fb) = match self.backend.bind() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("desk: {e}");
                return;
            }
        };
        let mut elements: Vec<DeskElement> = Vec::new();
        if let Some((buf, (pw, ph))) = &self.header_buffer
            && let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (0.0, 0.0),
                buf,
                None,
                // The whole picture (drawn at the desk's scale) into the strip.
                Some(Rectangle::from_size((*pw as f64, *ph as f64).into())),
                Some((self.size.w, header::HEIGHT as i32).into()),
                Kind::Unspecified,
            )
        {
            elements.push(DeskElement::Memory(e));
        }
        if !opening {
            for w in self.space.elements().rev() {
                let Some(loc) = self.space.element_location(w) else { continue };
                let at = (loc - w.geometry().loc).to_physical_precise_round(scale);
                let tile = self.units.iter().find(|u| &u.window == w).and_then(|u| u.tile);
                let parts = w.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(renderer, at, scale, 1.0);
                match tile {
                    Some(r) => {
                        let crop = Rectangle::<f64, Logical>::new((r.x as f64, r.y as f64).into(), (r.w as f64, r.h as f64).into())
                            .to_physical_precise_round(scale);
                        elements.extend(parts.into_iter().filter_map(|e| CropRenderElement::from_element(e, scale, crop)).map(DeskElement::Tiled));
                    }
                    None => elements.extend(parts.into_iter().map(DeskElement::Floating)),
                }
            }
            for (i, at) in &lines {
                elements.push(DeskElement::Solid(SolidColorRenderElement::from_buffer(
                    &self.divider_buffers[*i],
                    at.to_physical_precise_round(scale),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )));
            }
        }
        let result = self.damage.render_output(renderer, &mut fb, age, &elements, bg);
        if let Some((path, due)) = &mut self.shot
            && !opening
        {
            // A second after the layout opened: Units have drawn by then.
            let wait = std::env::var("PATCH_DESK_SHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
            let at = *due.get_or_insert(Instant::now() + Duration::from_millis(wait));
            if Instant::now() >= at {
                let size = window_size;
                let region = Rectangle::from_size((size.w, size.h).into());
                let saved = renderer
                    .copy_framebuffer(&fb, region, Fourcc::Abgr8888)
                    .and_then(|m| renderer.map_texture(&m).map(|px| px.to_vec()))
                    .ok()
                    .and_then(|px| save_png(path, &px, size.w as u32, size.h as u32));
                eprintln!("desk: shot {} {}", path.display(), if saved.is_some() { "saved" } else { "FAILED" });
                self.signal.stop();
            } else {
                let _ = self.handle.insert_source(Timer::from_duration(at - Instant::now()), |_, _, state| {
                    state.header_model = None;
                    state.redraw();
                    TimeoutAction::Drop
                });
            }
        }
        drop(fb);
        match result {
            Ok(r) => {
                if let Some(damage) = r.damage.cloned()
                    && let Err(e) = self.backend.submit(Some(&damage))
                {
                    eprintln!("desk: {e}");
                }
            }
            Err(e) => eprintln!("desk: {e:?}"),
        }
        // Frame callbacks: Units draw their next frame now.
        let now = self.start.elapsed();
        let output = self.output.clone();
        for w in self.space.elements() {
            w.send_frame(&output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        self.space.refresh();
        self.popups.cleanup();
        self.flush();
    }

    /// The window under a point, and the point inside its surface.
    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        for w in self.space.elements().rev() {
            let Some(loc) = self.space.element_location(w) else { continue };
            if let Some(r) = self.units.iter().find(|u| &u.window == w).and_then(|u| u.tile)
                && !r.contains(pos.x as f32, pos.y as f32)
            {
                continue;
            }
            let at = (loc - w.geometry().loc).to_f64();
            if let Some((s, p)) = w.surface_under(pos - at, WindowSurfaceType::ALL) {
                return Some((s, p.to_f64() + at));
            }
        }
        None
    }

    fn hover_at(&self, pos: Point<f64, Logical>) -> Hover {
        let (x, y) = (pos.x as f32, pos.y as f32);
        if y < header::HEIGHT {
            return self.header.hit(x, y).cloned().map_or(Hover::None, Hover::Header);
        }
        // Over a floating window: it has the pointer, not a divider under it.
        if let Some((s, _)) = self.surface_under(pos) {
            let floating = self.units.iter().any(|u| u.tile.is_none() && u.window.toplevel().is_some_and(|t| t.wl_surface() == &s));
            if floating {
                return Hover::Client;
            }
        }
        for (r, path, across) in &self.dividers {
            // A little wider than the gap, easier to catch.
            let grab = if *across { Rect::new(r.x - 3.0, r.y, r.w + 6.0, r.h) } else { Rect::new(r.x, r.y - 3.0, r.w, r.h + 6.0) };
            if grab.contains(x, y) {
                return Hover::Divider(path.clone(), *across);
            }
        }
        Hover::Client
    }

    fn set_host_cursor(&mut self, icon: Option<CursorIcon>) {
        let want = icon.or(match &self.cursor {
            CursorImageStatus::Named(i) => Some(*i),
            CursorImageStatus::Hidden => None,
            _ => Some(CursorIcon::Default),
        });
        if want == self.host_cursor {
            return;
        }
        self.host_cursor = want;
        match want {
            Some(i) => {
                self.backend.window().set_cursor(i);
                self.backend.window().set_cursor_visible(true);
            }
            None => self.backend.window().set_cursor_visible(false),
        }
    }

    fn header_action(&mut self, a: Action) {
        match a {
            Action::Page(n) => {
                self.page = n;
                self.zoom = None;
            }
            Action::Zoom(u) => {
                self.zoom = if self.zoom == Some(u) { None } else { Some(u) };
                self.edited = self.zoom.is_some() || self.laid.as_ref().is_some_and(|l| l.geometry != l.original);
                if let Some(z) = self.zoom {
                    let s = self.units.iter().find(|x| x.surface.as_deref() == Some(z)).and_then(|x| x.window.toplevel().map(|t| t.wl_surface().clone()));
                    if s.is_some() {
                        self.focus(s);
                    }
                }
            }
            Action::Reset => {
                if let Some(l) = self.laid.as_mut() {
                    l.geometry = l.original.clone();
                }
                self.zoom = None;
                self.edited = false;
            }
        }
        self.arrange();
    }

    fn input(&mut self, event: InputEvent<backend_winit::WinitInput>) {
        match event {
            InputEvent::Keyboard { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = Event::time_msec(&event);
                let pressed = event.state() == KeyState::Pressed;
                let keyboard = self.seat.get_keyboard().expect("keyboard");
                let page = keyboard.input::<i32, _>(self, event.key_code(), event.state(), serial, time, |_, mods, key| {
                    // Ctrl+Page Up/Down: the layout's pages, as a browser's tabs.
                    if pressed && mods.ctrl && !mods.alt && !mods.shift {
                        match key.modified_sym() {
                            Keysym::Page_Up => return FilterResult::Intercept(-1),
                            Keysym::Page_Down => return FilterResult::Intercept(1),
                            _ => {}
                        }
                    }
                    FilterResult::Forward
                });
                if let Some(step) = page
                    && let Some(l) = &self.laid
                {
                    let n = l.geometry.pages.len() as i32;
                    if n > 1 {
                        let next = (self.page as i32 + step).rem_euclid(n) as usize;
                        self.header_action(Action::Page(next));
                    }
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let pos = event.position_transformed(self.size);
                self.pointer = pos;
                let serial = SERIAL_COUNTER.next_serial();
                let pointer = self.seat.get_pointer().expect("pointer");
                if let Some((path, across)) = self.drag.clone() {
                    let area = self.area();
                    let at = if across { pos.x } else { pos.y } as f32;
                    let page = self.page;
                    let mins: HashMap<String, (f32, f32)> =
                        self.units.iter().filter_map(|u| u.surface.clone()).map(|s| (s.clone(), self.min_of(&s))).collect();
                    let min = |s: &str| mins.get(s).copied().unwrap_or_else(|| spec_min(s));
                    if let Some(l) = self.laid.as_mut() {
                        l.geometry.drag(page, &path, at, area, &min);
                        self.edited = l.geometry != l.original;
                    }
                    self.arrange();
                    return;
                }
                let hover = self.hover_at(pos);
                if hover != self.hover {
                    let was_header = matches!(self.hover, Hover::Header(_)) || matches!(hover, Hover::Header(_));
                    let was_divider = matches!(self.hover, Hover::Divider(..)) || matches!(hover, Hover::Divider(..));
                    self.hover = hover.clone();
                    if was_header || was_divider {
                        self.redraw();
                    }
                }
                match &hover {
                    Hover::Client => {
                        let under = self.surface_under(pos);
                        pointer.motion(self, under, &MotionEvent { location: pos, serial, time: event.time_msec() });
                        self.set_host_cursor(None);
                    }
                    other => {
                        pointer.motion(self, None, &MotionEvent { location: pos, serial, time: event.time_msec() });
                        let icon = match other {
                            Hover::Divider(_, true) => CursorIcon::ColResize,
                            Hover::Divider(_, false) => CursorIcon::RowResize,
                            Hover::Header(_) => CursorIcon::Pointer,
                            _ => CursorIcon::Default,
                        };
                        self.set_host_cursor(Some(icon));
                    }
                }
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let pointer = self.seat.get_pointer().expect("pointer");
                let serial = SERIAL_COUNTER.next_serial();
                let state = event.state();
                if state == ButtonState::Released && self.drag.take().is_some() {
                    self.redraw();
                    return;
                }
                if state == ButtonState::Pressed && !pointer.is_grabbed() {
                    match self.hover.clone() {
                        Hover::Header(a) => {
                            self.header_action(a);
                            return;
                        }
                        Hover::Divider(path, across) => {
                            // A double-click puts this split back as the layout has it.
                            let double = self.last_press.as_ref().is_some_and(|(t, p)| *p == path && t.elapsed() < Duration::from_millis(400));
                            self.last_press = Some((Instant::now(), path.clone()));
                            if double {
                                let page = self.page;
                                if let Some(l) = self.laid.as_mut()
                                    && let (Some(Some(orig)), Some(Some(now))) = (
                                        l.original.pages.get(page).map(|p| p.tiles.clone()),
                                        l.geometry.pages.get_mut(page).map(|p| p.tiles.as_mut()),
                                    )
                                {
                                    restore_ratio(now, &orig, &path);
                                    self.edited = l.geometry != l.original;
                                }
                                self.arrange();
                            } else {
                                self.drag = Some((path, across));
                            }
                            return;
                        }
                        Hover::None => return,
                        Hover::Client => {
                            let under = self.surface_under(pointer.current_location());
                            let window = under.as_ref().and_then(|(s, _)| {
                                let mut root = s.clone();
                                while let Some(p) = get_parent(&root) {
                                    root = p;
                                }
                                self.units.iter().find(|u| u.window.toplevel().is_some_and(|t| t.wl_surface() == &root)).map(|u| (u.window.clone(), root))
                            });
                            if let Some((w, root)) = window {
                                if self.units.iter().any(|u| u.window == w && u.tile.is_none()) {
                                    self.space.raise_element(&w, false);
                                }
                                let current = self.seat.get_keyboard().and_then(|k| k.current_focus());
                                if current.as_ref() != Some(&root) {
                                    self.focus(Some(root));
                                }
                            }
                        }
                    }
                }
                if std::env::var_os("PATCH_DESK_LOG").is_some() && state == ButtonState::Pressed {
                    let p = pointer.current_location();
                    let on = self.surface_under(p).and_then(|(s, _)| {
                        let mut root = s;
                        while let Some(r) = get_parent(&root) {
                            root = r;
                        }
                        self.unit_for(&root).and_then(|i| toplevel_title(&self.units[i].window))
                    });
                    eprintln!("desk: press {:.0} {:.0} on {}", p.x, p.y, on.as_deref().unwrap_or("-"));
                }
                pointer.button(self, &ButtonEvent { button: event.button_code(), state, serial, time: event.time_msec() });
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                if !matches!(self.hover, Hover::Client) {
                    return;
                }
                let source = event.source();
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let amount = event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(d) = event.amount_v120(axis) {
                            frame = frame.v120(axis, d as i32);
                        }
                    } else if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                        frame = frame.stop(axis);
                    }
                }
                let pointer = self.seat.get_pointer().expect("pointer");
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    /// Room for a Unit at its full size: the desk grows to the page's full
    /// size when the monitor has it, else the Unit shows alone on its page.
    fn make_room(&mut self, surface: &str) {
        let Some(l) = &self.laid else { return };
        let Some(unit) = l.geometry.pages.iter().flat_map(|p| p.tiles.iter().flat_map(|t| t.units())).find(|u| *u == surface) else {
            return;
        };
        let (mw, mh) = l.geometry.minimum(self.page, &spec_min);
        let need = ((mw + 2.0 * GAP) as f64, (mh + header::HEIGHT + 2.0 * GAP) as f64);
        let window = self.backend.window();
        let screen = window
            .current_monitor()
            .map(|m| m.size().to_logical::<f64>(self.scale))
            .map_or((0.0, 0.0), |s| (s.width * 0.95, s.height * 0.9));
        let small = (self.size.w as f64) < need.0 || (self.size.h as f64) < need.1;
        if small && !window.is_maximized() && need.0 <= screen.0 && need.1 <= screen.1 {
            let _ = window.request_inner_size(LogicalSize::new(need.0.max(self.size.w as f64), need.1.max(self.size.h as f64)));
            return;
        }
        self.zoom = Some(unit);
        self.edited = true;
        self.arrange();
    }

    /// The desktop's clipboard text into the desk, as the desk's own
    /// selection (Units paste it).
    fn host_clipboard(&mut self, text: Option<String>) {
        // What a Unit copied last is still the desktop's: it stays as it is.
        if text.is_some() && text == self.clip_last {
            return;
        }
        self.offer_host_clipboard();
    }

    /// The desk's selection is the desktop's clipboard, read when a Unit
    /// pastes (always current, nothing copied ahead).
    fn offer_host_clipboard(&mut self) {
        self.clip_last = None;
        let mimes = TEXT.iter().map(|m| m.to_string()).collect();
        set_data_device_selection(&self.dh, &self.seat, mimes, Arc::new(String::new()));
    }

    fn unit_for(&self, surface: &WlSurface) -> Option<usize> {
        self.units.iter().position(|u| u.window.toplevel().is_some_and(|t| t.wl_surface() == surface))
    }
}

/// A framebuffer read back (GL's rows run bottom up) as a PNG.
fn save_png(path: &std::path::Path, px: &[u8], w: u32, h: u32) -> Option<()> {
    let row = w as usize * 4;
    let mut flipped = Vec::with_capacity(px.len());
    for y in (0..h as usize).rev() {
        flipped.extend_from_slice(px.get(y * row..(y + 1) * row)?);
    }
    for p in flipped.chunks_exact_mut(4) {
        p[3] = 255;
    }
    let size = resvg::tiny_skia::IntSize::from_wh(w, h)?;
    resvg::tiny_skia::Pixmap::from_vec(flipped, size)?.save_png(path).ok()
}

/// Put the split at `path` back to its share in `orig`.
fn restore_ratio(now: &mut patch_core::desk::Node, orig: &patch_core::desk::Node, path: &[bool]) {
    use patch_core::desk::Node;
    if let (Node::Split { ratio, a, b, .. }, Node::Split { ratio: r0, a: a0, b: b0, .. }) = (now, orig) {
        match path.split_first() {
            None => *ratio = *r0,
            Some((false, rest)) => restore_ratio(a, a0, rest),
            Some((true, rest)) => restore_ratio(b, b0, rest),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    DeskElement<=GlesRenderer>;
    Tiled=CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Floating=WaylandSurfaceRenderElement<GlesRenderer>,
    Solid=SolidColorRenderElement,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().expect("client state").compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(p) = get_parent(&root) {
                root = p;
            }
            if let Some(i) = self.unit_for(&root) {
                let w = self.units[i].window.clone();
                w.on_commit();
                let tl = w.toplevel().expect("toplevel").clone();
                if !tl.is_initial_configure_sent() {
                    // A Unit the desk knows gets its tile's size at once.
                    let title = toplevel_title(&w);
                    if let Some(s) = title.as_ref().and_then(|t| self.titles.get(t)).cloned() {
                        self.units[i].surface = Some(s);
                    }
                    tl.with_pending_state(|st| {
                        st.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
                    });
                    tl.send_configure();
                    self.sync();
                } else if self.space.element_location(&w).is_none() || self.opening_until.is_some() {
                    // Its first frame: placed (floating ones at their size).
                    self.arrange();
                }
            }
        }
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(p)) = self.popups.find_popup(surface)
            && !p.is_initial_configure_sent()
        {
            let _ = p.send_configure();
        }
        self.redraw();
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.backend.renderer().import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        });
        let window = Window::new_wayland_window(surface);
        self.units.push(Unit { window, surface: None, tile: None });
        self.sync();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(i) = self.unit_for(surface.wl_surface()) {
            let u = self.units.remove(i);
            self.space.unmap_elem(&u.window);
        }
        let keyboard = self.seat.get_keyboard().expect("keyboard");
        if keyboard.current_focus().is_none_or(|f| &f == surface.wl_surface()) {
            // Back to the window that had the keyboard before.
            self.focus_history.retain(|h| h != surface.wl_surface() && h.is_alive());
            let mapped = |s: &WlSurface| self.unit_for(s).is_some_and(|i| self.space.element_location(&self.units[i].window).is_some());
            let next = self.focus_history.iter().rev().find(|s| mapped(s)).cloned().or_else(|| {
                self.units.iter().rev().find_map(|u| self.space.element_location(&u.window).and(u.window.toplevel().map(|t| t.wl_surface().clone())))
            });
            self.focus(next);
        }
        self.sync();
        self.arrange();
    }

    fn title_changed(&mut self, _surface: ToplevelSurface) {
        self.sync();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        self.unconstrain(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if std::env::var_os("PATCH_DESK_LOG").is_some() {
            eprintln!("desk: maximize {}", self.unit_for(surface.wl_surface()).and_then(|i| toplevel_title(&self.units[i].window)).unwrap_or_default());
        }
        // A Unit's "Make room" (the session asks for its window maximized).
        if let Some(s) = self.unit_for(surface.wl_surface()).and_then(|i| self.units[i].surface.clone()) {
            self.make_room(&s);
        }
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>) {
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }
}

impl State {
    fn unconstrain(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(w) = self.units.iter().find(|u| u.window.toplevel().is_some_and(|t| t.wl_surface() == &root)) else { return };
        let Some(geo) = self.space.element_geometry(&w.window) else { return };
        let mut target = Rectangle::from_size(self.size);
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= geo.loc;
        popup.with_pending_state(|s| {
            s.geometry = s.positioner.get_unconstrained_geometry(target);
        });
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: zxdg_toplevel_decoration_v1::Mode) {
        // The desk draws no title bars and wants none: the header names the Units.
        toplevel.with_pending_state(|s| s.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.request_mode(toplevel, zxdg_toplevel_decoration_v1::Mode::ServerSide);
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor = image;
        if matches!(self.hover, Hover::Client) {
            self.set_host_cursor(None);
        }
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, seat, client);
    }
}

impl TabletSeatHandler for State {}

impl SelectionHandler for State {
    type SelectionUserData = Arc<String>;

    /// A Unit copied: its text goes to the desktop's clipboard too.
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, seat: Seat<Self>) {
        if ty != SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else { return };
        let offered = source.mime_types();
        let Some(mime) = TEXT.iter().find(|m| offered.iter().any(|o| o == *m)) else { return };
        // Smithay stores the new selection after telling: ask for its text
        // on the loop's next turn.
        let mime = mime.to_string();
        self.handle.insert_idle(move |state| state.copy_out(&seat, mime));
    }

    /// A Unit pastes what the desktop has copied.
    fn send_selection(&mut self, _ty: SelectionTarget, _mime: String, fd: OwnedFd, _seat: Seat<Self>, text: &Arc<String>) {
        if text.is_empty() {
            let _ = self.clipboard.send(Clip::Paste(fd));
            return;
        }
        let text = text.clone();
        let _ = std::thread::Builder::new().name("desk-paste".into()).spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(fd).write_all(text.as_bytes());
        });
    }
}

impl State {
    /// A Unit's copied text, read and put on the desktop's clipboard.
    fn copy_out(&mut self, seat: &Seat<Self>, mime: String) {
        let Ok((mut reader, writer)) = std::io::pipe() else { return };
        if request_data_device_client_selection(seat, mime, OwnedFd::from(writer)).is_err() {
            return;
        }
        self.flush();
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new().name("desk-copy".into()).spawn(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            // At most 4 MB of text.
            if (&mut reader).take(4 << 20).read_to_end(&mut bytes).is_ok()
                && let Ok(text) = String::from_utf8(bytes)
                && !text.is_empty()
            {
                let _ = tx.send(Msg::DeskClipboard(text));
            }
        });
    }

}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.scale;
        with_states(&surface, |states| with_fractional_scale(states, |f| f.set_preferred_scale(scale)));
    }
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_dmabuf!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);
delegate_cursor_shape!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_click_restores_only_that_split() {
        use patch_core::desk::Node;
        let orig = Node::Split {
            across: false,
            ratio: 0.6,
            a: Box::new(Node::Split { across: true, ratio: 0.4, a: Box::new(Node::Unit("a")), b: Box::new(Node::Unit("b")) }),
            b: Box::new(Node::Unit("c")),
        };
        let mut now = orig.clone();
        if let Node::Split { ratio, a, .. } = &mut now {
            *ratio = 0.3;
            if let Node::Split { ratio, .. } = &mut **a {
                *ratio = 0.9;
            }
        }
        restore_ratio(&mut now, &orig, &[false]);
        let Node::Split { ratio, a, .. } = &now else { panic!() };
        assert_eq!(*ratio, 0.3, "the outer split keeps its drag");
        let Node::Split { ratio, .. } = &**a else { panic!() };
        assert_eq!(*ratio, 0.4, "the inner one is back");
    }
}
