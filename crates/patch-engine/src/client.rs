//! Talking to the engine from Patch's UI, the CLI or anything else.
//!
//! Requests are JSON lines, replies `{"ok": …}` or `{"err": "…"}`:
//! `status`, `meters {since}`, `gain {channel, db}`, `mute {channel, muted}`,
//! `pan {channel, pan}`, `master {db}`, `output {sink|null}`,
//! `reset_loudness`, `quit`.
//!
//! A reply is one line, at most 1 MB. The read has a single deadline for
//! the whole line (500 ms, 30 s for a set start or a routing change, 90 s
//! while a transition is reviewed), so a trickle cannot grow the buffer
//! past that cap.

use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

thread_local! {
    /// This thread's connection, kept open: every new connection costs the
    /// engine a thread (the UI once made ~150 a second).
    static KEPT: std::cell::RefCell<Option<Client>> = const { std::cell::RefCell::new(None) };
}

/// One JSON line in one write. `writeln!(stream, "{value}")` formats
/// straight into the unbuffered socket: a syscall per key, colon and value,
/// about 150 for a deck status, a third of what the engine spent on it.
pub fn write_line(out: &mut impl Write, value: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    out.write_all(&line)
}

/// A request on this thread's kept connection (made on first use). Any
/// error drops it, so a late reply can never answer the next request.
pub fn call(req: &Value) -> Result<Value, String> {
    KEPT.with(|k| {
        let mut k = k.borrow_mut();
        if k.is_none() {
            *k = Some(Client::connect().ok_or("the engine is not running")?);
        }
        // Outer error: the connection broke (drop it); inner: the engine's
        // answer was an error (the connection is fine).
        match k.as_mut().map(|c| c.exchange(req)) {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                *k = None;
                Err(e)
            }
            None => Err("no connection".into()),
        }
    })
}

/// A request on a new connection (to whichever engine holds the socket).
pub fn call_fresh(req: &Value) -> Result<Value, String> {
    let mut c = Client::connect().ok_or("the engine is not running")?;
    c.call(req)
}

/// Forget this thread's kept connection (the next call connects anew).
pub fn drop_kept() {
    KEPT.with(|k| *k.borrow_mut() = None);
}

pub fn is_running() -> bool {
    call(&json!({"op": "ping"})).is_ok()
}

/// Whether this process runs inside a unit that belongs to Patch's own
/// service, whose lifetime the engine is meant to share.
fn in_patch_service(cgroup: &str) -> bool {
    cgroup.contains("omarchy-patch.service")
}

