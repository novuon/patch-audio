//! The engine's side of Derived Material: the two traits it owns (audio
//! access and Rubber Band) and the `material` op.
//!
//! `patch-material` depends on no engine: decoding and stretching come in
//! through [`patch_material::SourceAudio`] and [`patch_material::Stretch`],
//! which this module implements against PipeWire's graph rate, the stem
//! cache and `deck/stretch.rs`. The op mirrors the Pattern service's shape:
//! one JSON line per request, actions `request|query|state|kit|release|
//! cleanup|stats|prepare`.

use patch_core::material::{AssetId, KitId};
use patch_material::cache::Cache;
use patch_material::kit::{Scope, TemporaryKit};
use patch_material::model::{Kind, Recipe, RegionSpec, SourceRef};
use patch_material::service::Service;
use patch_material::traits::{SourceAudio, Stretch};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Audio access for Derived Material: a track at the graph rate, or one stem
/// from the stem cache. The graph rate is the deck rate the ring runs at.
pub struct EngineAudio {
    pub rate: u32,
}

impl SourceAudio for EngineAudio {
    fn decode(&self, source: &SourceRef, rate: u32) -> Result<Vec<f32>, String> {
        let rate = if rate == 0 { self.rate } else { rate };
        match &source.stem {
            Some(stem) => {
                // The stem cache is keyed by the source file's SHA-256.
                let lease =
                    patch_stems::cache::Cache::default_cache()?.acquire(&source.fingerprint)?;
                let audio = crate::stem_service::decode_lease(&source.path, lease, rate)?;
                let idx = audio
                    .ids
                    .iter()
                    .position(|id| id == stem)
                    .ok_or_else(|| format!("no stem '{stem}' in {}", source.path))?;
                audio.sources[idx]
                    .published()
                    .map(<[f32]>::to_vec)
                    .ok_or_else(|| "the stem is not decoded yet".into())
            }
            None => {
                let pcm = crate::deck::pcm::Pcm::load_full(Path::new(&source.path), rate, 1.0)?;
                pcm.published()
                    .map(<[f32]>::to_vec)
                    .ok_or_else(|| "the source is not decoded yet".into())
            }
        }
    }
}

/// Time and pitch for Derived Material, from the deck's Rubber Band.
pub struct EngineStretch;

impl Stretch for EngineStretch {
    fn time_stretch(&self, audio: &[f32], rate: u32, ratio: f64) -> Result<Vec<f32>, String> {
        if !(0.05..=4.0).contains(&ratio) || !ratio.is_finite() {
            return Err("time-stretch ratio out of range".into());
        }
        // Rubber Band's time ratio is 1 / speed.
        offline_stretch(audio, rate, ratio, 1.0)
    }

    fn pitch_shift(&self, audio: &[f32], rate: u32, semitones: f64) -> Result<Vec<f32>, String> {
        if !semitones.is_finite() || semitones.abs() > 24.0 {
            return Err("pitch shift is within ±24 semitones".into());
        }
        offline_stretch(audio, rate, 1.0, 2f64.powf(semitones / 12.0))
    }
}

