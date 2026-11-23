use super::*;
use patch_engine::sequencer::{NAMES, Pattern, Step};
use serde_json::json;

#[derive(Default)]
pub(super) struct View {
    presence_at: Cell<Option<std::time::Instant>>,
    pattern: RefCell<Pattern>,
    selected: Cell<(usize, usize)>,
    ready: Cell<bool>,
    playing: Cell<bool>,
    polls: crate::poll::Poller,
    pending: RefCell<Option<serde_json::Value>>,
    busy: Cell<bool>,
    busy_since: Cell<Option<std::time::Instant>>,
    sync_on: Cell<bool>,
    sync_source: Cell<u32>,
}
impl AudioApp {
    pub(super) fn bind_sequencer(&self, w: &PatchWindow) {
        if self.frozen {
            self.sequencer.ready.set(true);
        }
        let me = self.me.clone();
        w.global::<SeqState>().on_action(move |action, value| {
            if let Some(a) = me.upgrade() {
                a.seq_action(&action, value);
            }
        });
        let me = self.me.clone();
        w.global::<SeqState>().on_cell(move |track, step, toggle| {
            if let Some(a) = me.upgrade() {
                if !(0..8).contains(&track)
                    || !(0..32).contains(&step)
                    || !a.sequencer.ready.get()
                    || a.sequencer.busy.get()
                {
                    return;
                }
                a.sequencer.selected.set((track as usize, step as usize));
                if toggle {
                    let mut p = a.sequencer.pattern.borrow_mut();
                    let s = &mut p.tracks[track as usize].steps[step as usize];
                    s.velocity = if s.velocity > 0 { 0 } else { 100 };
                }
                a.seq_publish(toggle);
            }
        });
        let me = self.me.clone();
        w.global::<SeqState>().on_sync(move |on, source| {
            if let Some(a) = me.upgrade() {
                a.seq_sync(on, source as u32);
            }
        });
        self.seq_publish(false);
    }
    fn seq_message(&self, text: &str) {
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            w.global::<SeqState>().set_message(text.into());
        }
    }
    fn seq_action(&self, action: &str, value: i32) {
        let v = &self.sequencer;
        if !v.ready.get() {
            return;
        }
        if v.busy.get() {
            self.seq_message("Finishing the previous command; try again shortly.");
            return;
        }
        if matches!(action, "play" | "stop" | "save" | "load") {
            // Transport commands are lightweight — no pattern data needed.
            // Pattern edits are flushed separately via seq_publish.
            let req = match action {
                "play" => json!({"op":"sequencer", "playing": !v.playing.get()}),
                "stop" => json!({"op":"sequencer", "stop": true}),
                "save" => json!({"op":"sequencer", "save": true}),
                "load" => json!({"op":"sequencer", "load": true}),
                _ => return,
            };
            let key = if action == "load" {
                "seq-load"
            } else {
                "seq-command"
            };
            if v.polls.request(key, req) {
                v.pending.borrow_mut().take();
                v.busy.set(true);
                v.busy_since.set(Some(std::time::Instant::now()));
                if action == "load" {
                    v.ready.set(false);
                }
                self.seq_publish(false);
                self.seq_message(if action == "save" {
                    "Saving pattern…"
                } else {
                    "Updating sequencer…"
                });
            } else {
                self.seq_message("Engine busy; try again shortly.");
            }
            return;
        }
        let (track, step) = v.selected.get();
        let mut p = v.pattern.borrow_mut();
        match action {
            "bpm" => p.bpm = value.clamp(40, 240) as u32,
            "swing" => p.swing = value.clamp(0, 60) as u32,
            "level" => p.level = value.clamp(0, 100) as u32,
            "preset" => *p = Pattern::preset(value.clamp(0, 2) as usize),
            "mute" if (0..8).contains(&value) => {
                let t = &mut p.tracks[value as usize];
                t.muted = !t.muted;
                v.selected.set((value as usize, step));
            }
            "length" => p.tracks[track].length = value.clamp(1, 32) as u32,
            "track-level" => p.tracks[track].level = value.clamp(0, 100) as u32,
            "pan" => p.tracks[track].pan = value.clamp(-100, 100),
            "velocity" => p.tracks[track].steps[step].velocity = value.clamp(0, 127) as u32,
            "probability" => p.tracks[track].steps[step].probability = value.clamp(0, 100) as u32,
            "repeats" => p.tracks[track].steps[step].repeats = value.clamp(1, 4) as u32,
            "clear" => p.tracks[track].steps = [Step::default(); 32],
            "duplicate" => {
                let t = &mut p.tracks[track];
                for i in 0..16 {
                    t.steps[i + 16] = t.steps[i];
                }
                t.length = 32;
            }
            _ => return,
        }
        drop(p);
        self.seq_publish(true);
    }
    pub(super) fn seq_sync(&self, on: bool, source: u32) {
        let v = &self.sequencer;
        if !v.ready.get() || v.busy.get() {
            return;
        }
        v.sync_on.set(on);
        v.sync_source.set(source);
        let req =
            json!({"op":"sequencer-sync", "on":on, "source": if source == 1 { "b" } else { "a" }});
        if v.polls.request("seq-sync", req) {
            v.busy.set(true);
            v.busy_since.set(Some(std::time::Instant::now()));
            self.seq_message("Syncing tempo…");
        }
    }
    pub(super) fn seq_publish(&self, send: bool) {
        let v = &self.sequencer;
        let p = v.pattern.borrow();
        if send {
            *v.pending.borrow_mut() = Some(json!({"op":"sequencer", "pattern":&*p}));
        }
        let (track, step) = v.selected.get();
        let rows: ModelRc<SeqTrack> = Rc::new(VecModel::from(
            p.tracks
                .iter()
                .enumerate()
                .map(|(i, t)| SeqTrack {
                    name: NAMES[i].into(),
                    muted: t.muted,
                    length: t.length as i32,
                    level: t.level as i32,
                    steps: Rc::new(VecModel::from(
                        t.steps
                            .iter()
                            .map(|s| s.velocity as i32)
                            .collect::<Vec<_>>(),
                    ))
                    .into(),
                })
                .collect::<Vec<_>>(),
        ))
        .into();
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            let s = w.global::<SeqState>();
            if self.frozen {
                s.set_message("Preview · original synthesized drum bank".into());
            }
            s.set_tracks(rows.clone());
            s.set_ready(v.ready.get() && !v.busy.get());
            s.set_playing(v.playing.get());
            s.set_bpm(p.bpm as i32);
            s.set_swing(p.swing as i32);
            s.set_level(p.level as i32);
            s.set_track(track as i32);
            s.set_step(step as i32);
            let t = &p.tracks[track];
            let st = t.steps[step];
            s.set_length(t.length as i32);
            s.set_track_level(t.level as i32);
            s.set_pan(t.pan);
            s.set_velocity(st.velocity as i32);
            s.set_probability(st.probability as i32);
            s.set_repeats(st.repeats as i32);
            s.set_sync_on(v.sync_on.get());
            s.set_sync_source(v.sync_source.get() as i32);
        }
    }
    pub(super) fn poll_sequencer(&self) {
        if self.frozen {
            return;
        }
        let v = &self.sequencer;
        // Auto-clear stuck busy flag after 5 seconds
        v.polls.take("seq-presence");
        let shown = self.windows.borrow().iter().filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "sequencer");
        if !shown && v.presence_at.replace(None).is_some() {
            v.polls.request("seq-presence", json!({"op":"module_presence", "module":"sequencer", "owner":std::process::id(), "open":false}));
        }
        if shown
            && v.presence_at.get().is_none_or(|t| t.elapsed().as_secs() >= 1)
            && v.polls.request("seq-presence", json!({"op":"module_presence", "module":"sequencer", "owner":std::process::id()}))
        {
            v.presence_at.set(Some(std::time::Instant::now()));
        }
        if v.busy.get()
            && v.busy_since
                .get()
                .is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(5))
        {
            v.busy.set(false);
            v.busy_since.set(None);
            self.seq_message("Command timed out — ready");
        }
        let mut replies: Vec<_> = [
            "seq-command",
            "seq-load",
            "seq-edit",
            "seq-status",
            "seq-sync",
        ]
        .into_iter()
        .filter_map(|key| v.polls.take(key).map(|reply| (key, reply)))
        .collect();
        replies.sort_by_key(|(_, reply)| reply.at);
        for (key, reply) in replies {
            if key == "seq-command" || key == "seq-load" {
                v.busy.set(false);
                self.seq_publish(false);
            }
            match reply.result {
                Ok(data) => {
                    if (!v.ready.get() && !v.busy.get() || key == "seq-load")
                        && let Ok(p) = serde_json::from_value::<Pattern>(data["pattern"].clone())
                    {
                        *v.pattern.borrow_mut() = p;
                        v.ready.set(true);
                        self.seq_publish(false);
                    }
                    v.playing.set(data["playing"].as_bool().unwrap_or(false));
                    for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                        let s = w.global::<SeqState>();
                        s.set_playing(v.playing.get());
                        s.set_position(data["position"].as_u64().unwrap_or(0) as i32);
                    }
                    if data["saved"] == true {
                        self.seq_message("Pattern saved to patch/sequencer.json");
                    } else if key != "seq-status" {
                        self.seq_message("Pattern ready · original synthesized drum bank · output through Patch master");
                    } else if self.sequencer.ready.get() {
                        // Clear only the initial connection message.
                        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
                            let s = w.global::<SeqState>();
                            if s.get_message().starts_with("Connecting") {
                                s.set_message("Ready · choose a groove and press Play".into());
                            }
                        }
                    }
                }
                Err(e) => {
                    self.seq_message(if e.contains("unknown op") || e.contains("Unknown op") { "This engine predates Sequencer. Upgrade the running Patch engine to this build." } else { &e });
                    if key == "seq-load" {
                        v.ready.set(true);
                        self.seq_publish(false);
                    }
                    if key == "seq-sync" {
                        v.busy.set(false);
                        self.seq_publish(false);
                    }
                }
            }
        }
        if let Some(req) = v.pending.borrow().as_ref() {
            if v.polls.request("seq-edit", req.clone()) { /* cleared below, outside borrow */
            } else {
                return;
            }
        }
        v.pending.borrow_mut().take();
        if self
            .windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| w.get_surface() == "sequencer")
        {
            v.polls.request(
                "seq-status",
                json!({"op":"sequencer", "status_only":v.ready.get()}),
            );
        }
    }
}
