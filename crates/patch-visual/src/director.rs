//! The director: which effect plays, on what text, and how fast, from the
//! music. A new track is revealed; energy rising lifts, a breakdown calms,
//! a steady groove keeps moving; new effects start on a bar line, and the
//! recent ones rest.

use crate::pack::{Pack, Palette};
use crate::stage::{Act, Backdrop};

/// What is playing, as the visualiser sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Music {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Loudness 0…1 (a meter's position).
    pub level: f32,
    /// Beats since the grid's first beat (negative: no grid).
    pub beat: f64,
    pub bpm: f64,
    /// A transition is running.
    pub transition: bool,
    /// The AI DJ's transition, when one is planned.
    pub handover: Option<Handover>,
    /// The spectrum, low to high, 0…1 per band.
    pub bands: Vec<f32>,
    /// A kick is there: the track's grid has no beatless stretch here (a
    /// breakdown, an intro without drums).
    pub kicking: bool,
    /// How laid back the track is from its sound, 0…1: a song rather than
    /// a club track ([`crate::pack::laid_back`]); its tempo counts too, in
    /// [`Music::laid_back`].
    pub calm: f32,
}

impl Music {
    /// How laid back the show is for this track: its sound or its tempo.
    pub fn laid_back(&self) -> f32 {
        self.calm.max(crate::pack::calmness(self.bpm))
    }
}

/// A planned transition, on the outgoing track's beat clock.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Handover {
    /// The outgoing track (the loudest deck can turn to the incoming one
    /// before the transition ends).
    pub from: String,
    /// The incoming track.
    pub title: String,
    pub artist: String,
    /// Beats since the transition began (negative: still to come), and
    /// its length.
    pub beats_in: f64,
    pub beats: f64,
    pub bpm: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    /// A new track: visual arrival, then a delayed first announcement.
    Reveal,
    /// Energy rising or high.
    Lift,
    /// A steady groove.
    Groove,
    /// A breakdown: quiet, sparse.
    Calm,
    /// Nothing plays.
    Idle,
    /// A transition runs: visuals build without announcing the next track.
    Handover,
    /// The handover ends: a visual drop, followed later by the first title.
    Drop,
}

impl Mood {
    pub fn name(self) -> &'static str {
        match self {
            Mood::Reveal => "reveal",
            Mood::Lift => "lift",
            Mood::Groove => "groove",
            Mood::Calm => "calm",
            Mood::Idle => "idle",
            Mood::Handover => "handover",
            Mood::Drop => "drop",
        }
    }
}

#[derive(Default)]
pub struct Director {
    preferences: Option<crate::choreography::Preferences>,
    materials: Vec<crate::choreography::Entry>,
    last_material: String,
    /// The materials shown lately, newest last: none comes back until
    /// several others have played.
    recent: std::collections::VecDeque<String>,
    /// The shader playing with it, so a title can play over the same picture.
    last_shader: Option<(Option<String>, crate::generative::Mode)>,
    /// This cue continues the picture playing (a title over it, or the
    /// arrangement's word slot): no new material, no wipe.
    keep_picture: bool,
    shaders_available: bool,
    ambient: bool,
    intent: Option<patch_core::visual_intent::Intent>,
    title: String,
    /// Loudness, followed slowly (the section) and fast (right now).
    slow: f32,
    fast: f32,
    /// How laid back the track heard is ([`Music::laid_back`]).
    laid: f32,
    seed: u64,
    /// When the scene playing started, in beats (or seconds without a grid).
    started: f64,
    pub mood: Option<Mood>,
    pending: Option<Mood>,
    /// The incoming title a handover has played for, and when (seconds)
    /// its drop is due.
    handed: String,
    drop_at: Option<(f64, String, String)>,
    /// First announcement waits until the mix ends plus a varied delay.
    announcement: Option<(f64, String, String)>,
    revealed: String,
    /// Text passages always have at least four visual-only bars between.
    text_clear_at: f64,
    backdrop: Option<Backdrop>,
    overlay: Option<Backdrop>,
    act: Option<Act>,
    /// The handover's passage on the stage, and the outgoing track's look
    /// and choreography (the incoming one's become the song's).
    handover_step: usize,
    outgoing: Option<(Pack, Recipe)>,
    /// The playing song's look, and the song it is for.
    pack: Option<Pack>,
    pack_for: String,
    /// The playing song's choreography, and how far its groove and lift
    /// scenes have come in each independently arranged movement.
    recipe: Option<Recipe>,
    groove_n: u32,
    lift_n: u32,
    calm_n: u32,
    motion: Motion,
    selected_scene: Option<(Mood, usize)>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SceneChoice {
    pub part: &'static str,
    pub index: usize,
    pub name: String,
    pub bars: f64,
}

/// A track's choreography: a variable cast arranged into phrases, with
/// recurring motifs, builds and text interludes. Worked out
/// from the title: the same song always gets the same show.
#[derive(Clone, Debug, PartialEq)]
pub struct Recipe {
    pub groove: Vec<Scene>,
    pub lift: Vec<Scene>,
    pub calm: Vec<Scene>,
    /// Signature reveal, drop and handover acts. Arranged interludes
    /// can use additional acts from the same track palette.
    pub title_act: Act,
    pub drop_act: Act,
    pub build_act: Act,
    /// One face and pixel style for the song, a beat feel per mood.
    pub font: u8,
    pub style: u8,
    pub beat_groove: crate::beat::Beat,
    pub beat_lift: crate::beat::Beat,
    pub beat_calm: crate::beat::Beat,
}

/// One composed phrase in the track's arrangement.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub backdrop: Backdrop,
    pub overlay: Option<Backdrop>,
    pub act: Act,
    pub bars: f64,
    pub size: u8,
    pub place: u8,
}

impl Recipe {
    pub fn for_song(title: &str, pack: &Pack, calm: f32) -> Recipe {
        use crate::beat::Beat;
        let mut seed = title.bytes().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        }) ^ 0xc0de;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let slow = calm > 0.5;
        // Dense ones fill the screen; the song's own first (its pack), then
        // any that suit it.
        let pick = |pool: Vec<Backdrop>,
                    n: usize,
                    avoid: &[Backdrop],
                    next: &mut dyn FnMut() -> usize|
         -> Vec<Backdrop> {
            let mut pool: Vec<Backdrop> = pool.into_iter().filter(|b| !avoid.contains(b)).collect();
            let mut out = vec![];
            while out.len() < n && !pool.is_empty() {
                out.push(pool.remove(next() % pool.len()));
            }
            out
        };
        let dense_of = |calm: bool| -> Vec<Backdrop> {
            let own: Vec<Backdrop> = pack
                .backdrops
                .iter()
                .copied()
                .filter(|b| crate::stage::dense(*b) && (!calm || b.calm()))
                .collect();
            if own.len() >= 2 {
                own
            } else {
                Backdrop::ALL
                    .iter()
                    .copied()
                    .filter(|b| {
                        crate::stage::dense(*b)
                            && (!calm
                                || b.calm()
                                || matches!(
                                    b,
                                    Backdrop::Plasma | Backdrop::Kaleido | Backdrop::Ripple
                                ))
                    })
                    .collect()
            }
        };
        let sparse: Vec<Backdrop> = Backdrop::ALL
            .iter()
            .copied()
            .filter(|b| !crate::stage::dense(*b))
            .collect();
        let sparse_calm: Vec<Backdrop> = sparse.iter().copied().filter(|b| b.calm()).collect();
        // Use the pack as a motif, with a track-specific supporting cast.
        // A minimal arrangement and an elaborate one need different lengths.
        let ng = if slow { 2 + next() % 2 } else { 2 + next() % 5 };
        let nl = if slow { 1 + next() % 2 } else { 2 + next() % 5 };
        let nc = 2 + next() % 2;
        let mut pool = dense_of(slow);
        pool.extend(
            Backdrop::ALL
                .iter()
                .copied()
                .filter(|b| crate::stage::dense(*b) && (!slow || b.calm())),
        );
        pool.sort_by_key(|b| b.name());
        pool.dedup();
        let motif = pack
            .backdrops
            .iter()
            .copied()
            .find(|b| crate::stage::dense(*b) && (!slow || b.calm()))
            .unwrap_or(Backdrop::Plasma);
        let mut g = vec![motif];
        g.extend(pick(pool.clone(), ng - 1, &[motif], &mut next));
        let l = pick(pool, nl, &g, &mut next);
        let c = pick(dense_of(true), nc, &[], &mut next);
        let over = |pool: &[Backdrop], next: &mut dyn FnMut() -> usize| -> Option<Backdrop> {
            (!pool.is_empty()).then(|| pool[next() % pool.len()])
        };
        // Words: the song's own acts; a big one for the drop when it has one.
        let acts: Vec<Act> = pack
            .acts
            .iter()
            .copied()
            .filter(|a| *a != Act::Visual)
            .collect();
        let acts = if acts.is_empty() {
            vec![Act::Wave]
        } else {
            acts
        };
        let title_act = acts[next() % acts.len()];
        let big: Vec<Act> = acts
            .iter()
            .copied()
            .filter(|a| a.big() && *a != title_act)
            .collect();
        let drop_act = if big.is_empty() {
            title_act
        } else {
            big[next() % big.len()]
        };
        let building = [
            Act::Assemble,
            Act::Type,
            Act::Pixelate,
            Act::Stack,
            Act::Scan,
        ];
        let builds: Vec<Act> = acts
            .iter()
            .copied()
            .filter(|a| building.contains(a))
            .collect();
        let build_act = if builds.is_empty() {
            building[next() % building.len()]
        } else {
            builds[next() % builds.len()]
        };
        let make = |cast: &[Backdrop], calm: bool, lift: bool, next: &mut dyn FnMut() -> usize| {
            let mut scenes = vec![];
            for (i, &backdrop) in cast.iter().enumerate() {
                let overlay = if i == 0 || next().is_multiple_of(4) {
                    None
                } else {
                    over(if calm { &sparse_calm } else { &sparse }, next)
                };
                // Long enough to settle into: a scene is a passage of the
                // track, not a flash.
                let bars = if calm {
                    [16.0, 24.0, 32.0][next() % 3]
                } else if lift {
                    [8.0, 16.0, 16.0][next() % 3]
                } else {
                    [16.0, 24.0, 32.0][next() % 3]
                };
                scenes.push(Scene {
                    backdrop,
                    overlay,
                    act: Act::Visual,
                    bars,
                    size: 1,
                    place: 0,
                });
                // Return to the motif between movements, now with words or
                // a different sparse layer. These are arranged, not rerolled.
                if !calm && (i + 1 == cast.len() || (cast.len() > 4 && i == 2)) {
                    let act = if lift {
                        drop_act
                    } else {
                        acts[next() % acts.len()]
                    };
                    scenes.push(Scene {
                        backdrop: cast[0],
                        overlay: None,
                        act,
                        bars: if lift { 2.0 } else { 4.0 },
                        size: if lift { 2 } else { 1 },
                        place: if lift { 0 } else { (next() % 3) as u8 },
                    });
                }
            }
            scenes
        };
        let groove = make(&g, false, false, &mut next);
        let lift = make(&l, false, true, &mut next);
        let calm = make(&c, true, false, &mut next);
        let font = pack.fonts[next() % 3];
        // Hollow, dotted or slanted for the whole song, or plain.
        let style = if (next() % 1000) as f32 / 1000.0 < pack.fancy {
            1 + (next() % 3) as u8
        } else {
            0
        };
        let feel =
            |calm: bool, strobe: bool, avoid: &[Beat], next: &mut dyn FnMut() -> usize| -> Beat {
                let pool: Vec<Beat> = Beat::ALL
                    .iter()
                    .copied()
                    .filter(|b| {
                        (!calm || b.calm()) && (strobe || *b != Beat::Strobe) && !avoid.contains(b)
                    })
                    .collect();
                pool.get(next() % pool.len().max(1))
                    .copied()
                    .unwrap_or(Beat::Breathe)
            };
        let beat_groove = feel(slow, false, &[], &mut next);
        let beat_lift = feel(slow, !slow, &[beat_groove], &mut next);
        let beat_calm = feel(true, false, &[], &mut next);
        Recipe {
            groove,
            lift,
            calm,
            title_act,
            drop_act,
            build_act,
            font,
            style,
            beat_groove,
            beat_lift,
            beat_calm,
        }
    }
}

