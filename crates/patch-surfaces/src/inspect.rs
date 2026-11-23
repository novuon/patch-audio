//! Context-sensitive inspector content. What is shown depends on what the
//! selected node *is*, not just on which fields happen to exist.

use crate::app::{db_text, gain_to_pos, output_pos, output_text};
use crate::ui::{InspRow, InspSection};
use patch_model::audio::*;
use slint::{ModelRc, VecModel};

pub struct View {
    pub name: String,
    pub kind: String,
    pub detail: String,
    pub hardware: bool,
    pub sections: Vec<InspSection>,
}

fn row(kind: &str, label: &str, value: impl Into<String>) -> InspRow {
    InspRow {
        kind: kind.into(),
        label: label.into(),
        value: value.into().into(),
        amount: 0.0,
        active: false,
    }
}

fn kv(label: &str, value: impl Into<String>) -> InspRow {
    row("kv", label, value)
}

fn section(title: &str, rows: Vec<InspRow>) -> InspSection {
    InspSection {
        title: title.into(),
        rows: ModelRc::new(VecModel::from(rows)),
    }
}

fn route_rows(p: &dyn AudioProvider, id: &str, outgoing: bool) -> Vec<InspRow> {
    let nodes = p.nodes();
    let name = |n: &'static str| {
        nodes
            .iter()
            .find(|x| x.id == n)
            .map(|x| x.name)
            .unwrap_or(n)
    };
    let rows: Vec<InspRow> = p
        .routes()
        .iter()
        .filter(|r| if outgoing { r.from == id } else { r.to == id })
        .map(|r| {
            let mut x = row(
                "route",
                if outgoing { "→" } else { "←" },
                name(if outgoing { r.to } else { r.from }),
            );
            x.amount = r.channels as f32;
            x
        })
        .collect();
    if rows.is_empty() {
        vec![row(
            "note",
            "",
            if outgoing {
                "No outgoing routes."
            } else {
                "No inputs."
            },
        )]
    } else {
        rows
    }
}

/// Processors downstream of `id`, in order.
fn chain(p: &dyn AudioProvider, id: &str) -> Vec<InspRow> {
    let nodes = p.nodes();
    let routes = p.routes();
    let mut out = vec![];
    let mut cur = id.to_string();
    while let Some(next) = routes.iter().find(|r| {
        r.from == cur
            && nodes
                .iter()
                .any(|n| n.id == r.to && n.kind == NodeKind::Processor)
    }) {
        let n = nodes.iter().find(|n| n.id == next.to).unwrap();
        let summary = match &n.role {
            NodeRole::Processor { params } => {
                params.first().map(|(_, v)| v.clone()).unwrap_or_default()
            }
            _ => String::new(),
        };
        let mut r = row("chain", n.name, summary);
        r.active = true;
        out.push(r);
        cur = n.id.to_string();
    }
    if out.is_empty() {
        out.push(row(
            "note",
            "",
            "No processing. Route through a processor in the Router.",
        ));
    }
    out
}

fn level_rows(p: &dyn AudioProvider, id: &str, label: &str) -> Vec<InspRow> {
    let mut rows = vec![];
    let mut meter = row("live-meter", label, "live");
    meter.amount = 0.0;
    rows.push(meter);
    if let Some(s) = p.strips().into_iter().find(|s| s.node == id) {
        // Sink-volume strips use the bar's OUTPUT slider scale, in percentage
        // (see `output_pos`).
        let volume = s.node == "output" || s.node == "system-sounds";
        let (text, amount) = if volume {
            (output_text(output_pos(s.gain_db)), output_pos(s.gain_db))
        } else {
            (db_text(s.gain_db), gain_to_pos(s.gain_db))
        };
        let mut g = row("gain", if volume { "Volume" } else { "Gain" }, text);
        g.amount = amount;
        rows.push(g);
        let mut m = row("toggle", "Muted", "");
        m.active = s.mute;
        rows.push(m);
    }
    rows
}

