//! Shared input to composable performance layers. No clock, analysis or Director
//! lives here: the Visualiser supplies its current scene and real source state.
use crate::VisualIntent;
use serde::{Deserialize, Serialize};

pub const MAX_SOURCES: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Source {
    pub active: bool,
    pub level: f32,
    /// The channel's level meters, left and right (0 … 1), the same
    /// readings the DJ surface's channel meters show.
    pub meter: [f32; 2],
    /// Engine-derived beat position; None means no reliable grid.
    pub beat: Option<f32>,
    pub progress: f32,
    /// Actual channel fader times crossfader law, not a linear guess.
    pub weight: f32,
    /// Actual normalized channel fader position for the booth geometry.
    pub gain: f32,
    /// Actual normalized EQ controls, -1 = kill, 0 = flat.
    pub eq: [f32; 3],
    pub filter: f32,
    /// Actual enabled beat-FX depth; no synthetic stem or effect activity.
    pub fx: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub decks: bool,
    pub performer: bool,
    pub deck_opacity: f32,
    pub performer_opacity: f32,
    /// 0 dark silhouette, 1 wire/points. Intermediate values morph smoothly.
    pub appearance: f32,
    pub automatic: bool,
    pub performer_first: bool,
    pub director_colours: bool,
    pub colours: [[f32; 3]; 2],
    /// The mark on the DJ's chest.
    pub emblem: Emblem,
    /// The filmed DJ (the user's own clips on this machine) in place of the
    /// drawn one. Falls back to the drawn DJ where they are not installed.
    pub me: bool,
    /// The filmed DJ's looks, each 0 … 1: the person, the shadow, the points.
    /// They are one film, so they always show one pose; they mix.
    pub look: [f32; 3],
    /// The decks' signal ribbons across the stage (point streams behind the
    /// booth). Off leaves the booth alone.
    pub ribbons: bool,
}
/// The mark on the shadow DJ's chest: Patch's "P.", the Omarchy logo, or
/// none.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Emblem {
    #[default]
    Patch,
    Omarchy,
    Off,
}
impl Emblem {
    /// As the shader and the settings panel number them.
    pub fn index(self) -> i32 {
        match self {
            Emblem::Patch => 0,
            Emblem::Omarchy => 1,
            Emblem::Off => 2,
        }
    }
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Emblem::Omarchy,
            2 => Emblem::Off,
            _ => Emblem::Patch,
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            decks: false,
            performer: false,
            deck_opacity: 0.85,
            performer_opacity: 0.95,
            appearance: 0.0,
            automatic: true,
            performer_first: false,
            director_colours: false,
            colours: BRAND,
            emblem: Emblem::Patch,
            me: false,
            look: [1.0, 0.0, 0.0],
            ribbons: true,
        }
    }
}
/// Patch's own deck colours: signal red (#f0243b) for A, violet (#9d7cf0)
/// for B, the Patch Dark theme's. Cyan and magenta were never Patch's.
pub const BRAND: [[f32; 3]; 2] = [[0.941, 0.141, 0.231], [0.616, 0.486, 0.941]];
/// The colours this layer started with before Patch's own (2026-10-25):
/// settings that still hold them never chose a colour, and get Patch's.
pub fn first_defaults(c: [[f32; 3]; 2]) -> bool {
    let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02);
    near(c[0], [0.05, 0.88, 1.0]) && near(c[1], [1.0, 0.12, 0.7])
}
fn unit(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.deck_opacity = unit(self.deck_opacity);
        self.performer_opacity = unit(self.performer_opacity);
        self.appearance = unit(self.appearance);
        self.look = self.look.map(unit);
        for c in self.colours.iter_mut().flatten() {
            *c = unit(*c);
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Scene {
    pub seconds: f32,
    pub beat: Option<f32>,
    pub seed: f32,
    pub progress: f32,
    pub intent: VisualIntent,
    pub palette: [[f32; 3]; 2],
    /// None before/without a real AI handover; progress is never fabricated.
    pub handover: Option<f32>,
    pub transition_preparing: bool,
    /// Mixed-output bands. These are deliberately not per-source spectra.
    pub bands: [f32; 3],
    pub kick: f32,
    pub crossfader: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub scene: Scene,
    pub sources: [Source; MAX_SOURCES],
    pub count: usize,
    pub settings: Settings,
    pub appearance: f32,
    /// Bounded GPU point decimation; 1 full, 2 half, 4 quarter density.
    pub point_step: u32,
    /// The DJ's body and gestures this frame (see `groove`).
    pub body: crate::groove::Body,
}
impl Frame {
    pub fn visible(&self) -> bool {
        (self.settings.decks && self.settings.deck_opacity > 0.0)
            || (self.settings.performer && self.settings.performer_opacity > 0.0)
    }
    pub fn sources(&self) -> &[Source] {
        &self.sources[..self.count.min(MAX_SOURCES)]
    }
}

/// Target appearance uses the Director's section, not an independent timeline.
pub fn appearance(settings: Settings, scene: &Scene) -> f32 {
    use patch_core::visual_intent::Section;
    if !settings.automatic {
        return settings.appearance;
    }
    match scene.intent.section {
        Section::Build | Section::Transition => {
            (0.25 + scene.intent.motion * 0.65).max(settings.appearance)
        }
        Section::Drop => (1.0 - scene.progress * 0.8).max(settings.appearance),
        _ => settings.appearance,
    }
    .clamp(0.0, 1.0)
}

/// Which performer is on stage, and how solid: [person, silhouette, points],
/// one of them non-zero. Never two at rest (their bodies differ); a change is
/// a hologram's morph from one to the next. With Auto on the choreography
/// chooses: through a groove the three take turns every 16 bars (the person,
/// the silhouette, the person, the points, so he shows most); the person on a
/// drop, the points through a build or a transition, a see-through silhouette
/// in a breakdown, the silhouette for an intro, the points for an outro. With
/// Auto off the DJ's own choice holds.
pub fn film_look(settings: Settings, scene: &Scene) -> [f32; 3] {
    use patch_core::visual_intent::Section;
    if !settings.automatic {
        return one(settings.look);
    }
    match scene.intent.section {
        // The person carries the story: the drop, and the breakdown where he
        // takes his headphones off and plays on relaxed.
        Section::Drop | Section::Breakdown => [1.0, 0.0, 0.0],
        Section::Build | Section::Transition | Section::Outro => [0.0, 0.0, 1.0],
        Section::Intro => [0.0, 1.0, 0.0],
        Section::Verse | Section::Unknown => {
            let turn = scene.beat.map_or(0, |b| (b / 64.0).floor().rem_euclid(4.0) as usize);
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]][turn]
        }
    }
}
/// The strongest of the looks alone, at its own amount.
fn one(look: [f32; 3]) -> [f32; 3] {
    let i = (0..3).fold(0, |b, i| if look[i] > look[b] { i } else { b });
    let mut out = [0.0; 3];
    out[i] = if look[i] > 0.0 { look[i] } else { 1.0 };
    out
}
/// Whether the filmed DJ should have his headphones on. With Auto on they rest
/// around his neck through a breakdown and come back for the next build.
pub fn film_headphones(settings: Settings, scene: &Scene) -> bool {
    use patch_core::visual_intent::Section;
    !(settings.automatic && scene.intent.section == Section::Breakdown)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_filmed_dj_follows_the_section_only_on_auto() {
        use patch_core::visual_intent::Section;
        let mut scene = Scene::default();
        scene.intent.section = Section::Breakdown;
        let manual = Settings {
            automatic: false,
            look: [0.2, 0.3, 0.4],
            ..Default::default()
        };
        // One performer only, the strongest of a mixed setting.
        assert_eq!(film_look(manual, &scene), [0.0, 0.0, 0.4]);
        assert!(film_headphones(manual, &scene));
        let auto = Settings {
            automatic: true,
            ..Default::default()
        };
        // In a breakdown he plays on with his headphones around his neck.
        assert_eq!(film_look(auto, &scene), [1.0, 0.0, 0.0]);
        assert!(!film_headphones(auto, &scene));
        scene.intent.section = Section::Drop;
        assert_eq!(film_look(auto, &scene), [1.0, 0.0, 0.0]);
        assert!(film_headphones(auto, &scene));
        // Through a groove they take turns, the person most.
        scene.intent.section = Section::Verse;
        let turns: Vec<[f32; 3]> = (0..4)
            .map(|k| {
                scene.beat = Some(k as f32 * 64.0 + 1.0);
                film_look(auto, &scene)
            })
            .collect();
        assert_eq!(turns.iter().filter(|l| l[0] > 0.0).count(), 2);
        assert!(turns.iter().any(|l| l[1] > 0.0) && turns.iter().any(|l| l[2] > 0.0));
        for section in [Section::Intro, Section::Verse, Section::Build, Section::Drop, Section::Breakdown, Section::Transition, Section::Outro] {
            scene.intent.section = section;
            let on = film_look(auto, &scene).iter().filter(|v| **v > 0.0).count();
            assert_eq!(on, 1, "{section:?} shows one performer");
        }
    }
    #[test]
    fn missing_sources_and_disabled_layers_are_empty() {
        let f = Frame {
            scene: Scene::default(),
            sources: [Source::default(); MAX_SOURCES],
            count: 0,
            settings: Settings::default(),
            appearance: 0.0,
            point_step: 1,
            body: Default::default(),
        };
        assert!(!f.visible());
        assert!(f.sources().is_empty());
    }
    #[test]
    fn independent_opacity_and_bounded_sources() {
        let mut f = Frame {
            scene: Scene::default(),
            sources: [Source::default(); MAX_SOURCES],
            count: usize::MAX,
            settings: Settings::default(),
            appearance: 0.0,
            point_step: 1,
            body: Default::default(),
        };
        f.settings.decks = true;
        f.settings.deck_opacity = 0.0;
        assert!(!f.visible());
        f.settings.performer = true;
        assert!(f.visible());
        assert_eq!(f.sources().len(), MAX_SOURCES);
        f.settings.performer_opacity = 0.0;
        assert!(!f.visible());
    }
    #[test]
    fn automatic_appearance_obeys_shared_scene_and_manual_setting() {
        let mut s = Scene::default();
        let mut c = Settings::default();
        s.intent.section = patch_core::visual_intent::Section::Drop;
        assert_eq!(appearance(c, &s), 1.0);
        s.progress = 1.0;
        assert!(appearance(c, &s) < 0.3);
        c.automatic = false;
        c.appearance = 0.4;
        assert_eq!(appearance(c, &s), 0.4);
    }
    #[test]
    fn settings_reject_nonfinite_and_out_of_range_values() {
        let c = Settings {
            deck_opacity: f32::NAN,
            performer_opacity: 2.0,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.deck_opacity, 0.0);
        assert_eq!(c.performer_opacity, 1.0);
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
    }
}

