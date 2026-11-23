//! The library in the DJ surfaces: rows, slices, status, rebuilding, loading a deck.

use super::*;

impl AudioApp {
    pub(super) fn deck(&self, id: &str) -> &Deck {
        &self.decks[Self::deck_index(id)]
    }

    pub(super) fn visible_library(&self, filter: &str) -> Vec<LibraryItem> {
        let f = filter.to_lowercase();
        self.library
            .borrow()
            .iter()
            .zip(self.library_keys.borrow().iter())
            .filter(|(_, (title, detail, path))| {
                f.is_empty() || title.contains(&f) || detail.contains(&f) || path.contains(&f)
            })
            .map(|(item, _)| item.clone())
            .collect()
    }

    /// The Library's rows: search, place, collection, analysis standing, and
    /// the smart filters (what the cursor and Load buttons index).
    pub(super) fn visible_library_for(&self, view: &LibView) -> Vec<LibraryItem> {
        let mut rows = self.library_rows(view);
        let want = view.state.borrow().clone();
        let played = view.played.borrow().clone();
        let fits_only = view.fits_only.get();
        let fav_only = view.fav_only.get();
        let straight_only = view.straight_only.get();
        if want != "all" || !played.is_empty() || fits_only || fav_only || straight_only {
            let reference = self.playing_reference();
            rows.retain(|r| {
                keep_library_row(
                    &want,
                    &played,
                    fits_only,
                    fav_only,
                    straight_only,
                    &self.row_facts(r, reference.as_ref()),
                )
            });
        }
        // A set prep keeps its own order.
        if view.favourites_first.get() && !self.viewing_ordered(view) {
            let favourites = self.lib_favourites.borrow();
            rows.sort_by_key(|r| !favourites.contains(&r.url));
        }
        rows
    }

    /// Mark a track as a favourite, or take the mark off. Every Library keeps
    /// its highlighted track when the sort moves rows around it.
    pub(super) fn toggle_favourite(&self, url: &str) {
        if self.frozen {
            return;
        }
        let on = !self.lib_favourites.borrow().contains(url);
        let done = patch_library::open_quick().and_then(|db| {
            let track = db
                .track_by_path(url)
                .ok_or_else(|| "it isn't a library track".to_string())?;
            db.set_favourite(track, on)
        });
        if let Err(e) = done {
            *self.dj_status.borrow_mut() = format!("Couldn't change the favourite: {e}");
            self.poll_dj();
            return;
        }
        let views: Vec<Rc<LibView>> = self
            .lib_views
            .borrow()
            .iter()
            .filter_map(|v| v.upgrade())
            .collect();
        let highlighted: Vec<Option<String>> = views
            .iter()
            .map(|v| {
                self.visible_library_for(v)
                    .get(v.cursor.get())
                    .map(|r| r.url.clone())
            })
            .collect();
        // Shown at once; the library's own reload confirms it.
        {
            let mut favourites = self.lib_favourites.borrow_mut();
            if on {
                favourites.insert(url.to_string());
            } else {
                favourites.remove(url);
            }
        }
        for (view, url) in views.iter().zip(highlighted) {
            self.keep_cursor_on(view, url.as_deref());
        }
        self.refresh_libraries();
        self.request_library(true);
        self.poll_dj();
    }

    /// The cursor back on this track after the rows moved.
    pub(super) fn keep_cursor_on(&self, view: &LibView, url: Option<&str>) {
        if let Some(i) = url.and_then(|u| {
            self.visible_library_for(view)
                .iter()
                .position(|r| r.url == u)
        }) {
            view.cursor.set(i);
        }
    }

    /// The view shows a set prep.
    pub(super) fn viewing_set(&self, view: &LibView) -> bool {
        view.collection_view
            .get()
            .is_some_and(|id| self.lib_sets.borrow().contains(&id))
    }

    /// The view shows a collection in its listed order (a set prep, or one
    /// played in order): its tracks move with ⇧↑ ⇧↓.
    pub(super) fn viewing_ordered(&self, view: &LibView) -> bool {
        view.collection_view
            .get()
            .is_some_and(|id| self.lib_set_order.borrow().contains_key(&id))
    }

    /// Whether the AI DJ plays a collection in its listed order; else (the
    /// default, a set prep too) it decides the order.
    pub(super) fn plays_in_order(&self, id: i64) -> bool {
        self.lib_play_order.borrow().get(&id).copied().unwrap_or(false)
    }

    /// The highlighted track, from the rows the window shows (no filtering
    /// of the whole library).
    pub(super) fn cursor_item(&self, view: &LibView) -> Option<LibraryItem> {
        let url = view.urls.borrow().get(view.cursor.get()).cloned()?;
        self.library.borrow().iter().find(|i| i.url == url).cloned()
    }

    /// Search and collection only (the status filter's counts).
    pub(super) fn library_rows(&self, view: &LibView) -> Vec<LibraryItem> {
        let mut rows = self.visible_library(&view.filter.borrow());
        if let Some(collection) = view.collection_view.get() {
            let members = self.lib_members.borrow();
            rows.retain(|row| {
                members
                    .get(&collection)
                    .is_some_and(|paths| paths.contains(&row.url))
            });
            // A set prep, or a collection played in order: its listed order.
            if let Some(order) = self.lib_set_order.borrow().get(&collection) {
                let at: HashMap<&str, usize> =
                    order.iter().enumerate().map(|(i, u)| (u.as_str(), i)).collect();
                rows.sort_by_key(|r| at.get(r.url.as_str()).copied().unwrap_or(usize::MAX));
            }
        }
        let location = view.location.borrow();
        if !location.is_empty() {
            rows.retain(|row| crate::library_place::under(&row.url, &location));
        }
        rows
    }