/// What to play next.
pub struct Cue {
    pub material_id: String,
    pub shader: Option<String>,
    pub mode: crate::generative::Mode,
    pub enabled: bool,
    pub transition: Option<u64>,
    /// Stable identity of the track whose world leads this passage.
    pub world_seed: u64,
    pub mood: Mood,
    pub title: String,
    pub small: String,
    pub seed: u64,
    /// Play it in exactly this long (whole bars, or a handover's rest).
    pub fit_s: Option<f64>,
    /// What moves behind the title, and how the title plays; a visual
    /// scene lays a second backdrop over the first.
    pub backdrop: Backdrop,
    pub overlay: Option<Backdrop>,
    pub act: Act,
    /// The song's look: colours, how hard the kick hits, the glow, and
    /// the ttfx easing its motion follows.
    pub palette: Palette,
    pub punch: f32,
    pub glow: f32,
    pub ease: ttfx::utils::easing::Easing,
    /// The title's size (0 small, 1 normal, 2 biggest) and place (0
    /// centre, 1 upper, 2 lower third); the backdrop's brightness.
    pub size: u8,
    pub place: u8,
    pub bright: f32,
    /// The type (0 block letters, n a system face) and pixel style.
    pub font: u8,
    pub style: u8,
    /// How the scene answers the beat.
    pub beat: crate::beat::Beat,
}

/// Continuous animation clocks and a track-relative passage detector.
#[derive(Default)]
pub struct Motion {
    pub seconds: f64,
    pub beats: f64,
    rate: f64,
    title: String,
    bass_reference: f32,
    level_reference: f32,
    bass: f32,
    previous_bass: f32,
    onset_age: f64,
    quiet_for: f64,
    calm: bool,
}

impl Motion {
    pub fn advance(&mut self, m: &Music, dt: f64) {
        let n = (m.bands.len() / 5).max(1).min(m.bands.len());
        let bass = if n > 0 {
            m.bands[..n].iter().sum::<f32>() / n as f32
        } else {
            m.level
        };
        if self.title != m.title {
            self.title = m.title.clone();
            self.bass_reference = bass;
            self.level_reference = m.level;
            self.bass = bass;
            self.previous_bass = bass;
            self.onset_age = 0.0;
            self.quiet_for = 0.0;
            self.calm = false;
        }
        self.onset_age += dt;
        // Small, repeated low-band attacks count too: a softer kick must
        // override an unreliable grid gap or a quieter new section.
        if n > 0 && bass > self.bass + 0.035 && bass > self.previous_bass + 0.008 {
            self.onset_age = 0.0;
        }
        self.previous_bass = bass;
        self.bass += (bass - self.bass) * (1.0 - (-dt / 0.25).exp()) as f32;
        let silent = !m.playing || m.level <= 0.01;
        // A soft kick is still a kick. Require a substantial loss relative
        // to this track, not an absolute bass/loudness threshold. Grid gaps
        // alone are not reliable enough for lighter progressive tracks.
        let bass_lost = self.bass < self.bass_reference - 0.22;
        let level_lost = m.level < self.level_reference * 0.45;
        let missing = if n > 0 {
            bass_lost && (!m.kicking || level_lost)
        } else {
            !m.kicking && level_lost
        };
        let recent_attack = n > 0 && self.onset_age < (120.0 / m.bpm.max(60.0)).max(1.2);
        let candidate = silent || (missing && !recent_attack);
        self.quiet_for = if candidate { self.quiet_for + dt } else { 0.0 };
        self.calm = self.quiet_for >= if silent { 0.8 } else { 1.8 };
        // Learn the track's usual level; hold it through a breakdown so
        // silence cannot become its new reference. Lower levels decay slowly.
        if !candidate {
            let follow = |now: f32, reference: f32| {
                reference
                    + (now - reference)
                        * (1.0 - (-dt / if now > reference { 2.0 } else { 20.0 }).exp()) as f32
            };
            self.bass_reference = follow(self.bass, self.bass_reference);
            self.level_reference = follow(m.level, self.level_reference);
        }
        let target = if self.calm {
            if silent { 0.16 } else { 0.35 }
        } else if candidate {
            0.85
        } else {
            1.0
        };
        let tau = if target < self.rate { 0.8 } else { 0.45 };
        self.rate += (target - self.rate) * (1.0 - (-dt / tau).exp());
        self.seconds += dt * self.rate;
        self.beats += dt * self.rate * if m.bpm > 0.0 { m.bpm / 60.0 } else { 2.0 };
    }

    pub fn calm(&self) -> bool {
        self.calm
    }

    /// Keep the track's beat feel audible at its own normal level.
    pub fn beat_strength(&self, level: f32) -> f32 {
        if self.calm || level <= 0.01 {
            0.0
        } else {
            (level / self.level_reference.max(0.02)).clamp(0.0, 1.0)
        }
    }
}

impl Director {
    pub fn configure(&mut self, preferences: &crate::choreography::Preferences, materials: &[crate::choreography::Entry], shaders_available: bool) {
        if self.preferences.as_ref()!=Some(preferences) || self.materials!=materials {
            self.preferences=Some(preferences.clone());self.materials=materials.to_vec();
            self.recipe=None;self.outgoing=None;self.handed.clear();
        }
        self.shaders_available=shaders_available;
    }

