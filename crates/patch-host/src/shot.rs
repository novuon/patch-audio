//! Headless screenshots for visual review.
//!
//! Installs a Slint platform backed by the software renderer, so every
//! surface can be rendered at arbitrary tile sizes without a compositor and
//! without touching the user's desktop. Workspace shots composite several
//! surfaces with Omarchy's default gaps and borders.

use crate::session::{CONTROLLER, HostState, HostedApp, SurfaceWindow};
use image::{Rgba, RgbaImage};
use patch_core::LayoutRecipe;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{PhysicalSize, Rgb8Pixel};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

thread_local! {
    static LAST: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) };
}

struct ShotPlatform;

impl Platform for ShotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let w = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        LAST.with(|l| *l.borrow_mut() = Some(w.clone()));
        Ok(w)
    }
}

static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True when rendering screenshots: providers freeze time for determinism.
pub fn active() -> bool {
    ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn install() -> anyhow::Result<()> {
    ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    slint::platform::set_platform(Box::new(ShotPlatform)).map_err(|e| anyhow::anyhow!("{e:?}"))
}

/// Named tile sizes (logical px) that cover the responsive cases.
pub const SIZES: &[(&str, u32, u32)] = &[
    ("narrow", 320, 760),
    ("quarter", 800, 460),
    ("short", 1180, 250),
    ("portrait", 620, 1040),
    ("half", 1190, 1300),
    ("wide", 1600, 720),
    ("ultrawide", 2560, 700),
];

/// Extra sizes from `PATCH_SHOT_SIZES` ("800x300,800x360"), named by their
/// dimensions, for sweeping the breakpoints between the named sizes.
fn extra_sizes() -> Vec<(String, u32, u32)> {
    let Ok(v) = std::env::var("PATCH_SHOT_SIZES") else {
        return vec![];
    };
    v.split(',')
        .filter_map(|s| {
            let (w, h) = s.trim().split_once('x')?;
            Some((s.trim().to_string(), w.parse().ok()?, h.parse().ok()?))
        })
        .collect()
}

pub struct Shooter {
    pub scale: f32,
}

impl Shooter {
    /// Render one window at `w`×`h` logical px.
    pub fn render<W: SurfaceWindow>(&self, window: &W, w: u32, h: u32) -> RgbaImage {
        self.render_window(window.window(), w, h)
    }

    pub fn render_component(&self, c: &impl slint::ComponentHandle, w: u32, h: u32) -> RgbaImage {
        self.render_window(c.window(), w, h)
    }

    /// Milliseconds for each of `n` full repaints at `w`×`h` (a fresh buffer
    /// every frame), for comparing what surfaces and styles cost to draw.
    pub fn time_component(&self, c: &impl slint::ComponentHandle, w: u32, h: u32, n: usize) -> Vec<f64> {
        let win = c.window();
        let adapter = LAST
            .with(|l| l.borrow().clone())
            .expect("window created on shot platform");
        let pw = (w as f32 * self.scale).round() as u32;
        let ph = (h as f32 * self.scale).round() as u32;
        win.set_size(PhysicalSize::new(pw, ph));
        let _ = win.show();
        let mut buf = vec![Rgb8Pixel::new(0, 0, 0); (pw * ph) as usize];
        let mut out = Vec::with_capacity(n);
        for _ in 0..n.max(1) {
            let at = std::time::Instant::now();
            slint::platform::update_timers_and_animations();
            adapter.request_redraw();
            adapter.draw_if_needed(|r| {
                r.render(&mut buf, pw as usize);
            });
            out.push(at.elapsed().as_secs_f64() * 1e3);
        }
        let _ = win.hide();
        out
    }

    fn render_window(&self, win: &slint::Window, w: u32, h: u32) -> RgbaImage {
        let adapter = LAST
            .with(|l| l.borrow().clone())
            .expect("window created on shot platform");
        win.dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: self.scale,
        });
        let pw = (w as f32 * self.scale).round() as u32;
        let ph = (h as f32 * self.scale).round() as u32;
        win.set_size(PhysicalSize::new(pw, ph));
        let _ = win.show();
        let mut buf = vec![Rgb8Pixel::new(0, 0, 0); (pw * ph) as usize];
        for _ in 0..3 {
            slint::platform::update_timers_and_animations();
            adapter.request_redraw();
            adapter.draw_if_needed(|r| {
                r.render(&mut buf, pw as usize);
            });
        }
        // Type after the window has had its first frames, as the controller's
        // Setup-tab shots do. Headless windows don't receive the desktop's
        // activation event; focus-scoped shortcuts need it explicitly.
        if let Ok(keys) = std::env::var("PATCH_SHOT_KEYS") {
            win.dispatch_event(WindowEvent::WindowActiveChanged(true));
            for c in keys.chars() {
                let text: slint::SharedString = c.to_string().into();
                win.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                win.dispatch_event(WindowEvent::KeyReleased { text });
            }
            slint::platform::update_timers_and_animations();
            adapter.request_redraw();
            adapter.draw_if_needed(|r| {
                r.render(&mut buf, pw as usize);
            });
        }
        let _ = win.hide();
        let mut img = RgbaImage::new(pw, ph);
        for (i, p) in buf.iter().enumerate() {
            img.put_pixel(i as u32 % pw, i as u32 / pw, Rgba([p.r, p.g, p.b, 255]));
        }
        img
    }

    /// Create + bind a window for `surface` as the session would, then render.
    pub fn surface<U: HostedApp>(
        &self,
        app: &Rc<U>,
        surface: &str,
        open: &[String],
        layout: Option<&str>,
        w: u32,
        h: u32,
    ) -> RgbaImage {
        let spec = U::spec();
        let win = app.create(surface);
        win.apply_theme(&patch_ui::load_palette());
        // `PATCH_SHOT_STYLE=patch|pro|classic`: shoot a surface in that
        // style (the default is Classic, whatever the user chose).
        if let Some(style) = std::env::var("PATCH_SHOT_STYLE")
            .ok()
            .and_then(|s| patch_ui::style::Style::parse(&s))
        {
            win.set_style(style);
        }
        win.apply_host(&HostState::build(&**app, &spec, surface, open, layout));
        let img = self.render(&win, w, h);
        drop(win);
        img
    }

    /// Composite one workspace (`ws`, 0-based) of a layout recipe into a
    /// workspace-sized image.
    pub fn workspace<U: HostedApp>(
        &self,
        app: &Rc<U>,
        recipe: &LayoutRecipe,
        ws: u8,
        w: u32,
        h: u32,
    ) -> RgbaImage {
        // Omarchy defaults: gaps_out 10, gaps_in 5, border 2, rounding 0.
        let (gap_out, gap_in, border) = (10.0, 5.0, 2.0);
        let palette = patch_ui::load_palette();
        let s = self.scale;
        let mut canvas = RgbaImage::from_pixel(
            (w as f32 * s) as u32,
            (h as f32 * s) as u32,
            Rgba([8, 8, 8, 255]),
        );
        let open: Vec<String> = recipe.surfaces().map(String::from).collect();
        let inner_w = w as f32 - 2.0 * gap_out;
        let inner_h = h as f32 - 2.0 * gap_out;
        for (i, t) in recipe.tiles.iter().filter(|t| t.ws == ws).enumerate() {
            let x0 = gap_out + t.x * inner_w + if t.x > 0.0 { gap_in } else { 0.0 };
            let y0 = gap_out + t.y * inner_h + if t.y > 0.0 { gap_in } else { 0.0 };
            let x1 = gap_out + (t.x + t.w) * inner_w - if t.x + t.w < 0.999 { gap_in } else { 0.0 };
            let y1 = gap_out + (t.y + t.h) * inner_h - if t.y + t.h < 0.999 { gap_in } else { 0.0 };
            let (cw, ch) = (
                (x1 - x0 - 2.0 * border) as u32,
                (y1 - y0 - 2.0 * border) as u32,
            );
            // An extra instance ("meters#2") is its base surface, as live.
            let img = self.surface(
                app,
                patch_core::instance::base(t.surface),
                &open,
                Some(recipe.id),
                cw,
                ch,
            );
            let color = if i == 0 {
                palette.accent
            } else {
                slint::Color::from_argb_u8(0xaa, 0x59, 0x59, 0x59)
            };
            let a = color.alpha() as f32 / 255.0;
            let bc = |bg: u8, c: u8| (bg as f32 * (1.0 - a) + c as f32 * a) as u8;
            let border_px = Rgba([
                bc(8, color.red()),
                bc(8, color.green()),
                bc(8, color.blue()),
                255,
            ]);
            let (bx0, by0) = ((x0 * s) as u32, (y0 * s) as u32);
            let (bx1, by1) = ((x1 * s) as u32, (y1 * s) as u32);
            for y in by0..by1.min(canvas.height()) {
                for x in bx0..bx1.min(canvas.width()) {
                    canvas.put_pixel(x, y, border_px);
                }
            }
            image::imageops::replace(
                &mut canvas,
                &img,
                ((x0 + border) * s) as i64,
                ((y0 + border) * s) as i64,
            );
        }
        canvas
    }
}

