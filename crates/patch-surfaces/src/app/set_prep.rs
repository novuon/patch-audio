//! Set preparation in the Library: pre-listen to the highlighted track from
//! its major moments (start, drop 1, break, drop 2, outro) with keys, and put
//! it in the set prep being made (a second kind of collection, in the order
//! the tracks were added).

use super::*;
use crate::player::Action;
use patch_analysis::sections;

/// The track being pre-listened to, and its moments.
#[derive(Default)]
pub(super) struct Preview {
    pub url: String,
    pub title: String,
    /// The five cue moments (seconds), by `sections::CUE_WORDS`.
    pub cues: [Option<f64>; 5],
    /// Every part's start (seconds), for ← →.
    pub parts: Vec<f64>,
    pub bpm: Option<f64>,
    /// The moment last asked for: the next track starts there too.
    pub cue: usize,
    /// What the preview says beside the Player's own message.
    pub note: String,
    /// The HUD: artist · genre · years · tempo · key · loudness; the
    /// waveform as white masks (the window tints them); the parts.
    pub meta: String,
    pub rest: slint::Image,
    pub played: slint::Image,
    pub parts_view: Vec<TrackPart>,
    pub facts: Facts,
    /// Its length (seconds), 0 unknown.
    pub duration: f64,
    /// How it sits in the set prep, for (set, its order): made again when
    /// either changes.
    pub fit_for: Option<(i64, Vec<String>)>,
    pub fit_title: String,
    pub fits: Vec<PrepFit>,
}

/// What mixing two tracks depends on: key, tempo, energy (0…1, the body
/// of the track without intro and outro).
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Facts {
    pub key: Option<patch_analysis::key::Key>,
    pub bpm: Option<f64>,
    pub energy: Option<f32>,
}

impl Facts {
    fn of(a: &patch_analysis::Analysis) -> Self {
        let e = &a.energy;
        let energy = (!e.is_empty()).then(|| {
            let (lo, hi) = (e.len() / 5, (e.len() * 4 / 5).max(e.len() / 5 + 1).min(e.len()));
            e[lo..hi].iter().map(|v| *v as f32).sum::<f32>() / (hi - lo) as f32 / 255.0
        });
        Facts { key: a.key, bpm: a.bpm, energy }
    }
}

/// A track's analysis: the shared cache, else the library's document.
fn analysis_of(path: &str) -> Option<patch_analysis::Analysis> {
    if path.contains("://") {
        return None;
    }
    patch_analysis::cached_shared(std::path::Path::new(path))
        .map(|a| (*a).clone())
        .or_else(|| {
            patch_library::open_quick()
                .ok()?
                .analysis_doc(path)
                .and_then(|doc| serde_json::from_str(&doc).ok())
        })
}

thread_local! {
    /// Set prep tracks' facts, by path: read once.
    static FACTS: RefCell<HashMap<String, Option<Facts>>> = RefCell::new(HashMap::new());
}

fn facts_of(path: &str) -> Option<Facts> {
    FACTS.with(|f| {
        if let Some(v) = f.borrow().get(path) {
            return *v;
        }
        let v = analysis_of(path).map(|a| Facts::of(&a));
        f.borrow_mut().insert(path.to_string(), v);
        v
    })
}

/// How mixing `from` into `to` goes: key, tempo, energy, and a verdict
/// ("good": keys that fit and tempos within 3 %; "ok": one of them; "far").
pub(super) fn pair(from: Facts, to: Facts) -> (String, &'static str) {
    let mut words = vec![];
    let key_ok = match (from.key, to.key) {
        (Some(a), Some(b)) => {
            let ok = a.compatible(b);
            words.push(format!("{}→{} {}", a.camelot(), b.camelot(), if ok { "✓" } else { "✗" }));
            Some(ok)
        }
        _ => None,
    };
    let tempo = match (from.bpm, to.bpm) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => {
            let d = patch_ai::select::tempo_distance(a, b);
            let pct = (b / a - 1.0) * 100.0;
            words.push(if pct.abs() > 25.0 {
                format!("{a:.0}→{b:.0} BPM (half/double {:+.1} %)", -d.copysign(a - b) * 100.0)
            } else {
                format!("{a:.0}→{b:.1} BPM {pct:+.1} %")
            });
            Some(d)
        }
        _ => None,
    };
    if let (Some(a), Some(b)) = (from.energy, to.energy) {
        words.push(
            if b - a > 0.08 { "energy up" } else if a - b > 0.08 { "energy down" } else { "same energy" }.into(),
        );
    }
    let verdict = match (key_ok, tempo) {
        (Some(true), Some(d)) if d <= 0.03 => "good",
        (Some(false), Some(d)) if d > 0.06 => "far",
        (None, None) => "ok",
        (Some(k), Some(d)) if k || d <= 0.06 => "ok",
        (Some(true), None) => "ok",
        (None, Some(d)) if d <= 0.06 => "ok",
        _ => "far",
    };
    (words.join(" · "), verdict)
}

