//! The Recorder surface: its folder, record modes, requests and status polling.

use super::*;

impl AudioApp {
    pub(super) fn bind_recorder(&self, w: &PatchWindow) {
        let r = w.global::<RecState>();
        let me = self.me.clone();
        r.on_play_take(move |path| {
            if let Some(a) = me.upgrade() {
                if (0..2).any(|d| a.decks[d].status().loaded && !a.decks[d].status().paused) {
                    *a.rec_message.borrow_mut() =
                        "Pause the live mix before playing a take through Player.".into();
                    a.poll_recorder();
                } else if a.rec_snapshot.borrow()["path"].as_str() == Some(path.as_str()) {
                    *a.rec_message.borrow_mut() =
                        "Stop the recording before playing this take.".into();
                    a.poll_recorder();
                } else {
                    a.player.send(crate::player::Action::Load(path.to_string()));
                }
            }
        });
        let me = self.me.clone();
        r.on_pause_record(move || {
            if let Some(a) = me.upgrade()
                && !a.rec_busy.get()
                && !a.rec_snapshot.borrow().is_null()
            {
                let paused = a.rec_snapshot.borrow()["paused"].as_bool() != Some(true);
                a.queue_record(serde_json::json!({"op":"pause", "paused":paused}));
            }
        });
        let me = self.me.clone();
        r.on_toggle_record(move || {
            if let Some(a) = me.upgrade()
                && !a.rec_busy.get()
            {
                let _ = a.toggle_record_mode(None);
            }
        });
        let me = self.me.clone();
        r.on_move_source(move |d| {
            if let Some(a) = me.upgrade() {
                if a.rec_busy.get() || !a.rec_snapshot.borrow().is_null() {
                    return;
                }
                let n = a.provider.borrow().recordable().len() as i32;
                a.rec_cursor
                    .set((a.rec_cursor.get() as i32 + d).clamp(0, (n - 1).max(0)) as usize);
                a.poll_recorder();
            }
        });
        let me = self.me.clone();
        r.on_set_float(move |f| {
            if let Some(a) = me.upgrade() {
                if !a.rec_busy.get() && a.rec_snapshot.borrow().is_null() {
                    a.rec_float.set(f);
                }
                a.poll_recorder();
            }
        });
    }

    /// Where takes go (looked up once: this runs at 5 Hz).
    pub(super) fn rec_dir() -> std::path::PathBuf {
        static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        DIR.get_or_init(Self::find_rec_dir).clone()
    }

    pub(super) fn find_rec_dir() -> std::path::PathBuf {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let music = std::fs::read_to_string(config.join("user-dirs.dirs"))
            .ok()
            .and_then(|s| {
                s.lines().find_map(|line| {
                    let value = line
                        .trim()
                        .strip_prefix("XDG_MUSIC_DIR=")?
                        .trim()
                        .trim_matches('"');
                    let path = if let Some(rest) = value.strip_prefix("$HOME/") {
                        home.join(rest)
                    } else if value == "$HOME" {
                        home.clone()
                    } else {
                        value.into()
                    };
                    path.is_absolute().then_some(path)
                })
            })
            .unwrap_or_else(|| home.join("Music"));
        music.join("Patch Recordings")
    }

    pub(super) fn queue_record(&self, request: serde_json::Value) {
        self.rec_watch.set(true);
        if self
            .rec_actions
            .request_with("action", request, crate::record_view::read)
        {
            self.rec_busy.set(true);
            self.rec_edited.set(Instant::now());
            *self.rec_message.borrow_mut() = "Working…".into();
        }
    }

