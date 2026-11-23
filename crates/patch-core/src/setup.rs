//! How Patch is set up on this machine: chosen at install, changed later
//! in the controller (Ctrl+K) or with `omarchy-patch setup`. One file,
//! `~/.config/omarchy-patch/setup.toml`, read by the engine, the UI and the
//! CLI; the CLI applies a change (service, bar, launchers, themes).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The parts of Patch a user can switch on or off. The core (system
/// audio, the service, the bar menu and the Mixer, Meter, Inspector and
/// Device Units) is always there; the modules add Units on top of it
/// (`modules.json` describes them for the installer and Settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setup {
    /// Patch is the system's sound: every app plays through Patch's mixer
    /// (Patch is the default output; the output you pick in the bar is the
    /// one Patch plays to). Off: apps play straight to the device, and only
    /// what you route through Patch goes through it.
    pub system_audio: bool,
    /// Module: decks, crossfader, library (the background librarian that
    /// finds and analyses your music), the DJ launcher.
    pub dj: bool,
    /// Module (needs `dj`): the DJ Booth with the AI DJ and stem controls
    /// (the stem separator is a separate download).
    pub ai_dj: bool,
    /// Module: the Visualiser.
    pub visualiser: bool,
    /// Module: podcast and broadcast (Mic, Broadcast).
    pub podcast: bool,
    /// Module: live coding (Tidal, Pattern, Sequencer).
    pub live_coding: bool,
    /// Module: the Plugins Unit (native VST3 hosting).
    pub plugins: bool,
    /// Module: MIDI and HID controllers.
    pub controllers: bool,
    /// Module: the EQ (master, apps, mics, headphone correction).
    pub eq: bool,
    /// Module: the Mic Unit (pre-amp, noise, voice). Podcast shows it too.
    pub mic: bool,
    /// Module: Studio tools (Router, Analyzer, Recorder). Podcast shows the
    /// Recorder too.
    pub studio: bool,
    /// Module (preview): the Player. Podcast shows it too.
    pub player: bool,
    /// The Patch service starts with your session (the engine, so audio and
    /// decks run without any window open).
    pub start_with_session: bool,
    /// Patch in the Omarchy bar's audio menu.
    pub bar: bool,
    /// The Patch Light and Patch Dark themes in Omarchy's theme menu.
    pub themes: bool,
    /// Decks keep lossless tracks as 32-bit float (their full resolution).
    /// Off (the default): every track is kept as 16-bit, half the memory;
    /// its noise (−93 dBFS, dithered) is far below hearing in a mix. A
    /// preference, not part of a profile. Applies to the next track loaded.
    pub hq_decks: bool,
    /// In Patch's own themes (Patch Dark, Patch Light), the focused Patch
    /// window's border lights up as Omarchy's does. Off (the default): it
    /// keeps the inactive colour; focus itself still follows the pointer.
    /// Every other theme draws Patch windows as Omarchy draws any window.
    /// A preference, not part of a profile.
    pub focus_border: bool,
    /// Decks show the file path under the title and artist. Off (the
    /// default): a deck shows the title and artist only. A preference,
    /// not part of a profile.
    pub show_track_path: bool,
    /// The decks' overview names the track's parts under the waveform
    /// (intro, main, break, build, drop, outro; a solid line where the kick
    /// plays, dotted where it is out) and counts down the part playing. On
    /// by default. A preference, not part of a profile.
    pub track_map: bool,
    /// The track map marks the three parts best to mix a beat over (a
    /// steady kick, a grid that holds, little voice). On by default. A
    /// preference, not part of a profile.
    pub mix_spots: bool,
    /// Patch may look up facts about the library's tracks online (release
    /// years at MusicBrainz), sending a track's artist and title. Off until
    /// the user allows it when the Library asks. A preference.
    pub online_lookups: bool,
    /// The Library asked about `online_lookups` (yes or no): it asks once.
    pub online_lookups_asked: bool,
    /// The Library says so the first time in a run that its pre-listen
    /// moves to AUX 1 (a deck or the Player plays). On by default. A
    /// preference.
    pub preview_aux_notice: bool,
    /// Developer mode: every layout opens with a Diagnostics window on
    /// each of its workspaces (engine timing, clock, latency, workers). A
    /// preference, not part of a profile.
    pub dev_mode: bool,
    /// The layout the "Patch" launchers and `omarchy-patch layout` open.
    /// Chosen at install; nothing opens it by itself (at login only the
    /// audio service starts). A preference, not part of a profile.
    pub layout: DefaultLayout,
}

