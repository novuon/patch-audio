//! One kept IPC connection, one bounded command queue, one shared snapshot.
//! Browser count never multiplies engine polling or engine threads.
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, Instant};

pub struct Job {
    pub request: Value,
    pub id: String,
    pub deadline: Instant,
    pub reply: mpsc::SyncSender<Result<Value, String>>,
}
/// Whether a phone is looking. Pages read `/api/state` every 250-500 ms
/// while visible and stop when hidden; the engine is polled only while one
/// has read lately (it cost the engine ~25 ms of CPU a second with no phone).
pub struct Demand {
    epoch: Instant,
    /// Milliseconds since `epoch`: the last state read, the last snapshot.
    read: AtomicU64,
    fresh: AtomicU64,
}

/// A phone that read within this keeps the engine polled.
const WATCHED: Duration = Duration::from_secs(5);
/// A snapshot older than this is not served after an idle spell: the read
/// waits (bounded) for a new one.
const STALE: Duration = Duration::from_secs(1);

impl Default for Demand {
    fn default() -> Self {
        Demand {
            epoch: Instant::now(),
            read: AtomicU64::new(0),
            fresh: AtomicU64::new(0),
        }
    }
}

impl Demand {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64 + 1
    }
    fn since(&self, at: &AtomicU64) -> Duration {
        match at.load(Ordering::Acquire) {
            0 => Duration::MAX,
            t => Duration::from_millis(self.now().saturating_sub(t)),
        }
    }
    /// A phone read the state: keep polling, and give it a recent snapshot.
    pub fn read(&self) {
        self.read.store(self.now(), Ordering::Release);
        let until = Instant::now() + Duration::from_millis(500);
        while self.since(&self.fresh) > STALE && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn watched(&self) -> bool {
        self.since(&self.read) < WATCHED
    }
    fn refreshed(&self) {
        self.fresh.store(self.now(), Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Bridge {
    pub snapshot: Arc<RwLock<Value>>,
    pub demand: Arc<Demand>,
    pub commands: mpsc::SyncSender<Job>,
    pub token: Arc<String>,
    pub control: bool,
}
struct Client(BufReader<UnixStream>);
impl Client {
    fn connect() -> Result<Self, String> {
        let root = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is missing")?;
        let stream =
            UnixStream::connect(std::path::PathBuf::from(root).join("omarchy-patch/engine.sock"))
                .map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;
        Ok(Self(BufReader::new(stream)))
    }
    fn call(&mut self, request: &Value) -> Result<Value, String> {
        // One write for the line (formatting into the socket wrote each
        // JSON fragment with its own syscall).
        let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.0.get_mut().write_all(&line).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        self.0
            .by_ref()
            .take(1_048_576)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.last() != Some(&b'\n') {
            return Err("engine reply incomplete or too large".into());
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(error) = v["err"].as_str() {
            return Err(error.into());
        }
        v.get("ok")
            .cloned()
            .ok_or_else(|| "malformed engine reply".into())
    }
}
impl Bridge {
    pub fn start(control: bool) -> anyhow::Result<Self> {
        let mut random = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let token = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let (commands, rx) = mpsc::sync_channel::<Job>(16);
        let snapshot = Arc::new(RwLock::new(
            json!({"connected":false,"sequence":0,"control":control}),
        ));
        let demand = Arc::new(Demand::default());
        let watching = demand.clone();
        let bridge = Self {
            snapshot: snapshot.clone(),
            demand,
            commands,
            token: Arc::new(token),
            control,
        };
        std::thread::Builder::new().name("remote-ipc".into()).spawn(move || {
            let mut client: Option<Client> = None;
            let mut recent = VecDeque::new();
            let mut sequence = 0u64;
            let mut next = Instant::now();
            loop {
                if let Ok(job) = rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                    let result = dispatch(&job, &mut client, &mut recent);
                    let _ = job.reply.send(result);
                    next = Instant::now();
                }
                if Instant::now() >= next && !watching.watched() {
                    // No phone: no engine traffic; look again shortly.
                    next = Instant::now() + Duration::from_millis(100);
                    continue;
                }
                if Instant::now() >= next {
                    let booth = exchange(&mut client, &json!({"op":"dj"})).map(booth_summary);
                    let dj = exchange(&mut client, &json!({"op":"ai_state"}));
                    let mixer = exchange(&mut client, &json!({"op":"status"})).map(mixer_summary);
                    let visual = exchange(&mut client, &json!({"op":"visual_intent","action":"get"}));
                    sequence += 1;
                    *snapshot.write().unwrap() = json!({"connected":booth.is_ok() || dj.is_ok(),"sequence":sequence,"control":control,
                        "booth":booth.as_ref().ok(),"booth_error":booth.err(),"dj":dj.as_ref().ok(),"error":dj.err(),"mixer":mixer.as_ref().ok(),"mixer_error":mixer.err(),"visual":visual.as_ref().ok(),"visual_error":visual.err(),"visual_output":visual_output()});
                    watching.refreshed();
                    next = Instant::now() + Duration::from_millis(250);
                }
            }
        })?;
        Ok(bridge)
    }
}
fn visual_output() -> Option<Value> {
    use std::io::Read;
    let path = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?)
        .join("omarchy-patch/visualiser-status.json");
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    let status: Value = serde_json::from_slice(&bytes).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let updated = status["updated_ms"].as_u64()?;
    if updated > now || now - updated > 3000 {
        return None;
    }
    Some(status)
}
fn exchange(client: &mut Option<Client>, request: &Value) -> Result<Value, String> {
    if client.is_none() {
        *client = Some(Client::connect()?);
    }
    let result = client.as_mut().unwrap().call(request);
    // Never retry a write: a timed-out command may already have executed.
    if result.is_err() {
        *client = None;
    }
    result
}

/// Translate a narrow browser intent; no arbitrary engine requests escape.
pub fn command(v: &Value) -> Result<Value, String> {
    match v["action"].as_str().unwrap_or("") {
        "transport" | "sync" | "fader" | "cue" => {
            let deck = v["deck"]
                .as_str()
                .filter(|d| matches!(*d, "a" | "b"))
                .ok_or("Deck is a or b")?;
            let action = v["action"].as_str().unwrap();
            let request = match action {
                "transport" => {
                    let state = v["value"]
                        .as_str()
                        .filter(|s| matches!(*s, "play" | "pause"))
                        .ok_or("Transport is play or pause")?;
                    json!({"op":"dj.transport","deck":deck,"action":state})
                }
                "sync" => json!({"op":"dj.sync","deck":deck}),
                "cue" => return Ok(json!({"op":"remote_cue","deck":deck})),
                _ => json!({"op":"dj.fader","deck":deck,"position":unit_amount(v)?}),
            };
            Ok(json!({"op":"command","origin":"user","command":request}))
        }
        "crossfader" => Ok(
            json!({"op":"command","origin":"user","command":{"op":"dj.crossfader","position":unit_amount(v)?}}),
        ),
        "mix_now" => {
            Ok(json!({"op":"command","origin":"user","command":{"op":"dj.transition_now"}}))
        }
        "change_next" => Ok(
            json!({"op":"command","origin":"user","command":{"op":"dj.session","request":{"action":"change_next"}}}),
        ),
        "set_start" => Ok(
            json!({"op":"command","origin":"user","command":{"op":"dj.session","request":{"action":"start","set":{"name":"Auto Mix Set","mode":"act"}}}}),
        ),
        "set_resume" => Ok(
            json!({"op":"command","origin":"user","command":{"op":"dj.session","request":{"action":"resume"}}}),
        ),
        "mode" => {
            let mode = v["value"]
                .as_str()
                .filter(|s| matches!(*s, "observe" | "suggest" | "prepare" | "act"))
                .ok_or("invalid AI mode")?;
            Ok(json!({"op":"ai_mode","mode":mode}))
        }
        "direction" => {
            let direction = v["value"]
                .as_str()
                .filter(|s| matches!(*s, "build" | "hold" | "release"))
                .ok_or("invalid direction")?;
            Ok(json!({"op":"automix","direction":direction}))
        }
        "visual" => {
            let field = v["field"]
                .as_str()
                .filter(|s| {
                    matches!(
                        *s,
                        "intensity" | "motion" | "density" | "symmetry" | "feedback"
                    )
                })
                .ok_or("invalid visual parameter")?;
            let amount = v["value"]
                .as_f64()
                .filter(|n| (0.0..=1.0).contains(n))
                .ok_or("amount is 0 to 1")?;
            Ok(json!({"op":"visual_intent","action":"set","origin":"user","intent":{field:amount}}))
        }
        "visual_intent" => {
            let fields = v["intent"]
                .as_object()
                .filter(|f| !f.is_empty() && f.len() <= 11)
                .ok_or("invalid visual intent")?;
            for (field, value) in fields {
                let valid = match field.as_str() {
                    "intensity" | "motion" | "density" | "symmetry" | "feedback" => {
                        value.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n))
                    }
                    "hue" => {
                        value.is_null() || value.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n))
                    }
                    "transition_beats" => value.as_f64().is_some_and(|n| (0.0..=64.0).contains(&n)),
                    "colour" => value.as_str().is_some_and(|s| {
                        matches!(s, "theme" | "hold" | "drift" | "step" | "flash")
                    }),
                    "focus" => value.as_str().is_some_and(|s| {
                        matches!(s, "whole" | "kick" | "bass" | "drums" | "vocals" | "melody")
                    }),
                    "section" => value.as_str().is_some_and(|s| {
                        matches!(
                            s,
                            "unknown"
                                | "intro"
                                | "verse"
                                | "build"
                                | "drop"
                                | "breakdown"
                                | "outro"
                                | "transition"
                        )
                    }),
                    _ => false,
                };
                if !valid {
                    return Err("invalid visual intent field".into());
                }
            }
            Ok(json!({"op":"visual_intent","action":"set","origin":"user","intent":fields}))
        }
        "visual_clear" => Ok(json!({"op":"visual_intent","action":"clear","origin":"user"})),
        "visual_scene" => {
            let part = v["part"].as_str().filter(|p| matches!(*p, "text" | "groove" | "lift" | "calm")).ok_or("invalid choreography part")?;
            let index = v["index"].as_u64().filter(|i| *i < 64).ok_or("invalid scene index")?;
            Ok(json!({"op":"visual_scene_cue","action":"call","part":part,"index":index}))
        }
        "unit_gain" => {
            let channel = v["channel"]
                .as_str()
                .filter(|name| !name.is_empty() && name.len() <= 256 && !name.bytes().any(|b| b.is_ascii_control()))
                .ok_or("Invalid mixer channel")?;
            let db = v["value"]
                .as_f64()
                .filter(|n| (-120.0..=12.0).contains(n))
                .ok_or("Gain is −120 to +12 dB")?;
            Ok(json!({"op":"gain","channel":channel,"db":db}))
        }
        "unit_mute" => {
            let channel = v["channel"]
                .as_str()
                .filter(|name| !name.is_empty() && name.len() <= 256 && !name.bytes().any(|b| b.is_ascii_control()))
                .ok_or("Invalid mixer channel")?;
            let muted = v["value"].as_bool().ok_or("Mute must be true or false")?;
            Ok(json!({"op":"mute","channel":channel,"muted":muted}))
        }
        "unit_master" => {
            let value = &v["value"];
            if !value.is_object() { return Err("Master command needs a value object".into()); }
            if let Some(db) = value["gain_db"].as_f64()
                && !(-120.0..=12.0).contains(&db)
            {
                return Err("Master gain is −120 to +12 dB".into());
            }
            let gain = value["gain_db"].as_f64();
            let mute = value["mute"].as_bool();
            if value.get("gain_db").is_some_and(|v| !v.is_null() && gain.is_none())
                || value.get("mute").is_some_and(|v| !v.is_null() && mute.is_none())
            { return Err("Invalid master control value".into()); }
            if mute.is_none() && gain.is_none() {
                return Err("Master command needs gain or mute".into());
            }
            let mut request = json!({"op":"master"});
            if let Some(db) = gain { request["db"] = json!(db); }
            if let Some(muted) = mute { request["mute"] = json!(muted); }
            Ok(request)
        }
        "mixer_eq" => {
            let deck = remote_deck(v)?;
            let band = v["band"]
                .as_str()
                .filter(|b| matches!(*b, "low" | "mid" | "high"))
                .ok_or("EQ band is low, mid or high")?;
            let db = v["value"]
                .as_f64()
                .filter(|n| (-26.0..=6.0).contains(n))
                .ok_or("EQ is −26 to +6 dB")?;
            Ok(
                json!({"op":"command","origin":"user","command":{"op":"dj.eq","deck":deck,"band":band,"db":db}}),
            )
        }
        "mixer_filter" => {
            let deck = remote_deck(v)?;
            let amount = v["value"]
                .as_f64()
                .filter(|n| (-1.0..=1.0).contains(n))
                .ok_or("Filter is −1 to +1")?;
            Ok(
                json!({"op":"command","origin":"user","command":{"op":"dj.filter","deck":deck,"amount":amount}}),
            )
        }
        "library_track" => {
            let id = v["track_id"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Select a library track")?;
            let kind = v["kind"]
                .as_str()
                .filter(|k| matches!(*k, "load" | "queue"))
                .ok_or("Action is load or queue")?;
            let mut request = json!({"op":"remote_library_track","track_id":id,"kind":kind});
            if kind == "load" {
                request["deck"] = json!(remote_deck(v)?);
            }
            Ok(request)
        }
        _ => Err("unsupported remote action".into()),
    }
}

