//! What the Network window and `omarchy-patch network` ask for: one JSON
//! request in, one JSON reply out (called off the UI thread).

use crate::config::{Config, Feed, Proto, Receive};
use crate::firewall::Firewall;
use crate::link::{self, Link};
use crate::sdp::Announced;
use serde_json::{Value, json};
use std::path::Path;

fn link_state(id: &str) -> (String, String, u64) {
    match link::status(id) {
        Some(v) => (
            v["state"].as_str().unwrap_or("starting").to_string(),
            v["note"].as_str().unwrap_or_default().to_string(),
            v["restarts"].as_u64().unwrap_or(0),
        ),
        None => ("off".into(), String::new(), 0),
    }
}

/// Is this announced stream one Patch should offer here? Not this
/// machine's own, and not a unicast stream meant for another machine.
pub fn offered(a: &Announced) -> bool {
    if crate::net::is_local(a.origin) {
        return false;
    }
    match a.to {
        Some(to) => crate::net::is_local(to),
        None => true,
    }
}

fn stream_json(a: &Announced, cfg: &Config) -> Value {
    let id = format!("rx-{}", a.id);
    let listening = cfg.receives.iter().any(|r| r.id == id) && link::running(&id);
    json!({
        "id": id,
        "name": a.name,
        "origin": a.origin.to_string(),
        "format": a.format_label(),
        "supported": a.supported(),
        "multicast": a.multicast(),
        "ptp": a.ptp,
        "tool": a.tool,
        "listening": listening,
    })
}

/// Everything the window shows.
pub fn status() -> Value {
    let cfg = Config::load();
    let host = crate::net::primary();
    let firewall = host
        .as_ref()
        .map(|h| crate::firewall::check(&h.subnet()))
        .unwrap_or(Firewall::Off);
    let announced = crate::discover::streams();
    let sends: Vec<Value> = cfg
        .sends
        .iter()
        .map(|s| {
            let (state, note, restarts) = link_state(&s.id);
            let (ip, port) = s.destination();
            json!({
                "id": s.id,
                "name": s.name,
                "feed": s.feed.key(),
                "feed_label": s.feed.label(),
                "proto": s.proto.label(),
                "to": s.to_label(),
                "address": format!("{ip}:{port}"),
                "on": s.on,
                "state": state,
                "note": note,
                "restarts": restarts,
            })
        })
        .collect();
    let receives: Vec<Value> = cfg
        .receives
        .iter()
        .map(|r| {
            let (state, note, restarts) = link_state(&r.id);
            let announced = announced.iter().any(|a| format!("rx-{}", a.id) == r.id);
            json!({
                "id": r.id,
                "name": r.name,
                "origin": r.origin,
                "format": crate::sdp::format_label(&r.encoding, r.rate, r.channels),
                "on": r.on,
                "state": state,
                "note": note,
                "restarts": restarts,
                "announced": announced,
            })
        })
        .collect();
    let streams: Vec<Value> = announced
        .iter()
        .filter(|a| offered(a))
        .map(|a| stream_json(a, &cfg))
        .collect();
    let (vban_state, vban_note, _) = link_state("vban");
    let fw = match &firewall {
        Firewall::Blocked { command } => json!({"state": "blocked", "command": command}),
        f => json!({"state": f.state()}),
    };
    json!({
        "host": host.as_ref().map(|h| json!({"ip": h.ip.to_string(), "iface": h.iface, "subnet": h.subnet()})),
        "hostname": crate::net::hostname(),
        "firewall": fw,
        "latency_ms": cfg.latency(),
        "vban": {"on": cfg.vban, "state": vban_state, "note": vban_note},
        "sends": sends,
        "receives": receives,
        "streams": streams,
        "listening_for": crate::discover::listening_for().as_secs(),
        "discovery_error": crate::discover::error(),
    })
}

fn exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

fn start_receive(exe: &Path, r: &Receive, latency_ms: u32) -> Result<(), String> {
    let l = Link::Receive { receive: r.clone(), latency_ms };
    // Check it makes a configuration before a service is started for it.
    l.conf(None)?;
    link::start(exe, &l)
}

