//! A running link: one send, one receive or the VBAN listener.
//!
//! Each runs as its own user service, `omarchy-patch-net-<id>.service`
//! (systemd-run; systemd restarts it if it fails), so a link keeps going
//! when Patch's windows close, the UI restarts or Patch updates. The
//! service is `omarchy-patch network link`: a small supervisor that runs
//! PipeWire's modules (`pipewire -c <conf>`) and keeps them working:
//!
//! * PipeWire stopped the link: start it again (1, 2, 4 … 15 s apart).
//! * Patch's node went away and came back (an engine restart or upgrade):
//!   WirePlumber links a stream only when it starts, so the supervisor
//!   starts it again once Patch's node is back and the link is unlinked.
//! * The machine's address changed (Wi-Fi roamed, a cable went in): the
//!   announcement and the multicast interface follow.
//!
//! It writes what it is doing to `$XDG_RUNTIME_DIR/omarchy-patch/network/
//! <id>.json`, which the Network window and `network status` read.

use crate::config::{Receive, Send};
use crate::net::Host;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const ENV: &str = "PATCH_NETWORK_LINK";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Link {
    Send { send: Send },
    Receive { receive: Receive, latency_ms: u32 },
    Vban { latency_ms: u32 },
}

impl Link {
    pub fn id(&self) -> &str {
        match self {
            Link::Send { send } => &send.id,
            Link::Receive { receive, .. } => &receive.id,
            Link::Vban { .. } => "vban",
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Link::Send { send } => &send.name,
            Link::Receive { receive, .. } => &receive.name,
            Link::Vban { .. } => "VBAN",
        }
    }

    pub fn conf(&self, host: Option<&Host>) -> Result<String, String> {
        match self {
            Link::Send { send } => Ok(crate::conf::send(send, host)),
            Link::Receive { receive, latency_ms } => crate::conf::receive(receive, *latency_ms, host),
            Link::Vban { latency_ms } => Ok(crate::conf::vban_listen(*latency_ms)),
        }
    }

    /// Patch's node the link is linked to.
    fn target(&self) -> &'static str {
        match self {
            Link::Send { send } => send.feed.node(),
            _ => "patch",
        }
    }

    /// Does the configuration depend on the machine's address?
    fn follows_host(&self) -> bool {
        match self {
            Link::Send { send } => send.proto == crate::config::Proto::Rtp,
            Link::Receive { receive, .. } => receive.multicast(),
            Link::Vban { .. } => false,
        }
    }

    /// What the window says while it works.
    fn working(&self) -> &'static str {
        match self {
            Link::Send { .. } => "sending",
            _ => "listening",
        }
    }
}

pub fn unit_name(id: &str) -> String {
    format!("omarchy-patch-net-{id}.service")
}

fn dir() -> PathBuf {
    patch_core::ipc::runtime_dir().join("network")
}

fn status_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.json"))
}

fn conf_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.conf"))
}

/// Is `pid` a link's supervisor (`… network link`)? A status file can
/// outlive its process, and a pid is reused: never take another process
/// for a link (`stop` signals it).
fn alive(pid: u32) -> bool {
    pid > 0
        && std::fs::read(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|c| is_supervisor(&c))
}

fn is_supervisor(cmdline: &[u8]) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    args.windows(2).any(|w| w[0] == b"network" && w[1] == b"link")
}

