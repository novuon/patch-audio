//! The AI DJ inside Patch: feeds the patch-ai brain with the library and
//! what the decks play, keeps its state machine, and turns its suggestions
//! into prepare-only actions or (when a human says so) a scheduled plan.
//! It never touches the engine except through validated commands.

use super::AudioApp;
use crate::dj;
use patch_core::command::{AiAction, Command, Deck, Origin};
use patch_core::intent::{ControlLevel, Direction, MixerNow, SuggestionView};
use patch_host::HostedApp;
use slint::SharedString;
use std::time::Instant;

fn evidence_label(value: f32) -> &'static str {
    if value < 0.45 {
        "limited evidence"
    } else if value < 0.72 {
        "partial evidence"
    } else {
        "strong evidence"
    }
}

fn performance_start_request(
    graph: &patch_core::performance::PerformanceGraph,
    current_track: &str,
    target_track: &str,
) -> Result<serde_json::Value, String> {
    if !graph
        .sources
        .iter()
        .any(|source| source.id == current_track)
        || !graph.sources.iter().any(|source| source.id == target_track)
    {
        return Err(
            "the proposed performance no longer matches the playing and target tracks".into(),
        );
    }
    graph
        .validate()
        .map_err(|e| format!("invalid AI performance: {e}"))?;
    let mut source_paths = serde_json::Map::new();
    for source in &graph.sources {
        let path = std::path::PathBuf::from(&source.id);
        if !path.is_file() {
            return Err(format!(
                "AI performance source is unavailable: {}",
                source.id
            ));
        }
        source_paths.insert(
            source.id.clone(),
            serde_json::Value::String(path.to_string_lossy().into_owned()),
        );
    }
    Ok(serde_json::json!({
        "op": "performance",
        "action": "start_entry",
        "graph": graph,
        "source_paths": source_paths,
    }))
}

/// What the Library's AI DJ panel shows.
pub struct Panel {
    pub on: bool,
    pub state: SharedString,
    pub title: SharedString,
    pub detail: SharedString,
    pub reasons: SharedString,
    pub direction: SharedString,
    pub auto: bool,
    /// The runner-up suggestions ("2 · Title · 78 %").
    pub alternates: Vec<SharedString>,
}

/// Prepare this long before the mix-out point when auto-prepare is on.
pub const AUTO_PREPARE_S: f64 = 45.0;

/// The AI DJ's memory in this session.
#[derive(Default)]
pub struct AiDj {
    pub on: bool,
    pub state: patch_ai::State,
    pub direction: Direction,
    pub suggestions: Vec<patch_ai::Suggestion>,
    /// Tracks already played this session (never suggested again).
    pub history: Vec<String>,
    /// The suggestion's track is loaded, cued and synced on the idle deck.
    pub prepared: Option<String>,
    /// Background engine job executing the selected source-referenced graph.
    pub performance_job: Option<String>,
    /// Deck holding the outgoing live source until the graph hands off.
    pub performance_origin_deck: Option<usize>,
    /// The track the current graph hands off to.
    pub performance_target_track: Option<String>,
    /// Preferred deck after a graph handoff (may be C or D).
    pub preferred_deck: Option<usize>,
    pub last_tick: Option<Instant>,
    /// A human touched the decks since the last suggestion.
    pub touched: bool,
    /// Prepare the idle deck by itself when the mix-out comes near.
    pub auto: bool,
    /// Per-second band levels per track (from the detailed waveform).
    bands: std::collections::HashMap<String, Vec<[u8; 3]>>,
}

impl AudioApp {
    fn ai_stem_context(&self, source: &str, deck: Option<usize>) -> patch_core::stems::StemContext {
        let mut context = patch_engine::stem_service::context(source);
        let Some(deck) = deck.filter(|deck| *deck < 4) else {
            return context;
        };
        let deck_id = ["a", "b", "c", "d"][deck];
        let Ok(status) = patch_engine::client::call(&serde_json::json!({
            "op": "deck_status",
            "deck": deck_id,
        })) else {
            return context;
        };
        merge_live_stem_context(&mut context, source, &status);
        context
    }