/// Start every link that is switched on and not running (the audio
/// service calls this as it starts, so a send set up once keeps going
/// after a restart or a new login).
pub fn restore(exe: &Path) -> Vec<String> {
    let cfg = Config::load();
    let mut errors = Vec::new();
    for s in cfg.sends.iter().filter(|s| s.on && !link::running(&s.id)) {
        if let Err(e) = link::start(exe, &Link::Send { send: s.clone() }) {
            errors.push(format!("{}: {e}", s.name));
        }
    }
    for r in cfg.receives.iter().filter(|r| r.on && !link::running(&r.id)) {
        if let Err(e) = start_receive(exe, r, cfg.latency()) {
            errors.push(format!("{}: {e}", r.name));
        }
    }
    if cfg.vban
        && !link::running("vban")
        && let Err(e) = link::start(exe, &Link::Vban { latency_ms: cfg.latency() })
    {
        errors.push(format!("VBAN: {e}"));
    }
    errors
}

/// Stop every link and switch it off (the bar's Stop, `network stop-all`).
/// Returns how many were running.
pub fn stop_all() -> Result<usize, String> {
    let mut cfg = Config::load();
    let running = link::all_running().len();
    for s in &mut cfg.sends {
        s.on = false;
    }
    for r in &mut cfg.receives {
        r.on = false;
    }
    cfg.vban = false;
    cfg.save()?;
    for (id, _) in link::all_running() {
        link::stop(&id);
    }
    Ok(running)
}

/// One line for the bar: what the running links do ("Sending Program ·
/// listening to Studio"), or None when none runs.
pub fn summary() -> Option<String> {
    let running = link::all_running();
    if running.is_empty() {
        return None;
    }
    let cfg = Config::load();
    let mut parts = Vec::new();
    let sends: Vec<&str> = cfg
        .sends
        .iter()
        .filter(|s| running.iter().any(|(id, _)| *id == s.id))
        .map(|s| s.feed.label())
        .collect();
    if !sends.is_empty() {
        parts.push(format!("Sending {}", sends.join(", ")));
    }
    let receives: Vec<&str> = cfg
        .receives
        .iter()
        .filter(|r| running.iter().any(|(id, _)| *id == r.id))
        .map(|r| r.name.as_str())
        .collect();
    match receives.len() {
        0 => {}
        1 => parts.push(format!("listening to {}", receives[0])),
        n => parts.push(format!("listening to {n} streams")),
    }
    if running.iter().any(|(id, _)| id == "vban") {
        parts.push("taking VBAN".into());
    }
    if running.iter().any(|(_, v)| v["state"] == "waiting") {
        parts.push("waiting for the engine".into());
    }
    let mut line = parts.join(" · ");
    if let Some(c) = line.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    Some(line)
}

fn text<'a>(req: &'a Value, key: &str) -> &'a str {
    req[key].as_str().unwrap_or_default()
}

fn reply(message: impl Into<String>) -> Result<Value, String> {
    Ok(json!({"message": message.into(), "status": status()}))
}