/// The DJ layers stepped down on a machine too slow for them, and back up
/// when it copes again: 0 as set, 1 without the DJ, 2 without the layers.
/// Judged by what the layers themselves cost the GPU (not by the UI's
/// frame rate, which every window shares): over 8 ms a frame, held ~3 s,
/// steps down. After `wait` (20 s, doubled each time a step back up proves
/// too slow, at most 160 s) it tries one step up. Never saved; the DJ's own
/// setting stays as chosen, and changing it starts afresh.
#[derive(Clone, Copy, Debug)]
pub struct Shedding {
    pub level: u8,
    avg: f32,
    slow: f32,
    since: f32,
    wait: f32,
    retried: bool,
}
impl Default for Shedding {
    fn default() -> Self {
        Self { level: 0, avg: 0.0, slow: 0.0, since: 0.0, wait: 20.0, retried: false }
    }
}
/// What the layers may cost the GPU per frame: half a 60 Hz frame.
pub const LAYER_BUDGET_MS: f32 = 8.0;
impl Shedding {
    /// One frame, `dt` seconds after the last, with the layers `on` (as the
    /// DJ set them) and their measured GPU cost (None: not measured, never
    /// a reason to step down). Returns what changed, to show briefly.
    pub fn advance(&mut self, dt: f32, on: bool, cost_ms: Option<f32>) -> Option<&'static str> {
        if !on {
            *self = Self { wait: self.wait, ..Self::default() };
            return None;
        }
        if !(0.0..0.25).contains(&dt) {
            return None;
        }
        match cost_ms {
            Some(ms) => {
                self.avg += (ms - self.avg) * (1.0 - (-dt / 0.5).exp());
                self.slow = if self.avg > LAYER_BUDGET_MS { self.slow + dt } else { 0.0 };
            }
            None => self.slow = 0.0,
        }
        if self.slow > 3.0 && self.level < 2 {
            // Too slow again right after a step up: wait longer next time.
            if self.retried {
                self.wait = (self.wait * 2.0).min(160.0);
            }
            self.level += 1;
            self.slow = 0.0;
            self.since = 0.0;
            self.avg = 0.0;
            self.retried = false;
            return Some(if self.level == 1 { "DJ off: too heavy for this GPU" } else { "DJ layers off: too heavy for this GPU" });
        }
        if self.level > 0 {
            self.since += dt;
            if self.since > self.wait {
                self.level -= 1;
                self.since = 0.0;
                self.avg = 0.0;
                self.retried = true;
                return Some(if self.level == 0 { "DJ back on" } else { "booth back on" });
            }
        }
        None
    }
}