/// The fit's label for a set prep track: "after no. 4 · Night Ferry".
fn neighbour(word: &str, n: usize, title: &str) -> String {
    format!("{word} no. {n} · {title}")
}

impl AudioApp {
    /// The set prep `s` adds to: the one last viewed, else the newest.
    fn set_target(&self) -> Option<i64> {
        let sets = self.lib_sets.borrow();
        self.set_target
            .get()
            .filter(|id| sets.contains(id))
            .or_else(|| sets.iter().max().copied())
    }

    fn set_name(&self, id: i64) -> String {
        self.lib_collections
            .borrow()
            .iter()
            .find(|(c, ..)| *c == id)
            .map(|(_, n, _)| n.clone())
            .unwrap_or_default()
    }

    /// The moments of a track and its HUD, from its analysis (none when
    /// not analysed) and the library's facts.
    fn preview_of(item: &LibraryItem) -> Preview {
        let mut p = Preview {
            url: item.url.clone(),
            title: item.title.clone(),
            ..Default::default()
        };
        let a = analysis_of(&item.url);
        let mut meta: Vec<String> = vec![];
        for w in [item.detail.as_str(), item.genre.as_str()] {
            if !w.is_empty() {
                meta.push(w.to_string());
            }
        }
        match (item.year, item.year_release) {
            (y, r) if y > 0 && r > y => meta.push(format!("{y} (this release {r})")),
            (y, _) if y > 0 => meta.push(y.to_string()),
            _ => {}
        }
        if let Some(a) = a {
            if let Some(b) = a.bpm {
                meta.push(format!("{b:.1} BPM"));
            }
            if let Some(k) = a.key {
                meta.push(k.camelot());
            }
            if a.lufs.is_finite() && a.lufs < 0.0 {
                meta.push(format!("{:.1} LUFS", a.lufs));
            }
            if a.duration > 0.0 {
                meta.push(clock(a.duration));
            }
            let parts = sections::map(&a);
            p.cues = sections::cues(&parts);
            p.parts = parts.iter().map(|x| x.start_s).collect();
            p.bpm = a.bpm;
            p.facts = Facts::of(&a);
            p.duration = a.duration;
            if a.duration > 0.0 {
                let span = |t: f64| (t / a.duration).clamp(0.0, 1.0) as f32;
                p.parts_view = parts
                    .iter()
                    .map(|x| {
                        let word = x.kind.word().to_uppercase();
                        TrackPart {
                            from: span(x.start_s),
                            to: span(x.end_s),
                            chars: word.chars().count() as i32,
                            word: word.into(),
                            rank: 0,
                        }
                    })
                    .collect();
            }
            let wave: Vec<[f32; 2]> = a.waveform.iter().map(|w| [w[0] as f32 / 255.0, w[1] as f32 / 255.0]).collect();
            if !wave.is_empty() {
                p.rest = slint::Image::from_rgba8(crate::overview::render(&wave, [255; 4]));
                p.played = slint::Image::from_rgba8(crate::overview::render(&wave, [255; 4]));
            }
        }
        p.meta = meta.join(" · ");
        p
    }

