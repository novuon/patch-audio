//! Engine housekeeping threads: idling and driving the graph, stream checks, the system volume, the link keeper, thread scheduling and channel activity.

use super::*;

/// The graph runs for the engine alone only while it makes sound itself (a
/// deck plays or previews, a latency measurement). Otherwise it runs when
/// something plays into Patch, as any filter would, and an idle device
/// suspends (WirePlumber, after a few seconds) as it does without Patch:
/// always-on, the sound card ran and the CPU woke ~47 times a second in
/// silence. A deck command wakes it at once (`wake`). It rests 5 minutes
/// after the decks stop while a track is loaded (mid-set the device is
/// awake when you press play: waking a suspended card costs ~0.25 s), 10 s
/// with none. A plain start (login, `engine start`) rests at once: nothing
/// plays yet, and waking the card in silence is heard as a crackle on some
/// (a handover successor drives: it must run cycles before it takes over).
pub(super) fn idler(s: Arc<Shared>) {
    let mut quiet_since: Option<std::time::Instant> = None;
    let mut resting_start = std::env::var_os("PATCH_ENGINE_SILENT").is_none();
    loop {
        // Resting, 4 times a second: a deck command wakes the engine at
        // once anyway (`wake`); this catches a deck started otherwise.
        std::thread::sleep(Duration::from_millis(if s.always.load(Ordering::Relaxed) {
            100
        } else {
            250
        }));
        // An app already playing at start keeps the graph running: no
        // relink of the outputs under its sound.
        if resting_start {
            check_streams(&s);
        }
        let decks = s.params.decks.iter().any(|d| {
            d.status
                .lock()
                .map(|st| st.playing || st.previewing)
                .unwrap_or(true)
        });
        let measuring = s
            .measure
            .lock()
            .map(|m| m.phase == "measuring")
            .unwrap_or(false);
        if decks
            || s.params.capture.active.load(Ordering::Relaxed)
            || s.params.sequencer.playing.load(Ordering::Relaxed)
            || s.pattern.playing()
            || s.tidal.playing()
            || measuring
            || s.apps.load(Ordering::Relaxed)
        {
            quiet_since = None;
            resting_start = false;
            wake(&s);
        } else if s.always.load(Ordering::Relaxed) {
            let since = *quiet_since.get_or_insert_with(std::time::Instant::now);
            let loaded = s
                .params
                .decks
                .iter()
                .any(|d| d.status.lock().map(|st| st.loaded).unwrap_or(true));
            if resting_start
                || since.elapsed() >= Duration::from_secs(if loaded { 300 } else { 10 })
            {
                drive(&s, false);
            }
        }
        // Until the engine is up, `drive` can't act yet: keep trying.
        if !s.always.load(Ordering::Relaxed) {
            resting_start = false;
        }
    }
}

/// Why the engine drives the graph (for status): what plays or measures.
pub(super) fn drive_reason(s: &Shared) -> String {
    let mut why: Vec<String> = s
        .params
        .decks
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            let st = d.status.lock().ok()?;
            let deck = (b'a' + i as u8) as char;
            (st.playing || st.previewing).then(|| {
                format!(
                    "deck {deck} {}",
                    if st.playing { "playing" } else { "previewing" }
                )
            })
        })
        .collect();
    if s.measure.lock().is_ok_and(|m| m.phase == "measuring") {
        why.push("latency measurement".into());
    }
    if s.params.capture.active.load(Ordering::Relaxed) {
        why.push("program recording".into());
    }
    if s.apps.load(Ordering::Relaxed) {
        why.push("an app plays into Patch".into());
    }
    if s.params.sequencer.playing.load(Ordering::Relaxed) {
        why.push("sequencer playing".into());
    }
    if why.is_empty() && s.always.load(Ordering::Relaxed) {
        let loaded: Vec<String> = s
            .params
            .decks
            .iter()
            .enumerate()
            .filter(|(_, d)| d.status.lock().is_ok_and(|st| st.loaded))
            .map(|(i, _)| ((b'a' + i as u8) as char).to_string())
            .collect();
        why.push(if loaded.is_empty() {
            "resting soon (10 s after the decks stop)".into()
        } else {
            format!(
                "a track on deck {} (rests 5 min after the decks stop)",
                loaded.join(", ")
            )
        });
    }
    why.join(", ")
}

/// The engine drives the graph now (before a deck plays: no wait).
pub(super) fn wake(s: &Shared) {
    if !s.always.load(Ordering::Relaxed) {
        drive(s, true);
    }
}

