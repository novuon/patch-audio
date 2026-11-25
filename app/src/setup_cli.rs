//! `omarchy-patch setup`: how Patch is set up here, and changing it. Every
//! change is saved (`~/.config/omarchy-patch/setup.toml`) and applied at
//! once: the service, the bar menu, the DJ launcher, the themes; the engine
//! applies system audio and the librarian follows the DJ Booth itself.

use anyhow::bail;
use patch_core::setup::{self, DefaultLayout, MODULES, Profile, SWITCHES, Setup};
use std::path::PathBuf;

pub fn run(args: &[&str]) -> anyhow::Result<()> {
    match args {
        ["stems", "install"] => install_separator(),
        [] | ["show"] => show(&setup::load()),
        ["apply"] => apply(&setup::load(), None),
        // The controller saved a change: apply these switches.
        ["apply", ids @ ..] => {
            let now = setup::load_saved();
            let mut before = now;
            for id in ids {
                if let Some(on) = now.get(id) {
                    before.set_only(id, !on);
                }
            }
            apply(&now, Some(before))
        }
        ["layout"] => {
            println!("{}", setup::load().layout.id());
            Ok(())
        }
        ["layout", id] => {
            let Some(l) = DefaultLayout::from_id(id) else {
                bail!("layouts: {}", DefaultLayout::ALL.map(|l| l.id()).join(", "));
            };
            let mut s = setup::load_saved();
            if !s.set_layout(l) {
                bail!("{} needs {} (switched off here)", l.id(), l.needs().join(", "));
            }
            setup::save(&s)?;
            launchers(&s)?;
            println!("✓ the Patch launcher and `omarchy-patch layout` open {}", l.id());
            Ok(())
        }
        ["profile", id] => {
            let Some(p) = Profile::from_id(id) else {
                bail!("profiles: {}", Profile::ALL.map(|p| p.id()).join(", "));
            };
            change(|s| *s = p.apply(s))
        }
        [id, on @ ("on" | "off")] => {
            if Setup::default().get(id).is_none() {
                bail!("switches: {}", SWITCHES.map(|s| s.0).join(", "));
            }
            change(|s| {
                s.set(id, *on == "on");
            })
        }
        // install.sh: the choices from its screen, all at once; --all
        // applies every switch (a reinstall redoes the service and bar).
        ["set", pairs @ ..] => {
            let all = pairs.first() == Some(&"--all");
            let mut s = setup::load_saved();
            let mut layout = None;
            for p in pairs.iter().filter(|p| p.contains('=')) {
                if let Some((id, v)) = p.split_once('=') {
                    if id == "layout" {
                        layout = Some(DefaultLayout::from_id(v).ok_or_else(|| {
                            anyhow::anyhow!("layouts: {}", DefaultLayout::ALL.map(|l| l.id()).join(", "))
                        })?);
                        continue;
                    }
                    if s.get(id).is_none() {
                        bail!("switches: {}", SWITCHES.map(|s| s.0).join(", "));
                    }
                    s.set_only(id, v == "on" || v == "true" || v == "1");
                }
            }
            // What the switches can't mean together (the AI DJ without
            // the decks), then the default layout once the modules are set.
            s = s.normalized();
            if let Some(l) = layout
                && !s.set_layout(l)
            {
                bail!("the {} layout needs {} (switched off here)", l.id(), l.needs().join(", "));
            }
            // The rule still holds after all of them.
            let (sa, ss) = (s.system_audio, s.start_with_session);
            s.set("system_audio", sa && ss);
            let before = (!all && setup::exists()).then(setup::load);
            setup::save(&s)?;
            apply(&s, before)
        }
        _ => bail!(
            "setup [show] | profile standard|dj|podcast|live_coding|full|minimal | <switch> on|off | layout [<id>] | set <switch>=on|off… [layout=<id>] | stems install | apply"
        ),
    }
}

