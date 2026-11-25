//! Rust side of the design system.
//!
//! The components themselves live in `ui/*.slint` and are compiled into every
//! Unit (see `patch-build`). Slint globals are per compilation, so each Unit
//! applies the palette with [`apply_theme!`].

mod desktop;
pub mod meter;
mod omarchy;
pub mod style;

pub use omarchy::{Palette, load_palette, theme_stamp};

/// Push a [`Palette`] into a window's `Theme` global.
/// `Theme` must be in scope (the Unit's generated Slint type).
#[macro_export]
macro_rules! apply_theme {
    ($window:expr, $palette:expr) => {{
        use slint::ComponentHandle as _;
        let t = $window.global::<Theme>();
        let p: &$crate::Palette = $palette;
        t.set_bg(p.bg);
        t.set_bg_deep(p.bg_deep);
        t.set_bg_deeper(p.bg_deeper);
        t.set_raised(p.raised);
        t.set_hover(p.hover);
        t.set_selection(p.selection);
        t.set_border(p.border);
        t.set_border_strong(p.border_strong);
        t.set_text(p.text);
        t.set_text_bright(p.text_bright);
        t.set_text_dim(p.text_dim);
        t.set_text_faint(p.text_faint);
        t.set_accent(p.accent);
        t.set_accent_soft(p.accent_soft);
        t.set_accent_ink(p.accent_ink);
        t.set_logo_accent($crate::style::load().logo_accent);
        t.set_mock(p.mock);
        t.set_warn(p.warn);
        t.set_error(p.error);
        t.set_level(p.level);
        t.set_level_warn(p.level_warn);
        t.set_code_word(p.code_word);
        t.set_code_control(p.code_control);
        t.set_code_string(p.code_string);
        t.set_code_number(p.code_number);
        t.set_font(p.font.clone().into());
        t.set_fs(p.font_size);
    }};
}