    fn configure_cue(&mut self,cue:&mut Cue,m:&Music,now:f64,theme:Palette) {
        use crate::choreography::{Material,PaletteMode};
        // Only a picture still among the chosen materials carries on.
        if self.keep_picture && !self.materials.is_empty() && !self.materials.iter().any(|e|e.id==self.last_material) {self.keep_picture=false;}
        if self.keep_picture && self.last_shader.is_none() && self.preferences.as_ref().is_some_and(|p|p.automatic) {self.keep_picture=false;}
        if self.keep_picture {cue.transition=None;}
        let Some(p)=&self.preferences else {return;};
        if self.keep_picture && let Some((shader,mode))=self.last_shader.clone() {
            cue.material_id=self.last_material.clone();cue.shader=shader;cue.mode=mode;
        } else if p.automatic {
            let mut pool=self.materials.clone();
            if p.order==2 && let Some(r)=&self.recipe {
                pool.retain(|e| match e.material {Material::Grid(b)=>r.groove.iter().chain(&r.lift).chain(&r.calm).any(|s|s.backdrop==b),Material::Shader(_)=>true,_=>false});
            }
            let fresh:Vec<_>=pool.iter().filter(|e|!self.recent.contains(&e.id)).cloned().collect();
            let pick=p.select(&fresh,cue.mood,cue.seed,Some(&self.last_material),self.shaders_available).or_else(||p.select(&pool,cue.mood,cue.seed,Some(&self.last_material),self.shaders_available));
            if let Some(e)=pick {
                cue.material_id=e.id.clone();self.last_material=e.id.clone();
                // Remember about half of what may play here (at most eight).
                let room=(pool.iter().filter(|x|p.allowed(x,cue.mood)>0.0).count()/2).clamp(1,8);
                self.recent.retain(|id|*id!=e.id);self.recent.push_back(e.id.clone());
                while self.recent.len()>room {self.recent.pop_front();}
                match &e.material {
                    Material::Grid(b)=>{cue.backdrop = *b;if b.ambient(){cue.overlay=None;cue.bright=1.0;}},
                    Material::Shader(name)=>{cue.shader=Some(name.clone());cue.mode=p.shader_mode;},
                    Material::Effect(_)=>{}
                }
            } else {cue.enabled=false;}
            self.last_shader=Some((cue.shader.clone(),cue.mode));
        }
        // An overlay is a second backdrop: it must come from the chosen sources.
        if !p.overlays || cue.overlay.is_some_and(|b|self.materials.iter().find(|e|matches!(e.material,Material::Grid(kind) if kind==b)).is_none_or(|e|p.allowed(e,cue.mood)<=0.0)) {cue.overlay=None;}
        let titles=self.materials.iter().any(|e|matches!(e.material,Material::Effect(_)));
        if cue.act!=Act::Visual {cue.act=if titles {p.effect(cue.act,cue.mood,cue.seed)} else {Act::Visual};}
        // The real handover timing is never replaced by a visual duration.
        if p.custom_lengths && cue.mood!=Mood::Handover {cue.fit_s=Some(bar_length(m,p.bars(cue.mood,cue.seed)));}
        if cue.act!=Act::Visual {self.text_clear_at=now+cue.fit_s.unwrap_or(0.0)+bar_length(m,f64::from(p.title_gap_bars));}
        if let Some(palette)=crate::pack::PALETTES.get(p.palette_index.max(0) as usize).filter(|_|p.palette_index>=0) {cue.palette = *palette;}
        if p.palette==PaletteMode::Theme || p.palette_index==crate::pack::PALETTES.len() as i32 {cue.palette=theme;}
        cue.transition=if self.keep_picture {None} else {p.transition(cue.seed)};
        self.backdrop=Some(cue.backdrop);self.overlay=cue.overlay;
    }

    /// Change the background collection without resetting musical clocks or
    /// the pending title announcement. Rebuild only the scene recipes.
    pub fn set_ambient(&mut self, enabled: bool) {
        if self.ambient != enabled {
            self.ambient = enabled;
            // A new collection is a new picture, title or not.
            self.backdrop = None;
            self.recipe = None;
            self.outgoing = None;
            self.handed.clear();
        }
    }

    fn recipe_for(&self, title: &str, pack: &Pack, calm: f32) -> Recipe {
        let mut r = Recipe::for_song(title, pack, calm);
        if self.ambient {
            let seed = title.bytes().fold(0usize, |h,b| h.wrapping_mul(31).wrapping_add(b as usize));
            for (part, scenes) in [&mut r.groove, &mut r.lift, &mut r.calm].into_iter().enumerate() {
                for (i, scene) in scenes.iter_mut().enumerate() {
                    let cast: &[Backdrop] = if part == 2 || calm > 0.5 {
                        &[Backdrop::Jellyfish, Backdrop::SunsetDrive]
                    } else if part == 1 {
                        &[Backdrop::EventHorizon, Backdrop::SunsetDrive]
                    } else { Backdrop::AMBIENT };
                    scene.backdrop = cast[(seed.wrapping_add(i)) % cast.len()];
                    // These are complete environments, never stacked over
                    // each other as if they were sparse signal effects.
                    scene.overlay = None;
                }
            }
        }
        if let Some(p)=&self.preferences {
            let entries:Vec<_>=self.materials.iter().filter(|e|matches!(e.material,crate::choreography::Material::Grid(_))).cloned().collect();
            let titles=self.materials.iter().any(|e|matches!(e.material,crate::choreography::Material::Effect(_)));
            for (mood,scenes) in [(Mood::Groove,&mut r.groove),(Mood::Lift,&mut r.lift),(Mood::Calm,&mut r.calm)] {
                let mut previous=None;
                for (i,scene) in scenes.iter_mut().enumerate() {
                    if let Some(e)=p.select(&entries,mood,i as u64 ^ self.seed,previous.as_deref(),false) {
                        if let crate::choreography::Material::Grid(b)=e.material {scene.backdrop=b;if b.ambient(){scene.overlay=None;}}
                        previous=Some(e.id.clone());
                    }
                    if p.custom_lengths {scene.bars=p.bars(mood,i as u64 ^ self.seed);}
                    if scene.act!=Act::Visual {scene.act=if titles {p.effect(scene.act,mood,i as u64 ^ self.seed)} else {Act::Visual};}
                }
            }
        }
        r
    }

    /// Named scenes from this song's generated choreography.
    pub fn scene_choices(&self) -> Vec<SceneChoice> {
        let Some(r) = &self.recipe else { return Vec::new() };
        let mut out = vec![SceneChoice { part: "text", index: 0, name: format!("text · {} · {}", r.title_act.name(), r.groove[0].backdrop.name()), bars: 8.0 }];
        for (part, scenes) in [("groove", &r.groove), ("lift", &r.lift), ("calm", &r.calm)] {
            out.extend(scenes.iter().enumerate().map(|(index, s)| SceneChoice {
                part, index, name: format!("{} · {} · {}{}", part, s.act.name(), s.backdrop.name(), s.overlay.map(|b| format!(" + {}", b.name())).unwrap_or_default()), bars: s.bars,
            }));
        }
        out
    }

    /// Play one named recipe scene on the next cue, then carry on normally.
    pub fn select_scene(&mut self, mood: Mood, index: usize) { self.selected_scene = Some((mood, index)); }
    /// Follow the music (every frame, `dt` seconds on).
    pub fn listen(&mut self, m: &Music, dt: f64) {
        let k = |tau: f64| (1.0 - (-dt / tau).exp()) as f32;
        self.motion.advance(m, dt);
        let level = if m.playing { m.level } else { 0.0 };
        self.slow += (level - self.slow) * k(6.0);
        self.fast += (level - self.fast) * k(0.4);
        self.laid = m.laid_back();
    }

    /// The mood the music asks for now.
    pub fn steer(&mut self, intent: Option<patch_core::visual_intent::Intent>) {
        self.intent = intent;
    }

    pub fn mood(&self, m: &Music) -> Mood {
        if !m.playing || m.title.is_empty() {
            return Mood::Idle;
        }
        if m.title != self.title {
            return Mood::Reveal;
        }
        if let Some(intent) = self.intent {
            use patch_core::visual_intent::Section;
            match intent.section {
                Section::Intro | Section::Breakdown | Section::Outro => return Mood::Calm,
                Section::Build | Section::Drop => return Mood::Lift,
                Section::Verse => return Mood::Groove,
                Section::Unknown | Section::Transition => {}
            }
            if intent.intensity > 0.75 { return Mood::Lift; }
            if intent.intensity < 0.25 { return Mood::Calm; }
        }
        let rise = self.fast - self.slow;
        if self.motion.calm() {
            Mood::Calm
        } else if rise > 0.06 || (self.fast > 0.8 && self.laid <= 0.5) {
            // (A laid-back song lifts when it rises, not for being loud.)
            Mood::Lift
        } else {
            Mood::Groove
        }
    }