/// Frame cadence is a pressure hint, never a musical clock. Reduce optional
/// detail before affecting animation time; recover slowly to avoid oscillation.
#[derive(Default)]
pub struct Budget {
    pressure: f32,
}
impl Budget {
    pub fn advance(&mut self, dt: f32) -> u32 {
        let target = if dt > 1.0 / 45.0 { 1.0 } else { 0.0 };
        let tau = if target > self.pressure { 0.8 } else { 4.0 };
        self.pressure += (target - self.pressure) * (1.0 - (-dt.clamp(0.0, 0.25) / tau).exp());
        if self.pressure > 0.75 {
            4
        } else if self.pressure > 0.35 {
            2
        } else {
            1
        }
    }
}
#[cfg(test)]
mod budget_tests {
    #[test]
    fn pressure_reduces_detail_and_recovers_without_retiming_scene() {
        let mut b = super::Budget::default();
        for _ in 0..120 {
            b.advance(1.0 / 30.0);
        }
        assert_eq!(b.advance(1.0 / 30.0), 4);
        for _ in 0..1200 {
            b.advance(1.0 / 60.0);
        }
        assert_eq!(b.advance(1.0 / 60.0), 1);
    }
}

/// The existing AI transition contract, interpolated from its graph stamp.
/// Shared by the Visualiser and capture/replay tests; never a new beat clock.
pub fn handover_at(
    st: &patch_core::aivis::VisualState,
    now_ns: u64,
) -> Option<crate::director::Handover> {
    let tr = st.transition.as_ref().filter(|tr| tr.ready || tr.started)?;
    let from = st.sources.iter().find(|s| s.id == tr.from)?;
    let to = st.sources.iter().find(|s| s.id == tr.to)?;
    let (beat, bpm) = (from.beat?, from.bpm?);
    if !beat.is_finite() || !bpm.is_finite() || bpm <= 0.0 || !tr.bars.is_finite() || tr.bars <= 0.0
    {
        return None;
    }
    let ahead = if from.playing {
        now_ns.saturating_sub(st.clock.nsec) as f64 / 1e9
    } else {
        0.0
    };
    (!to.title.is_empty()).then(|| crate::director::Handover {
        from: crate::banner::clean_title(&from.title),
        title: crate::banner::clean_title(&to.title),
        artist: to.artist.clone(),
        beats_in: beat + ahead * bpm / 60.0 - tr.start_beat,
        beats: tr.bars as f64 * 4.0,
        bpm,
    })
}
#[cfg(test)]
mod handover_tests {
    use super::*;
    #[test]
    fn transition_uses_outgoing_stamp_even_when_incoming_is_louder() {
        let mut st = patch_core::aivis::VisualState::default();
        assert!(handover_at(&st, 0).is_none());
        st.clock.nsec = 1_000_000_000;
        st.sources = vec![
            patch_core::aivis::Source {
                id: "a".into(),
                title: "A".into(),
                playing: true,
                beat: Some(32.0),
                bpm: Some(120.0),
                presence: 0.1,
                ..Default::default()
            },
            patch_core::aivis::Source {
                id: "b".into(),
                title: "B".into(),
                presence: 0.9,
                ..Default::default()
            },
        ];
        st.transition = Some(patch_core::aivis::Transition {
            from: "a".into(),
            to: "b".into(),
            ready: true,
            bars: 8.0,
            start_beat: 32.0,
            ..Default::default()
        });
        let h = handover_at(&st, 3_000_000_000).unwrap();
        assert_eq!(h.beats_in, 4.0);
        assert_eq!(h.beats, 32.0);
        st.sources[0].playing = false;
        assert_eq!(handover_at(&st, 3_000_000_000).unwrap().beats_in, 0.0);
        st.sources[0].beat = None;
        assert!(handover_at(&st, 3_000_000_000).is_none());
    }
}