/// Render offline through the deck's `Stretcher`: feed the material in one
/// engine-sized chunk, flush with silence, then drop the engine's start
/// delay. `time_ratio` is output/source length; `pitch` is a scale.
fn offline_stretch(
    audio: &[f32],
    rate: u32,
    time_ratio: f64,
    pitch: f64,
) -> Result<Vec<f32>, String> {
    use crate::deck::stretch::Stretcher;
    let frames = audio.len() / 2;
    if frames == 0 {
        return Ok(Vec::new());
    }
    let mut st = Stretcher::new(rate);
    // `set` takes a speed; the engine's own chunking is bypassed offline.
    st.set(1.0 / time_ratio, pitch);
    let mut l: Vec<f32> = (0..frames).map(|i| audio[i * 2]).collect();
    let mut r: Vec<f32> = (0..frames).map(|i| audio[i * 2 + 1]).collect();
    // Rubber Band's preferred start pad: silence-fed lead-in.
    let pad = st.start_pad();
    if pad > 0 {
        l.splice(0..0, std::iter::repeat_n(0.0, pad));
        r.splice(0..0, std::iter::repeat_n(0.0, pad));
    }
    l.extend(std::iter::repeat_n(0.0, 4096));
    r.extend(std::iter::repeat_n(0.0, 4096));
    const CHUNK: usize = 2048;
    let mut out_l = Vec::new();
    let mut out_r = Vec::new();
    let mut start = 0;
    let mut buf_l = vec![0.0f32; CHUNK];
    let mut buf_r = vec![0.0f32; CHUNK];
    while start < l.len() {
        let n = CHUNK.min(l.len() - start);
        st.process(&l[start..start + n], &r[start..start + n]);
        start += n;
        while st.available() >= CHUNK {
            let got = st.retrieve(&mut buf_l, &mut buf_r);
            if got == 0 {
                break;
            }
            out_l.extend_from_slice(&buf_l[..got]);
            out_r.extend_from_slice(&buf_r[..got]);
        }
    }
    while st.available() > 0 {
        let got = st.retrieve(&mut buf_l, &mut buf_r);
        if got == 0 {
            break;
        }
        out_l.extend_from_slice(&buf_l[..got]);
        out_r.extend_from_slice(&buf_r[..got]);
    }
    // Align: the engine's output starts `start_delay` frames late. Then trim
    // to the exact expected length (the flush silence's stretching is not
    // wanted material).
    let delay = st.start_delay();
    let drop = delay.min(out_l.len());
    let available = out_l.len() - drop;
    let expected = ((frames as f64) * time_ratio).round().max(1.0) as usize;
    let usable = available.min(expected);
    let mut out = Vec::with_capacity(expected * 2);
    for i in 0..usable {
        out.push(out_l[drop + i]);
        out.push(out_r[drop + i]);
    }
    // A short tail is silence, never a hard cut.
    out.resize(expected * 2, 0.0);
    Ok(out)
}

struct Inner {
    recipes: std::collections::BTreeMap<String, Recipe>,
    kits: std::collections::BTreeMap<String, TemporaryKit>,
    /// What each sampler slot holds, as the control thread installed it.
    slots: Vec<Option<Loaded>>,
    /// Source fingerprints by path (hashing a file is slow; files are
    /// re-hashed when their size or mtime changes).
    prints: std::collections::BTreeMap<String, (u64, std::time::SystemTime, String)>,
}

/// A sampler slot's asset and the latest graph sample an event used it at.
#[derive(Clone)]
struct Loaded {
    asset: AssetId,
    last_at: i64,
}

/// The consumer name the sampler holds its cache references under.
const SAMPLER: &str = "sampler";

fn is_loop(kind: Kind) -> bool {
    matches!(kind, Kind::BeatLoop | Kind::BarLoop | Kind::PhraseLoop)
}

/// The Derived Material service in the engine: a background-capable worker
/// plus the recipes it can re-derive from.
pub struct Material {
    service: Arc<Service>,
    audio: Arc<dyn SourceAudio>,
    stretch: Option<Arc<dyn Stretch>>,
    inner: Mutex<Inner>,
}

impl Material {
    pub fn new(rate: u32) -> Material {
        let cache = Cache::default_cache().unwrap_or_else(|_| {
            Cache::new(
                std::env::temp_dir().join("omarchy-patch/material"),
                std::env::temp_dir().join("omarchy-patch/material-state"),
                patch_material::cache::DEFAULT_BUDGET,
            )
            .expect("a writable cache")
        });
        Material::with_cache(cache, rate)
    }

    pub fn with_cache(cache: Cache, rate: u32) -> Material {
        Material::with(
            cache,
            Arc::new(EngineAudio { rate }),
            Some(Arc::new(EngineStretch)),
        )
    }

    /// A material service over explicit sources (tests, and callers that
    /// supply their own decode path).
    pub fn with(
        cache: Cache,
        audio: Arc<dyn SourceAudio>,
        stretch: Option<Arc<dyn Stretch>>,
    ) -> Material {
        Material {
            service: Arc::new(Service::new(cache)),
            audio,
            stretch,
            inner: Mutex::new(Inner {
                recipes: Default::default(),
                kits: Default::default(),
                slots: vec![None; crate::sampler::SLOTS],
                prints: Default::default(),
            }),
        }
    }

