//! Reads the active Omarchy theme so every Unit follows `omarchy-theme-set`.

use serde::Deserialize;
use slint::Color;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Palette {
    pub name: String,
    pub bg: Color,
    pub bg_deep: Color,
    pub bg_deeper: Color,
    pub raised: Color,
    pub hover: Color,
    pub selection: Color,
    pub border: Color,
    pub border_strong: Color,
    pub text: Color,
    pub text_bright: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub accent_ink: Color,
    pub mock: Color,
    pub warn: Color,
    pub error: Color,
    /// Meter levels and their warning zone (the Patch style's Meter green and
    /// Meter amber: functional only, never decoration).
    pub level: Color,
    pub level_warn: Color,
    /// Code (the Tidal Unit's editor): the theme's own terminal colours,
    /// eased toward the text so code reads calm: words of the language,
    /// controls, strings, numbers.
    pub code_word: Color,
    pub code_control: Color,
    pub code_string: Color,
    pub code_number: Color,
    pub font: String,
    pub font_size: f32,
}

#[derive(Deserialize, Default)]
struct ColorsToml {
    mode: Option<String>,
    accent: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
    background: Option<String>,
    dark_background: Option<String>,
    darker_background: Option<String>,
    foreground: Option<String>,
    light_foreground: Option<String>,
    red: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    cyan: Option<String>,
    green: Option<String>,
    orange: Option<String>,
}

/// A theme's own `patch.toml`: every Patch colour role exactly, for a
/// theme made for Patch (Patch Light and Dark). Roles left out are
/// derived from `colors.toml`, as for any other theme.
#[derive(Deserialize, Default)]
struct PatchToml {
    bg: Option<String>,
    bg_deep: Option<String>,
    bg_deeper: Option<String>,
    raised: Option<String>,
    hover: Option<String>,
    selection: Option<String>,
    border: Option<String>,
    border_strong: Option<String>,
    text: Option<String>,
    text_bright: Option<String>,
    text_dim: Option<String>,
    text_faint: Option<String>,
    accent: Option<String>,
    accent_soft: Option<String>,
    accent_ink: Option<String>,
    mock: Option<String>,
    warn: Option<String>,
    error: Option<String>,
    level: Option<String>,
    level_warn: Option<String>,
}

#[derive(Deserialize, Default)]
struct ShellToml {
    font: Option<ShellFont>,
}

#[derive(Deserialize, Default)]
struct ShellFont {
    #[serde(rename = "base-size")]
    base_size: Option<f32>,
}

/// `#rrggbb`, or `#rrggbbaa` with alpha.
fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h.get(..6)?, 16).ok()?;
    let rgb = Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8);
    match h.len() {
        8 => Some(rgb.with_alpha(u8::from_str_radix(&h[6..8], 16).ok()? as f32 / 255.0)),
        _ => Some(rgb),
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::from_rgb_u8(
        l(a.red(), b.red()),
        l(a.green(), b.green()),
        l(a.blue(), b.blue()),
    )
}

fn luminance(c: Color) -> f32 {
    let channel = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.red()) + 0.7152 * channel(c.green()) + 0.0722 * channel(c.blue())
}

fn contrasting_ink(bg: Color) -> Color {
    let black = Color::from_rgb_u8(0, 0, 0);
    let white = Color::from_rgb_u8(255, 255, 255);
    let l = luminance(bg);
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.05 {
        white
    } else {
        black
    }
}

fn hue(c: Color) -> Option<f32> {
    let (r, g, b) = (
        c.red() as f32 / 255.0,
        c.green() as f32 / 255.0,
        c.blue() as f32 / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d < 0.12 {
        return None; // too grey to carry meaning
    }
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    Some(if h < 0.0 { h + 360.0 } else { h })
}

/// Keep a theme's semantic color only if it actually looks like that meaning
/// (some themes remap "yellow" to red for a monochrome look).
fn semantic(value: Option<Color>, hue_range: (f32, f32), fallback: Color) -> Color {
    match value.and_then(|c| hue(c).map(|h| (c, h))) {
        Some((c, h)) if in_hue(h, hue_range) => c,
        _ => fallback,
    }
}

fn in_hue(h: f32, (a, b): (f32, f32)) -> bool {
    if a <= b {
        h >= a && h <= b
    } else {
        h >= a || h <= b
    }
}

fn theme_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("omarchy/current/theme"))
}