#[cfg(test)]
mod shedding_tests {
    use super::Shedding;
    #[test]
    fn heavy_layers_shed_the_dj_then_the_layers_and_recover_without_flapping() {
        let mut s = Shedding::default();
        let run = |s: &mut Shedding, ms: Option<f32>, secs: f32| {
            for _ in 0..(secs * 60.0) as usize {
                s.advance(1.0 / 60.0, true, ms);
            }
        };
        // Cheap layers, or no measurement at all: never shed.
        run(&mut s, Some(2.0), 30.0);
        run(&mut s, None, 30.0);
        assert_eq!(s.level, 0);
        run(&mut s, Some(14.0), 4.0);
        assert_eq!(s.level, 1);
        run(&mut s, Some(14.0), 4.0);
        assert_eq!(s.level, 2);
        // Nothing drawn now: one step back after the wait, not before.
        run(&mut s, None, 15.0);
        assert_eq!(s.level, 2);
        run(&mut s, None, 6.0);
        assert_eq!(s.level, 1);
        // That step proves too heavy: down again, and the next try waits longer.
        run(&mut s, Some(14.0), 4.0);
        assert_eq!(s.level, 2);
        run(&mut s, None, 25.0);
        assert_eq!(s.level, 2);
        run(&mut s, None, 20.0);
        assert_eq!(s.level, 1);
        // A hidden window's long gap counts for nothing; layers off resets.
        assert_eq!(s.advance(2.0, true, Some(20.0)), None);
        s.advance(1.0 / 60.0, false, None);
        assert_eq!(s.level, 0);
    }
}