/// What a link is doing, if it runs: `state` is starting, sending,
/// listening, waiting or restarting.
pub fn status(id: &str) -> Option<Value> {
    let text = std::fs::read_to_string(status_path(id)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    alive(v["pid"].as_u64()? as u32).then_some(v)
}

pub fn running(id: &str) -> bool {
    status(id).is_some()
}

/// Every link running now, by id, with its status (no service is asked:
/// only the status files, so the bar can call it each time it opens).
pub fn all_running() -> Vec<(String, Value)> {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Value)> = entries
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let id = name.strip_suffix(".json")?.to_string();
            let v = status(&id)?;
            Some((id, v))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn quiet(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
}

fn systemctl(args: &[&str]) -> bool {
    quiet(Command::new("systemctl").arg("--user").args(args))
        .status()
        .is_ok_and(|s| s.success())
}

/// Start (or restart) a link as its own user service.
pub fn start(exe: &Path, link: &Link) -> Result<(), String> {
    let id = link.id().to_string();
    stop(&id);
    let json = serde_json::to_string(link).map_err(|e| e.to_string())?;
    let unit = unit_name(&id);
    let out = Command::new("systemd-run")
        .args(["--user", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg(format!("--description=Patch Network · {}", link.name()))
        .args(["-p", "Restart=on-failure", "-p", "RestartSec=3"])
        .arg(format!("--setenv={ENV}={json}"))
        .arg(exe)
        .args(["network", "link"])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "systemd-run: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        // No systemd: a detached process of its own.
        Err(_) => {
            use std::os::unix::process::CommandExt;
            Command::new(exe)
                .args(["network", "link"])
                .env(ENV, json)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }
}

/// Stop a link (and forget why it last stopped).
pub fn stop(id: &str) {
    let unit = unit_name(id);
    systemctl(&["stop", &unit]);
    systemctl(&["reset-failed", &unit]);
    // A link started without systemd.
    if let Some(v) = status(id)
        && let Some(pid) = v["pid"].as_u64()
    {
        // SAFETY: a signal to a process id read from our own status file,
        // checked alive just before.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(2);
        while alive(pid as u32) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = std::fs::remove_file(status_path(id));
}

// ---- the supervisor ----------------------------------------------------

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Ports as `pw-link -io` lists them, links as `pw-link -l` does.
#[derive(Debug, Default)]
pub struct Graph {
    ports: String,
    links: String,
}

impl Graph {
    fn read() -> Graph {
        let run = |a: &str| {
            Command::new("pw-link")
                .arg(a)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        Graph { ports: run("-io"), links: run("-l") }
    }

    #[cfg(test)]
    pub fn from(ports: &str, links: &str) -> Graph {
        Graph { ports: ports.into(), links: links.into() }
    }

    /// Does `node` have any port?
    pub fn has(&self, node: &str) -> bool {
        self.ports.lines().any(|l| port_node(l.trim()) == Some(node))
    }

    /// Is any of `node`'s ports linked to a node `peer` accepts?
    pub fn linked(&self, node: &str, peer: impl Fn(&str) -> bool) -> bool {
        let mut header: Option<&str> = None;
        for line in self.links.lines() {
            let t = line.trim_start();
            if let Some(other) = t.strip_prefix("|->").or_else(|| t.strip_prefix("|<-")) {
                let other = port_node(other.trim());
                let hit = match (header, other) {
                    (Some(h), Some(o)) if h == node => peer(o),
                    (Some(h), Some(o)) if o == node => peer(h),
                    _ => false,
                };
                if hit {
                    return true;
                }
            } else {
                header = port_node(t);
            }
        }
        false
    }
}

/// "patch.program:capture_FL" → "patch.program".
fn port_node(port: &str) -> Option<&str> {
    port.rsplit_once(':').map(|(n, _)| n)
}

/// What the supervisor sees on one check.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// Linked to Patch.
    Working,
    /// Patch's node is not there (the engine is stopped or restarting).
    Waiting,
    /// The link has ports, Patch's node is there, and they are not linked.
    Unlinked,
    /// The link has no ports yet (a VBAN listener before any stream).
    Idle,
}

pub fn look(link: &Link, g: &Graph) -> Seen {
    let node = crate::conf::node_name(link.id());
    let target = link.target();
    if !g.has(target) {
        Seen::Waiting
    } else if !g.has(&node) {
        Seen::Idle
    } else if g.linked(&node, |peer| match link {
        // A send only from its own feed.
        Link::Send { .. } => peer == target,
        // A receive into Patch: `patch` or the stream channel Patch's
        // routing gave it, not a sound card it fell back to while Patch
        // was away.
        _ => peer == "patch" || peer.starts_with("patch."),
    }) {
        Seen::Working
    } else {
        Seen::Unlinked
    }
}

struct Report {
    id: String,
    pid: u32,
    restarts: u32,
    since: u64,
}

impl Report {
    fn write(&self, state: &str, note: &str, host: Option<&Host>) {
        let v = json!({
            "pid": self.pid,
            "state": state,
            "note": note,
            "restarts": self.restarts,
            "since": self.since,
            "ip": host.map(|h| h.ip.to_string()),
        });
        let p = status_path(&self.id);
        let tmp = p.with_extension("json.new");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn spawn(conf: &Path) -> std::io::Result<Child> {
    Command::new("pipewire")
        .arg("-c")
        .arg(conf)
        .stdin(Stdio::null())
        .spawn()
}

fn end(child: &mut Child) {
    // SAFETY: SIGTERM to our own child, which we then reap.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Sleep in short steps, stopping early on SIGTERM. True if stopped.
fn nap(d: Duration) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if STOP.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    STOP.load(Ordering::SeqCst)
}

/// Run a link until SIGTERM (`omarchy-patch network link`).
pub fn supervise(link: Link) -> Result<(), String> {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let mut report = Report { id: link.id().into(), pid: std::process::id(), restarts: 0, since: now_s() };
    let mut backoff = Duration::from_secs(1);
    let mut note = String::new();
    while !STOP.load(Ordering::SeqCst) {
        let host = crate::net::primary();
        let conf = link.conf(host.as_ref())?;
        let path = conf_path(link.id());
        std::fs::write(&path, conf).map_err(|e| e.to_string())?;
        report.write("starting", &note, host.as_ref());
        let mut child = match spawn(&path) {
            Ok(c) => c,
            Err(e) => {
                report.write("restarting", &format!("pipewire did not start: {e}"), host.as_ref());
                if nap(backoff) {
                    break;
                }
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
        };
        let started = Instant::now();
        let mut unlinked = 0;
        // Why this run ended, for the next one's note.
        let why: String = loop {
            if nap(Duration::from_secs(2)) {
                end(&mut child);
                break String::new();
            }
            if let Ok(Some(code)) = child.try_wait() {
                break format!("PipeWire ended the link ({code}); started again");
            }
            if link.follows_host() && crate::net::primary() != host {
                end(&mut child);
                break "the network changed; started again on the new address".into();
            }
            match look(&link, &Graph::read()) {
                Seen::Working | Seen::Idle => {
                    unlinked = 0;
                    // Thirty steady seconds: the last trouble is over.
                    if started.elapsed() > Duration::from_secs(30) {
                        backoff = Duration::from_secs(1);
                        note.clear();
                    }
                    report.write(link.working(), &note, host.as_ref());
                }
                Seen::Waiting => {
                    unlinked = 0;
                    report.write("waiting", "waiting for Patch's engine", host.as_ref());
                }
                Seen::Unlinked => {
                    unlinked += 1;
                    if unlinked >= 2 {
                        end(&mut child);
                        break "was not linked to Patch; started again to link it".into();
                    }
                }
            }
        };
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        report.restarts += 1;
        note = why;
        report.write("restarting", &note, host.as_ref());
        if nap(backoff) {
            break;
        }
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
    let _ = std::fs::remove_file(status_path(link.id()));
    let _ = std::fs::remove_file(conf_path(link.id()));
    Ok(())
}

/// The link the service was started with.
pub fn from_env() -> Result<Link, String> {
    let json = std::env::var(ENV).map_err(|_| format!("{ENV} is not set"))?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Feed, Proto};

    // `pw-link -io` and `pw-link -l` with a Patch send linked to the
    // program (as they printed on 2026-11-22).
    const PORTS: &str = "patch.program:capture_FL\npatch.program:capture_FR\npatch.net.send-0:send_FL\npatch.net.send-0:send_FR\npatch:playback_FL\n";
    const LINKS: &str = "patch.program:capture_FL\n  |-> patch.net.send-0:send_FL\npatch.program:capture_FR\n  |-> patch.net.send-0:send_FR\npatch.net.send-0:send_FL\n  |<- patch.program:capture_FL\n";

    fn send() -> Link {
        let mut c = Config::default();
        Link::Send { send: c.add_send(Feed::Program, Proto::Rtp, "", "x").unwrap() }
    }

    #[test]
    fn a_linked_send_works() {
        assert_eq!(look(&send(), &Graph::from(PORTS, LINKS)), Seen::Working);
    }

    #[test]
    fn without_patchs_node_it_waits_and_never_takes_another() {
        let ports = "patch.net.send-0:send_FL\nalsa_input.usb-mic:capture_MONO\n";
        assert_eq!(look(&send(), &Graph::from(ports, "")), Seen::Waiting);
    }

    #[test]
    fn after_an_engine_restart_it_is_unlinked() {
        assert_eq!(look(&send(), &Graph::from(PORTS, "")), Seen::Unlinked);
        // Another node's links do not count.
        let other = "patch.program:capture_FL\n  |-> patch.net.send-1:send_FL\n";
        assert_eq!(look(&send(), &Graph::from(PORTS, other)), Seen::Unlinked);
    }

    #[test]
    fn a_receive_left_on_the_sound_card_is_unlinked() {
        let r = Link::Receive {
            receive: crate::config::Receive {
                id: "rx-1".into(),
                name: "Studio".into(),
                origin: "192.168.68.20".into(),
                address: "239.69.83.80".into(),
                port: 5004,
                encoding: "L24".into(),
                rate: 48_000,
                channels: 2,
                on: true,
            },
            latency_ms: 100,
        };
        let ports = "patch:playback_FL\nnetwork.rx-1:output_FL\n";
        let card = "network.rx-1:output_FL\n  |-> alsa_output.usb-Focusrite:playback_FL\n";
        assert_eq!(look(&r, &Graph::from(ports, card)), Seen::Unlinked);
        let patch = "network.rx-1:output_FL\n  |-> patch.stream-2:playback_FL\n";
        assert_eq!(look(&r, &Graph::from(ports, patch)), Seen::Working);
    }

    #[test]
    fn a_vban_listener_without_streams_is_idle() {
        let v = Link::Vban { latency_ms: 100 };
        assert_eq!(look(&v, &Graph::from("patch:playback_FL\n", "")), Seen::Idle);
        let linked = "network.vban:output_FL\n  |-> patch.stream-2:playback_FL\n";
        assert_eq!(
            look(&v, &Graph::from("patch:playback_FL\nnetwork.vban:output_FL\n", linked)),
            Seen::Working
        );
    }

    #[test]
    fn only_a_link_supervisor_counts_as_a_running_link() {
        assert!(is_supervisor(b"/home/u/.local/bin/omarchy-patch\0network\0link\0"));
        assert!(!is_supervisor(b"/usr/bin/firefox\0--network\0"));
        assert!(!is_supervisor(b"omarchy-patch\0network\0status\0"));
        // This test process is not one.
        assert!(!alive(std::process::id()));
    }

    #[test]
    fn the_link_travels_to_its_service_as_json() {
        let l = send();
        let back: Link = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        assert_eq!(unit_name(l.id()), "omarchy-patch-net-send-0.service");
    }
}