    fn reveal_delay(&self, title: &str, bpm: f64) -> f64 {
        let mut hash = title.bytes().fold(self.seed ^ 0x72657665616c, |h, b| {
            h.wrapping_mul(1099511628211) ^ b as u64
        });
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccd);
        hash ^= hash >> 33;
        [2.0, 4.0, 6.0, 8.0][hash as usize % 4] * 240.0 / bpm.max(60.0)
    }

    fn outgoing_title<'a>(m: &'a Music, h: &'a Handover) -> &'a str {
        if h.from.is_empty() { &m.title } else { &h.from }
    }

    /// The transition's arrangement, seeded by both titles.
    fn handover_plan(&self, m: &Music, h: &Handover) -> Vec<crate::handover::Passage> {
        let seed = format!("{}\u{1f}{}", Self::outgoing_title(m, h), h.title)
            .bytes()
            .fold(0xcbf29ce484222325u64, |x, b| {
                (x ^ b as u64).wrapping_mul(0x100000001b3)
            });
        crate::handover::plan(h.beats, seed, Self::handover_calm(m, h) > 0.5)
    }

    /// A handover is laid back when its tempo or the track heard is.
    fn handover_calm(m: &Music, h: &Handover) -> f32 {
        crate::pack::calmness(h.bpm).max(m.calm)
    }

    fn mixing(m: &Music) -> bool {
        m.transition
            || m.handover
                .as_ref()
                .is_some_and(|h| h.beats_in >= 0.0 && h.beats_in < h.beats)
    }

    /// Whether to change scene now: the scene ended, a new track came, or
    /// the mood moved and the scene has had its bars; always on a bar line
    /// when there is a grid (a new track at once).
    pub fn due(&mut self, m: &Music, ended: bool, now_s: f64) -> Option<Mood> {
        // The AI DJ's transition leads: its drop, then its handover.
        if let Some((at, ..)) = &self.drop_at
            && now_s >= *at
        {
            return Some(Mood::Drop);
        }
        if let Some(h) = &m.handover
            && h.beats_in >= 0.0
            && h.beats_in < h.beats - 2.0
            && self.title != h.title
        {
            // Each passage of the arrangement on its own bar; between them
            // the passage holds (its scene can end a little early).
            let step = crate::handover::step(&self.handover_plan(m, h), h.beats_in);
            if self.handed != h.title || step > self.handover_step {
                return Some(Mood::Handover);
            }
            if self.drop_at.is_some() {
                return None;
            }
        }
        if self.drop_at.is_some() && !ended {
            return None;
        }
        let clock = if m.beat >= 0.0 { m.beat } else { now_s * 2.0 };
        let played = clock - self.started;
        if let Some((at, title, _)) = &self.announcement {
            if Self::mixing(m) {
                let delay = self.reveal_delay(title, m.bpm);
                if let Some((at, ..)) = self.announcement.as_mut() {
                    *at = (*at).max(now_s + delay);
                }
            } else if m.playing
                && m.title == *title
                && now_s >= *at
                && now_s >= self.text_clear_at
                && (ended || played >= 32.0)
                && (m.beat < 0.0 || m.beat.rem_euclid(4.0) < 0.25)
            {
                return Some(Mood::Reveal);
            }
        }
        let mood = self.mood(m);
        // Breakdowns should not keep a wild scene for another eight bars.
        // Debounce brief gaps, then enter calm immediately; resume on a bar.
        if mood == Mood::Calm && self.mood != Some(Mood::Calm) && self.motion.calm() && (ended || played >= 16.0) {
            return Some(Mood::Calm);
        }
        if self.mood == Some(Mood::Calm)
            && matches!(mood, Mood::Groove | Mood::Lift)
            && !self.motion.calm()
            && (m.beat < 0.0 || m.beat.rem_euclid(4.0) < 0.25)
        {
            return Some(mood);
        }
        let on_bar = m.beat < 0.0 || (m.beat.rem_euclid(4.0) < 0.25);
        // A change of mood waits for a phrase (8 bars) and for the scene to
        // have had 8 bars: the choreography holds, the music still leads.
        let on_phrase = m.beat < 0.0 || (m.beat.rem_euclid(32.0) < 0.25);
        if mood == Mood::Reveal {
            return Some(mood);
        }
        if self.mood != Some(mood) && self.pending != Some(mood) {
            self.pending = Some(mood);
        }
        let moved =
            self.pending.is_some_and(|p| Some(p) != self.mood) && played >= 32.0 && on_phrase;
        ((ended && on_bar) || moved).then_some(mood)
    }

    /// Pick the next scene for `mood` (and remember it).
    pub fn cue(&mut self, m: &Music, mood: Mood, now_s: f64, theme: Palette) -> Cue {
        self.seed = self
            .seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.mood = Some(mood);
        self.pending = None;
        let clock = if m.beat >= 0.0 { m.beat } else { now_s * 2.0 };
        // A scene only just begun is not cut for the mix's first passage.
        let fresh_scene = clock - self.started < 16.0;
        let mut opens_mix = false;
        self.started = clock;
        if mood == Mood::Reveal {
            self.title = m.title.clone();
        }
        let mut fit_s = None;
        let (title, small) = match mood {
            Mood::Idle => ("PATCH".to_string(), String::new()),
            Mood::Handover => {
                let h = m.handover.clone().unwrap_or_default();
                // The drop at the transition's end; this passage to its own.
                let plan = self.handover_plan(m, &h);
                let step = crate::handover::step(&plan, h.beats_in);
                let beat_s = 60.0 / h.bpm.max(60.0);
                let secs = (h.beats - h.beats_in).max(1.0) * beat_s;
                let end = plan.get(step).map_or(h.beats, |s| (s.start + s.bars) as f64 * 4.0);
                fit_s = Some(((end.min(h.beats) - h.beats_in).max(1.0) * beat_s).min(secs));
                self.drop_at = Some((now_s + secs, h.title.clone(), h.artist.clone()));
                opens_mix = self.handed != h.title && step == 0;
                if self.handed != h.title {
                    // The outgoing track's look and what it shows now stay
                    // on the stage beside the incoming one's.
                    let from = Self::outgoing_title(m, &h);
                    let out_pack = self
                        .pack
                        .clone()
                        .filter(|_| self.pack_for == from)
                        .unwrap_or_else(|| crate::pack::for_song(from, None, theme, Self::handover_calm(m, &h)));
                    let out_recipe = self
                        .recipe
                        .clone()
                        .filter(|_| self.pack_for == from)
                        .unwrap_or_else(|| self.recipe_for(from, &out_pack, Self::handover_calm(m, &h)));
                    self.outgoing = Some((out_pack, out_recipe));
                }
                self.handed = h.title.clone();
                self.handover_step = step;
                (h.title, h.artist)
            }
            Mood::Drop => {
                let (_, t, a) = self
                    .drop_at
                    .take()
                    .unwrap_or_else(|| (0.0, m.title.clone(), m.artist.clone()));
                // Recognise the new track without announcing it yet.
                self.title = t.clone();
                (t, a)
            }
            Mood::Reveal => {
                let artist = if m.artist.is_empty() {
                    self.announcement
                        .as_ref()
                        .filter(|(_, title, _)| *title == m.title)
                        .map(|(_, _, artist)| artist.clone())
                        .unwrap_or_default()
                } else {
                    m.artist.clone()
                };
                (m.title.clone(), artist)
            }
            _ => (m.title.clone(), m.artist.clone()),
        };
        // The song's look: new for each song, never the last one's; and its
        // choreography.
        if self.pack.is_none() || self.pack_for != title {
            self.pack = Some(crate::pack::for_song(
                &title,
                self.pack.as_ref(),
                theme,
                m.laid_back(),
            ));
            self.pack_for = title.clone();
            self.recipe = None;
        }
        let pack = self
            .pack
            .clone()
            .unwrap_or_else(|| crate::pack::for_song(&title, None, theme, m.laid_back()));
        if self.recipe.is_none() {
            self.recipe = Some(self.recipe_for(&title, &pack, m.laid_back()));
            self.groove_n = 0;
            self.lift_n = 0;
            self.calm_n = 0;
        }
        let r = self
            .recipe
            .clone()
            .unwrap_or_else(|| self.recipe_for(&title, &pack, m.laid_back()));
        use crate::beat::Beat;
        let mut composition = None;
        let mut text_slot = false;
        let (mut act, backdrop, overlay, bars, beat): (Act, Backdrop, Option<Backdrop>, f64, Beat) =
            match mood {
                Mood::Reveal => (r.title_act, r.groove[0].backdrop, None, 8.0, r.beat_groove),
                Mood::Drop => (
                    Act::Visual,
                    r.lift[0].backdrop,
                    r.lift[0].overlay,
                    8.0,
                    Beat::Lift,
                ),
                // Arranged below, from both tracks' worlds.
                Mood::Handover => (Act::Visual, r.groove[0].backdrop, None, 0.0, r.beat_groove),
                Mood::Calm | Mood::Idle | Mood::Lift | Mood::Groove => {
                    let (scenes, index, feel) = match mood {
                        Mood::Calm | Mood::Idle => (&r.calm, &mut self.calm_n, r.beat_calm),
                        Mood::Lift => (&r.lift, &mut self.lift_n, r.beat_lift),
                        _ => (&r.groove, &mut self.groove_n, r.beat_groove),
                    };
                    let selected = self.selected_scene.take().filter(|(selected_mood, i)| *selected_mood == mood && *i < scenes.len()).map(|(_, i)| i);
                    let scene_index = selected.unwrap_or(*index as usize % scenes.len());
                    let scene = &scenes[scene_index];
                    *index = scene_index as u32 + 1;
                    composition = Some((scene.size, scene.place));
                    text_slot = scene.act != Act::Visual;
                    (scene.act, scene.backdrop, scene.overlay, scene.bars, feel)
                }
            };
        if fit_s.is_none() {
            fit_s = Some(bar_length(m, bars.max(1.0)));
        }
        let hash_world = |s: &str| s.bytes().fold(0xcbf29ce484222325u64, |h,b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        let mut world_seed = hash_world(&title);
        let mut lead = pack.clone();
        let (mut backdrop, mut overlay, mut beat) = (backdrop, overlay, beat);
        if mood == Mood::Handover
            && let Some(h) = &m.handover
            && let Some((out_pack, out)) = self.outgoing.clone()
        {
            use crate::handover::Side;
            let plan = self.handover_plan(m, h);
            let passage = plan[self.handover_step.min(plan.len() - 1)];
            world_seed = hash_world(if passage.side.incoming() { &h.title } else { Self::outgoing_title(m,h) });
            // Each track's own designs: its groove scenes, then its lifts;
            // the closing quick turns from the lifts.
            let designs = |r: &Recipe, finale: bool| -> Vec<(Backdrop, Option<Backdrop>)> {
                let mut v: Vec<(Backdrop, Option<Backdrop>)> = vec![];
                let scenes = if finale {
                    r.lift.iter().chain(&r.groove).collect::<Vec<_>>()
                } else {
                    r.groove.iter().chain(&r.lift).collect()
                };
                for s in scenes {
                    if !v.iter().any(|(b, o)| (*b, *o) == (s.backdrop, s.overlay)) {
                        v.push((s.backdrop, s.overlay));
                    }
                }
                v
            };
            let (own, own_pack, other, other_pack, own_recipe) = if passage.side.incoming() {
                (designs(&r, passage.finale), &pack, designs(&out, passage.finale), &out_pack, &r)
            } else {
                (designs(&out, passage.finale), &out_pack, designs(&r, passage.finale), &pack, &out)
            };
            // Its next design, never the one just shown.
            let mut k = passage.turn;
            let (b, o) = loop {
                let d = own[k % own.len()];
                if Some(d.0) != self.backdrop || k >= passage.turn + own.len() {
                    break d;
                }
                k += 1;
            };
            backdrop = b;
            overlay = o;
            // A mashup: this backdrop wears the other track's layer.
            if matches!(passage.side, Side::OutWithIn | Side::InWithOut) {
                overlay = other
                    .iter()
                    .filter_map(|(_, o)| *o)
                    .chain(
                        other_pack
                            .backdrops
                            .iter()
                            .copied()
                            .filter(|b| !crate::stage::dense(*b)),
                    )
                    .find(|o| *o != backdrop)
                    .or(overlay);
            }
            beat = if passage.finale {
                if Self::handover_calm(m, h) > 0.5 {
                    own_recipe.beat_lift
                } else {
                    Beat::Lift
                }
            } else {
                own_recipe.beat_groove
            };
            lead = own_pack.clone();
        }
        if mood != Mood::Idle && self.revealed != title {
            if self
                .announcement
                .as_ref()
                .is_none_or(|(_, pending, _)| *pending != title)
            {
                let end = self
                    .drop_at
                    .as_ref()
                    .filter(|(_, pending, _)| *pending == title)
                    .map(|(at, ..)| *at)
                    .unwrap_or(now_s);
                self.announcement = Some((
                    end + self.reveal_delay(&title, m.bpm),
                    title.clone(),
                    small.clone(),
                ));
            }
            if Self::mixing(m) {
                let delay = self.reveal_delay(&title, m.bpm);
                if let Some((at, ..)) = self.announcement.as_mut() {
                    *at = (*at).max(now_s + delay);
                }
            }
            let ready = self
                .announcement
                .as_ref()
                .is_some_and(|(at, pending, _)| *pending == title && now_s >= *at);
            if mood == Mood::Reveal && ready && !Self::mixing(m) && now_s >= self.text_clear_at {
                self.revealed = title.clone();
                self.announcement = None;
            } else {
                // This guard also covers manual scene skips, startup on the
                // incoming deck, and a text interlude before the first reveal.
                act = Act::Visual;
            }
        }
        if Self::mixing(m) || now_s < self.text_clear_at {
            act = Act::Visual;
        }
        let artist_only = act != Act::Visual
            && mood != Mood::Reveal
            && !small.trim().is_empty()
            && ((self.seed >> 48) & 0xffff) < 22937;
        let display_title = if artist_only {
            small.clone()
        } else {
            title.clone()
        };
        if act != Act::Visual {
            self.text_clear_at = now_s + fit_s.unwrap_or(0.0) + 960.0 / m.bpm.max(60.0);
        }
        let pure = act == Act::Visual;
        // Words play over the picture already showing: a title (or the
        // arrangement's word slot) is not a reason to change the scene.
        self.keep_picture = self.backdrop.is_some()
            && match mood {
                Mood::Groove | Mood::Lift | Mood::Calm => text_slot,
                Mood::Reveal => act != Act::Visual,
                Mood::Handover => opens_mix && fresh_scene,
                _ => false,
            };
        if self.keep_picture {
            backdrop = self.backdrop.unwrap_or(backdrop);
            overlay = self.overlay;
        }
        self.backdrop = Some(backdrop);
        self.overlay = overlay;
        self.act = Some(act);
        // The title big as the song arrives and drops, normal when it
        // returns; centred. Bright when the visual is the show, dim behind
        // words (readable).
        let default_size = match mood {
            Mood::Reveal | Mood::Drop => 2,
            _ => 1,
        };
        let (size, place) = composition.unwrap_or((default_size, 0));
        let bright = if backdrop.ambient() { 1.0 } else if pure { 1.7 } else { 0.7 };
        if self.ambient { overlay = None; }
        let (font, style) = (r.font, r.style);
        // Every act receives exactly one identity. Artists use the main
        // lettering in their own later scene, never a subtitle or a second
        // word sequence inside the title act.
        let small = String::new();
        let mut cue = Cue {
            material_id:format!("cells:{}",backdrop.name()),shader:None,mode:crate::generative::Mode::Choreography,enabled:true,transition:Some(self.seed%3),
            world_seed,
            mood,
            title: display_title,
            small,
            seed: self.seed,
            fit_s,
            backdrop,
            overlay,
            act,
            palette: lead.palette,
            punch: lead.punch,
            glow: lead.glow,
            ease: if matches!(mood, Mood::Calm | Mood::Idle) {
                crate::Easing::InOutSine
            } else {
                lead.ease
            },
            size,
            place,
            bright,
            font,
            style,
            beat,
        };
        self.configure_cue(&mut cue,m,now_s,theme);
        cue
    }

    pub fn motion(&self) -> &Motion {
        &self.motion
    }

    /// Loudness right now, followed (0…1).
    pub fn energy(&self) -> f32 {
        self.fast
    }

    /// What the director is playing, as semantic intent: for the generative
    /// scene in hybrid mode (and anyone reading it). It reads the director;
    /// it never changes what the director plays.
    pub fn intent(&self) -> patch_core::visual_intent::Intent {
        use patch_core::visual_intent::{Colour, Focus, Intent, Section};
        let mood = self.mood.unwrap_or(Mood::Idle);
        let (section, motion, symmetry, density, feedback) = match mood {
            Mood::Idle => (Section::Unknown, 0.1, 0.5, 0.2, 0.6),
            Mood::Reveal => (Section::Intro, 0.4, 0.7, 0.4, 0.4),
            Mood::Calm => (Section::Breakdown, 0.2, 0.6, 0.25, 0.7),
            Mood::Groove => (Section::Verse, 0.5, 0.5, 0.5, 0.35),
            Mood::Lift => (Section::Build, 0.75, 0.6, 0.7, 0.3),
            Mood::Handover => (Section::Transition, 0.6, 0.4, 0.6, 0.6),
            Mood::Drop => (Section::Drop, 0.95, 0.9, 0.9, 0.2),
        };
        // A laid-back song moves less and never flashes.
        let soft = 1.0 - 0.5 * self.laid;
        Intent {
            section,
            intensity: self.fast.clamp(0.0, 1.0) * soft,
            motion: motion * soft,
            symmetry,
            density,
            feedback,
            colour: match mood {
                Mood::Drop | Mood::Lift if self.laid <= 0.5 => Colour::Flash,
                Mood::Handover => Colour::Drift,
                _ => Colour::Theme,
            },
            hue: None,
            transition_beats: if mood == Mood::Drop { 0.0 } else { 4.0 },
            focus: if self.motion.calm() || self.laid > 0.5 { Focus::Melody } else { Focus::Kick },
        }
    }
}

