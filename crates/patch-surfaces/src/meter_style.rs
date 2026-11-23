//! How meters are styled: Classic (Patch's own two-tone meters), Banded
//! (colour bands per meter kind), LED (a ladder of green, amber and red
//! segments) or Patch (the Patch style's Meter green, Amber from −6 dB,
//! red only for clipping). Scales and levels are identical in all four. The default
//! is a setting (saved); clicking a meter's scale tries the next style for
//! the session.

fn path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch").join("meters"))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Classic,
    Banded,
    Led,
    Patch,
}

impl Look {
    pub const ALL: [Look; 4] = [Look::Classic, Look::Banded, Look::Led, Look::Patch];

    /// The next style when a meter scale is clicked.
    pub fn next(self) -> Look {
        match self {
            Look::Classic => Look::Banded,
            Look::Banded => Look::Led,
            Look::Led => Look::Patch,
            Look::Patch => Look::Classic,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Look::Classic => "classic",
            Look::Banded => "banded",
            Look::Led => "led",
            Look::Patch => "patch",
        }
    }

    /// The setting's name and what it shows.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Look::Classic => ("Classic", "Patch's two-tone meters: a quiet body, the accent from −1 dBFS"),
            Look::Banded => ("Banded", "Colour bands per meter kind"),
            Look::Led => ("LED", "A ladder of green, amber and red segments, as on a hi-fi amplifier"),
            Look::Patch => ("Patch", "The Patch style: Meter green levels, Amber from −6 dB, red only for clipping"),
        }
    }

    pub fn parse_name(s: &str) -> Option<Look> {
        Look::ALL.into_iter().find(|l| l.name() == s)
    }

    fn parse(s: &str) -> Look {
        match s.trim() {
            // "1"/"2": a numeric format briefly written by another build.
            "banded" | "1" => Look::Banded,
            "led" | "2" => Look::Led,
            // "house": the Patch style's earlier name.
            "patch" | "house" | "3" => Look::Patch,
            _ => Look::Classic,
        }
    }
}

/// A Classic ladder at a meter's physical size: `n` segments from the
/// bottom on a `pitch`, `gap` pixels apart, `body` below the hot point and
/// `hot` above it, on transparent. Cached by every argument: a meter asks
/// again only when its size, theme or muting changes.
#[allow(clippy::too_many_arguments)]
pub fn ladder(
    w: i32,
    h: i32,
    n: i32,
    pitch: i32,
    gap: i32,
    hot_at: f32,
    body: slint::Color,
    hot: slint::Color,
) -> slint::Image {
    use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
    use std::cell::RefCell;
    use std::collections::HashMap;
    type Key = (i32, i32, i32, i32, i32, u32, [u8; 4], [u8; 4]);
    thread_local! {
        static CACHE: RefCell<HashMap<Key, Image>> = RefCell::new(HashMap::new());
    }
    let rgba = |c: slint::Color| [c.red(), c.green(), c.blue(), c.alpha()];
    let (w, h) = (w.clamp(1, 4096), h.clamp(1, 8192));
    let key = (w, h, n, pitch, gap, hot_at.to_bits(), rgba(body), rgba(hot));
    if let Some(img) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return img;
    }
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    let px = buf.make_mut_slice();
    let n = n.max(1);
    for i in 0..n {
        let c = if (i as f32 + 0.5) / n as f32 > hot_at { hot } else { body };
        let p = Rgba8Pixel::new(c.red(), c.green(), c.blue(), c.alpha());
        let top = h - (i + 1) * pitch;
        for y in top.max(0)..(top + pitch - gap).clamp(0, h) {
            px[(y * w) as usize..((y + 1) * w) as usize].fill(p);
        }
    }
    let img = Image::from_rgba8(buf);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        // Sizes change while windows are dragged: never a growing pile.
        if c.len() > 512 {
            c.clear();
        }
        c.insert(key, img.clone());
    });
    img
}

/// The saved choice (Classic by default).
pub fn load() -> Look {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or(Look::Classic, |s| Look::parse(&s))
}

pub fn save(look: Look) {
    if let Some(p) = path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, format!("{}\n", look.name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_styles_cycle_and_old_choices_still_read() {
        assert_eq!(Look::Classic.next().next().next().next(), Look::Classic);
        assert_eq!(Look::Led.next(), Look::Patch);
        assert_eq!(Look::parse("house"), Look::Patch);
        assert_eq!(Look::parse("patch"), Look::Patch);
        assert_eq!(Look::parse_name("patch"), Some(Look::Patch));
        assert_eq!(Look::parse("banded\n"), Look::Banded);
        assert_eq!(Look::parse("classic"), Look::Classic);
        assert_eq!(Look::parse("2"), Look::Led);
        assert_eq!(Look::parse(Look::Led.name()), Look::Led);
    }
}