pub(super) fn drive(s: &Shared, on: bool) {
    let h = s.handle.load(Ordering::Acquire);
    if h.is_null() {
        return;
    }
    s.always.store(on, Ordering::Relaxed);
    // The node first (PipeWire takes a link's passivity from its nodes when
    // it's made), then the outputs made again.
    // SAFETY: the handle is valid until patch_engine_run returns.
    unsafe { patch_engine_set_always(h, on as std::ffi::c_int) };
    std::thread::sleep(Duration::from_millis(15));
    s.links.lock().unwrap().set_drive(on);
}

/// Whether an app has a stream on one of Patch's sinks (the bus, the app
/// channels): the engine must run for it. On stream events, and at start.
pub(super) fn check_streams(s: &Shared) {
    let run = |args: &[&str]| {
        std::process::Command::new("pactl")
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let any = apps_on_patch(
        &run(&["list", "short", "sinks"]),
        &run(&["-f", "json", "list", "sink-inputs"]),
    );
    s.apps.store(any, Ordering::Relaxed);
    if any {
        wake(s);
    }
}

/// Whether a stream plays on one of Patch's sinks (`pactl list short sinks`,
/// `pactl -f json list sink-inputs`). The system-sounds loopback is Patch's
/// own: it exists from the first window on and carries sound only when
/// something plays into `patch.system-sounds`, which counts by itself.
/// Counted, it kept the graph and the sound card running in silence, and
/// the card woke (on some, an audible crackle) when Patch's windows opened.
fn apps_on_patch(sinks: &str, inputs: &str) -> bool {
    let ours: Vec<u64> = sinks
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (id, name) = (f.next()?, f.next()?);
            (name == crate::links::BUS
                || name == "patch.system-sounds"
                || name.starts_with("patch.app-")
                || name.starts_with("patch.stream-"))
            .then(|| id.parse().ok())
            .flatten()
        })
        .collect();
    let inputs: Vec<serde_json::Value> = serde_json::from_str(inputs).unwrap_or_default();
    inputs.iter().any(|v| {
        v["sink"].as_u64().is_some_and(|sink| ours.contains(&sink))
            && v["properties"]["node.name"] != "patch.system-sounds.loopback"
    })
}

pub(super) fn system_volume_json(p: &Params) -> serde_json::Value {
    let (v, m) = p.system();
    let native = p.bus_watch.lock().unwrap().volume.is_some();
    json!({"left": v[0], "right": v[1], "mute": m, "source": if native { "pipewire" } else { "pactl" }})
}

