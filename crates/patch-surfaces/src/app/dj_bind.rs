//! The DJ Booth and AI DJ surfaces' callbacks.

use super::*;

impl AudioApp {
    /// AI DJ windows: every control is a validated command to the service.
    pub(super) fn bind_aidj(&self, w: &PatchWindow) {
        let a = w.global::<crate::ui::AiDjState>();
        let me = self.me.clone();
        a.on_key_guard(move |text| me.upgrade().is_none_or(|a| a.keys.allow(&text)));
        let send = {
            let me = self.me.clone();
            move |cmd: Command| {
                if let Some(app) = me.upgrade()
                    && let Err(e) = app.command(cmd, Origin::User)
                {
                    *app.toast.borrow_mut() = format!("Auto Mix: {e}");
                    app.push_toast();
                }
            }
        };
        let s = send.clone();
        a.on_set_mode(move |m| {
            if let Ok(mode) = serde_json::from_value(serde_json::json!(m.as_str())) {
                s(Command::AiMode { mode });
            }
        });
        let s = send.clone();
        a.on_pick(move |rank| {
            s(Command::AiPick {
                id: None,
                rank: Some(rank.max(1) as usize),
            })
        });
        let s = send.clone();
        a.on_drop(move |rank| {
            s(Command::AiDrop {
                id: None,
                rank: Some(rank.max(1) as usize),
            })
        });
        let s = send.clone();
        a.on_approve(move || s(Command::AiApprove));
        a.on_cancel(move || send(Command::AiCancel));
        // The engine marks the previous transition as reported; the view
        // shows it from its next poll (a second at most).
        let weak = w.as_weak();
        let me = self.me.clone();
        a.on_report(move |why| {
            if let Some(w)=weak.upgrade() {
                let id=w.global::<crate::ui::AiDjState>().get_report_target();
                let result=patch_engine::client::call(&serde_json::json!({"op":"report","why":why.as_str(),"transition_id":id.as_str()}));
                if let Some(app)=me.upgrade() { *app.toast.borrow_mut()=result.map(|_|"Transition report saved".into()).unwrap_or_else(|e|format!("Report could not be saved: {e}"));app.push_toast(); }
            }
        });
    }

    pub(super) fn bind_dj(&self, w: &PatchWindow) {
        self.bind_sample(w);
        let d = w.global::<DjState>();
        d.set_mixer_four(self.mixer_decks.borrow().four);
        let me = self.me.clone();
        d.on_mixer_decks_toggle(move || {
            if let Some(app) = me.upgrade() {
                let four = !app.mixer_decks.borrow().four;
                app.mixer_decks.borrow_mut().four = four;
                for w in app.dj_windows() {
                    w.global::<DjState>().set_mixer_four(four);
                }
            }
        });
        // A deck window that opens while an AI DJ session runs opens on the AI
        // phrase timeline too: that is the session's default, and the deck's
        // own toggle still overrides it afterwards.
        if self.ai_was_on.get() {
            d.set_mission_view_a(true);
            d.set_mission_view_b(true);
        }
        // Headless shots of the AI timeline (`PATCH_SHOT_MISSION=1`): the
        // markers of a transition under way, so its look can be checked
        // without an AI DJ running.
        if patch_host::shot::active() && std::env::var_os("PATCH_SHOT_MISSION").is_some() {
            let ev = |label: &str, progress: f32, status: i32| crate::ui::MissionEvent {
                label: label.into(),
                stamp: "".into(),
                progress,
                freeze_progress: -1.0,
                status,
                preview: false,
            };
            let events = vec![ev("prepare", 0.2, 1), ev("8-bar", 0.62, 1), ev("mix", 0.74, 0), ev("complete", 0.86, 0)];
            d.set_mission_available(true);
            d.set_mission_view_a(true);
            d.set_mission_a(Rc::new(VecModel::from(events)).into());
        }
        // Each window browses the library on its own (several Library
        // windows can be open); only Library windows fill their rows.
        let view = Rc::new(LibView {
            filter: RefCell::default(),
            state: RefCell::new("all".into()),
            played: RefCell::default(),
            fits_only: Cell::new(false),
            fav_only: Cell::new(false),
            straight_only: Cell::new(false),
            favourites_first: Cell::new(false),
            cursor: Cell::new(0),
            collection: Cell::new(None),
            // Shots can show a collection played in order
            // (`PATCH_SHOT_COLLECTION=<id>` of `dj::demo_collections`).
            collection_view: Cell::new(
                std::env::var("PATCH_SHOT_COLLECTION")
                    .ok()
                    .filter(|_| patch_host::shot::active())
                    .and_then(|v| v.parse().ok())
                    .inspect(|id| {
                        self.lib_play_order.borrow_mut().insert(*id, true);
                    }),
            ),
            location: RefCell::default(),
            all: RefCell::default(),
            urls: RefCell::default(),
            first: Cell::new(0),
            shown: Cell::new(40),
            model: Rc::default(),
            window: w.as_weak(),
        });
        d.set_library(view.model.clone().into());
        if w.get_surface() == "library" {
            self.rebuild_library(&view);
            self.lib_views.borrow_mut().push(Rc::downgrade(&view));
        }
        // A window that browses only on demand (the Mini Booth's load
        // picker) joins when the picker first opens: until then it costs
        // no rebuilds and no library details.
        let (me, v) = (self.me.clone(), Rc::downgrade(&view));
        d.on_lib_attach(move || {
            if let (Some(a), Some(view)) = (me.upgrade(), v.upgrade()) {
                let known = a.lib_views.borrow().iter().any(|w| w.ptr_eq(&v));
                if !known {
                    a.lib_views.borrow_mut().push(v.clone());
                    a.request_library(false);
                }
                a.rebuild_library(&view);
            }
        });
        self.bind_dj_transport(w, &view);
        self.bind_dj_library(w, &view);
        self.bind_set_prep(w, &view);
        self.bind_dj_decks(w);
        self.bind_dj_ai_sets(w);
        self.bind_dj_effects(w);
        self.bind_dj_mixer(w);
    }