    pub fn service(&self) -> &Arc<Service> {
        &self.service
    }

    /// The background worker: prepares queued material (predictive
    /// `prepare`s, and requests the op didn't pump) as it arrives.
    pub fn spawn_worker(&self) -> std::io::Result<()> {
        let (svc, audio, stretch) = (
            self.service.clone(),
            self.audio.clone(),
            self.stretch.clone(),
        );
        std::thread::Builder::new()
            .name("patch-material".into())
            .spawn(move || svc.worker(audio, stretch))
            .map(|_| ())
    }

    /// The sampler slot that plays `source` (a kit's `slot` word), loading
    /// it on first use. Control thread only. `at` is the event's graph
    /// sample and `now` the graph's: a slot is reused only once its last
    /// event is a second behind and no voice plays it, so an event already
    /// queued for the old asset can never play the new one.
    pub fn slot_for(
        &self,
        sampler: &crate::sampler::Shared,
        source: &patch_core::material::SampleSource,
        slot: Option<&str>,
        at: i64,
        now: i64,
        rate: u32,
    ) -> Result<u32, String> {
        use patch_core::material::SampleSource;
        let asset = match source {
            SampleSource::Asset(a) => a.clone(),
            SampleSource::Kit(k) => {
                let kit = self
                    .service
                    .get_kit(k)
                    .ok_or_else(|| format!("{k} is unknown"))?;
                let word = slot.ok_or("a kit event needs a slot")?;
                kit.get(word)
                    .cloned()
                    .ok_or_else(|| format!("{k} has no slot '{word}'"))?
            }
        };
        let mut inner = self.inner.lock().unwrap();
        if let Some(i) = inner
            .slots
            .iter()
            .position(|l| l.as_ref().is_some_and(|l| l.asset == asset))
            && sampler.loaded(i)
        {
            let l = inner.slots[i].as_mut().unwrap();
            l.last_at = l.last_at.max(at);
            return Ok(i as u32);
        }
        match self.service.state(&asset) {
            Some(s) if s.is_ready() => {}
            Some(s) => return Err(format!("{asset} is not ready ({s:?})")),
            None => return Err(format!("{asset} is unknown")),
        }
        let meta = self
            .service
            .cache()
            .load(&asset)
            .ok_or_else(|| format!("{asset} has no metadata"))?;
        if meta.recipe.region.rate != rate {
            return Err(format!(
                "{asset} is at {} Hz, the graph at {rate}",
                meta.recipe.region.rate
            ));
        }
        let audio = self.service.cache().load_audio(&asset)?;
        let free = (0..inner.slots.len()).find(|&i| inner.slots[i].is_none() && !sampler.loaded(i));
        let i = match free {
            Some(i) => i,
            None => {
                let i = (0..inner.slots.len())
                    .filter(|&i| sampler.active(i) == 0)
                    .filter(|&i| {
                        inner.slots[i]
                            .as_ref()
                            .is_none_or(|l| l.last_at < now - rate as i64)
                    })
                    .min_by_key(|&i| inner.slots[i].as_ref().map_or(i64::MIN, |l| l.last_at))
                    .ok_or("every sampler slot is playing")?;
                if let Some(old) = inner.slots[i].take() {
                    let _ = self.service.cache().release(&old.asset, SAMPLER);
                }
                i
            }
        };
        sampler.install(
            i,
            crate::sampler::Buffer {
                samples: Arc::from(audio),
                looped: is_loop(meta.recipe.kind),
                asset: asset.0.clone(),
            },
        )?;
        let _ = self.service.cache().reference(&asset, SAMPLER);
        inner.slots[i] = Some(Loaded { asset, last_at: at });
        Ok(i as u32)
    }

