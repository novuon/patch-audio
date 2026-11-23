//! The Network window: sends, announced streams, VBAN and latency. Every
//! read and action runs on the background poller (patch_network::ops);
//! the UI thread only shows the last reply.

use super::*;

/// The window's form and its poller. The form belongs to the user (edits
/// stay); the poll reads the links and the network back once a second.
pub(super) struct NetView {
    feed: RefCell<String>,
    proto: RefCell<String>,
    to: RefCell<String>,
    name: RefCell<String>,
    cursor: Cell<usize>,
    busy: Cell<bool>,
    message: RefCell<String>,
    snapshot: RefCell<serde_json::Value>,
    polls: crate::poll::Poller,
    actions: crate::poll::Poller,
    polled: Cell<Option<Instant>>,
    acted: Cell<Instant>,
}

impl Default for NetView {
    fn default() -> Self {
        NetView {
            feed: RefCell::new("program".into()),
            proto: RefCell::new("rtp".into()),
            to: RefCell::new(String::new()),
            name: RefCell::new(String::new()),
            cursor: Cell::new(0),
            busy: Cell::new(false),
            message: RefCell::new(String::new()),
            snapshot: RefCell::new(serde_json::Value::Null),
            polls: crate::poll::Poller::default(),
            actions: crate::poll::Poller::default(),
            polled: Cell::new(None),
            acted: Cell::new(Instant::now()),
        }
    }
}

fn call(request: &serde_json::Value) -> Result<serde_json::Value, String> {
    patch_network::ops::act(request)
}

/// What the headless shots and the mock show: a send running, one stream
/// listened to, one announced, the firewall closed.
fn demo() -> serde_json::Value {
    serde_json::json!({
        "host": {"ip": "192.168.1.12", "iface": "wlan0", "subnet": "192.168.1.0/24"},
        "hostname": "omarchy",
        "firewall": {"state": "blocked", "command": patch_network::firewall::allow_command("192.168.1.0/24")},
        "latency_ms": 100,
        "vban": {"on": false, "state": "off", "note": ""},
        "sends": [
            {"id": "send-0", "name": "Patch Program · omarchy", "feed_label": "Program", "proto": "RTP",
             "to": "everyone on this network", "address": "239.69.83.80:5004", "on": true, "state": "sending", "note": ""},
            {"id": "send-1", "name": "Stage monitors", "feed_label": "AUX 1", "proto": "VBAN",
             "to": "192.168.1.40", "address": "192.168.1.40:6980", "on": false, "state": "off", "note": ""}
        ],
        "receives": [
            {"id": "rx-1", "name": "Studio Mac", "origin": "192.168.1.20", "format": "L24 · 48 kHz · 2 ch", "on": true, "state": "listening", "note": "", "announced": true},
            {"id": "rx-2", "name": "Kitchen Patch", "origin": "192.168.1.31", "format": "L24 · 48 kHz · 2 ch", "on": false, "state": "off", "note": "", "announced": false}
        ],
        "streams": [
            {"id": "rx-1", "name": "Studio Mac", "origin": "192.168.1.20", "format": "L24 · 48 kHz · 2 ch", "supported": true, "listening": true, "tool": "PipeWire 1.6.11"},
            {"id": "rx-3", "name": "AVIO USB : 2", "origin": "192.168.1.50", "format": "L24 · 48 kHz · 2 ch", "supported": true, "listening": false, "ptp": true}
        ],
        "listening_for": 30,
    })
}

impl AudioApp {
    fn net_request(&self, request: serde_json::Value, busy: &str) {
        if self.frozen {
            return;
        }
        if self.net.actions.request_with("action", request, call) {
            self.net.busy.set(true);
            self.net.acted.set(Instant::now());
            *self.net.message.borrow_mut() = busy.into();
            self.push_network();
        }
    }

    pub(super) fn poll_network(&self) {
        if self.frozen {
            return;
        }
        if let Some(reply) = self.net.actions.take("action") {
            self.net.busy.set(false);
            self.net.acted.set(Instant::now());
            match reply.result {
                Ok(value) => {
                    *self.net.message.borrow_mut() =
                        value["message"].as_str().unwrap_or_default().to_owned();
                    if value["status"].is_object() {
                        *self.net.snapshot.borrow_mut() = value["status"].clone();
                    }
                }
                Err(error) => *self.net.message.borrow_mut() = error,
            }
        }
        let due = self
            .net
            .polled
            .get()
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        if due
            && self.net.polls.request_with("status", serde_json::json!({"op": "status"}), call)
        {
            self.net.polled.set(Some(Instant::now()));
        }
        if let Some(reply) = self.net.polls.take("status")
            && reply.requested >= self.net.acted.get()
            && let Ok(value) = reply.result
        {
            *self.net.snapshot.borrow_mut() = value;
        }
        self.push_network();
    }