    /// The rows in view into the window's model (in place: only changed
    /// rows are touched), from `first` for `shown` rows.
    pub(super) fn fill_library_slice(&self, view: &LibView) {
        let all = view.all.borrow();
        let first = view.first.get().min(all.len());
        let rows = &all[first..(first + view.shown.get()).min(all.len())];
        if view.model.row_count() == rows.len() {
            for (i, r) in rows.iter().enumerate() {
                if view.model.row_data(i).as_ref() != Some(r) {
                    view.model.set_row_data(i, r.clone());
                }
            }
        } else {
            view.model.set_vec(rows.to_vec());
        }
        if let Some(w) = view.window.upgrade() {
            w.global::<DjState>().set_lib_first(first as i32);
        }
    }

    pub(super) fn request_library(&self, force: bool) {
        if !self.frozen {
            let details = self.club_library_needed()
                || self
                    .lib_views
                    .borrow()
                    .iter()
                    .any(|v| v.upgrade().is_some());
            self.library_loader.request(force, details);
        }
    }

    /// Consume worker snapshots without touching disk or waiting on I/O.
    pub(super) fn poll_library(&self) {
        let has_views = self.club_library_needed()
            || self
                .lib_views
                .borrow()
                .iter()
                .any(|v| v.upgrade().is_some());
        let cadence = Duration::from_secs(if has_views { 1 } else { 3 });
        if !self.frozen
            && self
                .lib_status_at
                .get()
                .is_none_or(|at| at.elapsed() >= cadence)
        {
            self.lib_status_at.set(Some(Instant::now()));
            self.request_library(false);
        }
        if let Some(mut update) = self.library_loader.take() {
            let changed = update.items.is_some()
                || update.ready.is_some()
                || update.collections.is_some()
                || update.briefs.is_some();
            if let Some(items) = update.items.take() {
                *self.lib_favourites.borrow_mut() = dj::favourite_paths(&items.items);
                *self.library_keys.borrow_mut() = items.keys;
                *self.library.borrow_mut() = items.items;
            }
            if let Some(ready) = update.ready.take() {
                *self.lib_ready.borrow_mut() = ready;
            }
            if let Some(briefs) = update.briefs.take() {
                *self.library_briefs.borrow_mut() = briefs;
            }
            if let Some(collections) = update.collections.take() {
                *self.lib_collections.borrow_mut() = collections;
            }
            if let Some(members) = update.members.take() {
                *self.lib_members.borrow_mut() = members;
            }
            if let Some((sets, order, plays)) = update.sets.take() {
                *self.lib_sets.borrow_mut() = sets;
                *self.lib_set_order.borrow_mut() = order;
                *self.lib_play_order.borrow_mut() = plays;
            }
            *self.lib_status.borrow_mut() = update.status;
            if changed {
                self.refresh_libraries();
            }
        }
        // A preview playing: its position, a few times a second; and its
        // end (the track's, a minute paused, the Library window closed).
        if !self.preview.borrow().url.is_empty() {
            self.previewer.send(crate::player::Action::Poll);
            self.preview_housekeeping();
        }
        for v in self.lib_views.borrow().iter().filter_map(|v| v.upgrade()) {
            if let Some(w) = v.window.upgrade() {
                let d = w.global::<DjState>();
                self.push_library_status(&d);
                self.push_set_prep(&d, &v);
            }
        }
    }

