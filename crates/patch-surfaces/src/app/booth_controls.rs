//! The photographed players' controls and real source/browser/memory screens.
//! All transport remains in the engine; these views own only operator choices.
use super::*;
use patch_core::command::Deck as CDeck;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    vinyl: bool,
    load_lock: bool,
    jump: f32,
    jog_response: f32,
    wave_span: f32,
    usb: [Option<i64>; 2],
    active_slot: Option<(usize, String)>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            vinyl: true,
            load_lock: true,
            jump: 4.0,
            jog_response: 1.0,
            wave_span: 8.0,
            usb: [None; 2],
            active_slot: None,
        }
    }
}
#[derive(Clone)]
struct Row {
    label: String,
    detail: String,
    key: String,
}
impl Row {
    fn new(label: impl Into<String>, detail: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            key: key.into(),
        }
    }
}
#[derive(Clone)]
struct Memory {
    id: Option<String>,
    from: f64,
    to: Option<f64>,
    label: String,
}
#[derive(Clone)]
struct PendingDouble {
    from: usize,
    path: String,
    load: Option<u32>,
    at: Instant,
}
#[derive(Clone)]
struct Player {
    prefs: Preferences,
    mode: String,
    status: String,
    query: String,
    cursor: usize,
    rows: Vec<Row>,
    dirty: bool,
    source: String,
    collection: Option<i64>,
    assigning: Option<usize>,
    slot: Option<usize>,
    memories: Vec<Memory>,
    memory_cursor: Option<usize>,
    loop_edit: String,
    filter: String,
    key_before: Option<i32>,
    powered: bool,
    censor_held: bool,
    jog_held: bool,
    wave_base: Option<f64>,
    pending_double: Option<PendingDouble>,
}
impl Player {
    fn new(prefs: Preferences) -> Self {
        Self {
            prefs,
            mode: "play".into(),
            status: String::new(),
            query: String::new(),
            cursor: 0,
            rows: vec![],
            dirty: true,
            source: String::new(),
            collection: None,
            assigning: None,
            slot: None,
            memories: vec![],
            memory_cursor: None,
            loop_edit: String::new(),
            filter: String::new(),
            key_before: None,
            powered: true,
            censor_held: false,
            jog_held: false,
            wave_base: None,
            pending_double: None,
        }
    }
    fn screen(&mut self, name: &str) {
        self.mode = name.into();
        self.cursor = 0;
        self.dirty = true;
    }
}
#[derive(Clone)]
struct Context {
    players: [Player; 4],
}
pub(super) struct View {
    window: slint::Weak<PatchWindow>,
    context: RefCell<Context>,
    rows: [Rc<VecModel<crate::ui::BoothRow>>; 4],
    panels: Rc<VecModel<crate::ui::BoothPanel>>,
}
fn deck(i: usize) -> CDeck {
    match i {
        1 => CDeck::B,
        2 => CDeck::C,
        3 => CDeck::D,
        _ => CDeck::A,
    }
}
fn prefs_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("omarchy-patch/club-controls.json")
}
fn load_preferences() -> [Preferences; 4] {
    std::fs::read(prefs_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Preferences>>(&b).ok())
        .map(|values| std::array::from_fn(|i| values.get(i).cloned().unwrap_or_default()))
        .unwrap_or_default()
}
fn save_preferences(players: &[Player; 4]) {
    let path = prefs_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let values = players.each_ref().map(|p| &p.prefs);
    if let Ok(bytes) = serde_json::to_vec_pretty(&values) {
        let temp = path.with_extension("json.new");
        if std::fs::write(&temp, bytes).is_ok() {
            let _ = std::fs::rename(temp, path);
        }
    }
}
fn database() -> Result<patch_library::Library, String> {
    patch_library::open_quick()
}
fn tag_list(db: &patch_library::Library, create: bool) -> Result<Option<i64>, String> {
    match db.collection_by_name("Club Booth Tag List") {
        Some(id) => Ok(Some(id)),
        None if create => db.create_collection("Club Booth Tag List").map(Some),
        None => Ok(None),
    }
}
fn read_memory(source: &str) -> Vec<Memory> {
    let Ok(db) = database() else { return vec![] };
    let Some(track) = db.track_by_path(source) else {
        return vec![];
    };
    let mut values: Vec<Memory> = db
        .cues(track)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.kind == "memory")
        .map(|c| Memory {
            id: Some(c.id),
            from: c.sample_position as f64 / c.sample_rate.max(1) as f64,
            to: None,
            label: c.label,
        })
        .collect();
    values.extend(
        db.loops(track)
            .unwrap_or_default()
            .into_iter()
            .filter(|l| !l.temporary)
            .map(|l| Memory {
                id: Some(l.id),
                from: l.start_sample as f64 / l.sample_rate.max(1) as f64,
                to: Some(l.end_sample as f64 / l.sample_rate.max(1) as f64),
                label: l.label,
            }),
    );
    let perf = db.performance(track);
    for (at, label) in perf.memory_cues {
        if !values
            .iter()
            .any(|v| v.to.is_none() && (v.from - at).abs() < 0.001)
        {
            values.push(Memory {
                id: None,
                from: at,
                to: None,
                label,
            });
        }
    }
    for (from, to) in perf.loops {
        if !values.iter().any(|v| {
            v.to.is_some_and(|b| (v.from - from).abs() < 0.001 && (b - to).abs() < 0.001)
        }) {
            values.push(Memory {
                id: None,
                from,
                to: Some(to),
                label: "Loop".into(),
            });
        }
    }
    values.sort_by(|a, b| {
        a.from
            .total_cmp(&b.from)
            .then(a.to.is_some().cmp(&b.to.is_some()))
    });
    values
}
fn memory_rows(values: &[Memory]) -> Vec<Row> {
    values
        .iter()
        .enumerate()
        .map(|(i, m)| {
            Row::new(
                if m.label.is_empty() {
                    if m.to.is_some() {
                        "Saved loop"
                    } else {
                        "Memory cue"
                    }
                } else {
                    &m.label
                },
                match m.to {
                    Some(to) => format!("{} – {}", dj::clock(m.from), dj::clock(to)),
                    None => dj::clock(m.from),
                },
                format!("memory:{i}"),
            )
        })
        .collect()
}
fn save_memory(st: &dj::DeckStatus) -> Result<(), String> {
    let db = database()?;
    let track = db
        .track_by_path(&st.source)
        .ok_or("Add this track to the Library before storing memory")?;
    let rate = patch_analysis::RATE;
    let mut perf = db.performance(track);
    if let Some((from, to)) = st.looping.filter(|_| st.loop_on) {
        let id = format!(
            "club-loop:{track}:{}:{}",
            (from * rate as f64).round() as u64,
            (to * rate as f64).round() as u64
        );
        db.put_loop(
            track,
            &patch_library::SavedLoop {
                id,
                start_sample: (from * rate as f64).round() as u64,
                end_sample: (to * rate as f64).round() as u64,
                sample_rate: rate,
                beat_length: None,
                label: "Memory loop".into(),
                quantized: st.quantize,
                temporary: false,
                origin: "performer".into(),
            },
        )?;
        if !perf
            .loops
            .iter()
            .any(|(a, b)| (a - from).abs() < 0.001 && (b - to).abs() < 0.001)
        {
            perf.loops.push((from, to));
        }
    } else {
        if !st.has_cue {
            return Err("Set a cue with IN/CUE or CUE before pressing MEMORY".into());
        }
        let at = st.cue_point;
        let id = format!("club-cue:{track}:{}", (at * rate as f64).round() as u64);
        db.put_cue(
            track,
            &patch_library::Cue {
                id,
                kind: "memory".into(),
                sample_position: (at * rate as f64).round() as u64,
                sample_rate: rate,
                beat_position: None,
                label: "Memory cue".into(),
                color: None,
                comment: String::new(),
                origin: "performer".into(),
                confidence: None,
                generated: false,
            },
        )?;
        if !perf.memory_cues.iter().any(|(v, _)| (v - at).abs() < 0.001) {
            perf.memory_cues.push((at, "Memory cue".into()));
        }
    }
    db.set_performance(track, &perf)
}
fn delete_memory(source: &str, memory: &Memory) -> Result<(), String> {
    let db = database()?;
    let track = db
        .track_by_path(source)
        .ok_or("Track is not in the Library")?;
    if let Some(id) = &memory.id {
        if memory.to.is_some() {
            db.delete_loop(id)?;
        } else {
            db.delete_cue(id)?;
        }
    }
    let mut perf = db.performance(track);
    if let Some(to) = memory.to {
        perf.loops
            .retain(|(a, b)| (a - memory.from).abs() > 0.001 || (b - to).abs() > 0.001);
    } else {
        perf.memory_cues
            .retain(|(at, _)| (at - memory.from).abs() > 0.001);
    }
    db.set_performance(track, &perf)
}
// Keep TouchAreas alive across polls: replacing a row model during a press
// loses its release/click, and the encoder then activates the wrong row.
pub(super) fn update_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, next: Vec<T>) {
    if model.row_count() != next.len() {
        model.set_vec(next);
        return;
    }
    for (i, row) in next.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}