    /// How the track being pre-listened to sits in the set prep `s` adds
    /// to: in it, beside its neighbours; else after its last track, and
    /// beside the one it mixes best with.
    fn prep_fits(&self, p: &mut Preview) {
        let Some(id) = self.set_target() else {
            p.fit_for = None;
            p.fit_title.clear();
            p.fits.clear();
            return;
        };
        let order = self.lib_set_order.borrow().get(&id).cloned().unwrap_or_default();
        if p.fit_for.as_ref().is_some_and(|(i, o)| *i == id && *o == order) {
            return;
        }
        let name = self.set_name(id);
        let titles: HashMap<String, String> = {
            let lib = self.library.borrow();
            order
                .iter()
                .filter_map(|u| lib.iter().find(|i| i.url == *u).map(|i| (u.clone(), i.title.clone())))
                .collect()
        };
        let title = |u: &str| titles.get(u).cloned().unwrap_or_else(|| {
            std::path::Path::new(u).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        });
        let me = p.facts;
        let fit = |label: String, from: Facts, to: Facts| {
            let (detail, verdict) = pair(from, to);
            PrepFit { label: label.into(), detail: detail.into(), verdict: verdict.into() }
        };
        let mut fits = vec![];
        if order.is_empty() {
            p.fit_title = format!("“{name}” is empty: s adds this as no. 1");
        } else if let Some(i) = order.iter().position(|u| *u == p.url) {
            p.fit_title = format!("No. {} in “{name}”", i + 1);
            if i > 0 {
                let prev = &order[i - 1];
                fits.push(fit(neighbour("after", i, &title(prev)), facts_of(prev).unwrap_or_default(), me));
            }
            if let Some(next) = order.get(i + 1) {
                fits.push(fit(neighbour("before", i + 2, &title(next)), me, facts_of(next).unwrap_or_default()));
            }
        } else {
            p.fit_title = format!("“{name}” · {} tracks", order.len());
            let n = order.len();
            let last = &order[n - 1];
            fits.push(fit(
                format!("as no. {} after · {}", n + 1, title(last)),
                facts_of(last).unwrap_or_default(),
                me,
            ));
            // The one it mixes best with, when that isn't the last.
            let score = |f: Facts| {
                let (_, v) = pair(f, me);
                let d = match (f.bpm, me.bpm) {
                    (Some(a), Some(b)) => patch_ai::select::tempo_distance(a, b),
                    _ => 1.0,
                };
                (match v { "good" => 0, "ok" => 1, _ => 2 }, (d * 1000.0) as i64)
            };
            if let Some((k, f)) = order[..n - 1]
                .iter()
                .enumerate()
                .filter_map(|(k, u)| facts_of(u).map(|f| (k, f)))
                .min_by_key(|(_, f)| score(*f))
                .filter(|(_, f)| score(*f) < score(facts_of(last).unwrap_or_default()))
            {
                fits.push(fit(neighbour("best beside", k + 1, &title(&order[k])), f, me));
            }
        }
        p.fits = fits;
        p.fit_for = Some((id, order));
    }

    /// Pre-listen to a track from moment `cue` (0 start … 4 outro).
    fn preview_start(&self, item: &LibraryItem, cue: usize) {
        let mut p = Self::preview_of(item);
        let word = sections::CUE_WORDS[cue.min(4)];
        let at = match p.cues[cue.min(4)] {
            Some(at) => at,
            None => {
                p.note = if p.parts.is_empty() {
                    "Not analysed yet: from the start".into()
                } else {
                    format!("No {word} found: from the start")
                };
                p.cues[0].unwrap_or(0.0)
            }
        };
        p.cue = cue.min(4);
        self.previewer.send(Action::Preview(p.url.clone(), at));
        self.prep_fits(&mut p);
        *self.preview.borrow_mut() = p;
    }

    /// End the pre-listen: pause it, give its channel back to main, and
    /// close the strip.
    pub(super) fn end_preview(&self) {
        self.previewer.send(Action::PreviewEnd);
        self.preview.borrow_mut().url.clear();
        self.preview_paused.set(None);
    }

    /// The pre-listen ends by itself when its track has played to the
    /// end, after a minute paused, and when no Library window is open.
    pub(super) fn preview_housekeeping(&self) {
        if self.frozen {
            return;
        }
        let shown = self.lib_views.borrow().iter().filter_map(|v| v.upgrade()).any(|v| {
            v.window.upgrade().is_some_and(|w| w.window().is_visible())
        });
        if !shown {
            self.end_preview();
            return;
        }
        let Some(s) = self.previewing() else { return };
        if s.ended {
            self.end_preview();
            return;
        }
        if s.playing || s.loading {
            self.preview_paused.set(None);
        } else {
            match self.preview_paused.get() {
                None => self.preview_paused.set(Some(Instant::now())),
                Some(at) if at.elapsed() >= Duration::from_secs(60) => self.end_preview(),
                Some(_) => {}
            }
        }
    }

    /// Whether the Player is playing the track being previewed.
    fn previewing(&self) -> Option<crate::player::Snapshot> {
        let url = self.preview.borrow().url.clone();
        if url.is_empty() {
            return None;
        }
        self.previewer.snapshot().filter(|s| s.path == url || s.loading)
    }

    /// Moving the cursor while a preview plays: the next track plays from
    /// the same moment (at drop 1, the next one's drop 1).
    pub(super) fn preview_follow(&self, view: &LibView) {
        let Some(s) = self.previewing() else { return };
        if !s.playing {
            return;
        }
        let Some(item) = self.cursor_item(view) else {
            return;
        };
        if item.url != self.preview.borrow().url {
            let cue = self.preview.borrow().cue;
            self.preview_start(&item, cue);
        }
    }