    /// Deck transport: play, cue, seek, scrub, load.
    fn bind_dj_transport(&self, w: &PatchWindow, view: &Rc<LibView>) {
        let d = w.global::<DjState>();
        let me = self.me.clone();
        d.on_toggle(move |id| {
            if let Some(a) = me.upgrade() {
                let before = a.deck(&id).status();
                a.deck(&id).toggle();
                if before.loaded {
                    a.log_track_event(
                        if before.paused { "play" } else { "pause" },
                        id.as_str(),
                        &before.source,
                        serde_json::Value::Null,
                    );
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_cue(move |id| {
            if let Some(a) = me.upgrade() {
                a.deck(&id).cue();
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_cue_release(move |id| {
            if let Some(a) = me.upgrade() {
                a.deck(&id).cue_release();
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_seek_by(move |id, s| {
            if let Some(a) = me.upgrade() {
                a.deck(&id).seek_by(s as f64);
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_scrub(move |id, phase, seconds| {
            if let Some(a) = me.upgrade() {
                a.deck(&id).scrub(&phase, f64::from(seconds));
            }
        });
        let me = self.me.clone();
        d.on_seek_to(move |id, f| {
            if let Some(a) = me.upgrade() {
                a.deck(&id).seek_to(f as f64);
                a.poll_dj();
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_load(move |id, i| {
            if let Some(a) = me.upgrade()
                && let Some(item) = a.visible_library_for(&v).get(i.max(0) as usize).cloned()
            {
                let _ = a.load_deck(&id, &item);
            }
        });
    }

    /// Browsing the library and managing collections and drives.
    fn bind_dj_library(&self, w: &PatchWindow, view: &Rc<LibView>) {
        let d = w.global::<DjState>();
        let (me, v) = (self.me.clone(), view.clone());
        d.on_move_lib(move |n| {
            if let Some(a) = me.upgrade() {
                // The rows the window shows (no filtering of the library).
                let len = v.urls.borrow().len() as i32;
                let from = v.cursor.get();
                v.cursor
                    .set((from as i32 + n).clamp(0, (len - 1).max(0)) as usize);
                *a.lib_selected.borrow_mut() = a.cursor_item(&v);
                a.move_library_cursor(&v, from);
                a.preview_follow(&v);
                // The HUD follows the cursor at once, not at the next poll.
                if let Some(w) = v.window.upgrade() {
                    let d = w.global::<DjState>();
                    if d.get_hud_open() {
                        a.push_set_prep(&d, &v);
                    }
                }
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_set_filter(move |f| {
            if let Some(a) = me.upgrade() {
                *v.filter.borrow_mut() = f.to_string();
                v.cursor.set(0);
                a.rebuild_library(&v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_create(move |name| {
            if let Some(a) = me.upgrade()
                && let Ok(db) = patch_library::open_quick()
            {
                let clean = name.trim();
                let id = db
                    .collection_by_name(clean)
                    .or_else(|| db.create_collection(clean).ok());
                v.collection.set(id);
                // The new one is shown: its tracks come from the track list.
                if id.is_some() {
                    v.collection_view.set(id);
                    v.cursor.set(0);
                    *a.dj_status.borrow_mut() = format!(
                        "Made “{clean}”: highlight a track and use Add to collection"
                    );
                }
                a.request_library(true);
                a.refresh_libraries();
                a.poll_dj();
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_rename(move |id, name| {
            let Some(a) = me.upgrade() else { return };
            let clean = name.trim().to_string();
            let note = match patch_library::open_quick()
                .and_then(|db| db.rename_collection(id as i64, &clean))
            {
                Ok(()) => format!("Renamed to “{clean}”"),
                Err(e) => format!("Couldn't rename it: {e}"),
            };
            *a.dj_status.borrow_mut() = note;
            a.request_library(true);
            a.rebuild_library(&v);
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_delete(move |id| {
            let Some(a) = me.upgrade() else { return };
            let name = a
                .lib_collections
                .borrow()
                .iter()
                .find(|(c, ..)| *c == id as i64)
                .map(|(_, n, _)| n.clone())
                .unwrap_or_default();
            let note = match patch_library::open_quick().and_then(|db| {
                let folder = db.collection_source(id as i64);
                db.delete_collection(id as i64, true).map(|()| folder)
            }) {
                Ok(Some(s)) => format!("Deleted “{name}”: {} is out of the library (its files stay)", s.path),
                Ok(None) => format!("Deleted “{name}”: its tracks stay in the library"),
                Err(e) => format!("Couldn't delete “{name}”: {e}"),
            };
            if v.collection_view.get() == Some(id as i64) {
                v.collection_view.set(None);
                v.collection.set(None);
                v.cursor.set(0);
            }
            *a.dj_status.borrow_mut() = note;
            a.request_library(true);
            a.rebuild_library(&v);
            a.poll_dj();
        });
        // The highlighted track into a collection (true) or out of it.
        let (me, v) = (self.me.clone(), view.clone());
        d.on_track_collection(move |id, add| {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.visible_library_for(&v).get(v.cursor.get()).cloned() else {
                return;
            };
            let name = a
                .lib_collections
                .borrow()
                .iter()
                .find(|(c, ..)| *c == id as i64)
                .map(|(_, n, _)| n.clone())
                .unwrap_or_default();
            let done = patch_library::open_quick().and_then(|db| {
                let track = db
                    .track_by_path(&item.url)
                    .ok_or_else(|| "it isn't a library track".to_string())?;
                if add {
                    db.add_to_collection(id as i64, track)
                } else {
                    db.remove_from_collection(id as i64, &[track], true)
                }
            });
            *a.dj_status.borrow_mut() = match (done, add) {
                (Ok(()), true) => format!("Added “{}” to “{name}”", item.title),
                (Ok(()), false) => format!("Took “{}” out of “{name}” (it stays in the library)", item.title),
                (Err(e), _) => format!("Couldn't change “{name}”: {e}"),
            };
            // Shown at once; the library's own reload confirms it.
            if let Some(paths) = a.lib_members.borrow_mut().get_mut(&(id as i64)) {
                if add {
                    paths.insert(item.url.clone());
                } else {
                    paths.remove(&item.url);
                }
            }
            a.request_library(true);
            a.refresh_libraries();
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_track_new_collection(move |name| {
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.visible_library_for(&v).get(v.cursor.get()).cloned() else {
                return;
            };
            let clean = name.trim().to_string();
            let done = patch_library::open_quick().and_then(|db| {
                let track = db
                    .track_by_path(&item.url)
                    .ok_or_else(|| "it isn't a library track".to_string())?;
                let id = match db.collection_by_name(&clean) {
                    Some(id) => id,
                    None => db.create_collection(&clean)?,
                };
                db.add_to_collection(id, track)
            });
            *a.dj_status.borrow_mut() = match done {
                Ok(()) => format!("Added “{}” to “{clean}”", item.title),
                Err(e) => format!("Couldn't make “{clean}”: {e}"),
            };
            a.request_library(true);
            a.poll_dj();
        });
        // Files picked in the desktop's chooser: into the library where they
        // are, and into the collection shown; analysed in the background.
        let v = view.clone();
        d.on_lib_add_files(move || {
            let collection = v.collection_view.get();
            std::thread::spawn(move || {
                let say = |m: String| {
                    if let Ok(mut n) = FOLDER_NOTE.lock() {
                        *n = Some(m);
                    }
                };
                let files = match crate::portal::pick_files("Add music: choose audio files") {
                    Ok(f) if f.is_empty() => return,
                    Ok(f) => f,
                    Err(e) => return say(format!("{e}. From a terminal: omarchy-patch library collection add <name> <files>")),
                };
                let db = match patch_library::open_default() {
                    Ok(db) => db,
                    Err(e) => return say(format!("Couldn't open the library: {e}")),
                };
                let into = collection.and_then(|id| {
                    db.collections().into_iter().find(|(c, ..)| *c == id).map(|(_, n, _)| n)
                });
                say(match db.import_files(&files, collection) {
                    Ok((ids, skipped)) => {
                        if let Ok(exe) = std::env::current_exe() {
                            patch_engine::librarian::ensure(&exe);
                        }
                        let n = ids.len();
                        let mut m = format!(
                            "Added {n} {}{}, analysed in the background",
                            if n == 1 { "track" } else { "tracks" },
                            into.map(|c| format!(" to “{c}”")).unwrap_or_default()
                        );
                        if skipped > 0 {
                            m += &format!(" · {skipped} skipped (not audio, or unreadable)");
                        }
                        m
                    }
                    Err(e) => format!("Couldn't add the files: {e}"),
                });
            });
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_select(move |id| {
            v.collection.set((id >= 0).then_some(id as i64));
            if let Some(a) = me.upgrade() {
                a.rebuild_library(&v);
            }
        });
        let me = self.me.clone();
        d.on_collection_activate(move |id, active| {
            if let Some(a) = me.upgrade() {
                let mut inactive = a.lib_inactive.borrow_mut();
                if active {
                    inactive.remove(&(id as i64));
                } else {
                    inactive.insert(id as i64);
                }
                if !a.frozen {
                    dj::save_inactive_collections(&inactive);
                }
                drop(inactive);
                a.refresh_libraries();
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_view(move |id| {
            v.collection_view.set((id >= 0).then_some(id as i64));
            v.collection.set((id >= 0).then_some(id as i64));
            v.location.borrow_mut().clear();
            v.cursor.set(0);
            if let Some(a) = me.upgrade() {
                a.rebuild_library(&v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_browse(move |path| {
            *v.location.borrow_mut() = path.to_string();
            if !path.is_empty() {
                v.collection_view.set(None);
                v.collection.set(None);
            }
            v.cursor.set(0);
            if let Some(a) = me.upgrade() {
                a.rebuild_library(&v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_remove_source(move |path| {
            let Some(a) = me.upgrade() else { return };
            let note = match patch_library::open_quick()
                .and_then(|db| db.remove_source(path.as_str()))
            {
                Ok(source) => {
                    let loc = v.location.borrow().clone();
                    let root = source.path.trim_end_matches('/').to_string();
                    let prefix = format!("{root}/");
                    if loc.trim_end_matches('/') == root || loc.starts_with(&prefix) {
                        v.location.borrow_mut().clear();
                    }
                    patch_engine::librarian::request_rescan();
                    a.request_library(true);
                    format!(
                        "“{}” left the library. Its files stay on disk, and its analyses are kept.",
                        source.name
                    )
                }
                Err(e) => e,
            };
            *a.dj_status.borrow_mut() = note;
            a.rebuild_library(&v);
            a.poll_dj();
        });
        let me = self.me.clone();
        d.on_lib_collect_folder(move |path| {
            let dir = path.to_string();
            if dir.is_empty() {
                return;
            }
            if let Some(a) = me.upgrade() {
                *a.dj_status.borrow_mut() = "Making a collection of this folder…".into();
                a.poll_dj();
            }
            std::thread::spawn(move || super::add_folder(&dir));
        });
        let (me, v) = (self.me.clone(), view.clone());
        let weak = w.as_weak();
        d.on_lib_edit(move || {
            let Some(a) = me.upgrade() else { return };
            let Some(path) = a.visible_library_for(&v).get(v.cursor.get()).map(|r| r.url.clone()) else { return };
            let result = patch_library::open_quick().and_then(|db| {
                let track = db.tracks()?.into_iter().find(|t| t.path == path).ok_or_else(|| "track no longer exists".to_string())?;
                let year = db.years().get(&path).and_then(|(original, release)| original.or(*release)).unwrap_or(0);
                Ok((track, year))
            });
            match result {
                Ok((track, year)) => if let Some(w) = weak.upgrade() {
                    let d = w.global::<DjState>();
                    d.set_lib_edit_path(path.into());
                    d.set_lib_edit_title(track.title.into());
                    d.set_lib_edit_artist(track.artist.into());
                    d.set_lib_edit_year(if year > 0 { year.to_string() } else { String::new() }.into());
                    d.set_lib_edit_genre(track.genre.into());
                    d.set_lib_edit_open(true);
                },
                Err(e) => { *a.dj_status.borrow_mut() = format!("Couldn't edit track tags: {e}"); a.poll_dj(); }
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        let weak = w.as_weak();
        d.on_lib_edit_save(move |title, artist, year, genre| {
            let Some(a) = me.upgrade() else { return };
            let Some(w) = weak.upgrade() else { return };
            let d = w.global::<DjState>();
            let path = d.get_lib_edit_path().to_string();
            let year_text = year.trim();
            let year = if year_text.is_empty() {
                None
            } else {
                match year_text.parse::<u16>() {
                    Ok(y) if (1000..=2999).contains(&y) => Some(y),
                    _ => {
                        *a.dj_status.borrow_mut() = "Enter a year from 1000 to 2999, or leave it blank".into();
                        a.poll_dj();
                        return;
                    }
                }
            };
            let edit = patch_library::TrackEdit {
                title: Some(title.to_string()), artist: Some(artist.to_string()),
                year, genre: Some(genre.to_string()),
            };
            let result = patch_library::open_quick().and_then(|db| db.set_track_edit(&path, &edit));
            match result {
                Ok(()) => {
                    d.set_lib_edit_open(false);
                    *a.dj_status.borrow_mut() = "Patch tags saved (audio files unchanged)".into();
                    a.request_library(true);
                    a.rebuild_library(&v);
                }
                Err(e) => *a.dj_status.borrow_mut() = format!("Couldn't save Patch tags: {e}"),
            }
            a.poll_dj();
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_scrolled(move |first, n| {
            v.first.set(first.max(0) as usize);
            v.shown.set(n.clamp(1, 400) as usize);
            if let Some(a) = me.upgrade() {
                a.fill_library_slice(&v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_favourite(move |i| {
            let Some(a) = me.upgrade() else { return };
            let url = a.visible_library_for(&v).get(i.max(0) as usize).map(|r| r.url.clone());
            if let Some(url) = url {
                a.toggle_favourite(&url);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_favourites_first_toggle(move || {
            let Some(a) = me.upgrade() else { return };
            let url = a.visible_library_for(&v).get(v.cursor.get()).map(|r| r.url.clone());
            v.favourites_first.set(!v.favourites_first.get());
            a.keep_cursor_on(&v, url.as_deref());
            a.rebuild_library(&v);
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_set_state(move |st| {
            *v.state.borrow_mut() = st.to_string();
            v.cursor.set(0);
            if let Some(a) = me.upgrade() {
                a.rebuild_library(&v);
            }
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_lib_smart_toggle(move |which| {
            let which = which.to_string();
            match which.as_str() {
                "fits" => v.fits_only.set(!v.fits_only.get()),
                "fav" => v.fav_only.set(!v.fav_only.get()),
                "straight" => v.straight_only.set(!v.straight_only.get()),
                "session" | "hour" | "earlier" | "unplayed" => {
                    let on = v.played.borrow().as_str() == which;
                    *v.played.borrow_mut() = if on { String::new() } else { which };
                }
                "clear" => {
                    *v.state.borrow_mut() = "all".into();
                    *v.played.borrow_mut() = String::new();
                    v.fits_only.set(false);
                    v.fav_only.set(false);
                    v.straight_only.set(false);
                }
                _ => return,
            }
            v.cursor.set(0);
            if let Some(a) = me.upgrade() {
                a.rebuild_library(&v);
            }
        });
        let me = self.me.clone();
        d.on_lib_add_drive(move |path, label| {
            let Some(a) = me.upgrade() else { return };
            let note = match patch_library::open_quick().and_then(|db| db.add_source(std::path::Path::new(path.as_str()), label.as_str())) {
                Ok(_) => {
                    patch_engine::librarian::request_rescan();
                    if let Ok(exe) = std::env::current_exe() {
                        patch_engine::librarian::ensure(&exe);
                    }
                    format!("{label} is in the library: its music is found and analysed in the background")
                }
                Err(e) => e,
            };
            *a.toast.borrow_mut() = note;
            a.push_toast();
            a.lib_status_at.set(None);
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_add(move || {
            let Some(collection) = v.collection.get() else {
                return;
            };
            let Some(a) = me.upgrade() else { return };
            let Some(item) = a.visible_library_for(&v).get(v.cursor.get()).cloned() else {
                return;
            };
            if let Ok(db) = patch_library::open_quick()
                && let Some(track) = db.track_by_path(&item.url)
            {
                let _ = db.add_to_collection(collection, track);
                a.request_library(true);
                a.refresh_libraries();
            }
        });
        let me = self.me.clone();
        d.on_lib_rescan(move || {
            patch_engine::librarian::request_rescan();
            if let Some(a) = me.upgrade() {
                a.refresh_libraries();
            }
        });
        // Pick a folder in the desktop's own chooser (the portal: the
        // user's default dialog), and it becomes a collection named after
        // it, found and analysed in the background; the list shows it on
        // its next refresh.
        let me = self.me.clone();
        d.on_collection_choose(move |open| {
            let Some(a) = me.upgrade() else { return };
            for w in a.dj_windows() {
                w.global::<DjState>().set_folder_picking(open);
            }
        });
        let me = self.me.clone();
        d.on_collection_add_folder(move |path| {
            if let Some(a) = me.upgrade() {
                for w in a.dj_windows() {
                    w.global::<DjState>().set_folder_picking(false);
                }
            }
            let dir = path.to_string();
            std::thread::spawn(move || add_folder(&dir));
        });
        d.on_collection_from_folder(move || {
            std::thread::spawn(|| {
                // Said in the Library's own status line too: a desktop
                // notification alone was easy to miss ("nothing happened").
                let say = |m: String| {
                    notify("Patch Library", &m);
                    if let Ok(mut n) = FOLDER_NOTE.lock() {
                        *n = Some(m);
                    }
                };
                let dir = match crate::portal::pick_folder("Add a folder: open it, then choose it") {
                    Ok(Some(p)) => p.display().to_string(),
                    Ok(None) => return,
                    Err(e) => {
                        say(format!("{e}. From a terminal: omarchy-patch library collection create <name> <folder>"));
                        return;
                    }
                };
                let db = match patch_library::open_default() {
                    Ok(db) => db,
                    Err(e) => return say(format!("Couldn't open the library: {e}")),
                };
                // A chooser hands back the folder it shows when one inside
                // it was only highlighted (Omarchy's does): that's a source
                // already, and re-adding it changed nothing, silently.
                let dir = dir.trim_end_matches('/').to_string();
                if let Some(s) = db.sources().into_iter().find(|s| s.path == dir) {
                    return say(format!("{dir} is already in the library as “{}”. For a folder inside it: open that folder in the chooser (or mark it with Space), then choose it.", s.name));
                }
                drop(db);
                // A whole drive (or home, or Music) is not what "a folder"
                // meant: say so and add nothing.
                if too_wide(&dir) {
                    let name = std::path::Path::new(&dir)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dir.clone());
                    return say(format!("The chooser gave all of “{name}”, so nothing was added. New collection → From a folder…, then open the folder you mean before choosing it."));
                }
                add_folder(&dir);
            });
        });
        let (me, v) = (self.me.clone(), view.clone());
        d.on_collection_start_set(move |id| {
            if let Some(a) = me.upgrade() {
                let Some(item) = a.visible_library_for(&v).get(v.cursor.get()).cloned() else {
                    *a.dj_status.borrow_mut() = "Select a track in this collection to start Auto Mix".into();
                    a.poll_dj();
                    return;
                };
                let cmd = Command::DjSet {
                    on: true,
                    minutes: None,
                    arc: None,
                    bpm: None,
                    crate_dir: None,
                    // -1: the whole library.
                    collection: (id >= 0).then_some(id as i64),
                    opener: Some(item.url.clone()),
                };
                // Said at once, in the Library's status line (a press that
                // seemed to do nothing was pressed twice).
                let name = if id >= 0 { a.lib_collections.borrow().iter().find(|c| c.0 == id as i64).map(|c| format!("“{}”", c.1)).unwrap_or_else(|| "this collection".into()) } else { "all tracks".into() };
                *a.dj_status.borrow_mut() = match a.command(cmd, Origin::User) {
                    Ok(()) => format!("Auto Mix set selected: {name} · next track: {} (an audible transition finishes first)", item.title),
                    Err(e) => format!("Couldn't start the set: {e}"),
                };
                a.poll_dj();
            }
        });
    }

    /// Deck controls: power, tempo, keylock, key, nudge, sync, EQ and filter, loops, jumps, slip, quantize, keys, cue and phones, hot cues.
    fn bind_dj_decks(&self, w: &PatchWindow) {
        let d = w.global::<DjState>();
        let me = self.me.clone();
        d.on_power(move || {
            if let Some(a) = me.upgrade() {
                if a.is_live() && a.decks.iter().any(|d| d.alive()) {
                    // Never cut live audio by accident: warn, stop on a second press.
                    let live = a.live_decks();
                    let confirmed = a
                        .stop_armed
                        .get()
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                    if !live.is_empty() && !confirmed {
                        a.stop_armed.set(Some(Instant::now()));
                        *a.dj_status.borrow_mut() = format!(
                            "{} still playing · press again within 5 s to stop anyway",
                            decks_text(&live)
                        );
                    } else {
                        a.stop_armed.set(None);
                        a.stop_dj();
                        *a.dj_status.borrow_mut() =
                            "DJ booth stopped · other audio untouched".into();
                    }
                } else if let Err(e) = a.start_dj() {
                    *a.dj_status.borrow_mut() = format!("Couldn't start the DJ engine: {e}");
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_tempo(move |id, v| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| p.tempo = v as f64 * p.range);
            }
        });
        let me = self.me.clone();
        d.on_tempo_step(move |id, pct| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| {
                    p.tempo = (p.tempo + pct as f64 / 100.0).clamp(-p.range, p.range)
                });
            }
        });
        let me = self.me.clone();
        d.on_reset_tempo(move |id| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| p.tempo = 0.0);
            }
        });
        let me = self.me.clone();
        d.on_cycle_range(move |id| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| {
                    p.range = p.next_range();
                    p.tempo = p.tempo.clamp(-p.range, p.range);
                });
            }
        });
        let me = self.me.clone();
        d.on_toggle_keylock(move |id| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| p.keylock = !p.keylock);
            }
        });
        let me = self.me.clone();
        d.on_shift_key(move |id, n| {
            if let Some(a) = me.upgrade() {
                a.with_pitch(&id, |p| p.key_shift = (p.key_shift + n).clamp(-12, 12));
            }
        });
        let me = self.me.clone();
        d.on_nudge(move |id, b| {
            if let Some(a) = me.upgrade()
                && let Err(error) = a.command(Command::DeckBend { deck: [patch_core::command::Deck::A, patch_core::command::Deck::B, patch_core::command::Deck::C, patch_core::command::Deck::D][Self::deck_index(&id)], amount: b }, Origin::User)
            { *a.dj_status.borrow_mut() = error; }
        });
        let me = self.me.clone();
        d.on_sync(move |id| {
            if let Some(a) = me.upgrade() {
                let msg = match a.sync_deck(&id) {
                    Ok(m) => m,
                    Err(e) => e,
                };
                *a.dj_status.borrow_mut() = msg;
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_eq(move |id, band, v| {
            if let Some(a) = me.upgrade() {
                let slot = match band.as_str() {
                    "high" => 0,
                    "mid" => 1,
                    _ => 2,
                };
                a.set_eq_knob(Self::deck_index(&id), slot, v);
            }
        });
        let me = self.me.clone();
        d.on_set_deck_filter(move |id, v| {
            if let Some(a) = me.upgrade() {
                a.set_eq_knob(Self::deck_index(&id), 3, v);
            }
        });
        let me = self.me.clone();
        d.on_loop(move |id, beats| {
            if let Some(a) = me.upgrade() {
                let deck = [patch_core::command::Deck::A, patch_core::command::Deck::B, patch_core::command::Deck::C, patch_core::command::Deck::D][Self::deck_index(&id)];
                let command = if beats <= 0.0 {
                    Command::DeckReloop { deck }
                } else {
                    Command::DeckLoopToggle { deck, beats }
                };
                if let Err(e) = a.command(command, Origin::User) {
                    *a.dj_status.borrow_mut() = format!("Loop: {e}");
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_loop_in(move |id| {
            if let Some(a) = me.upgrade() {
                let deck = [patch_core::command::Deck::A, patch_core::command::Deck::B, patch_core::command::Deck::C, patch_core::command::Deck::D][Self::deck_index(&id)];
                if let Err(e) = a.command(Command::DeckLoopIn { deck }, Origin::User) {
                    *a.dj_status.borrow_mut() = format!("Loop in: {e}");
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_loop_out(move |id| {
            if let Some(a) = me.upgrade() {
                let deck = [patch_core::command::Deck::A, patch_core::command::Deck::B, patch_core::command::Deck::C, patch_core::command::Deck::D][Self::deck_index(&id)];
                if let Err(e) = a.command(Command::DeckLoopOut { deck }, Origin::User) {
                    *a.dj_status.borrow_mut() = format!("Loop out: {e}");
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_loop_scale(move |id, factor| {
            if let Some(a) = me.upgrade() {
                let deck = [patch_core::command::Deck::A, patch_core::command::Deck::B, patch_core::command::Deck::C, patch_core::command::Deck::D][Self::deck_index(&id)];
                if let Err(e) = a.command(Command::DeckLoopScale { deck, factor }, Origin::User) {
                    *a.dj_status.borrow_mut() = format!("Loop length: {e}");
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_jump(move |id, beats| {
            if let Some(a) = me.upgrade() {
                if a.deck(&id).status().native {
                    let _ = a
                        .deck(&id)
                        .deck_action("jump", serde_json::json!({"beats": beats}));
                } else {
                    a.deck(&id).seek_by(if beats < 0.0 { -10.0 } else { 10.0 });
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_toggle_slip(move |id| {
            if let Some(a) = me.upgrade() {
                let on = !a.deck(&id).status().slip;
                let _ = a
                    .deck(&id)
                    .deck_action("slip", serde_json::json!({"on": on}));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_toggle_quantize(move |id| {
            if let Some(a) = me.upgrade() {
                let on = !a.deck(&id).status().quantize;
                let _ = a
                    .deck(&id)
                    .deck_action("quantize", serde_json::json!({"on": on}));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_key(move |ctx, text, shift, ctrl, alt, down| {
            me.upgrade().is_some_and(|a| {
                a.dj_input(
                    &ctx,
                    patch_core::input::InputEvent::Key {
                        text: text.to_string(),
                        shift,
                        ctrl,
                        alt,
                        down,
                    },
                )
            })
        });
        let me = self.me.clone();
        d.on_toggle_pfl(move |id| {
            if let Some(a) = me.upgrade() {
                let i = Self::deck_index(&id);
                let mut p = a.pfl.get();
                p[i] = !p[i];
                a.pfl.set(p);
                a.send_cue(serde_json::json!({
                    "op": "pfl", "channel": (["deck-a", "deck-b", "deck-c", "deck-d"][i]), "on": p[i]
                }));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_cue_mix(move |v| {
            if let Some(a) = me.upgrade() {
                let (_, l) = a.cue_knobs.get();
                a.cue_knobs.set((v, l));
                a.send_cue(serde_json::json!({"op": "cue", "mix": (v + 1.0) / 2.0}));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_cue_level(move |v| {
            if let Some(a) = me.upgrade() {
                let (m, _) = a.cue_knobs.get();
                a.cue_knobs.set((m, v));
                a.send_cue(serde_json::json!({"op": "cue", "level_db": cue_level_db(v)}));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_cycle_phones(move || {
            if let Some(a) = me.upgrade() {
                let msg = a.cycle_phones();
                *a.dj_status.borrow_mut() = msg;
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_xf_assign(move |id, side| {
            if let Some(a) = me.upgrade() {
                let msg = a.set_xf(&format!("assign_{}", ["a", "b", "c", "d"][Self::deck_index(&id).min(3)]), &side);
                if !msg.is_empty() {
                    *a.dj_status.borrow_mut() = msg;
                }
                a.show_xfader();
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_xf_curve(move |curve| {
            if let Some(a) = me.upgrade() {
                let msg = a.set_xf("curve", &curve);
                if !msg.is_empty() {
                    *a.dj_status.borrow_mut() = msg;
                }
                a.show_xfader();
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_cycle_line(move |id| {
            if let Some(a) = me.upgrade() {
                let msg = a.cycle_line(Self::deck_index(&id));
                *a.dj_status.borrow_mut() = msg;
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_hot_cue(move |id, n, clear| {
            if let Some(a) = me.upgrade() {
                a.hot_cue(Self::deck_index(&id), n.clamp(0, dj::HOT_CUES as i32 - 1) as usize, clear);
            }
        });
        let me = self.me.clone();
        d.on_hot_cue_press(move |id, n, clear| {
            if let Some(a) = me.upgrade() {
                a.hot_cue_press(Self::deck_index(&id), n.clamp(0, dj::HOT_CUES as i32 - 1) as usize, clear);
            }
        });
        let me = self.me.clone();
        d.on_hot_cue_release(move |id| {
            if let Some(a) = me.upgrade() {
                a.hot_cue_release(Self::deck_index(&id));
            }
        });
    }

    /// The AI DJ and sets: light mode, set toggles, drum swaps, what to say, automix and the queue, the master, AI choices and plan shifts.
    fn bind_dj_ai_sets(&self, w: &PatchWindow) {
        let d = w.global::<DjState>();
        use patch_core::command::Deck as CDeck;
        let me = self.me.clone();
        d.on_ai(move |what| {
            if let Some(a) = me.upgrade() {
                let action = match what.as_str() {
                    "prepare" => AiAction::Prepare,
                    "run" => AiAction::Run,
                    "off" => AiAction::Off,
                    _ => AiAction::Suggest,
                };
                if let Err(e) = a.command(
                    Command::Ai {
                        action,
                        direction: None,
                    },
                    Origin::User,
                ) {
                    *a.dj_status.borrow_mut() = e;
                }
                a.poll_dj();
            }
        });
        // Light DJ: every action is a validated command on the playing deck.
        let me = self.me.clone();
        d.on_light(move |what| {
            if let Some(a) = me.upgrade() {
                let deck = if a.now_deck() == 1 {
                    CDeck::B
                } else {
                    CDeck::A
                };
                let cmd = match what.as_str() {
                    "play" => {
                        // Nothing loaded: start the queue.
                        if !a.decks[a.now_deck()].status().loaded {
                            let head = a.session_queue().into_iter().next();
                            match head {
                                Some(path) => {
                                    let _ = a.command(
                                        Command::DeckLoadPath {
                                            deck,
                                            path: path.clone(),
                                        },
                                        Origin::User,
                                    );
                                    let _ =
                                        a.command(Command::QueueRemove { index: 0 }, Origin::User);
                                    std::thread::sleep(Duration::from_millis(150));
                                }
                                None => {
                                    *a.dj_status.borrow_mut() =
                                        "Queue a track first (q in the Library or here)".into();
                                    a.poll_dj();
                                    return;
                                }
                            }
                        }
                        Command::DeckTransport {
                            deck,
                            action: Transport::Toggle,
                        }
                    }
                    "cue" => Command::DeckTransport {
                        deck,
                        action: Transport::Cue,
                    },
                    "loop" => Command::DeckLoopToggle { deck, beats: 4.0 },
                    "sync" => Command::DeckSync { deck },
                    _ => Command::TransitionNow,
                };
                if let Err(e) = a.command(cmd, Origin::User) {
                    *a.dj_status.borrow_mut() = e;
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_toggle(move || {
            if let Some(a) = me.upgrade() {
                let on = !a.light.borrow().set_on;
                let cmd = Command::DjSet {
                    on,
                    minutes: None,
                    arc: None,
                    bpm: None,
                    crate_dir: None,
                    collection: None,
                    opener: None,
                };
                if let Err(e) = a.command(cmd, Origin::User) {
                    a.light.borrow_mut().say_reply = e;
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_drum_swaps_toggle(move || {
            if let Some(a) = me.upgrade() {
                let on = !a.light.borrow().drum_swaps;
                match patch_engine::client::call(
                    &serde_json::json!({"op": "set_option", "drum_swaps": on}),
                ) {
                    Ok(_) => a.light.borrow_mut().drum_swaps = on,
                    Err(e) => a.light.borrow_mut().say_reply = e,
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_say(move |text| {
            if let Some(a) = me.upgrade() {
                let reply = if a.is_live() {
                    match patch_engine::client::call(
                        &serde_json::json!({"op": "say", "text": text.as_str()}),
                    ) {
                        Ok(v) => v["done"]
                            .as_array()
                            .map(|d| {
                                d.iter()
                                    .filter_map(|x| x.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            })
                            .unwrap_or_default(),
                        Err(e) => e,
                    }
                } else {
                    "Auto Mix runs on the live engine".into()
                };
                a.light.borrow_mut().say_reply = reply;
                a.dj_synced.set(None);
                a.dj_changed.set(Some(Instant::now()));
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_automix_toggle(move || {
            if let Some(a) = me.upgrade() {
                let on = !a.light.borrow().automix;
                if let Err(e) = a.command(
                    Command::AutoMix {
                        on,
                        direction: None,
                    },
                    Origin::User,
                ) {
                    *a.dj_status.borrow_mut() = e;
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_automix_direct(move |dir| {
            if let Some(a) = me.upgrade() {
                let direction = serde_json::from_value(serde_json::json!(dir.as_str())).ok();
                let on = a.light.borrow().automix;
                let _ = a.command(Command::AutoMix { on, direction }, Origin::User);
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_queue_remove(move |i| {
            if let Some(a) = me.upgrade() {
                let _ = a.command(
                    Command::QueueRemove {
                        index: i.max(0) as usize,
                    },
                    Origin::User,
                );
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_queue_selected(move || {
            if let Some(a) = me.upgrade() {
                let item = a
                    .lib_selected
                    .borrow()
                    .clone()
                    .or_else(|| a.library.borrow().first().cloned());
                let msg = match item {
                    Some(it) if !it.url.contains("://") => match a.command(
                        Command::QueueAdd {
                            path: it.url.clone(),
                            at: None,
                        },
                        Origin::User,
                    ) {
                        Ok(()) => format!("Queued {}", it.title),
                        Err(e) => e,
                    },
                    Some(_) => "Streams can't be queued (they have no end to mix out of)".into(),
                    None => "Select a track in the Library".into(),
                };
                *a.dj_status.borrow_mut() = msg;
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_master(move |v| {
            if let Some(a) = me.upgrade() {
                if a.is_live() {
                    // The DJ Booth's own fader (it set the Patch mix's master:
                    // apps and mics went down with it). Through the provider,
                    // as the Mixer's DJ Booth strip sets it: one gain, both
                    // faders show it at once.
                    a.provider
                        .borrow_mut()
                        .set_gain("engine.dj-mixer", patch_engine::session::fader_db(v) as f32);
                    a.sync_strips();
                    a.dj_synced.set(None); a.dj_changed.set(Some(Instant::now()));
                }
                a.light.borrow_mut().master_db = patch_engine::session::fader_db(v);
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_ai_toggle_auto(move || {
            if let Some(a) = me.upgrade() {
                let on = !a.ai.borrow().auto;
                a.set_ai_auto(on);
                *a.dj_status.borrow_mut() = if on {
                    format!("Auto Mix auto: prepares the idle deck {:.0} s before the mix-out · the mix stays yours", ai_dj::AUTO_PREPARE_S)
                } else {
                    "Auto Mix auto off".into()
                };
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_ai_pick(move |i| {
            if let Some(a) = me.upgrade() {
                let msg = a.ai_pick(i.max(0) as usize).unwrap_or_else(|e| e);
                *a.dj_status.borrow_mut() = msg;
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_ai_direct(move |what| {
            if let Some(a) = me.upgrade() {
                let d = match what.as_str() {
                    "build" => patch_core::intent::Direction::Build,
                    "release" => patch_core::intent::Direction::Release,
                    _ => patch_core::intent::Direction::Hold,
                };
                a.set_ai_direction(d);
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_plan_shift(move |direction| {
            if let Some(a) = me.upgrade() {
                if a.plan_adjusting.replace(true) {
                    return;
                }
                if !a.plan_adjust_polls.request("plan_adjust", serde_json::json!({
                    "op": "plan_adjust", "direction": direction.as_str()
                })) {
                    a.plan_adjusting.set(false);
                    *a.dj_status.borrow_mut() = "Could not start transition review".into();
                    return;
                }
                for w in a.dj_windows() {
                    w.global::<DjState>().set_plan_adjusting(true);
                    w.global::<DjState>().set_plan_feedback("Finding suitable musical opportunities…".into());
                }
                *a.dj_status.borrow_mut() = "Reviewing musical alternatives…".into();
                a.light.borrow_mut().say_reply = "Reviewing musical alternatives…".into();
            }
        });
    }

    /// Beat FX: chains, kinds, beats, depth, rolls, the FX deck; the key guard.
    fn bind_dj_effects(&self, w: &PatchWindow) {
        let d = w.global::<DjState>();
        let fx_cmd = |me: std::rc::Weak<Self>, f: Box<dyn Fn(&Self) -> Command>| {
            move || {
                if let Some(a) = me.upgrade() {
                    let cmd = f(&a);
                    if let Err(e) = a.command(cmd, Origin::User) {
                        *a.dj_status.borrow_mut() = e;
                    }
                    a.poll_dj();
                }
            }
        };
        use patch_core::command::Deck as CDeck;
        let deck_of = |a: &Self| {
            [CDeck::A, CDeck::B, CDeck::C, CDeck::D][a.fx.get().deck]
        };
        let me = self.me.clone();
        d.on_fx_chain_toggle(move |k| {
            let (Some(a), Some(k)) = (me.upgrade(), patch_core::command::FxKind::parse(&k)) else {
                return;
            };
            let u = a.fx.get();
            let mut kinds = u.kinds();
            match kinds.iter().position(|&x| x == k) {
                Some(_) if kinds.len() == 1 => return,
                Some(i) => {
                    kinds.remove(i);
                }
                None if kinds.len() == 3 => return,
                None => kinds.push(k),
            }
            let _ = a.command(
                Command::DeckFxChain {
                    deck: deck_of(&a),
                    kinds,
                    on: u.on,
                    beats: None,
                    depth: None,
                },
                Origin::User,
            );
            a.poll_dj();
        });
        d.on_fx_toggle(fx_cmd(
            self.me.clone(),
            Box::new(move |a| Command::DeckFx {
                deck: deck_of(a),
                kind: None,
                on: Some(!a.fx.get().on),
                beats: None,
                depth: None,
            }),
        ));
        let me = self.me.clone();
        d.on_fx_kind_set(move |k| {
            if let Some(a) = me.upgrade() {
                let kind = patch_core::command::FxKind::parse(&k);
                let _ = a.command(
                    Command::DeckFx {
                        deck: deck_of(&a),
                        kind,
                        on: None,
                        beats: None,
                        depth: None,
                    },
                    Origin::User,
                );
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_fx_beats_set(move |b| {
            if let Some(a) = me.upgrade() {
                let _ = a.command(
                    Command::DeckFx {
                        deck: deck_of(&a),
                        kind: None,
                        on: None,
                        beats: Some(b),
                        depth: None,
                    },
                    Origin::User,
                );
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_fx_depth_set(move |v| {
            if let Some(a) = me.upgrade() {
                let _ = a.command(
                    Command::DeckFx {
                        deck: deck_of(&a),
                        kind: None,
                        on: None,
                        beats: None,
                        depth: Some(v.clamp(0.0, 1.0)),
                    },
                    Origin::User,
                );
                a.poll_dj();
            }
        });
        // Roll is momentary: pressed = roll on the FX deck; released = back
        // to whatever effect was set before (and its on/off state).
        let me = self.me.clone();
        d.on_fx_roll(move |beats, down| {
            if let Some(a) = me.upgrade() {
                let deck = deck_of(&a);
                if down {
                    a.fx_before.set(Some(a.fx.get()));
                    let _ = a.command(
                        Command::DeckFx {
                            deck,
                            kind: Some(patch_core::command::FxKind::Roll),
                            on: Some(true),
                            beats: Some(beats),
                            depth: Some(1.0),
                        },
                        Origin::User,
                    );
                } else if let Some(prev) = a.fx_before.take() {
                    let _ = a.command(
                        Command::DeckFxChain {
                            deck,
                            kinds: prev.kinds(),
                            on: prev.on,
                            beats: Some(prev.beats),
                            depth: Some(prev.depth),
                        },
                        Origin::User,
                    );
                }
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_fx_deck_set(move |id| {
            if let Some(a) = me.upgrade() {
                let deck = [CDeck::A, CDeck::B, CDeck::C, CDeck::D][Self::deck_index(&id)];
                let _ = a.command(
                    Command::DeckFx {
                        deck,
                        kind: None,
                        on: None,
                        beats: None,
                        depth: None,
                    },
                    Origin::User,
                );
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_key_guard(move |text| me.upgrade().is_none_or(|a| a.keys.allow(&text)));
    }

    /// The DJ mixer: scope style, auto-gain, crossfader and deck gains.
    fn bind_dj_mixer(&self, w: &PatchWindow) {
        let d = w.global::<DjState>();
        let me = self.me.clone();
        d.on_scope_style_next(move || {
            if let Some(a) = me.upgrade() {
                let next = a.scope_style.get().next();
                next.save();
                a.scope_style.set(next);
                a.render_scopes();
            }
        });
        let me = self.me.clone();
        d.on_deck_favourite(move |id| {
            if let Some(a) = me.upgrade() {
                let source = a.decks[Self::deck_index(&id)].snapshot().source;
                a.toggle_favourite(&source);
            }
        });
        let me = self.me.clone();
        d.on_toggle_auto_gain(move || {
            if let Some(a) = me.upgrade() {
                a.auto_gain.set(!a.auto_gain.get());
                a.apply_dj_gains();
                a.poll_dj();
            }
        });
        let me = self.me.clone();
        d.on_set_xfader(move |x| {
            if let Some(a) = me.upgrade() {
                // One engine call and the crossfader's own readouts: a drag
                // sends many of these, each must be instant (no full poll).
                a.xfader.set(x);
                a.apply_dj_gains();
                a.show_xfader();
            }
        });
        let me = self.me.clone();
        d.on_set_input_trim(move |id, value| {
            if let Some(a) = me.upgrade() {
                a.set_input_trim(Self::deck_index(&id), value);
            }
        });
        let me = self.me.clone();
        d.on_set_gain(move |id, g| {
            if let Some(a) = me.upgrade() {
                let mut gains = a.deck_gain.get();
                gains[Self::deck_index(&id)] = g;
                a.deck_gain.set(gains);
                a.apply_dj_gains();
                a.rebuild_strips();
                a.poll_dj();
            }
        });
    }
}
