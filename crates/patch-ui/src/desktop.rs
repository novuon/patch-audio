//! Outside Omarchy: Patch's own themes, following the desktop.
//!
//! On Omarchy every Unit follows the Omarchy theme (`omarchy.rs`). Any other
//! desktop has no Omarchy theme, so the Units wear Patch Dark or Patch
//! Light, as the desktop's light/dark setting says (the XDG portal's
//! `org.freedesktop.appearance color-scheme`: GNOME, KDE Plasma, Cinnamon,
//! COSMIC, …), or as `~/.config/omarchy-patch/appearance` says (`dark`,
//! `light`, or `auto` for the desktop's).
//!
//! The portal is read once, on a background thread that then waits for its
//! change signal: no polling, and the UI never waits on D-Bus for more than
//! a moment at start. Nothing here runs on Omarchy.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Once, mpsc};
use std::time::Duration;

pub const DARK_COLORS: &str = include_str!("../../../themes/patch-dark/colors.toml");
pub const DARK_EXACT: &str = include_str!("../../../themes/patch-dark/patch.toml");
pub const LIGHT_COLORS: &str = include_str!("../../../themes/patch-light/colors.toml");
pub const LIGHT_EXACT: &str = include_str!("../../../themes/patch-light/patch.toml");

/// The portal's answer: 0 no preference (or no portal), 1 dark, 2 light.
static SCHEME: AtomicU8 = AtomicU8::new(0);
static START: Once = Once::new();

/// What the user chose in `~/.config/omarchy-patch/appearance`.
fn chosen() -> Option<bool> {
    match std::fs::read_to_string(appearance_file()?).ok()?.trim() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

fn appearance_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("omarchy-patch/appearance"))
}

/// Dark unless the user or the desktop asks for light (Patch is a dark
/// studio tool first, as Omarchy's default theme is).
pub fn dark() -> bool {
    if let Some(d) = chosen() {
        return d;
    }
    follow();
    SCHEME.load(Ordering::Relaxed) != 2
}

/// Changes when the user's choice or the desktop's setting changes: the
/// session's theme watch then reapplies the palette.
pub fn stamp() -> Option<std::time::SystemTime> {
    let file = appearance_file()
        .and_then(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
        .unwrap_or(std::time::UNIX_EPOCH);
    Some(file + Duration::from_nanos(u64::from(SCHEME.load(Ordering::Relaxed))))
}

/// Starts the portal thread once; waits up to 150 ms for its first answer,
/// so a light desktop's first window opens light, not dark then light.
fn follow() {
    START.call_once(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("patch-appearance".into())
            .spawn(move || {
                let Ok(conn) = zbus::blocking::Connection::session() else {
                    let _ = tx.send(());
                    return;
                };
                let Ok(proxy) = zbus::blocking::Proxy::new(
                    &conn,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Settings",
                ) else {
                    let _ = tx.send(());
                    return;
                };
                if let Some(v) = read(&proxy) {
                    SCHEME.store(v, Ordering::Relaxed);
                }
                let _ = tx.send(());
                let Ok(changes) = proxy.receive_signal("SettingChanged") else {
                    return;
                };
                for msg in changes {
                    let Ok((ns, key, value)) = msg
                        .body()
                        .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
                    else {
                        continue;
                    };
                    if ns == "org.freedesktop.appearance"
                        && key == "color-scheme"
                        && let Some(v) = scheme(&value)
                    {
                        SCHEME.store(v, Ordering::Relaxed);
                    }
                }
            });
        let _ = rx.recv_timeout(Duration::from_millis(150));
    });
}

fn read(proxy: &zbus::blocking::Proxy) -> Option<u8> {
    let ns = "org.freedesktop.appearance";
    // ReadOne (portal version 2) gives the value; the older Read wraps it
    // in one more variant.
    let value: zbus::zvariant::OwnedValue = proxy
        .call("ReadOne", &(ns, "color-scheme"))
        .or_else(|_| proxy.call("Read", &(ns, "color-scheme")))
        .ok()?;
    scheme(&value)
}

fn scheme(value: &zbus::zvariant::OwnedValue) -> Option<u8> {
    use zbus::zvariant::Value;
    let mut v: &Value = value;
    while let Value::Value(inner) = v {
        v = inner;
    }
    match v {
        Value::U32(n) => u8::try_from(*n).ok().filter(|n| *n <= 2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portal_value_is_read_through_its_variants() {
        use zbus::zvariant::{OwnedValue, Value};
        let plain = OwnedValue::from(1u32);
        assert_eq!(scheme(&plain), Some(1));
        let wrapped = OwnedValue::try_from(Value::Value(Box::new(Value::U32(2)))).unwrap();
        assert_eq!(scheme(&wrapped), Some(2));
        assert_eq!(scheme(&OwnedValue::from(7u32)), None);
    }

    #[test]
    fn the_embedded_themes_are_the_patch_themes() {
        assert!(DARK_COLORS.contains("mode = \"dark\""));
        assert!(LIGHT_COLORS.contains("mode = \"light\""));
        assert!(DARK_EXACT.contains("bg = ") && LIGHT_EXACT.contains("bg = "));
    }
}