/// Lay several images out in a labelled grid (no text: order = SIZES order).
pub fn contact_sheet(images: &[RgbaImage], max_row_w: u32) -> RgbaImage {
    let pad = 24;
    let mut rows: Vec<Vec<&RgbaImage>> = vec![vec![]];
    let mut row_w = pad;
    for img in images {
        if row_w + img.width() + pad > max_row_w && !rows.last().unwrap().is_empty() {
            rows.push(vec![]);
            row_w = pad;
        }
        rows.last_mut().unwrap().push(img);
        row_w += img.width() + pad;
    }
    let width = rows
        .iter()
        .map(|r| r.iter().map(|i| i.width() + pad).sum::<u32>() + pad)
        .max()
        .unwrap_or(0);
    let height = rows
        .iter()
        .map(|r| r.iter().map(|i| i.height()).max().unwrap_or(0) + pad)
        .sum::<u32>()
        + pad;
    let mut sheet = RgbaImage::from_pixel(width, height, Rgba([40, 40, 40, 255]));
    let mut y = pad;
    for r in rows {
        let mut x = pad;
        for img in &r {
            image::imageops::replace(&mut sheet, *img, x as i64, y as i64);
            x += img.width() + pad;
        }
        y += r.iter().map(|i| i.height()).max().unwrap_or(0) + pad;
    }
    sheet
}

