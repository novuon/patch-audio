//! How adventurous the AI DJ's transitions are, and how much of each track
//! plays: one setting, kept in the engine session, read by the planner,
//! shown in the DJ Booth and set from `omarchy-patch dj style`.
//!
//! Since 2026-10-13 the AI plans only plain transitions (EQ blends, a clean
//! cut with an echo). Everything it dropped then comes back here as
//! switches, each off in the default Plain style, so nothing changes for the
//! ear until the DJ chooses it.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The preset: which switches start on, and how far the flourishes go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// EQ blends, breakdown entries, an EQ fade or filter out, or a cut
    /// with a short echo (stem-led blends where stems exist).
    #[default]
    Plain,
    /// Plain, plus the gentle flourishes: soft effect exits and one subtle
    /// accent; no loops, no bass replace, no stabs.
    Balanced,
    /// Everything the AI did before 2026-10-13: effect exits, accents,
    /// crossfader teases, loops and bass replace (the ones reported as
    /// clashes score lower, none is banned).
    Adventurous,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Plain, Style::Balanced, Style::Adventurous];

    pub fn id(self) -> &'static str {
        match self {
            Style::Plain => "plain",
            Style::Balanced => "balanced",
            Style::Adventurous => "adventurous",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Style::Plain => "Plain",
            Style::Balanced => "Balanced",
            Style::Adventurous => "Adventurous",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Style::Plain => "Clean EQ blends and cuts only: the safest, the simplest",
            Style::Balanced => "Plain plus soft effect exits and a subtle accent now and then",
            Style::Adventurous => {
                "Everything: effect exits, accents, loops, bass replace, crossfader tricks"
            }
        }
    }

    pub fn parse(id: &str) -> Option<Style> {
        Style::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// Which tracks get stems prepared by themselves (the user, 2026-11-14:
/// one global setting, Auto by default). Which prepared stems a transition
/// uses is never this setting's call: the mix keeps them only where they
/// mix better (docs/decisions.md).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StemPrep {
    /// When needed or Ahead, from how fast this machine separates a track.
    #[default]
    Auto,
    /// None by itself; only "Prepare stems" (a set prep's own option too).
    Off,
    /// Only the two tracks of a planned transition that gains from them.
    WhenNeeded,
    /// Also the likeliest next tracks, once per playing track: one, or two
    /// when tracks play briefly.
    Ahead,
    /// Every track of the set's collections, in the background.
    Everything,
}

impl StemPrep {
    pub const ALL: [StemPrep; 5] = [
        StemPrep::Auto,
        StemPrep::Off,
        StemPrep::WhenNeeded,
        StemPrep::Ahead,
        StemPrep::Everything,
    ];

    pub fn id(self) -> &'static str {
        match self {
            StemPrep::Auto => "auto",
            StemPrep::Off => "off",
            StemPrep::WhenNeeded => "when_needed",
            StemPrep::Ahead => "ahead",
            StemPrep::Everything => "everything",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StemPrep::Auto => "Auto",
            StemPrep::Off => "Off",
            StemPrep::WhenNeeded => "When needed",
            StemPrep::Ahead => "Ahead",
            StemPrep::Everything => "Everything",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            StemPrep::Auto => "When needed or Ahead, from how fast this machine prepares a track",
            StemPrep::Off => "Only what you prepare yourself",
            StemPrep::WhenNeeded => "Only the two tracks of a planned mix that gains from them",
            StemPrep::Ahead => "Also the likeliest next track (two when tracks play briefly)",
            StemPrep::Everything => "Every track of the set's collections, in the background",
        }
    }

    /// `auto`, `off`, `when_needed` (or `needed`), `ahead`, `everything`
    /// (or `all`).
    pub fn parse(s: &str) -> Option<StemPrep> {
        match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "needed" | "when_needed" => Some(StemPrep::WhenNeeded),
            "all" => Some(StemPrep::Everything),
            id => StemPrep::ALL.into_iter().find(|p| p.id() == id),
        }
    }

    /// What Auto means on a machine that prepares a track in about
    /// `seconds_per_track` (None: not measured yet, `gpu`: whether the
    /// configured separator runs on a GPU). Ahead when a track is ready in
    /// a minute, else When needed. Other levels are themselves.
    pub fn resolve(self, seconds_per_track: Option<f64>, gpu: bool) -> StemPrep {
        match self {
            StemPrep::Auto => match seconds_per_track {
                Some(s) if s <= 60.0 => StemPrep::Ahead,
                Some(_) => StemPrep::WhenNeeded,
                None if gpu => StemPrep::Ahead,
                None => StemPrep::WhenNeeded,
            },
            other => other,
        }
    }
}