    /// Sources, activity and drives to add, into one Library window.
    pub(super) fn push_library_status(&self, d: &DjState) {
        let st = self.lib_status.borrow();
        let collections = self.lib_collections.borrow();
        let home = std::env::var("HOME").unwrap_or_default();
        let (sources, activity, busy, offers) = match st.as_ref() {
            Some(s) => (
                s.sources
                    .iter()
                    .map(|x| LibSource {
                        name: x.name.as_str().into(),
                        place: if !home.is_empty() && x.path.starts_with(&home) {
                            format!("~{}", &x.path[home.len()..])
                        } else {
                            x.path.clone()
                        }
                        .into(),
                        path: x.path.as_str().into(),
                        connected: x.connected,
                        tracks: x.counts.tracks as i32,
                        ready: x.counts.ready as i32,
                        check: x.counts.check as i32,
                        waiting: x.counts.waiting as i32,
                        failed: x.counts.failed as i32,
                        collection: collections
                            .iter()
                            .find(|(_, n, _)| *n == x.name)
                            .map(|(id, ..)| *id as i32)
                            .unwrap_or(-1),
                    })
                    .collect::<Vec<_>>(),
                match (&s.scanning, &s.analysing) {
                    (Some(src), _) => format!("looking for new music in {src}…"),
                    (None, Some(t)) => format!(
                        "analysing “{t}” · {} waiting{}",
                        s.queue,
                        match crate::library_eta(s.queue, s.per_track_s) {
                            e if e.is_empty() => String::new(),
                            e => format!(" · {e} left"),
                        }
                    ),
                    (None, None) if s.grids_left > 0 => format!(
                        "measuring beat grids: {} left{}",
                        s.grids_left,
                        match crate::library_eta(s.grids_left, 0.5) {
                            e if e.is_empty() => String::new(),
                            e => format!(" · {e}"),
                        }
                    ),
                    (None, None) if s.vocals_left > 0 => format!(
                        "finding where tracks sing: {} left{}",
                        s.vocals_left,
                        match crate::library_eta(s.vocals_left, 7.0) {
                            e if e.is_empty() => String::new(),
                            e => format!(" · {e}"),
                        }
                    ),
                    (None, None) if s.years_left > 0 => format!(
                        "looking up years and genres: {} left{}",
                        s.years_left,
                        match crate::library_eta(s.years_left, 2.5) {
                            e if e.is_empty() => String::new(),
                            e => format!(" · {e}"),
                        }
                    ),
                    (None, None) => "everything that can be analysed is analysed".into(),
                },
                s.scanning.is_some()
                    || s.analysing.is_some()
                    || s.grids_left > 0
                    || s.vocals_left > 0
                    || s.years_left > 0,
                s.offers
                    .iter()
                    .map(|o| LibOffer {
                        path: o.path.as_str().into(),
                        label: o.label.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            ),
            None => (
                vec![],
                "the librarian isn't running: it starts with the Patch service".to_string(),
                false,
                vec![],
            ),
        };
        let same = |m: ModelRc<LibSource>| {
            m.row_count() == sources.len()
                && sources
                    .iter()
                    .enumerate()
                    .all(|(i, x)| m.row_data(i).as_ref() == Some(x))
        };
        if !same(d.get_lib_sources()) {
            d.set_lib_sources(Rc::new(VecModel::from(sources)).into());
        }
        let summary = match st.as_ref() {
            Some(s) => {
                let on: Vec<_> = s.sources.iter().filter(|x| x.connected).collect();
                let (t, r, c) = on.iter().fold((0, 0, 0), |a, x| {
                    (
                        a.0 + x.counts.tracks,
                        a.1 + x.counts.ready,
                        a.2 + x.counts.check,
                    )
                });
                let off: usize = s
                    .sources
                    .iter()
                    .filter(|x| !x.connected)
                    .map(|x| x.counts.tracks)
                    .sum();
                format!(
                    "{t} tracks · {r} ready{}{}",
                    if c > 0 {
                        format!(" · {c} to check")
                    } else {
                        String::new()
                    },
                    if off > 0 {
                        format!(" · {off} offline")
                    } else {
                        String::new()
                    }
                )
            }
            None => String::new(),
        };
        d.set_lib_summary(summary.into());
        d.set_lib_activity(activity.into());
        d.set_lib_busy(busy);
        let o = d.get_lib_offers();
        if o.row_count() != offers.len()
            || offers
                .iter()
                .enumerate()
                .any(|(i, x)| o.row_data(i).as_ref() != Some(x))
        {
            d.set_lib_offers(Rc::new(VecModel::from(offers)).into());
        }
    }

    pub(super) fn refresh_libraries(&self) {
        for view in self.club_views.borrow().iter().filter_map(Weak::upgrade) {
            view.invalidate();
        }
        self.lib_views
            .borrow_mut()
            .retain(|v| v.upgrade().is_some());
        let views: Vec<Rc<LibView>> = self
            .lib_views
            .borrow()
            .iter()
            .filter_map(|v| v.upgrade())
            .collect();
        for v in views {
            self.rebuild_library(&v);
        }
    }

    /// Key and tempo of the deck that plays (the reference for "fits").
    pub(super) fn playing_reference(&self) -> Option<(patch_analysis::key::Key, f64)> {
        for i in 0..4 {
            let st = if self.frozen {
                dj::DeckStatus::demo(i)
            } else if self.is_live() {
                self.decks[i].snapshot()
            } else {
                continue;
            };
            if st.loaded && !st.paused {
                let a = self.deck_analysis(i, &st)?;
                let p = self.pitch.borrow()[i];
                return Some((
                    a.key?.shifted(p.semitones().round() as i32),
                    a.bpm? * p.speed(),
                ));
            }
        }
        None
    }

    pub(super) fn rebuild_library(&self, view: &LibView) {
        let visible = self.visible_library_for(view);
        // A track taken out of the collection shown: the next one is highlighted.
        if !visible.is_empty() && view.cursor.get() >= visible.len() {
            view.cursor.set(visible.len() - 1);
        }
        let cur = view.cursor.get();
        let reference = self.playing_reference();
        let favourites = self.lib_favourites.borrow();
        let home = std::env::var("HOME").unwrap_or_default();
        let source_pairs = self.source_pairs();
        let source_refs: Vec<(&str, &str)> = source_pairs
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_str()))
            .collect();
        let rows: Vec<LibItem> = visible
            .iter()
            .enumerate()
            .map(|(i, it)| {
                // Only the figures a row shows: never a copy of the whole
                // analysis (its waveform), per row, per rebuild.
                let an = self.library_brief(&it.url);
                let fits = brief_fits(an.as_ref(), reference.as_ref());
                let (state, why) = {
                    let ready = self.lib_ready.borrow();
                    let r = ready.get(&it.url);
                    // Streams and radio can't be analysed: no state at all.
                    let state = if it.url.contains("://") {
                        ""
                    } else {
                        state_name(r.map(|x| x.0))
                    };
                    let why = match (r.map(|x| x.0), &an) {
                        (Some(patch_library::Readiness::Failed), _) => format!(
                            "couldn't read it: {}",
                            patch_library::plain_error(
                                &r.and_then(|x| x.1.clone()).unwrap_or_default()
                            )
                        ),
                        (Some(patch_library::Readiness::Check), None) => {
                            "analysed with doubts: check it before mixing".into()
                        }
                        (Some(patch_library::Readiness::Check), Some(a)) => {
                            // Exactly why (the user, 2026-11-14): "check"
                            // is a tempo under 50 % sure, a missing key or
                            // downbeat, nothing vaguer.
                            let mut w: Vec<String> = vec![];
                            match a.bpm {
                                None => w.push("no tempo found".into()),
                                Some(_) if a.bpm_confidence < 0.5 => w.push(format!(
                                    "unsteady beat ({:.0} % sure)",
                                    a.bpm_confidence * 100.0
                                )),
                                _ => {}
                            }
                            if a.key.is_none() {
                                w.push("no key found".into());
                            }
                            if a.downbeat.is_none() {
                                w.push("no downbeat found".into());
                            }
                            if w.is_empty() {
                                "analysed with doubts: check it before mixing".into()
                            } else {
                                format!("{}: check the grid, or set it by hand", w.join(", "))
                            }
                        }
                        (Some(patch_library::Readiness::Soft), _) => {
                            "BPM confirmed by the file · soft beat: it mixes short (cuts, echoes, short blends)".into()
                        }
                        // Ready, but a loose beat: say what that means for mixing.
                        (Some(patch_library::Readiness::Ready), Some(a)) => {
                            a.grid.as_ref().map(|g| g.1.clone()).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    (state, why)
                };
                let placed = crate::library_place::track_place(&it.url, &source_refs, &home);
                LibItem {
                    year: if it.year > 0 { it.year.to_string().into() } else { "".into() },
                    year_re: if it.year_release > it.year && it.year > 0 {
                        it.year_release.to_string().into()
                    } else {
                        "".into()
                    },
                    favourite: favourites.contains(&it.url),
                    genre: it.genre.as_str().into(),
                    place: placed.short.into(),
                    path_line: placed.path_line.into(),
                    // Only what the library holds: not streams or cliamp's files.
                    can_favourite: self.frozen || (!it.url.contains("://") && it.kind != "file"),
                    played: it.played.as_str().into(),
                    played_at: it.played_at.as_str().into(),
                    state: state.into(),
                    why: why.into(),
                    title: it.title.as_str().into(),
                    kind: it.kind.as_str().into(),
                    detail: it.detail.as_str().into(),
                    selected: i == cur,
                    // "?" marks a tempo without a steady beat behind it.
                    bpm: an
                        .as_ref()
                        .and_then(|a| {
                            a.bpm.map(|b| {
                                format!("{b:.1}{}", if a.bpm_confidence < 0.5 { "?" } else { "" })
                            })
                        })
                        .unwrap_or_default()
                        .into(),
                    key: an
                        .as_ref()
                        .and_then(|a| a.key)
                        .map(|k| k.camelot())
                        .unwrap_or_default()
                        .into(),
                    lufs: an
                        .as_ref()
                        .filter(|a| a.lufs > -70.0)
                        .map(|a| format!("{:.1} LUFS", a.lufs))
                        .unwrap_or_default()
                        .into(),
                    fits,
                    grid: an
                        .as_ref()
                        .and_then(|a| a.grid.as_ref())
                        .map(|g| g.0.clone())
                        .unwrap_or_default()
                        .into(),
                }
            })
            .collect();
        drop(favourites);
        let count = rows.len();
        *view.all.borrow_mut() = rows;
        *view.urls.borrow_mut() = visible.iter().map(|r| r.url.clone()).collect();
        if let Some(w) = view.window.upgrade() {
            w.global::<DjState>().set_lib_count(count as i32);
        }
        self.fill_library_slice(view);
        if let Some(w) = view.window.upgrade() {
            let d = w.global::<DjState>();
            d.set_lib_cursor(cur as i32);
            // How many tracks stand where, and how the smart filters would cut
            // this place (search, folder, collection), before they combine.
            let scope = self.library_rows(view);
            let reference = self.playing_reference();
            let mut fits_n = 0i32;
            let mut fav_n = 0i32;
            let mut straight_n = 0i32;
            let mut unplayed_n = 0i32;
            let mut session_n = 0i32;
            let mut hour_n = 0i32;
            let mut earlier_n = 0i32;
            let mut ready_n = 0i32;
            let mut check_n = 0i32;
            let mut soft_n = 0i32;
            let mut waiting_n = 0i32;
            let mut failed_n = 0i32;
            for row in &scope {
                let facts = self.row_facts(row, reference.as_ref());
                if facts.fits {
                    fits_n += 1;
                }
                if facts.favourite {
                    fav_n += 1;
                }
                if facts.straight {
                    straight_n += 1;
                }
                if facts.played == "Unplayed" {
                    unplayed_n += 1;
                }
                if facts.played.starts_with("This session") {
                    session_n += 1;
                }
                if facts.played.ends_with("Last hour") {
                    hour_n += 1;
                }
                if played_matches("earlier", facts.played) {
                    earlier_n += 1;
                }
                match facts.state {
                    "ready" => ready_n += 1,
                    "check" => check_n += 1,
                    "soft" => soft_n += 1,
                    "waiting" => waiting_n += 1,
                    "failed" => failed_n += 1,
                    _ => {}
                }
            }
            let counts = vec![scope.len() as i32, ready_n, check_n, waiting_n, failed_n, soft_n];
            if d.get_lib_state_counts().iter().collect::<Vec<_>>() != counts {
                d.set_lib_state_counts(Rc::new(VecModel::from(counts)).into());
            }
            d.set_lib_state(view.state.borrow().as_str().into());
            d.set_lib_favourites_first(view.favourites_first.get());
            d.set_lib_smart_fits(view.fits_only.get());
            d.set_lib_smart_fav(view.fav_only.get());
            d.set_lib_smart_straight(view.straight_only.get());
            d.set_lib_played(view.played.borrow().as_str().into());
            d.set_lib_fits_live(reference.is_some());
            d.set_lib_fits_n(fits_n);
            d.set_lib_fav_n(fav_n);
            d.set_lib_straight_n(straight_n);
            d.set_lib_unplayed_n(unplayed_n);
            d.set_lib_session_n(session_n);
            d.set_lib_hour_n(hour_n);
            d.set_lib_earlier_n(earlier_n);
            d.set_lib_total(self.library.borrow().len() as i32);
            let bits = active_filters(
                view.state.borrow().as_str(),
                view.played.borrow().as_str(),
                view.fits_only.get(),
                view.fav_only.get(),
                view.straight_only.get(),
            );
            d.set_lib_view_line(view_line(count, scope.len(), &bits).into());
            self.push_library_status(&d);
            let inactive = self.lib_inactive.borrow();
            let members = self.lib_members.borrow();
            let collections: Vec<CollectionItem> = self
                .lib_collections
                .borrow()
                .iter()
                .cloned()
                .map(|(id, name, count)| CollectionItem {
                    id: id as i32,
                    name: name.into(),
                    count: count as i32,
                    active: !inactive.contains(&id),
                    place: members
                        .get(&id)
                        .map(|paths| {
                            crate::library_place::shared_place(
                                paths.iter().map(String::as_str),
                                &source_refs,
                                &home,
                            )
                        })
                        .unwrap_or_default()
                        .into(),
                })
                .collect();
            drop(inactive);
            // Set preps are their own list in the sidebar; the rest below.
            let (sets, plain): (Vec<CollectionItem>, Vec<CollectionItem>) = {
                let ids = self.lib_sets.borrow();
                collections.iter().cloned().partition(|c| ids.contains(&(c.id as i64)))
            };
            if self.viewing_set(view) {
                self.set_target.set(view.collection_view.get());
            }
            let now = d.get_collections();
            if now.row_count() != plain.len()
                || plain
                    .iter()
                    .enumerate()
                    .any(|(i, c)| now.row_data(i).as_ref() != Some(c))
            {
                d.set_collections(Rc::new(VecModel::from(plain.clone())).into());
            }
            let now = d.get_prep_sets();
            if now.row_count() != sets.len()
                || sets.iter().enumerate().any(|(i, c)| now.row_data(i).as_ref() != Some(c))
            {
                d.set_prep_sets(Rc::new(VecModel::from(sets)).into());
            }
            d.set_collection_selected(view.collection.get().map(|id| id as i32).unwrap_or(-1));
            d.set_collection_viewed(view.collection_view.get().map(|id| id as i32).unwrap_or(-1));
            let viewed = view
                .collection_view
                .get()
                .and_then(|id| collections.iter().find(|c| c.id as i64 == id));
            d.set_collection_viewed_name(viewed.map(|c| c.name.clone()).unwrap_or_default());
            d.set_collection_viewed_in_order(
                view.collection_view.get().is_some_and(|id| self.plays_in_order(id)),
            );
            // A library folder's own collection: where (its delete says so).
            let folder = viewed
                .and_then(|c| {
                    let st = self.lib_status.borrow();
                    st.as_ref()?
                        .sources
                        .iter()
                        .find(|s| c.name == s.name.as_str())
                        .map(|s| s.path.clone())
                })
                .map(|p| {
                    let home = std::env::var("HOME").unwrap_or_default();
                    match p.strip_prefix(&home) {
                        Some(rest) if !home.is_empty() => format!("~{rest}"),
                        _ => p,
                    }
                })
                .unwrap_or_default();
            d.set_collection_viewed_folder(folder.into());
            d.set_collection_viewed_place(viewed.map(|c| c.place.clone()).unwrap_or_default());
            // The highlighted track, and each collection ticked where it is in.
            drop(members);
            self.push_cursor_facts(&d, visible.get(cur), &plain, &source_refs, &home);
            d.set_collection_viewed_count(viewed.map(|c| c.count).unwrap_or(0));
            d.set_collection_viewed_active(viewed.is_none_or(|c| c.active));
            d.set_collections_active(plain.iter().filter(|c| c.active).count() as i32);
            // The place's own size, before the smart filters cut it.
            self.push_location(&d, view, &source_refs, &home, scope.len());
            self.push_set_prep(&d, view);
        }
    }

    /// The folders and drives the Library can browse by, as `(name, path)`.
    /// The cursor moved (j/k, arrows, a click): only what follows it changes,
    /// the two rows' highlight, the rows shown and the cursor's own facts. A
    /// full `rebuild_library` re-derived every row and every filter count
    /// (16 000 tracks here) on each key repeat.
    pub(super) fn move_library_cursor(&self, view: &LibView, from: usize) {
        let cur = view.cursor.get();
        {
            let mut all = view.all.borrow_mut();
            if all.len() != view.urls.borrow().len() {
                drop(all);
                return self.rebuild_library(view);
            }
            if let Some(row) = all.get_mut(from) {
                row.selected = false;
            }
            if let Some(row) = all.get_mut(cur) {
                row.selected = true;
            }
        }
        self.fill_library_slice(view);
        let Some(w) = view.window.upgrade() else {
            return;
        };
        let d = w.global::<DjState>();
        d.set_lib_cursor(cur as i32);
        let home = std::env::var("HOME").unwrap_or_default();
        let source_pairs = self.source_pairs();
        let source_refs: Vec<(&str, &str)> = source_pairs
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_str()))
            .collect();
        // The collections as the last rebuild listed them (their places
        // walk every member's path: not per key press).
        let shown = d.get_collections();
        let plain: Vec<CollectionItem> = (0..shown.row_count()).filter_map(|i| shown.row_data(i)).collect();
        let at = self.cursor_item(view);
        self.push_cursor_facts(&d, at.as_ref(), &plain, &source_refs, &home);
    }