/// One request: `op` is status, send, start, stop, remove, listen, vban,
/// latency or allow (open the firewall in a terminal).
pub fn act(req: &Value) -> Result<Value, String> {
    let op = req["op"].as_str().unwrap_or("status");
    if op == "status" {
        return Ok(status());
    }
    let exe = exe()?;
    let mut cfg = Config::load();
    match op {
        "send" => {
            let feed = Feed::parse(text(req, "feed")).unwrap_or_default();
            let proto = Proto::parse(text(req, "proto")).unwrap_or_default();
            let s = cfg.add_send(feed, proto, text(req, "to"), text(req, "name"))?;
            cfg.save()?;
            link::start(&exe, &Link::Send { send: s.clone() })?;
            reply(format!("Sending {} to {}.", s.feed.label(), s.to_label()))
        }
        "start" | "stop" => {
            let id = text(req, "id");
            let on = op == "start";
            if let Some(s) = cfg.sends.iter_mut().find(|s| s.id == id) {
                s.on = on;
                let s = s.clone();
                cfg.save()?;
                if on {
                    link::start(&exe, &Link::Send { send: s.clone() })?;
                } else {
                    link::stop(id);
                }
                return reply(format!("{} {}.", s.name, if on { "sending" } else { "stopped" }));
            }
            if let Some(r) = cfg.receives.iter_mut().find(|r| r.id == id) {
                r.on = on;
                let r = r.clone();
                cfg.save()?;
                if on {
                    start_receive(&exe, &r, cfg.latency())?;
                } else {
                    link::stop(id);
                }
                return reply(format!("{} {}.", r.name, if on { "listening" } else { "stopped" }));
            }
            Err(format!("No link {id}."))
        }
        "remove" => {
            let id = text(req, "id").to_string();
            let before = cfg.sends.len() + cfg.receives.len();
            cfg.sends.retain(|s| s.id != id);
            cfg.receives.retain(|r| r.id != id);
            if cfg.sends.len() + cfg.receives.len() == before {
                return Err(format!("No link {id}."));
            }
            link::stop(&id);
            cfg.save()?;
            reply("Removed.")
        }
        "listen" => {
            // A stream announced now, by its id ("rx-…").
            let id = text(req, "id");
            let a = crate::discover::streams()
                .into_iter()
                .find(|a| format!("rx-{}", a.id) == id || a.id == id)
                .ok_or("That stream is no longer announced.")?;
            if !a.supported() {
                return Err(format!("Patch can't play {} streams.", a.format_label()));
            }
            let r = Receive::from_announced(&a);
            cfg.receives.retain(|x| x.id != r.id);
            cfg.receives.push(r.clone());
            cfg.save()?;
            start_receive(&exe, &r, cfg.latency())?;
            reply(format!("Listening to {} — it plays in the Mixer like an app.", r.name))
        }
        "vban" => {
            let on = req["on"].as_bool().unwrap_or(!cfg.vban);
            cfg.vban = on;
            cfg.save()?;
            if on {
                link::start(&exe, &Link::Vban { latency_ms: cfg.latency() })?;
                reply("Taking in VBAN streams sent to this machine (port 6980).")
            } else {
                link::stop("vban");
                reply("VBAN off.")
            }
        }
        "latency" => {
            let ms = req["ms"].as_u64().unwrap_or(100) as u32;
            if !crate::config::LATENCIES.iter().any(|(l, _)| *l == ms) {
                return Err(format!("Latency is one of 20, 50, 100 or 250 ms, not {ms}."));
            }
            cfg.latency_ms = ms;
            cfg.save()?;
            // Receives that run take it now.
            for r in cfg.receives.iter().filter(|r| link::running(&r.id)) {
                start_receive(&exe, r, ms)?;
            }
            if cfg.vban && link::running("vban") {
                link::start(&exe, &Link::Vban { latency_ms: ms })?;
            }
            reply(format!("Receives play {ms} ms behind."))
        }
        "allow" => {
            let host = crate::net::primary().ok_or("This machine is not on a network.")?;
            let Firewall::Blocked { command } = crate::firewall::check(&host.subnet()) else {
                return reply("The firewall already lets network audio in.");
            };
            open_terminal(&command)?;
            reply("Type your password in the terminal to let network audio in from this network.")
        }
        _ => Err(format!("Unknown network request: {op}")),
    }
}

/// Run a command in a terminal the user sees (it asks for a password).
fn open_terminal(command: &str) -> Result<(), String> {
    let omarchy = "omarchy-launch-floating-terminal-with-presentation";
    let found = std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| d.join(omarchy).is_file())
    }) || Path::new("/usr/share/omarchy/bin").join(omarchy).is_file();
    let mut cmd = if found {
        let mut c = std::process::Command::new(omarchy);
        c.arg(command);
        c
    } else {
        let mut c = std::process::Command::new("xdg-terminal-exec");
        c.args(["--title=Patch", "--", "bash", "-c"])
            .arg(format!("{command}; echo; read -n1 -rsp 'Press a key to close'"));
        c
    };
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open a terminal: {e}. Run: {command}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announced(origin: &str, to: Option<&str>) -> Announced {
        Announced {
            id: "1".into(),
            name: "x".into(),
            origin: origin.parse().unwrap(),
            address: "239.69.83.80".parse().unwrap(),
            port: 5004,
            payload: 127,
            encoding: "L24".into(),
            rate: 48_000,
            channels: 2,
            ptime: None,
            ptp: false,
            tool: String::new(),
            to: to.map(|t| t.parse().unwrap()),
        }
    }

    #[test]
    fn own_streams_and_streams_for_others_are_not_offered() {
        assert!(!offered(&announced("127.0.0.1", None)));
        // TEST-NET-3: never an address of this machine.
        assert!(offered(&announced("203.0.113.7", None)));
        assert!(!offered(&announced("203.0.113.7", Some("203.0.113.8"))));
        assert!(offered(&announced("203.0.113.7", Some("127.0.0.1"))));
    }
}