    /// The current snapshot's streams (for the cursor and Enter).
    fn net_streams(&self) -> Vec<serde_json::Value> {
        self.net.snapshot.borrow()["streams"].as_array().cloned().unwrap_or_default()
    }

    pub(super) fn push_network(&self) {
        self.push_network_to(None);
    }

    /// Show the snapshot in every Network window, or in `only` (a window
    /// being bound, before it is in the app's list).
    fn push_network_to(&self, only: Option<&PatchWindow>) {
        let snap = self.net.snapshot.borrow().clone();
        let host = &snap["host"];
        let online = host.is_object();
        let address = if online {
            format!(
                "{} · {}",
                host["ip"].as_str().unwrap_or_default(),
                host["iface"].as_str().unwrap_or_default()
            )
        } else if snap.is_null() {
            "…".into()
        } else {
            "no network".into()
        };
        let link = |v: &serde_json::Value, detail: String| crate::ui::NetLink {
            id: v["id"].as_str().unwrap_or_default().into(),
            name: v["name"].as_str().unwrap_or_default().into(),
            detail: detail.into(),
            state: v["state"].as_str().unwrap_or("off").into(),
            note: v["note"].as_str().unwrap_or_default().into(),
            on: v["on"].as_bool().unwrap_or(false),
        };
        let empty = Vec::new();
        let sends: Vec<crate::ui::NetLink> = snap["sends"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|s| {
                link(
                    s,
                    format!(
                        "{} → {} · {} {}",
                        s["feed_label"].as_str().unwrap_or_default(),
                        s["to"].as_str().unwrap_or_default(),
                        s["proto"].as_str().unwrap_or_default(),
                        s["address"].as_str().unwrap_or_default()
                    ),
                )
            })
            .collect();
        // Saved receives whose stream is not announced now (the announced
        // ones are in the stream list, with their Listen / Stop).
        let saved: Vec<crate::ui::NetLink> = snap["receives"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter(|r| !r["announced"].as_bool().unwrap_or(false))
            .map(|r| {
                link(
                    r,
                    format!(
                        "from {} · {}",
                        r["origin"].as_str().unwrap_or_default(),
                        r["format"].as_str().unwrap_or_default()
                    ),
                )
            })
            .collect();
        let list = self.net_streams();
        let cur = self.net.cursor.get().min(list.len().saturating_sub(1));
        let streams: Vec<crate::ui::NetStream> = list
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut detail = format!(
                    "from {} · {}",
                    s["origin"].as_str().unwrap_or_default(),
                    s["format"].as_str().unwrap_or_default()
                );
                if s["ptp"].as_bool() == Some(true) {
                    detail.push_str(" · PTP clock (played on Patch's clock)");
                }
                crate::ui::NetStream {
                    id: s["id"].as_str().unwrap_or_default().into(),
                    name: s["name"].as_str().unwrap_or_default().into(),
                    detail: detail.into(),
                    listening: s["listening"].as_bool().unwrap_or(false),
                    supported: s["supported"].as_bool().unwrap_or(false),
                    selected: i == cur,
                }
            })
            .collect();
        let fw = &snap["firewall"];
        let message: SharedString = self.net.message.borrow().as_str().into();
        let windows: Vec<PatchWindow> = match only {
            Some(w) => vec![w.clone_strong()],
            None => {
                self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
                self.windows.borrow().iter().filter_map(|w| w.upgrade()).collect()
            }
        };
        for w in windows {
            if w.get_surface() != "network" {
                continue;
            }
            let n = w.global::<crate::ui::NetworkState>();
            n.set_online(online);
            n.set_address(address.as_str().into());
            n.set_hostname(snap["hostname"].as_str().unwrap_or_default().into());
            n.set_firewall(fw["state"].as_str().unwrap_or("off").into());
            n.set_firewall_command(fw["command"].as_str().unwrap_or_default().into());
            n.set_subnet(host["subnet"].as_str().unwrap_or_default().into());
            n.set_listening_for(snap["listening_for"].as_i64().unwrap_or(0) as i32);
            n.set_discovery_error(snap["discovery_error"].as_str().unwrap_or_default().into());
            n.set_vban_on(snap["vban"]["on"].as_bool().unwrap_or(false));
            n.set_vban_state(snap["vban"]["state"].as_str().unwrap_or("off").into());
            n.set_latency(snap["latency_ms"].as_i64().unwrap_or(100) as i32);
            n.set_message(message.clone());
            n.set_busy(self.net.busy.get());
            n.set_cursor(cur as i32);
            n.set_feed(self.net.feed.borrow().as_str().into());
            n.set_proto(self.net.proto.borrow().as_str().into());
            if !same_rows(&n.get_sends(), &sends) {
                n.set_sends(ModelRc::new(VecModel::from(sends.clone())));
            }
            if !same_rows(&n.get_saved(), &saved) {
                n.set_saved(ModelRc::new(VecModel::from(saved.clone())));
            }
            if !same_rows(&n.get_streams(), &streams) {
                n.set_streams(ModelRc::new(VecModel::from(streams.clone())));
            }
        }
    }

    /// Listen to an announced stream, or stop it; a stream listened to
    /// before (saved) starts again as it was.
    fn net_listen(&self, id: &str) {
        let snap = self.net.snapshot.borrow().clone();
        let saved = snap["receives"]
            .as_array()
            .and_then(|r| r.iter().find(|r| r["id"].as_str() == Some(id)).cloned());
        let listening = snap["streams"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["id"].as_str() == Some(id)))
            .is_some_and(|s| s["listening"].as_bool() == Some(true));
        let op = match (listening, saved.is_some()) {
            (true, _) => "stop",
            (false, true) => "start",
            (false, false) => "listen",
        };
        self.net_request(serde_json::json!({"op": op, "id": id}), if listening { "Stopping…" } else { "Starting…" });
    }

    pub(super) fn bind_network(&self, w: &PatchWindow) {
        if w.get_surface() != "network" {
            return;
        }
        if self.frozen && self.net.snapshot.borrow().is_null() {
            *self.net.snapshot.borrow_mut() = demo();
        }
        let n = w.global::<crate::ui::NetworkState>();
        n.set_to(self.net.to.borrow().as_str().into());
        n.set_name(self.net.name.borrow().as_str().into());
        let me = self.me.clone();
        n.on_edit(move |field, value| {
            if let Some(a) = me.upgrade() {
                match field.as_str() {
                    "to" => *a.net.to.borrow_mut() = value.to_string(),
                    "name" => *a.net.name.borrow_mut() = value.to_string(),
                    _ => {}
                }
            }
        });
        let me = self.me.clone();
        n.on_set_feed(move |f| {
            if let Some(a) = me.upgrade() {
                *a.net.feed.borrow_mut() = f.to_string();
                a.push_network();
            }
        });
        let me = self.me.clone();
        n.on_set_proto(move |p| {
            if let Some(a) = me.upgrade() {
                *a.net.proto.borrow_mut() = p.to_string();
                a.push_network();
            }
        });
        let me = self.me.clone();
        n.on_add_send(move || {
            if let Some(a) = me.upgrade() {
                let request = serde_json::json!({
                    "op": "send",
                    "feed": a.net.feed.borrow().clone(),
                    "proto": a.net.proto.borrow().clone(),
                    "to": a.net.to.borrow().clone(),
                    "name": a.net.name.borrow().clone(),
                });
                a.net_request(request, "Starting the send…");
            }
        });
        let me = self.me.clone();
        n.on_toggle(move |id| {
            if let Some(a) = me.upgrade() {
                // Stop what runs, start what doesn't (a link switched on
                // whose service is gone starts again).
                let snap = a.net.snapshot.borrow().clone();
                let on = ["sends", "receives"].iter().any(|k| {
                    snap[*k].as_array().is_some_and(|l| {
                        l.iter().any(|x| {
                            x["id"].as_str() == Some(id.as_str())
                                && x["state"].as_str().is_some_and(|s| s != "off")
                        })
                    })
                });
                let op = if on { "stop" } else { "start" };
                a.net_request(serde_json::json!({"op": op, "id": id.as_str()}), if on { "Stopping…" } else { "Starting…" });
            }
        });
        let me = self.me.clone();
        n.on_remove(move |id| {
            if let Some(a) = me.upgrade() {
                a.net_request(serde_json::json!({"op": "remove", "id": id.as_str()}), "Removing…");
            }
        });
        let me = self.me.clone();
        n.on_listen(move |id| {
            if let Some(a) = me.upgrade() {
                if let Some(i) = a.net_streams().iter().position(|s| s["id"].as_str() == Some(id.as_str())) {
                    a.net.cursor.set(i);
                }
                a.net_listen(id.as_str());
            }
        });
        let me = self.me.clone();
        n.on_move(move |d| {
            if let Some(a) = me.upgrade() {
                let len = a.net_streams().len() as i32;
                let c = (a.net.cursor.get() as i32 + d).clamp(0, (len - 1).max(0));
                a.net.cursor.set(c as usize);
                a.push_network();
            }
        });
        let me = self.me.clone();
        n.on_set_vban(move |on| {
            if let Some(a) = me.upgrade() {
                a.net_request(serde_json::json!({"op": "vban", "on": on}), if on { "Starting VBAN…" } else { "Stopping VBAN…" });
            }
        });
        let me = self.me.clone();
        n.on_set_latency(move |ms| {
            if let Some(a) = me.upgrade() {
                a.net_request(serde_json::json!({"op": "latency", "ms": ms}), "Setting the latency…");
            }
        });
        let me = self.me.clone();
        n.on_allow(move || {
            if let Some(a) = me.upgrade() {
                a.net_request(serde_json::json!({"op": "allow"}), "Opening a terminal…");
            }
        });
        self.push_network_to(Some(w));
    }
}