impl Default for Setup {
    fn default() -> Self {
        Profile::Full.setup()
    }
}

/// The built-in layouts one can make the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultLayout {
    #[default]
    Simple,
    BasicDj,
    AiDj,
    PerformanceDj,
    /// The booth, the mixing desk and the library (three workspaces).
    DjStudio,
    Podcast,
    LiveCode,
    /// Mixing and engineering on two workspaces (the core Units, and the
    /// plugin rack with the Plugins module).
    Pro,
}

impl DefaultLayout {
    pub const ALL: [DefaultLayout; 8] = [
        DefaultLayout::Simple,
        DefaultLayout::BasicDj,
        DefaultLayout::AiDj,
        DefaultLayout::PerformanceDj,
        DefaultLayout::DjStudio,
        DefaultLayout::Podcast,
        DefaultLayout::LiveCode,
        DefaultLayout::Pro,
    ];

    /// The layout's id (as `omarchy-patch layout <id>` takes it).
    pub fn id(self) -> &'static str {
        match self {
            DefaultLayout::Simple => "simple",
            DefaultLayout::BasicDj => "basic-dj",
            DefaultLayout::AiDj => "ai-dj",
            DefaultLayout::PerformanceDj => "performance-dj",
            DefaultLayout::DjStudio => "dj-studio",
            DefaultLayout::Podcast => "podcast",
            DefaultLayout::LiveCode => "live-code",
            DefaultLayout::Pro => "pro",
        }
    }

    pub fn from_id(id: &str) -> Option<DefaultLayout> {
        let id = crate::layout::canonical(id);
        DefaultLayout::ALL.into_iter().find(|l| l.id() == id)
    }

    /// The modules the layout's windows need. Simple needs none: without
    /// the EQ the Mixer takes its row (`arrange`).
    pub fn needs(self) -> &'static [&'static str] {
        match self {
            DefaultLayout::Simple => &[],
            DefaultLayout::Pro => &["eq", "studio"],
            DefaultLayout::BasicDj => &["dj"],
            DefaultLayout::AiDj => &["dj", "ai_dj"],
            DefaultLayout::PerformanceDj => &["dj", "ai_dj", "visualiser", "eq"],
            DefaultLayout::DjStudio => &["dj", "ai_dj", "visualiser", "eq", "studio"],
            DefaultLayout::Podcast => &["podcast"],
            DefaultLayout::LiveCode => &["live_coding", "visualiser"],
        }
    }
}

/// The modules with a switch: (id, name, what it adds). The core is always
/// installed. `modules.json` has them all (and the download-only ones).
pub const MODULES: [(&str, &str, &str); 11] = [
    ("eq", "EQ", "Equaliser for the master, apps and mics, with headphone correction (AutoEq)"),
    ("mic", "Mic", "Mic pre-amp, noise reduction and voice processing, for calls and recording"),
    ("studio", "Studio tools", "Router, Analyzer and Recorder, for routing and engineering"),
    ("player", "Player", "A cart player for jingles and beds (preview, being built for podcasts)"),
    ("dj", "DJ", "Decks, crossfader, Mini Booth and the library (a background librarian analyses your music)"),
    ("ai_dj", "Auto Mix", "The DJ Booth with Auto Mix: it orders and crossfades a set from your library (needs DJ)"),
    ("visualiser", "Visualiser", "Visuals to the music, for a screen or a projector"),
    ("podcast", "Podcast & broadcast", "Mic processing and streaming to Icecast, RTMP or a file"),
    ("live_coding", "Live coding", "Tidal, Pattern and the Sequencer"),
    ("plugins", "Plugins", "Native VST3 plugins on the master, a channel, AUX or FX"),
    ("controllers", "Controllers", "MIDI and HID controllers (a udev rule gives Patch raw HID access)"),
];