fn remote_deck(v: &Value) -> Result<&str, String> {
    v["deck"]
        .as_str()
        .filter(|d| matches!(*d, "a" | "b"))
        .ok_or_else(|| "Deck is a or b".into())
}

fn unit_amount(v: &Value) -> Result<f64, String> {
    v["value"]
        .as_f64()
        .filter(|v| (0.0..=1.0).contains(v))
        .ok_or_else(|| "Amount is 0 to 1".into())
}
fn booth_summary(v: Value) -> Value {
    let decks: Vec<Value> = v["decks"]
        .as_array()
        .into_iter()
        .flatten()
        .take(2)
        .map(|d| {
            let mut summary = json!({});
            for key in [
                "loaded",
                "title",
                "artist",
                "playing",
                "position",
                "duration",
                "cue_point",
                "tempo",
                "grid",
                "loop_on",
            ] {
                summary[key] = d[key].clone();
            }
            summary
        })
        .collect();
    json!({"xfader":v["xfader"],"faders":v["faders"],"decks":decks,"set":v["set"],"queue":v["queue"]})
}
fn mixer_summary(v: Value) -> Value {
    let configured_mics: std::collections::HashSet<&str> = v["mics"].as_array().into_iter().flatten()
        .filter_map(|m| m["channel"].as_str()).collect();
    let channels: Vec<Value> = v["channels"].as_array().into_iter().flatten()
        .filter(|c| c["name"].as_str().is_some())
        .filter(|c| c["name"].as_str().is_some_and(|name| !name.starts_with("mic-") || configured_mics.contains(name)))
        .map(|c| json!({"name":c["name"],"gain_db":c["gain_db"],"mute":c["mute"],"active":c["active"],"pan":c["pan"],"eq":c["eq"],"filter":c["eq"]["filter"],"fx":c["fx"],"pfl":c["pfl"],"voice":c["voice"]}))
        .collect();
    json!({"channels":channels,"master_db":v["master_db"],"master_mute":v["master_mute"],"dj_group":v["dj_group"]})
}
type Remembered = (String, Value, Result<Value, String>);
fn dispatch(
    job: &Job,
    client: &mut Option<Client>,
    recent: &mut VecDeque<Remembered>,
) -> Result<Value, String> {
    if let Some((_, request, result)) = recent.iter().find(|(id, _, _)| *id == job.id) {
        return if request == &job.request {
            result.clone()
        } else {
            Err("Command id already used for another action".into())
        };
    }
    let result = if Instant::now() < job.deadline {
        execute(client, &job.request)
    } else {
        Err("Command expired before dispatch".into())
    };
    if recent.len() == 128 {
        recent.pop_front();
    }
    recent.push_back((job.id.clone(), job.request.clone(), result.clone()));
    result
}