/// A microphone's strip: its input (one of the capture sources), level and
/// gain, and its voice processing (each stage on/off with an amount 0…1).
pub fn mic(p: &dyn AudioProvider, id: &str, info: &serde_json::Value) -> View {
    let ch = id.trim_start_matches("engine.");
    let n = ch.trim_start_matches("mic-");
    let mic = info["mics"]
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["channel"] == ch));
    let source = mic.and_then(|m| m["source"].as_str()).unwrap_or("");
    let desc = mic
        .and_then(|m| m["description"].as_str())
        .unwrap_or(source);
    let present = mic.is_none_or(|m| m["present"].as_bool() != Some(false));
    let remote = patch_core::mic::remote_source(source);
    let mut input = vec![];
    if !present {
        input.push(row(
            "note",
            "",
            if remote {
                "The call or stream this lane took has ended (a browser closes a paused stream): choose the playing stream below again.".to_string()
            } else {
                format!("{desc} isn't connected: choose another input, or plug it back in.")
            },
        ));
    }
    let choose = |s: &serde_json::Value| {
        let name = s["name"].as_str().unwrap_or("");
        let mut r = row("source", s["description"].as_str().unwrap_or(name), name);
        r.active = name == source;
        r
    };
    input.extend(info["sources"].as_array().into_iter().flatten().map(choose));
    if input.is_empty() {
        input.push(row("note", "", "Reading the inputs…"));
    }
    let remotes: Vec<_> = info["remotes"].as_array().into_iter().flatten().map(choose).collect();
    if !remotes.is_empty() {
        input.push(row(
            "note",
            "",
            "Remote voices: apps playing into Patch (a call, an X Space). The voice leaves its own strip and runs through this lane's voice chain.",
        ));
        input.extend(remotes);
    }
    input.push(row("remove-mic", if remote { "Remove this remote voice" } else { "Remove this microphone" }, ""));
    let mut monitoring = row("monitor", "Monitor in main mix", "");
    monitoring.active = mic.is_none_or(|m| m["monitor"].as_bool() != Some(false));
    input.push(monitoring);
    let sending = mic.is_none_or(|m| m["input_send"].as_bool() != Some(false));
    if remote {
        // The lane's role: a call's far end (mix-minus) or a voice source.
        let mut role = row("monitor", "Call far end", "MixMinus");
        role.active = !sending;
        input.push(role);
        input.push(row(
            "note",
            "",
            if sending {
                "Voice source: this app's voice goes through the voice chain like a microphone. Send it to AUX 1 with the Mixer's Aux1 knob, then choose Patch AUX 1 as the microphone in the other browser. Switch Call far end on if these are the voices of a call you are in yourself."
            } else {
                "Call far end: you hear and record these voices, but they never go back to the call: not into Patch Input, and not to AUX 1/2 or the effects, whatever the send knobs say (mix-minus). Switch it off to pass this voice on as a Voice source."
            },
        ));
    } else {
        let mut send = row("monitor", "Into Patch Input and sends", "Send");
        send.active = sending;
        input.push(send);
    }
    // Voice: amounts as the sliders show them (the engine's settings).
    let v = mic.map(|m| &m["voice"]);
    let f = |k: &str| v.and_then(|v| v[k].as_f64()).unwrap_or(0.0) as f32;
    let stage = |label: &str, on: bool, amount: f32, value: String| {
        let mut r = row("voice", label, value);
        r.active = on;
        r.amount = amount.clamp(0.0, 1.0);
        r
    };
    let hpf = f("hpf_hz");
    let gate = v.and_then(|v| v["gate_db"].as_f64()).map(|g| g as f32);
    let (deess, comp) = (f("deess"), f("comp"));
    let voice = vec![
        stage("Input trim", true, (f("input_gain_db") + 24.0) / 48.0, format!("{} dB", crate::num::fixed(f("input_gain_db"), 1))),
        stage("Output trim", true, (f("output_gain_db") + 24.0) / 48.0, format!("{} dB", crate::num::fixed(f("output_gain_db"), 1))),
        stage(
            "High-pass",
            hpf > 0.0,
            if hpf > 0.0 {
                (hpf - 40.0) / 160.0
            } else {
                0.25
            },
            if hpf > 0.0 {
                format!("{hpf:.0} Hz")
            } else {
                "off".into()
            },
        ),
        stage(
            "Noise gate",
            gate.is_some(),
            gate.map_or(0.5, |g| (g + 70.0) / 40.0),
            gate.map_or("off".into(), |g| format!("{g:.0} dB")),
        ),
        stage(
            "Band suppression", v.and_then(|v| v["noise_db"].as_f64()).is_some(),
            v.and_then(|v| v["noise_db"].as_f64()).map_or(0.5, |g| (g as f32 + 70.0) / 40.0),
            v.and_then(|v| v["noise_db"].as_f64()).map_or("off".into(), |g| format!("{g:.0} dBFS")),
        ),
        stage(
            "Expander", v.and_then(|v| v["expander_db"].as_f64()).is_some(),
            v.and_then(|v| v["expander_db"].as_f64()).map_or(0.5, |g| (g as f32 + 70.0) / 40.0),
            v.and_then(|v| v["expander_db"].as_f64()).map_or("off".into(), |g| format!("{g:.0} dBFS · 2:1")),
        ),
        stage(
            "De-esser",
            deess > 0.0,
            if deess > 0.0 { deess } else { 0.4 },
            if deess > 0.0 {
                format!("{:.0} %", deess * 100.0)
            } else {
                "off".into()
            },
        ),
        stage(
            "Compressor",
            comp > 0.0,
            if comp > 0.0 { comp } else { 0.5 },
            if comp > 0.0 {
                format!("{:.0} %", comp * 100.0)
            } else {
                "off".into()
            },
        ),
        row(
            "note",
            "",
            if remote {
                "Input trim → high-pass → band suppression → de-esser → gate → expander → compressor → output trim → channel EQ → fader. A remote voice arrives processed by the call (codec, auto gain): Remote voice levels it without a gate."
            } else {
                "Input trim → high-pass → band suppression → de-esser → gate → expander → compressor → output trim → channel EQ → fader. Patch Input has a final peak guard. Muted when added: put on headphones, then unmute (speakers would feed back)."
            },
        ),
    ];
    let mut processing = vec![row("kv", "Voice chain", if v.is_some_and(|v| v["on"].as_bool() == Some(true)) { "enabled" } else { "bypassed" })];
    if let Some(meter) = mic.filter(|_| present).map(|m| &m["meter"]).filter(|v| !v.is_null()) {
        let db = |key: &str| 20.0 * meter[key].as_f64().unwrap_or(0.0).max(1e-6).log10();
        for (label, value) in [
            ("Raw input peak", format!("{:.1} dBFS", db("input_peak"))),
            ("Raw input RMS", format!("{:.1} dBFS", db("input_rms"))),
            ("Gate attenuation", format!("{} dB", crate::num::fixed(-db("gate_gain"), 1))),
            ("Compressor reduction", format!("{} dB", crate::num::fixed(-db("compressor_gain"), 1))),
            ("De-esser reduction", format!("{} dB", crate::num::fixed(-db("deesser_gain"), 1))),
            ("Algorithmic delay", "0 samples · excludes device/backend latency".into()),
        ] { processing.push(row("kv", label, value)); }
        if meter["input_peak"].as_f64().unwrap_or(0.0) >= 1.0 { processing.push(row("note", "", "Input reached digital full scale: check device gain. Software processing cannot repair capture clipping.")); }
        processing.push(row("note", "", "Latest engine-cycle snapshot; input is before voice processing. The live meter is after channel EQ and the software fader."));
    }
    View {
        name: format!("{} {n}", if !remote { "Mic" } else if sending { "Voice" } else { "Remote" }),
        kind: if !remote { "Microphone" } else if sending { "Voice source" } else { "Call far end" }.into(),
        detail: if present { desc.to_string() } else if remote { format!("{desc} · ended") } else { format!("{desc} · disconnected") },
        hardware: !remote,
        sections: vec![
            section("Level", level_rows(p, id, "Output")),
            section("Processing state", processing),
            section("Voice", voice),
            section("Input", input),
        ],
    }
}

