//! Unit styles, chosen for all units and overridden per unit:
//! * Classic: Patch's Omarchy look and layouts (the default);
//! * Patch: the same layouts in Patch's own style (docs/patch-style.md:
//!   colour, type, the Module in the header, controls, corners);
//! * Pro gear: the Patch style with layouts after club gear and the
//!   reference designs (the Crossfader's strips, the Mixer's large trims).
//!
//! Stored in
//! `~/.config/omarchy-patch/styles`, one `key value` per line:
//!
//! ```text
//! default classic
//! dj-mixer patch
//! ```
//!
//! Keys are a unit group (see [`group`]); values `classic`, `patch` or
//! `pro` (`modern` and `house`, earlier names, still read as `patch`).

use std::path::PathBuf;

/// The units whose layouts exist in both styles, as Settings lists them:
/// (key, label).
pub const UNITS: [(&str, &str); 5] = [
    ("dj-mixer", "Crossfader"),
    ("decks", "Decks"),
    ("mixer", "Mixer"),
    ("meters", "Meter"),
    ("dj", "DJ Booth"),
];

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch").join("styles"))
}

/// A surface's style key: every deck shares "decks"; other surfaces are
/// their own base id ("meters#2" is "meters").
pub fn group(surface: &str) -> &str {
    let base = surface.split('#').next().unwrap_or(surface);
    if base.starts_with("deck-") { "decks" } else { base }
}

/// One unit style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Classic,
    Patch,
    Pro,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Classic, Style::Patch, Style::Pro];
    pub fn name(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Patch => "patch",
            Style::Pro => "pro",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Style::Classic => "Classic",
            Style::Patch => "Patch",
            Style::Pro => "Pro gear",
        }
    }
    pub fn parse(s: &str) -> Option<Style> {
        // Earlier names of the Patch style; saved files may still say them.
        if s == "modern" || s == "house" {
            return Some(Style::Patch);
        }
        Style::ALL.into_iter().find(|v| v.name() == s)
    }
    /// The Patch look (colour, type, header, controls): Patch and Pro gear.
    pub fn patch_style(self) -> bool {
        self != Style::Classic
    }
    /// The club-gear layouts: Pro gear only.
    pub fn pro(self) -> bool {
        self == Style::Pro
    }
}

/// The saved choices: the default and each unit's override.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Styles {
    pub default: Style,
    /// Use the active theme accent for the Patch mark's plug-in unit.
    pub logo_accent: bool,
    pub overrides: Vec<(String, Style)>,
}

impl Styles {
    pub fn parse(text: &str) -> Styles {
        let mut s = Styles::default();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
            if k == "default" {
                if let Some(style) = Style::parse(v) { s.default = style; }
            } else if k == "logo-accent" {
                s.logo_accent = v == "on";
            } else {
                let Some(style) = Style::parse(v) else { continue };
                s.overrides.retain(|(o, _)| o != k);
                s.overrides.push((k.to_string(), style));
            }
        }
        s
    }

    fn render(&self) -> String {
        let mut out = format!("default {}\nlogo-accent {}\n", self.default.name(), if self.logo_accent { "on" } else { "off" });
        for (k, v) in &self.overrides {
            out.push_str(&format!("{k} {}\n", v.name()));
        }
        out
    }

    /// This unit's own choice, if it overrides the default.
    pub fn unit(&self, key: &str) -> Option<Style> {
        self.overrides.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    /// The style `surface` draws.
    pub fn style(&self, surface: &str) -> Style {
        self.unit(group(surface)).unwrap_or(self.default)
    }

    /// Cycle a unit: as all units → Classic → Patch → Pro gear → as all.
    pub fn cycle_unit(&mut self, key: &str) {
        let next = match self.unit(key) {
            None => Some(Style::Classic),
            Some(Style::Classic) => Some(Style::Patch),
            Some(Style::Patch) => Some(Style::Pro),
            Some(Style::Pro) => None,
        };
        self.overrides.retain(|(k, _)| k != key);
        if let Some(v) = next {
            self.overrides.push((key.to_string(), v));
        }
    }
}

pub fn load() -> Styles {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| Styles::parse(&t))
        .unwrap_or_default()
}

pub fn save(styles: &Styles) {
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, styles.render());
    }
}

/// Changes when the styles file does (for the theme watcher).
pub fn stamp() -> u64 {
    path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_by_default_and_units_override() {
        let s = Styles::parse("");
        assert_eq!(s.style("dj-mixer"), Style::Classic);
        let mut s = Styles::parse("default patch\ndj-mixer pro\nold patch-ish\n");
        assert_eq!(s.style("dj-mixer"), Style::Pro);
        assert_eq!(s.style("mixer"), Style::Patch);
        assert!(s.style("mixer").patch_style() && !s.style("mixer").pro());
        // Saved under an earlier name.
        assert_eq!(Styles::parse("default modern\n").default, Style::Patch);
        assert_eq!(Styles::parse("default house\n").default, Style::Patch);
        s.cycle_unit("decks");
        assert_eq!(s.style("deck-a"), Style::Classic);
        s.cycle_unit("decks");
        s.cycle_unit("decks");
        assert_eq!(s.style("deck-d"), Style::Pro);
        s.cycle_unit("decks");
        assert_eq!(s.unit("decks"), None);
        assert_eq!(Styles::parse(&s.render()), s);
        assert_eq!(group("meters#2"), "meters");
    }
}