/// Start `<exe> <args>` detached, with `env`. Started from a terminal the
/// engine stayed in that terminal's cgroup: closing the terminal, or the
/// out-of-memory daemon killing it, took the engine and the music along. It
/// gets a scope of its own; without a user manager, a plain detached process.
fn spawn_detached(exe: &Path, args: &[&str], env: &[(&str, &str)]) -> std::io::Result<()> {
    let quiet = |mut c: Command| {
        c.envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    };
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    if !in_patch_service(&cgroup) {
        let mut scoped = Command::new("systemd-run");
        scoped
            .args(["--user", "--scope", "--quiet", "--collect", "--", "setsid", "-f"])
            .arg(exe)
            .args(args);
        if quiet(scoped).status().is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    let mut plain = Command::new("setsid");
    plain.arg("-f").arg(exe).args(args);
    quiet(plain).status().map(|_| ())
}

/// Start the engine as a detached process (`<exe> engine`) unless it runs.
pub fn ensure_running(exe: &Path) -> Result<(), String> {
    if is_running() {
        return Ok(());
    }
    spawn_detached(exe, &["engine"], &[]).map_err(|e| e.to_string())?;
    for _ in 0..80 {
        if is_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("the engine didn't start".into())
}

/// How long a reply may take: most are immediate; a set start loads a track.
const REPLY: Duration = Duration::from_millis(500);
const SLOW_REPLY: Duration = Duration::from_secs(30);
/// A transition review stops starting rehearsals after 20 s but finishes the
/// one under way, live with stems often seconds each: past 30 s the window
/// reported "Resource temporarily unavailable" while the engine applied it.
const REVIEW_REPLY: Duration = Duration::from_secs(90);

/// How long to wait for this reply. Ordinary calls use `normal` (500 ms on
/// a fresh connection, 1 s while talking to a retiring engine).
///
/// A set start loads a track. A transition review finishes the rehearsal
/// already under way. Routing takes the link lock and runs a keeper pass
/// (`pactl`, `pw-link`), and it waits out a pass the keeper is already in.
/// On a large graph — the routing test does this after a 16-channel input —
/// that is longer than half a second. Cutting it short looks like the
/// engine died (`engine is not running`) while the pass is still applying.
fn reply_limit(req: &Value, normal: Duration) -> Duration {
    if req["op"] == "plan_adjust" {
        return REVIEW_REPLY;
    }
    let keeper = matches!(
        req["op"].as_str(),
        Some(
            "set_start"
                | "output"
                | "output_route"
                | "output_route_clear"
                | "output_select"
                | "cue_output"
                | "deck_sinks"
                | "input_select"
                | "mic"
        )
    );
    let session = req["op"] == "command"
        && matches!(req["command"]["op"].as_str(), Some("dj.set" | "dj.session"));
    if keeper || session { SLOW_REPLY } else { normal }
}

/// A kept-open connection (the UI polls meters ~30 times a second).
pub struct Client {
    stream: UnixStream,
    reader: crate::ipc_line::LineReader,
    /// Deadline for an ordinary reply. Slow ops use a longer one.
    reply_budget: Duration,
}

impl Client {
    pub fn connect() -> Option<Client> {
        let stream = UnixStream::connect(crate::daemon::socket_path()).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .ok()?;
        let reader = crate::ipc_line::LineReader::new(stream.try_clone().ok()?);
        Some(Client {
            stream,
            reader,
            reply_budget: REPLY,
        })
    }

    #[cfg(test)]
    fn for_test(stream: UnixStream, reply_budget: Duration) -> Self {
        let reader = crate::ipc_line::LineReader::new(stream.try_clone().unwrap());
        Self {
            stream,
            reader,
            reply_budget,
        }
    }

    pub fn call(&mut self, req: &Value) -> Result<Value, String> {
        self.exchange(req)?
    }

    /// Outer error: the transport failed; inner: the engine's reply.
    pub fn exchange(&mut self, req: &Value) -> Result<Result<Value, String>, String> {
        write_line(&mut self.stream, req).map_err(|e| e.to_string())?;
        // One deadline for the whole line, not a timeout that restarts on
        // every chunk. Routing and a set start wait out a keeper pass or a
        // track load; cutting that short looks like the engine died.
        let wait = reply_limit(req, self.reply_budget);
        let line = self
            .reader
            .read_line(crate::ipc_line::REPLY_LIMIT, wait, false)
            .map_err(|e| match e {
                crate::ipc_line::LineError::TooLarge => "engine reply too large".to_string(),
                other => other.to_string(),
            })?;
        let mut v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        Ok(match (v.get_mut("ok").map(Value::take), v.get("err")) {
            (Some(ok), _) => Ok(ok),
            (_, Some(err)) => Err(err.as_str().unwrap_or("error").to_string()),
            _ => return Err("malformed reply".into()),
        })
    }
}

/// Engine channels carrying signal right now (peak above −70 dBFS within
/// the last 1.5 s), and whether the program does. Asked fresh, for guards:
/// a Unit with live audio must never be stopped by accident.
pub fn live() -> Result<(Vec<String>, bool), String> {
    let s = call(&json!({"op": "status"}))?;
    let chans = s["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["active"].as_bool().unwrap_or(false))
        .filter_map(|c| c["name"].as_str().map(String::from))
        .collect();
    Ok((chans, s["program_active"].as_bool().unwrap_or(false)))
}

/// Deck channels with live audio ("deck-a" through "deck-d").
pub fn live_decks() -> Vec<String> {
    live()
        .map(|(c, _)| c.into_iter().filter(|n| n.starts_with("deck-")).collect())
        .unwrap_or_default()
}

/// Replace the running engine with `exe`'s without a gap in the audio.
///
/// The old engine steps aside but keeps playing (node renamed, socket moved,
/// its link keeper stopped); the new one starts silent as `patch.engine`,
/// links itself on the same graph clock, takes over the mixer state and the
/// loaded (paused) decks, and the two crossfade sample-aligned over
/// `FADE_MS` — the same input, so the sum stays constant. Then the old one
/// quits without handing the bus to a loopback. If anything fails before
/// the fade, the old engine takes back over and nothing was heard.
///
/// A deck that is playing can't be carried across exactly: refused unless
/// `force` (the deck then stops).
pub fn upgrade(exe: &Path, force: bool, log: &mut dyn FnMut(&str)) -> Result<(), String> {
    // Two engines take turns at the socket: every request goes over a new
    // connection, to whichever engine holds it now. (This thread's kept
    // connection would reach the old engine after the handover: the new
    // one then never got its settings or its fade-up, and stayed silent.)
    let call = |req: &Value| call_fresh(req);
    drop_kept();
    if !is_running() {
        return ensure_running(exe);
    }
    let old = call(&json!({"op": "status"}))?;
    let mut decks = vec![];
    for d in ["a", "b", "c", "d"] {
        let Ok(st) = call(&json!({"op": "deck_status", "deck": d})) else {
            break;
        };
        if st["playing"].as_bool().unwrap_or(false) && !force {
            return Err(format!(
                "deck {} is playing: an upgrade can't carry a playing deck across (pause it, or --force)",
                d.to_uppercase()
            ));
        }
        decks.push((d, st));
    }
    log(&format!("old engine pid {}; retiring it", old["pid"]));
    call(&json!({"op": "retire"})).map_err(|e| {
        if e.contains("unknown op") {
            "the running engine predates gapless handovers: this once it needs a plain restart (a short gap; `omarchy-patch engine quit` then `engine start`)".to_string()
        } else {
            e
        }
    })?;
    let back = |why: String| -> String {
        let _ = retiring_call(&json!({"op": "unretire"}));
        format!("{why}; the old engine carries on")
    };
    // The successor: silent until the crossfade.
    let spawned = spawn_detached(exe, &["engine"], &[("PATCH_ENGINE_SILENT", "1")]);
    if let Err(e) = spawned {
        return Err(back(e.to_string()));
    }
    // Linked to the output and running cycles.
    let t0 = std::time::Instant::now();
    let new = loop {
        if let Ok(st) = call(&json!({"op": "status"}))
            && st["linked"].as_bool().unwrap_or(false)
            && st["cycles"].as_u64().unwrap_or(0) > 10
        {
            break st;
        }
        if t0.elapsed() > Duration::from_secs(8) {
            // A half-started successor must not keep the socket.
            let _ = call(&json!({"op": "quit", "handover": true}));
            std::thread::sleep(Duration::from_millis(300));
            return Err(back("the new engine didn't link to the output".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    log(&format!("new engine pid {} linked", new["pid"]));
    // Everything restored below lands on an engine every window is just
    // reconnecting to: some requests then get "too many connections" (or a
    // refused connection). Each is retried until it lands, for a while; one
    // that never does is said, not dropped (a deck once came back without
    // its grid, at 0 s, and the AI DJ could not plan).
    let lost = std::cell::RefCell::new(Vec::<String>::new());
    let put = |req: &Value| -> Option<Value> {
        let t0 = std::time::Instant::now();
        loop {
            match call(req) {
                Ok(v) => return Some(v),
                Err(e) if transient(&e) && t0.elapsed() < Duration::from_secs(4) => {
                    std::thread::sleep(Duration::from_millis(60));
                }
                Err(e) => {
                    lost.borrow_mut().push(format!("{}: {e}", req["op"].as_str().unwrap_or("?")));
                    return None;
                }
            }
        }
    };
    // Give up before the crossfade: the successor goes (it never sounded)
    // and the old engine takes requests again, as it was.
    let abandon = |why: &str| -> String {
        let why = match lost.borrow().last() {
            Some(last) => format!("{why} ({last})"),
            None => why.to_string(),
        };
        let _ = call(&json!({"op": "quit", "handover": true}));
        std::thread::sleep(Duration::from_millis(300));
        back(why)
    };
    // Mixer state.
    for c in old["channels"].as_array().into_iter().flatten() {
        let ch = c["name"].clone();
        put(&json!({"op": "gain", "channel": ch, "db": c["gain_db"]}));
        put(&json!({"op": "mute", "channel": ch, "muted": c["mute"]}));
        if c["pan"].is_number() {
            put(&json!({"op": "pan", "channel": ch, "pan": c["pan"]}));
        }
        if c["trim_db"].is_number() {
            put(&json!({"op": "trim", "channel": ch, "db": c["trim_db"]}));
        }
        for (b, send) in c["sends"].as_array().into_iter().flatten().enumerate() {
            put(&json!({"op": "send", "channel": ch, "bus": b, "db": send["db"], "pre": send["pre"]}));
        }
        let e = &c["eq"];
        put(
            &json!({"op": "eq", "channel": ch, "low_db": e["low_db"], "mid_db": e["mid_db"], "high_db": e["high_db"], "filter": e["filter"]}),
        );
        put(&json!({"op": "pfl", "channel": ch, "on": c["pfl"]}));
        let f = &c["fx"];
        if f.is_object() {
            put(
                &json!({"op": "fx", "channel": ch, "kind": f["kind"], "chain": f["chain"], "on": f["on"], "beats": f["beats"], "depth": f["depth"]}),
            );
        }
    }
    put(&json!({"op": "cue", "mix": old["cue_mix"], "level_db": old["cue_level_db"]}));
    // The Mixer's buses, the DJ Booth's sends and the returns' effects.
    let buses = &old["mixer_buses"];
    for (b, bus) in buses["buses"].as_array().into_iter().flatten().enumerate() {
        put(&json!({"op": "bus", "bus": b, "db": bus["gain_db"], "mute": bus["mute"]}));
    }
    for (b, send) in buses["dj_sends"].as_array().into_iter().flatten().enumerate() {
        put(&json!({"op": "send", "channel": "dj", "bus": b, "db": send["db"], "pre": send["pre"]}));
    }
    if buses["aux_fx"].is_object() {
        let mut fx = buses["aux_fx"].clone();
        fx["op"] = json!("aux_fx");
        put(&fx);
    }
    log("mixer state copied");
    if old["output_chosen"].is_string() {
        put(&json!({"op": "output", "sink": old["output_chosen"]}));
    }
    // Decks: the same track, grid, tempo, key and cue point, paused where
    // they were.
    for (d, st) in &decks {
        let Some(path) = st["path"]
            .as_str()
            .filter(|p| !p.is_empty() && st["loaded"].as_bool().unwrap_or(false))
        else {
            continue;
        };
        let deck = |extra: Value| {
            let mut req = json!({"op": "deck", "deck": d});
            for (k, v) in extra.as_object().into_iter().flatten() {
                req[k] = v.clone();
            }
            put(&req)
        };
        // The load is sent once (a retry would load the track again); a
        // reply that never came (a big file, a busy engine) is not a
        // failure until the deck shows another track or none.
        let on_deck = || {
            call(&json!({"op": "deck_status", "deck": d}))
                .is_ok_and(|n| n["path"].as_str() == Some(path) && n["loaded"].as_bool().unwrap_or(false))
        };
        if call(&json!({"op": "deck", "deck": d, "do": "load", "path": path})).is_err() {
            let t0 = std::time::Instant::now();
            while !on_deck() && t0.elapsed() < Duration::from_secs(15) {
                std::thread::sleep(Duration::from_millis(100));
            }
            if !on_deck() {
                log(&format!("deck {d}: couldn't reload {path}"));
                lost.borrow_mut().push(format!("deck {d}: its track"));
                continue;
            }
        }
        // Seek only once the new engine has decoded up to there (a seek
        // past the decoded part left the deck at 0 s).
        let at = st["position"].as_f64().unwrap_or(0.0);
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            if let Ok(n) = call(&json!({"op": "deck_status", "deck": d})) {
                let length = n["duration"].as_f64().unwrap_or(0.0);
                if n["decode_done"].as_bool().unwrap_or(false)
                    || (length > 0.0 && n["decoded"].as_f64().unwrap_or(0.0) * length >= at + 2.0)
                {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // No grid on the old deck (lost in an earlier handover): the track's
        // own, from its analysis, as a load sets it.
        let analysed = (!st["grid"].is_object())
            .then(|| patch_analysis::cached(std::path::Path::new(path)))
            .flatten()
            .and_then(|mut a| a.use_grid().then_some(a));
        if let Some(a) = &analysed {
            deck(json!({"do": "grid", "bpm": a.bpm, "offset": a.beat_offset, "downbeat": a.downbeat}));
        }
        let g = &st["grid"];
        if g.is_object() {
            let bpm = g["bpm"].as_f64().unwrap_or(0.0);
            let old_offset = g["offset"].as_f64().unwrap_or(0.0);
            let fit = patch_analysis::kick_grid_for(std::path::Path::new(path), bpm);
            let offset = fit.map_or(old_offset, |fit| fit.offset);
            let bpm = fit.map_or(bpm, |fit| fit.bpm);
            let beat = if bpm > 0.0 { 60.0 / bpm } else { 0.0 };
            let shift = if beat > 0.0 {
                (offset - old_offset + beat / 2.0).rem_euclid(beat) - beat / 2.0
            } else {
                0.0
            };
            let downbeat = if shift.abs() > beat * 0.1 {
                Value::Null
            } else {
                g["downbeat"].clone()
            };
            deck(json!({"do": "grid", "bpm": bpm, "offset": offset, "downbeat": downbeat}));
        }
        deck(json!({"do": "tempo", "value": st["tempo"]}));
        deck(json!({"do": "keylock", "on": st["keylock"]}));
        deck(json!({"do": "key", "semitones": st["key"]}));
        if let Some(c) = st["cue_point"].as_f64() {
            deck(json!({"do": "seek", "seconds": c}));
            deck(json!({"do": "cue"}));
        }
        // The place, until it holds: a seek past what the new engine has
        // decoded so far doesn't land, so it is read back and sent again.
        let t0 = std::time::Instant::now();
        loop {
            deck(json!({"do": "seek", "seconds": at}));
            std::thread::sleep(Duration::from_millis(200));
            let held = call(&json!({"op": "deck_status", "deck": d}))
                .is_ok_and(|n| (n["position"].as_f64().unwrap_or(0.0) - at).abs() <= 1.0);
            if held || t0.elapsed() > Duration::from_secs(30) {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        log(&format!(
            "deck {d}: {path} at {:.1} s",
            st["position"].as_f64().unwrap_or(0.0)
        ));
    }
    for (d, st) in &decks {
        // The deck's reported place lags a seek by a moment.
        let was = st["position"].as_f64().unwrap_or(0.0);
        let t0 = std::time::Instant::now();
        let mut status = call(&json!({"op": "deck_status", "deck": d}));
        while status.as_ref().is_ok_and(|n| (n["position"].as_f64().unwrap_or(0.0) - was).abs() > 1.0)
            && t0.elapsed() < Duration::from_secs(1)
        {
            std::thread::sleep(Duration::from_millis(100));
            status = call(&json!({"op": "deck_status", "deck": d}));
        }
        let Ok(n) = status else {
            continue;
        };
        if (st["grid"].is_object() || patch_analysis::cached(std::path::Path::new(st["path"].as_str().unwrap_or(""))).is_some_and(|mut a| a.use_grid()))
            && st["loaded"].as_bool().unwrap_or(false)
            && !n["grid"].is_object()
        {
            lost.borrow_mut().push(format!("deck {d}: its beat grid"));
        }
        let now = n["position"].as_f64().unwrap_or(0.0);
        if st["loaded"].as_bool().unwrap_or(false) && (was - now).abs() > 1.0 {
            lost.borrow_mut().push(format!("deck {d}: its place ({now:.1} s, was {was:.1} s)"));
        }
    }
    for what in lost.borrow().iter() {
        log(&format!("not carried across: {what}"));
    }
    // A --force upgrade carries a playing deck across playing: the new
    // deck jumps to where the old one is now and plays, then the crossfade
    // hands over. It came back paused and the agent that installed had to
    // press play; on 2026-11-16 none did and the AI DJ's set sat silent
    // ("a set doesn't even start anymore"). A deck the DJ paused stays
    // paused (it was not playing).
    for (d, st) in &decks {
        if !(st["playing"].as_bool().unwrap_or(false) && st["loaded"].as_bool().unwrap_or(false)) {
            continue;
        }
        let now = retiring_call(&json!({"op": "deck_status", "deck": d}))
            .ok()
            .and_then(|n| n["position"].as_f64())
            .unwrap_or_else(|| st["position"].as_f64().unwrap_or(0.0));
        // Retried like the restore above (windows are reconnecting). A deck
        // that cannot be carried across playing ends the upgrade with the old
        // engine carrying on: a plain `?` here left the old engine retired
        // (sounding, but deaf to every request) and the new one silent.
        let carried = put(&json!({"op": "deck", "deck": d, "do": "seek", "seconds": now}))
            .and_then(|_| put(&json!({"op": "deck", "deck": d, "do": "play"})));
        if carried.is_none() {
            return Err(abandon(&format!("deck {d} could not be carried across playing")));
        }
        log(&format!("deck {d}: playing on from {now:.1} s"));
    }
    log("crossfading");
    // The crossfade: same input on the same clock, so the sum is constant.
    let master = old["master_db"].as_f64().unwrap_or(0.0);
    if put(&json!({"op": "fade", "db": master, "ms": FADE_MS})).is_none() {
        return Err(abandon("the new engine did not take the crossfade"));
    }
    let _ = retiring_call(&json!({"op": "fade", "db": -120.0, "ms": FADE_MS}));
    std::thread::sleep(Duration::from_millis(FADE_MS as u64 + 100));
    let _ = retiring_call(&json!({"op": "quit", "handover": true}));
    // The new topology can now take over live stream inputs. Before this,
    // both engines must receive the same input throughout the crossfade.
    let _ = call(&json!({"op": "handover_complete"}));
    drop_kept();
    log("handed over");
    Ok(())
}

/// A refusal that passes: the engine busy with connections, or not
/// listening yet.
fn transient(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    ["too many connections", "not running", "connection", "timed out", "deadline", "broken pipe", "temporarily"]
        .iter()
        .any(|t| e.contains(t))
}

/// Crossfade between the old and the new engine.
pub const FADE_MS: f64 = 150.0;

/// A request to the retiring engine during a handover.
fn retiring_call(req: &Value) -> Result<Value, String> {
    let stream =
        UnixStream::connect(crate::daemon::retiring_socket_path()).map_err(|e| e.to_string())?;
    let reader = crate::ipc_line::LineReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    Client {
        stream,
        reader,
        reply_budget: Duration::from_millis(1000),
    }
    .call(req)
}

/// This binary's build: its modification time (Unix seconds), read once at
/// start (a rebuild replaces the file under a running process).
pub fn build_stamp() -> u64 {
    static STAMP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *STAMP.get_or_init(|| {
        std::env::current_exe()
            .and_then(std::fs::metadata)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

/// When the running engine is an older build than this binary and no deck
/// plays, hand over to this build in the background (gapless). Returns
/// whether an upgrade was started.
pub fn upgrade_if_outdated(exe: &Path) -> bool {
    let Ok(st) = call(&json!({"op": "status"})) else {
        return false;
    };
    // No stamp: an older engine. One that can't hand over refuses the
    // upgrade and simply keeps playing.
    let engine = st["build"].as_u64().unwrap_or(0);
    if engine >= build_stamp() || !live_decks().is_empty() {
        return false;
    }
    Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .args(["engine", "upgrade"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_patch_s_own_service_keeps_the_engine_in_its_cgroup() {
        assert!(in_patch_service(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/omarchy-patch.service\n"
        ));
        assert!(!in_patch_service(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-Hyprland-kitty.scope\n"
        ));
        assert!(!in_patch_service("0::/user.slice/user-1000.slice/session-345.scope\n"));
    }

    #[test]
    fn a_json_line_goes_out_in_one_write() {
        struct Counting(Vec<u8>, usize);
        impl Write for Counting {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.1 += 1;
                self.0.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let reply = json!({"ok": {"loaded": true, "path": "a \"b\".mp3", "grid": {"bpm": 124.0}}});
        let mut out = Counting(Vec::new(), 0);
        write_line(&mut out, &reply).unwrap();
        assert_eq!(out.1, 1, "one write, not one per JSON fragment");
        assert_eq!(out.0.last(), Some(&b'\n'));
        let back: Value = serde_json::from_slice(&out.0).unwrap();
        assert_eq!(back, reply);
    }

    #[test]
    fn routing_replies_wait_out_a_keeper_pass() {
        assert_eq!(reply_limit(&json!({"op": "gain"}), REPLY), REPLY);
        assert_eq!(reply_limit(&json!({"op": "output"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "output_route"}), REPLY), SLOW_REPLY);
        assert_eq!(
            reply_limit(&json!({"op": "output_route_clear"}), REPLY),
            SLOW_REPLY
        );
        assert_eq!(reply_limit(&json!({"op": "output_select"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "cue_output"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "deck_sinks"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "input_select"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "mic"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "set_start"}), REPLY), SLOW_REPLY);
        assert_eq!(reply_limit(&json!({"op": "plan_adjust"}), REPLY), REVIEW_REPLY);
        assert_eq!(
            reply_limit(&json!({"op": "command", "command": {"op": "dj.set"}}), REPLY),
            SLOW_REPLY
        );
        assert_eq!(
            reply_limit(&json!({"op": "command", "command": {"op": "dj.session"}}), REPLY),
            SLOW_REPLY
        );
        let retiring = Duration::from_millis(1000);
        assert_eq!(reply_limit(&json!({"op": "gain"}), retiring), retiring);
        assert_eq!(reply_limit(&json!({"op": "output_route"}), retiring), SLOW_REPLY);
    }

    #[test]
    fn a_busy_engine_is_retried_a_real_error_is_not() {
        assert!(super::transient("too many connections"));
        assert!(super::transient("the engine is not running"));
        assert!(super::transient("Connection refused (os error 111)"));
        assert!(super::transient("engine I/O deadline"));
        assert!(!super::transient("unknown op 'pan'"));
        assert!(!super::transient("deck must be a, b, c, or d"));
    }
    #[test]
    fn exchange_reads_one_reply() {
        let (mut server, stream) = UnixStream::pair().unwrap();
        let mut client = Client::for_test(stream, Duration::from_secs(1));
        let writer = std::thread::spawn(move || {
            // A server must read the request before replying and closing;
            // otherwise its early close races the client's request write.
            use std::io::BufRead;
            let mut request = String::new();
            std::io::BufReader::new(&mut server).read_line(&mut request).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&request).unwrap()["op"], "ping");
            server
                .write_all(b"{\"ok\":{\"pong\":true}}\n")
                .unwrap();
        });
        let value = client
            .exchange(&json!({"op": "ping"}))
            .unwrap()
            .unwrap();
        assert_eq!(value["pong"], true);
        writer.join().unwrap();
    }

    #[test]
    fn exchange_rejects_an_oversized_reply() {
        let (mut server, stream) = UnixStream::pair().unwrap();
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut client = Client::for_test(stream, Duration::from_secs(2));
        let writer = std::thread::spawn(move || {
            let mut body = vec![b' '; crate::ipc_line::REPLY_LIMIT];
            body.extend_from_slice(b"x\n");
            let _ = server.write_all(&body);
        });
        let err = client.exchange(&json!({"op": "ping"})).unwrap_err();
        drop(client);
        writer.join().unwrap();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn reply_wait_is_a_single_deadline() {
        let (server, stream) = UnixStream::pair().unwrap();
        let mut client = Client::for_test(stream, Duration::from_millis(80));
        let started = std::time::Instant::now();
        let err = client.exchange(&json!({"op": "ping"})).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "reply wait stretched past one deadline"
        );
        assert!(err.contains("deadline"), "{err}");
        drop(server);
    }
}
