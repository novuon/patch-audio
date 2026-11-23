//! A cover over the monitor while a layout recipe builds its windows.
//!
//! Hyprland has no way to arrange windows on a workspace nobody is looking
//! at: splits follow the focused window, so the recipe replay has to visit
//! every workspace it fills. The curtain is a pinned, unfocusable floating
//! window (`hypr::RULES_LUA`), so it stays on top across those workspace
//! switches and takes no clicks or keys away from the replay.
//!
//! While it is up, animations are off and neither the pointer nor the
//! windows follow each other: focus and workspace changes don't warp the
//! pointer (it would flash across the screen) and moving the mouse doesn't
//! move focus (a new window would split the wrong one). All of it comes back
//! when it drops, or from a watchdog thread if the UI stops answering first;
//! the watchdog also moves the curtain out of sight, so it never blocks the
//! screen. "Stops answering" is measured from the replay's last step
//! ([`Curtain::alive`]), not from the start: on a slow machine a big layout
//! takes longer than the watchdog, and restoring the options mid-replay
//! made the pointer jump to every window that opened after it (a tester's
//! report, a 4-core Celeron opening every Unit).

use crate::hypr;
use slint::ComponentHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Hyprland options set while the curtain is up: (section, key, value).
/// Omarchy 4.5 can leave animations off. A value that is already what the
/// curtain wants is not recorded, and only a value this changed is restored.
const OVERRIDES: [(&str, &str, &str); 4] = [
    ("animations", "enabled", "false"),
    ("cursor", "no_warps", "true"),
    ("cursor", "warp_on_change_workspace", "0"),
    ("input", "follow_mouse", "0"),
];

/// Far longer than any one step of a layout takes (a window maps in well
/// under a second, even on a slow machine).
const WATCHDOG: Duration = Duration::from_secs(20);

slint::slint! {
    export component CurtainWindow inherits Window {
        in property <string> window-title;
        in property <string> heading;
        in property <string> detail;
        in property <color> bg;
        in property <color> fg;
        in property <color> dim;
        in property <string> font;
        in property <length> fs;
        title: window-title;
        background: bg;
        default-font-family: font;
        default-font-size: fs;
        VerticalLayout {
            alignment: center;
            spacing: 8px;
            Text { text: heading; color: fg; font-size: fs * 1.6; horizontal-alignment: center; }
            Text { text: detail; color: dim; horizontal-alignment: center; }
        }
    }
}

pub struct Curtain {
    window: CurtainWindow,
    title: String,
    /// Hyprland options changed while this is up, with their old values.
    overrides: Vec<(&'static str, &'static str, String)>,
    /// Set once the options are back: by the drop or by the watchdog.
    done: Arc<AtomicBool>,
    /// When the replay last made progress, in ms after `start`.
    beat: Arc<AtomicU64>,
    start: Instant,
}

impl Curtain {
    pub fn show(title: &str, heading: &str, palette: &patch_ui::Palette) -> Option<Self> {
        let window = CurtainWindow::new().ok()?;
        window.set_window_title(title.into());
        window.set_heading(heading.into());
        window.set_detail("Arranging windows…".into());
        window.set_fg(palette.text_bright);
        window.set_dim(palette.text_dim);
        window.set_font(palette.font.clone().into());
        window.set_fs(palette.font_size);
        window.set_bg(palette.bg);
        window.show().ok()?;
        let overrides: Vec<_> = OVERRIDES
            .into_iter()
            .filter_map(|(section, key, during)| {
                let before = hypr::option(section, key)?;
                (before != during && hypr::set_option(section, key, during))
                    .then_some((section, key, before))
            })
            .collect();
        let done = Arc::new(AtomicBool::new(false));
        let beat = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        let (flag, restore, stuck) = (done.clone(), overrides.clone(), title.to_string());
        let last = beat.clone();
        let _ = std::thread::Builder::new()
            .name("patch-curtain".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    let idle = start.elapsed().as_millis() as u64 - last.load(Ordering::Relaxed);
                    if idle >= WATCHDOG.as_millis() as u64 {
                        break;
                    }
                }
                if !flag.swap(true, Ordering::SeqCst) {
                    eprintln!(
                        "layout: no progress for {} s with the curtain up: moving it away",
                        WATCHDOG.as_secs()
                    );
                    restore_options(&restore);
                    hypr::banish(&stuck);
                }
            });
        Some(Curtain {
            window,
            title: title.to_string(),
            overrides,
            done,
            beat,
            start,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The replay made progress: the watchdog counts from now.
    pub fn alive(&self) {
        self.beat
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

impl Drop for Curtain {
    fn drop(&mut self) {
        let _ = self.window.hide();
        if !self.done.swap(true, Ordering::SeqCst) {
            restore_options(&self.overrides);
        }
    }
}

fn restore_options(overrides: &[(&str, &str, String)]) {
    for (section, key, before) in overrides {
        hypr::set_option(section, key, before);
    }
}