/// The module a surface belongs to (by its base id); None: the core.
pub fn surface_module(surface: &str) -> Option<&'static str> {
    surface_modules(surface).first().copied()
}

/// The modules that show a surface (any one of them on is enough), its
/// own module first; empty: the core.
pub fn surface_modules(surface: &str) -> &'static [&'static str] {
    match crate::instance::base(surface) {
        "deck-a" | "deck-b" | "deck-c" | "deck-d" | "dj-mixer" | "dj-club" | "dj-mini" | "library" => &["dj"],
        "dj" => &["ai_dj"],
        // A Unit's other windows go with it: the Visualiser's controls and
        // the Broadcast Unit's Performer.
        "visualiser" | "visualiser-controls" => &["visualiser"],
        "broadcast" | "performer" => &["podcast"],
        // Podcast brings the mic, the Player and the Recorder with it.
        "mic" => &["mic", "podcast"],
        "player" => &["player", "podcast"],
        "recorder" => &["studio", "podcast"],
        "graph" | "analyzer" => &["studio"],
        "eq" => &["eq"],
        "tidal" | "pattern" | "sequencer" => &["live_coding"],
        "plugins" | "plugin" => &["plugins"],
        "controllers" => &["controllers"],
        _ => &[],
    }
}

/// Ready-made setups. Their modules and default layout are in
/// `modules.json` (Mixer, an older one, is defined here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Standard,
    Dj,
    Podcast,
    LiveCoding,
    Full,
    Minimal,
    /// An older profile, still taken by `--profile` and `setup profile`.
    Mixer,
}

impl Profile {
    /// The profiles the installer and the controller offer (anything
    /// else is a custom setup).
    pub const ALL: [Profile; 5] = [Profile::Standard, Profile::Dj, Profile::Podcast, Profile::LiveCoding, Profile::Full];
    const KNOWN: [Profile; 7] = [
        Profile::Standard,
        Profile::Dj,
        Profile::Podcast,
        Profile::LiveCoding,
        Profile::Full,
        Profile::Minimal,
        Profile::Mixer,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Profile::Standard => "standard",
            Profile::Dj => "dj",
            Profile::Podcast => "podcast",
            Profile::LiveCoding => "live_coding",
            Profile::Full => "full",
            Profile::Minimal => "minimal",
            Profile::Mixer => "mixer",
        }
    }

    fn def(self) -> Option<&'static crate::modules::ProfileDef> {
        crate::modules::builtin().profile(self.id())
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Mixer => "Mixer",
            p => p.def().map_or("", |d| d.name.as_str()),
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Profile::Mixer => "Patch replaces Omarchy's audio, with every module but the DJ ones",
            p => p.def().map_or("", |d| d.about.as_str()),
        }
    }

    pub fn from_id(id: &str) -> Option<Profile> {
        let id = if id == "live-coding" { "live_coding" } else { id };
        Profile::KNOWN.into_iter().find(|p| p.id() == id)
    }

    /// This profile, keeping `current`'s preferences (`hq_decks`,
    /// `focus_border`, `show_track_path`, the default layout while the
    /// profile still has it).
    pub fn apply(self, current: &Setup) -> Setup {
        let mut s = Setup {
            hq_decks: current.hq_decks,
            focus_border: current.focus_border,
            show_track_path: current.show_track_path,
            track_map: current.track_map,
            mix_spots: current.mix_spots,
            online_lookups: current.online_lookups,
            online_lookups_asked: current.online_lookups_asked,
            preview_aux_notice: current.preview_aux_notice,
            dev_mode: current.dev_mode,
            layout: current.layout,
            ..self.setup()
        };
        if !s.offers(s.layout) {
            s.layout = self.setup().layout;
        }
        s
    }

    pub fn setup(self) -> Setup {
        // Every profile replaces Omarchy's audio: system audio, the
        // service from login, the bar's audio menu. They differ in the
        // modules that come with it.
        let core = Setup {
            system_audio: true,
            dj: false,
            ai_dj: false,
            visualiser: false,
            podcast: false,
            live_coding: false,
            plugins: false,
            controllers: false,
            eq: false,
            mic: false,
            studio: false,
            player: false,
            start_with_session: true,
            bar: true,
            themes: false,
            hq_decks: false,
            focus_border: false,
            show_track_path: false,
            track_map: true,
            mix_spots: true,
            online_lookups: false,
            online_lookups_asked: false,
            preview_aux_notice: true,
            dev_mode: false,
            layout: DefaultLayout::Simple,
        };
        if self == Profile::Mixer {
            let mut s = Profile::Full.setup();
            s.set("dj", false);
            s.layout = DefaultLayout::Simple;
            return s;
        }
        let mut s = core;
        if let Some(d) = self.def() {
            for m in &d.modules {
                s.set(m, true);
            }
            s.layout = DefaultLayout::from_id(&d.layout).unwrap_or_default();
        }
        s.keep_layout();
        s
    }
}

