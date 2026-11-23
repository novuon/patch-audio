//! The Derived Material service: background preparation with priority and
//! deadlines, and the states a caller waits on.
//!
//! Extraction, decoding, stem loading and stretching never run on the audio
//! thread. A request returns an id and a state immediately
//! (`queued → preparing → ready | degraded | failed`). With a deadline the
//! service degrades in order — **cached → clean stem extraction → full-mix
//! extraction (flagged) → skip** — and the reason is part of the state.
//! `prepare()` queues likely work at low priority and never blocks a caller.

use crate::cache::{Cache, Report, Stats};
use crate::dsp;
use crate::kit::TemporaryKit;
use crate::model::{Asset, Meta, Policy, Quality, Recipe};
use crate::traits::{Audio, SourceAudio, Stretch};
use patch_core::material::{AssetId, KitId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

/// Where an asset is in preparation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum State {
    Queued,
    Preparing,
    Ready,
    /// Ready, but not as asked: the reason is why (e.g. "full mix (no stem)").
    Degraded(String),
    /// Given up: the reason is why.
    Failed(String),
    /// The source file changed since the recipe was made.
    Stale(String),
}

impl State {
    pub fn is_ready(&self) -> bool {
        matches!(self, State::Ready | State::Degraded(_))
    }
}

/// Priority: larger runs first. `predictive` is the lowest.
pub const PRIORITY_PREDICTIVE: i32 = 0;
pub const PRIORITY_NORMAL: i32 = 10;
pub const PRIORITY_URGENT: i32 = 20;

struct Job {
    recipe: Recipe,
    priority: i32,
    /// Milliseconds from enqueue after which the job is skipped (None: no
    /// deadline).
    deadline_ms: Option<f64>,
    enqueued_ms: f64,
}

struct Inner {
    jobs: Vec<Job>,
    states: BTreeMap<String, State>,
    kits: BTreeMap<String, TemporaryKit>,
    completed: u64,
    failed: u64,
}

/// The material service. Cheap to share (`Arc`) with background workers.
pub struct Service {
    cache: Arc<Cache>,
    inner: Arc<Mutex<Inner>>,
    cv: Arc<Condvar>,
}

fn millis() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Render a recipe into a complete [`Asset`]: audio plus measured metadata.
pub fn render_asset(
    recipe: &Recipe,
    source: &dyn SourceAudio,
    stretch: Option<&dyn Stretch>,
) -> Result<Asset, String> {
    render_asset_with_audio(recipe, source, stretch).map(|(asset, _)| asset)
}

/// Render a recipe into its [`Asset`] *and* the audio to cache, so the two are
/// measured and stored from exactly one render.
pub fn render_asset_with_audio(
    recipe: &Recipe,
    source: &dyn SourceAudio,
    stretch: Option<&dyn Stretch>,
) -> Result<(Asset, Audio), String> {
    let start = millis();
    let audio = dsp::render(recipe, source, stretch)?;
    let frames = dsp::frames(&audio) as u64;
    let duration = frames as f64 / recipe.region.rate.max(1) as f64;
    let peak = dsp::peak(&audio);
    let rms = dsp::rms(&audio);
    let transients = dsp::transients(&audio, recipe.region.rate);
    let loop_suitability = dsp::loop_suitability(&audio);
    let mut notes = Vec::new();
    if peak < 0.02 {
        notes.push(format!("very quiet (peak {:.4})", peak));
    }
    if loop_suitability < 0.8 {
        notes.push(format!("loop seam {:.2}", loop_suitability));
    }
    if recipe.region.confidence < 0.5 {
        notes.push(format!(
            "low grid confidence {:.2}",
            recipe.region.confidence
        ));
    }
    let score =
        (peak.min(1.0) * 0.3 + loop_suitability * 0.4 + recipe.region.confidence.min(1.0) * 0.3)
            .clamp(0.0, 1.0);
    let meta = Meta {
        frames,
        duration,
        peak,
        rms,
        transients,
        loop_suitability,
        bleed: None,
        quality: Quality { score, notes },
        result_bpm: recipe.region.source_bpm,
        result_key: recipe.region.source_key.clone(),
        created: (millis() / 1000.0) as u64,
        cost_ms: millis() - start,
        policy: Policy::WhileReferenced,
        consumers: Vec::new(),
    };
    Ok((Asset::new(recipe.clone(), meta), audio))
}