pub fn build(p: &dyn AudioProvider, id: &str) -> View {
    let devices = p.devices();
    let dev = |d: &str| devices.iter().find(|x| x.id == d);
    let engine = p.engine();

    if id == "system-sounds" {
        return View {
            name: "System sounds".into(),
            kind: "Event channel".into(),
            detail: "desktop alerts and short system playback".into(),
            hardware: false,
            sections: vec![
                section("Level", level_rows(p, id, "System sounds")),
                section("Output", route_rows(p, id, true)),
            ],
        };
    }

    if id == "master" {
        let live = p.strips().iter().any(|s| s.node == "engine.dj-mixer");
        return View {
            name: "Patch Mix".into(),
            kind: "Mix".into(),
            detail: "everything that plays through Patch".into(),
            hardware: false,
            sections: vec![
                section("Level", level_rows(p, "master", "Mix")),
                section(
                    "Chain",
                    vec![
                        kv(
                            "In it",
                            if live {
                                "apps through Patch · DJ Booth · mics"
                            } else {
                                "apps through Patch"
                            },
                        ),
                        kv("After it", "Output · the menu bar volume"),
                    ],
                ),
            ],
        };
    }
    if id == "output" {
        return View {
            name: "Output".into(),
            kind: "System volume".into(),
            detail: "the menu bar's slider and the volume keys".into(),
            hardware: false,
            sections: vec![
                section("Level", level_rows(p, "output", "Heard")),
                section(
                    "Chain",
                    vec![
                        kv(
                            "Before it",
                            if p.strips().iter().any(|s| s.node == "master") {
                                "the Patch Mix"
                            } else {
                                "every app"
                            },
                        ),
                        kv("After it", "your speakers or headphones"),
                    ],
                ),
            ],
        };
    }

    let Some(n) = p.nodes().into_iter().find(|n| n.id == id) else {
        return View {
            name: String::new(),
            kind: String::new(),
            detail: String::new(),
            hardware: false,
            sections: vec![],
        };
    };
    let ch = format!("{} ch", n.channels);
    match &n.role {
        NodeRole::HardwareIn { device } => {
            let d = dev(device);
            let rate = d.map(|d| d.sample_rate).unwrap_or(48_000);
            let rate_note = if rate != engine.sample_rate {
                format!(
                    "{} kHz → {} kHz (resampled)",
                    rate / 1000,
                    engine.sample_rate / 1000
                )
            } else {
                format!("{} kHz · native", rate / 1000)
            };
            View {
                name: n.name.into(),
                kind: "Hardware input".into(),
                detail: format!("{ch} · {}", d.map(|d| d.name).unwrap_or("—")),
                hardware: true,
                sections: vec![
                    section("Levels", level_rows(p, id, "Input")),
                    section("Effects", chain(p, id)),
                    section(
                        "Device",
                        vec![
                            kv("Device", d.map(|d| d.name).unwrap_or("—")),
                            kv(
                                "Ports",
                                if n.channels == 1 {
                                    "Mic (capsule)".into()
                                } else {
                                    format!("Analog In 1–{}", n.channels)
                                },
                            ),
                            kv("Rate", rate_note),
                            kv("Clock", d.map(|d| d.clock).unwrap_or("—")),
                            row(
                                "note",
                                "",
                                "MOCK DEVICE — illustrative hardware, not detected on this system.",
                            ),
                        ],
                    ),
                    section("Routing", route_rows(p, id, true)),
                ],
            }
        }
        NodeRole::AppStream { process, rules } => {
            let mut sections = vec![section("Level", level_rows(p, id, "Stream"))];
            let mut routing = route_rows(p, id, true);
            if let Some(through) = p.through_patch(id) {
                let mut t = row(
                    "patch",
                    "Through Patch",
                    if through {
                        "on the Patch bus"
                    } else {
                        "direct to output"
                    },
                );
                t.active = through;
                routing.insert(0, t);
                routing.push(row(
                    "note",
                    "",
                    "The Patch bus lives in the audio server, so playback continues if Patch closes or crashes.",
                ));
            }
            sections.push(section("Routing", routing));
            if p.provenance().is_simulated() {
                sections.push(section(
                    "Output",
                    vec![
                        kv(
                            "Target",
                            route_rows(p, id, true)
                                .first()
                                .map(|r| r.value.to_string())
                                .unwrap_or_default(),
                        ),
                        kv("Format", format!("{ch} · f32 · 48 kHz")),
                        kv("Latency", "10.7 ms"),
                    ],
                ));
            }
            if !rules.is_empty() {
                sections.push(section(
                    "Rules",
                    rules
                        .iter()
                        .enumerate()
                        .map(|(i, r)| row("rule", &format!("rule {}", i + 1), r.as_str()))
                        .chain(std::iter::once(row(
                            "note",
                            "",
                            "Rules re-apply when the app reconnects. Simulated.",
                        )))
                        .collect(),
                ));
            }
            View {
                name: n.name.into(),
                kind: "Application stream".into(),
                detail: format!("{ch} · process {process}"),
                hardware: false,
                sections,
            }
        }
        NodeRole::System => View {
            name: n.name.into(),
            kind: "System sounds".into(),
            detail: format!("{ch} · notifications, UI"),
            hardware: false,
            sections: vec![
                section("Level", level_rows(p, id, "Stream")),
                section("Routing", route_rows(p, id, true)),
            ],
        },
        NodeRole::Processor { params } => View {
            name: n.name.into(),
            kind: "Processor".into(),
            detail: format!("{ch} · built-in DSP"),
            hardware: false,
            sections: vec![
                section(
                    "Parameters",
                    params
                        .iter()
                        .map(|(k, v)| kv(k, v.as_str()))
                        .chain(std::iter::once({
                            let mut b = row("toggle", "Bypass", "");
                            b.active = false;
                            b
                        }))
                        .collect(),
                ),
                section("Inputs", route_rows(p, id, false)),
                section("Outputs", route_rows(p, id, true)),
            ],
        },
        NodeRole::HardwareOut { device } => {
            let d = dev(device);
            View {
                name: n.name.into(),
                kind: "Hardware output".into(),
                detail: format!("{ch} · {}", d.map(|d| d.name).unwrap_or("—")),
                hardware: true,
                sections: vec![
                    section("Inputs", route_rows(p, id, false)),
                    section(
                        "Device",
                        vec![
                            kv("Device", d.map(|d| d.name).unwrap_or("—")),
                            kv("Transport", d.map(|d| d.bus).unwrap_or("—")),
                            kv(
                                "Rate",
                                d.map(|d| format!("{} kHz", d.sample_rate as f32 / 1000.0))
                                    .unwrap_or_default(),
                            ),
                            kv("Ports", format!("{} ch", n.channels)),
                            kv("Clock", d.map(|d| d.clock).unwrap_or("—")),
                        ]
                        .into_iter()
                        .chain(d.filter(|d| d.mock).map(|_| {
                            row(
                                "note",
                                "",
                                "MOCK DEVICE — illustrative hardware, not detected on this system.",
                            )
                        }))
                        .collect(),
                    ),
                ],
            }
        }
        NodeRole::Virtual { purpose } => View {
            name: n.name.into(),
            kind: "Virtual".into(),
            detail: format!("{ch} · {purpose}"),
            hardware: false,
            sections: vec![
                section("Inputs", route_rows(p, id, false)),
                section(
                    "Format",
                    vec![
                        kv("Channels", ch.clone()),
                        kv("Graph rate", format!("{} kHz", engine.sample_rate / 1000)),
                    ],
                ),
            ],
        },
    }
}