fn read_toml<T: for<'de> Deserialize<'de> + Default>(path: Option<PathBuf>) -> T {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn omarchy_font() -> Option<String> {
    let out = std::process::Command::new("omarchy-font-current")
        .output()
        .ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Load the active Omarchy palette, falling back to matte-black.
/// `UNIT_THEME=default` forces the built-in palette (used for screenshots).
pub fn load_palette() -> Palette {
    let forced_default = std::env::var("UNIT_THEME").is_ok_and(|v| v == "default");
    // UNIT_THEME_DIR points at any Omarchy theme directory (for previews/tests).
    let dir = if forced_default {
        None
    } else {
        std::env::var_os("UNIT_THEME_DIR")
            .map(PathBuf::from)
            .or_else(theme_dir)
    };
    let colors: ColorsToml = read_toml(dir.as_ref().map(|d| d.join("colors.toml")));
    let exact: PatchToml = read_toml(dir.as_ref().map(|d| d.join("patch.toml")));
    let shell: ShellToml = if forced_default {
        ShellToml::default()
    } else {
        read_toml(
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/omarchy/shell.toml")),
        )
    };
    let name = if forced_default {
        "matte-black (built-in)".to_string()
    } else {
        dir.as_ref()
            .and_then(|d| {
                std::fs::read_to_string(d.parent()?.join("theme.name"))
                    .ok()
                    .or_else(|| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            })
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "matte-black (built-in)".into())
    };

    let c = |v: &Option<String>, d: &str| {
        v.as_deref()
            .and_then(parse_hex)
            .unwrap_or_else(|| parse_hex(d).unwrap())
    };
    let light = colors.mode.as_deref() == Some("light");
    let bg = c(&colors.background, "#121212");
    let fg = c(&colors.foreground, "#bebebe");
    let accent = c(&colors.accent, "#e68e0d");
    let black = Color::from_rgb_u8(0, 0, 0);
    let white = Color::from_rgb_u8(255, 255, 255);
    // "Bright" text moves away from the background, whichever way that is.
    let extreme = if light { black } else { white };
    let recede = if light {
        mix(bg, black, 0.04)
    } else {
        mix(bg, black, 0.25)
    };
    let recede_more = if light {
        mix(bg, black, 0.08)
    } else {
        mix(bg, black, 0.45)
    };
    // Some themes set light_foreground == foreground; then derive a real dim.
    let dim = colors
        .light_foreground
        .as_deref()
        .and_then(parse_hex)
        .filter(|d| *d != fg)
        .unwrap_or_else(|| mix(fg, bg, 0.3));

    // Code colours: the theme's terminal colours, eased toward the text.
    let code = |v: Option<&String>, d: &str| {
        let c = v.and_then(|s| parse_hex(s)).unwrap_or_else(|| parse_hex(d).unwrap());
        mix(c, fg, 0.18)
    };
    let derived = Palette {
        name,
        bg,
        bg_deep: c(&colors.dark_background, &hex(recede)),
        bg_deeper: c(&colors.darker_background, &hex(recede_more)),
        raised: mix(bg, fg, 0.035),
        hover: mix(bg, fg, 0.06),
        selection: c(&colors.selection, &hex(mix(bg, fg, 0.1))),
        border: mix(bg, fg, if light { 0.1 } else { 0.075 }),
        border_strong: c(&colors.muted, &hex(mix(bg, fg, 0.16))),
        text: fg,
        text_bright: mix(fg, extreme, 0.45),
        // Light theme keeps every text role on the same dark ink. Hierarchy
        // comes from size and weight there; tinting text toward grey-green
        // made small labels and captions hard to read on the paper surface.
        text_dim: if light { fg } else { dim },
        text_faint: if light { fg } else { mix(fg, bg, 0.52) },
        accent,
        accent_soft: accent.with_alpha(0.16),
        accent_ink: contrasting_ink(accent),
        mock: if hue(accent).is_some() { accent } else { fg },
        warn: semantic(
            colors.yellow.as_deref().and_then(parse_hex),
            (30.0, 65.0),
            Color::from_rgb_u8(0xc9, 0xa2, 0x3a),
        ),
        error: semantic(
            colors.red.as_deref().and_then(parse_hex),
            (340.0, 15.0),
            Color::from_rgb_u8(0xd3, 0x5f, 0x5f),
        ),
        // Meter green and amber, darker on a light ground so they read on its wells.
        level: if light { Color::from_rgb_u8(0x0b, 0x7f, 0x6a) } else { Color::from_rgb_u8(0x22, 0xc7, 0xa0) },
        level_warn: if light { Color::from_rgb_u8(0x9a, 0x64, 0x00) } else { Color::from_rgb_u8(0xff, 0xb0, 0x20) },
        code_word: code(colors.blue.as_ref(), "#7c8ae0"),
        code_control: code(colors.cyan.as_ref(), "#4fb3bd"),
        code_string: code(colors.green.as_ref(), "#7fb36f"),
        code_number: code(colors.orange.as_ref().or(colors.yellow.as_ref()), "#e07a45"),
        font: if forced_default { None } else { omarchy_font() }
            .unwrap_or_else(|| "JetBrainsMono Nerd Font".into()),
        font_size: shell
            .font
            .and_then(|f| f.base_size)
            .map(|pt| (pt * 1.2).round())
            .unwrap_or(13.0),
    };
    exact.over(derived)
}

impl PatchToml {
    /// The theme's exact roles over the derived ones.
    fn over(self, d: Palette) -> Palette {
        let pick = |v: Option<String>, fallback: Color| {
            v.as_deref().and_then(parse_hex).unwrap_or(fallback)
        };
        Palette {
            bg: pick(self.bg, d.bg),
            bg_deep: pick(self.bg_deep, d.bg_deep),
            bg_deeper: pick(self.bg_deeper, d.bg_deeper),
            raised: pick(self.raised, d.raised),
            hover: pick(self.hover, d.hover),
            selection: pick(self.selection, d.selection),
            border: pick(self.border, d.border),
            border_strong: pick(self.border_strong, d.border_strong),
            text: pick(self.text, d.text),
            text_bright: pick(self.text_bright, d.text_bright),
            text_dim: pick(self.text_dim, d.text_dim),
            text_faint: pick(self.text_faint, d.text_faint),
            accent: pick(self.accent, d.accent),
            accent_soft: pick(self.accent_soft, d.accent_soft),
            accent_ink: pick(self.accent_ink, d.accent_ink),
            mock: pick(self.mock, d.mock),
            warn: pick(self.warn, d.warn),
            error: pick(self.error, d.error),
            level: pick(self.level, d.level),
            level_warn: pick(self.level_warn, d.level_warn),
            ..d
        }
    }
}

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

/// Changes whenever the active Omarchy theme changes (`omarchy-theme-set`
/// rewrites the files in the current theme directory).
pub fn theme_stamp() -> Option<std::time::SystemTime> {
    let dir = std::env::var_os("UNIT_THEME_DIR")
        .map(PathBuf::from)
        .or_else(theme_dir)?;
    let colors = std::fs::metadata(dir.join("colors.toml"))
        .and_then(|m| m.modified())
        .ok();
    let exact = std::fs::metadata(dir.join("patch.toml"))
        .and_then(|m| m.modified())
        .ok();
    let colors = colors.max(exact);
    let name = dir.parent().and_then(|p| {
        std::fs::metadata(p.join("theme.name"))
            .and_then(|m| m.modified())
            .ok()
    });
    colors.max(name)
}

#[cfg(test)]
mod patch_theme_tests {
    use super::*;

    #[test]
    fn the_patch_themes_set_every_role_exactly() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        for name in ["patch-light", "patch-dark"] {
            let text = std::fs::read_to_string(root.join(name).join("patch.toml")).unwrap();
            let t: PatchToml = toml::from_str(&text).unwrap();
            let all = [
                &t.bg,
                &t.bg_deep,
                &t.bg_deeper,
                &t.raised,
                &t.hover,
                &t.selection,
                &t.border,
                &t.border_strong,
                &t.text,
                &t.text_bright,
                &t.text_dim,
                &t.text_faint,
                &t.accent,
                &t.accent_soft,
                &t.accent_ink,
                &t.mock,
                &t.warn,
                &t.error,
                &t.level,
                &t.level_warn,
            ];
            assert!(
                all.iter()
                    .all(|v| v.as_deref().and_then(parse_hex).is_some()),
                "{name}: every role, as a colour"
            );
            let soft = parse_hex(t.accent_soft.as_deref().unwrap()).unwrap();
            assert!(
                (soft.alpha() as f32 / 255.0 - 0.16).abs() < 0.01,
                "{name}: the soft accent keeps its alpha"
            );
        }
        assert_eq!(
            parse_hex("#b5bab6"),
            Some(Color::from_rgb_u8(0xb5, 0xba, 0xb6))
        );
    }
}