fn slot_is_playing(prefs: &Preferences, slot: usize, source: &str) -> bool {
    prefs
        .active_slot
        .as_ref()
        .is_some_and(|(loaded, path)| *loaded == slot && path == source)
}

impl View {
    fn opposite(&self, i: usize) -> usize {
        self.window.upgrade().map(|w| {
            let state = w.global::<crate::ui::BoothControls>();
            AudioApp::deck_index(if i == 0 || i == 2 { state.get_right_deck() } else { state.get_left_deck() }.as_str())
        }).unwrap_or(if i == 0 || i == 2 { 1 } else { 0 })
    }

    pub(super) fn invalidate(&self) {
        for p in &mut self.context.borrow_mut().players {
            p.dirty = true;
        }
    }
    fn library_needed(&self) -> bool {
        self.context.borrow().players.iter().any(|p| {
            matches!(
                p.mode.as_str(),
                "source" | "browse" | "search" | "tags" | "playlists" | "filter"
            )
        })
    }
    fn rebuild(&self, app: &AudioApp, p: &mut Player, i: usize) {
        let st = app.decks[i].snapshot();
        p.rows = match p.mode.as_str() {
            "source" => {
                let mut rows = vec![Row::new("All music", "The Patch Library", "all")];
                let db = database().ok();
                let collections = db.as_ref().map(|d| d.collections()).unwrap_or_default();
                for slot in 0..2 {
                    let label = p.prefs.usb[slot]
                        .and_then(|id| collections.iter().find(|c| c.0 == id).map(|c| c.1.clone()))
                        .unwrap_or_else(|| "Choose a collection".into());
                    rows.push(Row::new(
                        format!("USB {} · local collection", slot + 1),
                        label,
                        format!("usb:{slot}"),
                    ));
                }
                rows.push(Row::new("Playlists", "Choose a collection", "playlists"));
                rows.push(Row::new("Tag list", "Your prepared tracks", "tags"));
                rows.push(Row::new(
                    "Manage / import sources",
                    "Open the Library's source controls",
                    "manage",
                ));
                rows
            }
            "playlists" => database()
                .map(|db| {
                    db.collections()
                        .into_iter()
                        .map(|(id, name, n)| {
                            Row::new(name, format!("{n} tracks"), format!("collection:{id}"))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            "browse" | "search" | "tags" => {
                let collection = if p.mode == "tags" {
                    database()
                        .ok()
                        .and_then(|db| tag_list(&db, false).ok().flatten())
                } else {
                    p.collection
                };
                // The background Library loader already resolved collection paths.
                // Never materialize every Track on the UI thread to open a playlist.
                let paths = collection.map(|id| {
                    app.lib_members
                        .borrow()
                        .get(&id)
                        .cloned()
                        .unwrap_or_default()
                });
                let q = p.query.to_lowercase();
                let other = app.decks[self.opposite(i)].snapshot();
                let reference = app.analysis_brief(&other.source);
                let other_bpm = other.grid.map(|g|g.0).or_else(||reference.as_ref().and_then(|a|a.bpm)).map(|b|b*(1.0+other.tempo));
                let other_key = reference.as_ref().and_then(|a|a.key).map(|mut key| { key.tonic=(key.tonic as i32+other.key_shift).rem_euclid(12) as u8; key });
                app.library
                    .borrow()
                    .iter()
                    .filter(|t| {
                        if p.mode == "tags" && collection.is_none() {
                            return false;
                        }
                        if let Some(paths) = &paths
                            && !paths.contains(&t.url)
                        {
                            return false;
                        }
                        if !q.is_empty()
                            && !format!("{} {}", t.title, t.detail)
                                .to_lowercase()
                                .contains(&q)
                        {
                            return false;
                        }
                        match p.filter.as_str() {
                            "bpm3" | "bpm6" => other_bpm
                                .zip(app.analysis_brief(&t.url).and_then(|a| a.bpm))
                                .is_some_and(|(a, b)| {
                                    (a - b).abs() <= if p.filter == "bpm3" { 3.0 } else { 6.0 }
                                }),
                            "key" => other_key
                                .zip(app.analysis_brief(&t.url).and_then(|a| a.key))
                                .is_some_and(|(a, b)| a == b),
                            _ => true,
                        }
                    })
                    .map(|t| Row::new(&t.title, &t.detail, format!("track:{}", t.url)))
                    .collect()
            }
            "memory" => memory_rows(&p.memories),
            "filter" => vec![
                Row::new("All tracks", "Clear the track filter", "filter:all"),
                Row::new("BPM ±3", "Compared with the other player", "filter:bpm3"),
                Row::new("BPM ±6", "Compared with the other player", "filter:bpm6"),
                Row::new(
                    "Same musical key",
                    "Compared with the other player",
                    "filter:key",
                ),
                Row::new("Edit beat grid", "BPM, first beat and offset", "grid"),
            ],
            "grid" => {
                let (bpm, offset, _) = st.grid.unwrap_or((120.0, 0.0, None));
                vec![
                    Row::new("BPM −0.01", format!("{bpm:.2} BPM"), "grid:bpm-down"),
                    Row::new("BPM +0.01", format!("{bpm:.2} BPM"), "grid:bpm-up"),
                    Row::new("Grid −1 ms", format!("{offset:.3} s"), "grid:back"),
                    Row::new("Grid +1 ms", format!("{offset:.3} s"), "grid:forward"),
                    Row::new("First beat here", dj::clock(st.time), "grid:here"),
                    Row::new("Half BPM", "Keep the first beat", "grid:half"),
                    Row::new("Double BPM", "Keep the first beat", "grid:double"),
                ]
            }
            "menu" | "shortcut" => vec![
                Row::new(
                    "Load lock",
                    if p.prefs.load_lock {
                        "ON · pause before loading"
                    } else {
                        "OFF"
                    },
                    "setting:lock",
                ),
                Row::new(
                    "Jog mode",
                    if p.prefs.vinyl {
                        "VINYL · platter scratch"
                    } else {
                        "CDJ · pitch bend"
                    },
                    "setting:jog",
                ),
                Row::new(
                    "Beat jump",
                    format!("{} beats · press to cycle", p.prefs.jump),
                    "setting:jump",
                ),
                Row::new("Key −1", format!("{:+} semitones", st.key_shift), "key:-1"),
                Row::new("Key +1", format!("{:+} semitones", st.key_shift), "key:1"),
                Row::new("Key reset", "Original musical key", "key:reset"),
                Row::new("Beat grid", "Edit BPM and first beat", "grid"),
                Row::new(
                    "Memory cues / loops",
                    format!("{} saved", p.memories.len()),
                    "memory",
                ),
                Row::new("Source", "Library / USB source slots", "source"),
            ],
            _ => vec![],
        };
        p.cursor = p.cursor.min(p.rows.len().saturating_sub(1));
        p.dirty = false;
    }
    /// Loading is asynchronous. Finish a copy only when that exact native
    /// load is ready, then read the source's current heard position.
    fn finish_doubles(&self, app: &AudioApp) {
        let ready = {
            let mut context = self.context.borrow_mut();
            let mut ready = Vec::new();
            for (i, player) in context.players.iter_mut().enumerate() {
                let Some(request) = player.pending_double.clone() else { continue };
                let target = app.decks[i].snapshot();
                let superseded = matches!((target.load, request.load), (Some(now), Some(wanted)) if now > wanted);
                if superseded || request.at.elapsed() > Duration::from_secs(30) {
                    player.pending_double = None;
                    player.status = if superseded { "Instant Double cancelled by another load" } else { "Instant Double timed out loading the track" }.into();
                } else if target.loaded && target.source == request.path
                    && request.load.is_none_or(|load| target.load == Some(load))
                {
                    player.pending_double = None;
                    if target.paused { ready.push((i, request)); }
                    else { player.status = "Instant Double cancelled by manual playback".into(); }
                }
            }
            ready
        };
        // Commands may refresh views: never dispatch with the context borrowed.
        for (i, request) in ready {
            let other = app.decks[request.from].status();
            let result = (|| -> Result<(), String> {
                let target = app.decks[i].status();
                if !target.loaded || target.source != request.path || !target.paused
                    || request.load.is_some_and(|load| target.load != Some(load))
                {
                    return Err("Instant Double cancelled: the destination changed".into());
                }
                if !other.loaded || other.source != request.path {
                    return Err("Instant Double cancelled: the source track changed".into());
                }
                app.command(Command::DeckSeek { deck: deck(i), seconds: other.time }, Origin::User)?;
                app.command(Command::DeckTempo { deck: deck(i), percent: (other.tempo * 100.0) as f32 }, Origin::User)?;
                app.command(Command::DeckKeylock { deck: deck(i), on: other.keylock }, Origin::User)?;
                app.command(Command::DeckKey { deck: deck(i), semitones: other.key_shift }, Origin::User)?;
                if let Some((bpm, offset, down)) = other.grid {
                    app.decks[i].set_grid(Some(bpm), offset, down);
                }
                if !other.paused {
                    app.command(Command::DeckTransport { deck: deck(i), action: Transport::Play }, Origin::User)?;
                }
                Ok(())
            })();
            self.context.borrow_mut().players[i].status = match result {
                Ok(()) => format!("Instant Double from deck {}", deck(request.from).id().to_uppercase()),
                Err(error) => error,
            };
        }
    }

    pub(super) fn refresh(&self, app: &AudioApp) {
        let Some(_window) = self.window.upgrade() else {
            return;
        };
        self.finish_doubles(app);
        let mut context = self.context.borrow_mut();
        let mut panels = Vec::with_capacity(4);
        for (i, p) in context.players.iter_mut().enumerate() {
            let st = app.decks[i].snapshot();
            if p.source != st.source {
                p.source = st.source.clone();
                p.memories = read_memory(&st.source);
                p.memory_cursor = None;
                p.loop_edit.clear();
                p.key_before = None;
                p.dirty = true;
            }
            if !st.loop_on {
                p.loop_edit.clear();
            }
            if p.dirty {
                self.rebuild(app, p, i);
            }
            let first = (p.cursor / 8) * 8;
            let rows = p
                .rows
                .iter()
                .enumerate()
                .skip(first)
                .take(8)
                .map(|(index, row)| crate::ui::BoothRow {
                    label: row.label.as_str().into(),
                    detail: row.detail.as_str().into(),
                    index: index as i32,
                })
                .collect::<Vec<_>>();
            update_rows(&self.rows[i], rows);
            panels.push(crate::ui::BoothPanel {
                view: p.mode.as_str().into(),
                heading: match p.mode.as_str() {
                    "tags" => "TAG LIST",
                    "source" => "SOURCE",
                    "playlists" => "PLAYLISTS",
                    "search" => "SEARCH",
                    "memory" => "MEMORY CUES / LOOPS",
                    "filter" => "TRACK FILTER",
                    "grid" => "GRID EDIT",
                    "menu" => "PLAYER SETTINGS",
                    "shortcut" => "SHORTCUT",
                    _ => "BROWSE",
                }
                .into(),
                status: p.status.as_str().into(),
                query: p.query.as_str().into(),
                rows: self.rows[i].clone().into(),
                cursor: p.cursor as i32,
                total: p.rows.len() as i32,
                vinyl: p.prefs.vinyl,
                powered: p.powered,
                load_lock: p.prefs.load_lock,
                master: app.club_master.get() == i,
                reverse: st.reverse,
                censor: st.censor,
                key_synced: p.key_before.is_some(),
                loop_edit: p.loop_edit.as_str().into(),
                jump_beats: p.prefs.jump,
                jog_response: p.prefs.jog_response,
                wave_span: p.prefs.wave_span,
            });
        }
        update_rows(&self.panels, panels);
    }
    fn load(app: &AudioApp, p: &mut Player, i: usize, path: &str) -> Result<(), String> {
        let st = app.decks[i].snapshot();
        if p.prefs.load_lock && st.loaded && !st.paused {
            return Err("LOAD LOCK · Pause this deck, or turn Load lock off in MENU".into());
        }
        if !p.prefs.load_lock && st.loaded && !st.paused {
            app.decks[i].deck_action("pause", serde_json::json!({}))?;
        }
        p.pending_double = None;
        app.command(
            Command::DeckLoadPath {
                deck: deck(i),
                path: path.into(),
            },
            Origin::User,
        )?;
        p.powered = true;
        p.prefs.active_slot = p.slot.map(|slot| (slot, path.to_string()));
        p.screen("play");
        p.status = "Track loaded".into();
        p.loop_edit.clear();
        p.key_before = None;
        Ok(())
    }
    fn recall(app: &AudioApp, p: &mut Player, i: usize, index: usize) -> Result<(), String> {
        let memory = p
            .memories
            .get(index)
            .cloned()
            .ok_or("No memory cues or loops saved · set a cue/loop and press MEMORY")?;
        let st = app.decks[i].snapshot();
        if p.prefs.load_lock && !st.paused {
            return Err("Pause the deck before calling a memory cue or loop".into());
        }
        match memory.to {
            Some(to) => app.command(
                Command::DeckLoopSet {
                    deck: deck(i),
                    from: memory.from,
                    to,
                    play: false,
                },
                Origin::User,
            )?,
            None => app.command(
                Command::DeckCueRecall {
                    deck: deck(i),
                    seconds: memory.from,
                },
                Origin::User,
            )?,
        };
        p.memory_cursor = Some(index);
        p.screen("memory");
        p.cursor = index;
        p.status = format!("Called {}", memory.label);
        Ok(())
    }
    fn activate(
        app: &AudioApp,
        p: &mut Player,
        i: usize,
        window: &PatchWindow,
    ) -> Result<(), String> {
        let Some(row) = p.rows.get(p.cursor).cloned() else {
            return Err("No tracks / choices in this view".into());
        };
        if let Some(path) = row.key.strip_prefix("track:") {
            return Self::load(app, p, i, path);
        }
        if let Some(id) = row
            .key
            .strip_prefix("collection:")
            .and_then(|x| x.parse::<i64>().ok())
        {
            if let Some(slot) = p.assigning.take() {
                p.prefs.usb[slot] = Some(id);
                p.slot = Some(slot);
            }
            p.collection = Some(id);
            p.query.clear();
            p.screen("browse");
            return Ok(());
        }
        if let Some(slot) = row
            .key
            .strip_prefix("usb:")
            .and_then(|x| x.parse::<usize>().ok())
        {
            if let Some(id) = p.prefs.usb[slot] {
                p.collection = Some(id);
                p.slot = Some(slot);
                p.screen("browse");
            } else {
                p.assigning = Some(slot);
                p.screen("playlists");
                p.status = format!("Choose the collection for USB {}", slot + 1);
            }
            return Ok(());
        }
        if let Some(index) = row
            .key
            .strip_prefix("memory:")
            .and_then(|x| x.parse::<usize>().ok())
        {
            return Self::recall(app, p, i, index);
        }
        if let Some(filter) = row.key.strip_prefix("filter:") {
            p.filter = if filter == "all" {
                String::new()
            } else {
                filter.into()
            };
            p.screen("browse");
            return Ok(());
        }
        if let Some(setting) = row.key.strip_prefix("setting:") {
            match setting {
                "lock" => p.prefs.load_lock = !p.prefs.load_lock,
                "jog" => p.prefs.vinyl = !p.prefs.vinyl,
                "jump" => {
                    let values = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
                    let n = values.iter().position(|v| *v == p.prefs.jump).unwrap_or(2);
                    p.prefs.jump = values[(n + 1) % values.len()];
                }
                _ => {}
            }
            p.dirty = true;
            return Ok(());
        }
        if let Some(value) = row.key.strip_prefix("key:") {
            let next = if value == "reset" {
                0
            } else {
                app.decks[i].snapshot().key_shift + value.parse::<i32>().unwrap_or(0)
            };
            app.command(
                Command::DeckKey {
                    deck: deck(i),
                    semitones: next.clamp(-12, 12),
                },
                Origin::User,
            )?;
            p.dirty = true;
            return Ok(());
        }
        if let Some(edit) = row.key.strip_prefix("grid:") {
            let st = app.decks[i].snapshot();
            let (mut bpm, mut offset, down) = st
                .grid
                .ok_or("Wait for beat analysis before editing the grid")?;
            match edit {
                "bpm-down" => bpm -= 0.01,
                "bpm-up" => bpm += 0.01,
                "back" => offset -= 0.001,
                "forward" => offset += 0.001,
                "here" => offset = st.time,
                "half" => bpm *= 0.5,
                "double" => bpm *= 2.0,
                _ => {}
            }
            offset = offset.max(0.0);
            if !(40.0..=300.0).contains(&bpm) {
                return Err("BPM must be between 40 and 300".into());
            }
            let db = database()?;
            let track = db
                .track_by_path(&st.source)
                .ok_or("Track is not in the Library")?;
            let mut perf = db.performance(track);
            perf.grid = Some((bpm, offset));
            db.set_performance(track, &perf)?;
            db.set_human_downbeat(&st.source, bpm, offset, down.unwrap_or(0))?;
            patch_engine::track_analysis::invalidate_manual_grids();
            app.decks[i].set_grid(Some(bpm), offset, down);
            app.grid_sent.borrow_mut()[i].clear();
            p.status = format!("Grid {bpm:.2} BPM · {offset:.3} s");
            p.dirty = true;
            return Ok(());
        }
        match row.key.as_str() {
            "all" => {
                p.collection = None;
                p.slot = None;
                p.query.clear();
                p.screen("browse");
            }
            "manage" => {
                window
                    .global::<crate::ui::Host>()
                    .invoke_focus_surface("library".into());
            }
            mode => p.screen(mode),
        }
        Ok(())
    }
    fn action(&self, app: &AudioApp, id: &str, action: &str, value: f32) {
        let i = AudioApp::deck_index(id);
        let Some(window) = self.window.upgrade() else {
            return;
        };
        // Commands can refresh other views; never retain a RefCell guard while dispatching.
        let before_preferences = self.context.borrow().players.clone().map(|p| p.prefs);
        let mut context = self.context.borrow().clone();
        let p = &mut context.players[i];
        let st = app.decks[i].snapshot();
        let result: Result<(), String> = (|| {
            match action {
                "source" | "browse" | "search" | "tags" | "playlists" | "menu" | "shortcut"
                | "filter" | "grid" | "memory" => {
                    p.screen(action);
                    p.status.clear();
                }
                "back" => {
                    p.assigning = None;
                    p.screen(if matches!(p.mode.as_str(), "play" | "source") {
                        "play"
                    } else {
                        "source"
                    });
                }
                "close" => {
                    p.screen("play");
                    p.loop_edit.clear();
                }
                "select" => {
                    p.cursor = (value.max(0.0) as usize).min(p.rows.len().saturating_sub(1));
                }
                "step" => {
                    if p.mode == "play" {
                        p.screen("browse");
                        self.rebuild(app, p, i);
                    }
                    p.cursor = (p.cursor as i64 + value as i64)
                        .clamp(0, p.rows.len().saturating_sub(1) as i64)
                        as usize;
                }
                "activate" => {
                    if p.mode == "play" {
                        p.screen("browse");
                    } else {
                        Self::activate(app, p, i, &window)?;
                    }
                }
                "tag" => {
                    let source = p
                        .rows
                        .get(p.cursor)
                        .and_then(|r| r.key.strip_prefix("track:"))
                        .filter(|_| matches!(p.mode.as_str(), "browse" | "search" | "tags"))
                        .unwrap_or(&st.source);
                    let db = database()?;
                    let track = db.track_by_path(source).ok_or("Choose a track to tag")?;
                    let list = tag_list(&db, true)?.unwrap();
                    if db.collection_tracks(list).contains(&track) {
                        db.remove_from_collection(list, &[track], true)?;
                        p.status = "Removed from tag list".into();
                    } else {
                        db.add_to_collection(list, track)?;
                        p.status = "Added to tag list".into();
                    }
                    p.dirty = true;
                    app.request_library(true);
                }
                "track" => {
                    let previous = p.mode.clone();
                    p.mode = "browse".into();
                    p.dirty = true;
                    self.rebuild(app, p, i);
                    let current = p
                        .rows
                        .iter()
                        .position(|r| r.key.strip_prefix("track:") == Some(st.source.as_str()))
                        .unwrap_or(0);
                    let next = (current as i64 + value as i64)
                        .clamp(0, p.rows.len().saturating_sub(1) as i64)
                        as usize;
                    p.cursor = next;
                    if let Err(e) = Self::activate(app, p, i, &window) {
                        p.mode = previous;
                        return Err(e);
                    }
                }
                "power" => {
                    if p.powered {
                        app.decks[i].sync(false, None)?;
                        app.decks[i].deck_action("eject", serde_json::json!({}))?;
                        p.prefs.active_slot = None;
                        p.powered = false;
                        p.screen("play");
                        p.status = "PLAYER OFF · Press power to choose a source".into();
                    } else {
                        p.powered = true;
                        p.screen("source");
                        p.status.clear();
                    }
                }
                "usb-stop" => {
                    let slot = (value as usize).min(1);
                    p.prefs.usb[slot] = None;
                    if slot_is_playing(&p.prefs, slot, &st.source) {
                        app.decks[i].sync(false, None)?;
                        app.decks[i].deck_action("eject", serde_json::json!({}))?;
                    }
                    if p.prefs
                        .active_slot
                        .as_ref()
                        .is_some_and(|(loaded, _)| *loaded == slot)
                    {
                        p.prefs.active_slot = None;
                    }
                    if p.slot == Some(slot) {
                        p.slot = None;
                        p.collection = None;
                    }
                    p.screen("source");
                    p.status = format!("USB {} source released", slot + 1);
                }
                "jog-mode" => {
                    app.decks[i].scrub("end", 0.0);
                    app.command(
                        Command::DeckBend {
                            deck: deck(i),
                            amount: 0.0,
                        },
                        Origin::User,
                    )?;
                    p.prefs.vinyl = !p.prefs.vinyl;
                    p.status = if p.prefs.vinyl {
                        "VINYL · platter scratch"
                    } else {
                        "CDJ · jog to pitch bend"
                    }
                    .into();
                    p.dirty = true;
                }
                "jog-response" => {
                    p.prefs.jog_response = value.clamp(0.25, 3.0);
                }
                "jump" => app.command(
                    Command::DeckJump {
                        deck: deck(i),
                        beats: p.prefs.jump * value,
                    },
                    Origin::User,
                )?,
                "loop-in" | "loop-out" => {
                    if st.loop_on {
                        let edge = if action == "loop-out" { "out" } else { "in" };
                        p.loop_edit = if p.loop_edit == edge {
                            String::new()
                        } else {
                            edge.into()
                        };
                        p.status = if p.loop_edit.is_empty() {
                            "Loop adjustment finished".into()
                        } else {
                            format!(
                                "LOOP {} ADJUST · jog / SEARCH · press again to finish",
                                edge.to_uppercase()
                            )
                        };
                    } else {
                        app.command(
                            if action == "loop-in" {
                                Command::DeckLoopIn { deck: deck(i) }
                            } else {
                                Command::DeckLoopOut { deck: deck(i) }
                            },
                            Origin::User,
                        )?;
                    }
                }
                "loop-adjust" => {
                    if !p.loop_edit.is_empty() {
                        app.command(
                            Command::DeckLoopAdjust {
                                deck: deck(i),
                                out: p.loop_edit == "out",
                                seconds: value as f64,
                            },
                            Origin::User,
                        )?;
                    }
                }
                "reverse" => {
                    app.command(
                        Command::DeckReverse {
                            deck: deck(i),
                            on: value > 0.5,
                        },
                        Origin::User,
                    )?;
                    p.status = if value > 0.5 { "REVERSE" } else { "FORWARD" }.into();
                }
                "censor" => {
                    app.command(
                        Command::DeckCensor {
                            deck: deck(i),
                            on: value > 0.5,
                        },
                        Origin::User,
                    )?;
                    p.censor_held = value > 0.5;
                }
                "zoom" => {
                    p.prefs.wave_span =
                        (p.prefs.wave_span * if value > 0.0 { 0.5 } else { 2.0 }).clamp(2.0, 8.0);
                }
                "wave-start" => {
                    p.wave_base = Some(st.time);
                }
                "wave-move" => { if let Some(base)=p.wave_base { app.command(Command::DeckSeek {deck:deck(i),seconds:(base+value as f64).max(0.0)},Origin::User)?; } },
                "wave-end" => { p.wave_base=None; },
                // Headphones only, the program plays on: a spot on the
                // whole-track overview (`value` 0…1) or a hot cue (slot).
                "touch-preview" => {
                    if let Some(d) = st.duration.filter(|_| st.native) {
                        app.decks[i].audition(Some(f64::from(value.clamp(0.0, 1.0)) * d));
                        p.status = "Preview in headphones".into();
                    }
                }
                "cue-preview" => {
                    let slot = (value.max(0.0) as usize).min(dj::HOT_CUES - 1);
                    match app.cues.borrow().get(&st.source).and_then(|c| c[slot]) {
                        Some(t) if st.native => {
                            app.decks[i].audition(Some(t));
                            p.status = format!("Hot cue {} in headphones", slot + 1);
                        }
                        _ => p.status = format!("Hot cue {} is empty", slot + 1),
                    }
                }
                "preview-end" => {
                    app.decks[i].audition(None);
                    p.status.clear();
                }
                "jog-start" => {
                    p.jog_held = true;
                    if p.prefs.vinyl && p.loop_edit.is_empty() {
                        app.decks[i].scrub("start", 0.0);
                    }
                }
                "jog-bend" => {
                    app.command(Command::DeckBend {deck:deck(i),amount:value.clamp(-0.1,0.1)},Origin::User)?;
                    p.status=format!("CDJ pitch bend {}%", crate::num::signed(value * 100.0, 1));
                },
                "jog-end" => {
                    p.jog_held = false;
                    app.decks[i].scrub("end", 0.0);
                    app.command(
                        Command::DeckBend {
                            deck: deck(i),
                            amount: 0.0,
                        },
                        Origin::User,
                    )?;
                }
                "master" => {
                    app.command(Command::DeckMaster { deck: deck(i) }, Origin::User)?;
                    app.club_master.set(i);
                    p.status = "TEMPO MASTER".into();
                }
                "sync" => {
                    if !st.loaded || !app.decks[self.opposite(i)].snapshot().loaded || st.grid.is_none() {
                        return Err(
                            "Load both players and wait for beat analysis before sync".into()
                        );
                    }
                    if st.sync_on {
                        app.decks[i].sync(false, None)?;
                    } else {
                        let master = if app.club_master.get() == i || !app.decks[app.club_master.get()].snapshot().loaded {
                            self.opposite(i)
                        } else {
                            app.club_master.get()
                        };
                        app.command(Command::DeckMaster { deck: deck(master) }, Origin::User)?;
                        app.club_master.set(master);
                        app.decks[i].sync(true, Some(deck(master).id()))?;
                    }
                }
                "key-sync" => {
                    if let Some(before) = p.key_before.take() {
                        app.command(
                            Command::DeckKey {
                                deck: deck(i),
                                semitones: before,
                            },
                            Origin::User,
                        )?;
                        p.status = "Original key restored".into();
                    } else {
                        let other = app.decks[self.opposite(i)].snapshot();
                        let a = app
                            .analysis(&st.source)
                            .ok_or("Wait for this track's key analysis")?;
                        let b = app
                            .analysis(&other.source)
                            .ok_or("Load and analyse the other player first")?;
                        if a.vocals.as_ref().is_none_or(|v| v.presence >= 0.10) {
                            return Err(
                                "Vocal protection · this track keeps its original key".into()
                            );
                        }
                        let from = a.key.ok_or("This track's musical key is not known yet")?;
                        let to = b
                            .key
                            .ok_or("The other track's musical key is not known yet")?;
                        let target = (to.tonic as i32
                            + other.key_shift
                            + if from.minor == to.minor {
                                0
                            } else if from.minor {
                                9
                            } else {
                                3
                            })
                        .rem_euclid(12);
                        let shift = (target - from.tonic as i32 + 6).rem_euclid(12) - 6;
                        app.command(
                            Command::DeckKeylock {
                                deck: deck(i),
                                on: true,
                            },
                            Origin::User,
                        )?;
                        app.command(
                            Command::DeckKey {
                                deck: deck(i),
                                semitones: shift,
                            },
                            Origin::User,
                        )?;
                        p.key_before = Some(st.key_shift);
                        p.status = format!("Key sync {shift:+} semitones · press again to restore");
                    }
                }
                "duplicate" => {
                    let from = self.opposite(i);
                    let other = app.decks[from].snapshot();
                    if !other.loaded { return Err("Load the other player before Instant Doubles".into()); }
                    let slot = p.slot.take();
                    let collection = p.collection.take();
                    if let Err(error) = Self::load(app, p, i, &other.source) {
                        p.slot = slot;
                        p.collection = collection;
                        return Err(error);
                    }
                    p.pending_double = Some(PendingDouble {
                        from,
                        path: other.source,
                        load: st.load.map(|load| load.wrapping_add(1)),
                        at: Instant::now(),
                    });
                    p.status = "Loading Instant Double…".into();
                }

                "memory-save" => {
                    save_memory(&st)?;
                    p.memories = read_memory(&st.source);
                    p.memory_cursor = p.memories.iter().rposition(|m| {
                        (m.from
                            - st.looping
                                .filter(|_| st.loop_on)
                                .map_or(st.cue_point, |b| b.0))
                        .abs()
                            < 0.01
                    });
                    p.screen("memory");
                    p.cursor = p.memory_cursor.unwrap_or(0);
                    p.status = "Memory stored in the Library".into();
                }
                "memory-call" => {
                    if st.loop_on && p.mode != "memory" {
                        app.command(
                            Command::DeckLoopScale {
                                deck: deck(i),
                                factor: if value < 0.0 { 0.5 } else { 2.0 },
                            },
                            Origin::User,
                        )?;
                    } else {
                        p.memories = read_memory(&st.source);
                        let index = match p.memory_cursor {
                            Some(n) => (n as i64 + value as i64)
                                .clamp(0, p.memories.len().saturating_sub(1) as i64)
                                as usize,
                            None => {
                                if value < 0.0 {
                                    p.memories.len().saturating_sub(1)
                                } else {
                                    0
                                }
                            }
                        };
                        Self::recall(app, p, i, index)?;
                    }
                }
                "memory-delete" => {
                    let index = p
                        .memory_cursor
                        .ok_or("Select a memory with CUE/LOOP CALL before DELETE")?;
                    let memory = p.memories.get(index).ok_or("No memory selected")?.clone();
                    delete_memory(&st.source, &memory)?;
                    p.memories = read_memory(&st.source);
                    p.memory_cursor = None;
                    p.screen("memory");
                    p.status = "Selected memory deleted".into();
                }
                _ => return Err(format!("Unknown player control: {action}")),
            }
            Ok(())
        })();
        if let Err(error) = result {
            p.status = error;
        }
        let should_save = !app.frozen
            && context
                .players
                .iter()
                .zip(&before_preferences)
                .any(|(p, before)| p.prefs != *before);
        if should_save {
            save_preferences(&context.players);
        }
        self.context.replace(context);
        app.request_library(false);
        self.refresh(app);
    }
}
// Closing a window during a held gesture must never leave transport held.
impl Drop for View {
    fn drop(&mut self) {
        for (i, p) in self.context.get_mut().players.iter().enumerate() {
            let release = |action: &str, mut request: serde_json::Value| {
                request["op"] = "deck".into();
                request["deck"] = deck(i).id().into();
                request["do"] = action.into();
                let _ = patch_engine::client::call(&request);
            };
            if p.jog_held {
                release("scratch_end", serde_json::json!({}));
                release("bend", serde_json::json!({"value": 0.0}));
            }
            if p.censor_held {
                release("censor", serde_json::json!({"on": false}));
            }
        }
    }
}
impl AudioApp {
    pub(super) fn bind_club_controls(&self, w: &PatchWindow) {
        let prefs = load_preferences();
        let view = Rc::new(View {
            window: w.as_weak(),
            rows: std::array::from_fn(|_| Rc::new(VecModel::default())),
            panels: Rc::new(VecModel::default()),
            context: RefCell::new(Context {
                players: prefs.map(Player::new),
            }),
        });
        self.club_views.borrow_mut().push(Rc::downgrade(&view));
        let state = w.global::<crate::ui::BoothControls>();
        state.set_panels(view.panels.clone().into());
        let me = self.me.clone();
        let v = view.clone();
        state.on_action(move |id, action, value| {
            if let Some(app) = me.upgrade() {
                v.action(&app, &id, &action, value);
            }
        });
        let me = self.me.clone();
        let v = view.clone();
        state.on_query(move |id, query| {
            if let Some(app) = me.upgrade() {
                let i = AudioApp::deck_index(&id);
                {
                    let mut ctx = v.context.borrow_mut();
                    let p = &mut ctx.players[i];
                    p.query = query.to_string();
                    p.cursor = 0;
                    p.dirty = true;
                }
                v.refresh(&app);
            }
        });
        view.refresh(self);
    }
    pub(super) fn poll_club_controls(&self) {
        let views: Vec<_> = self
            .club_views
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        self.club_views
            .borrow_mut()
            .retain(|v| v.strong_count() > 0);
        for view in views {
            view.refresh(self);
        }
    }
    pub(super) fn club_library_needed(&self) -> bool {
        self.club_views
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|v| v.library_needed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsing_a_slot_does_not_make_an_unrelated_loaded_track_ejectable() {
        let mut prefs = Preferences::default();
        prefs.usb[0] = Some(42);
        assert!(!slot_is_playing(&prefs, 0, "old-track.flac"));
        prefs.active_slot = Some((0, "usb-track.flac".into()));
        assert!(slot_is_playing(&prefs, 0, "usb-track.flac"));
        assert!(!slot_is_playing(&prefs, 0, "old-track.flac"));
        assert!(!slot_is_playing(&prefs, 1, "usb-track.flac"));
        let saved = serde_json::to_vec(&prefs).unwrap();
        let reopened: Preferences = serde_json::from_slice(&saved).unwrap();
        assert!(slot_is_playing(&reopened, 0, "usb-track.flac"));
    }
}