/// Seconds from now to the bar line `bars` bars on (at least half a bar
/// away); without a grid, as at 120 BPM.
fn bar_length(m: &Music, bars: f64) -> f64 {
    if m.beat < 0.0 || m.bpm <= 0.0 {
        return bars * 2.0;
    }
    let mut target = ((m.beat / 4.0).floor() + bars) * 4.0;
    if target - m.beat < 2.0 {
        target += 4.0;
    }
    (target - m.beat) * 60.0 / m.bpm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_track_keeps_its_recipe_beat_strength() {
        let mut motion = Motion::default();
        let m = music("Soft kicks", 0.06, 0.0);
        for _ in 0..120 {
            motion.advance(&m, 1.0 / 60.0);
        }
        assert!(motion.beat_strength(m.level) > 0.9);
        let mut silent = m.clone();
        silent.level = 0.0;
        for _ in 0..120 {
            motion.advance(&silent, 1.0 / 60.0);
        }
        assert_eq!(motion.beat_strength(silent.level), 0.0);
    }

    fn music(title: &str, level: f32, beat: f64) -> Music {
        Music {
            title: title.into(),
            artist: "A".into(),
            playing: true,
            level,
            beat,
            bpm: 126.0,
            transition: false,
            handover: None,
            bands: vec![],
            kicking: true,
            calm: 0.0,
        }
    }

    #[test]
    fn a_song_at_club_tempo_gets_a_laid_back_show() {
        // A loud pop song at 132 BPM: no lift for being loud, no flash,
        // calm backdrops and gentle beat feels only.
        let mut m = music("You Never Ask Me Why", 0.9, 64.0);
        m.bpm = 132.0;
        m.calm = crate::pack::laid_back(0.0, Some(0.78), Some((0.38, 3.6)));
        let mut d = Director { title: m.title.clone(), ..Default::default() };
        // A minute in: the slow loudness has caught up (no rise).
        for _ in 0..3600 {
            d.listen(&m, 1.0 / 60.0);
        }
        assert_eq!(d.mood(&m), Mood::Groove);
        let i = d.intent();
        assert_ne!(i.colour, patch_core::visual_intent::Colour::Flash);
        assert!(i.intensity <= 0.5);
        let pack = crate::pack::for_song(&m.title, None, crate::pack::PALETTES[0], m.laid_back());
        let r = Recipe::for_song(&m.title, &pack, m.laid_back());
        assert!(pack.backdrops.iter().all(|b| b.calm()));
        assert!([r.beat_groove, r.beat_lift].iter().all(|b| b.calm()));
        // The same at club tempo without the voice: a club show.
        m.calm = 0.0;
        d.listen(&m, 1.0 / 60.0);
        assert_eq!(d.mood(&m), Mood::Lift);
    }

    #[test]
    fn soft_progressive_kicks_keep_normal_motion_even_with_grid_gaps() {
        for kicking in [true, false] {
            let mut d = Director::default();
            let mut m = music("Progressive", 0.28, 0.0);
            m.bands = vec![0.12; 20];
            m.kicking = kicking;
            d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
            for _ in 0..600 {
                d.listen(&m, 1.0 / 60.0);
            }
            assert_eq!(d.mood(&m), Mood::Groove);
            assert!(d.motion.rate > 0.99);
            // A moderate bass dip also keeps the groove.
            m.bands.fill(0.06);
            for _ in 0..180 {
                d.listen(&m, 1.0 / 60.0);
            }
            assert!(!d.motion.calm());
        }
    }

    #[test]
    fn softer_repeated_attacks_override_a_large_bass_drop_and_grid_gap() {
        let mut motion = Motion::default();
        let mut m = music("Soft kicks", 0.7, 0.0);
        m.bands = vec![0.7; 20];
        for _ in 0..600 {
            motion.advance(&m, 1.0 / 60.0);
        }
        m.kicking = false;
        for frame in 0..600 {
            m.bands[..4].fill(if frame % 30 < 5 { 0.20 } else { 0.12 });
            motion.advance(&m, 1.0 / 60.0);
        }
        assert!(!motion.calm());
        assert!(motion.rate > 0.99, "soft attacks retain the groove");
    }

    #[test]
    fn sustained_bass_loss_calms_but_short_gaps_do_not() {
        let mut d = Director {
            revealed: "Breakdown".into(),
            ..Director::default()
        };
        let mut m = music("Breakdown", 0.9, 3.0);
        m.bands = vec![0.7; 20];
        d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        for _ in 0..600 {
            d.listen(&m, 1.0 / 60.0);
        }
        d.cue(&m, Mood::Lift, 10.0, crate::pack::PALETTES[0]);
        m.kicking = false;
        m.bands[..4].fill(0.0);
        for _ in 0..60 {
            d.listen(&m, 1.0 / 60.0);
        }
        assert_eq!(d.due(&m, false, 11.0), None);
        assert!(d.motion.rate > 0.85);
        for _ in 0..180 {
            d.listen(&m, 1.0 / 60.0);
        }
        // Not before the scene has had four bars, then at once.
        assert_eq!(d.due(&m, false, 14.0), None);
        m.beat = 19.5;
        assert_eq!(d.due(&m, false, 14.0), Some(Mood::Calm));
        let a = d.cue(&m, Mood::Calm, 14.0, crate::pack::PALETTES[0]);
        let b = d.cue(&m, Mood::Calm, 15.0, crate::pack::PALETTES[0]);
        assert_ne!((a.backdrop, a.overlay), (b.backdrop, b.overlay));
        assert!(a.backdrop.calm() && b.backdrop.calm());
        m.kicking = true;
        m.bands.fill(0.7);
        m.beat = 4.0;
        for _ in 0..60 {
            d.listen(&m, 1.0 / 60.0);
        }
        assert_eq!(d.due(&m, false, 15.1), Some(Mood::Lift));
    }

    #[test]
    fn silence_slows_and_recovers_without_clock_jumps() {
        let mut motion = Motion::default();
        let mut m = music("Motion", 0.9, 0.0);
        for _ in 0..600 {
            motion.advance(&m, 1.0 / 60.0);
        }
        let start = motion.seconds;
        for _ in 0..60 {
            motion.advance(&m, 1.0 / 60.0);
        }
        assert!((motion.seconds - start - 1.0).abs() < 0.01);
        m.level = 0.0;
        for _ in 0..360 {
            motion.advance(&m, 1.0 / 60.0);
        }
        let start = motion.seconds;
        for _ in 0..60 {
            motion.advance(&m, 1.0 / 60.0);
        }
        assert!((motion.seconds - start - 0.16).abs() < 0.01);
        m.level = 0.9;
        let before = motion.beats;
        motion.advance(&m, 1.0 / 60.0);
        assert!(motion.beats > before && motion.beats - before < 0.04);
        assert!(!motion.calm());
    }

    #[test]
    fn title_and_artist_use_separate_passages_with_a_visual_gap() {
        let mut d = Director {
            revealed: "Tell Me Why".into(),
            ..Director::default()
        };
        let m = music("Tell Me Why", 0.8, 0.0);
        let mut titles = 0;
        let mut artists = 0;
        for n in 0..300 {
            let now = n as f64 * 120.0;
            let c = d.cue(&m, Mood::Groove, now, crate::pack::PALETTES[0]);
            assert!(c.small.is_empty(), "no combined title/artist act");
            if c.act != Act::Visual {
                if c.title == m.title {
                    titles += 1;
                } else {
                    assert_eq!(c.title, m.artist);
                    artists += 1;
                }
                let end = now + c.fit_s.unwrap();
                for mood in [Mood::Reveal, Mood::Groove, Mood::Lift] {
                    let next = d.cue(&m, mood, end + 0.1, crate::pack::PALETTES[0]);
                    assert_eq!(
                        next.act,
                        Act::Visual,
                        "never consecutive text passages, even when skipped"
                    );
                    assert!(next.small.is_empty());
                }
                assert!(d.text_clear_at >= end + 960.0 / m.bpm);
            }
        }
        assert!(
            titles > 0 && artists > 0,
            "both identities have their own passages: {titles}/{artists}"
        );
        let c = d.cue(&m, Mood::Reveal, 1e6, crate::pack::PALETTES[0]);
        assert_eq!(
            c.title, m.title,
            "the first/reveal act always names the track"
        );
        assert!(c.small.is_empty());
    }

    #[test]
    fn a_new_track_is_revealed_then_the_music_leads() {
        let mut d = Director::default();
        let m = music("One", 0.6, 0.0);
        assert_eq!(d.mood(&m), Mood::Reveal);
        let c = d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        assert!(c.fit_s.is_some());
        for _ in 0..60 * 60 {
            d.listen(&m, 1.0 / 60.0);
        }
        assert_eq!(
            d.mood(&m),
            Mood::Groove,
            "a minute of the same level: steady"
        );
        // Quiet: a breakdown.
        let quiet = music("One", 0.0, 40.0);
        for _ in 0..120 {
            d.listen(&quiet, 1.0 / 60.0);
        }
        assert_eq!(d.mood(&quiet), Mood::Calm);
        // Sustained silence calms immediately, even mid-phrase.
        assert_eq!(
            d.due(&music("One", 0.1, 41.0), false, 0.0),
            Some(Mood::Calm)
        );
        assert_eq!(
            d.due(&music("One", 0.1, 44.0), false, 0.0),
            Some(Mood::Calm)
        );
        assert_eq!(
            d.due(&music("One", 0.1, 64.0), false, 0.0),
            Some(Mood::Calm)
        );
        assert_eq!(d.mood(&music("Two", 0.1, 44.0)), Mood::Reveal);
    }

    #[test]
    fn first_announcement_waits_for_the_transition_then_a_varied_bar_delay() {
        let mut d = Director {
            revealed: "One".into(),
            ..Director::default()
        };
        let mut m = music("One", 0.6, 0.0);
        d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        m.transition = true;
        m.handover = Some(Handover {
            from: "One".into(),
            title: "Two".into(),
            artist: "B".into(),
            beats_in: 4.0,
            beats: 32.0,
            bpm: 126.0,
        });
        assert_eq!(d.due(&m, false, 10.0), Some(Mood::Handover));
        let c = d.cue(&m, Mood::Handover, 10.0, crate::pack::PALETTES[0]);
        assert_eq!(c.act, Act::Visual);
        assert!(c.small.is_empty());
        // The drop at the transition's end; this passage ends before it.
        let end = d.drop_at.as_ref().unwrap().0;
        assert!((end - 10.0 - 28.0 * 60.0 / 126.0).abs() < 1e-6);
        assert!(10.0 + c.fit_s.unwrap() <= end + 1e-6);
        assert!(d.announcement.as_ref().unwrap().0 > end);
        assert_eq!(d.due(&m, false, end), Some(Mood::Drop));
        let c = d.cue(&m, Mood::Drop, end, crate::pack::PALETTES[0]);
        assert_eq!(c.act, Act::Visual);
        m.title = "Two".into();
        m.artist = "B".into();
        m.beat = 0.0;
        m.handover = None;
        // Still mixing after the estimated handover: never reveal early.
        d.due(&m, false, end + 30.0);
        let skipped = d.cue(&m, Mood::Reveal, end + 31.0, crate::pack::PALETTES[0]);
        assert_eq!(skipped.act, Act::Visual);
        assert!(skipped.small.is_empty());
        m.transition = false;
        let at = d.announcement.as_ref().unwrap().0;
        assert_ne!(d.due(&m, false, at - 0.01), Some(Mood::Reveal));
        m.beat = 1.0;
        assert_ne!(d.due(&m, false, at + 0.1), Some(Mood::Reveal));
        m.beat = 4.0;
        // The scene playing has its eight bars before the title comes.
        assert_ne!(d.due(&m, false, at + 1.0), Some(Mood::Reveal));
        m.beat = 32.0;
        assert_eq!(d.due(&m, false, at + 1.0), Some(Mood::Reveal));
        let c = d.cue(&m, Mood::Reveal, at + 1.0, crate::pack::PALETTES[0]);
        assert_ne!(c.act, Act::Visual);
        assert_eq!(c.title, "Two");
        assert!(c.small.is_empty());
        assert!(d.announcement.is_none());
    }

    #[test]
    fn a_long_transition_alternates_both_tracks_designs() {
        let mut d = Director {
            revealed: "Outgoing".into(),
            ..Director::default()
        };
        let mut m = music("Outgoing", 0.7, 0.0);
        d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        let out_palette = d.pack.as_ref().unwrap().palette;
        m.transition = true;
        let beats = 32.0 * 4.0;
        let mut palettes = vec![];
        let mut drop_at = None;
        let mut last = None;
        for b in 0..(beats as usize - 3) {
            let beats_in = b as f64;
            m.handover = Some(Handover {
                from: "Outgoing".into(),
                title: "Incoming".into(),
                artist: "B".into(),
                beats_in,
                beats,
                bpm: 126.0,
            });
            // Halfway, the incoming deck is the louder one.
            if beats_in >= 64.0 {
                m.title = "Incoming".into();
            }
            let now = 100.0 + beats_in * 60.0 / 126.0;
            if d.due(&m, false, now) == Some(Mood::Handover) {
                let c = d.cue(&m, Mood::Handover, now, crate::pack::PALETTES[0]);
                assert_eq!(c.act, Act::Visual);
                assert_ne!(Some(c.backdrop), last, "a new design every passage");
                last = Some(c.backdrop);
                palettes.push(c.palette == out_palette);
                let at = d.drop_at.as_ref().unwrap().0;
                assert!((at - (100.0 + beats * 60.0 / 126.0)).abs() < 1e-6);
                drop_at = Some(at);
            }
        }
        assert!(palettes.len() >= 4, "{palettes:?}");
        assert!(palettes[0], "it starts on the outgoing track");
        assert!(!palettes.last().unwrap(), "and ends on the incoming");
        let turns = palettes.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(turns >= 3, "the tracks take turns: {palettes:?}");
        let at = drop_at.unwrap();
        assert_eq!(d.due(&m, false, at), Some(Mood::Drop));
        let c = d.cue(&m, Mood::Drop, at, crate::pack::PALETTES[0]);
        assert_eq!(c.backdrop, d.recipe.as_ref().unwrap().lift[0].backdrop);
        m.handover.as_mut().unwrap().beats_in = beats - 3.0;
        assert_ne!(d.due(&m, false, at + 0.5), Some(Mood::Handover));
    }

    #[test]
    fn manual_mix_and_scene_skip_restart_the_delay_after_the_mix() {
        let mut d = Director::default();
        let mut m = music("Manual arrival", 0.7, 12.0);
        m.transition = true;
        let c = d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        assert_eq!(c.act, Act::Visual);
        let c = d.cue(&m, Mood::Reveal, 100.0, crate::pack::PALETTES[0]);
        assert_eq!(c.act, Act::Visual);
        let at = d.announcement.as_ref().unwrap().0;
        assert!(at > 100.0);
        m.transition = false;
        assert_ne!(d.due(&m, true, 100.1), Some(Mood::Reveal));
        assert_eq!(d.due(&m, true, at + 1.0), Some(Mood::Reveal));
    }

    #[test]
    fn startup_and_interludes_cannot_bypass_the_first_announcement_delay() {
        let mut d = Director::default();
        let m = music("New track", 0.6, 0.0);
        let c = d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        assert_eq!(c.act, Act::Visual);
        assert!(c.small.is_empty());
        for _ in 0..20 {
            let c = d.cue(&m, Mood::Groove, 0.1, crate::pack::PALETTES[0]);
            assert_eq!(c.act, Act::Visual);
            assert!(c.small.is_empty());
        }
        let mut delays = std::collections::HashSet::new();
        for n in 0..32 {
            delays.insert(d.reveal_delay(&format!("Track {n}"), 120.0) as u32);
        }
        assert!(delays.len() >= 3, "the reveal must vary between tracks");
    }

    #[test]
    fn a_song_keeps_its_arrangement_and_identity() {
        let mut d = Director {
            revealed: "One".into(),
            ..Director::default()
        };
        let m = music("One", 0.6, 0.0);
        let _ = d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0]);
        let moods = [
            Mood::Groove,
            Mood::Groove,
            Mood::Lift,
            Mood::Groove,
            Mood::Calm,
            Mood::Groove,
            Mood::Lift,
            Mood::Groove,
        ];
        let cues: Vec<Cue> = (0..40)
            .map(|i| {
                d.cue(
                    &m,
                    moods[i % moods.len()],
                    i as f64 * 120.0,
                    crate::pack::PALETTES[0],
                )
            })
            .collect();
        let visuals: std::collections::HashSet<_> = cues
            .iter()
            .filter(|c| c.act == Act::Visual)
            .map(|c| format!("{:?}{:?}", c.backdrop, c.overlay))
            .collect();
        let words: std::collections::HashSet<_> = cues
            .iter()
            .filter(|c| c.act != Act::Visual)
            .map(|c| format!("{:?}", c.act))
            .collect();
        assert!(visuals.len() >= 4, "composed visual movements: {visuals:?}");
        assert!(
            !words.is_empty(),
            "the arrangement includes text interludes"
        );
        let text = cues.iter().filter(|c| c.act != Act::Visual).count();
        assert!(
            text * 2 < cues.len(),
            "mostly visuals: {text} of {}",
            cues.len()
        );
        let fonts: std::collections::HashSet<_> = cues
            .iter()
            .filter(|c| c.act != Act::Visual)
            .map(|c| (c.font, c.style))
            .collect();
        assert!(fonts.len() <= 1, "one face and style for the song");
        // The same song, the same show; another song, its own.
        let a = Recipe::for_song(
            "One",
            &crate::pack::for_song("One", None, crate::pack::PALETTES[0], 0.0),
            0.0,
        );
        assert_eq!(
            a,
            Recipe::for_song(
                "One",
                &crate::pack::for_song("One", None, crate::pack::PALETTES[0], 0.0),
                0.0
            )
        );
        let b = Recipe::for_song(
            "Another One",
            &crate::pack::for_song("Another One", None, crate::pack::PALETTES[0], 0.0),
            0.0,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn arrangements_have_variable_casts_phrase_lengths_and_safe_breakdowns() {
        let mut lengths = std::collections::HashSet::new();
        let mut elaborate = false;
        for n in 0..64 {
            let title = format!("Arrangement {n}");
            let pack = crate::pack::for_song(&title, None, crate::pack::PALETTES[0], 0.0);
            let r = Recipe::for_song(&title, &pack, 0.0);
            lengths.insert((r.groove.len(), r.lift.len()));
            elaborate |= r
                .groove
                .iter()
                .chain(&r.lift)
                .filter(|s| s.act == Act::Visual)
                .count()
                > 4;
            for scenes in [&r.groove, &r.lift] {
                let words: f64 = scenes
                    .iter()
                    .filter(|s| s.act != Act::Visual)
                    .map(|s| s.bars)
                    .sum();
                let all: f64 = scenes.iter().map(|s| s.bars).sum();
                assert!(words / all <= 0.25);
                assert!(scenes.iter().all(|s| s.bars >= 2.0 && s.bars % 2.0 == 0.0));
                assert_eq!(
                    scenes.last().unwrap().backdrop,
                    scenes[0].backdrop,
                    "return to the motif"
                );
            }
            assert!(r.calm.iter().all(|s| s.act == Act::Visual
                && s.backdrop.calm()
                && s.overlay.is_none_or(|b| b.calm())));
        }
        assert!(
            lengths.len() > 6,
            "track arrangements must differ in length"
        );
        assert!(
            elaborate,
            "elaborate tracks use more than four visual scenes"
        );
    }

    #[test]
    fn visuals_hold_longer_than_words() {
        let mut d = Director {
            revealed: "One".into(),
            ..Director::default()
        };
        let m = music("One", 0.6, 0.0);
        let bars = |c: &Cue| (c.fit_s.unwrap() * 126.0 / 60.0 / 4.0).round();
        assert_eq!(
            bars(&d.cue(&m, Mood::Reveal, 0.0, crate::pack::PALETTES[0])),
            8.0
        );
        let mut groove = vec![];
        for i in 0..8 {
            let c = d.cue(&m, Mood::Groove, i as f64 * 120.0, crate::pack::PALETTES[0]);
            groove.push((c.act == Act::Visual, bars(&c)));
        }
        assert!(
            groove.iter().all(|(v, b)| if *v {
                [16.0, 24.0, 32.0].contains(b)
            } else {
                *b == 4.0
            }),
            "{groove:?}"
        );
        assert!(
            groove.iter().any(|(v, _)| !v),
            "a text reprise in the arrangement"
        );
        let lift = d.cue(&m, Mood::Lift, 0.0, crate::pack::PALETTES[0]);
        assert_eq!(lift.act, Act::Visual);
        assert!([8.0, 16.0].contains(&bars(&lift)));
        let drop = d.cue(&m, Mood::Drop, 2000.0, crate::pack::PALETTES[0]);
        assert_eq!(bars(&drop), 8.0);
    }
}