    fn ai_deck_status(&self, index: usize) -> dj::DeckStatus {
        match index {
            0 | 1 => self.decks[index].status(),
            2 => dj::Deck::new("c", "Deck C").status(),
            3 => dj::Deck::new("d", "Deck D").status(),
            _ => dj::DeckStatus::default(),
        }
    }

    /// Band levels of a track (read once from its analysis sidecar).
    fn ai_bands(&self, path: &str) -> Vec<[u8; 3]> {
        if let Some(b) = self.ai.borrow().bands.get(path) {
            return b.clone();
        }
        let b = patch_analysis::detail_of(std::path::Path::new(path))
            .map(|d| patch_ai::bands_per_second(&d))
            .unwrap_or_default();
        self.ai
            .borrow_mut()
            .bands
            .insert(path.to_string(), b.clone());
        b
    }

    /// The analysed local library as the AI sees it.
    fn ai_library(&self) -> Vec<patch_ai::TrackInfo> {
        let mut live_statuses = std::collections::HashMap::<String, Vec<serde_json::Value>>::new();
        for deck in ["a", "b", "c", "d"] {
            let Ok(status) = patch_engine::client::call(&serde_json::json!({
                "op": "deck_status",
                "deck": deck,
            })) else {
                continue;
            };
            if status["loaded"] == true
                && let Some(path) = status["path"].as_str()
            {
                live_statuses
                    .entry(path.to_owned())
                    .or_default()
                    .push(status);
            }
        }
        let items: Vec<_> = self
            .library
            .borrow()
            .iter()
            .filter(|it| !it.url.contains("://"))
            .cloned()
            .collect();
        items
            .into_iter()
            .filter_map(|it| {
                let a = self.analysis(&it.url)?;
                let bands = self.ai_bands(&it.url);
                let mut stems = patch_engine::stem_service::context(&it.url);
                if let Some(statuses) = live_statuses.get(&it.url) {
                    let human_owned = live_human_stem_ids(&stems, &it.url, statuses);
                    for status in statuses {
                        merge_live_stem_context(&mut stems, &it.url, status);
                    }
                    stems.human_owned.extend(human_owned);
                    stems.human_owned.sort();
                    stems.human_owned.dedup();
                }
                Some(patch_ai::TrackInfo {
                    stems,
                    id: it.url.clone(),
                    path: it.url.clone(),
                    title: it.title,
                    artist: it.detail,
                    rating: 3,
                    analysis: a,
                    bands,
                })
            })
            .collect()
    }

    /// The deck that plays (with a grid), as the AI needs it.
    fn ai_playing(&self) -> Option<(usize, patch_ai::Playing)> {
        let preferred = self.ai.borrow().preferred_deck;
        let mut order = vec![0, 1, 2, 3];
        if let Some(index) = preferred.filter(|index| *index < 4) {
            order.retain(|candidate| *candidate != index);
            order.insert(0, index);
        }
        for i in order {
            let st = self.ai_deck_status(i);
            if !st.loaded || st.paused {
                continue;
            }
            let Some(a) = self.deck_analysis(i, &st) else {
                continue;
            };
            let Some(bpm) = a.bpm.filter(|_| a.bpm_confidence >= 0.5) else {
                continue;
            };
            let (speed, semitones) = if i < 2 {
                let pitch = self.pitch.borrow()[i];
                (pitch.speed(), pitch.semitones())
            } else {
                ((1.0 + st.tempo) * (1.0 + st.bend), 0.0)
            };
            let key = a.key.map(|k| k.shifted(semitones.round() as i32));
            let bands = self.ai_bands(&st.source);
            let track = patch_ai::TrackInfo {
                stems: self.ai_stem_context(&st.source, Some(i)),
                id: st.source.clone(),
                path: st.source.clone(),
                title: st.title.clone(),
                artist: String::new(),
                rating: 3,
                analysis: a,
                bands,
            };
            return Some((
                i,
                patch_ai::Playing {
                    deck: [Deck::A, Deck::B, Deck::C, Deck::D][i],
                    track,
                    position: st.time,
                    earliest_mix_out: None,
                    entered_at: None,
                    bpm: bpm * speed,
                    key,
                },
            ));
        }
        None
    }