impl Service {
    pub fn new(cache: Cache) -> Service {
        Service {
            cache: Arc::new(cache),
            inner: Arc::new(Mutex::new(Inner {
                jobs: Vec::new(),
                states: BTreeMap::new(),
                kits: BTreeMap::new(),
                completed: 0,
                failed: 0,
            })),
            cv: Arc::new(Condvar::new()),
        }
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Ask for material. Returns immediately with the id and its state: a
    /// cached recipe is `Ready`, anything else is `Queued`. Never blocks.
    pub fn request(
        &self,
        recipe: Recipe,
        priority: i32,
        deadline_ms: Option<f64>,
    ) -> (AssetId, State) {
        let id = recipe.id();
        let mut inner = self.inner.lock().unwrap();
        if self.cache.has(&id) {
            let state = State::Ready;
            inner.states.insert(id.0.clone(), state.clone());
            return (id, state);
        }
        if !inner.states.contains_key(&id.0) {
            inner.jobs.push(Job {
                recipe,
                priority,
                deadline_ms,
                enqueued_ms: millis(),
            });
            inner.states.insert(id.0.clone(), State::Queued);
            self.cv.notify_one();
        }
        let state = inner.states.get(&id.0).cloned().unwrap_or(State::Queued);
        (id, state)
    }

    /// Predictive work: the same queue at the lowest priority.
    pub fn prepare(&self, recipe: Recipe, deadline_ms: Option<f64>) -> (AssetId, State) {
        self.request(recipe, PRIORITY_PREDICTIVE, deadline_ms)
    }

    pub fn state(&self, id: &AssetId) -> Option<State> {
        self.inner.lock().unwrap().states.get(&id.0).cloned()
    }

    /// Take the next job (highest priority, then oldest). Worker entry point.
    fn take(&self) -> Option<Job> {
        let mut inner = self.inner.lock().unwrap();
        let best = inner
            .jobs
            .iter()
            .enumerate()
            .max_by_key(|(_, j)| (j.priority, -(j.enqueued_ms as i64)))
            .map(|(i, _)| i);
        let job = inner.jobs.remove(best?);
        inner
            .states
            .insert(job.recipe.id().0.clone(), State::Preparing);
        Some(job)
    }

    /// Run one queued job synchronously (tests, or one worker step).
    pub fn process_one(
        &self,
        source: &dyn SourceAudio,
        stretch: Option<&dyn Stretch>,
    ) -> Option<State> {
        let job = self.take()?;
        let key = job.recipe.id().0.clone();
        let state = self.run(job, source, stretch);
        let mut inner = self.inner.lock().unwrap();
        match &state {
            State::Failed(_) => inner.failed += 1,
            _ => inner.completed += 1,
        }
        inner.states.insert(key, state.clone());
        Some(state)
    }

    fn run(&self, job: Job, source: &dyn SourceAudio, stretch: Option<&dyn Stretch>) -> State {
        // Cached first, whatever the deadline.
        let id = job.recipe.id();
        if self.cache.has(&id) {
            return State::Ready;
        }
        if let Some(deadline) = job.deadline_ms
            && millis() - job.enqueued_ms > deadline
        {
            return State::Failed("deadline: skipped".into());
        }
        // Clean stem extraction, then the full mix (flagged), then skip.
        match render_asset_with_audio(&job.recipe, source, stretch) {
            Ok((asset, audio)) => match self.cache.store(&asset, &audio) {
                Ok(()) => State::Ready,
                Err(e) => State::Failed(format!("cache: {e}")),
            },
            Err(stem_error) => {
                let mut fallback = job.recipe.clone();
                if fallback.source.stem.is_none() {
                    return State::Failed(stem_error);
                }
                // Degrade to the full mix and flag it.
                fallback.source.stem = None;
                fallback.source.stem_set = None;
                match render_asset_with_audio(&fallback, source, stretch) {
                    Ok((asset, audio)) => {
                        let _ = self.cache.store(&asset, &audio);
                        State::Degraded(format!("full mix (stem unavailable: {stem_error})"))
                    }
                    Err(full_error) => {
                        State::Failed(format!("stem: {stem_error}; full mix: {full_error}"))
                    }
                }
            }
        }
    }

    /// A background worker: renders queued jobs until the service is dropped.
    /// `Arc<Service>` + the engine's `SourceAudio`/`Stretch`.
    pub fn worker(
        self: Arc<Service>,
        source: Arc<dyn SourceAudio>,
        stretch: Option<Arc<dyn Stretch>>,
    ) {
        loop {
            let job = {
                let mut inner = self.inner.lock().unwrap();
                if inner.jobs.is_empty() {
                    inner = self.cv.wait(inner).unwrap();
                }
                let best = inner
                    .jobs
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, j)| (j.priority, -(j.enqueued_ms as i64)))
                    .map(|(i, _)| i);
                match best {
                    Some(i) => {
                        let job = inner.jobs.remove(i);
                        inner
                            .states
                            .insert(job.recipe.id().0.clone(), State::Preparing);
                        job
                    }
                    None => continue,
                }
            };
            let key = job.recipe.id().0.clone();
            let state = self.run(job, source.as_ref(), stretch.as_deref());
            let mut inner = self.inner.lock().unwrap();
            match &state {
                State::Failed(_) => inner.failed += 1,
                _ => inner.completed += 1,
            }
            inner.states.insert(key, state);
        }
    }

    pub fn kit(&self, kit: TemporaryKit) {
        self.inner
            .lock()
            .unwrap()
            .kits
            .insert(kit.id.0.clone(), kit);
    }

    pub fn get_kit(&self, id: &KitId) -> Option<TemporaryKit> {
        self.inner.lock().unwrap().kits.get(&id.0).cloned()
    }

    /// Release a consumer's reference on an asset.
    pub fn release(&self, id: &AssetId, consumer: &str) -> Result<(), String> {
        self.cache.release(id, consumer)
    }

    pub fn cleanup(&self) -> Report {
        self.cache.cleanup()
    }

    pub fn stats(&self) -> Stats {
        self.cache.stats()
    }

    /// (completed, failed) jobs since the service started.
    pub fn counts(&self) -> (u64, u64) {
        let inner = self.inner.lock().unwrap();
        (inner.completed, inner.failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Resolved, SourceRef, Transform, VERSION};

    struct Synthetic {
        /// Seconds of audio the source pretends to have.
        seconds: f64,
        /// When set, a request for this stem fails (to exercise degradation).
        fail_stem: Option<String>,
        /// Which stems were asked for.
        seen: Mutex<Vec<String>>,
    }

    impl Synthetic {
        fn new(seconds: f64, fail_stem: Option<&str>) -> Synthetic {
            Synthetic {
                seconds,
                fail_stem: fail_stem.map(str::to_string),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl SourceAudio for Synthetic {
        fn decode(&self, source: &SourceRef, rate: u32) -> Result<Vec<f32>, String> {
            self.seen
                .lock()
                .unwrap()
                .push(source.stem.clone().unwrap_or_else(|| "mix".into()));
            if let (Some(stem), Some(fail)) = (&source.stem, &self.fail_stem)
                && stem == fail
            {
                return Err("stem not separated".into());
            }
            let n = (self.seconds * rate as f64) as usize;
            Ok((0..n * 2)
                .map(|i| ((i as f32 * 0.01).sin()) * 0.4)
                .collect())
        }
    }

    fn recipe(stem: Option<&str>, transforms: Vec<Transform>) -> Recipe {
        Recipe {
            version: VERSION,
            source: SourceRef {
                track_id: "track-a".into(),
                path: "/music/a.flac".into(),
                fingerprint: "0".repeat(64),
                stem: stem.map(str::to_string),
                stem_set: stem.map(|_| "set-1".into()),
            },
            region: Resolved {
                rate: 48_000,
                start_frame: 0,
                end_frame: 96_000,
                first_beat: 0.0,
                beats: 4.0,
                bar: 0,
                phrase: Some(0),
                source_bpm: Some(128.0),
                source_key: None,
                confidence: 0.9,
                why: "bars 0…1".into(),
            },
            kind: Kind::BarLoop,
            transforms,
            analysis_version: 7,
        }
    }

    fn service(name: &str) -> Service {
        let root =
            std::env::temp_dir().join(format!("patch-material-svc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Service::new(Cache::new(root.join("a"), root.join("s"), 512 * 1024 * 1024).unwrap())
    }

    #[test]
    fn a_cached_asset_is_ready_immediately() {
        let s = service("cached");
        let src = Synthetic::new(4.0, None);
        let r = recipe(None, vec![]);
        let (id, st) = s.request(r.clone(), PRIORITY_NORMAL, None);
        assert_eq!(st, State::Queued);
        let state = s.process_one(&src, None).unwrap();
        assert_eq!(state, State::Ready);
        assert_eq!(s.state(&id), Some(State::Ready));
        // The cache short-circuits the second request.
        let (id2, st2) = s.request(r, PRIORITY_NORMAL, None);
        assert_eq!(id2, id);
        assert_eq!(st2, State::Ready);
    }

    #[test]
    fn a_deadline_skips_instead_of_late_material() {
        let s = service("deadline");
        let src = Synthetic::new(4.0, None);
        let (id, _) = s.request(recipe(None, vec![]), PRIORITY_NORMAL, Some(-1.0));
        let state = s.process_one(&src, None).unwrap();
        assert!(matches!(state, State::Failed(ref r) if r.contains("deadline")));
        assert!(matches!(s.state(&id), Some(State::Failed(_))));
    }

    #[test]
    fn a_missing_stem_degrades_to_the_full_mix_and_flags_it() {
        let s = service("degrade");
        let src = Synthetic::new(4.0, Some("vocals"));
        let (_, state) = s.prepare(recipe(Some("vocals"), vec![]), None);
        assert_eq!(state, State::Queued);
        let done = s.process_one(&src, None).unwrap();
        assert!(
            matches!(done, State::Degraded(ref why) if why.contains("full mix")),
            "{done:?}"
        );
    }

    #[test]
    fn a_refused_transform_fails_rather_than_faking_it() {
        let s = service("nofake");
        let src = Synthetic::new(4.0, None);
        let r = recipe(None, vec![Transform::TimeStretch { to_bpm: 124.0 }]);
        let _ = s.request(r, PRIORITY_NORMAL, None);
        let done = s.process_one(&src, None).unwrap();
        assert!(
            matches!(done, State::Failed(ref e) if e.contains("Rubber Band")),
            "{done:?}"
        );
    }

    #[test]
    fn kits_and_references_round_trip() {
        let s = service("kits");
        let src = Synthetic::new(4.0, None);
        let (id, _) = s.request(recipe(None, vec![]), PRIORITY_NORMAL, None);
        s.process_one(&src, None).unwrap();
        let mut kit = TemporaryKit::new(
            KitId("transition-1".into()),
            crate::kit::Scope::Transition,
            0,
        );
        kit.set("kick", id.clone()).unwrap();
        s.kit(kit);
        assert!(s.get_kit(&KitId("transition-1".into())).is_some());
        s.cache().reference(&id, "kit").unwrap();
        assert!(s.cache().referenced(&id));
        s.release(&id, "kit").unwrap();
        assert!(!s.cache().referenced(&id));
        let (done, failed) = s.counts();
        assert_eq!((done, failed), (1, 0));
    }

    #[test]
    fn two_regions_of_the_same_kind_are_different_assets() {
        let s = service("distinct");
        let src = Synthetic::new(10.0, None);
        let a = recipe(None, vec![]);
        let mut b = a.clone();
        b.region.start_frame = 48_000;
        b.region.end_frame = 144_000;
        let (ida, _) = s.request(a, PRIORITY_NORMAL, None);
        let (idb, _) = s.request(b, PRIORITY_NORMAL, None);
        assert_ne!(ida, idb);
        s.process_one(&src, None).unwrap();
        s.process_one(&src, None).unwrap();
        assert!(s.cache().has(&ida) && s.cache().has(&idb));
    }

    #[test]
    fn a_stem_request_decodes_that_stem() {
        let s = service("stem");
        let src = Synthetic::new(4.0, None);
        let _ = s.request(recipe(Some("vocals"), vec![]), PRIORITY_NORMAL, None);
        s.process_one(&src, None).unwrap();
        assert_eq!(src.seen.lock().unwrap().as_slice(), ["vocals"]);
    }
}