    pub(super) fn toggle_record_mode(&self, pick: Option<(&str, Option<bool>)>) -> Result<(), String> {
        let recording = !self.rec_snapshot.borrow().is_null();
        if recording {
            self.queue_record(serde_json::json!({"op":"stop"}));
            return Ok(());
        }
        if !self.is_live() {
            *self.rec_message.borrow_mut() = "Recording needs the live PipeWire provider.".into();
            self.poll_recorder();
            return Err("mock mode".into());
        }
        let sources = self.provider.borrow().recordable();
        let src = if pick.is_some_and(|(want, _)| want == "program") {
            Some(("program".into(), "DJ Program".into(), "program:mix".into()))
        } else {
            match pick {
                Some((want, _)) => sources.iter().find(|(id, label, _)| {
                    id == want || label.to_lowercase().contains(&want.to_lowercase())
                }),
                None => sources.get(self.rec_cursor.get()),
            }
            .cloned()
        }
        .ok_or("no such source")?;
        if let Some((_, Some(f))) = pick {
            self.rec_float.set(f);
        }
        let float = self.rec_float.get();
        let rate = self.provider.borrow().engine().sample_rate;
        let stamp = timestamp_utc().replace(':', "");
        let clean: String = src
            .1
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        let path = Self::rec_dir().join(format!(
            "Patch {stamp} {}-{}.wav",
            clean.split_whitespace().collect::<Vec<_>>().join(" "),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let take = patch_record::daemon::Take {
            source: patch_record::daemon::Source::parse(&src.2).ok_or("bad source")?,
            label: src.1.clone(),
            format: if float {
                patch_record::wav::SampleFormat::F32
            } else {
                patch_record::wav::SampleFormat::S24
            },
            rate,
            path,
        };
        self.queue_record(
            serde_json::json!({"op":"start", "source":take.source.encode(), "label":take.label,
            "float":float, "rate":rate, "path":take.path}),
        );
        Ok(())
    }

    pub(super) fn poll_record_status(&self) {
        if !self.rec_watch.get() {
            return;
        }
        self.rec_polls.request_with(
            "status",
            serde_json::json!({"op":"status"}),
            crate::record_view::read,
        );
        if let Some(reply) = self.rec_polls.take("status")
            && !self.rec_busy.get()
            && reply.requested >= self.rec_edited.get()
            && let Ok(value) = reply.result
        {
            *self.rec_snapshot.borrow_mut() = value;
        }
        if self.rec_snapshot.borrow().is_null() && !self.rec_busy.get() {
            self.rec_watch.set(false);
        }
    }

    pub(super) fn poll_recorder(&self) {
        self.rec_watch.set(true);
        if let Some(reply) = self.rec_actions.take("action") {
            self.rec_busy.set(false);
            self.rec_edited.set(Instant::now());
            self.rec_scan_at.set(None);
            match reply.result {
                Ok(value) => {
                    *self.rec_message.borrow_mut() =
                        value["message"].as_str().unwrap_or_default().to_owned();
                    *self.rec_snapshot.borrow_mut() = value["status"].clone();
                    if value.get("result").is_some() {
                        self.rec_files.borrow_mut()["result"] = value["result"].clone();
                    }
                }
                Err(error) => *self.rec_message.borrow_mut() = error,
            }
        }
        self.poll_record_status();
        let status = self
            .rec_snapshot
            .borrow()
            .as_object()
            .map(|_| self.rec_snapshot.borrow().clone());
        let sources = self.provider.borrow().recordable();
        let cur = status
            .as_ref()
            .and_then(|s| s["source"].as_str())
            .and_then(|source| sources.iter().position(|s| s.2 == source))
            .unwrap_or(self.rec_cursor.get())
            .min(sources.len().saturating_sub(1));
        self.rec_cursor.set(cur);
        let rows: Vec<RecSource> = sources
            .iter()
            .enumerate()
            .map(|(i, (id, label, _))| RecSource {
                id: id.as_str().into(),
                label: label.as_str().into(),
                selected: i == cur,
            })
            .collect();
        let dir = Self::rec_dir();
        if self
            .rec_scan_at
            .get()
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
            && self.rec_scans.request_with(
                "files",
                serde_json::json!({"op":"files", "dir":dir}),
                crate::record_view::read,
            )
        {
            self.rec_scan_at.set(Some(Instant::now()));
        }
        if let Some(reply) = self.rec_scans.take("files")
            && reply.requested >= self.rec_edited.get()
            && let Ok(value) = reply.result
        {
            *self.rec_files.borrow_mut() = value;
        }
        let files = self.rec_files.borrow();
        let takes: Vec<TakeItem> = files["takes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| TakeItem {
                name: t["name"].as_str().unwrap_or_default().into(),
                path: t["path"].as_str().unwrap_or_default().into(),
                detail: t["detail"].as_str().unwrap_or_default().into(),
            })
            .collect();
        let free = files["free"].as_str().unwrap_or("?").to_owned();
        if status.is_none() && !self.rec_busy.get() {
            if let Some(error) = files["result"]["error"].as_str() {
                *self.rec_message.borrow_mut() = format!("Take stopped: {error}");
            }
        } else if let Some(error) = status.as_ref().and_then(|s| s["error"].as_str()) {
            *self.rec_message.borrow_mut() = error.to_owned();
        }
        let last = files["result"].as_object().map(|_| files["result"].clone());
        let display = status.as_ref().or(last.as_ref());
        let (recording, tc, size, peak, peak_pos, clips, drop, loud, file) = match display {
            Some(s) => {
                let secs = s["seconds"].as_f64().unwrap_or(0.0);
                let rate = s["rate"].as_f64().unwrap_or(48_000.0);
                let frames = s["frames"].as_f64().unwrap_or(0.0);
                let h = (secs / 3600.0) as u64;
                let m = ((secs % 3600.0) / 60.0) as u64;
                let sec = (secs % 60.0) as u64;
                let cs = ((frames % rate) / rate * 100.0) as u64;
                let pk = s["peak"].as_f64().unwrap_or(0.0) as f32;
                let pk_db = if pk > 0.0 { 20.0 * pk.log10() } else { -120.0 };
                (
                    status.is_some(),
                    format!("{h:02}:{m:02}:{sec:02}.{cs:02}"),
                    format!(
                        "{:.1} MB · {}",
                        s["bytes"].as_f64().unwrap_or(0.0) / 1e6,
                        s["format"].as_str().unwrap_or("")
                    ),
                    metering::db_text(pk_db),
                    patch_ui::meter::MeterKind::System.position(pk_db),
                    s["clips"].as_i64().unwrap_or(0) as i32,
                    s["dropouts"].as_i64().unwrap_or(0) as i32,
                    s["integrated"]
                        .as_f64()
                        .map(|v| metering::lufs_text(v as f32))
                        .unwrap_or_else(|| "—".into()),
                    s["path"].as_str().unwrap_or("").to_string(),
                )
            }
            None => (
                false,
                "00:00:00.00".into(),
                "—".into(),
                "−∞".into(),
                0.0,
                0,
                0,
                "—".into(),
                String::new(),
            ),
        };
        let msg: SharedString = self.rec_message.borrow().as_str().into();
        let float = status
            .as_ref()
            .map(|s| s["format"].as_str().is_some_and(|f| f.contains("32-bit")))
            .unwrap_or(self.rec_float.get());
        let folder: SharedString = dir.display().to_string().into();
        let src_model = ModelRc::new(VecModel::from(rows.clone()));
        let takes_model = ModelRc::new(VecModel::from(takes.clone()));
        self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let r = w.global::<RecState>();
            if !same_rows(&r.get_sources(), &rows) {
                r.set_sources(src_model.clone());
            }
            r.set_source_cursor(cur as i32);
            r.set_float32(float);
            r.set_recording(recording);
            r.set_busy(self.rec_busy.get());
            r.set_paused(status.as_ref().is_some_and(|s| s["paused"] == true));
            r.set_timecode(tc.as_str().into());
            r.set_size(size.as_str().into());
            r.set_peak(peak.as_str().into());
            r.set_peak_pos(peak_pos);
            let level = status
                .as_ref()
                .and_then(|s| s["level"].as_f64())
                .unwrap_or(0.0) as f32;
            r.set_level_pos(patch_ui::meter::MeterKind::System.position(if level > 0.0 {
                20.0 * level.log10()
            } else {
                -120.0
            }));
            r.set_clips(clips);
            r.set_dropouts(drop);
            r.set_loudness(loud.as_str().into());
            r.set_file(file.as_str().into());
            r.set_folder(folder.clone());
            r.set_free(free.as_str().into());
            r.set_message(msg.clone());
            if !same_rows(&r.get_takes(), &takes) {
                r.set_takes(takes_model.clone());
            }
        }
    }

    // ---- Broadcast unit --------------------------------------------------
}