/// Install the shared separator backend and wait for the first-run setup to
/// finish. Stem audio itself remains demand-driven per track.
fn install_separator() -> anyhow::Result<()> {
    use std::time::Duration;
    let exe = std::env::current_exe()?;
    patch_engine::client::ensure_running(&exe).map_err(anyhow::Error::msg)?;
    let call = |action| {
        patch_engine::client::call(&serde_json::json!({"op":"stems","deck":"a","action":action}))
            .map_err(anyhow::Error::msg)
    };
    let status = call("status")?;
    if status["backend_ready"] == true {
        println!("Stem separator is already installed");
        return Ok(());
    }
    call("setup")?;
    println!("Installing the stem separator runtime and model (this downloads several GB)…");
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let status = call("status")?;
        if status["backend_ready"] == true {
            println!("Stem separator ready");
            return Ok(());
        }
        let state = &status["setup"];
        if state["state"] == "failed" {
            bail!(
                "Stem separator setup failed: {}",
                state["error"]
                    .as_str()
                    .unwrap_or("see the Patch stem setup log")
            );
        }
        let progress = state["progress"].as_u64().unwrap_or(0);
        let line = format!("Installing separator… {progress}%");
        if line != last {
            println!("{line}");
            last = line;
        }
    }
}

fn show(s: &Setup) -> anyhow::Result<()> {
    println!(
        "Patch setup: {}",
        // The profile as chosen: a module waiting for its key (Auto Mix
        // in DJ) doesn't turn the choice into "custom".
        setup::load_saved().profile().or(s.profile()).map_or("custom", |p| p.name())
    );
    println!("  ● Core               Patch replaces Omarchy's audio: Mixer, Meter, Inspector, Device");
    for (id, name, about) in SWITCHES {
        if id == "system_audio" {
            println!();
        }
        let on = s.get(id).unwrap_or(false);
        println!("  {} {name:<20} {about}", if on { "●" } else { "○" });
    }
    println!("\n  Default layout: {} (the Patch launchers, `omarchy-patch layout`)", s.layout.id());
    println!(
        "\nChange it in the controller (Ctrl+K), or: omarchy-patch setup profile standard|dj|podcast|live_coding|full · setup <switch> on|off · setup layout <id> · omarchy-patch modules"
    );
    Ok(())
}

/// Turn these switches on or off together and apply what changed
/// (`omarchy-patch modules` adds and removes modules through it).
pub fn switch(pairs: &[(&str, bool)]) -> anyhow::Result<()> {
    change(|s| {
        for (id, on) in pairs {
            s.set(id, *on);
        }
    })
}

fn change(f: impl FnOnce(&mut Setup)) -> anyhow::Result<()> {
    let before = setup::exists().then(setup::load_saved);
    let mut s = setup::load_saved();
    f(&mut s);
    setup::save(&s)?;
    apply(&s, before)
}

/// Apply `s`; with the setup before the change, only what changed.
pub fn apply(s: &Setup, before: Option<Setup>) -> anyhow::Result<()> {
    let changed = |id: &str| before.is_none_or(|b| b.get(id) != s.get(id));
    let say = |what: &str, r: anyhow::Result<()>| match r {
        Ok(()) => println!("✓ {what}"),
        Err(e) => println!("✗ {what}: {e}"),
    };
    if changed("start_with_session") {
        let r = crate::service(if s.start_with_session {
            "install"
        } else {
            "uninstall"
        });
        say(
            if s.start_with_session {
                "starts with your session"
            } else {
                "doesn't start with your session"
            },
            r,
        );
    }
    if changed("bar") {
        let r = if s.bar {
            crate::bar::install()
        } else {
            crate::bar::uninstall()
        };
        say(
            if s.bar {
                "in the bar's audio menu"
            } else {
                "out of the bar's audio menu"
            },
            r,
        );
    }
    if changed("focus_border") {
        patch_host::hypr::set_focus_border(s.focus_border);
        println!(
            "✓ {}",
            if s.focus_border {
                "the focused Patch window's border lights up in the Patch themes too"
            } else {
                "in the Patch themes, Patch window borders stay quiet when focused"
            }
        );
    }
    if changed("show_track_path") {
        println!(
            "✓ {}",
            if s.show_track_path {
                "decks show the track's file path"
            } else {
                "decks show the title and artist"
            }
        );
    }
    if changed("track_map") {
        println!(
            "✓ {}",
            if s.track_map {
                "decks name the track's parts under the waveform"
            } else {
                "decks show minutes under the waveform"
            }
        );
    }
    if changed("mix_spots") {
        println!(
            "✓ {}",
            if s.mix_spots {
                "the track map marks the best parts to mix over"
            } else {
                "the track map marks no mix spots"
            }
        );
    }
    for (id, name, _) in MODULES {
        if changed(id) {
            let on = s.get(id) == Some(true);
            if on && !patch_core::licence::module_unlocked(id) {
                println!("✓ {name} chosen: it starts with your key (omarchy-patch licence activate <key>)");
            } else {
                println!("✓ {name} {}", if on { "on" } else { "off" });
            }
        }
    }
    if before.is_some() && changed("dj") && !s.dj {
        println!("  the librarian stops; your library and its analysis stay");
    }
    // The launchers follow the modules and the default layout.
    say("app launchers", launchers(s));
    // With the themes on, every setup (an install, an update) checks the
    // links: a theme installed from GitHub since is replaced by Patch's.
    if changed("themes") || s.themes {
        say(
            if s.themes {
                "Patch Light and Patch Dark in the theme menu"
            } else {
                "Patch themes out of the theme menu"
            },
            themes(s.themes),
        );
    }
    if changed("system_audio") {
        say(
            if s.system_audio {
                "system audio: every app plays through Patch"
            } else {
                "system audio: apps play straight to your output"
            },
            system_audio(s.system_audio),
        );
    }
    Ok(())
}