    /// Into the Library window: the preview strip and the set prep target.
    pub(super) fn push_set_prep(&self, d: &DjState, view: &LibView) {
        // Screenshots: a preview at drop 1, so the strip can be judged.
        let snap = if self.frozen {
            let mut p = self.preview.borrow_mut();
            if p.url.is_empty() {
                *p = mock_preview();
            }
            Some(crate::player::Snapshot {
                path: "mock".into(),
                route: "aux1".into(),
                playing: true,
                position: 131.0,
                duration: 384.0,
                ..Default::default()
            })
        } else {
            self.previewing()
        };
        if !self.frozen {
            let mut p = self.preview.borrow_mut();
            if !p.url.is_empty() {
                self.prep_fits(&mut p);
            }
        }
        let p = self.preview.borrow();
        super::trace_hud(format_args!(
            "S {} {}",
            match &snap { Some(s) if s.playing => "playing", Some(_) => "paused", None => "none" },
            snap.as_ref().map_or(0.0, |s| s.position)
        ));
        match &snap {
            Some(s) => {
                let dur = if s.duration > 0.0 { s.duration } else { 1.0 };
                d.set_prev_title(p.title.as_str().into());
                d.set_prev_playing(s.playing);
                let now = s.position_now();
                // Playing, the frame hook moves the strip and the HUD
                // (`glide_hud`) from this reading.
                let gliding = s.playing && !self.frozen;
                super::hud_reading(gliding.then(|| (s.position, s.read_at.unwrap_or_else(Instant::now), s.duration)));
                if !gliding {
                    d.set_prev_progress((now / dur).clamp(0.0, 1.0) as f32);
                }
                d.set_prev_time(format!("{} / {}", clock(now), clock(s.duration)).into());
                let marks: Vec<f32> = p
                    .cues
                    .iter()
                    .map(|c| c.map_or(-1.0, |t| (t / dur).clamp(0.0, 1.0) as f32))
                    .collect();
                d.set_prev_marks(Rc::new(VecModel::from(marks)).into());
                // The moment the playhead is in, by the cue keys' words.
                let at = p
                    .cues
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.map(|t| (i, t)))
                    .filter(|(_, t)| *t <= now + 0.05)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| sections::CUE_WORDS[i])
                    .unwrap_or("");
                d.set_prev_at(at.into());
                let msg = if !s.message.is_empty() { s.message.clone() } else { p.note.clone() };
                d.set_prev_message(msg.into());
                d.set_prev_route(
                    match s.route.as_str() {
                        "aux1" => "AUX 1",
                        "cue" => "HEADPHONES",
                        _ => "",
                    }
                    .into(),
                );
                // Said once a run, unless turned off.
                let notice = if s.route == "aux1"
                    && !self.aux_told.get()
                    && (self.frozen || super::dj_poll::aux_notice_on())
                {
                    if s.aux_heard {
                        "A deck or the Player is playing, so the pre-listen plays on AUX 1, kept off main. It goes back to main when nothing else plays."
                    } else {
                        "A deck or the Player is playing, so the pre-listen plays on AUX 1, kept off main. AUX 1 has no output of its own yet: in the Mixer, choose one on AUX 1 (your headphones) to hear it."
                    }
                } else {
                    ""
                };
                d.set_aux_notice(notice.into());
            }
            None => {
                super::hud_reading(None);
                d.set_prev_title("".into());
                d.set_prev_playing(false);
                d.set_prev_route("".into());
                d.set_aux_notice("".into());
                // A refusal (a deck plays on main) still has to be read.
                let msg = self
                    .previewer
                    .snapshot()
                    .filter(|_| !p.url.is_empty())
                    .map(|s| s.message)
                    .unwrap_or_default();
                d.set_prev_message(msg.into());
            }
        }
        self.push_hud(d, view, snap.as_ref(), &p);
        drop(p);
        let target = self.set_target();
        let cursor_url = view.urls.borrow().get(view.cursor.get()).cloned();
        match target {
            Some(id) => {
                let order = self.lib_set_order.borrow();
                let tracks = order.get(&id).map_or(0, |o| o.len());
                d.set_prep_target(self.set_name(id).into());
                d.set_prep_target_count(tracks as i32);
                d.set_prep_has_cursor(
                    cursor_url.is_some_and(|u| order.get(&id).is_some_and(|o| o.contains(&u))),
                );
            }
            None => {
                d.set_prep_target("".into());
                d.set_prep_target_count(0);
                d.set_prep_has_cursor(false);
            }
        }
        d.set_prep_viewed(self.viewing_ordered(view));
        d.set_lookups_ask(self.frozen || super::dj_poll::lookups_ask());
    }

    /// The HUD (h), when open: the pre-listen, or the highlighted track
    /// when nothing is pre-listened to. Its images, parts and set prep fit
    /// change only with the track.
    fn push_hud(&self, d: &DjState, view: &LibView, snap: Option<&crate::player::Snapshot>, p: &Preview) {
        if self.frozen && std::env::var_os("PATCH_SHOT_HUD").is_some() {
            d.set_hud_open(true);
        }
        if !d.get_hud_open() {
            if !d.get_hud_title().is_empty() {
                d.set_hud_title("".into());
            }
            return;
        }
        let idle: std::cell::Ref<Preview>;
        let (src, state, pos, dur, cue) = match snap {
            Some(s) => (p, if s.playing { "playing" } else { "paused" }, s.position_now(), s.duration, p.cue as i32),
            None => {
                let Some(item) = self.cursor_item(view) else {
                    d.set_hud_title("".into());
                    return;
                };
                {
                    let mut h = self.hud_idle.borrow_mut();
                    if h.url != item.url {
                        *h = Self::preview_of(&item);
                    }
                    self.prep_fits(&mut h);
                }
                idle = self.hud_idle.borrow();
                let dur = idle.duration;
                (&*idle, "", 0.0, dur, -1)
            }
        };
        let span = if dur > 0.0 { dur } else { src.duration.max(1.0) };
        if d.get_hud_title() != src.title.as_str() || d.get_prev_meta() != src.meta.as_str() {
            d.set_prev_meta(src.meta.as_str().into());
            d.set_prev_wave_rest(src.rest.clone());
            d.set_prev_wave_played(src.played.clone());
            d.set_prev_parts(Rc::new(VecModel::from(src.parts_view.clone())).into());
            let marks: Vec<f32> = src
                .cues
                .iter()
                .map(|c| c.map_or(-1.0, |t| (t / span).clamp(0.0, 1.0) as f32))
                .collect();
            d.set_hud_marks(Rc::new(VecModel::from(marks)).into());
        }
        d.set_hud_title(src.title.as_str().into());
        d.set_hud_state(state.into());
        // While it plays the frame hook moves the playhead (`glide_hud`);
        // otherwise it is set here.
        if snap.is_some_and(|s| s.playing) && !self.frozen {
            if let Some(w) = view.window.upgrade() {
                w.window().request_redraw();
            }
        } else {
            d.set_hud_progress((pos / span).clamp(0.0, 1.0) as f32);
        }
        d.set_hud_span(span as f32);
        d.set_hud_time(
            if state.is_empty() { clock(src.duration) } else { format!("{} / {}", clock(pos), clock(dur)) }.into(),
        );
        d.set_prev_cue(cue);
        d.set_prev_cue_word(sections::CUE_WORDS[(cue.max(0) as usize).min(4)].to_uppercase().into());
        if d.get_prev_fit_title() != src.fit_title.as_str() || d.get_prev_fits().iter().ne(src.fits.iter().cloned()) {
            d.set_prev_fit_title(src.fit_title.as_str().into());
            d.set_prev_fits(Rc::new(VecModel::from(src.fits.clone())).into());
        }
    }

    /// A new set prep, named by the day ("Set prep · 12 Nov", then "… 2").
    fn new_set_prep(&self) -> Result<(i64, String), String> {
        let day = day_label();
        let db = patch_library::open_quick()?;
        let taken: HashSet<String> = db.collections().into_iter().map(|(_, n, _)| n).collect();
        let base = format!("Set prep · {day}");
        let name = (1..)
            .map(|n| if n == 1 { base.clone() } else { format!("{base} {n}") })
            .find(|n| !taken.contains(n))
            .unwrap_or(base);
        let id = db.create_set_prep(&name)?;
        // Shown at once; the library's reload confirms it.
        self.lib_sets.borrow_mut().insert(id);
        self.lib_set_order.borrow_mut().insert(id, vec![]);
        self.lib_collections.borrow_mut().push((id, name.clone(), 0));
        self.set_target.set(Some(id));
        Ok((id, name))
    }

    pub(super) fn bind_set_prep(&self, w: &PatchWindow, view: &Rc<LibView>) {
        let d = w.global::<DjState>();
        // The answer to "Look up release years online?" (also in Settings).
        let me = self.me.clone();
        d.on_lookups_answer(move |allow| {
            let Some(a) = me.upgrade() else { return };
            if a.frozen {
                return;
            }
            let mut s = patch_core::setup::load();
            s.online_lookups = allow;
            s.online_lookups_asked = true;
            *a.dj_status.borrow_mut() = match patch_core::setup::save(&s) {
                Ok(()) if allow => "Release years: looked up at MusicBrainz in the background (Settings · DJ Booth turns it off)".into(),
                Ok(()) => "Release years: only those in your files' tags (Settings · DJ Booth can allow lookups)".into(),
                Err(e) => format!("Couldn't save the answer: {e}"),
            };
            a.poll_dj();
        });
        // h: the HUD opened or closed; shown now, not at the next poll.
        let (me, v) = (self.me.clone(), view.clone());
        d.on_hud_refresh(move || {
            let Some(a) = me.upgrade() else { return };
            if let Some(w) = v.window.upgrade() {
                a.push_set_prep(&w.global::<DjState>(), &v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_grid_edit(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else { return };
            let Some(analysis) = analysis_of(&item.url) else {
                *a.dj_status.borrow_mut() = "This track has no beat analysis yet".into();
                a.poll_dj();
                return;
            };
            let human = patch_library::open_quick().ok()
                .and_then(|db| db.human_grid_overrides().ok())
                .and_then(|grids| grids.get(&item.url).cloned());
            if let Some(w) = v.window.upgrade() {
                let d = w.global::<DjState>();
                d.set_lib_grid_bpm(human.as_ref().map_or(analysis.bpm.unwrap_or(120.0), |g| g.bpm) as f32);
                d.set_lib_grid_first_beat(human.as_ref().map_or(analysis.beat_offset.max(0.0), |g| g.first_beat_s) as f32);
                d.set_lib_grid_downbeat(human.and_then(|g| g.downbeat_phase).or(analysis.downbeat).unwrap_or(0) as i32);
                d.set_lib_grid_editing(true);
                d.set_hud_open(true);
                a.push_set_prep(&d, &v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_grid_beat_here(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.previewing() else {
                *a.dj_status.borrow_mut() = "Start pre-listen, then press b at the first beat".into();
                a.poll_dj();
                return;
            };
            if let Some(w) = v.window.upgrade() {
                w.global::<DjState>().set_lib_grid_first_beat(s.position_now().max(0.0) as f32);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_grid_reset(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else { return };
            let result = patch_library::open_quick().and_then(|db| db.clear_human_grid(&item.url));
            match result {
                Ok(()) => {
                    if let Some(w) = v.window.upgrade() { w.global::<DjState>().set_lib_grid_editing(false); }
                    *a.dj_status.borrow_mut() = "Back to Patch's analysed beat grid".into();
                    a.request_library(true);
                }
                Err(e) => *a.dj_status.borrow_mut() = format!("Couldn't reset beat grid: {e}"),
            }
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_grid_save(move |bpm, first, downbeat| {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else { return };
            let result = patch_library::open_quick().and_then(|db| {
                db.set_human_downbeat(&item.url, bpm as f64, first as f64, downbeat.clamp(0, 3) as u8)
            });
            if let Some(w) = v.window.upgrade() {
                let d = w.global::<DjState>();
                match result {
                    Ok(()) => {
                        d.set_lib_grid_editing(false);
                        *a.dj_status.borrow_mut() = "Beat grid saved in Patch".into();
                        a.request_library(true);
                    }
                    Err(e) => *a.dj_status.borrow_mut() = format!("Couldn't save beat grid: {e}"),
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_aux_notice_answer(move |keep| {
            let Some(a) = me.upgrade() else { return };
            a.aux_told.set(true);
            if !keep && !a.frozen {
                let mut s = patch_core::setup::load();
                s.preview_aux_notice = false;
                if let Err(e) = patch_core::setup::save(&s) {
                    *a.dj_status.borrow_mut() = format!("Couldn't save that: {e}");
                } else {
                    *a.dj_status.borrow_mut() = "Pre-listen on AUX 1: not said again (Settings · DJ Booth turns it back on)".into();
                }
            }
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_preview_toggle(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else {
                return;
            };
            let same = a.preview.borrow().url == item.url;
            if same && a.previewing().is_some() {
                a.previewer.send(Action::Toggle);
            } else {
                let cue = a.preview.borrow().cue;
                a.preview_start(&item, cue);
            }
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_preview_cue(move |n| {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else {
                return;
            };
            let n = (n.clamp(0, 4)) as usize;
            if a.preview.borrow().url == item.url && a.previewing().is_some() {
                let mut p = a.preview.borrow_mut();
                p.cue = n;
                match p.cues[n] {
                    Some(at) => {
                        p.note.clear();
                        a.previewer.send(Action::PreviewAt(at));
                    }
                    None => p.note = format!("No {} found in this track", sections::CUE_WORDS[n]),
                }
            } else {
                a.preview_start(&item, n);
            }
            a.poll_dj();
        });
        let me = self.me.clone();
        d.on_preview_part(move |dir| {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.previewing() else { return };
            let p = a.preview.borrow();
            // Back: the part before the one playing once 2 s into it.
            let to = if dir > 0 {
                p.parts.iter().copied().find(|t| *t > s.position_now() + 0.5)
            } else {
                p.parts.iter().copied().rev().find(|t| *t < s.position_now() - 2.0)
            };
            if let Some(at) = to {
                a.previewer.send(Action::PreviewAt(at));
            }
        });
        let me = self.me.clone();
        d.on_preview_bars(move |bars| {
            let Some(a) = me.upgrade() else { return };
            let Some(s) = a.previewing() else { return };
            let bar = a.preview.borrow().bpm.filter(|b| *b > 0.0).map_or(2.0, |b| 240.0 / b);
            a.previewer.send(Action::PreviewAt((s.position_now() + bars as f64 * bar).max(0.0)));
        });
        let me = self.me.clone();
        d.on_preview_stop(move || {
            if let Some(a) = me.upgrade() {
                a.end_preview();
                a.poll_dj();
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_prep_toggle(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.cursor_item(&v) else {
                return;
            };
            let target = match a.set_target() {
                Some(id) => Ok((id, a.set_name(id))),
                None => a.new_set_prep(),
            };
            let (id, name) = match target {
                Ok(t) => t,
                Err(e) => {
                    *a.dj_status.borrow_mut() = format!("Couldn't make a set prep: {e}");
                    a.poll_dj();
                    return;
                }
            };
            let had = a.lib_set_order.borrow().get(&id).is_some_and(|o| o.contains(&item.url));
            let done = patch_library::open_quick().and_then(|db| {
                let track = db
                    .track_by_path(&item.url)
                    .ok_or_else(|| "it isn't a library track".to_string())?;
                if had {
                    db.remove_from_collection(id, &[track], true)
                } else {
                    db.add_to_collection(id, track)
                }
            });
            match done {
                Ok(()) => {
                    {
                        let mut order = a.lib_set_order.borrow_mut();
                        let o = order.entry(id).or_default();
                        if had {
                            o.retain(|u| *u != item.url);
                        } else {
                            o.push(item.url.clone());
                        }
                        let mut members = a.lib_members.borrow_mut();
                        let m = members.entry(id).or_default();
                        if had {
                            m.remove(&item.url);
                        } else {
                            m.insert(item.url.clone());
                        }
                    }
                    let n = a.lib_set_order.borrow().get(&id).map_or(0, |o| o.len());
                    *a.dj_status.borrow_mut() = if had {
                        format!("Took “{}” out of “{name}” ({n})", item.title)
                    } else {
                        format!("“{}” is no. {n} in “{name}”", item.title)
                    };
                }
                Err(e) => *a.dj_status.borrow_mut() = format!("Couldn't change “{name}”: {e}"),
            }
            a.request_library(true);
            a.refresh_libraries();
            a.poll_dj();
        });
        let me = self.me.clone();
        d.on_prep_new(move || {
            let Some(a) = me.upgrade() else { return };
            *a.dj_status.borrow_mut() = match a.new_set_prep() {
                Ok((_, name)) => format!("Made “{name}”: highlight a track and press s to add it"),
                Err(e) => format!("Couldn't make a set prep: {e}"),
            };
            a.request_library(true);
            a.refresh_libraries();
            a.poll_dj();
        });
        // In its listed order, or as the DJ decides (o, or the header).
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_play_order(move |in_order| {
            let Some(a) = me.upgrade() else { return };
            let Some(id) = v.collection_view.get() else { return };
            if a.frozen {
                return;
            }
            let done = patch_library::open_quick().and_then(|db| {
                db.set_play_in_order(id, in_order)?;
                Ok(db.collection_paths(id))
            });
            let name = a.set_name(id);
            *a.dj_status.borrow_mut() = match done {
                Ok(paths) => {
                    a.lib_play_order.borrow_mut().insert(id, in_order);
                    if in_order {
                        a.lib_set_order.borrow_mut().insert(id, paths);
                        format!("“{name}” plays in this order: ⇧↑ ⇧↓ move a track (o: the DJ decides)")
                    } else {
                        if !a.lib_sets.borrow().contains(&id) {
                            a.lib_set_order.borrow_mut().remove(&id);
                        }
                        format!("“{name}”: the DJ decides the order (o: in this order)")
                    }
                }
                Err(e) => format!("Couldn't change how “{name}” plays: {e}"),
            };
            a.request_library(true);
            a.rebuild_library(&v);
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_prep_move(move |dir| {
            let Some(a) = me.upgrade() else { return };
            let Some(id) = v.collection_view.get().filter(|id| a.lib_set_order.borrow().contains_key(id)) else {
                return;
            };
            let Some(item) = a.cursor_item(&v) else {
                return;
            };
            let done = patch_library::open_quick().and_then(|db| {
                let track = db
                    .track_by_path(&item.url)
                    .ok_or_else(|| "it isn't a library track".to_string())?;
                db.move_in_collection(id, track, dir)
            });
            if let Err(e) = done {
                *a.dj_status.borrow_mut() = format!("Couldn't move it: {e}");
            } else if let Some(o) = a.lib_set_order.borrow_mut().get_mut(&id)
                && let Some(at) = o.iter().position(|u| *u == item.url)
            {
                let to = (at as i32 + dir.signum()).clamp(0, o.len() as i32 - 1) as usize;
                o.swap(at, to);
                v.cursor.set(to);
            }
            a.request_library(true);
            a.rebuild_library(&v);
            a.poll_dj();
        });
    }
}

/// m:ss
fn clock(s: f64) -> String {
    let s = if s.is_finite() { s.max(0.0) as u64 } else { 0 };
    format!("{}:{:02}", s / 60, s % 60)
}

/// Today as "12 Nov", in local time.
fn day_label() -> String {
    let stamp = patch_engine::session::now_s() as libc::time_t;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let mut buf = [0u8; 32];
    // localtime_r writes only to our tm; strftime writes at most buf.len().
    unsafe {
        if libc::localtime_r(&stamp, local.as_mut_ptr()).is_null() {
            return String::new();
        }
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%-d %b".as_ptr(), local.as_ptr());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// Screenshots: a track at drop 1 in a set prep of four, so the HUD can be
/// judged.
fn mock_preview() -> Preview {
    let dur = 384.0;
    // A club track's shape: intro, build, drop, break, drop, outro.
    let wave: Vec<[f32; 2]> = (0..600)
        .map(|i| {
            let t = i as f64 / 600.0 * dur;
            let level = match t {
                t if t < 30.0 => 0.35,
                t if t < 98.0 => 0.55,
                t if t < 160.0 => 0.85,
                t if t < 196.0 => 0.6,
                t if t < 240.0 => 0.25,
                t if t < 262.0 => 0.5,
                t if t < 340.0 => 0.9,
                _ => 0.4,
            };
            let wobble = ((i * 37 % 11) as f32 / 11.0 - 0.5) * 0.12;
            [(level as f32 + wobble + 0.08).min(1.0), (level as f32 * 0.7 + wobble * 0.5).max(0.02)]
        })
        .collect();
    let parts = [
        ("INTRO", 0.0, 30.0), ("BUILD", 30.0, 98.0), ("DROP", 98.0, 196.0),
        ("BREAK", 196.0, 262.0), ("DROP", 262.0, 352.0), ("OUTRO", 352.0, dur),
    ];
    let fit = |label: &str, detail: &str, verdict: &str| PrepFit {
        label: label.into(),
        detail: detail.into(),
        verdict: verdict.into(),
    };
    Preview {
        url: "mock".into(),
        title: "Harbour Lights (Extended Mix)".into(),
        cues: [Some(8.0), Some(98.0), Some(196.0), Some(262.0), Some(352.0)],
        cue: 1,
        duration: dur,
        meta: "Oda Vei · Deep House · 1998 (this release 2024) · 125.5 BPM · 9A · -8.1 LUFS · 6:24".into(),
        rest: slint::Image::from_rgba8(crate::overview::render(&wave, [255; 4])),
        played: slint::Image::from_rgba8(crate::overview::render(&wave, [255; 4])),
        parts_view: parts
            .iter()
            .map(|(w, a, b)| TrackPart {
                from: (*a / dur) as f32,
                to: (*b / dur) as f32,
                word: (*w).into(),
                chars: w.len() as i32,
                rank: 0,
            })
            .collect(),
        fit_title: "“Set prep · 13 Nov” · 4 tracks".into(),
        fits: vec![
            fit("as no. 5 after · Night Ferry", "8B→9A ✗ · 122→125.5 BPM +2.9 % · energy up", "far"),
            fit("best beside no. 2 · Midnight Transit", "8A→9A ✓ · 124→125.5 BPM +1.2 % · same energy", "good"),
        ],
        ..Default::default()
    }
}

/// The window process quitting with a pre-listen on: it ends with it (its
/// mpv runs detached and would play on with nothing to stop it).
impl Drop for AudioApp {
    fn drop(&mut self) {
        if !self.frozen && !self.preview.get_mut().url.is_empty() {
            crate::player::end_preview_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_analysis::key::Key;

    fn f(tonic: u8, minor: bool, bpm: f64, energy: f32) -> Facts {
        Facts { key: Some(Key { tonic, minor }), bpm: Some(bpm), energy: Some(energy) }
    }

    #[test]
    fn a_neighbour_fits_by_key_tempo_and_energy() {
        // A minor (8A) into E minor (9A): neighbours on the wheel, +1.2 %.
        let (detail, v) = pair(f(9, true, 124.0, 0.5), f(4, true, 125.5, 0.5));
        assert_eq!(v, "good", "{detail}");
        assert!(detail.contains("8A→9A ✓") && detail.contains("same energy"), "{detail}");
        // A clash and 10 % apart: far.
        let (_, v) = pair(f(9, true, 124.0, 0.3), f(1, false, 137.0, 0.6));
        assert_eq!(v, "far");
        // Double time counts as the same tempo.
        let (detail, v) = pair(f(9, true, 87.0, 0.5), f(9, true, 174.0, 0.7));
        assert_eq!(v, "good", "{detail}");
        assert!(detail.contains("half/double") && detail.contains("energy up"), "{detail}");
        // Nothing known: no verdict either way.
        assert_eq!(pair(Facts::default(), Facts::default()).1, "ok");
    }
}