    /// The highlighted track's title, place, path and last play, and each
    /// collection ticked where it is in.
    fn push_cursor_facts(
        &self,
        d: &DjState,
        at: Option<&LibraryItem>,
        plain: &[CollectionItem],
        source_refs: &[(&str, &str)],
        home: &str,
    ) {
        let members = self.lib_members.borrow();
        d.set_lib_cursor_title(at.map(|h| h.title.as_str()).unwrap_or("").into());
        let placed = at.map(|h| crate::library_place::track_place(&h.url, source_refs, home));
        d.set_lib_cursor_place(
            placed
                .as_ref()
                .map(|p| p.short.as_str())
                .unwrap_or("")
                .into(),
        );
        d.set_lib_cursor_path(
            placed
                .as_ref()
                .map(|p| p.path_line.as_str())
                .unwrap_or("")
                .into(),
        );
        let played_line = at
            .filter(|h| !h.played.is_empty() && h.played != "Unplayed")
            .map(|h| {
                if h.played_at.is_empty() {
                    h.played.as_str()
                } else {
                    h.played_at.as_str()
                }
            })
            .unwrap_or("");
        d.set_lib_cursor_played(played_line.into());
        let ticks: Vec<TrackCollection> = match at.filter(|h| !h.url.contains("://")) {
            Some(h) => plain
                .iter()
                .map(|c| TrackCollection {
                    id: c.id,
                    name: c.name.clone(),
                    place: c.place.clone(),
                    member: members
                        .get(&(c.id as i64))
                        .is_some_and(|p| p.contains(&h.url)),
                })
                .collect(),
            None => vec![],
        };
        let now = d.get_track_collections();
        if now.row_count() != ticks.len()
            || ticks
                .iter()
                .enumerate()
                .any(|(i, t)| now.row_data(i).as_ref() != Some(t))
        {
            d.set_track_collections(Rc::new(VecModel::from(ticks)).into());
        }
    }