/// How far the flourishes that are switched on go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Soft exits (echo, reverb, filter, ping-pong), at most one subtle
    /// accent, no stabs or stutters.
    Gentle,
    /// The whole repertoire, up to three accents.
    Full,
}

/// The least a track plays, in percent of its length.
pub const MIN_PLAY_PERCENT: u8 = 20;
/// The least a track plays however short it is: this many bars (2 phrases)...
pub const MIN_PLAY_BARS: f64 = 16.0;
/// ...and this many seconds, whichever is longer.
pub const MIN_PLAY_SECONDS: f64 = 40.0;

/// The whole setting.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixStyle {
    pub style: Style,
    /// Leave a track with a riser, roll, brake, gate, reverb, ping-pong or
    /// power cut instead of an EQ fade or filter.
    pub effect_exits: bool,
    /// Loop a hook, an intro or the end of a track under the mix.
    pub loops: bool,
    /// Accents: crossfader teases and throws, bass stutters, echo throws,
    /// filter builds, flanger and phaser sweeps.
    pub accents: bool,
    /// Blend in over a track's drums and bass alone when its stems are
    /// prepared.
    pub stem_led: bool,
    /// The incoming bassline plays under the outgoing melody first.
    pub bass_replace: bool,
    /// Where a stem mix and the plain mix rehearse about the same, keep
    /// the stems.
    pub prefer_stems: bool,
    /// Roughly how much of each track plays, in percent of its length,
    /// transitions included (100 = the whole track).
    pub play_percent: u8,
    /// Which tracks get stems prepared by themselves (not part of a style).
    pub stem_prep: StemPrep,
}

impl Default for MixStyle {
    fn default() -> Self {
        MixStyle::preset(Style::Plain)
    }
}

impl MixStyle {
    /// The switches a style starts with. Prefer-stems and the play length
    /// are not part of a style.
    pub fn preset(style: Style) -> MixStyle {
        let (effect_exits, loops, accents, bass_replace) = match style {
            Style::Plain => (false, false, false, false),
            Style::Balanced => (true, false, true, false),
            Style::Adventurous => (true, true, true, true),
        };
        MixStyle {
            style,
            effect_exits,
            loops,
            accents,
            stem_led: true,
            bass_replace,
            prefer_stems: true,
            play_percent: 100,
            stem_prep: StemPrep::Auto,
        }
    }

    /// Choose a style: its switches replace the current ones, the play
    /// length and prefer-stems stay.
    pub fn choose(&mut self, style: Style) {
        *self = MixStyle {
            prefer_stems: self.prefer_stems,
            play_percent: self.play_percent,
            stem_prep: self.stem_prep,
            ..MixStyle::preset(style)
        };
    }

    /// True when the switches no longer match the style's preset.
    pub fn custom(&self) -> bool {
        let p = MixStyle::preset(self.style);
        (p.effect_exits, p.loops, p.accents, p.stem_led, p.bass_replace)
            != (
                self.effect_exits,
                self.loops,
                self.accents,
                self.stem_led,
                self.bass_replace,
            )
    }

    pub fn reach(&self) -> Reach {
        match self.style {
            Style::Adventurous => Reach::Full,
            _ => Reach::Gentle,
        }
    }

    /// Any flourish at all (an exit, an accent, a loop or a bass replace).
    pub fn any_flourish(&self) -> bool {
        self.effect_exits || self.loops || self.accents || self.bass_replace
    }

    /// Change what `req` names: `style` first (its preset), then each
    /// switch, `prefer_stems` and `play_percent`. Nothing changes on an error.
    pub fn apply(&mut self, req: &Value) -> Result<(), String> {
        let mut next = *self;
        if let Some(v) = req.get("style").filter(|v| !v.is_null()) {
            let id = v.as_str().ok_or("style is plain, balanced or adventurous")?;
            next.choose(
                Style::parse(id).ok_or("style is plain, balanced or adventurous")?,
            );
        }
        for (key, slot) in [
            ("effect_exits", &mut next.effect_exits),
            ("loops", &mut next.loops),
            ("accents", &mut next.accents),
            ("stem_led", &mut next.stem_led),
            ("bass_replace", &mut next.bass_replace),
            ("prefer_stems", &mut next.prefer_stems),
        ] {
            if let Some(v) = req.get(key).filter(|v| !v.is_null()) {
                *slot = v.as_bool().ok_or_else(|| format!("{key} is on or off"))?;
            }
        }
        if let Some(v) = req.get("stem_prep").filter(|v| !v.is_null()) {
            next.stem_prep = v
                .as_str()
                .and_then(StemPrep::parse)
                .ok_or("stem_prep is auto, off, when_needed, ahead or everything")?;
        }
        if let Some(v) = req.get("play_percent").filter(|v| !v.is_null()) {
            let p = v
                .as_u64()
                .filter(|p| (MIN_PLAY_PERCENT as u64..=100).contains(p))
                .ok_or_else(|| format!("play_percent is {MIN_PLAY_PERCENT}–100"))?;
            next.play_percent = p as u8;
        }
        *self = next;
        Ok(())
    }