/// One switch, for menus: (id, name, what it does). The modules, then the
/// rest of the setup, then preferences.
pub const SWITCHES: [(&str, &str, &str); 23] = [
    MODULES[0],
    MODULES[1],
    MODULES[2],
    MODULES[3],
    MODULES[4],
    MODULES[5],
    MODULES[6],
    MODULES[7],
    MODULES[8],
    MODULES[9],
    MODULES[10],
    (
        "system_audio",
        "System audio",
        "Every app plays through Patch; pick the output in the bar",
    ),
    (
        "start_with_session",
        "Start with session",
        "The Patch service runs from login, windows or not",
    ),
    ("bar", "Bar menu", "Patch in the Omarchy bar's audio menu"),
    (
        "themes",
        "Themes",
        "Patch Light and Patch Dark in Omarchy's theme menu",
    ),
    (
        "hq_decks",
        "High-quality decks",
        "Lossless tracks kept as 32-bit float: twice the memory (off: 16-bit, inaudible)",
    ),
    (
        "focus_border",
        "Highlight focused window",
        "In the Patch themes, the focused Patch window's border lights up as in Omarchy (off: it stays quiet; focus still follows the pointer). Other themes: always as Omarchy",
    ),
    (
        "show_track_path",
        "Show track path",
        "Decks show the file path under the title and artist (off: title and artist only)",
    ),
    (
        "track_map",
        "Track map",
        "Under each deck's waveform: the track's parts, solid where the kick plays, and bars left in the part playing",
    ),
    (
        "mix_spots",
        "Mix spots",
        "The track map marks the three parts best to mix a beat over",
    ),
    (
        "online_lookups",
        "Look up release years and genres online",
        "The library asks MusicBrainz (musicbrainz.org) for each track's original release year and, where its tag names none, its genre, sending its artist and title, one request a second (off: only what your files' own tags say)",
    ),
    (
        "preview_aux_notice",
        "Say when pre-listen moves to AUX 1",
        "While a deck or the Player plays, the Library's pre-listen plays on AUX 1, off main: say so the first time each run",
    ),
    (
        "dev_mode",
        "Developer mode",
        "Every layout opens with a Diagnostics window on each workspace (engine timing, clock, latency)",
    ),
];

impl Setup {
    pub fn get(&self, id: &str) -> Option<bool> {
        Some(match id {
            "system_audio" => self.system_audio,
            "dj" => self.dj,
            "ai_dj" => self.ai_dj,
            "visualiser" => self.visualiser,
            "podcast" => self.podcast,
            "live_coding" => self.live_coding,
            "plugins" => self.plugins,
            "controllers" => self.controllers,
            "eq" => self.eq,
            "mic" => self.mic,
            "studio" => self.studio,
            "player" => self.player,
            "start_with_session" => self.start_with_session,
            "bar" => self.bar,
            "themes" => self.themes,
            "hq_decks" => self.hq_decks,
            "focus_border" => self.focus_border,
            "show_track_path" => self.show_track_path,
            "track_map" => self.track_map,
            "mix_spots" => self.mix_spots,
            "online_lookups" => self.online_lookups,
            "preview_aux_notice" => self.preview_aux_notice,
            "dev_mode" => self.dev_mode,
            _ => return None,
        })
    }