#[cfg(test)]
mod steering_tests {
    use super::*;
    #[test]
    fn intent_steers_patterns_but_preserves_arrivals_and_silence() {
        let mut director=Director::default();
        director.steer(Some(patch_core::visual_intent::Intent {section:patch_core::visual_intent::Section::Build,..Default::default()}));
        let mut music=Music {playing:true,title:"Track".into(),..Default::default()};
        assert_eq!(director.mood(&music),Mood::Reveal);
        director.title=music.title.clone();assert_eq!(director.mood(&music),Mood::Lift);
        music.playing=false;assert_eq!(director.mood(&music),Mood::Idle);
    }
}

#[cfg(test)]
mod ambient_collection_tests {
    use super::*;
    #[test]
    fn collection_uses_the_same_arrangement_and_quiet_sections_stay_calm() {
        let palette = crate::pack::PALETTES[0];
        let pack = crate::pack::for_song("Sunset",None,palette,0.0);
        let mut director = Director::default();
        let original = director.recipe_for("Sunset",&pack,0.0);
        director.set_ambient(true);
        let ambient = director.recipe_for("Sunset",&pack,0.0);
        for (a,b) in original.groove.iter().zip(&ambient.groove) {
            assert_eq!(a.bars,b.bars);
            assert_eq!(a.act,b.act);
            assert!(b.backdrop.ambient());
            assert!(b.overlay.is_none());
        }
        assert!(ambient.calm.iter().all(|s| s.backdrop.calm()));
        assert_eq!(original.title_act,ambient.title_act);
        director.set_ambient(false);
        assert!(director.recipe_for("Sunset",&pack,0.0).groove.iter().all(|s| !s.backdrop.ambient()));
    }
}