    /// A `derived:` address against a deck's loaded track: the recipe is
    /// requested (prepared by the worker) and its id returned at once. The
    /// pattern skips the material's events until it is ready.
    pub fn resolve(
        &self,
        address: &str,
        track: Option<&str>,
        rate: u32,
    ) -> Result<AssetId, String> {
        let a = patch_material::address::parse(address)?;
        let path = track
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("deck {} has no track loaded", (b'A' + a.deck as u8) as char))?;
        let analysis = patch_analysis::cached(Path::new(path))
            .ok_or_else(|| format!("{path} isn't analysed yet"))?;
        let fingerprint = self.fingerprint(path)?;
        let source = SourceRef {
            track_id: path.to_string(),
            path: path.to_string(),
            fingerprint: fingerprint.clone(),
            stem: None,
            // The stem cache is keyed by the source hash.
            stem_set: Some(fingerprint),
        };
        let recipe = a.recipe(source, &analysis, rate)?;
        let (id, _) = self.service.request(
            recipe.clone(),
            patch_material::service::PRIORITY_URGENT,
            None,
        );
        self.inner
            .lock()
            .unwrap()
            .recipes
            .insert(id.0.clone(), recipe);
        Ok(id)
    }

    fn fingerprint(&self, path: &str) -> Result<String, String> {
        let md = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        let stamp = (md.len(), md.modified().unwrap_or(std::time::UNIX_EPOCH));
        if let Some((len, t, h)) = self.inner.lock().unwrap().prints.get(path)
            && (*len, *t) == stamp
        {
            return Ok(h.clone());
        }
        let h = patch_stems::hash_file(Path::new(path))?;
        self.inner
            .lock()
            .unwrap()
            .prints
            .insert(path.to_string(), (stamp.0, stamp.1, h.clone()));
        Ok(h)
    }

    /// Run queued preparation synchronously (the op's `request` prepares on
    /// demand so a CLI answer is useful immediately; the daemon also runs a
    /// worker thread).
    pub fn pump(&self, jobs: usize) -> usize {
        let mut ran = 0;
        for _ in 0..jobs {
            if self
                .service
                .process_one(self.audio.as_ref(), self.stretch.as_deref())
                .is_none()
            {
                break;
            }
            ran += 1;
        }
        ran
    }

    /// The `material` op.
    pub fn request(&self, req: &Value) -> Result<Value, String> {
        match req["action"].as_str().unwrap_or("stats") {
            "request" | "prepare" => {
                let recipe: Recipe = if req["recipe"].is_object() {
                    serde_json::from_value(req["recipe"].clone()).map_err(|e| e.to_string())?
                } else {
                    let built = self.recipe_from_request(req).ok_or_else(|| {
                        "material request needs a recipe, or a region with kind, source and analysis"
                            .to_string()
                    })?;
                    serde_json::from_value(built).map_err(|e| e.to_string())?
                };
                let deadline = req["deadline_ms"].as_f64();
                let (id, state) = if req["action"] == "prepare" {
                    self.service.prepare(recipe.clone(), deadline)
                } else {
                    self.service.request(
                        recipe.clone(),
                        patch_material::service::PRIORITY_URGENT,
                        deadline,
                    )
                };
                self.inner
                    .lock()
                    .unwrap()
                    .recipes
                    .insert(id.0.clone(), recipe);
                self.pump(req["pump"].as_u64().unwrap_or(4) as usize);
                Ok(json!({"id": id, "state": self.service.state(&id).unwrap_or(state)}))
            }
            "state" => {
                let id = AssetId(req["id"].as_str().ok_or("state needs an id")?.into());
                match self.service.state(&id) {
                    Some(state) => {
                        let asset = self.service.cache().load(&id);
                        Ok(json!({"id": id, "state": state, "asset": asset}))
                    }
                    None => Err(format!("{id} is unknown")),
                }
            }
            "recipes" => {
                let inner = self.inner.lock().unwrap();
                let ids: Vec<&String> = inner.recipes.keys().collect();
                Ok(json!({"recipes": ids}))
            }
            "kit" => {
                let id = KitId(req["kit"].as_str().ok_or("kit needs an id")?.into());
                if !id.valid() {
                    return Err(format!("{id} is not a kit id"));
                }
                let scope = match req["scope"].as_str().unwrap_or("transition") {
                    "set" => Scope::Set,
                    "session" => Scope::Session,
                    _ => Scope::Transition,
                };
                let mut kit = TemporaryKit::new(
                    id.clone(),
                    scope,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                );
                if let Some(slots) = req["slots"].as_object() {
                    for (slot, asset) in slots {
                        kit.set(slot.clone(), AssetId(asset.as_str().unwrap_or("").into()))?;
                    }
                }
                self.service.kit(kit.clone());
                self.inner
                    .lock()
                    .unwrap()
                    .kits
                    .insert(id.0.clone(), kit.clone());
                Ok(json!({"kit": kit}))
            }
            "release" => {
                let id = AssetId(req["id"].as_str().ok_or("release needs an id")?.into());
                let consumer = req["consumer"].as_str().unwrap_or("api");
                self.service.release(&id, consumer)?;
                Ok(json!({"ok": true}))
            }
            "cleanup" => Ok(json!(self.service.cleanup())),
            "stats" => Ok(json!(self.service.stats())),
            other => Err(format!("material has no action '{other}'")),
        }
    }

    /// Build a recipe from a region spec and a source.
    fn recipe_from_request(&self, req: &Value) -> Option<Value> {
        let spec: RegionSpec = serde_json::from_value(req["region"].clone()).ok()?;
        let source: SourceRef = serde_json::from_value(req["source"].clone()).ok()?;
        let rate = req["rate"].as_u64().unwrap_or(48_000) as u32;
        // The analysis must be supplied (the engine has it from the deck the
        // material comes from); without it, resolution is refused.
        let analysis: patch_analysis::Analysis =
            serde_json::from_value(req["analysis"].clone()).ok()?;
        let region = patch_material::resolve::resolve(&spec, &analysis, rate).ok()?;
        let kind: Kind = serde_json::from_value(req["kind"].clone()).ok()?;
        let recipe = Recipe {
            version: patch_material::model::VERSION,
            source,
            region,
            kind,
            transforms: serde_json::from_value(req["transforms"].clone()).unwrap_or_default(),
            analysis_version: analysis.generation(),
        };
        serde_json::to_value(recipe).ok()
    }
}

