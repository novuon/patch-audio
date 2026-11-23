//! Patch commands: the one way to change what Patch does.
//!
//! The command line, the UI, bar items and (later) the AI DJ all speak these.
//! Every command is validated against [`Limits`] for its [`Origin`] before it
//! reaches the engine, so an AI or a script can't do what the brief forbids
//! (sudden jumps in level, silencing the program, touching a take).
//!
//! On the control socket a command travels as one JSON line:
//! `cmd {"op":"mixer.set_gain","strip":"cliamp","db":-3.0}`. The CLI grammar
//! (`patch gain cliamp -3`) parses into the same type.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Deck {
    A,
    B,
    C,
    D,
}

impl Deck {
    pub fn id(self) -> &'static str {
        match self {
            Deck::A => "a",
            Deck::B => "b",
            Deck::C => "c",
            Deck::D => "d",
        }
    }
    pub fn index(self) -> usize {
        match self {
            Deck::A => 0,
            Deck::B => 1,
            Deck::C => 2,
            Deck::D => 3,
        }
    }
    fn parse(s: &str) -> Option<Deck> {
        match s {
            "a" | "A" => Some(Deck::A),
            "b" | "B" => Some(Deck::B),
            "c" | "C" => Some(Deck::C),
            "d" | "D" => Some(Deck::D),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Play,
    Pause,
    Toggle,
    Cue,
    /// Cue released: ends a held preview (back to the cue point).
    #[serde(rename = "cue_release")]
    CueRelease,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AiAction {
    Suggest,
    Prepare,
    Run,
    /// Suggest, and prepare the idle deck by itself before each mix-out.
    Auto,
    Off,
}

/// Beat FX (the engine's `fx` module).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FxKind {
    Echo,
    Reverb,
    Flanger,
    Trans,
    Crush,
    Roll,
    Brake,
    #[serde(rename = "pingpong")]
    PingPong,
    Phaser,
    Noise,
    Sweep,
    Tremolo,
    Pan,
    Drive,
}

impl FxKind {
    pub const ALL: [FxKind; 14] = [
        FxKind::Echo,
        FxKind::Reverb,
        FxKind::Flanger,
        FxKind::Trans,
        FxKind::Crush,
        FxKind::Roll,
        FxKind::Brake,
        FxKind::PingPong,
        FxKind::Phaser,
        FxKind::Noise,
        FxKind::Sweep,
        FxKind::Tremolo,
        FxKind::Pan,
        FxKind::Drive,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FxKind::Echo => "echo",
            FxKind::Reverb => "reverb",
            FxKind::Flanger => "flanger",
            FxKind::Trans => "trans",
            FxKind::Crush => "crush",
            FxKind::Roll => "roll",
            FxKind::Brake => "brake",
            FxKind::PingPong => "pingpong",
            FxKind::Phaser => "phaser",
            FxKind::Noise => "noise",
            FxKind::Sweep => "sweep",
            FxKind::Tremolo => "tremolo",
            FxKind::Pan => "pan",
            FxKind::Drive => "drive",
        }
    }

    pub fn parse(s: &str) -> Option<FxKind> {
        FxKind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// The beat length a DJ usually starts from.
    pub fn default_beats(self) -> f32 {
        match self {
            FxKind::Echo | FxKind::PingPong => 0.75,
            FxKind::Reverb | FxKind::Crush | FxKind::Brake | FxKind::Drive | FxKind::Pan => 1.0,
            FxKind::Flanger => 16.0,
            FxKind::Trans | FxKind::Roll | FxKind::Tremolo => 0.25,
            FxKind::Phaser | FxKind::Sweep => 8.0,
            FxKind::Noise => 16.0,
        }
    }

    /// "echo+sweep" (at most three, no repeats) → the chain.
    pub fn parse_chain(s: &str) -> Result<Vec<FxKind>, String> {
        let kinds = s
            .split('+')
            .map(|k| {
                FxKind::parse(k).ok_or_else(|| format!("no effect '{k}' ({})", FxKind::names()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        check_chain(&kinds)?;
        Ok(kinds)
    }

    /// "echo, reverb, …": every effect, for messages.
    pub fn names() -> String {
        FxKind::ALL.map(FxKind::name).join(", ")
    }
}

fn yes() -> bool {
    true
}

fn check_chain(kinds: &[FxKind]) -> Result<(), String> {
    if kinds.is_empty() || kinds.len() > 3 {
        return Err("a chain is 1 … 3 effects".into());
    }
    if kinds
        .iter()
        .enumerate()
        .any(|(i, k)| kinds[..i].contains(k))
    {
        return Err("each effect once per chain".into());
    }
    Ok(())
}

/// What a deck's sample command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SampleAction {
    Panel,
    Save,
}

/// Deck features a toggle flips.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    Keylock,
    Slip,
    Quantize,
    Pfl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Low,
    Mid,
    High,
}

impl Band {
    pub fn key(self) -> &'static str {
        match self {
            Band::Low => "low_db",
            Band::Mid => "mid_db",
            Band::High => "high_db",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordFormat {
    S24,
    F32,
}

/// How many hot cues a deck holds.
///
/// One number, in the crate the engine and the controller both depend on, so the
/// command's limit, the mapping importer and the feedback sources cannot drift
/// apart. Eight is what DJ hardware provides, and the imported Mixxx corpus names
/// cues 1 to 8 and nothing above.
pub const HOT_CUES: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Command {
    // Mixer
    #[serde(rename = "mixer.set_gain")]
    SetGain { strip: String, db: f32 },
    #[serde(rename = "mixer.set_mute")]
    SetMute { strip: String, muted: bool },
    // Router
    /// Send an application's stream through Patch (`through`) or straight to
    /// the system output.
    #[serde(rename = "router.route")]
    Route { stream: String, through: bool },
    #[serde(rename = "router.connect")]
    Connect { from: String, to: String },
    /// Put every stream back where it was before Patch.
    #[serde(rename = "router.restore")]
    Restore,
    // Recorder
    #[serde(rename = "recorder.start")]
    RecordStart {
        source: Option<String>,
        format: Option<RecordFormat>,
    },
    #[serde(rename = "recorder.stop")]
    RecordStop,
    #[serde(rename = "dj.stem")]
    StemControl {
        deck: Deck,
        stem: String,
        value: crate::stems::StemValue,
    },
    #[serde(rename = "dj.stem_ramp")]
    StemRamp {
        deck: Deck,
        stem: String,
        to: f32,
        beats: f64,
    },
    // DJ Booth
    #[serde(rename = "dj.start")]
    DjStart,
    /// Stop the decks. Refused while a deck carries audio unless `force`.
    #[serde(rename = "dj.stop")]
    DjStop {
        #[serde(default)]
        force: bool,
    },
    #[serde(rename = "dj.load_track")]
    LoadTrack { deck: Deck, query: String },
    #[serde(rename = "dj.transport")]
    DeckTransport { deck: Deck, action: Transport },
    /// Tempo fader, percent (+2.5 = 2.5 % faster).
    #[serde(rename = "dj.tempo")]
    DeckTempo { deck: Deck, percent: f32 },
    /// Master tempo: tempo changes keep the key.
    #[serde(rename = "dj.keylock")]
    DeckKeylock { deck: Deck, on: bool },
    /// Transpose, semitones (−12 … +12).
    #[serde(rename = "dj.key")]
    DeckKey { deck: Deck, semitones: i32 },
    /// Eject the deck (unload the track, stop playback).
    #[serde(rename = "dj.eject")]
    DeckEject { deck: Deck },
    /// DJ EQ band: `db` −26 … +6, or None for a kill.
    #[serde(rename = "dj.eq")]
    DeckEq {
        deck: Deck,
        band: Band,
        db: Option<f32>,
    },
    /// Filter knob: −1 low-pass … 0 off … +1 high-pass.
    #[serde(rename = "dj.filter")]
    DeckFilter { deck: Deck, amount: f32 },
    /// Filter knob by `amount`: the endless form, for hardware whose knob has no
    /// position to send and can only describe movement.
    #[serde(rename = "dj.filter_step")]
    DeckFilterStep { deck: Deck, amount: f32 },
    /// Queue a track (path) to play next, at the end or at `at`.
    #[serde(rename = "dj.queue_add")]
    QueueAdd {
        path: String,
        #[serde(default)]
        at: Option<usize>,
    },
    #[serde(rename = "dj.queue_remove")]
    QueueRemove { index: usize },
    #[serde(rename = "dj.queue_clear")]
    QueueClear,
    /// Deterministic auto-mix on/off, with a direction for its choices.
    #[serde(rename = "dj.automix")]
    AutoMix {
        on: bool,
        #[serde(default)]
        direction: Option<crate::intent::Direction>,
    },
    /// Mix into the next track (queue first) at the next bar.
    #[serde(rename = "dj.transition_now")]
    TransitionNow,
    /// The AI DJ's set: start (length, arc, tempo corridor) or stop.
    #[serde(rename = "dj.set")]
    DjSet {
        on: bool,
        #[serde(default)]
        minutes: Option<f64>,
        #[serde(default)]
        arc: Option<String>,
        #[serde(default)]
        bpm: Option<(f64, f64)>,
        /// Play only from this folder.
        #[serde(default)]
        crate_dir: Option<String>,
        /// Limit this set to tracks in a library collection.
        #[serde(default)]
        collection: Option<i64>,
        /// Explicit library selection to play first.
        #[serde(default)]
        opener: Option<String>,
    },
    /// Explicit unified workspace session operations.
    #[serde(rename = "dj.session")]
    DjSession {
        request: crate::dj_workspace::SessionAction,
    },
    /// How far the AI DJ may go (observe, suggest, prepare, act).
    #[serde(rename = "dj.ai_mode")]
    AiMode { mode: crate::aivis::Mode },
    /// Lock a candidate (by id, or rank 1…) to be prepared next.
    #[serde(rename = "dj.ai_pick")]
    AiPick {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        rank: Option<usize>,
    },
    /// Reject a candidate (by id or rank): it leaves the possibilities.
    #[serde(rename = "dj.ai_drop")]
    AiDrop {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        rank: Option<usize>,
    },
    /// Run the prepared transition (Prepare mode).
    #[serde(rename = "dj.ai_approve")]
    AiApprove,
    /// Cancel what's prepared or running; the AI goes back to suggesting.
    #[serde(rename = "dj.ai_cancel")]
    AiCancel,
    /// Master EQ: change band `band` (0-based); unset fields stay.
    #[serde(rename = "eq.band")]
    EqBand {
        /// "master" (default) or a channel: "bus", "deck-a", "deck-b".
        #[serde(default)]
        target: Option<String>,
        band: u8,
        #[serde(default)]
        on: Option<bool>,
        #[serde(default)]
        kind: Option<crate::eq::EqKind>,
        #[serde(default)]
        freq: Option<f32>,
        #[serde(default)]
        gain_db: Option<f32>,
        #[serde(default)]
        q: Option<f32>,
    },
    /// Master EQ in or out (the settings stay).
    #[serde(rename = "eq.bypass")]
    EqBypass {
        #[serde(default)]
        target: Option<String>,
        on: bool,
    },
    /// An EQ's input gain before its bands (dB, ±18): a correction's
    /// preamp, so its boosts don't reach the limiter.
    #[serde(rename = "eq.gain")]
    EqGain {
        #[serde(default)]
        target: Option<String>,
        db: f32,
    },
    /// An EQ back to flat.
    #[serde(rename = "eq.reset")]
    EqReset {
        #[serde(default)]
        target: Option<String>,
    },
    /// Give an application its own EQ (its own engine channel), or take
    /// it away (its audio goes back where it came from).
    #[serde(rename = "eq.app")]
    EqApp { app: String, on: bool },
    /// Load a preset (built-in or saved; by id or name) into an EQ.
    #[serde(rename = "eq.preset")]
    EqPreset {
        #[serde(default)]
        target: Option<String>,
        name: String,
    },
    /// Save an EQ's settings as a preset (patch-eq JSON, in
    /// ~/.config/omarchy-patch/eq-presets).
    #[serde(rename = "eq.save")]
    EqSave {
        #[serde(default)]
        target: Option<String>,
        name: String,
    },
    /// Words to the AI DJ ("keep the energy up", "around 125 BPM").
    #[serde(rename = "dj.say")]
    DjSay { text: String },
    /// Beat FX on a deck: any of effect, on/off, length in beats, depth.
    #[serde(rename = "dj.fx")]
    DeckFx {
        deck: Deck,
        #[serde(default)]
        kind: Option<FxKind>,
        #[serde(default)]
        on: Option<bool>,
        #[serde(default)]
        beats: Option<f32>,
        #[serde(default)]
        depth: Option<f32>,
    },
    /// Beat FX chain on a deck: up to three effects in series, all on one
    /// depth knob (on unless `on` says otherwise).
    #[serde(rename = "dj.fx_chain")]
    DeckFxChain {
        deck: Deck,
        kinds: Vec<FxKind>,
        #[serde(default = "yes")]
        on: bool,
        #[serde(default)]
        beats: Option<f32>,
        #[serde(default)]
        depth: Option<f32>,
    },
    /// Hot cue 1–8: jump to it, or set it (snapped to the beat) when empty.
    #[serde(rename = "dj.hot_cue")]
    HotCue {
        deck: Deck,
        n: u8,
        #[serde(default)]
        clear: bool,
    },
    /// Put the loop's in-point here (the hand sets the start of a manual loop).
    #[serde(rename = "dj.loop_in")]
    DeckLoopIn { deck: Deck },
    /// Put the loop's out-point here and engage the loop. Ignored unless an
    /// in-point was set first, which is what makes this a two-button loop.
    #[serde(rename = "dj.loop_out")]
    DeckLoopOut { deck: Deck },
    /// Play the deck backwards, or stop doing so. Mixxx's `reverse` control is a
    /// maintainable switch, so a held pad is a press and a release.
    #[serde(rename = "dj.reverse")]
    DeckReverse { deck: Deck, on: bool },
    /// Loop `beats` from the current beat (quantized); None exits the loop.
    #[serde(rename = "dj.loop")]
    DeckLoop { deck: Deck, beats: Option<f32> },
    /// Exit an active loop, or restart the last loop at its original in-point.
    /// With no loop history this does nothing; it never creates a new loop.
    #[serde(rename = "dj.reloop")]
    DeckReloop { deck: Deck },
    #[serde(rename = "dj.loop_set")]
    DeckLoopSet {
        deck: Deck,
        from: f64,
        to: f64,
        play: bool,
    },
    #[serde(rename = "dj.loop_adjust")]
    DeckLoopAdjust { deck: Deck, out: bool, seconds: f64 },
    #[serde(rename = "dj.censor")]
    DeckCensor { deck: Deck, on: bool },
    #[serde(rename = "dj.tempo_master")]
    DeckMaster { deck: Deck },
    #[serde(rename = "dj.cue_recall")]
    DeckCueRecall { deck: Deck, seconds: f64 },
    /// Beat jump (negative = back); keeps the phase.
    #[serde(rename = "dj.jump")]
    DeckJump { deck: Deck, beats: f32 },
    #[serde(rename = "dj.slip")]
    DeckSlip { deck: Deck, on: bool },
    /// The deck's sample maker: open its panel, or save a sample with the
    /// panel's last settings (a controller's "sample" button).
    #[serde(rename = "dj.sample")]
    DeckSample { deck: Deck, action: SampleAction },
    #[serde(rename = "dj.quantize")]
    DeckQuantize { deck: Deck, on: bool },
    /// Headphone cue (PFL): hear the deck in the headphones, pre-fader.
    #[serde(rename = "dj.pfl")]
    Pfl { deck: Deck, on: bool },
    /// Match tempo to the other deck and line up the beats.
    #[serde(rename = "dj.sync")]
    DeckSync { deck: Deck },
    /// 0.0 = deck A only, 1.0 = deck B only.
    #[serde(rename = "dj.crossfader")]
    Crossfader { position: f32 },
    // Relative, toggle and controller forms (keys, MIDI, HID, AI).
    /// The AI DJ: suggest the next track, prepare it (load, cue, sync the
    /// idle deck), run the suggested transition (humans only), or stop.
    #[serde(rename = "dj.ai")]
    Ai {
        action: AiAction,
        #[serde(default)]
        direction: Option<crate::intent::Direction>,
    },
    /// Load a library track by path (the app checks it is in the library).
    #[serde(rename = "dj.load_path")]
    DeckLoadPath { deck: Deck, path: String },
    /// Playhead to `seconds` (cue a mix-in point).
    #[serde(rename = "dj.seek")]
    DeckSeek { deck: Deck, seconds: f64 },
    /// Scratch by hand: put the playhead at `seconds`, or move it by `delta`
    /// seconds from where it is now, with the platter following the hand.
    ///
    /// `velocity` is the starting platter speed (−4…4, 0 = held under the
    /// hand). The deck re-aims its speed at the target until the hand leaves,
    /// so a jog wheel's ticks glide like a platter instead of jumping; a
    /// controller that can only send movement uses `delta`, and a touch or
    /// platter-release control ends it with [`Command::DeckScratchEnd`].
    #[serde(rename = "dj.scratch")]
    DeckScratch {
        deck: Deck,
        #[serde(default)]
        seconds: Option<f64>,
        #[serde(default)]
        delta: Option<f64>,
        #[serde(default)]
        velocity: f32,
    },
    /// The hand left the platter: playback resumes as it was.
    #[serde(rename = "dj.scratch_end")]
    DeckScratchEnd { deck: Deck },
    /// Tempo fader by `percent` (+0.1 = 0.1 % faster).
    #[serde(rename = "dj.tempo_step")]
    DeckTempoStep { deck: Deck, percent: f32 },
    /// Next tempo range (±8 → ±16 → ±50 %).
    #[serde(rename = "dj.tempo_range")]
    DeckRange { deck: Deck },
    #[serde(rename = "dj.toggle")]
    DeckToggle { deck: Deck, what: Feature },
    #[serde(rename = "dj.key_step")]
    DeckKeyStep { deck: Deck, semitones: i32 },
    /// Engage a loop of `beats`, or leave it when that loop is on.
    #[serde(rename = "dj.loop_toggle")]
    DeckLoopToggle { deck: Deck, beats: f32 },
    #[serde(rename = "dj.loop_scale")]
    DeckLoopScale { deck: Deck, factor: f32 },
    /// Nudge while held (±0.1 = ±10 %); 0 releases.
    #[serde(rename = "dj.bend")]
    DeckBend { deck: Deck, amount: f32 },
    #[serde(rename = "dj.crossfader_step")]
    CrossfaderStep { delta: f32 },
    /// Channel fader, 0 … 1 (a controller's fader).
    #[serde(rename = "dj.fader")]
    DeckFader { deck: Deck, position: f32 },
    /// A transition plan (see `crate::intent`), compiled and validated for
    /// the origin at `level`; it runs on the outgoing deck's beat clock.
    #[serde(rename = "dj.plan")]
    DjPlan {
        plan: Box<crate::intent::TransitionPlan>,
        level: crate::intent::ControlLevel,
    },
    #[serde(rename = "dj.plan_cancel")]
    DjPlanCancel,
    /// EQ knob, −1 (kill) … 0 … +1 (+6 dB) (a controller's knob).
    #[serde(rename = "dj.eq_knob")]
    DeckEqKnob { deck: Deck, band: Band, value: f32 },
}

/// Who is asking. Limits get stricter further down the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// A person at the UI.
    User,
    /// The command line, a bar item or a keybinding.
    Cli,
    /// The AI DJ (or any automation).
    Ai,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    pub min_gain_db: f32,
    pub max_gain_db: f32,
    /// Largest gain change a single command may make (dB). Protects ears
    /// and speakers from jumps; people at a fader are exempt.
    pub max_gain_step_db: Option<f32>,
    pub may_mute: bool,
    pub may_reroute: bool,
    pub may_record: bool,
    /// Largest crossfader move per command (0…1).
    pub max_crossfade_step: Option<f32>,
}

impl Limits {
    pub fn for_origin(origin: Origin) -> Limits {
        let open = Limits {
            min_gain_db: -96.0,
            max_gain_db: 12.0,
            max_gain_step_db: None,
            may_mute: true,
            may_reroute: true,
            may_record: true,
            max_crossfade_step: None,
        };
        match origin {
            Origin::User | Origin::Cli => open,
            // The AI DJ mixes; it doesn't silence the program, change where
            // audio goes, or touch a recording.
            Origin::Ai => Limits {
                max_gain_db: 6.0,
                max_gain_step_db: Some(6.0),
                may_mute: false,
                may_reroute: false,
                may_record: false,
                max_crossfade_step: Some(0.25),
                ..open
            },
        }
    }
}

/// What validation may need to know about the current state.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// Current gain of the strip named by a `SetGain`, if known.
    pub current_gain_db: Option<f32>,
    pub crossfader: Option<f32>,
    pub recording: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn reject<T>(why: impl Into<String>) -> Result<T, Rejected> {
    Err(Rejected(why.into()))
}

/// Check `cmd` against the limits for `origin`. Returns the command to run,
/// which is the same command, never a silently adjusted one: a request
/// outside the limits is refused, with the reason.
pub fn validate(cmd: Command, origin: Origin, ctx: &Context) -> Result<Command, Rejected> {
    let l = Limits::for_origin(origin);
    let name_ok = |s: &str| !s.trim().is_empty() && s.len() <= 256;
    match &cmd {
        Command::StemControl { stem, value, .. } => {
            if !crate::stems::valid_id(stem) {
                return reject("Invalid stem identity");
            }
            value.validate().map_err(Rejected)?;
            if origin == Origin::Ai {
                return reject("stem changes require an authorized timed plan");
            }
        }
        Command::StemRamp {
            stem, to, beats, ..
        } => {
            if !crate::stems::valid_id(stem) || !beats.is_finite() || !(0.0..=256.0).contains(beats)
            {
                return reject("Invalid stem ramp");
            }
            crate::stems::StemValue::Gain(*to)
                .validate()
                .map_err(Rejected)?;
        }
        Command::SetGain { strip, db } => {
            if !name_ok(strip) {
                return reject("no strip named");
            }
            if !db.is_finite() {
                return reject("gain must be a number of dB");
            }
            if *db < l.min_gain_db || *db > l.max_gain_db {
                return reject(format!(
                    "gain {db:+.1} dB is outside {:+.0} … {:+.0} dB",
                    l.min_gain_db, l.max_gain_db
                ));
            }
            if let (Some(step), Some(now)) = (l.max_gain_step_db, ctx.current_gain_db)
                && (db - now).abs() > step
            {
                return reject(format!(
                    "gain change {:+.1} dB is larger than {step:.0} dB",
                    db - now
                ));
            }
        }
        Command::SetMute { strip, muted } => {
            if !name_ok(strip) {
                return reject("no strip named");
            }
            if *muted && !l.may_mute {
                return reject("muting is not allowed from here");
            }
        }
        Command::Route { stream, .. } => {
            if !name_ok(stream) {
                return reject("no stream named");
            }
            if !l.may_reroute {
                return reject("rerouting is not allowed from here");
            }
        }
        Command::Connect { from, to } => {
            if !name_ok(from) || !name_ok(to) {
                return reject("connect needs a source and a destination");
            }
            if from == to {
                return reject("a node can't feed itself");
            }
            if !l.may_reroute {
                return reject("rerouting is not allowed from here");
            }
        }
        Command::Restore if !l.may_reroute => return reject("rerouting is not allowed from here"),
        Command::Restore => {}
        Command::RecordStart { .. } | Command::RecordStop if !l.may_record => {
            return reject("the recorder is not available from here");
        }
        Command::RecordStart { .. } if ctx.recording => return reject("already recording"),
        Command::RecordStart { .. } | Command::RecordStop => {}
        Command::DjStart | Command::DjStop { .. } => {}
        Command::LoadTrack { query, .. } => {
            if !name_ok(query) {
                return reject("load needs a track name");
            }
        }
        Command::DeckTransport { .. } | Command::DeckSync { .. } | Command::DeckKeylock { .. } | Command::DeckEject { .. } => {}
        Command::DeckTempo { percent, .. } => {
            if !percent.is_finite() || percent.abs() > 50.0 {
                return reject("tempo is −50 … +50 %");
            }
        }
        Command::DeckEq { db, .. } => {
            if let Some(d) = db {
                if !d.is_finite() || !(-26.0..=6.0).contains(d) {
                    return reject("EQ is −26 … +6 dB, or kill");
                }
            } else if !l.may_mute {
                return reject("kills are not allowed from here");
            }
        }
        Command::HotCue { n, .. } if !(1..=HOT_CUES).contains(&usize::from(*n)) => {
            return reject("hot cues are 1 … 8");
        }
        Command::HotCue { .. }
        | Command::Pfl { .. }
        | Command::DeckSlip { .. }
        | Command::DeckSample { .. }
        | Command::DeckQuantize { .. } => {}
        Command::DeckRange { .. } | Command::DeckToggle { .. } | Command::DjPlanCancel => {}
        Command::Ai {
            action: AiAction::Run,
            ..
        } if origin == Origin::Ai => {
            return reject("Auto Mix suggests and prepares; a human runs the transition");
        }
        Command::Ai {
            action: AiAction::Auto,
            ..
        } if origin == Origin::Ai => return reject("only a human puts Auto Mix on auto"),
        Command::Ai { .. } => {}
        Command::DeckLoadPath { path, .. } if path.trim().is_empty() || path.contains("://") => {
            return reject("load needs a local library track");
        }
        Command::DeckLoadPath { .. } => {}
        Command::DeckSeek { seconds, .. } if !(seconds.is_finite() && *seconds >= 0.0) => {
            return reject("seek needs a position ≥ 0 s");
        }
        Command::DeckSeek { .. } => {}
        // A scratch is one of two forms, never both and never neither, and its
        // movement is bounded: a controller that sends a wild tick must not be
        // able to throw the playhead across a set.
        Command::DeckScratch {
            seconds,
            delta,
            velocity,
            ..
        } => {
            match (seconds, delta) {
                (Some(seconds), None) if !(seconds.is_finite() && *seconds >= 0.0) => {
                    return reject("scratch needs a position ≥ 0 s");
                }
                (None, Some(delta)) if !(delta.is_finite() && delta.abs() <= 60.0) => {
                    return reject("scratch movement is ±60 s at most");
                }
                (Some(_), None) | (None, Some(_)) => {}
                _ => return reject("scratch needs exactly one of seconds or delta"),
            }
            if !(velocity.is_finite() && (-4.0..=4.0).contains(velocity)) {
                return reject("scratch velocity is −4…4");
            }
        }
        Command::DeckScratchEnd { .. } => {}
        // Plans are validated step by step when compiled (crate::intent).
        Command::DjPlan { level, .. } if *level > crate::intent::ControlLevel::ceiling(origin) => {
            return reject(format!(
                "{origin:?} plans may go up to {:?}",
                crate::intent::ControlLevel::ceiling(origin)
            ));
        }
        Command::DjPlan { .. } => {}
        Command::DeckTempoStep { percent, .. }
            if !(percent.is_finite() && percent.abs() <= 10.0) =>
        {
            return reject("tempo steps are up to ±10 %");
        }
        Command::DeckTempoStep { .. } => {}
        Command::DeckKeyStep { semitones, .. } if semitones.abs() > 12 => {
            return reject("key steps are up to ±12 semitones");
        }
        Command::DeckKeyStep { .. } => {}
        Command::DeckLoopToggle { beats, .. }
            if !(beats.is_finite() && (1.0 / 32.0..=128.0).contains(beats)) =>
        {
            return reject("loops are 1/32 … 128 beats");
        }
        Command::DeckLoopToggle { .. } => {}
        Command::DeckLoopScale { factor, .. } if !(*factor == 0.5 || *factor == 2.0) => {
            return reject("loops halve (0.5) or double (2)");
        }
        Command::DeckLoopScale { .. } => {}
        Command::DeckBend { amount, .. } if !(amount.is_finite() && amount.abs() <= 0.1) => {
            return reject("bend is up to ±10 %");
        }
        Command::DeckBend { .. } => {}
        Command::CrossfaderStep { delta } if !(delta.is_finite() && delta.abs() <= 1.0) => {
            return reject("crossfader steps are up to ±1");
        }
        Command::CrossfaderStep { delta } => {
            if let Some(step) = l.max_crossfade_step
                && delta.abs() > step
            {
                return reject(format!(
                    "crossfader move {:.2} is larger than {step:.2}",
                    delta
                ));
            }
        }
        Command::DeckFader { position, .. }
            if !(position.is_finite() && (0.0..=1.0).contains(position)) =>
        {
            return reject("faders are 0 … 1");
        }
        Command::DeckFader { .. } => {}
        Command::DeckEqKnob { value, .. }
            if !(value.is_finite() && (-1.0..=1.0).contains(value)) =>
        {
            return reject("EQ knobs are −1 … 1");
        }
        Command::DeckEqKnob { value, .. } if *value <= -0.97 && !l.may_mute => {
            return reject("kills are not allowed from here");
        }
        Command::DeckEqKnob { .. } => {}
        Command::DeckLoop { beats: Some(b), .. }
            if !(b.is_finite() && (1.0 / 32.0..=128.0).contains(b)) =>
        {
            return reject("loops are 1/32 … 128 beats");
        }
        Command::DeckLoop { .. } => {}
        Command::DeckCueRecall { .. }
        | Command::DeckLoopSet { .. }
        | Command::DeckLoopAdjust { .. }
        | Command::DeckCensor { .. }
        | Command::DeckMaster { .. }
            if origin == Origin::Ai =>
        {
            return reject("player memory, loop edits, censor and master selection require a human");
        }
        Command::DeckCueRecall { seconds, .. }
            if !(seconds.is_finite() && (0.0..=86_400.0).contains(seconds)) =>
        {
            return reject("cue must be source seconds");
        }
        Command::DeckCueRecall { .. } => {}
        Command::DeckLoopSet { from, to, .. }
            if !(from.is_finite()
                && to.is_finite()
                && *from >= 0.0
                && *to > *from
                && *to <= 86_400.0) =>
        {
            return reject("loop bounds must be ordered source seconds");
        }
        Command::DeckLoopSet { .. } => {}
        Command::DeckLoopAdjust { seconds, .. }
            if !(seconds.is_finite() && seconds.abs() <= 10.0) =>
        {
            return reject("loop adjustment is at most ten seconds");
        }
        Command::DeckLoopAdjust { .. }
        | Command::DeckCensor { .. }
        | Command::DeckMaster { .. } => {}
        // A manual loop's points, and reverse: the deck is the whole argument.
        Command::DeckLoopIn { .. }
        | Command::DeckLoopOut { .. }
        | Command::DeckReverse { .. }
        | Command::DeckReloop { .. } => {}
        Command::DeckJump { beats, .. } if !(beats.is_finite() && beats.abs() <= 512.0) => {
            return reject("jumps are up to ±512 beats");
        }
        Command::DeckJump { .. } => {}
        Command::DeckFx { .. } | Command::DeckFxChain { .. } if origin == Origin::Ai => {
            return reject("performance effects are the DJ's");
        }
        Command::AutoMix { on: true, .. } if origin == Origin::Ai => {
            return reject("only a human puts the decks on auto-mix");
        }
        Command::TransitionNow if origin == Origin::Ai => {
            return reject("Auto Mix proposes transitions; a human runs them");
        }
        Command::QueueAdd { path, .. } if path.trim().is_empty() => {
            return reject("queue a track by its path");
        }
        Command::DjSession { .. } if origin == Origin::Ai => {
            return reject("only a human controls a set");
        }
        Command::DjSession { request } => {
            use crate::dj_workspace::SessionAction;
            match request {
                SessionAction::Start { set } => set.validate().map_err(Rejected)?,
                SessionAction::StartTest { set, bars } => {
                    set.validate().map_err(Rejected)?;
                    if ![8, 16].contains(bars) {
                        return reject("Test sessions require 8 or 16 bars");
                    }
                }
                SessionAction::Sources { collections }
                    if collections.len() > 100 || collections.iter().any(|id| *id <= 0) =>
                {
                    return reject("invalid collection selection");
                }
                _ => {}
            }
        }
        Command::DjSet { on: true, .. } if origin == Origin::Ai => {
            return reject("only a human starts a set");
        }
        Command::DjSet {
            minutes: Some(m), ..
        } if !(m.is_finite() && (10.0..=600.0).contains(m)) => {
            return reject("a set is 10 … 600 minutes");
        }
        Command::DjSet { arc: Some(a), .. }
            if !["journey", "warmup", "peak"].contains(&a.as_str()) =>
        {
            return reject("arcs: journey, warmup, peak");
        }
        Command::DjSay { text } if text.trim().is_empty() => return reject("say something"),
        Command::AiMode { .. }
        | Command::AiPick { .. }
        | Command::AiDrop { .. }
        | Command::AiApprove
        | Command::AiCancel
            if origin == Origin::Ai =>
        {
            return reject("the DJ decides what Auto Mix may do and which way it goes");
        }
        Command::AiPick {
            id: None,
            rank: None,
        }
        | Command::AiDrop {
            id: None,
            rank: None,
        } => return reject("which candidate? (id or rank)"),
        Command::AiMode { .. }
        | Command::AiPick { .. }
        | Command::AiDrop { .. }
        | Command::AiApprove
        | Command::AiCancel => {}
        Command::EqBand { band, .. } if *band as usize >= crate::eq::BANDS => {
            return reject("the EQ has bands 1 … 8");
        }
        Command::EqBand { freq: Some(f), .. }
            if !(f.is_finite()
                && (crate::eq::FREQ_RANGE.0..=crate::eq::FREQ_RANGE.1).contains(f)) =>
        {
            return reject("EQ frequency is 20 … 20 000 Hz");
        }
        Command::EqBand {
            gain_db: Some(g), ..
        } if !(g.is_finite() && g.abs() <= crate::eq::GAIN_RANGE) => {
            return reject("EQ gain is ±18 dB");
        }
        Command::EqBand { q: Some(q), .. }
            if !(q.is_finite() && (crate::eq::Q_RANGE.0..=crate::eq::Q_RANGE.1).contains(q)) =>
        {
            return reject("EQ Q is 0.1 … 18");
        }
        Command::EqGain { db, .. } if !(db.is_finite() && db.abs() <= crate::eq::GAIN_RANGE) => {
            return reject("EQ input gain is ±18 dB");
        }
        Command::EqBand {
            target: Some(t), ..
        }
        | Command::EqBypass {
            target: Some(t), ..
        }
        | Command::EqGain {
            target: Some(t), ..
        }
        | Command::EqReset { target: Some(t) }
        | Command::EqPreset {
            target: Some(t), ..
        }
        | Command::EqSave {
            target: Some(t), ..
        } if !crate::eq::valid_target(t) => {
            return reject("EQ target is master, apps (bus), deck-a or deck-b");
        }
        Command::EqSave { name, .. }
            if name.trim().is_empty() || name.len() > 80 || name.contains(['/', '\\', '\0']) =>
        {
            return reject("a preset name is 1 … 80 characters, no slashes");
        }
        Command::EqPreset { name, .. } if name.trim().is_empty() => return reject("which preset?"),
        Command::EqApp { app, .. } if app.trim().is_empty() || app.len() > 100 => {
            return reject("which application?");
        }
        Command::EqApp { .. } => {}
        Command::EqBand { .. }
        | Command::EqBypass { .. }
        | Command::EqGain { .. }
        | Command::EqReset { .. }
        | Command::EqPreset { .. }
        | Command::EqSave { .. } => {}
        Command::QueueAdd { .. }
        | Command::QueueRemove { .. }
        | Command::QueueClear
        | Command::AutoMix { .. }
        | Command::TransitionNow
        | Command::DjSet { .. }
        | Command::DjSay { .. } => {}
        Command::DeckFx { beats: Some(b), .. }
            if !(b.is_finite() && (1.0 / 32.0..=32.0).contains(b)) =>
        {
            return reject("effects run 1/32 … 32 beats");
        }
        Command::DeckFx { depth: Some(d), .. } if !(d.is_finite() && (0.0..=1.0).contains(d)) => {
            return reject("effect depth is 0 … 1");
        }
        Command::DeckFx { .. } => {}
        Command::DeckFxChain { kinds, .. } if check_chain(kinds).is_err() => {
            return reject(check_chain(kinds).unwrap_err());
        }
        Command::DeckFxChain { beats: Some(b), .. }
            if !(b.is_finite() && (1.0 / 32.0..=32.0).contains(b)) =>
        {
            return reject("effects run 1/32 … 32 beats");
        }
        Command::DeckFxChain { depth: Some(d), .. }
            if !(d.is_finite() && (0.0..=1.0).contains(d)) =>
        {
            return reject("effect depth is 0 … 1");
        }
        Command::DeckFxChain { .. } => {}
        Command::DeckFilter { amount, .. } => {
            if !amount.is_finite() || !(-1.0..=1.0).contains(amount) {
                return reject("filter is −1 (low-pass) … +1 (high-pass)");
            }
        }
        Command::DeckFilterStep { amount, .. } => {
            // A step, not a position: bounded by the travel it moves over, so a
            // step larger than the whole range is refused rather than clamped
            // into a silent full sweep.
            if !amount.is_finite() || !(-1.0..=1.0).contains(amount) {
                return reject("a filter step is −1 … +1");
            }
        }
        Command::DeckKey { semitones, .. } => {
            if semitones.abs() > 12 {
                return reject("key shift is −12 … +12 semitones");
            }
        }
        Command::Crossfader { position } => {
            if !position.is_finite() || !(0.0..=1.0).contains(position) {
                return reject("crossfader takes 0.0 (A) … 1.0 (B)");
            }
            if let (Some(step), Some(now)) = (l.max_crossfade_step, ctx.crossfader)
                && (position - now).abs() > step
            {
                return reject(format!(
                    "crossfader move {:.2} is larger than {step:.2}",
                    position - now
                ));
            }
        }
    }
    Ok(cmd)
}

impl Command {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("commands serialize")
    }

    pub fn from_json(s: &str) -> Result<Command, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }

    /// The CLI grammar, as typed after `omarchy-patch`.
    pub const USAGE: &'static str = "route <app> | bypass <app> | send <from> <to> | restore | \
gain <strip> <dB> | mute <strip> | unmute <strip> | record start [source] [s24|f32] | record stop | \
dj start|stop [--force] | deck a|b|c|d load <name>|play|pause|toggle|cue|eject|tempo <±%>|keylock on|off|key <±st>|sync | xfader <0…1>";

    /// Parse the CLI grammar (words after the program name).
    pub fn parse_cli(words: &[&str]) -> Result<Command, String> {
        let db = |s: &str| {
            s.trim_end_matches("dB")
                .trim_end_matches("db")
                .parse::<f32>()
                .map_err(|_| format!("'{s}' is not a number of dB"))
        };
        Ok(match words {
            ["route", app] => Command::Route {
                stream: app.to_string(),
                through: true,
            },
            ["bypass", app] => Command::Route {
                stream: app.to_string(),
                through: false,
            },
            ["send", from, to] => Command::Connect {
                from: from.to_string(),
                to: to.to_string(),
            },
            ["restore"] => Command::Restore,
            ["gain", strip, v] => Command::SetGain {
                strip: strip.to_string(),
                db: db(v)?,
            },
            ["mute", strip] => Command::SetMute {
                strip: strip.to_string(),
                muted: true,
            },
            ["unmute", strip] => Command::SetMute {
                strip: strip.to_string(),
                muted: false,
            },
            ["record", "stop"] => Command::RecordStop,
            ["record", "start", rest @ ..] => {
                let format = rest.iter().find_map(|w| match *w {
                    "s24" => Some(RecordFormat::S24),
                    "f32" => Some(RecordFormat::F32),
                    _ => None,
                });
                let source = rest
                    .iter()
                    .find(|w| **w != "s24" && **w != "f32")
                    .map(|s| s.to_string());
                Command::RecordStart { source, format }
            }
            ["dj", "start"] => Command::DjStart,
            ["dj", "stop"] => Command::DjStop { force: false },
            ["dj", "stop", "--force"] => Command::DjStop { force: true },
            ["dj", "blend", from, to] => Command::DjPlan {
                plan: Box::new(crate::intent::blend(
                    Deck::parse(from).ok_or("deck is a, b, c, or d")?,
                    Deck::parse(to).ok_or("deck is a, b, c, or d")?,
                    -1.0,
                )),
                level: crate::intent::ControlLevel::Execute,
            },
            ["dj", "cancel"] => Command::DjPlanCancel,
            ["dj", "queue", "add", p @ ..] if !p.is_empty() => Command::QueueAdd {
                path: p.join(" "),
                at: None,
            },
            ["dj", "queue", "remove", i] => Command::QueueRemove {
                index: i.parse().map_err(|_| "queue remove <index>")?,
            },
            ["dj", "queue", "clear"] => Command::QueueClear,
            ["dj", "automix", on, rest @ ..] => Command::AutoMix {
                on: match *on {
                    "on" => true,
                    "off" => false,
                    _ => return Err("dj automix on|off [build|hold|release]".into()),
                },
                direction: match rest.first().copied() {
                    Some("build") => Some(crate::intent::Direction::Build),
                    Some("hold") => Some(crate::intent::Direction::Hold),
                    Some("release") => Some(crate::intent::Direction::Release),
                    None => None,
                    Some(_) => return Err("direction is build, hold or release".into()),
                },
            },
            ["dj", "next"] => Command::TransitionNow,
            ["dj", "set", "stop"] => Command::DjSet {
                on: false,
                minutes: None,
                arc: None,
                bpm: None,
                crate_dir: None,
                collection: None,
                opener: None,
            },
            ["dj", "set", "start", rest @ ..] => Command::DjSet {
                on: true,
                minutes: rest.iter().find_map(|w| w.parse::<f64>().ok()),
                arc: rest
                    .iter()
                    .find(|w| ["journey", "warmup", "peak"].contains(w))
                    .map(|w| w.to_string()),
                bpm: None,
                // Any other word is the crate: a folder to play from.
                crate_dir: rest
                    .iter()
                    .find(|w| {
                        w.parse::<f64>().is_err() && !["journey", "warmup", "peak"].contains(w)
                    })
                    .map(|w| {
                        std::fs::canonicalize(w)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| w.to_string())
                    }),
                collection: None,
                opener: None,
            },
            ["dj", "say", words @ ..] if !words.is_empty() => Command::DjSay {
                text: words.join(" "),
            },
            ["dj", "ai", "mode", m] => Command::AiMode {
                mode: serde_json::from_value(serde_json::json!(m))
                    .map_err(|_| "dj ai mode observe|suggest|prepare|act")?,
            },
            ["dj", "ai", "pick", n] => Command::AiPick {
                id: None,
                rank: Some(n.parse().map_err(|_| "dj ai pick <rank>")?),
            },
            ["dj", "ai", "drop", n] => Command::AiDrop {
                id: None,
                rank: Some(n.parse().map_err(|_| "dj ai drop <rank>")?),
            },
            ["dj", "ai", "approve"] => Command::AiApprove,
            ["dj", "ai", "cancel"] => Command::AiCancel,
            // eq [master|apps|dj|mic-N] band|bypass|reset …
            ["eq", rest @ ..] if !rest.is_empty() => {
                if let ["app", name @ ..] = rest {
                    let off = name.last() == Some(&"off");
                    let name = if off { &name[..name.len() - 1] } else { name };
                    if name.is_empty() {
                        return Err("eq app <name> [off]".into());
                    }
                    return Ok(Command::EqApp {
                        app: name.join(" "),
                        on: !off,
                    });
                }
                let (target, rest) = match crate::eq::target(rest[0])
                    .map(String::from)
                    .or_else(|| rest[0].starts_with("app:").then(|| rest[0].to_string()))
                {
                    Some(t)
                        if !["band", "bypass", "gain", "reset", "preset", "save"]
                            .contains(&rest[0]) =>
                    {
                        (Some(t), &rest[1..])
                    }
                    _ => (None, rest),
                };
                match rest {
                    ["bypass", v] => Command::EqBypass { target, on: matches!(*v, "on" | "true" | "1") },
                    ["gain", v] => Command::EqGain {
                        target,
                        db: v.trim_end_matches("dB").parse().map_err(|_| format!("eq gain: what is '{v}'? (dB)"))?,
                    },
                    ["reset"] => Command::EqReset { target },
                    ["preset", name @ ..] if !name.is_empty() => Command::EqPreset { target, name: name.join(" ") },
                    ["save", name @ ..] if !name.is_empty() => Command::EqSave { target, name: name.join(" ") },
                    // band <1-8> [on|off] [kind] [freq Hz] [gain dB] [q]
                    ["band", n, rest @ ..] => {
                        let band = n.parse::<u8>().ok().filter(|b| (1..=crate::eq::BANDS as u8).contains(b)).ok_or("eq band <1-8> …")? - 1;
                        let mut on = None;
                        let mut kind = None;
                        let mut nums = vec![];
                        for w in rest {
                            match *w {
                                "on" => on = Some(true),
                                "off" => on = Some(false),
                                w => match crate::eq::EqKind::parse(w) {
                                    Some(k) => kind = Some(k),
                                    None => nums.push(w.trim_end_matches("Hz").trim_end_matches("dB").parse::<f32>().map_err(|_| format!("eq band: what is '{w}'?"))?),
                                },
                            }
                        }
                        Command::EqBand { target, band, on, kind, freq: nums.first().copied(), gain_db: nums.get(1).copied(), q: nums.get(2).copied() }
                    }
                    _ => return Err("eq [master|apps|dj|mic-N] band <1-8> … | bypass on|off | reset | preset <name> | save <name>".into()),
                }
            }
            ["dj", "ai", what, rest @ ..] => Command::Ai {
                action: match *what {
                    "suggest" => AiAction::Suggest,
                    "prepare" => AiAction::Prepare,
                    "run" => AiAction::Run,
                    "off" => AiAction::Off,
                    "auto" => AiAction::Auto,
                    _ => {
                        return Err(
                            "dj ai suggest|prepare|run|auto|off [build|hold|release]".into()
                        );
                    }
                },
                direction: match rest.first().copied() {
                    Some("build") => Some(crate::intent::Direction::Build),
                    Some("hold") => Some(crate::intent::Direction::Hold),
                    Some("release") => Some(crate::intent::Direction::Release),
                    None => None,
                    Some(_) => return Err("direction is build, hold or release".into()),
                },
            },
            ["deck", d, rest @ ..] => {
                let deck = Deck::parse(d).ok_or("deck is a, b, c, or d")?;
                match rest {
                    ["load", q @ ..] if !q.is_empty() => Command::LoadTrack { deck, query: q.join(" ") },
                    ["play"] => Command::DeckTransport { deck, action: Transport::Play },
                    ["pause"] => Command::DeckTransport { deck, action: Transport::Pause },
                    ["toggle"] => Command::DeckTransport { deck, action: Transport::Toggle },
                    ["eject"] => Command::DeckEject { deck },
                    ["cue"] => Command::DeckTransport { deck, action: Transport::Cue },
                    ["tempo", v] => Command::DeckTempo { deck, percent: v.trim_end_matches('%').parse().map_err(|_| "tempo takes a percentage, e.g. +2.5")? },
                    ["keylock", "on"] => Command::DeckKeylock { deck, on: true },
                    ["keylock", "off"] => Command::DeckKeylock { deck, on: false },
                    ["key", v] => Command::DeckKey { deck, semitones: v.parse().map_err(|_| "key takes semitones, e.g. -1")? },
                    ["sync"] => Command::DeckSync { deck },
                    ["reloop"] => Command::DeckReloop { deck },
                    ["loop", "off"] => Command::DeckLoop { deck, beats: None },
                    ["loop", b] => Command::DeckLoop { deck, beats: Some(b.parse().map_err(|_| "loop takes beats, e.g. 4 or 0.5")?) },
                    ["jump", b] => Command::DeckJump { deck, beats: b.parse().map_err(|_| "jump takes beats, e.g. -16")? },
                    ["sample"] => Command::DeckSample { deck, action: SampleAction::Save },
                    ["sample", "panel"] => Command::DeckSample { deck, action: SampleAction::Panel },
                    ["slip", "on"] => Command::DeckSlip { deck, on: true },
                    ["slip", "off"] => Command::DeckSlip { deck, on: false },
                    ["quantize", "on"] => Command::DeckQuantize { deck, on: true },
                    ["quantize", "off"] => Command::DeckQuantize { deck, on: false },
                    ["pfl", "on"] => Command::Pfl { deck, on: true },
                    ["pfl", "off"] => Command::Pfl { deck, on: false },
                    ["cue", n] => Command::HotCue { deck, n: n.parse().map_err(|_| "cue takes 1 … 8")?, clear: false },
                    ["cue", n, "clear"] => Command::HotCue { deck, n: n.parse().map_err(|_| "cue takes 1 … 8")?, clear: true },
                    ["eq", band, v] => {
                        let band = match *band {
                            "low" => Band::Low,
                            "mid" => Band::Mid,
                            "high" => Band::High,
                            _ => return Err("eq band is low, mid or high".into()),
                        };
                        let db = match *v {
                            "kill" | "-inf" => None,
                            v => Some(db(v)?),
                        };
                        Command::DeckEq { deck, band, db }
                    }
                    ["filter", v] => Command::DeckFilter { deck, amount: v.parse().map_err(|_| "filter takes −1 … 1")? },
                    ["fx", "off"] => Command::DeckFx { deck, kind: None, on: Some(false), beats: None, depth: None },
                    ["fx", k, rest @ ..] => {
                        let kinds = FxKind::parse_chain(k).map_err(|e| format!("{e}\nfx <effect>[+<effect>…] [beats] [depth] | fx off"))?;
                        let num = |i: usize| rest.get(i).map(|v| v.parse::<f32>().map_err(|_| format!("'{v}' is not a number"))).transpose();
                        let beats = num(0)?.or(Some(kinds[0].default_beats()));
                        match kinds[..] {
                            [kind] => Command::DeckFx { deck, kind: Some(kind), on: Some(true), beats, depth: num(1)? },
                            _ => Command::DeckFxChain { deck, kinds, on: true, beats, depth: num(1)? },
                        }
                    }
                    _ => return Err("deck a|b|c|d load <name> | play | pause | toggle | cue | eject | tempo <±%> | keylock on|off | key <±st> | sync | eq low|mid|high <dB|kill> | filter <−1…1> | cue <1…8> [clear] | pfl on|off | loop <beats>|off | reloop | jump <±beats> | slip on|off | quantize on|off".into()),
                }
            }
            ["xfader", x] => Command::Crossfader {
                position: x.parse().map_err(|_| "xfader takes 0.0 (A) … 1.0 (B)")?,
            },
            _ => return Err(format!("usage: {}", Self::USAGE)),
        })
    }
}

#[cfg(test)]
mod eq_cli_tests {
    use super::*;

    #[test]
    fn player_commands_validate_source_coordinates_and_human_authority() {
        let commands=[Command::DeckLoopSet{deck:Deck::A,from:1.234,to:2.456,play:false},Command::DeckLoopAdjust{deck:Deck::B,out:true,seconds:1.0/75.0},Command::DeckCueRecall{deck:Deck::C,seconds:3.456},Command::DeckCensor{deck:Deck::D,on:true},Command::DeckMaster{deck:Deck::B}];
        for command in commands {
            let json=command.to_json();
            let parsed:Command=serde_json::from_str(&json).unwrap();
            assert!(validate(parsed.clone(),Origin::User,&Context::default()).is_ok());
            assert!(validate(parsed,Origin::Ai,&Context::default()).is_err());
        }
        for command in [Command::DeckLoopSet{deck:Deck::A,from:2.0,to:1.0,play:true},Command::DeckLoopAdjust{deck:Deck::A,out:false,seconds:f64::NAN},Command::DeckCueRecall{deck:Deck::A,seconds:-1.0}] {
            assert!(validate(command,Origin::User,&Context::default()).is_err());
        }
    }

    #[test]
    fn eq_commands_parse_with_targets() {
        let p = |s: &str| Command::parse_cli(&s.split_whitespace().collect::<Vec<_>>());
        assert!(matches!(
            p("eq band 3 bell 1000 -2 1").unwrap(),
            Command::EqBand {
                target: None,
                band: 2,
                ..
            }
        ));
        assert!(
            matches!(p("eq a band 1 off").unwrap(), Command::EqBand { target: Some(t), on: Some(false), .. } if t == "deck-a")
        );
        assert!(
            matches!(p("eq app:cliamp preset warm").unwrap(), Command::EqPreset { target: Some(t), .. } if t == "app:cliamp")
        );
        assert!(matches!(
            p("eq app cliamp").unwrap(),
            Command::EqApp { on: true, .. }
        ));
        assert!(
            matches!(p("eq app my player off").unwrap(), Command::EqApp { app, on: false } if app == "my player")
        );
        assert!(p("eq band 9").is_err());
        assert!(
            matches!(p("eq dj gain -6.2dB").unwrap(), Command::EqGain { target: Some(t), db } if t == "dj" && db == -6.2)
        );
        assert!(p("eq gain loud").is_err());
        let v = |c: Command| validate(c, Origin::User, &Default::default());
        // Eight hot cues, which is what DJ hardware has: the imported Mixxx
        // corpus names cues 1 to 8 and nothing above.
        for n in 1..=8 {
            assert!(
                v(Command::HotCue {
                    deck: Deck::A,
                    n,
                    clear: false
                })
                .is_ok(),
                "cue {n} should be allowed"
            );
        }
        for n in [0u8, 9] {
            assert!(
                v(Command::HotCue {
                    deck: Deck::A,
                    n,
                    clear: false
                })
                .is_err(),
                "cue {n} should be refused"
            );
        }
        assert!(
            v(Command::EqBand {
                target: Some("nowhere".into()),
                band: 0,
                on: None,
                kind: None,
                freq: None,
                gain_db: None,
                q: None
            })
            .is_err()
        );
        assert!(
            v(Command::EqBand {
                target: Some("app:cliamp".into()),
                band: 0,
                on: None,
                kind: None,
                freq: Some(100.0),
                gain_db: Some(3.0),
                q: None
            })
            .is_ok()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(cmd: Command, o: Origin) -> bool {
        validate(cmd, o, &Context::default()).is_ok()
    }

    #[test]
    fn a_scratch_is_one_form_and_bounded() {
        let scratch = |seconds, delta, velocity| Command::DeckScratch {
            deck: Deck::A,
            seconds,
            delta,
            velocity,
        };
        // The UI's form: an absolute position under the hand.
        assert!(ok(scratch(Some(12.0), None, 0.0), Origin::User));
        // A controller's form: how far the hand moved, and the rate it moved at.
        assert!(ok(scratch(None, Some(-0.5), -1.0), Origin::User));
        assert!(ok(Command::DeckScratchEnd { deck: Deck::A }, Origin::User));
        // Both forms at once name no single movement.
        assert!(!ok(scratch(Some(1.0), Some(1.0), 0.0), Origin::User));
        // Neither does either.
        assert!(!ok(scratch(None, None, 0.0), Origin::User));
        // A controller may not throw the playhead across a set…
        assert!(!ok(scratch(None, Some(61.0), 0.0), Origin::User));
        assert!(!ok(scratch(None, Some(-61.0), 0.0), Origin::User));
        // …nor behind the start of the track, nor past the platter's own rate.
        assert!(!ok(scratch(Some(-1.0), None, 0.0), Origin::User));
        assert!(!ok(scratch(None, Some(1.0), 5.0), Origin::User));
        assert!(!ok(scratch(None, Some(f64::NAN), 0.0), Origin::User));
        assert!(!ok(scratch(Some(f64::INFINITY), None, 0.0), Origin::User));
    }

    #[test]
    fn reloop_is_one_validated_command_for_cli_and_controller_json() {
        for deck in [Deck::A, Deck::B, Deck::C, Deck::D] {
            let expected = Command::DeckReloop { deck };
            let cli = Command::parse_cli(&["deck", deck.id(), "reloop"]).unwrap();
            assert_eq!(cli, expected);
            let wire = serde_json::json!({"op": "dj.reloop", "deck": deck.id()});
            assert_eq!(Command::from_json(&wire.to_string()).unwrap(), expected);
            assert!(validate(expected, Origin::User, &Default::default()).is_ok());
        }
    }

    #[test]
    fn a_scratch_keeps_its_wire_names() {
        // The names a mapping file writes, and what the engine reads back.
        assert_eq!(
            Command::from_json(r#"{"op":"dj.scratch","deck":"a","delta":0.01}"#).unwrap(),
            Command::DeckScratch {
                deck: Deck::A,
                seconds: None,
                delta: Some(0.01),
                velocity: 0.0,
            }
        );
        assert_eq!(
            Command::from_json(r#"{"op":"dj.scratch_end","deck":"b"}"#).unwrap(),
            Command::DeckScratchEnd { deck: Deck::B }
        );
    }

    #[test]
    fn queue_and_automix_commands() {
        assert_eq!(
            Command::parse_cli(&["dj", "queue", "add", "/music/a b.flac"]).unwrap(),
            Command::QueueAdd {
                path: "/music/a b.flac".into(),
                at: None
            }
        );
        assert_eq!(
            Command::parse_cli(&["dj", "automix", "on", "build"]).unwrap(),
            Command::AutoMix {
                on: true,
                direction: Some(crate::intent::Direction::Build)
            }
        );
        assert!(ok(
            Command::AutoMix {
                on: true,
                direction: None
            },
            Origin::User
        ));
        assert!(
            !ok(
                Command::AutoMix {
                    on: true,
                    direction: None
                },
                Origin::Ai
            ),
            "the AI can't start auto-mix"
        );
        assert!(
            ok(
                Command::AutoMix {
                    on: false,
                    direction: None
                },
                Origin::Ai
            ),
            "but may stop it"
        );
        assert!(!ok(Command::TransitionNow, Origin::Ai));
        assert!(
            ok(
                Command::QueueAdd {
                    path: "/x.flac".into(),
                    at: None
                },
                Origin::Ai
            ),
            "the AI may propose by queueing"
        );
        assert!(!ok(
            Command::QueueAdd {
                path: " ".into(),
                at: None
            },
            Origin::User
        ));
    }

    #[test]
    fn beat_fx_commands() {
        let echo = Command::parse_cli(&["deck", "a", "fx", "echo", "0.5", "0.7"]).unwrap();
        assert_eq!(
            echo,
            Command::DeckFx {
                deck: Deck::A,
                kind: Some(FxKind::Echo),
                on: Some(true),
                beats: Some(0.5),
                depth: Some(0.7)
            }
        );
        assert!(ok(echo.clone(), Origin::User));
        assert!(!ok(echo, Origin::Ai), "effects are the DJ's");
        assert_eq!(
            Command::parse_cli(&["deck", "b", "fx", "trans"]).unwrap(),
            Command::DeckFx {
                deck: Deck::B,
                kind: Some(FxKind::Trans),
                on: Some(true),
                beats: Some(0.25),
                depth: None
            }
        );
        assert!(!ok(
            Command::DeckFx {
                deck: Deck::A,
                kind: None,
                on: None,
                beats: Some(64.0),
                depth: None
            },
            Origin::User
        ));
        let j = Command::DeckFx {
            deck: Deck::A,
            kind: Some(FxKind::Reverb),
            on: Some(true),
            beats: None,
            depth: None,
        }
        .to_json();
        assert_eq!(
            Command::from_json(&j).unwrap(),
            Command::DeckFx {
                deck: Deck::A,
                kind: Some(FxKind::Reverb),
                on: Some(true),
                beats: None,
                depth: None
            }
        );
        for k in FxKind::ALL {
            assert_eq!(FxKind::parse(k.name()), Some(k));
        }
    }

    #[test]
    fn beat_fx_chains() {
        let c = Command::parse_cli(&["deck", "a", "fx", "echo+sweep", "1", "0.6"]).unwrap();
        assert_eq!(
            c,
            Command::DeckFxChain {
                deck: Deck::A,
                kinds: vec![FxKind::Echo, FxKind::Sweep],
                on: true,
                beats: Some(1.0),
                depth: Some(0.6)
            }
        );
        let bare = Command::from_json(r#"{"op":"dj.fx_chain","deck":"a","kinds":["pan","drive"]}"#);
        assert!(
            matches!(bare, Ok(Command::DeckFxChain { on: true, .. })),
            "on by default: {bare:?}"
        );
        assert!(ok(c.clone(), Origin::User));
        assert!(!ok(c.clone(), Origin::Ai), "effects are the DJ's");
        assert_eq!(Command::from_json(&c.to_json()).unwrap(), c);
        assert!(c.to_json().contains("dj.fx_chain"), "{}", c.to_json());
        // Defaults from the first effect; a lone effect stays a plain DeckFx.
        let c = Command::parse_cli(&["deck", "b", "fx", "sweep+drive"]).unwrap();
        assert!(matches!(
            c,
            Command::DeckFxChain {
                beats: Some(8.0),
                ..
            }
        ));
        assert!(matches!(
            Command::parse_cli(&["deck", "b", "fx", "drive"]).unwrap(),
            Command::DeckFx { .. }
        ));
        // At most three, each once, all known.
        assert!(Command::parse_cli(&["deck", "a", "fx", "echo+echo"]).is_err());
        assert!(Command::parse_cli(&["deck", "a", "fx", "echo+sweep+pan+drive"]).is_err());
        assert!(
            Command::parse_cli(&["deck", "a", "fx", "echo+wobble"])
                .unwrap_err()
                .contains("tremolo")
        );
        assert!(!ok(
            Command::DeckFxChain {
                deck: Deck::A,
                kinds: vec![],
                on: true,
                beats: None,
                depth: None
            },
            Origin::User
        ));
    }

    #[test]
    fn the_ai_dj_can_neither_run_a_mix_nor_put_itself_on_auto() {
        for action in [
            AiAction::Suggest,
            AiAction::Prepare,
            AiAction::Run,
            AiAction::Auto,
            AiAction::Off,
        ] {
            assert!(
                ok(
                    Command::Ai {
                        action,
                        direction: None
                    },
                    Origin::User
                ),
                "{action:?} by a human"
            );
        }
        assert!(ok(
            Command::Ai {
                action: AiAction::Prepare,
                direction: None
            },
            Origin::Ai
        ));
        assert!(!ok(
            Command::Ai {
                action: AiAction::Run,
                direction: None
            },
            Origin::Ai
        ));
        assert!(!ok(
            Command::Ai {
                action: AiAction::Auto,
                direction: None
            },
            Origin::Ai
        ));
        assert_eq!(
            Command::parse_cli(&["dj", "ai", "auto", "build"]).unwrap(),
            Command::Ai {
                action: AiAction::Auto,
                direction: Some(crate::intent::Direction::Build)
            }
        );
    }

    #[test]
    fn json_round_trip_and_wire_names() {
        let c = Command::SetGain {
            strip: "cliamp".into(),
            db: -3.0,
        };
        let j = c.to_json();
        assert_eq!(j, r#"{"op":"mixer.set_gain","strip":"cliamp","db":-3.0}"#);
        assert_eq!(Command::from_json(&j).unwrap(), c);
        let t = Command::DeckTransport {
            deck: Deck::B,
            action: Transport::Cue,
        };
        assert_eq!(
            t.to_json(),
            r#"{"op":"dj.transport","deck":"b","action":"cue"}"#
        );
        assert!(Command::from_json(r#"{"op":"engine.format_disk"}"#).is_err());
    }

    #[test]
    fn cli_grammar() {
        assert_eq!(
            Command::parse_cli(&["gain", "cliamp", "-3dB"]).unwrap(),
            Command::SetGain {
                strip: "cliamp".into(),
                db: -3.0
            }
        );
        assert_eq!(
            Command::parse_cli(&["record", "start", "deck-a", "f32"]).unwrap(),
            Command::RecordStart {
                source: Some("deck-a".into()),
                format: Some(RecordFormat::F32)
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "a", "load", "midnight", "transit"]).unwrap(),
            Command::LoadTrack {
                deck: Deck::A,
                query: "midnight transit".into()
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "c", "play"]).unwrap(),
            Command::DeckTransport {
                deck: Deck::C,
                action: Transport::Play
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "d", "key", "-2"]).unwrap(),
            Command::DeckKey {
                deck: Deck::D,
                semitones: -2
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "b", "eject"]).unwrap(),
            Command::DeckEject { deck: Deck::B }
        );
        assert_eq!(
            serde_json::to_string(&Command::DeckEject { deck: Deck::B }).unwrap(),
            r#"{"op":"dj.eject","deck":"b"}"#
        );
        assert_eq!(
            Command::parse_cli(&["deck", "b", "tempo", "+2.5%"]).unwrap(),
            Command::DeckTempo {
                deck: Deck::B,
                percent: 2.5
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "a", "key", "-1"]).unwrap(),
            Command::DeckKey {
                deck: Deck::A,
                semitones: -1
            }
        );
        assert_eq!(
            Command::parse_cli(&["deck", "a", "eq", "low", "kill"]).unwrap(),
            Command::DeckEq {
                deck: Deck::A,
                band: Band::Low,
                db: None
            }
        );
        assert!(
            validate(
                Command::DeckEq {
                    deck: Deck::A,
                    band: Band::Low,
                    db: None
                },
                Origin::Ai,
                &Context::default()
            )
            .is_err(),
            "AI may not kill"
        );
        assert!(
            validate(
                Command::DeckTempo {
                    deck: Deck::A,
                    percent: 60.0
                },
                Origin::User,
                &Context::default()
            )
            .is_err()
        );
        assert!(Command::parse_cli(&["frobnicate"]).is_err());
    }

    #[test]
    fn ranges() {
        assert!(ok(
            Command::SetGain {
                strip: "x".into(),
                db: 12.0
            },
            Origin::User
        ));
        assert!(!ok(
            Command::SetGain {
                strip: "x".into(),
                db: 12.1
            },
            Origin::User
        ));
        assert!(!ok(
            Command::SetGain {
                strip: "x".into(),
                db: f32::NAN
            },
            Origin::Cli
        ));
        assert!(!ok(Command::Crossfader { position: 1.5 }, Origin::User));
        assert!(!ok(
            Command::Connect {
                from: "a".into(),
                to: "a".into()
            },
            Origin::User
        ));
        assert!(!ok(
            Command::SetGain {
                strip: " ".into(),
                db: 0.0
            },
            Origin::User
        ));
    }

    #[test]
    fn ai_is_bounded() {
        let ctx = Context {
            current_gain_db: Some(-10.0),
            crossfader: Some(0.5),
            recording: false,
        };
        let v = |c| validate(c, Origin::Ai, &ctx);
        assert!(
            v(Command::SetGain {
                strip: "a".into(),
                db: -6.0
            })
            .is_ok()
        );
        assert!(
            v(Command::SetGain {
                strip: "a".into(),
                db: 0.0
            })
            .is_err(),
            "10 dB jump"
        );
        assert!(
            v(Command::SetGain {
                strip: "a".into(),
                db: 8.0
            })
            .is_err(),
            "above AI ceiling"
        );
        assert!(v(Command::Crossfader { position: 0.7 }).is_ok());
        assert!(
            v(Command::Crossfader { position: 1.0 }).is_err(),
            "crossfader slam"
        );
        assert!(
            v(Command::SetMute {
                strip: "a".into(),
                muted: true
            })
            .is_err()
        );
        assert!(
            v(Command::SetMute {
                strip: "a".into(),
                muted: false
            })
            .is_ok()
        );
        assert!(
            v(Command::Route {
                stream: "a".into(),
                through: false
            })
            .is_err()
        );
        assert!(v(Command::RecordStop).is_err());
        assert!(
            v(Command::LoadTrack {
                deck: Deck::A,
                query: "x".into()
            })
            .is_ok()
        );
    }

    #[test]
    fn one_take_at_a_time() {
        let ctx = Context {
            recording: true,
            ..Default::default()
        };
        assert!(
            validate(
                Command::RecordStart {
                    source: None,
                    format: None
                },
                Origin::User,
                &ctx
            )
            .is_err()
        );
        assert!(validate(Command::RecordStop, Origin::User, &ctx).is_ok());
    }
}