/// Render every surface of a Unit at every size, the controller, and every
/// layout recipe as a workspace composite.
pub fn unit_shots<U: HostedApp>(
    out: &Path,
    scale: f32,
    only_surface: Option<&str>,
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let spec = U::spec();
    let app = U::new();
    let dir = out.join(spec.id.slug());
    std::fs::create_dir_all(&dir)?;
    let shooter = Shooter { scale };
    let mut written = vec![];
    let default_layout = spec.layouts.first().map(|l| l.id);
    let open: Vec<String> = spec
        .layouts
        .first()
        .map(|l| l.surfaces().map(String::from).collect())
        .unwrap_or_default();

    let mut surfaces: Vec<&str> = spec.openable().map(|s| s.id).collect();
    surfaces.push(CONTROLLER);
    for surface in surfaces {
        if only_surface.is_some_and(|o| o != surface) {
            continue;
        }
        let mut sheet = vec![];
        let mut sizes: Vec<(String, u32, u32)> = if surface == CONTROLLER {
            vec![
                ("default".into(), 700, 480),
                ("compact".into(), 420, 520),
                ("short".into(), 700, 330),
            ]
        } else {
            SIZES
                .iter()
                .map(|&(n, w, h)| (n.to_string(), w, h))
                .collect()
        };
        sizes.extend(extra_sizes());
        for (name, w, h) in sizes {
            let img = shooter.surface(&app, surface, &open, default_layout, w, h);
            let path = dir.join(format!("{surface}-{name}.png"));
            img.save(&path)?;
            written.push(path);
            sheet.push(img);
        }
        let path = dir.join(format!("_sheet-{surface}.png"));
        contact_sheet(&sheet, (3000.0 * scale) as u32).save(&path)?;
        written.push(path);
    }
    if only_surface.is_none() {
        for recipe in &spec.layouts {
            for ws in 0..recipe.workspaces() as u8 {
                let img = shooter.workspace(&app, recipe, ws, 2400, 1326);
                let name = if ws == 0 {
                    format!("layout-{}.png", recipe.id)
                } else {
                    format!("layout-{}-{}.png", recipe.id, ws + 1)
                };
                let path = dir.join(name);
                img.save(&path)?;
                written.push(path);
            }
        }
    }
    Ok(written)
}