#[cfg(test)]
mod ambient_handover_tests {
    use super::*;
    #[test]
    fn changing_collection_mid_mix_keeps_drop_and_title_timing() {
        let mut d = Director::default();
        let mut m = Music {title:"Outgoing".into(),artist:"One".into(),playing:true,level:0.7,beat:48.0,bpm:124.0,kicking:true,..Default::default()};
        let palette = crate::pack::PALETTES[0];
        d.cue(&m,Mood::Groove,0.0,palette);
        m.transition = true;
        m.handover = Some(Handover {from:"Outgoing".into(),title:"Incoming".into(),artist:"Two".into(),beats_in:4.0,beats:32.0,bpm:124.0});
        d.cue(&m,Mood::Handover,10.0,palette);
        let drop = d.drop_at.clone();
        let announce = d.announcement.clone();
        d.set_ambient(true);
        let cue = d.cue(&m,Mood::Handover,10.0,palette);
        assert!(cue.backdrop.ambient());
        assert!(cue.overlay.is_none());
        assert_eq!(cue.act,Act::Visual);
        assert!(cue.small.is_empty());
        assert_eq!(d.drop_at,drop);
        assert_eq!(d.announcement,announce);
        assert!(d.outgoing.as_ref().unwrap().1.groove.iter().all(|s| s.backdrop.ambient()));
    }
}