fn applications() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("applications")
}

/// One app per layout in the setup, for the Omarchy launcher: (layout,
/// name, what it opens, icon). "Patch" itself (the controller) is the
/// package's entry; the default layout's app says it is the default.
const APPS: [(DefaultLayout, &str, &str, &str); 8] = [
    (DefaultLayout::Simple, "Patch Audio", "The Mixer, the Router and the Player", "multimedia-volume-control"),
    (DefaultLayout::BasicDj, "Patch DJ", "Two decks, the crossfader and the library", "media-optical-audio"),
    (DefaultLayout::AiDj, "Patch AI DJ", "The DJ Booth with the AI DJ, the library and the meter", "media-playlist-shuffle"),
    (DefaultLayout::DjStudio, "Patch DJ Studio", "The booth, a mixing desk (Router, Inspector, EQ, Mixer) and the library (three workspaces)", "audio-x-generic"),
    (DefaultLayout::PerformanceDj, "Patch Performance DJ", "The booth, a control workspace and the Visualiser (three workspaces)", "video-display"),
    (DefaultLayout::Podcast, "Patch Podcast", "Mic, Mixer, meter, Player, Recorder and Broadcast", "audio-input-microphone"),
    (DefaultLayout::LiveCode, "Patch Live Code", "Tidal with the Mixer and meter, and the Visualiser (two workspaces)", "utilities-terminal"),
    (DefaultLayout::Pro, "Patch Pro", "Mixing and engineering: EQ, meter and Mixer · Router, Analyzer, Recorder and plugins (two workspaces)", "preferences-system"),
];

fn app_file(l: DefaultLayout) -> String {
    format!("omarchy-patch-{}.desktop", l.id())
}

/// The app launchers for this setup: one per layout its modules allow,
/// none for the others (an older install's DJ entry goes too).
pub fn launchers(s: &Setup) -> anyhow::Result<()> {
    let dir = applications();
    std::fs::create_dir_all(&dir)?;
    let _ = std::fs::remove_file(dir.join("omarchy-patch-dj.desktop"));
    for (l, name, about, icon) in APPS {
        let path = dir.join(app_file(l));
        if !s.offers(l) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let default = if s.layout == l { " (your default)" } else { "" };
        std::fs::write(
            &path,
            format!(
                "[Desktop Entry]\nType=Application\nName={name}\nGenericName=Patch layout\nComment={about}{default}; opens on the next empty workspace\nExec=omarchy-patch layout {}\nIcon={icon}\nTerminal=false\nCategories=AudioVideo;Audio;\nKeywords=patch;audio;{}\n",
                l.id(),
                l.id().replace('-', ";")
            ),
        )?;
    }
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .output();
    Ok(())
}