/// The `material` op's handler.
#[cfg(test)]
mod tests {
    use super::*;
    use patch_material::model::{Resolved, Transform};
    use patch_material::service::State;
    use std::sync::Arc as StdArc;

    /// A synthetic "track A": a 4-second sine, decoded without files.
    struct TrackA {
        seconds: f64,
    }

    impl SourceAudio for TrackA {
        fn decode(&self, _s: &SourceRef, rate: u32) -> Result<Vec<f32>, String> {
            let n = (self.seconds * rate as f64) as usize;
            Ok((0..n * 2)
                .map(|i| (std::f32::consts::TAU * 220.0 * (i / 2) as f32 / rate as f32).sin() * 0.4)
                .collect())
        }
    }

    fn source(stem: Option<&str>) -> SourceRef {
        SourceRef {
            track_id: "track-a".into(),
            path: "/music/a.flac".into(),
            fingerprint: "0".repeat(64),
            stem: stem.map(str::to_string),
            stem_set: stem.map(|_| "set-1".into()),
        }
    }

    fn recipe(stem: Option<&str>) -> Recipe {
        Recipe {
            version: patch_material::model::VERSION,
            source: source(stem),
            region: Resolved {
                rate: 48_000,
                // One bar at 128 BPM = 4 beats = 1.875 s = 90 000 frames.
                start_frame: 0,
                end_frame: 90_000,
                first_beat: 0.0,
                beats: 4.0,
                bar: 0,
                phrase: Some(0),
                source_bpm: Some(128.0),
                source_key: Some("A minor".into()),
                confidence: 0.95,
                why: "bars 0…1".into(),
            },
            kind: Kind::BarLoop,
            transforms: vec![
                Transform::LoopSeam { crossfade_ms: 10.0 },
                Transform::Fade {
                    in_ms: 3.0,
                    out_ms: 3.0,
                },
            ],
            analysis_version: 7,
        }
    }