fn execute(client: &mut Option<Client>, request: &Value) -> Result<Value, String> {
    if request["op"] == "remote_library_track" {
        let id = request["track_id"]
            .as_i64()
            .ok_or("Select a library track")?;
        let track = patch_library::open_quick()?
            .tracks()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or("Track is no longer in the library")?;
        if track.missing || !std::path::Path::new(&track.path).is_file() {
            return Err("Track file is unavailable".into());
        }
        let command = if request["kind"] == "load" {
            json!({"op":"dj.load_path","deck":request["deck"],"path":track.path})
        } else if request["kind"] == "queue" {
            json!({"op":"dj.queue_add","path":track.path})
        } else {
            return Err("unsupported library action".into());
        };
        return exchange(
            client,
            &json!({"op":"command","origin":"user","command":command}),
        );
    }
    if request["op"] != "remote_cue" {
        return exchange(client, request);
    }
    // No browser-held preview: Safari sleeping cannot leave a cue playing.
    let deck = &request["deck"];
    let state = exchange(client, &json!({"op":"deck_status","deck":deck}))?;
    if state["loaded"] != true {
        return Err("No track loaded".into());
    }
    let cue = state["cue_point"]
        .as_f64()
        .filter(|p| p.is_finite() && *p >= 0.0)
        .ok_or("Cue point unavailable")?;
    exchange(
        client,
        &json!({"op":"command","origin":"user","command":{"op":"dj.transport","deck":deck,"action":"pause"}}),
    )?;
    exchange(
        client,
        &json!({"op":"command","origin":"user","command":{"op":"dj.seek","deck":deck,"seconds":cue}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_engine_is_polled_only_while_a_phone_reads() {
        let d = Demand::default();
        assert!(!d.watched(), "no phone yet: no polling");
        // No snapshot yet: a read waits a bounded while, then answers.
        let t = Instant::now();
        d.read();
        assert!(t.elapsed() >= Duration::from_millis(400) && t.elapsed() < Duration::from_secs(2));
        assert!(d.watched(), "a phone read: poll");
        d.refreshed();
        let t = Instant::now();
        d.read();
        assert!(t.elapsed() < Duration::from_millis(50), "a fresh snapshot is served at once");
        // The phone's last read 10 s ago (pages stop reading when hidden).
        let old = Demand {
            epoch: Instant::now() - Duration::from_secs(10),
            read: AtomicU64::new(1),
            fresh: AtomicU64::new(1),
        };
        assert!(!old.watched(), "an old read stops the polling");
    }
    #[test]
    fn remote_actions_cannot_escape_the_allowlist() {
        for v in [
            json!({"action":"quit"}),
            json!({"action":"mode","value":"quit"}),
            json!({"action":"visual","field":"shader","value":0.5}),
            json!({"action":"visual","field":"motion","value":1.01}),
        ] {
            assert!(command(&v).is_err());
        }
        assert_eq!(
            command(&json!({"action":"visual","field":"motion","value":0.7})).unwrap(),
            json!({"op":"visual_intent","action":"set","origin":"user","intent":{"motion":0.7}})
        );
        assert_eq!(
            command(&json!({"action":"set_start"})).unwrap()["command"]["request"]["action"],
            "start"
        );
        assert_eq!(
            command(&json!({"action":"set_resume"})).unwrap()["command"]["request"]["action"],
            "resume"
        );
    }
    #[test]
    fn ipc_accepts_fragmented_lines_and_engine_rejections() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let join = std::thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["op"],
                "ai_state"
            );
            server.write_all(b"{\"ok\":").unwrap();
            server.write_all(b"{\"mode\":\"suggest\"}}\n").unwrap();
            request.clear();
            reader.read_line(&mut request).unwrap();
            server.write_all(b"{\"err\":\"unsupported op\"}\n").unwrap();
        });
        let mut client = Client(BufReader::new(client));
        assert_eq!(
            client.call(&json!({"op":"ai_state"})).unwrap()["mode"],
            "suggest"
        );
        assert_eq!(
            client.call(&json!({"op":"visual_intent"})).unwrap_err(),
            "unsupported op"
        );
        join.join().unwrap();
    }
    #[test]
    fn booth_commands_use_human_validation_and_enforce_bounds() {
        assert_eq!(
            command(&json!({"action":"transport","deck":"a","value":"pause"})).unwrap(),
            json!({"op":"command","origin":"user","command":{"op":"dj.transport","deck":"a","action":"pause"}})
        );
        assert_eq!(
            command(&json!({"action":"crossfader","value":0.4})).unwrap(),
            json!({"op":"command","origin":"user","command":{"op":"dj.crossfader","position":0.4}})
        );
        for bad in [
            json!({"action":"transport","deck":"a","value":"load"}),
            json!({"action":"transport","deck":"../../engine","value":"play"}),
            json!({"action":"fader","deck":"a","value":1.1}),
            json!({"action":"crossfader","value":-0.1}),
        ] {
            assert!(command(&bad).is_err());
        }
    }
    #[test]
    fn duplicate_command_ids_execute_once_and_conflicts_are_rejected() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            server.write_all(b"{\"ok\":\"accepted\"}\n").unwrap();
            server
                .set_read_timeout(Some(Duration::from_millis(40)))
                .unwrap();
            line.clear();
            assert!(
                reader.read_line(&mut line).is_err(),
                "duplicate reached the engine"
            );
        });
        let mut client = Some(Client(BufReader::new(client)));
        let (reply, _) = mpsc::sync_channel(1);
        let mut job = Job {
            id: "a-unique-command-id".into(),
            request: json!({"op":"command","command":{"op":"dj.sync","deck":"a"}}),
            deadline: Instant::now() + Duration::from_secs(1),
            reply,
        };
        let mut recent = VecDeque::new();
        assert_eq!(
            dispatch(&job, &mut client, &mut recent).unwrap(),
            "accepted"
        );
        assert_eq!(
            dispatch(&job, &mut client, &mut recent).unwrap(),
            "accepted"
        );
        job.request = json!({"op":"quit"});
        assert!(dispatch(&job, &mut client, &mut recent).is_err());
        worker.join().unwrap();
    }
    #[test]
    fn cue_reads_saved_position_then_pauses_and_seeks_without_preview() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            for (expected, reply) in [
                (
                    json!({"op":"deck_status","deck":"b"}),
                    json!({"ok":{"loaded":true,"cue_point":24.5}}),
                ),
                (
                    json!({"op":"command","origin":"user","command":{"op":"dj.transport","deck":"b","action":"pause"}}),
                    json!({"ok":"ok"}),
                ),
                (
                    json!({"op":"command","origin":"user","command":{"op":"dj.seek","deck":"b","seconds":24.5}}),
                    json!({"ok":"ok"}),
                ),
            ] {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected);
                writeln!(server, "{reply}").unwrap();
            }
        });
        let mut client = Some(Client(BufReader::new(client)));
        execute(
            &mut client,
            &command(&json!({"action":"cue","deck":"b"})).unwrap(),
        )
        .unwrap();
        worker.join().unwrap();
    }
}