/// The short guide install.sh shows at the end (`omarchy-patch welcome`):
/// the controller first, the bar's output choice, the default layout,
/// then what each module needs to get going.
pub fn welcome(s: &Setup, color: bool) -> String {
    let (b, d, r) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
    let app = APPS.iter().find(|a| a.0 == s.layout).map_or("Patch Audio", |a| a.1);
    let ws = patch_surfaces::spec()
        .layout(s.layout.id())
        .map_or(1, |l| l.workspaces());
    let mut out = format!("\n{b}Patch is set up: it is now your system audio.{r}\n\n");
    let mut n = 0;
    let mut step = |out: &mut String, title: &str, body: &str| {
        n += 1;
        out.push_str(&format!("{b}{n}  {title}{r}\n"));
        for line in body.lines() {
            out.push_str(&format!("   {line}\n"));
        }
        out.push('\n');
    };
    step(
        &mut out,
        "Ctrl+K: the controller",
        "In any Patch window, Ctrl+K opens the controller: every Unit, the\nlayouts and Setup (switch modules on or off). Without a window open:\nrun `omarchy-patch`, or open \"Patch\" from the Omarchy menu (Super+Space).",
    );
    step(
        &mut out,
        "The bar's audio menu: the main output",
        "Click the volume icon in the bar. Its Patch section sits above OUTPUT.\nPatch is selected there: every app gets its own strip and the Mixer\nis the full console (tone, sends, AUX 1/2 and the FX returns),\nplaying to your sound device. Pick a device under OUTPUT instead for\nthe simple Mixer. Patch remembers your choice.",
    );
    step(
        &mut out,
        &format!(
            "Your layout: {}",
            patch_surfaces::spec().layout(s.layout.id()).map_or(s.layout.id(), |l| l.name)
        ),
        &format!(
            "Open \"{app}\" from the app launcher, or run `omarchy-patch layout`. It\nopens on the next empty workspace{}; closing its windows never\nstops the music. Nothing opens by itself: at login only the audio\nservice starts. Other layouts: Ctrl+K, Layouts, or the bar's Patch section.",
            if ws > 1 { format!(" ({ws} workspaces)") } else { String::new() }
        ),
    );
    if s.dj {
        step(&mut out, "DJ: add your music", "omarchy-patch library add ~/Music\nThe librarian finds and analyses the tracks in the background\n(tempo, key, grid); the library shows what is ready.");
    }
    if s.ai_dj {
        step(&mut out, "Auto Mix", "In the DJ Booth, start a set from a collection: Auto Mix plans each\ntransition and you steer (pause, next, approve). `omarchy-patch licence`\nshows your key.");
    }
    // Chosen, waiting for its key.
    if !s.ai_dj && setup::load_saved().ai_dj {
        let c = patch_core::modules::builtin();
        if let Some(l) = patch_core::licence::for_module(c, "ai_dj") {
            let (title, what) = l.welcome();
            step(&mut out, &title, &what);
        }
    }
    if s.podcast {
        step(&mut out, "Podcast & broadcast", "Choose your microphone in the Mic Unit (it measures and profiles it),\nrecord in the Recorder, and go live in Broadcast (Icecast, RTMP or a file).");
    }
    if s.eq && s.studio {
        step(&mut out, "Pro and developer mode", "\"Patch Pro\" is a desk for mixing and engineering: the EQ, meter and Mixer\non one workspace, the Router, Analyzer, Recorder and plugins on the next.\nDeveloper mode (Ctrl+K, Setup) adds a Diagnostics window to every workspace\nof any layout you open.");
    }
    step(&mut out, "More later", "Add or remove modules any time: `omarchy-patch modules` lists them\n(the DJ decks, Studio tools, the Visualiser, themes and wallpapers, …),\n`omarchy-patch modules add <id>` adds one. Big downloads run in the\nbackground and Patch tells you when they are ready.");
    if s.controllers {
        step(&mut out, "Controllers", "Plug in a MIDI or HID controller: the MIDI & HID Unit shows it and its mapping.");
    }
    out.push_str(&format!(
        "{d}Show this again: omarchy-patch welcome · Change the setup: omarchy-patch setup{r}\n"
    ));
    out
}