    /// What the next transition will use, in one line.
    pub fn summary(&self) -> String {
        let mut on: Vec<&str> = vec![];
        for (flag, name) in [
            (self.effect_exits, "effect exits"),
            (self.accents, "accents"),
            (self.loops, "loops"),
            (self.bass_replace, "bass replace"),
            (self.stem_led, "stem-led blends"),
        ] {
            if flag {
                on.push(name);
            }
        }
        format!(
            "{}{}: {}{}{}",
            self.style.label(),
            if self.custom() { " (custom)" } else { "" },
            if on.is_empty() { "EQ blends and cuts".into() } else { on.join(", ") },
            if self.prefer_stems { " · stems preferred" } else { "" },
            if self.play_percent < 100 {
                format!(" · {}% of each track", self.play_percent)
            } else {
                String::new()
            },
        )
    }

    /// The setting as the engine reports it: the values, each control's
    /// words and the one-line summary.
    pub fn json(&self) -> Value {
        json!({
            "style": self.style.id(),
            "custom": self.custom(),
            "effect_exits": self.effect_exits,
            "loops": self.loops,
            "accents": self.accents,
            "stem_led": self.stem_led,
            "bass_replace": self.bass_replace,
            "prefer_stems": self.prefer_stems,
            "play_percent": self.play_percent,
            "stem_prep": self.stem_prep.id(),
            "stem_preps": StemPrep::ALL.iter().map(|p| json!({
                "id": p.id(), "label": p.label(), "about": p.about(),
            })).collect::<Vec<_>>(),
            "summary": self.summary(),
            "styles": Style::ALL.iter().map(|s| json!({
                "id": s.id(), "label": s.label(), "about": s.about(),
            })).collect::<Vec<_>>(),
            "controls": CONTROLS.iter().map(|(id, label, about)| json!({
                "id": id, "label": label, "about": about,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Each switch's name and what it does, in one line (shared by the DJ
/// Booth, the CLI and the docs).
pub const CONTROLS: [(&str, &str, &str); 6] = [
    (
        "effect_exits",
        "Effect exits",
        "Leave a track with a riser, roll, brake, gate, reverb or ping-pong, not only an EQ fade",
    ),
    (
        "accents",
        "Accents & teases",
        "Small moves in a blend: crossfader teases, echo throws, filter builds, sweeps",
    ),
    (
        "loops",
        "Loops",
        "Loop a hook or the end of a track so a blend has room when the track is short",
    ),
    (
        "stem_led",
        "Stem-led blends",
        "Open over a track's drums and bass alone, when its stems are prepared",
    ),
    (
        "bass_replace",
        "Bass replace",
        "The new bassline plays under the old melody before the swap",
    ),
    (
        "prefer_stems",
        "Prefer stems",
        "Keep the stem mix when it rehearses about as clean as the plain one",
    ),
];

/// The seconds of one track that play (transitions included): `percent` of
/// its length, never less than [`MIN_PLAY_BARS`] bars nor
/// [`MIN_PLAY_SECONDS`], rounded up to a whole phrase (8 bars of `bar_s`
/// seconds) and never past the track. Where a whole phrase would overshoot
/// by more than 15 % it rounds up to 4, 2 or 1 bars instead, so the result
/// is always on the grid and within 15 % above what was asked. 100 % is the
/// whole track.
pub fn play_seconds(duration_s: f64, percent: u8, bar_s: f64) -> f64 {
    if !duration_s.is_finite() || duration_s <= 0.0 {
        return 0.0;
    }
    if percent >= 100 || !bar_s.is_finite() || bar_s <= 0.0 {
        return duration_s;
    }
    let wanted = (duration_s * f64::from(percent) / 100.0)
        .max(MIN_PLAY_BARS * bar_s)
        .max(MIN_PLAY_SECONDS)
        .min(duration_s);
    for bars in [8.0, 4.0, 2.0, 1.0] {
        let unit = bars * bar_s;
        let up = (wanted / unit - 1e-9).ceil().max(1.0) * unit;
        if up <= wanted * 1.15 && up <= duration_s {
            return up;
        }
    }
    // Too short a track for any whole bar count above the wish: the last
    // whole phrase, or all of it.
    let phrase = 8.0 * bar_s;
    ((duration_s / phrase).floor() * phrase).max(wanted.min(duration_s))
}

/// About how many minutes of a track of `duration_s` play at `percent`
/// (before phrase snapping): what the DJ Booth shows.
pub fn approx_minutes(duration_s: f64, percent: u8) -> f64 {
    let s = if percent >= 100 {
        duration_s
    } else {
        (duration_s * f64::from(percent) / 100.0).max(MIN_PLAY_SECONDS.min(duration_s))
    };
    s / 60.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_the_default_and_matches_todays_plans() {
        let d = MixStyle::default();
        assert_eq!(d.style, Style::Plain);
        assert!(!d.any_flourish());
        assert!(d.stem_led && d.prefer_stems);
        assert_eq!(d.play_percent, 100);
        assert!(!d.custom());
        assert_eq!(d.reach(), Reach::Gentle);
    }

    #[test]
    fn presets_order_by_how_much_they_allow() {
        let on = |s: Style| {
            let m = MixStyle::preset(s);
            [m.effect_exits, m.accents, m.loops, m.bass_replace]
                .iter()
                .filter(|b| **b)
                .count()
        };
        assert_eq!(on(Style::Plain), 0);
        assert_eq!(on(Style::Balanced), 2);
        assert_eq!(on(Style::Adventurous), 4);
        assert_eq!(MixStyle::preset(Style::Adventurous).reach(), Reach::Full);
        assert_eq!(MixStyle::preset(Style::Balanced).reach(), Reach::Gentle);
    }

    #[test]
    fn a_request_sets_the_style_then_its_switches_and_keeps_the_rest() {
        let mut m = MixStyle::default();
        m.apply(&json!({"play_percent": 40, "prefer_stems": false})).unwrap();
        m.apply(&json!({"style": "adventurous", "loops": false})).unwrap();
        assert_eq!(m.style, Style::Adventurous);
        assert!(m.effect_exits && m.accents && m.bass_replace && !m.loops);
        assert!(m.custom(), "a switch off from the preset reads as custom");
        // Choosing a style resets the switches, not the play length.
        assert_eq!((m.play_percent, m.prefer_stems), (40, false));
        m.apply(&json!({"style": "plain"})).unwrap();
        assert!(!m.any_flourish() && !m.custom());
        assert_eq!(m.play_percent, 40);
        // A single switch on, with the style left alone.
        m.apply(&json!({"effect_exits": true})).unwrap();
        assert!(m.effect_exits && m.custom());
    }

    #[test]
    fn a_bad_request_changes_nothing() {
        let mut m = MixStyle::default();
        let before = m;
        for bad in [
            json!({"style": "wild"}),
            json!({"style": 3}),
            json!({"loops": "yes"}),
            json!({"play_percent": 5}),
            json!({"play_percent": 101}),
            json!({"play_percent": "half"}),
            // The valid half of a bad request is not applied either.
            json!({"accents": true, "play_percent": 0}),
        ] {
            assert!(m.apply(&bad).is_err(), "{bad}");
            assert_eq!(m, before, "{bad}");
        }
    }

    #[test]
    fn the_setting_survives_a_session_file_and_old_files() {
        let mut m = MixStyle::preset(Style::Balanced);
        m.play_percent = 35;
        let back: MixStyle = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        // A session saved before the setting existed.
        let old: MixStyle = serde_json::from_str("{}").unwrap();
        assert_eq!(old, MixStyle::default());
        let partial: MixStyle = serde_json::from_str(r#"{"style":"adventurous"}"#).unwrap();
        assert_eq!(partial.style, Style::Adventurous);
        assert!(!partial.loops, "unnamed switches stay at the default");
    }

    #[test]
    fn the_report_names_every_control_in_one_line() {
        let v = MixStyle::default().json();
        assert_eq!(v["controls"].as_array().unwrap().len(), CONTROLS.len());
        for c in v["controls"].as_array().unwrap() {
            let about = c["about"].as_str().unwrap();
            assert!(!about.is_empty() && !about.contains('\n') && about.len() < 100, "{about}");
        }
        assert!(v["summary"].as_str().unwrap().starts_with("Plain: stem-led blends"));
        let mut m = MixStyle::preset(Style::Adventurous);
        m.play_percent = 30;
        assert!(m.summary().contains("30% of each track"));
    }

    #[test]
    fn thirty_percent_of_three_minutes_is_about_a_minute_on_phrases() {
        for bpm in (80..=170).step_by(2) {
            let bar = 240.0 / f64::from(bpm);
            let s = play_seconds(180.0, 30, bar);
            assert!((54.0..=66.0).contains(&s), "{bpm} BPM: {s}");
            // On a bar line of the grid: a whole number of bars.
            let bars = s / bar;
            assert!((bars - bars.round()).abs() < 1e-6, "{bpm} BPM: {bars} bars");
        }
        // At 128 BPM it is 4 whole phrases: 60 s.
        let s = play_seconds(180.0, 30, 240.0 / 128.0);
        assert!((s - 60.0).abs() < 1e-9, "{s}");
    }

    #[test]
    fn a_track_plays_a_sensible_minimum_and_never_past_its_end() {
        let bar = 240.0 / 128.0;
        // 20 % of a short 2-minute track is 24 s: raised to the minimum.
        let s = play_seconds(120.0, 20, bar);
        assert!(s >= MIN_PLAY_SECONDS.max(MIN_PLAY_BARS * bar) - 1e-9, "{s}");
        // 100 % is the whole track, unsnapped.
        assert_eq!(play_seconds(213.7, 100, bar), 213.7);
        // A track shorter than the minimum plays on its phrases, no more.
        let s = play_seconds(50.0, 20, bar);
        assert!(s <= 50.0 && s > 0.0, "{s}");
        assert_eq!(play_seconds(0.0, 30, bar), 0.0);
        assert_eq!(play_seconds(f64::NAN, 30, bar), 0.0);
        // More percent never plays less.
        let mut last = 0.0;
        for p in (MIN_PLAY_PERCENT..=100).step_by(5) {
            let s = play_seconds(300.0, p, bar);
            assert!(s >= last, "{p}: {s} < {last}");
            last = s;
        }
    }

    #[test]
    fn the_minutes_shown_follow_the_percentage() {
        assert!((approx_minutes(180.0, 30) - 0.9).abs() < 1e-9);
        assert!((approx_minutes(180.0, 100) - 3.0).abs() < 1e-9);
        assert!((approx_minutes(100.0, 20) - 40.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn stem_preparation_is_auto_by_default_and_survives_a_style_change() {
        let mut m = MixStyle::default();
        assert_eq!(m.stem_prep, StemPrep::Auto, "the user, 2026-11-14: Auto by default");
        m.apply(&json!({"stem_prep": "everything"})).unwrap();
        m.choose(Style::Adventurous);
        assert_eq!(m.stem_prep, StemPrep::Everything);
        assert!(!m.custom(), "not a style switch");
        assert!(m.apply(&json!({"stem_prep": "sometimes"})).is_err());
        assert_eq!(m.stem_prep, StemPrep::Everything, "nothing changes on an error");
        // A session saved before the setting existed: Auto.
        let old: MixStyle = serde_json::from_value(json!({"style": "plain", "prefer_stems": false})).unwrap();
        assert_eq!(old.stem_prep, StemPrep::Auto);
        assert_eq!(m.json()["stem_prep"], "everything");
    }

    #[test]
    fn stem_preparation_names_and_what_auto_means() {
        for p in StemPrep::ALL {
            assert_eq!(StemPrep::parse(p.id()), Some(p));
        }
        assert_eq!(StemPrep::parse("needed"), Some(StemPrep::WhenNeeded));
        assert_eq!(StemPrep::parse("When needed"), Some(StemPrep::WhenNeeded));
        assert_eq!(StemPrep::parse("all"), Some(StemPrep::Everything));
        // 25 s a track (this machine's GPU, 2026-11-14): Ahead.
        assert_eq!(StemPrep::Auto.resolve(Some(25.0), true), StemPrep::Ahead);
        assert_eq!(StemPrep::Auto.resolve(Some(240.0), false), StemPrep::WhenNeeded);
        assert_eq!(StemPrep::Auto.resolve(None, true), StemPrep::Ahead);
        assert_eq!(StemPrep::Auto.resolve(None, false), StemPrep::WhenNeeded);
        assert_eq!(StemPrep::Off.resolve(Some(5.0), true), StemPrep::Off);
        assert_eq!(StemPrep::Everything.resolve(Some(900.0), false), StemPrep::Everything);
    }
}