    fn source_pairs(&self) -> Vec<(String, String)> {
        self.lib_status
            .borrow()
            .as_ref()
            .map(|s| {
                s.sources
                    .iter()
                    .map(|x| (x.name.clone(), x.path.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where the list is narrowed to, and the source tree with that path open.
    fn push_location(
        &self,
        d: &DjState,
        view: &LibView,
        sources: &[(&str, &str)],
        home: &str,
        shown: usize,
    ) {
        let location = view.location.borrow().clone();
        let kind = crate::library_place::location_kind(&location, sources);
        d.set_lib_location(location.as_str().into());
        d.set_lib_location_kind(kind.into());
        d.set_lib_location_label(
            crate::library_place::location_label(&location, sources, home).into(),
        );
        d.set_lib_location_name(crate::library_place::location_name(&location, sources).into());
        let (up, up_label) =
            crate::library_place::parent_location(&location, sources).unwrap_or_default();
        d.set_lib_location_up(up.into());
        d.set_lib_location_up_label(up_label.into());
        d.set_lib_folder_root(crate::library_place::folder_root(&location, sources).into());
        let offline = if kind == "source" {
            let status = self.lib_status.borrow();
            status
                .as_ref()
                .and_then(|s| {
                    s.sources.iter().find(|src| {
                        src.path.trim_end_matches('/') == location.trim_end_matches('/')
                    })
                })
                .and_then(|src| {
                    (!src.connected)
                        .then(|| format!("not connected · {} tracks offline", src.counts.tracks))
                })
        } else {
            None
        };
        let note = offline.unwrap_or_else(|| tracks_label(shown));
        d.set_lib_location_note(note.into());
        let root_path = crate::library_place::folder_root(&location, sources);
        let folders: Vec<LibFolder> = if location.is_empty() || root_path.is_empty() {
            vec![]
        } else {
            let catalog = self.library.borrow();
            let paths: Vec<&str> = catalog.iter().map(|i| i.url.as_str()).collect();
            crate::library_place::open_branch(&paths, &root_path, &location)
                .into_iter()
                .map(|n| LibFolder {
                    path: n.path.into(),
                    label: n.label.into(),
                    count: n.count as i32,
                    depth: n.depth as i32,
                    expanded: n.expanded,
                    expandable: n.expandable,
                })
                .collect()
        };
        let now = d.get_lib_folders();
        if now.row_count() != folders.len()
            || folders
                .iter()
                .enumerate()
                .any(|(i, f)| now.row_data(i).as_ref() != Some(f))
        {
            d.set_lib_folders(Rc::new(VecModel::from(folders)).into());
        }
    }

    pub(super) fn load_deck(&self, id: &str, item: &LibraryItem) -> Result<(), String> {
        if !self.is_live() {
            *self.dj_status.borrow_mut() =
                "Decks play real audio only with PipeWire (not in mock mode).".into();
            self.poll_dj();
            return Err(self.dj_status.borrow().clone());
        }
        let deck = self.deck(id);
        // Recent meter activity includes a paused deck's tail. The transport
        // is the authority on whether replacing its source is safe.
        let status = deck.status();
        if status.loaded && !status.paused {
            *self.dj_status.borrow_mut() =
                format!("{} is playing: pause it to load another track", deck.name);
            self.poll_dj();
            return Err(self.dj_status.borrow().clone());
        }
        if let Some(an) = &self.analyst {
            an.now(&item.url);
        }
        // The engine already up: just the load (re-routing rebuilds every
        // window's graph and strips, a visible stall on the other deck).
        let ready = if self.engine_online.get() {
            Ok(())
        } else {
            self.start_dj()
        };
        let result = ready.and_then(|()| deck.load(&item.url));
        if result.is_ok() {
            self.log_track_event(
                "track_loaded",
                deck.id,
                &item.url,
                serde_json::json!({"title":item.title}),
            );
        }
        *self.dj_status.borrow_mut() = match &result {
            Ok(()) => format!(
                "{} loaded on {} · press space to play",
                item.title, deck.name
            ),
            Err(e) => format!("Couldn't load {}: {e}", item.title),
        };
        self.poll_dj();
        result
    }
}

fn tracks_label(n: usize) -> String {
    format!("{n} {}", if n == 1 { "track" } else { "tracks" })
}

/// What one library row is, for the filters. `played` is the row's own text.
struct RowFacts<'a> {
    is_stream: bool,
    state: &'static str,
    played: &'a str,
    favourite: bool,
    fits: bool,
    straight: bool,
}

/// Every active filter applies. An empty played filter and state "all" add nothing.
fn keep_library_row(
    want_state: &str,
    played_filter: &str,
    fits_only: bool,
    fav_only: bool,
    straight_only: bool,
    row: &RowFacts,
) -> bool {
    if fits_only && !row.fits {
        return false;
    }
    if fav_only && !row.favourite {
        return false;
    }
    if straight_only && !row.straight {
        return false;
    }
    if !played_filter.is_empty() && !played_matches(played_filter, row.played) {
        return false;
    }
    if want_state != "all" {
        // Streams have no analysis standing.
        if row.is_stream || row.state != want_state {
            return false;
        }
    }
    true
}

/// Played history. "Earlier" is anything already played that is not this
/// session and not the last hour, so the four choices do not overlap.
fn played_matches(filter: &str, played: &str) -> bool {
    match filter {
        "session" => played.starts_with("This session"),
        "hour" => played.ends_with("Last hour"),
        "earlier" => {
            !played.is_empty()
                && played != "Unplayed"
                && !played.starts_with("This session")
                && !played.ends_with("Last hour")
        }
        "unplayed" => played == "Unplayed",
        _ => true,
    }
}

/// The same fit the row's key paints: compatible key, and tempo within 8 %
/// at half, the same, or double time.
fn brief_fits(an: Option<&Brief>, reference: Option<&(patch_analysis::key::Key, f64)>) -> bool {
    let (Some(a), Some(&(k, b))) = (an, reference) else {
        return false;
    };
    let tempo_ok = a.bpm.is_some_and(|x| {
        [0.5, 1.0, 2.0]
            .iter()
            .any(|m| ((b * m / x) - 1.0).abs() <= 0.08)
    });
    a.key.is_some_and(|x| x.compatible(k)) && tempo_ok
}

/// Words for the footer's view line, in the order the sidebar lists them.
fn active_filters(state: &str, played: &str, fits: bool, fav: bool, straight: bool) -> Vec<String> {
    let mut bits = Vec::new();
    if state != "all" {
        bits.push(state.to_string());
    }
    if fits {
        bits.push("fits".into());
    }
    if fav {
        bits.push("favourites".into());
    }
    if straight {
        bits.push("straight".into());
    }
    match played {
        "session" => bits.push("this session".into()),
        "hour" => bits.push("last hour".into()),
        "earlier" => bits.push("earlier".into()),
        "unplayed" => bits.push("unplayed".into()),
        _ => {}
    }
    bits
}

/// The footer’s left side. Empty when nothing is filtering the place: the
/// title already says how many tracks it holds.
fn view_line(shown: usize, scope: usize, filters: &[String]) -> String {
    if filters.is_empty() {
        return String::new();
    }
    let mut line = format!("Showing {shown}");
    if shown != scope {
        line.push_str(&format!(" of {scope}"));
    }
    for f in filters {
        line.push_str(" · ");
        line.push_str(f);
    }
    line
}

impl AudioApp {
    /// A row's analysis. Headless shots use the catalog index, so a filter
    /// that hides earlier rows does not hand the next track someone else's
    /// tempo and key.
    fn library_brief(&self, url: &str) -> Option<Brief> {
        if self.frozen {
            let index = {
                let lib = self.library.borrow();
                lib.iter().position(|it| it.url == url)
            };
            return index
                .and_then(dj::demo_library_analysis)
                .map(|a| Brief::of(&a));
        }
        self.analysis_brief(url)
    }

    fn row_facts<'a>(
        &self,
        r: &'a LibraryItem,
        reference: Option<&(patch_analysis::key::Key, f64)>,
    ) -> RowFacts<'a> {
        let is_stream = r.url.contains("://");
        let state = if is_stream {
            ""
        } else {
            state_name(self.lib_ready.borrow().get(&r.url).map(|x| x.0))
        };
        let favourite = self.lib_favourites.borrow().contains(&r.url);
        let brief = self.library_brief(&r.url);
        let straight = brief
            .as_ref()
            .and_then(|b| b.grid.as_ref())
            .is_some_and(|g| g.0 == "straight");
        let fits = brief_fits(brief.as_ref(), reference);
        RowFacts {
            is_stream,
            state,
            played: r.played.as_str(),
            favourite,
            fits,
            straight,
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn row<'a>(
        state: &'static str,
        played: &'a str,
        favourite: bool,
        fits: bool,
        straight: bool,
    ) -> RowFacts<'a> {
        RowFacts {
            is_stream: false,
            state,
            played,
            favourite,
            fits,
            straight,
        }
    }

    #[test]
    fn state_and_favourite_combine() {
        let fav = row("ready", "Unplayed", true, false, true);
        let plain = row("ready", "Unplayed", false, false, true);
        assert!(keep_library_row("ready", "", false, true, false, &fav));
        assert!(!keep_library_row("ready", "", false, true, false, &plain));
        assert!(keep_library_row(
            "check",
            "",
            false,
            false,
            false,
            &row("check", "Unplayed", false, false, false)
        ));
        assert!(!keep_library_row(
            "ready",
            "",
            false,
            false,
            false,
            &row("check", "Unplayed", false, false, false)
        ));
    }

    #[test]
    fn fits_and_straight_exclude_the_rest() {
        let hit = row("ready", "Unplayed", false, true, true);
        let miss = row("ready", "Unplayed", false, false, false);
        assert!(keep_library_row("all", "", true, false, false, &hit));
        assert!(!keep_library_row("all", "", true, false, false, &miss));
        assert!(keep_library_row("all", "", false, false, true, &hit));
        assert!(!keep_library_row("all", "", false, false, true, &miss));
    }

    #[test]
    fn earlier_excludes_session_last_hour_and_unplayed() {
        assert!(played_matches("earlier", "Today"));
        assert!(played_matches("earlier", "3 days ago"));
        assert!(!played_matches("earlier", "This session"));
        assert!(!played_matches("earlier", "This session · 2h ago"));
        assert!(!played_matches("earlier", "Last hour"));
        assert!(!played_matches("earlier", "This session · Last hour"));
        assert!(!played_matches("earlier", "Unplayed"));
        assert!(!played_matches("earlier", ""));
        assert!(played_matches("session", "This session · Last hour"));
        assert!(played_matches("hour", "Last hour"));
        assert!(played_matches("hour", "This session · Last hour"));
        assert!(played_matches("unplayed", "Unplayed"));
        assert!(!played_matches("unplayed", ""));
        assert!(!played_matches("unplayed", "Today"));
    }

    #[test]
    fn a_stream_fails_a_status_filter_and_can_still_be_a_favourite() {
        let stream = RowFacts {
            is_stream: true,
            state: "",
            played: "",
            favourite: true,
            fits: false,
            straight: false,
        };
        assert!(!keep_library_row("ready", "", false, false, false, &stream));
        assert!(keep_library_row("all", "", false, true, false, &stream));
        assert!(!keep_library_row(
            "all",
            "",
            false,
            true,
            false,
            &row("ready", "", false, false, false)
        ));
    }

    #[test]
    fn the_view_line_names_only_the_filters_that_are_on() {
        assert_eq!(view_line(8, 8, &[]), "");
        assert_eq!(
            view_line(1, 10, &active_filters("all", "", false, true, false)),
            "Showing 1 of 10 · favourites"
        );
        assert_eq!(
            view_line(
                3,
                8,
                &active_filters("ready", "earlier", true, false, false)
            ),
            "Showing 3 of 8 · ready · fits · earlier"
        );
        assert_eq!(
            view_line(4, 4, &active_filters("all", "unplayed", false, false, true)),
            "Showing 4 · straight · unplayed"
        );
    }

    fn key(tonic: u8, minor: bool) -> patch_analysis::key::Key {
        patch_analysis::key::Key { tonic, minor }
    }

    fn brief(bpm: f64, tonic: u8) -> Brief {
        Brief {
            downbeat: Some(0),
            bpm: Some(bpm),
            bpm_confidence: 1.0,
            key: Some(key(tonic, true)),
            lufs: -10.0,
            grid: Some(("straight".into(), String::new())),
        }
    }

    #[test]
    fn fits_uses_the_key_and_half_or_double_tempo() {
        let reference = (key(9, true), 124.0);
        assert!(brief_fits(Some(&brief(124.0, 9)), Some(&reference)));
        assert!(brief_fits(Some(&brief(62.0, 9)), Some(&reference)));
        assert!(brief_fits(Some(&brief(248.0, 9)), Some(&reference)));
        assert!(!brief_fits(Some(&brief(140.0, 9)), Some(&reference)));
        assert!(!brief_fits(Some(&brief(124.0, 3)), Some(&reference)));
        assert!(!brief_fits(None, Some(&reference)));
        assert!(!brief_fits(Some(&brief(124.0, 9)), None));
    }
}