/// Where the Patch themes are: the package's, or next to this build (a
/// checkout's target/release).
fn themes_source() -> Option<PathBuf> {
    let package = PathBuf::from("/usr/share/omarchy-patch/themes");
    if package.join("patch-light").is_dir() {
        return Some(package);
    }
    // Downloaded by `omarchy-patch modules add themes`.
    let downloaded = patch_core::module_state::data_dir().join("themes");
    if downloaded.join("patch-light/colors.toml").is_file() {
        return Some(downloaded);
    }
    let exe = crate::bar::invoked()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .or_else(|| std::env::current_exe().ok())?;
    // install.sh's copy (PREFIX/bin → PREFIX/share/omarchy-patch/themes),
    // or a checkout's target/release.
    let prefix = exe
        .parent()
        .and_then(|b| b.parent())
        .map(|p| p.join("share/omarchy-patch/themes"));
    prefix
        .into_iter()
        .chain(exe.ancestors().map(|a| a.join("themes")))
        .find(|t| t.join("patch-light/colors.toml").is_file())
}

/// Omarchy applies a theme from a copy (`~/.local/state/omarchy/current/
/// theme`): its wallpaper picker and the intros read that copy, not the
/// theme's folder. Files added to a Patch theme in use (the Wallpapers
/// download, which lands minutes after the install applied the theme) only
/// show once the theme is staged again. 2026-11-25, a clean install: the
/// wallpapers and intros were "installed" but missing from the picker.
/// The wallpaper on screen stays, unless it was one just removed.
pub fn restage_theme() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let current = home.join(".local/state/omarchy/current");
    let active = std::fs::read_to_string(current.join("theme.name")).unwrap_or_default();
    let active = active.trim();
    if !active.starts_with("patch-") {
        return;
    }
    let set = |keep_background: bool| {
        let mut cmd = std::process::Command::new("omarchy-theme-set");
        cmd.arg(active);
        if keep_background {
            cmd.env("OMARCHY_THEME_SKIP_BACKGROUND", "1");
        }
        let _ = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    set(true);
    // The link points at a wallpaper that went: Omarchy picks one that is there.
    let link = current.join("background");
    if link.symlink_metadata().is_ok() && !link.exists() {
        set(false);
    }
}

fn themes(on: bool) -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let dest = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("omarchy/themes");
    let kept = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("omarchy-patch/themes-replaced");
    let src = if on {
        let Some(src) = themes_source() else {
            bail!("the Patch themes aren't next to this build")
        };
        Some(src)
    } else {
        None
    };
    let changed = link_themes(src.as_deref(), &dest, &kept)?;
    // A theme in use is staged by Omarchy: stage it again, so the link's
    // window borders, terminal colours and patch.toml apply now. The
    // wallpaper stays.
    let current = home.join(".local/state/omarchy/current/theme.name");
    let active = std::fs::read_to_string(current).unwrap_or_default();
    if changed.iter().any(|n| *n == active.trim()) {
        restage_theme();
    }
    // The theme in use went (removed, no public copy to fall back on):
    // Omarchy back on its default theme, not on one missing from its menu.
    let active = active.trim();
    if !on && active.starts_with("patch-") && !dest.join(active).exists() {
        let _ = std::process::Command::new("omarchy-theme-set")
            .arg("tokyo-night")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(())
}

/// The public, theme-only Patch themes (omarchy-patch-dark-theme and
/// omarchy-patch-light-theme on GitHub) installed with `omarchy-theme-install`:
/// a git clone whose remote names the theme's repository.
fn public_clone(dir: &std::path::Path, name: &str) -> bool {
    !dir.is_symlink()
        && std::fs::read_to_string(dir.join(".git/config"))
            .is_ok_and(|c| c.contains(&format!("omarchy-{name}-theme")))
}

