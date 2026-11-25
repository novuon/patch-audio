//! The desk's header strip: the layout's name, its pages and the Units of
//! the page on screen, as the Mac desk's header has them
//! (`patch-host/src/macos/workspace.rs` on `dev/macos`).
//!
//! Drawn as SVG with resvg (Slint's own SVG renderer, already in the
//! binary), only when something on it changes: an idle desk draws nothing.

use patch_core::desk::Rect;
use patch_ui::Palette;
use resvg::{tiny_skia, usvg};
use std::sync::Arc;

/// The strip's height (logical px).
pub const HEIGHT: f32 = 40.0;

/// What a click on the strip does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Page(usize),
    /// Show this Unit alone on its page, or the page again.
    Zoom(&'static str),
    /// Put the page back as the layout has it.
    Reset,
}

/// What the strip shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Model {
    /// The layout's name ("" while none is open).
    pub layout: String,
    /// Each page's label.
    pub pages: Vec<String>,
    pub page: usize,
    /// The page's tiled Units: id and name.
    pub units: Vec<(&'static str, String)>,
    pub zoom: Option<&'static str>,
    /// Dividers moved or a Unit zoomed: Reset shows.
    pub edited: bool,
    /// "Opening <layout>" while its windows map.
    pub opening: bool,
}

pub struct Header {
    fonts: Arc<usvg::fontdb::Database>,
    /// The clickable parts of the last strip drawn.
    pub hits: Vec<(Rect, Action)>,
}