    pub fn set(&mut self, id: &str, on: bool) -> bool {
        let slot = match id {
            "system_audio" => &mut self.system_audio,
            "dj" => &mut self.dj,
            "ai_dj" => &mut self.ai_dj,
            "visualiser" => &mut self.visualiser,
            "podcast" => &mut self.podcast,
            "live_coding" => &mut self.live_coding,
            "plugins" => &mut self.plugins,
            "controllers" => &mut self.controllers,
            "eq" => &mut self.eq,
            "mic" => &mut self.mic,
            "studio" => &mut self.studio,
            "player" => &mut self.player,
            "start_with_session" => &mut self.start_with_session,
            "bar" => &mut self.bar,
            "themes" => &mut self.themes,
            "hq_decks" => &mut self.hq_decks,
            "focus_border" => &mut self.focus_border,
            "show_track_path" => &mut self.show_track_path,
            "track_map" => &mut self.track_map,
            "mix_spots" => &mut self.mix_spots,
            "online_lookups" => &mut self.online_lookups,
            "preview_aux_notice" => &mut self.preview_aux_notice,
            "dev_mode" => &mut self.dev_mode,
            _ => return false,
        };
        *slot = on;
        // System audio needs the service from login: the default output is
        // Patch, and nothing may stand behind it silent. The AI DJ plays
        // on the decks.
        match (id, on) {
            ("system_audio", true) => self.start_with_session = true,
            ("start_with_session", false) => self.system_audio = false,
            ("ai_dj", true) => self.dj = true,
            ("dj", false) => self.ai_dj = false,
            // Answered in Settings: the Library does not ask again.
            ("online_lookups", _) => self.online_lookups_asked = true,
            _ => {}
        }
        self.keep_layout();
        true
    }

    /// Set one switch alone (only what it cannot do without follows).
    pub fn set_only(&mut self, id: &str, on: bool) {
        let tied = (self.system_audio, self.start_with_session);
        self.set(id, on);
        match id {
            "system_audio" => self.start_with_session = tied.1,
            "start_with_session" => self.system_audio = tied.0,
            _ => {}
        }
    }

    /// Whether the setup shows this surface (one of its modules is on).
    pub fn shows(&self, surface: &str) -> bool {
        let ms = surface_modules(surface);
        ms.is_empty() || ms.iter().any(|m| self.get(m).unwrap_or(true))
    }

    /// Whether every module a layout needs is on.
    pub fn offers(&self, layout: DefaultLayout) -> bool {
        layout.needs().iter().all(|m| self.get(m).unwrap_or(false))
    }

    /// Make `layout` the default; false (and unchanged) when its modules
    /// are off.
    pub fn set_layout(&mut self, layout: DefaultLayout) -> bool {
        let ok = self.offers(layout);
        if ok {
            self.layout = layout;
        }
        ok
    }

    /// The default layout falls back to Simple when a module it needs went.
    fn keep_layout(&mut self) {
        if !self.offers(self.layout) {
            self.layout = DefaultLayout::Simple;
        }
    }

    /// What an older file (or a hand edit) can't mean: the AI DJ without
    /// the decks, a default layout whose modules are off.
    pub fn normalized(mut self) -> Setup {
        self.ai_dj &= self.dj;
        self.keep_layout();
        self
    }

    /// The profile this is exactly, if any (else a custom setup);
    /// preferences (`hq_decks`, `focus_border`, `show_track_path`, the
    /// default layout) don't count.
    pub fn profile(&self) -> Option<Profile> {
        Profile::KNOWN.into_iter().find(|p| {
            let mut p = p.apply(self);
            p.layout = self.layout;
            p == *self
        })
    }

    /// The modules switched on, by id.
    pub fn modules(&self) -> Vec<&'static str> {
        MODULES
            .iter()
            .map(|m| m.0)
            .filter(|m| self.get(m) == Some(true))
            .collect()
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("omarchy-patch/setup.toml")
}