/// Points Omarchy's patch-dark and patch-light at Patch's full themes (`src`),
/// or takes them away (`None`). A public clone of the theme is moved to `kept`
/// and replaced by the link, which adds what Omarchy leaves out of a git
/// theme; taking the link away puts the clone back. Any other folder of that
/// name is the user's and stays. Returns the themes it changed.
fn link_themes(
    src: Option<&std::path::Path>,
    dest: &std::path::Path,
    kept: &std::path::Path,
) -> anyhow::Result<Vec<&'static str>> {
    let mut changed = Vec::new();
    for n in ["patch-light", "patch-dark"] {
        let link = dest.join(n);
        let saved = kept.join(n);
        if let Some(src) = src {
            let target = src.join(n);
            if link.is_symlink() && std::fs::read_link(&link).is_ok_and(|t| t == target) {
                continue;
            }
            if public_clone(&link, n) {
                std::fs::create_dir_all(kept)?;
                let _ = std::fs::remove_dir_all(&saved);
                std::fs::rename(&link, &saved)?;
            } else if link.exists() && !link.is_symlink() {
                bail!(
                    "{} is a copy of the theme, not Patch's link: left as it is",
                    link.display()
                );
            }
            std::fs::create_dir_all(dest)?;
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&target, &link)?;
            changed.push(n);
        } else if link.is_symlink() {
            std::fs::remove_file(&link)?;
            if saved.is_dir() {
                std::fs::rename(&saved, &link)?;
            }
            changed.push(n);
        }
    }
    Ok(changed)
}

/// System audio is the engine's (it keeps the default output and the apps
/// on Patch); here it only starts, or the default output goes back to the
/// device when no engine runs to do it.
fn system_audio(on: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    if on {
        patch_engine::client::ensure_running(&exe).map_err(anyhow::Error::msg)?;
        let _ = patch_engine::client::call(&serde_json::json!({"op": "setup"}));
        return Ok(());
    }
    if patch_engine::client::is_running() {
        // The engine puts the device back: now, not on its next slow pass.
        let _ = patch_engine::client::call(&serde_json::json!({"op": "setup"}));
        return Ok(());
    }
    let pactl = |args: &[&str]| {
        std::process::Command::new("pactl")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let device = std::fs::read_to_string(patch_core::ipc::runtime_dir().join("output-device"))
        .ok()
        .map(|s| s.trim().to_string());
    if pactl(&["get-default-sink"]).as_deref() == Some("patch")
        && let Some(d) = device.filter(|d| !d.is_empty())
    {
        pactl(&["set-default-sink", &d]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::link_themes;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("patch-themes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn clone(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git/config"),
            format!("[remote \"origin\"]\n\turl = https://github.com/novuon/omarchy-{name}-theme\n"),
        )
        .unwrap();
    }

    #[test]
    fn a_public_clone_gives_way_to_the_link_and_comes_back() {
        let root = scratch("clone");
        let (src, dest, kept) = (root.join("src"), root.join("themes"), root.join("kept"));
        for n in ["patch-light", "patch-dark"] {
            std::fs::create_dir_all(src.join(n)).unwrap();
        }
        clone(&dest.join("patch-dark"), "patch-dark");

        assert_eq!(link_themes(Some(&src), &dest, &kept).unwrap(), ["patch-light", "patch-dark"]);
        assert_eq!(std::fs::read_link(dest.join("patch-dark")).unwrap(), src.join("patch-dark"));
        assert!(kept.join("patch-dark/.git/config").is_file());
        // Already linked: nothing to do.
        assert!(link_themes(Some(&src), &dest, &kept).unwrap().is_empty());

        link_themes(None, &dest, &kept).unwrap();
        assert!(!dest.join("patch-dark").is_symlink());
        assert!(dest.join("patch-dark/.git/config").is_file());
        assert!(!dest.join("patch-light").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn any_other_folder_of_that_name_stays() {
        let root = scratch("other");
        let (src, dest, kept) = (root.join("src"), root.join("themes"), root.join("kept"));
        std::fs::create_dir_all(src.join("patch-dark")).unwrap();
        std::fs::create_dir_all(dest.join("patch-dark")).unwrap();
        std::fs::write(dest.join("patch-dark/colors.toml"), "").unwrap();

        assert!(link_themes(Some(&src), &dest, &kept).is_err());
        assert!(dest.join("patch-dark/colors.toml").is_file());
        assert!(!kept.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