fn hex(c: slint::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

fn opacity(c: slint::Color) -> f32 {
    c.alpha() as f32 / 255.0
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl Header {
    pub fn new() -> Self {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Header { fonts: Arc::new(db), hits: Vec::new() }
    }

    fn options(&self, p: &Palette) -> usvg::Options<'static> {
        let mut o = usvg::Options { fontdb: self.fonts.clone(), ..Default::default() };
        o.font_family = p.font.clone();
        o
    }

    /// The width of each label at `size` px (`weight` 400 or 700).
    fn measure(&self, p: &Palette, labels: &[(&str, f32, u16)]) -> Vec<f32> {
        let mut svg = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="100">"#);
        for (i, (text, size, weight)) in labels.iter().enumerate() {
            svg += &format!(
                r#"<text id="m{i}" x="0" y="{}" font-family="{}" font-size="{size}" font-weight="{weight}">{}</text>"#,
                50 + i,
                esc(&p.font),
                esc(text)
            );
        }
        svg += "</svg>";
        let Ok(tree) = usvg::Tree::from_str(&svg, &self.options(p)) else {
            return labels.iter().map(|(t, s, _)| t.chars().count() as f32 * s * 0.6).collect();
        };
        (0..labels.len())
            .map(|i| {
                tree.node_by_id(&format!("m{i}"))
                    .map(|n| n.abs_bounding_box().width())
                    .unwrap_or(labels[i].0.chars().count() as f32 * labels[i].1 * 0.6)
            })
            .collect()
    }

    /// The strip `width` logical px wide at `scale`, as RGBA premultiplied
    /// pixels (physical size), with its clickable parts in `hits`.
    pub fn draw(&mut self, m: &Model, p: &Palette, width: f32, scale: f64, hover: Option<&Action>) -> Option<(Vec<u8>, u32, u32)> {
        let fs = (p.font_size).clamp(11.0, 15.0);
        let small = fs - 1.0;
        let mark = "PATCH";
        let title = if m.opening && !m.layout.is_empty() {
            format!("Opening {}", m.layout)
        } else {
            m.layout.clone()
        };
        let mut labels: Vec<(&str, f32, u16)> = vec![(mark, fs, 700), (&title, fs, 400)];
        for page in &m.pages {
            labels.push((page, small, 400));
        }
        for (_, name) in &m.units {
            labels.push((name, small, 400));
        }
        let reset = "Reset";
        labels.push((reset, small, 400));
        let widths = self.measure(p, &labels);

        let h = HEIGHT;
        let mid = h / 2.0;
        let pill_h = 26.0;
        let pill_y = mid - pill_h / 2.0;
        let pad = 10.0;
        let mut svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{h}" viewBox="0 0 {width} {h}">"#
        );
        svg += &format!(r#"<rect width="{width}" height="{h}" fill="{}"/>"#, hex(p.bg_deep));
        svg += &format!(
            r#"<rect y="{}" width="{width}" height="1" fill="{}" fill-opacity="{}"/>"#,
            h - 1.0,
            hex(p.border),
            opacity(p.border)
        );
        let font = esc(&p.font);
        let text = |svg: &mut String, x: f32, s: &str, size: f32, weight: u16, color: slint::Color| {
            *svg += &format!(
                r#"<text x="{x}" y="{}" font-family="{font}" font-size="{size}" font-weight="{weight}" fill="{}" fill-opacity="{}" dominant-baseline="central">{}</text>"#,
                mid,
                hex(color),
                opacity(color),
                esc(s)
            );
        };
        let mut hits = Vec::new();
        let pill = |svg: &mut String, x: f32, w: f32, label: &str, active: bool, action: Action, hits: &mut Vec<(Rect, Action)>| {
            let r = Rect::new(x, pill_y, w + 2.0 * pad, pill_h);
            let hovered = hover == Some(&action);
            let (fill, ink) = if active {
                (Some(p.accent_soft), p.text_bright)
            } else if hovered {
                (Some(p.hover), p.text_bright)
            } else {
                (None, p.text_dim)
            };
            if let Some(f) = fill {
                *svg += &format!(
                    r#"<rect x="{}" y="{}" width="{}" height="{}" rx="6" fill="{}" fill-opacity="{}"/>"#,
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    hex(f),
                    opacity(f)
                );
            }
            if active {
                *svg += &format!(
                    r#"<rect x="{}" y="{}" width="{}" height="{}" rx="6" fill="none" stroke="{}" stroke-opacity="{}"/>"#,
                    r.x + 0.5,
                    r.y + 0.5,
                    r.w - 1.0,
                    r.h - 1.0,
                    hex(p.accent),
                    opacity(p.accent) * 0.6
                );
            }
            text(svg, x + pad, label, small, 400, ink);
            hits.push((r, action));
            r.x + r.w
        };

        // Left: the mark and the layout, then the pages.
        let mut x = 16.0;
        text(&mut svg, x, mark, fs, 700, p.accent);
        x += widths[0] + 10.0;
        text(&mut svg, x, &title, fs, 400, p.text);
        x += widths[1] + 22.0;
        let mut i = 2;
        if m.pages.len() > 1 {
            for (n, page) in m.pages.iter().enumerate() {
                x = pill(&mut svg, x, widths[i], page, n == m.page && !m.opening, Action::Page(n), &mut hits) + 4.0;
                i += 1;
            }
        } else {
            i += m.pages.len();
        }
        let left_end = x;

        // Right: the page's Units (a click shows one alone), Reset.
        let units_w: f32 = m.units.iter().enumerate().map(|(n, _)| widths[i + n] + 2.0 * pad + 4.0).sum::<f32>()
            + if m.edited { widths[i + m.units.len()] + 2.0 * pad + 12.0 } else { 0.0 };
        let mut x = width - 12.0 - units_w;
        if m.units.len() > 1 && x > left_end + 12.0 && !m.opening {
            for (n, (id, name)) in m.units.iter().enumerate() {
                x = pill(&mut svg, x, widths[i + n], name, m.zoom == Some(*id), Action::Zoom(id), &mut hits) + 4.0;
            }
            if m.edited {
                x += 8.0;
                pill(&mut svg, x, widths[i + m.units.len()], reset, false, Action::Reset, &mut hits);
            }
        }
        svg += "</svg>";

        self.hits = hits;
        let tree = usvg::Tree::from_str(&svg, &self.options(p)).ok()?;
        let (pw, ph) = ((width as f64 * scale).round() as u32, (h as f64 * scale).round() as u32);
        let mut pixmap = tiny_skia::Pixmap::new(pw.max(1), ph.max(1))?;
        resvg::render(&tree, tiny_skia::Transform::from_scale(scale as f32, scale as f32), &mut pixmap.as_mut());
        Some((pixmap.take(), pw.max(1), ph.max(1)))
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<&Action> {
        self.hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a)
    }
}
