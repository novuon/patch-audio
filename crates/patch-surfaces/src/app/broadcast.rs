//! Broadcasting: starting, stopping, its configuration and sources, the surface.

use super::*;

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl AudioApp {
    /// Start a detached broadcast of the selected source (a validated
    /// command like every other Unit's: nothing runs from the UI thread).
    pub(super) fn start_broadcast(&self) {
        if !self.is_live() {
            *self.bc.message.borrow_mut() =
                "Broadcasting needs the live PipeWire provider.".into();
            self.push_broadcast();
            return;
        }
        if self.bc.target.borrow().trim().is_empty() {
            *self.bc.message.borrow_mut() = "Set a destination first.".into();
            self.push_broadcast();
            return;
        }
        let mut request = self.bc_config_json();
        request["op"] = serde_json::json!("start");
        *self.bc.failure.borrow_mut() = None;
        self.bc.followed.set(None);
        let asked = unix_now();
        if self
            .bc
            .actions
            .request_with("action", request, crate::broadcast_view::read)
        {
            self.bc.busy.set(true);
            self.bc.last_edited.set(Instant::now());
            // Followed from now on (its own start replaces this bound).
            self.bc.followed.set(Some(asked - 0.05));
            *self.bc.message.borrow_mut() = "Starting…".into();
            self.push_broadcast();
        }
    }

    pub(super) fn stop_broadcast(&self) {
        if self
            .bc
            .actions
            .request_with(
                "action",
                serde_json::json!({"op":"stop"}),
                crate::broadcast_view::read,
            )
        {
            self.bc.busy.set(true);
            *self.bc.message.borrow_mut() = "Stopping…".into();
            self.push_broadcast();
        }
    }

    /// The broadcast config the daemon is started with (also its preview).
    pub(super) fn bc_config_json(&self) -> serde_json::Value {
        let sources = self.bc_sources();
        let cur = self
            .bc
            .cursor
            .get()
            .min(sources.len().saturating_sub(1));
        let (label, source) = sources
            .get(cur)
            .map(|(_, l, s)| (l.clone(), s.clone()))
            .unwrap_or_default();
        let target = self.bc.target.borrow().clone();
        let target = if self.bc.kind.borrow().as_str() == "file" {
            expand_home(&target)
        } else {
            target
        };
        serde_json::json!({
            "source": source,
            "label": label,
            "kind": self.bc.kind.borrow().clone(),
            "target": target,
            "mount": self.bc.mount.borrow().clone(),
            "user": self.bc.user.borrow().clone(),
            "password": self.bc.password.borrow().clone(),
            "key": self.bc.key.borrow().clone(),
            "picture": self.bc.picture.borrow().clone(),
            "codec": self.bc.codec.borrow().clone(),
            "bitrate": self.bc.bitrate.get(),
            "rate": self.bc.rate.get(),
            "channels": 2,
            "name": self.bc.name.borrow().clone(),
        })
    }

    /// What can go out: the provider's recordable sources (mock: a demo
    /// list, so the surface still shows what it is for).
    pub(super) fn bc_sources(&self) -> Vec<(String, String, String)> {
        let sources = self.provider.borrow().recordable();
        if sources.is_empty() && self.frozen {
            return vec![(
                "output".into(),
                "What you hear · Scarlett 18i20".into(),
                "device:scarlett.monitor".into(),
            )];
        }
        sources
    }

    pub(super) fn poll_broadcast(&self) {
        if self.frozen {
            return;
        }
        if let Some(reply) = self.bc.actions.take("action") {
            self.bc.busy.set(false);
            self.bc.last_edited.set(Instant::now());
            match reply.result {
                Ok(value) => {
                    *self.bc.message.borrow_mut() =
                        value["message"].as_str().unwrap_or_default().to_owned();
                    if value["status"].is_object() {
                        self.bc.followed.set(value["status"]["since"].as_f64().or(self.bc.followed.get()));
                        *self.bc.snapshot.borrow_mut() = value["status"].clone();
                    } else if value.get("result").is_some() {
                        // Stopped here: nothing to follow.
                        self.bc.followed.set(None);
                        *self.bc.snapshot.borrow_mut() = serde_json::Value::Null;
                    }
                }
                Err(error) => {
                    if self.bc.snapshot.borrow().is_null() {
                        // A start that failed: FAILED, and why.
                        self.bc.followed.set(None);
                        *self.bc.failure.borrow_mut() = Some(error);
                        self.bc.message.borrow_mut().clear();
                    } else {
                        *self.bc.message.borrow_mut() = error;
                    }
                }
            }
        }
        let following = self.bc.followed.get().is_some() || self.bc.snapshot.borrow().is_object();
        self.bc.polls.request_with(
            "status",
            serde_json::json!({"op": "status", "outcome": following}),
            crate::broadcast_view::read,
        );
        // While a start or stop is on its way, its own reply decides.
        if let Some(reply) = self.bc.polls.take("status")
            && reply.requested >= self.bc.last_edited.get()
            && !self.bc.busy.get()
            && let Ok(value) = reply.result
        {
            if value["offline"] == true {
                self.bc_ended(&value["outcome"]);
                *self.bc.snapshot.borrow_mut() = serde_json::Value::Null;
            } else {
                if let Some(since) = value["since"].as_f64() {
                    self.bc.followed.set(Some(since));
                }
                self.bc.gone_at.set(None);
                *self.bc.snapshot.borrow_mut() = value;
            }
        }
        self.bc_feed_picture();
        self.push_broadcast();
    }

    /// The running broadcast's picture comes from this process's window of
    /// it (the one on screen first). Nothing is taken while no broadcast
    /// wants a picture; the sink (and its connection) goes with it.
    fn bc_feed_picture(&self) {
        use patch_broadcast::video::Picture;
        let want = {
            let snap = self.bc.snapshot.borrow();
            snap["picture"].as_str().and_then(Picture::parse).filter(|_| {
                matches!(snap["state"].as_str(), Some("connecting" | "live" | "reconnecting"))
            })
        };
        let window = want.and_then(|p| {
            let wins: Vec<PatchWindow> = self
                .windows
                .borrow()
                .iter()
                .filter_map(|w| w.upgrade())
                .filter(|w| w.get_surface() == p.surface())
                .collect();
            let on_screen = wins.iter().position(|w| self.in_sight(w)).unwrap_or(0);
            wins.into_iter().nth(on_screen)
        });
        match window {
            Some(w) => {
                let sink = self
                    .bc
                    .sink
                    .borrow_mut()
                    .get_or_insert_with(|| Rc::new(patch_broadcast::video::FrameSink::new()))
                    .clone();
                crate::visual_gl::capture::set_target(Some((
                    crate::visual_gl::window_key(w.window()),
                    sink,
                )));
            }
            None => {
                crate::visual_gl::capture::set_target(None);
                if want.is_none() {
                    self.bc.sink.borrow_mut().take();
                }
            }
        }
    }

    /// What the Broadcast window says about the picture, and whether all
    /// is well with it.
    fn bc_picture_note(&self, snap: &serde_json::Value, active: bool) -> (String, bool) {
        use patch_broadcast::video::{FPS, HEIGHT, Picture, WIDTH};
        let size = format!("{WIDTH}×{HEIGHT}, {FPS} fps");
        if active {
            let Some(p) = snap["picture"].as_str().and_then(Picture::parse) else {
                return ("Sound only.".into(), true);
            };
            let coder = match snap["video_encoder"].as_str() {
                Some("h264_nvenc") => "H.264 on the GPU (NVENC)",
                _ => "H.264 (x264)",
            };
            return if snap["picture_live"] == true {
                (format!("{} on air · {size} · {coder}", p.label()), true)
            } else {
                (
                    format!(
                        "No {} on screen: the stream shows black. Show it, and keep it in view while you stream.",
                        p.label()
                    ),
                    false,
                )
            };
        }
        match Picture::parse(&self.bc.picture.borrow()) {
            Some(p) => (
                format!(
                    "The {} goes out with the sound, as it is drawn: {size}, H.264. Keep it on screen while you stream.",
                    p.label()
                ),
                true,
            ),
            None => (
                "Sound only, for a server that takes that: YouTube and Twitch need a picture.".into(),
                true,
            ),
        }
    }

    /// The followed broadcast is gone: its own outcome (never one an earlier
    /// broadcast left) says whether it failed, and the window keeps saying
    /// FAILED and why until the next start. It may still be finishing: its
    /// outcome is waited for a little.
    fn bc_ended(&self, outcome: &serde_json::Value) {
        let Some(since) = self.bc.followed.get() else {
            return;
        };
        if outcome["since"].as_f64().is_some_and(|s| s >= since - 0.01) {
            if let Some(error) = outcome["error"].as_str().filter(|e| !e.is_empty()) {
                *self.bc.failure.borrow_mut() = Some(error.to_owned());
                self.bc.message.borrow_mut().clear();
            }
            self.bc.followed.set(None);
            self.bc.gone_at.set(None);
            return;
        }
        let gone = self.bc.gone_at.get().unwrap_or_else(Instant::now);
        self.bc.gone_at.set(Some(gone));
        if gone.elapsed() > Duration::from_secs(10) {
            self.bc.followed.set(None);
            self.bc.gone_at.set(None);
        }
    }

    /// Show the running broadcast (and the form) in every window.
    pub(super) fn push_broadcast(&self) {
        let snap = self.bc.snapshot.borrow().clone();
        let active = snap.is_object();
        let failure = self.bc.failure.borrow().clone();
        let state = if active {
            snap["state"].as_str().unwrap_or("connecting").to_string()
        } else if failure.is_some() {
            "error".to_string()
        } else {
            "offline".to_string()
        };
        let live = state == "live";
        let secs = snap["seconds"].as_f64().unwrap_or(0.0);
        let wall = snap["wall"].as_f64().unwrap_or(0.0);
        // What the destination took: the file on disk, or what ffmpeg
        // handed the server.
        let bytes = snap["bytes"].as_f64().unwrap_or(0.0);
        let size = if !active {
            "—".into()
        } else if snap["kind"].as_str() == Some("file") {
            format!("{:.1} MB written", bytes / 1e6)
        } else if bytes > 0.0 {
            format!("{:.1} MB sent", bytes / 1e6)
        } else {
            "nothing sent yet".into()
        };
        let level_db = amp_db(snap["level"].as_f64().unwrap_or(0.0) as f32);
        let peak_db = amp_db(snap["peak"].as_f64().unwrap_or(0.0) as f32);
        let sources = self.bc_sources();
        let cur = self.bc.cursor.get().min(sources.len().saturating_sub(1));
        let rows: Vec<crate::ui::BcSource> = sources
            .iter()
            .enumerate()
            .map(|(i, (_, label, _))| crate::ui::BcSource {
                id: i.to_string().into(),
                label: label.as_str().into(),
                selected: i == cur,
            })
            .collect();
        let source_label = sources
            .get(cur)
            .map(|(_, l, _)| l.clone())
            .unwrap_or_else(|| "No source".into());
        // While one runs: where it really goes (a file may have taken the
        // next free name), never the form being edited.
        let destination = if active {
            snap["target"].as_str().unwrap_or_default().to_owned()
        } else {
            patch_broadcast::daemon::config_from_json(&self.bc_config_json())
                .ok()
                .filter(|c| !c.target.trim().is_empty())
                .map(|c| c.display_destination())
                .unwrap_or_default()
        };
        let message: SharedString = self.bc.message.borrow().as_str().into();
        let note: SharedString = if active {
            snap["note"].as_str().unwrap_or_default().into()
        } else {
            failure.unwrap_or_default().into()
        };
        let form = (
            self.bc.kind.borrow().clone(),
            self.bc.codec.borrow().clone(),
            self.bc.bitrate.get() as i32,
            self.bc.rate.get() as i32,
        );
        let (picture_note, picture_ok) = self.bc_picture_note(&snap, active);
        let picture_form = self.bc.picture.borrow().clone();
        let src_model = ModelRc::new(VecModel::from(rows.clone()));
        self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let b = w.global::<crate::ui::BroadcastState>();
            b.set_active(active);
            b.set_live(live);
            b.set_state(state.as_str().into());
            b.set_uptime(dj::clock(wall).as_str().into());
            b.set_sent(dj::clock(secs).as_str().into());
            b.set_size(size.as_str().into());
            b.set_peak(metering::db_text(peak_db).as_str().into());
            b.set_level_pos(patch_ui::meter::MeterKind::System.position(level_db));
            b.set_peak_pos(patch_ui::meter::MeterKind::System.position(peak_db));
            b.set_dropouts(snap["dropouts"].as_i64().unwrap_or(0) as i32);
            b.set_message(message.clone());
            b.set_note(note.clone());
            b.set_destination(destination.as_str().into());
            b.set_source_cursor(cur as i32);
            b.set_source_label(source_label.as_str().into());
            // Discrete chips: kept in step across windows. The text fields
            // are the user's (set when the window opens, never written back).
            b.set_kind(form.0.as_str().into());
            b.set_codec(form.1.as_str().into());
            b.set_bitrate(form.2);
            b.set_rate(form.3);
            b.set_picture(picture_form.as_str().into());
            b.set_picture_note(picture_note.as_str().into());
            b.set_picture_ok(picture_ok);
            if !same_rows(&b.get_sources(), &rows) {
                b.set_sources(src_model.clone());
            }
        }
    }

    pub(super) fn bind_broadcast(&self, w: &PatchWindow) {
        if w.get_surface() == "performer" {
            if !self.frozen { w.window().set_fullscreen(true); }
            self.broadcast_views.borrow_mut().push(crate::broadcast_performer::View::bind(w, self.frozen));
        }
        let win = w.as_weak();
        w.global::<crate::ui::BroadcastDjState>()
            .on_toggle_fullscreen(move || {
                if let Some(w) = win.upgrade() {
                    w.window().set_fullscreen(!w.window().is_fullscreen());
                }
            });
        let b = w.global::<crate::ui::BroadcastState>();
        // The form: the poll never writes these back, so edits stay.
        b.set_kind(self.bc.kind.borrow().as_str().into());
        b.set_codec(self.bc.codec.borrow().as_str().into());
        b.set_bitrate(self.bc.bitrate.get() as i32);
        b.set_rate(self.bc.rate.get() as i32);
        b.set_picture(self.bc.picture.borrow().as_str().into());
        let target = self.bc.target.borrow().clone();
        let target = if target.is_empty() && self.frozen {
            "~/Music/Patch Broadcast/set.ogg".to_string()
        } else {
            target
        };
        b.set_target(target.as_str().into());
        b.set_mount(self.bc.mount.borrow().as_str().into());
        b.set_user(self.bc.user.borrow().as_str().into());
        b.set_password(self.bc.password.borrow().as_str().into());
        b.set_key(self.bc.key.borrow().as_str().into());
        b.set_name(self.bc.name.borrow().as_str().into());
        let me = self.me.clone();
        b.on_edit(move |field, value| {
            if let Some(a) = me.upgrade() {
                a.bc.set_field(&field, &value);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_go_live(move || {
            if let Some(a) = me.upgrade() {
                a.start_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_stop_stream(move || {
            if let Some(a) = me.upgrade() {
                a.stop_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_move_source(move |d| {
            if let Some(a) = me.upgrade() {
                let n = a.bc_sources().len() as i32;
                a.bc.cursor
                    .set((a.bc.cursor.get() as i32 + d).clamp(0, (n - 1).max(0)) as usize);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_set_kind(move |k| {
            if let Some(a) = me.upgrade() {
                a.bc.set_field("kind", &k);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_set_picture(move |p| {
            if let Some(a) = me.upgrade() {
                a.bc.set_field("picture", &p);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_set_codec(move |c| {
            if let Some(a) = me.upgrade() {
                a.bc.set_field("codec", &c);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_set_bitrate(move |v| {
            if let Some(a) = me.upgrade() {
                a.bc.bitrate.set(v.clamp(32, 512) as u32);
                a.push_broadcast();
            }
        });
        let me = self.me.clone();
        b.on_set_rate(move |v| {
            if let Some(a) = me.upgrade() {
                a.bc.rate.set(if v == 44_100 { 44_100 } else { 48_000 });
                a.push_broadcast();
            }
        });
    }

    // ---- DJ booth ------------------------------------------------------
}