    fn ai_suggest(&self) -> Result<String, String> {
        if self.ai.borrow().performance_job.is_some() {
            self.ai_performance_tick();
            if self.ai.borrow().performance_job.is_some() {
                return Err("the current AI performance is still executing".into());
            }
        }
        let (i, playing) = self
            .ai_playing()
            .ok_or("Auto Mix listens to a playing deck with a beat grid")?;
        let library = self.ai_library();
        let mut ai = self.ai.borrow_mut();
        let history = ai.history.clone();
        let ctx = patch_ai::Context {
            playing,
            next_deck: if i == 0 { Deck::B } else { Deck::A },
            direction: ai.direction,
            history: &history,
            recent: &[],
        };
        ai.suggestions = patch_ai::suggest(&ctx, &library, 3);
        ai.prepared = None;
        ai.touched = false;
        // Asked again: back in, whatever happened before.
        ai.state = if ai.suggestions.is_empty() {
            patch_ai::State::Listening
        } else {
            patch_ai::State::Suggested
        };
        match ai.suggestions.first() {
            Some(s) => Ok(format!(
                "Auto Mix suggests {} · {:?} · {:.0} % fit · {}",
                s.title,
                s.style,
                s.score * 100.0,
                s.reasons.first().cloned().unwrap_or_default()
            )),
            None => Err(format!(
                "no analysed track in the library fits ({} analysed)",
                library.len()
            )),
        }
    }