#[cfg(test)]
mod pacing_probe {
    use super::*;
    /// A synthetic DJ set: 6-minute tracks at 124 BPM with a breakdown and
    /// a build, joined by AI handovers of `mix` bars. Prints every cue.
    pub(super) fn simulate(mix: f64, seconds: f64, choreo: bool) -> Vec<(f64, Mood, Act, f64, String)> {
        let mut d = Director::default();
        if choreo {
            let lib = crate::choreography::library();
            let src = crate::choreography::Sources::default();
            d.configure(&crate::choreography::Preferences::default(), &src.active(&lib), false);
        }
        let bpm = 124.0;
        let beat_s = 60.0 / bpm;
        let track_beats = 6.0 * 60.0 / beat_s;
        let dt = 1.0 / 30.0;
        let (mut out, mut end, mut t) = (vec![], 0.0f64, 0.0f64);
        let mut started = false;
        while t < seconds {
            let n = (t / beat_s / track_beats).floor();
            let beat = t / beat_s - n * track_beats;
            let bar = beat / 4.0;
            let title = format!("Track {n}");
            // Breakdown bars 64..80, build 80..88 (louder), otherwise groove.
            let (level, kicking) = if (64.0..80.0).contains(&bar) { (0.25, false) } else if (80.0..88.0).contains(&bar) { (0.55 + (bar - 80.0) as f32 * 0.04, true) } else { (0.62 + 0.05 * (beat * 0.37).sin() as f32, true) };
            let mix_start = track_beats - mix * 4.0;
            let handover = (beat >= mix_start - 32.0).then(|| Handover { from: title.clone(), title: format!("Track {}", n + 1.0), artist: "B".into(), beats_in: beat - mix_start, beats: mix * 4.0, bpm });
            let m = Music { title, artist: "A".into(), playing: true, level, beat, bpm, transition: beat >= mix_start, handover, bands: vec![], kicking, calm: 0.0 };
            d.listen(&m, dt);
            let due = if !started { started = true; Some(d.mood(&m)) } else { d.due(&m, t >= end, t) };
            if let Some(mood) = due {
                let c = d.cue(&m, mood, t, crate::pack::PALETTES[0]);
                end = t + c.fit_s.unwrap_or(8.0);
                out.push((t, mood, c.act, c.fit_s.unwrap_or(8.0) / (beat_s * 4.0), c.material_id.clone()));
            }
            t += dt;
        }
        out
    }
    /// How long each picture stays (the material only changes then).
    fn pictures(cues: &[(f64, Mood, Act, f64, String)]) -> Vec<f64> {
        let mut pics = vec![];
        let mut since = 0.0;
        for w in cues.windows(2) {
            if w[1].4 != w[0].4 {
                pics.push(w[1].0 - since);
                since = w[1].0;
            }
        }
        pics.sort_by(f64::total_cmp);
        pics
    }
    /// The user (2026-11-09): "too much just playing short visuals in a row
    /// … a couple of seconds and then next". A picture holds about half a
    /// minute, never less than four bars, through grooves, breakdowns,
    /// titles and AI mixes of any length.
    #[test]
    fn pictures_hold_long_enough_to_watch() {
        for mix in [8.0, 16.0, 32.0, 64.0] {
            for choreo in [false, true] {
                let pics = pictures(&simulate(mix, 1800.0, choreo));
                let four_bars = 4.0 * 4.0 * 60.0 / 124.0 - 0.1;
                assert!(pics[0] >= four_bars, "mix {mix}: a picture lasted {:.1}s", pics[0]);
                assert!(pics[pics.len() / 2] >= 25.0, "mix {mix}: median {:.1}s", pics[pics.len() / 2]);
                assert!(pics.len() <= 75, "mix {mix}: {} picture changes in 30 minutes", pics.len());
            }
        }
    }
    #[test]
    #[ignore]
    fn print_pacing() {
        for mix in [8.0, 16.0, 32.0] {
            for choreo in [false, true] {
                let cues = simulate(mix, 1800.0, choreo);
                let mut lens: Vec<f64> = cues.windows(2).map(|w| w[1].0 - w[0].0).collect();
                lens.sort_by(f64::total_cmp);
                let short = lens.iter().filter(|l| **l < 8.0).count();
                eprintln!("mix {mix} choreo {choreo}: {} cues in 30 min, median {:.1}s, p10 {:.1}s, under 8s: {short}", cues.len(), lens[lens.len() / 2], lens[lens.len() / 10]);
                let mut pics: Vec<f64> = vec![];
                let mut since = 0.0;
                for w in cues.windows(2) {
                    if w[1].4 != w[0].4 { if w[1].0 - since < 7.0 { eprintln!("     short {:.1}s at {:.1}s {:?}->{:?}", w[1].0 - since, w[1].0, w[0].1, w[1].1); } pics.push(w[1].0 - since); since = w[1].0; }
                }
                pics.sort_by(f64::total_cmp);
                eprintln!("   picture changes: {} in 30 min, median {:.1}s, shortest {:.1}s", pics.len(), pics[pics.len() / 2], pics[0]);
                if mix == 16.0 && choreo {
                    for (i, w) in cues.windows(2).enumerate().take(80) {
                        let (t, mood, act, bars, id) = &w[0];
                        eprintln!("  {i:3} {t:7.1}s {:5.1}s {mood:?} {act:?} plan {bars:.1} bars {id}", w[1].0 - t);
                    }
                }
            }
        }
    }
}