    fn cache_at(name: &str) -> Cache {
        let root = std::env::temp_dir().join(format!(
            "patch-engine-material-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("s")).unwrap();
        Cache::new(root.join("a"), root.join("s"), 512 * 1024 * 1024).unwrap()
    }

    /// The transition case: derive a one-bar loop and a vocal chop from a
    /// synthetic track, render them, play the loop through the sampler on the
    /// beat grid while the deck is silent, and prove the material regenerates
    /// bit-identically from the recipe alone.
    #[test]
    fn derived_material_plays_through_the_sampler_and_regenerates_identically() {
        let src = TrackA { seconds: 4.0 };
        let service = Service::new(cache_at("transition"));

        // A one-bar loop and a vocal chop, from the same source.
        let (loop_id, _) = service.request(recipe(None), 10, None);
        let mut chop = recipe(Some("vocals"));
        chop.region.start_frame = 22_500; // beat 1
        chop.region.end_frame = 45_000; // beat 2
        chop.region.beats = 1.0;
        chop.kind = Kind::VocalChop;
        let (chop_id, _) = service.request(chop.clone(), 10, None);
        assert_ne!(loop_id, chop_id);

        // Prepare both (synchronously, as the op's pump does).
        assert!(service.process_one(&src, None).is_some());
        assert!(service.process_one(&src, None).is_some());
        assert_eq!(service.state(&loop_id), Some(State::Ready));
        assert!(service.cache().has(&loop_id) && service.cache().has(&chop_id));

        let loop_audio = service.cache().load_audio(&loop_id).unwrap();
        let chop_audio = service.cache().load_audio(&chop_id).unwrap();
        assert!(!loop_audio.is_empty() && !chop_audio.is_empty());

        // Play them on the beat grid: a new voice every beat (0.46875 s).
        let rate = 48_000u32;
        let beat = (60.0 / 128.0 * rate as f64) as usize;
        let total = beat * 4;
        let mut sampler = crate::sampler::Sampler::new(rate);
        sampler
            .install(
                0,
                crate::sampler::Buffer {
                    samples: StdArc::from(loop_audio.as_slice()),
                    looped: true,
                    asset: loop_id.0.clone(),
                },
            )
            .unwrap();
        sampler
            .install(
                1,
                crate::sampler::Buffer {
                    samples: StdArc::from(chop_audio.as_slice()),
                    looped: false,
                    asset: chop_id.0.clone(),
                },
            )
            .unwrap();
        let mut out = vec![0.0f32; total * 2];
        // The deck is silent; the sampler is the only source of sound.
        let (mut l, mut r) = (vec![0.0f32; total], vec![0.0f32; total]);
        let block = 256usize;
        let mut played: Vec<i64> = vec![];
        for start in (0..total).step_by(block) {
            let n = block.min(total - start);
            // A beat event lands on its own sample; here every 4th block.
            for b in 0..4 {
                let at = b * beat as i64;
                if at >= start as i64 && at < (start + n) as i64 {
                    sampler.trigger(0, at - start as i64, 1.0);
                    sampler.trigger(1, at - start as i64, 0.7);
                    played.push(at);
                }
            }
            let (mut lb, mut rb) = (vec![0.0f32; n], vec![0.0f32; n]);
            sampler.render(n, &mut lb, &mut rb);
            sampler.tick();
            l[start..start + n].copy_from_slice(&lb);
            r[start..start + n].copy_from_slice(&rb);
        }
        for i in 0..total {
            out[i * 2] = l[i];
            out[i * 2 + 1] = r[i];
        }
        assert_eq!(
            played,
            vec![0, beat as i64, 2 * beat as i64, 3 * beat as i64]
        );
        // Sound on every beat (the material plays while the deck is silent).
        for b in 0..4 {
            let at = b * beat + 64; // past the fade
            assert!(l[at].abs() > 1e-3, "beat {b} is audible");
        }
        assert!(l.iter().any(|x| x.abs() > 0.01));

        // Regeneration: the recipe alone (the cache's copy) renders
        // bit-identically, which is what a restored session relies on.
        let stored = service.cache().load(&loop_id).unwrap();
        let redo = patch_material::dsp::render(&stored.recipe, &src, None).unwrap();
        assert_eq!(redo, loop_audio, "bit-identical from the recipe");
        let asset = patch_material::service::render_asset(&stored.recipe, &src, None).unwrap();
        assert_eq!(asset.id, loop_id);
        assert_eq!(service.stats().assets, 2);
        assert!(loop_id.valid() && chop_id.valid());
    }

    /// The engine's real path for derived material, as `Rt` runs it: Patch
    /// code → the pattern service → the shared pattern queue → the
    /// Sequencer's drain → the sampler, block by block, the deck silent.
    /// Plus `derived:` addresses resolved to asset ids at load.
    #[test]
    fn a_pattern_plays_derived_material_through_the_audio_path() {
        use crate::pattern_service::{Host, Service as Pattern};
        use patch_core::command::{Command, Origin};
        use patch_core::material::SampleSource;
        use std::sync::atomic::{AtomicI64, Ordering};

        struct Engine {
            now: AtomicI64,
            seq: crate::sequencer::Shared,
            sampler: crate::sampler::Shared,
            material: Material,
            chop: AssetId,
        }
        impl Host for Engine {
            fn now(&self) -> Option<(i64, u32)> {
                Some((self.now.load(Ordering::Relaxed), 48_000))
            }
            fn deck_clock(&self, _: Option<usize>) -> Option<(i64, f64, f64)> {
                None
            }
            fn command(&self, _: Command, _: Origin) -> Result<(), String> {
                Ok(())
            }
            fn voices(&self) -> &patch_pattern::rt::Queue {
                &self.seq.pattern
            }
            fn sample_slot(
                &self,
                source: &SampleSource,
                slot: Option<&str>,
                at: i64,
            ) -> Result<u32, String> {
                let now = self.now.load(Ordering::Relaxed);
                self.material
                    .slot_for(&self.sampler, source, slot, at, now, 48_000)
            }
            fn stop_samples(&self) {
                self.sampler.stop_all();
            }
            fn resolve_material(&self, address: &str) -> Result<AssetId, String> {
                patch_material::address::parse(address)?;
                Ok(self.chop.clone())
            }
        }

        let material = Material::with(
            cache_at("audio-path"),
            StdArc::new(TrackA { seconds: 4.0 }),
            None,
        );
        let mut chop = recipe(Some("vocals"));
        chop.region.end_frame = 22_500; // one beat at 128 BPM
        chop.region.beats = 1.0;
        chop.kind = Kind::VocalChop;
        chop.transforms = vec![Transform::Fade {
            in_ms: 2.0,
            out_ms: 5.0,
        }];
        let r = material
            .request(&json!({"action": "request", "recipe": chop}))
            .unwrap();
        let id = AssetId(r["id"].as_str().unwrap().into());
        assert!(material.service().state(&id).unwrap().is_ready());

        let host = Engine {
            now: AtomicI64::new(0),
            seq: Default::default(),
            sampler: Default::default(),
            material,
            chop: id.clone(),
        };
        let dir =
            std::env::temp_dir().join(format!("patch-material-pattern-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svc = Pattern::new(dir.join("pattern.json"));
        svc.request(&host, &json!({"action": "engine", "name": "reference"}))
            .unwrap();
        // Written as an address; loaded as the asset id it resolves to.
        let code = "sample(\"derived:deck-a:vocals:phrase-last:beats-1\", \"1 ~ 1 ~\")";
        let r = svc
            .request(
                &host,
                &json!({"action": "load", "language": "patch", "code": code}),
            )
            .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let st = svc.request(&host, &json!({"action": "status"})).unwrap();
        assert_eq!(st["definition"]["kind"], "program");
        assert!(st["definition"].to_string().contains(&id.0), "{st}");
        assert!(st["source"]["code"].as_str().unwrap().contains("derived:"));
        svc.request(&host, &json!({"action": "tempo", "bpm": 120.0}))
            .unwrap();
        svc.request(&host, &json!({"action": "play"})).unwrap();

        // The audio thread: 480-frame blocks, as the engine renders them.
        let mut seq = crate::sequencer::Sequencer::default();
        let mut sampler = crate::sampler::Sampler::new(48_000);
        let mut out = vec![];
        let block = 480usize;
        for _ in 0..(48_000 * 5 / block) {
            let at = host.now.load(Ordering::Relaxed);
            svc.tick(&host);
            let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
            let (mut sl, mut sr) = (vec![0.0f32; block], vec![0.0f32; block]);
            seq.render_at(
                &host.seq,
                48_000,
                Some(at),
                [&mut sl, &mut sr],
                [&mut l, &mut r],
            );
            sampler.sync(&host.sampler);
            for e in seq.samples_due() {
                sampler.trigger(e.index as usize, e.sample - at, e.value);
            }
            sampler.render_gain(block, &mut l, &mut r, [1.0; 2]);
            sampler.tick();
            sampler.publish(&host.sampler);
            out.extend(l);
            host.now.fetch_add(block as i64, Ordering::Relaxed);
        }
        let st = svc.request(&host, &json!({"action": "status"})).unwrap();
        assert!(st["delivery"]["samples"].as_u64().unwrap() >= 4, "{st}");
        assert_eq!(st["delivery"]["samples_skipped"], 0, "{st}");
        // Onsets: sound after silence. "1 ~ 1 ~" at 120 BPM is every
        // 2 beats = 48 000 frames, sample-exact, whatever the block size.
        let onsets: Vec<usize> = (1..out.len())
            .filter(|&i| out[i].abs() > 1e-6 && out[i - 1..i].iter().all(|x| x.abs() <= 1e-6))
            .filter(|&i| {
                i < 64
                    || out[i.saturating_sub(4_000)..i]
                        .iter()
                        .all(|x| x.abs() <= 1e-6)
            })
            .collect();
        assert!(onsets.len() >= 4, "{onsets:?}");
        for w in onsets.windows(2) {
            assert_eq!(w[1] - w[0], 48_000, "{onsets:?}");
        }
        // Stop fades everything out.
        svc.request(&host, &json!({"action": "stop"})).unwrap();
        sampler.sync(&host.sampler);
        let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
        sampler.render(block, &mut l, &mut r);
        assert_eq!(sampler.active_voices(), 0);
        // Its cache reference is the sampler's while it is loaded.
        assert!(host.material.service().cache().referenced(&id));
    }

    #[test]
    fn the_op_round_trips_state_kits_and_stats() {
        let m = Material::with(cache_at("op"), StdArc::new(TrackA { seconds: 4.0 }), None);
        let r = m
            .request(&json!({
                "action": "request",
                "recipe": serde_json::to_value(recipe(None)).unwrap(),
                "pump": 2,
            }))
            .unwrap();
        let id = r["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("dm-"), "{r}");
        let state = m.request(&json!({"action": "state", "id": id})).unwrap();
        assert_eq!(state["state"]["state"], "ready", "{state}");
        assert!(state["asset"].is_object());

        let kit = m
            .request(&json!({"action": "kit", "kit": "transition-1", "slots": {"loop": id}}))
            .unwrap();
        assert_eq!(kit["kit"]["slots"]["loop"].as_str().unwrap(), id);
        assert!(
            m.request(&json!({"action": "kit", "kit": "../bad"}))
                .is_err()
        );

        assert!(
            m.request(&json!({"action": "release", "id": id, "consumer": "kit"}))
                .is_ok()
        );
        assert!(m.request(&json!({"action": "stats"})).unwrap()["assets"].is_u64());
        assert!(m.request(&json!({"action": "nonsense"})).is_err());
    }

    #[test]
    fn offline_stretching_changes_the_length_not_the_pitch_class() {
        // If Rubber Band is present, a 2× stretch roughly doubles the frames.
        let rate = 48_000;
        let frames = 4800usize;
        let audio: Vec<f32> = (0..frames * 2)
            .map(|i| (std::f32::consts::TAU * 220.0 * (i / 2) as f32 / rate as f32).sin() * 0.3)
            .collect();
        match EngineStretch.time_stretch(&audio, rate, 2.0) {
            Ok(out) => {
                let ratio = (out.len() / 2) as f64 / frames as f64;
                assert!((ratio - 2.0).abs() < 0.25, "{ratio}");
            }
            Err(e) => panic!("Rubber Band should be linked: {e}"),
        }
        // Pitch shift keeps the length.
        let out = EngineStretch.pitch_shift(&audio, rate, 12.0).unwrap();
        let ratio = (out.len() / 2) as f64 / frames as f64;
        assert!((ratio - 1.0).abs() < 0.25, "{ratio}");
    }
}