    /// Prepare: load, cue and sync the idle deck now (AI-level commands).
    fn ai_prepare(&self) -> Result<String, String> {
        if self.ai.borrow().suggestions.is_empty() {
            self.ai_suggest()?;
        }
        let s = self
            .ai
            .borrow()
            .suggestions
            .first()
            .cloned()
            .ok_or("nothing suggested")?;
        // Never load over music: the incoming deck must be idle.
        let to = if s.plan.to == Deck::B { 1 } else { 0 };
        let st = self.decks[to].status();
        if st.loaded && !st.paused {
            return Err(format!(
                "deck {} is playing: Auto Mix only prepares an idle deck",
                s.plan.to.id().to_uppercase()
            ));
        }
        let now = MixerNow {
            crossfader: self.xfader.get(),
            fader: [self.deck_gain.get()[0], self.deck_gain.get()[1]],
            eq: [self.eq.get()[0], self.eq.get()[1]],
        };
        let steps = patch_core::intent::compile(&s.plan, Origin::Ai, ControlLevel::Prepare, now)
            .map_err(|e| e.0)?;
        for t in steps {
            // Loading takes a moment: cue and sync once the track is there.
            if matches!(
                t.command,
                Command::DeckSeek { .. } | Command::DeckSync { .. }
            ) {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            self.command(t.command, Origin::Ai)?;
        }
        {
            let mut ai = self.ai.borrow_mut();
            ai.prepared = Some(s.track_id.clone());
            ai.state = patch_ai::State::Prepared;
        }
        Ok(format!(
            "Auto Mix prepared {} on deck {} at {} · press run to mix",
            s.title,
            s.plan.to.id().to_uppercase(),
            dj::clock(s.mix_in)
        ))
    }

    /// Run the suggested transition: a human's decision, scheduled at its
    /// mix-out bar.
    fn ai_run(&self, origin: Origin) -> Result<String, String> {
        let s = self
            .ai
            .borrow()
            .suggestions
            .first()
            .cloned()
            .ok_or("nothing suggested")?;
        if let Some(graph) = s.arrangement.as_ref().filter(|graph| {
            graph.is_layered_arrangement()
                && patch_core::performance::PerformanceGraph::execution_enabled()
        }) {
            let (origin_deck, playing) = self
                .ai_playing()
                .ok_or("the current playing source is no longer available")?;
            let request = performance_start_request(graph, &playing.track.id, &s.track_id)?;
            let response = patch_engine::client::call(&request)?;
            let job = response["job"]
                .as_str()
                .ok_or("engine did not return an AI performance job")?
                .to_owned();
            let mut ai = self.ai.borrow_mut();
            ai.on = true;
            ai.performance_job = Some(job);
            ai.performance_origin_deck = Some(origin_deck);
            ai.performance_target_track = Some(s.track_id.clone());
            ai.prepared = Some(s.track_id.clone());
            ai.state = patch_ai::State::Preparing;
            return Ok(format!(
                "Preparing AI performance into {} · the live deck stays on its current phrase",
                s.title
            ));
        }
        if self.ai.borrow().prepared.is_none() {
            self.ai_prepare()?;
        }
        if self.ai_playing().is_some_and(|(index, _)| index >= 2) {
            return Err(
                "this suggestion has no executable arrangement for the active C/D source".into(),
            );
        }
        self.start_plan(s.plan.clone(), ControlLevel::Execute, origin)?;
        self.ai.borrow_mut().state = patch_ai::State::Mixing;
        Ok(format!(
            "Mixing into {}: {:?} from {}",
            s.title,
            s.style,
            dj::clock(s.mix_out)
        ))
    }

    fn ai_performance_tick(&self) {
        let (Some(job_id), Some(origin_deck), Some(target_track)) = ({
            let ai = self.ai.borrow();
            (
                ai.performance_job.clone(),
                ai.performance_origin_deck,
                ai.performance_target_track.clone(),
            )
        }) else {
            return;
        };
        let status = match patch_engine::client::call(&serde_json::json!({
            "op": "performance",
            "action": "status",
            "job": job_id,
        })) {
            Ok(status) => status,
            Err(error) => {
                let mut ai = self.ai.borrow_mut();
                ai.performance_job = None;
                ai.performance_origin_deck = None;
                ai.performance_target_track = None;
                ai.prepared = None;
                ai.state = patch_ai::State::Suggested;
                *self.dj_status.borrow_mut() = format!("AI performance failed: {error}");
                return;
            }
        };
        if status["state"] == "failed" {
            let error = status["error"]
                .as_str()
                .unwrap_or("engine rejected the plan");
            let mut ai = self.ai.borrow_mut();
            ai.performance_job = None;
            ai.performance_origin_deck = None;
            ai.performance_target_track = None;
            ai.prepared = None;
            ai.state = patch_ai::State::Suggested;
            *self.dj_status.borrow_mut() = format!("AI performance failed: {error}");
            return;
        }
        let target = status["lanes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|lane| lane["source_id"].as_str() == Some(target_track.as_str()));
        let Some(target) = target else {
            return;
        };
        let Some(deck_id) = target["deck"].as_str() else {
            return;
        };
        let target_deck = match deck_id {
            "a" => 0,
            "b" => 1,
            "c" => 2,
            "d" => 3,
            _ => return,
        };
        let engine_sample = status["engine_sample"].as_u64().unwrap_or(0);
        let target_at = target["scheduled_sample"].as_u64().unwrap_or(u64::MAX);
        if status["state"] == "scheduled" || status["state"] == "playing" {
            let mut ai = self.ai.borrow_mut();
            ai.state = patch_ai::State::Mixing;
        }
        let target_status = self.ai_deck_status(target_deck);
        let origin_status = self.ai_deck_status(origin_deck);
        if status["state"] == "playing"
            && engine_sample >= target_at
            && target_status.loaded
            && !target_status.paused
            && (!origin_status.loaded || origin_status.paused)
        {
            let mut ai = self.ai.borrow_mut();
            ai.performance_job = None;
            ai.performance_origin_deck = None;
            ai.performance_target_track = None;
            ai.preferred_deck = Some(target_deck);
            ai.prepared = None;
            ai.suggestions.clear();
            ai.state = patch_ai::State::Listening;
            *self.dj_status.borrow_mut() = format!(
                "AI performance handed off to {} on deck {}",
                target_status.title,
                deck_id.to_uppercase()
            );
        }
    }

    pub(super) fn ai_command(
        &self,
        action: AiAction,
        direction: Option<Direction>,
        origin: Origin,
    ) -> Result<(), String> {
        if let Some(d) = direction {
            self.ai.borrow_mut().direction = d;
        }
        let msg = match action {
            AiAction::Suggest => {
                self.ai.borrow_mut().on = true;
                self.ai_suggest()?
            }
            AiAction::Prepare => {
                self.ai.borrow_mut().on = true;
                self.ai_prepare()?
            }
            AiAction::Run => self.ai_run(origin)?,
            AiAction::Auto => {
                self.set_ai_auto(true);
                let m = self.ai_suggest().unwrap_or_else(|e| e);
                format!("Auto Mix auto · {m}")
            }
            AiAction::Off => {
                let (history, bands, performance_job, origin_deck, target_track, preferred_deck) = {
                    let ai = self.ai.borrow();
                    (
                        ai.history.clone(),
                        ai.bands.clone(),
                        ai.performance_job.clone(),
                        ai.performance_origin_deck,
                        ai.performance_target_track.clone(),
                        ai.preferred_deck,
                    )
                };
                *self.ai.borrow_mut() = AiDj {
                    history,
                    bands,
                    performance_job,
                    performance_origin_deck: origin_deck,
                    performance_target_track: target_track,
                    preferred_deck,
                    ..Default::default()
                };
                "Auto Mix off".into()
            }
        };
        *self.dj_status.borrow_mut() = msg;
        Ok(())
    }

    /// Once a second: history, state machine, and a fresh suggestion when
    /// the mix-out comes in range (only while the AI DJ is on).
    pub(super) fn ai_tick(&self) {
        if self
            .ai
            .borrow()
            .last_tick
            .is_some_and(|t| t.elapsed().as_secs_f64() < 1.0)
        {
            return;
        }
        self.ai.borrow_mut().last_tick = Some(Instant::now());
        if !self.ai.borrow().on || !self.is_live() {
            return;
        }
        if self.ai.borrow().performance_job.is_some() {
            self.ai_performance_tick();
        }
        // The mix is done (or was cancelled): listen to the new deck and
        // line up the next track.
        if self.ai.borrow().state == patch_ai::State::Mixing
            && self.plan.borrow().is_none()
            && self.ai.borrow().performance_job.is_none()
        {
            let mut ai = self.ai.borrow_mut();
            ai.state = patch_ai::State::Listening;
            ai.suggestions.clear();
            ai.prepared = None;
        }
        let playing = self.ai_playing();
        if let Some((_, p)) = &playing {
            let mut ai = self.ai.borrow_mut();
            if !ai.history.contains(&p.track.id) {
                ai.history.push(p.track.id.clone());
            }
        }
        let to_mix = self
            .ai
            .borrow()
            .suggestions
            .first()
            .zip(playing.as_ref())
            .map(|(s, (_, p))| s.mix_out - p.position);
        let (state, has, prepared, touched) = {
            let ai = self.ai.borrow();
            (
                ai.state,
                !ai.suggestions.is_empty(),
                ai.prepared.is_some(),
                ai.touched,
            )
        };
        let next = patch_ai::step(
            state,
            playing.is_some(),
            to_mix.or(Some(f64::MAX)),
            has,
            prepared,
            touched,
        );
        self.ai.borrow_mut().state = next;
        // Auto-prepare: the idle deck gets the suggestion in time to judge
        // it (cue it in the headphones); the mix itself stays yours.
        if self.ai.borrow().auto
            && next == patch_ai::State::Suggested
            && to_mix.is_some_and(|s| s <= AUTO_PREPARE_S)
        {
            match self.ai_prepare() {
                Ok(m) => *self.dj_status.borrow_mut() = m,
                Err(e) => {
                    *self.dj_status.borrow_mut() = format!("Auto Mix: {e}");
                    // Don't retry every second.
                    self.ai.borrow_mut().auto = false;
                }
            }
        }
        if next == patch_ai::State::Listening && !has {
            // Suggest early enough for the DJ to judge it.
            let _ = self.ai_suggest();
        }
    }

    /// State and suggestion for the AI view.
    pub(super) fn ai_summary(&self) -> (String, Option<SuggestionView>) {
        // The panel only needs a playback position. Avoid `ai_playing()` here:
        // that constructs a full analysis record and may populate the mutable
        // band cache. Doing so while holding `ai` below can RefCell-panic on
        // the first frame for a newly loaded/playing track.
        let preferred = self.ai.borrow().preferred_deck;
        let mut order = vec![0, 1, 2, 3];
        if let Some(index) = preferred.filter(|index| *index < 4) {
            order.retain(|candidate| *candidate != index);
            order.insert(0, index);
        }
        let pos = order.into_iter().find_map(|i| {
            let status = self.ai_deck_status(i);
            (status.loaded && !status.paused).then_some(status.time)
        });
        let ai = self.ai.borrow();
        let s = ai.suggestions.first().map(|s| SuggestionView {
            title: s.title.clone(),
            artist: s.artist.clone(),
            style: format!("{:?}", s.style),
            score: s.score,
            confidence: s.confidence,
            reasons: s.reasons.clone(),
            starts_in: pos.map(|p| s.mix_out - p).unwrap_or(0.0),
        });
        (format!("{:?}", ai.state).to_lowercase(), s)
    }

    /// A human touched the decks: the AI steps back.
    pub(super) fn ai_touched(&self) {
        let mut ai = self.ai.borrow_mut();
        if ai.on && ai.state != patch_ai::State::Mixing {
            ai.touched = true;
        }
    }

    pub(super) fn set_ai_direction(&self, d: Direction) {
        let changed = {
            let mut ai = self.ai.borrow_mut();
            let changed = ai.direction != d;
            ai.direction = d;
            changed
        };
        // A new direction changes what fits: suggest again.
        if changed
            && !self.ai.borrow().suggestions.is_empty()
            && let Err(e) = self.ai_suggest().map(|m| *self.dj_status.borrow_mut() = m)
        {
            *self.dj_status.borrow_mut() = e;
        }
    }

    pub(super) fn set_ai_auto(&self, on: bool) {
        let mut ai = self.ai.borrow_mut();
        ai.auto = on;
        if on {
            ai.on = true;
        }
    }

    /// Choose a runner-up (index into the suggestions) as the next track.
    pub(super) fn ai_pick(&self, i: usize) -> Result<String, String> {
        let mut ai = self.ai.borrow_mut();
        if i >= ai.suggestions.len() {
            return Err("no such suggestion".into());
        }
        if ai.state == patch_ai::State::Mixing {
            return Err("a mix is running".into());
        }
        let s = ai.suggestions.remove(i);
        let msg = format!(
            "Auto Mix: next is {} · {:?} · {:.0} % fit",
            s.title,
            s.style,
            s.score * 100.0
        );
        ai.suggestions.insert(0, s);
        // What was prepared was the old choice.
        ai.prepared = None;
        ai.state = patch_ai::State::Suggested;
        Ok(msg)
    }

    pub(super) fn ai_panel(&self) -> Panel {
        let (state, s) = self.ai_summary();
        let ai = self.ai.borrow();
        let (title, detail, reasons) = match (&s, ai.suggestions.first()) {
            (Some(v), Some(full)) => {
                let who = if v.artist.is_empty() {
                    v.title.clone()
                } else {
                    format!("{} — {}", v.title, v.artist)
                };
                let when = if v.starts_in > 0.0 {
                    format!("mix out in {}", dj::clock(v.starts_in))
                } else {
                    "mix-out point passed".into()
                };
                (
                    format!("AI suggestion: {who}"),
                    format!(
                        "{} {} · proposed for deck {} · {:.0} % fit · {} (uncalibrated) · {when}",
                        full.plan.bars,
                        v.style.to_lowercase(),
                        full.plan.to.id().to_uppercase(),
                        v.score * 100.0,
                        evidence_label(v.confidence)
                    ),
                    v.reasons.join(" · "),
                )
            }
            _ => Default::default(),
        };
        Panel {
            on: ai.on,
            state: state.into(),
            title: title.into(),
            detail: detail.into(),
            reasons: reasons.into(),
            direction: format!("{:?}", ai.direction).to_lowercase().into(),
            auto: ai.auto,
            alternates: ai
                .suggestions
                .iter()
                .enumerate()
                .skip(1)
                .map(|(i, s)| format!("{} · {} · {:.0} %", i + 1, s.title, s.score * 100.0).into())
                .collect(),
        }
    }
}

fn merge_live_stem_context(
    context: &mut patch_core::stems::StemContext,
    source: &str,
    status: &serde_json::Value,
) {
    let status_source = status["source"]
        .as_str()
        .or_else(|| status["path"].as_str());
    let controls = if status["controls"].is_object() {
        &status["controls"]
    } else {
        &status["stems"]
    };
    let status_set = status["set"]["id"]
        .as_str()
        .or_else(|| status["stem_set"]["id"].as_str());
    if controls["ready"] != true || status_source != Some(source) {
        return;
    }
    if let Some(id) = status_set {
        if context.set_id.as_deref().is_some_and(|cached| cached != id) {
            return;
        }
        context.set_id = Some(id.to_owned());
    }
    context.availability = patch_core::stems::Availability::Ready;
    context.available.clear();
    context.gains.clear();
    context.muted.clear();
    context.soloed.clear();
    context.human_owned.clear();
    for slot in controls["controls"].as_array().into_iter().flatten() {
        let Some(id) = slot["id"].as_str() else {
            continue;
        };
        context.available.push(id.to_owned());
        context
            .gains
            .insert(id.to_owned(), slot["gain"].as_f64().unwrap_or(1.0) as f32);
        if slot["mute"] == true {
            context.muted.push(id.to_owned());
        }
        if slot["solo"] == true {
            context.soloed.push(id.to_owned());
        }
        if slot["human"] == true {
            context.human_owned.push(id.to_owned());
        }
    }
}

fn live_human_stem_ids(
    cached: &patch_core::stems::StemContext,
    source: &str,
    statuses: &[serde_json::Value],
) -> Vec<String> {
    let mut owned = Vec::new();
    for status in statuses {
        let status_source = status["source"]
            .as_str()
            .or_else(|| status["path"].as_str());
        let controls = if status["controls"].is_object() {
            &status["controls"]
        } else {
            &status["stems"]
        };
        let set_id = status["set"]["id"]
            .as_str()
            .or_else(|| status["stem_set"]["id"].as_str());
        if status_source != Some(source)
            || controls["ready"] != true
            || set_id.is_some_and(|id| cached.set_id.as_deref().is_some_and(|cached| cached != id))
        {
            continue;
        }
        owned.extend(
            controls["controls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|slot| slot["human"] == true)
                .filter_map(|slot| slot["id"].as_str().map(str::to_owned)),
        );
    }
    owned.sort();
    owned.dedup();
    owned
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_core::performance::{
        ActiveLayer, ArrangementOperation, MaterialKind, PerformanceEdge, PerformanceGraph,
        PerformanceSource, PerformanceState, PerformanceTempoSegment,
    };

    #[test]
    fn live_stem_snapshot_merges_only_for_the_matching_ready_source_and_set() {
        let mut context = patch_core::stems::StemContext {
            set_id: Some("set-1".into()),
            ..Default::default()
        };
        let status = serde_json::json!({
            "source": "/music/a.flac",
            "set": {"id": "set-1"},
            "controls": {
                "ready": true,
                "controls": [
                    {"id":"vocals", "gain":0.7, "mute":true, "solo":false, "human":true},
                    {"id":"drums", "gain":1.0, "mute":false, "solo":true, "human":false}
                ]
            }
        });
        merge_live_stem_context(&mut context, "/music/a.flac", &status);
        assert!(matches!(
            context.availability,
            patch_core::stems::Availability::Ready
        ));
        assert_eq!(context.gains["vocals"], 0.7);
        assert_eq!(context.muted, ["vocals"]);
        assert_eq!(context.soloed, ["drums"]);
        assert_eq!(context.human_owned, ["vocals"]);

        let before = context.clone();
        let wrong_source = serde_json::json!({
            "source":"/music/b.flac", "set":{"id":"set-1"},
            "controls":{"ready":true,"controls":[]}
        });
        merge_live_stem_context(&mut context, "/music/a.flac", &wrong_source);
        assert_eq!(context.available, before.available);
        let wrong_set = serde_json::json!({
            "source":"/music/a.flac", "set":{"id":"set-2"},
            "controls":{"ready":true,"controls":[]}
        });
        merge_live_stem_context(&mut context, "/music/a.flac", &wrong_set);
        assert_eq!(context.set_id, before.set_id);
        assert_eq!(context.gains, before.gains);
    }

    #[test]
    fn native_deck_status_carries_human_ownership_into_material_search() {
        let mut context = patch_core::stems::StemContext {
            set_id: Some("set-1".into()),
            ..Default::default()
        };
        let first = serde_json::json!({
            "path":"/music/a.flac", "loaded":true, "stem_set":{"id":"set-1"},
            "stems":{"ready":true,"controls":[
                {"id":"vocals","gain":0.8,"mute":false,"solo":false,"human":true},
                {"id":"drums","gain":1.0,"mute":false,"solo":false,"human":false}
            ]}
        });
        let second = serde_json::json!({
            "path":"/music/a.flac", "loaded":true, "stem_set":{"id":"set-1"},
            "stems":{"ready":true,"controls":[
                {"id":"vocals","gain":1.0,"mute":false,"solo":false,"human":false},
                {"id":"drums","gain":0.5,"mute":false,"solo":false,"human":true}
            ]}
        });
        let statuses = vec![first, second];
        let human = live_human_stem_ids(&context, "/music/a.flac", &statuses);
        for status in &statuses {
            merge_live_stem_context(&mut context, "/music/a.flac", status);
        }
        context.human_owned.extend(human);
        context.human_owned.sort();
        context.human_owned.dedup();
        assert_eq!(context.human_owned, ["drums", "vocals"]);
        assert_eq!(
            live_human_stem_ids(&context, "/music/b.flac", &statuses),
            Vec::<String>::new()
        );
        let empty_cache = patch_core::stems::StemContext::default();
        assert_eq!(
            live_human_stem_ids(&empty_cache, "/music/a.flac", &statuses),
            ["drums", "vocals"]
        );
    }

    #[test]
    fn ai_run_request_preserves_full_track_sources_and_validates_identity() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let a = std::env::temp_dir().join(format!("patch-ai-run-a-{nonce}.audio"));
        let b = std::env::temp_dir().join(format!("patch-ai-run-b-{nonce}.audio"));
        std::fs::write(&a, b"source-a").unwrap();
        std::fs::write(&b, b"source-b").unwrap();
        let source_a = a.to_string_lossy().into_owned();
        let source_b = b.to_string_lossy().into_owned();
        let layer = |id: &str, source_id: &str, gain: f32| ActiveLayer {
            id: id.into(),
            source_id: source_id.into(),
            material: MaterialKind::OriginalMix,
            stem_set_id: None,
            source_start_s: 0.0,
            source_end_s: 30.0,
            gain,
        };
        let graph = PerformanceGraph {
            entry: "outgoing".into(),
            tempo: vec![PerformanceTempoSegment {
                start_beat: 0.0,
                end_beat: None,
                bpm: 120.0,
            }],
            sources: vec![
                PerformanceSource {
                    id: source_a.clone(),
                    content_hash: None,
                    tempo_bpm: Some(120.0),
                    duration_s: 30.0,
                },
                PerformanceSource {
                    id: source_b.clone(),
                    content_hash: None,
                    tempo_bpm: Some(120.0),
                    duration_s: 30.0,
                },
            ],
            states: vec![
                PerformanceState {
                    id: "outgoing".into(),
                    at_beat: 0.0,
                    layers: vec![layer("a", &source_a, 1.0)],
                },
                PerformanceState {
                    id: "overlap".into(),
                    at_beat: 4.0,
                    layers: vec![layer("a", &source_a, 1.0), layer("b", &source_b, 0.0)],
                },
            ],
            edges: vec![PerformanceEdge {
                from: "outgoing".into(),
                to: "overlap".into(),
                at_beat: 4.0,
                operation: ArrangementOperation::IntroduceLayer {
                    layer_id: "b".into(),
                },
            }],
        };
        let request = performance_start_request(&graph, &source_a, &source_b).unwrap();
        assert_eq!(request["op"], "performance");
        assert_eq!(request["action"], "start_entry");
        assert_eq!(
            request["source_paths"][&source_a],
            a.to_string_lossy().as_ref()
        );
        assert_eq!(
            request["source_paths"][&source_b],
            b.to_string_lossy().as_ref()
        );
        assert!(performance_start_request(&graph, "/different/current.audio", &source_b).is_err());
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }
}