/// Whether Patch has been set up here (install ran, or the user chose).
pub fn exists() -> bool {
    path().is_file()
}

/// The setup on this machine as Patch runs it: the saved one, with a
/// module that needs a key off while its key is missing.
pub fn load() -> Setup {
    load_saved().licensed()
}

/// The setup as saved: what the user chose, a module that needs a key
/// included while the key is missing (it comes on with the key). Before one is
/// chosen, everything on except system audio (Patch never takes over the
/// sound unasked).
pub fn load_saved() -> Setup {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| toml::from_str::<Setup>(&t).ok())
        .map(Setup::normalized)
        .unwrap_or(Setup {
            system_audio: false,
            ..Setup::default()
        })
}

impl Setup {
    /// A module that needs a key stays off without it.
    pub fn licensed(mut self) -> Setup {
        if self.ai_dj && !crate::licence::module_unlocked("ai_dj") {
            self.ai_dj = false;
            // The decks without the AI DJ: Basic DJ, not the Mixer alone.
            if !self.offers(self.layout) && self.dj {
                self.layout = DefaultLayout::BasicDj;
            }
            self.keep_layout();
        }
        self
    }
}

pub fn save(s: &Setup) -> std::io::Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = toml::to_string(s).map_err(std::io::Error::other)?;
    let head = "# How Patch is set up here. Change it in the controller (Ctrl+K) or\n# with `omarchy-patch setup`, which also applies it.\n";
    // One temp file per process: two writers sharing
    // one could rename the other's half-written file into place.
    let tmp = p.with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{head}{body}"))?;
    std::fs::rename(tmp, p)
}

/// A cheap cached read for loops (the engine's keeper, the UI's timer):
/// the file is read again only when it changed.
pub struct Watched {
    stamp: Option<std::time::SystemTime>,
    value: Setup,
}

impl Default for Watched {
    fn default() -> Self {
        Watched {
            stamp: None,
            value: load(),
        }
    }
}