/// The system volume: with Patch as the system's audio, the bar's slider
/// and the media keys set the `patch` sink's volume. Its monitor carries
/// it into the bus; everything else (decks, app channels) gets it here,
/// so every source follows the same volume. The engine's PipeWire listener
/// applies a change itself (`Params::bus_volume_changed`); here, the setup
/// and the monitor claim. `pactl subscribe` reports a change at once; the
/// volume is read then (pactl only before the listener reports), and when
/// the setup changes
/// (a file check every 2 s; no process: each pactl run was a connection
/// PipeWire had to serve). Off (no system audio): unity, the device's own
/// volume rules.
pub(super) fn system_volume(s: Arc<Shared>) {
    use std::io::BufRead as _;
    // Streams already open when the engine starts.
    check_streams(&s);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let st = s.clone();
    std::thread::Builder::new()
        .name("patch-volume-events".into())
        .spawn(move || {
            loop {
                // posix_spawn, not a before-exec hook: that hook forks the
                // live engine and copies its page tables under the audio
                // callback. The helper arms PR_SET_PDEATHSIG and execs
                // pactl, so a listener is still not left behind for
                // pipewire-pulse after the engine exits.
                let child = super::pdeath::spawn(&["pactl", "subscribe"]);
                if let Ok(mut child) = child {
                    if let Some(out) = child.stdout.take() {
                        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                            // A stream opened, closed or (un)corked: an app on
                            // Patch wakes the engine before its first audio.
                            if line.contains(" on sink-input #") {
                                check_streams(&st);
                            }
                            if (line.contains(" on sink #") || line.contains(" on server"))
                                && tx.send(()).is_err()
                            {
                                return;
                            }
                        }
                    }
                    let _ = child.wait();
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        })
        .ok();
    let mut last: Option<([f32; 2], bool)> = None;
    let mut claimed_at: Option<std::time::Instant> = None;
    let mut setup = patch_core::setup::Watched::default();
    let mut was_on: Option<bool> = None;
    let mut event = true;
    loop {
        let on = setup.get().system_audio;
        if !event && was_on == Some(on) {
            event = rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok();
            while rx.try_recv().is_ok() {}
            continue;
        }
        was_on = Some(on);
        // The system volume after the recording tap: claim the bus's
        // monitor (free of the volume), again on events at most every 5 s
        // (a recreated sink starts with it on). The output stage applies it
        // first: for a moment the bus is quieter, never louder. Given back
        // when system audio goes off (a retired engine leaves it to its
        // successor).
        let retired = s.retired.load(Ordering::Acquire);
        if on
            && !retired
            && claimed_at.is_none_or(|t: std::time::Instant| t.elapsed() > Duration::from_secs(5))
        {
            s.params.system_post.store(true, Ordering::Relaxed);
            if crate::links::set_monitor_volumes(false) {
                claimed_at = Some(std::time::Instant::now());
            } else {
                s.params.system_post.store(false, Ordering::Relaxed);
                claimed_at = None;
            }
        } else if !on && !retired && claimed_at.take().is_some() {
            crate::links::set_monitor_volumes(true);
            s.params.system_post.store(false, Ordering::Relaxed);
        }
        // The engine's PipeWire listener applies changes itself while
        // `live`; pactl only until it has reported.
        let native = {
            let mut w = s.params.bus_watch.lock().unwrap();
            w.live = on;
            w.volume
        };
        let now = if on {
            native.or_else(crate::links::bus_volume)
        } else {
            Some(([1.0, 1.0], false))
        };
        if let Some(v) = now.filter(|v| Some(*v) != last) {
            s.params.set_system(v.0[0], v.0[1], v.1);
            last = Some(v);
        }
        // The next event (a burst, a slider drag, is one read).
        event = rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok();
        while rx.try_recv().is_ok() {}
    }
}

pub(super) fn keeper(s: Arc<Shared>) {
    // Woken by graph changes (and by requests that change routing); a slow
    // pass every 30 s catches anything the events missed.
    let watch = crate::events::Watch::new(crate::events::Interest::Structure).kickable();
    let exe = std::env::current_exe().ok();
    let mut setup = patch_core::setup::Watched::default();
    loop {
        // The librarian keeps the library analysed (its own idle process),
        // while the DJ Booth is part of the setup.
        if let Some(exe) = &exe
            && !s.retired.load(Ordering::Acquire)
            && setup.get().dj
        {
            crate::librarian::ensure(exe);
        }
        // A retiring engine leaves the graph to its successor.
        if !s.retired.load(Ordering::Acquire) {
            let (arrivals, live, representatives, remembered) = {
                let mut links = s.links.lock().unwrap();
                links.tick();
                let remembered: std::collections::BTreeMap<_, _> = links.stream_profiles.iter()
                    .filter_map(|(app, profile)| Some((app.clone(), profile.eq8.clone()?))).collect();
                (std::mem::take(&mut links.stream_arrivals), links.stream_lanes.assigned.keys().cloned().collect::<std::collections::BTreeSet<_>>(),
                    links.stream_representatives.clone(), remembered)
            };
            let mut eq8_changes = Vec::new();
            {
                let explicit_apps = s.app_eq.lock().unwrap().clone();
                let mut states = s.eq.lock().unwrap();
                // An application keeps the EQ of the stream it was last heard
                // on (read before a departed stream's state is dropped).
                for (app, key) in &representatives {
                    if let Some(state) = states.get(&format!("app:{key}")) && remembered.get(app) != Some(state) {
                        eq8_changes.push((app.clone(), state.clone()));
                    }
                }
                states.retain(|key, _| !key.starts_with("app:stream:") || live.contains(key.trim_start_matches("app:")));
                for (key, app, channel) in arrivals {
                    let target = format!("app:{key}");
                    let state = states.get(&target).or_else(|| explicit_apps.contains_key(&app).then(|| states.get(&format!("app:{app}"))).flatten())
                        .or_else(|| eq8_changes.iter().rev().find(|(a, _)| *a == app).map(|(_, st)| st).or_else(|| remembered.get(&app)))
                        .cloned().unwrap_or_default();
                    if let Some(c) = s.params.channel(&channel) && let Ok(_owner) = s.params.stream_owner(c, &key) { s.params.channel_eq[c].set(&state); }
                    states.insert(target, state);
                }
            }
            if !eq8_changes.is_empty() {
                let mut links = s.links.lock().unwrap();
                for (app, state) in eq8_changes {
                    if let Some(profile) = links.stream_profiles.get_mut(&app) { profile.eq8 = Some(state); }
                }
                crate::stream_lanes::save_profiles(&links.stream_profiles);
            }
            let input_ready = s.links.lock().unwrap().active;
            if input_ready
                && setup.get().system_audio
                && !patch_core::ipc::runtime_dir()
                    .join("input-selection")
                    .exists()
            {
                let default = crate::links::run(&["pactl", "get-default-source"])
                    .map(|source| source.trim().to_string());
                let has_mics = !s.mics.lock().unwrap().is_empty();
                let source = if has_mics {
                    Some(crate::links::INPUT.to_string())
                } else {
                    default.filter(|name| !name.starts_with("patch"))
                };
                if let Some(source) = source {
                    let _ = select_input(&s, &source);
                }
            }
        }
        let (settling, recheck) = {
            let mut l = s.links.lock().unwrap();
            let r = l.recheck;
            l.recheck = r.saturating_sub(1);
            (s.retired.load(Ordering::Acquire) || l.settling(), r)
        };
        // An output switch: two quick passes (0.4 s, then 1.2 s) for the
        // app streams the bar moves after it.
        let wait = match recheck {
            2 => 400,
            1 => 800,
            _ if settling || !watch.live => 500,
            _ => 30_000,
        };
        watch.wait(Duration::from_millis(wait));
    }
}

/// Scheduling of PipeWire's data thread in this process (where the engine's
/// callback runs): (policy name, priority). SCHED_FIFO/RR is real-time.
/// Read again at most every 2 s (it walks /proc/self/task).
pub(super) fn data_thread_sched() -> Option<(&'static str, i64)> {
    type Seen = Option<(std::time::Instant, Option<(&'static str, i64)>)>;
    static LAST: Mutex<Seen> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some((at, v)) = *last
        && at.elapsed().as_secs() < 2
    {
        return v;
    }
    let v = data_thread_sched_now();
    *last = Some((std::time::Instant::now(), v));
    v
}

pub(super) fn data_thread_sched_now() -> Option<(&'static str, i64)> {
    for t in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
        let comm = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        if !comm.starts_with("data-loop") {
            continue;
        }
        let stat = std::fs::read_to_string(t.path().join("stat")).ok()?;
        // Fields after the ")" of comm: state is field 3; rt_priority 40, policy 41.
        let rest: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let prio: i64 = rest.get(37)?.parse().ok()?;
        let policy = match rest.get(38)?.parse::<i64>().ok()? {
            1 => "FIFO",
            2 => "RR",
            _ => "normal",
        };
        return Some((policy, prio));
    }
    None
}

/// A signal counts as live above this peak level.
pub(super) const ACTIVE_DBFS: f32 = -70.0;

/// … within this window.
pub(super) const ACTIVE_NS: u64 = 1_500_000_000;

/// Which channels (and the program) carried signal recently: a Unit with
/// live audio must never be stopped by accident.
pub(super) fn activity(s: &Shared) -> (Vec<bool>, bool) {
    let n = s.params.channels();
    let floor = crate::db_to_linear(ACTIVE_DBFS);
    let since = crate::now_ns().saturating_sub(ACTIVE_NS);
    let mut ch = vec![false; n];
    let mut program = false;
    for (_, b) in s
        .history
        .lock()
        .unwrap()
        .iter()
        .rev()
        .take_while(|(_, b)| b.stamp.nsec >= since)
    {
        for (c, a) in ch.iter_mut().enumerate() {
            *a |= b.ch_peak[c][0].max(b.ch_peak[c][1]) > floor;
        }
        program |= b.out_peak[0].max(b.out_peak[1]) > floor;
    }
    (ch, program)
}

#[cfg(test)]
mod tests {
    use super::apps_on_patch;

    // `pactl list short sinks` and `pactl -f json list sink-inputs` as they
    // read with Patch's windows open (Omarchy 4.0.4, PipeWire 1.6).
    const SINKS: &str = "198\talsa_output.pci-0000_00_05.0.analog-stereo\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
                         220\tpatch\tPipeWire\tfloat32le 2ch 48000Hz\tRUNNING\n\
                         363\tpatch.system-sounds\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n";
    const LOOPBACK: &str = r#"{"index":377,"sink":220,"properties":{"node.name":"patch.system-sounds.loopback","media.name":"loopback-658-13 output"}}"#;

    #[test]
    fn patchs_own_system_sounds_loopback_is_no_app() {
        assert!(!apps_on_patch(SINKS, &format!("[{LOOPBACK}]")));
        assert!(!apps_on_patch(SINKS, "[]"));
        // A failed listing reads as nothing playing.
        assert!(!apps_on_patch(SINKS, ""));
    }

    #[test]
    fn apps_on_the_bus_and_system_sounds_count() {
        let browser = r#"{"index":400,"sink":220,"properties":{"node.name":"Firefox","application.name":"Firefox"}}"#;
        assert!(apps_on_patch(SINKS, &format!("[{LOOPBACK},{browser}]")));
        let alert = r#"{"index":401,"sink":363,"properties":{"node.name":"pw-play","application.name":"pw-play"}}"#;
        assert!(apps_on_patch(SINKS, &format!("[{LOOPBACK},{alert}]")));
        // A stream on the device itself is not Patch's to run for.
        let direct = r#"{"index":402,"sink":198,"properties":{"node.name":"mpv"}}"#;
        assert!(!apps_on_patch(SINKS, &format!("[{LOOPBACK},{direct}]")));
    }
}