impl Watched {
    pub fn get(&mut self) -> Setup {
        let stamp = std::fs::metadata(path()).and_then(|m| m.modified()).ok();
        if stamp != self.stamp {
            self.stamp = stamp;
            self.value = load();
        }
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_and_switches() {
        for p in Profile::KNOWN {
            assert_eq!(p.setup().profile(), Some(p), "{}", p.name());
            assert_eq!(Profile::from_id(p.id()), Some(p));
            // Every profile is the system's audio, with the service and the bar.
            let s = p.setup();
            assert!(s.system_audio && s.start_with_session && s.bar, "{}", p.name());
            assert!(s.offers(s.layout), "{}: its own default layout", p.name());
        }
        assert_eq!(Profile::ALL[0], Profile::Standard, "Standard first: the installer's default");
        let standard = Profile::Standard.setup();
        assert_eq!(standard.modules(), ["eq"], "Standard is the core and the EQ");
        assert_eq!(standard.layout, DefaultLayout::Simple);
        let min = Profile::Minimal.setup();
        assert!(min.modules().is_empty() && !min.themes, "Minimal is the core alone");
        assert_eq!(Profile::Full.setup().modules().len(), MODULES.len());
        let mut s = Profile::Dj.setup();
        assert!(s.dj && s.ai_dj && !s.themes);
        assert!(s.set("system_audio", false));
        assert_eq!(s.profile(), None, "custom");
        assert!(!s.set("nonsense", true));
        // System audio and the service go together.
        let mut m = Profile::Minimal.setup();
        m.set("start_with_session", false);
        assert!(!m.system_audio);
        m.set("system_audio", true);
        assert!(m.start_with_session);
        for (id, _, _) in SWITCHES {
            assert!(s.get(id).is_some(), "{id}");
        }
    }

    #[test]
    fn the_ai_dj_needs_the_decks() {
        let mut s = Profile::Minimal.setup();
        s.set("ai_dj", true);
        assert!(s.dj, "the AI DJ brings the decks");
        s.set("dj", false);
        assert!(!s.ai_dj, "no decks, no AI DJ");
        let odd: Setup = toml::from_str("dj = false\nai_dj = true\n").unwrap();
        assert!(!odd.normalized().ai_dj);
    }

    #[test]
    fn modules_hide_their_surfaces() {
        let min = Profile::Minimal.setup();
        for core in ["mixer", "meters", "inspector", "devices", "diagnostics", "diagnostics#3"] {
            assert!(min.shows(core), "{core} is core");
        }
        for module in [
            "deck-a", "library", "dj", "visualiser", "mic", "broadcast", "tidal", "plugins", "controllers", "meters#2",
            "graph", "player", "recorder", "eq", "analyzer",
        ] {
            assert_eq!(min.shows(module), module == "meters#2", "{module}");
        }
        // Podcast brings the mic, the Player and the Recorder; Studio tools
        // the Recorder too.
        let mut pod = min;
        pod.set("podcast", true);
        assert!(pod.shows("mic") && pod.shows("player") && pod.shows("recorder") && !pod.shows("graph"));
        let mut studio = min;
        studio.set("studio", true);
        assert!(studio.shows("recorder") && studio.shows("graph") && !studio.shows("player"));
        let mut s = min;
        s.set("dj", true);
        assert!(s.shows("deck-b") && s.shows("dj-mixer") && !s.shows("dj"), "Basic DJ: no AI");
        // A Unit's second window goes with its module.
        let mut full = Profile::Full.setup();
        assert!(full.shows("visualiser-controls") && full.shows("performer"));
        full.set("visualiser", false);
        full.set("podcast", false);
        assert!(!full.shows("visualiser-controls"), "Controls without the Visualiser");
        assert!(!full.shows("performer"), "the Performer without Broadcast");
    }

    #[test]
    fn the_default_layout_follows_the_modules() {
        let mut s = Profile::Full.setup();
        assert_eq!(s.layout, DefaultLayout::PerformanceDj);
        assert!(s.set_layout(DefaultLayout::Podcast));
        s.set("podcast", false);
        assert_eq!(s.layout, DefaultLayout::Simple, "its module went: back to Simple");
        let mut min = Profile::Minimal.setup();
        assert!(!min.set_layout(DefaultLayout::BasicDj));
        assert_eq!(min.layout, DefaultLayout::Simple);
        // A profile keeps the chosen layout while it still offers it.
        let mut dj = Profile::Dj.setup();
        dj.set_layout(DefaultLayout::BasicDj);
        assert_eq!(Profile::Full.apply(&dj).layout, DefaultLayout::BasicDj);
        assert_eq!(Profile::Minimal.apply(&dj).layout, DefaultLayout::Simple);
        assert_eq!(dj.profile(), Some(Profile::Dj), "the layout is a preference");
        for l in DefaultLayout::ALL {
            assert_eq!(DefaultLayout::from_id(l.id()), Some(l));
        }
    }

    #[test]
    fn round_trips_and_fills_missing_keys() {
        let mut s = Profile::Mixer.setup();
        s.set_layout(DefaultLayout::Podcast);
        let t = toml::to_string(&s).unwrap();
        assert!(t.contains("layout = \"podcast\""), "{t}");
        assert_eq!(toml::from_str::<Setup>(&t).unwrap(), s);
        // An older file without a newer switch keeps the default for it.
        let old: Setup = toml::from_str("dj = false\n").unwrap();
        assert!(!old.dj && old.bar);
        assert!(!old.normalized().ai_dj, "no AI DJ without the decks");
        assert!(old.visualiser, "an older setup keeps the Units it showed");
        assert!(!old.hq_decks, "compact decks by default");
        assert!(!old.show_track_path, "decks hide the file path by default");
    }

    #[test]
    fn profiles_keep_preferences() {
        let mut s = Profile::Dj.setup();
        s.set("hq_decks", true);
        s.set("show_track_path", true);
        assert_eq!(
            s.profile(),
            Some(Profile::Dj),
            "a preference isn't a custom setup"
        );
        let full = Profile::Full.apply(&s);
        assert!(full.hq_decks && full.show_track_path && full.system_audio);
    }
}
