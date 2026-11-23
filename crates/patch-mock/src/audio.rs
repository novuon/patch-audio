use crate::noise;
use patch_model::Provenance;
use patch_model::audio::*;
use patch_model::channels::{ChannelLayout, Levels};

/// The simulated master in the layout named by `PATCH_MOCK_LAYOUT`
/// (`5.1`, or a channel count such as `16`; stereo by default), so
/// multichannel metering can be seen without multichannel hardware.
fn mock_output(master: f32, t: f64) -> Levels {
    let layout = match std::env::var("PATCH_MOCK_LAYOUT").as_deref() {
        Ok("5.1") => ChannelLayout::surround_51(),
        Ok(n) => n
            .parse()
            .ok()
            .filter(|n| (1..=128).contains(n))
            .map(ChannelLayout::aux)
            .unwrap_or_default(),
        Err(_) => ChannelLayout::stereo(),
    };
    if layout == ChannelLayout::stereo() {
        return Levels::stereo([master, master - 0.8], None);
    }
    let peak_db = (0..layout.count())
        .map(|i| {
            if master <= SILENCE_DB {
                SILENCE_DB
            } else {
                master - 1.5 * i as f32 + (noise::wave(i as u32 * 7 + 3, t * 2.0) - 0.5) * 6.0
            }
        })
        .collect();
    Levels {
        layout,
        peak_db,
        vu: None,
    }
}

fn db_to_power(db: f32) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10f32.powf(db / 10.0)
    }
}

fn power_to_db(p: f32) -> f32 {
    if p <= 1e-12 {
        SILENCE_DB
    } else {
        10.0 * p.log10()
    }
}

/// Instantaneous peak of a plausible programme for each source, in dBFS
/// before the fader. Deterministic in `t`.
fn program_db(node: &str, seed: u32, t: f64) -> f32 {
    let w = |k: u32, speed: f64| noise::wave(seed * 17 + k, t * speed);
    match node {
        // Speech: syllables at ~5 Hz, phrases with pauses the gate closes on.
        "mic" => {
            let phrase = w(1, 0.35);
            if phrase < 0.3 {
                return -62.0 + w(2, 3.0) * 3.0;
            }
            -24.0 + w(3, 5.2) * 14.0 + phrase * 4.0
        }
        // Music: steady body, kick transients around 2 Hz.
        "spotify" => {
            let beat = ((t * 2.0).fract() as f32).powf(0.3);
            -16.0 + (1.0 - beat) * 6.0 + w(4, 1.1) * 3.0
        }
        // Game: bed plus occasional hits.
        "game" => -26.0 + w(5, 0.8) * 8.0 + w(6, 2.4).powi(8) * 16.0,
        // Browser media: talky video.
        "firefox" => -30.0 + w(7, 3.7) * 10.0,
        // System sounds: short blips now and then.
        "system" => {
            if w(8, 0.5) > 0.93 {
                -20.0
            } else {
                SILENCE_DB
            }
        }
        _ => -40.0 + w(9, 1.0) * 10.0,
    }
}

/// Simulated audio engine. Illustrative devices only — nothing here touches
/// PipeWire or ALSA.
pub struct MockAudioProvider {
    strips: Vec<Strip>,
    /// Re-patched targets: source → destination.
    patched: Vec<(NodeId, NodeId)>,
    /// The device the main output plays to.
    main_out: NodeId,
    /// Advanced Mixer knobs per strip (the master has none).
    controls: std::collections::HashMap<String, StripControls>,
    active_preset: Option<&'static str>,
    xruns: u32,
}

impl Default for MockAudioProvider {
    fn default() -> Self {
        let strip = |node, name, gain_db| Strip {
            node,
            name,
            gain_db,
            headroom: false,
            mute: false,
            solo: false,
            master: false,
        };
        let mut strips = vec![
            strip("mic", "Mic", -3.0),
            strip("firefox", "Firefox", -12.0),
            strip("spotify", "Spotify", -9.5),
            strip("game", "Game", -6.0),
            strip("system", "System", -18.0),
            strip("master", "Master", 0.0),
        ];
        strips[4].mute = true;
        strips[5].master = true;
        // Browser voice routing fixture for headless screenshots.
        let remote = std::env::var_os("PATCH_MOCK_REMOTE").is_some();
        if remote {
            strips.splice(0..0, [
                strip("space", "Chromium", 0.0),
                strip("engine.mic-5", "Voice 5", 0.0),
                strip("engine.mic-6", "Remote 6", 0.0),
            ]);
        }
        let mut provider = MockAudioProvider {
            strips,
            patched: vec![],
            main_out: "speakers",
            controls: {
                let sends = |a1: Option<f32>, f1: Option<f32>| {
                    Some([
                        Send { db: a1, pre: true },
                        Send { db: None, pre: true },
                        Send { db: f1, pre: false },
                        Send { db: None, pre: false },
                    ])
                };
                [
                    ("mic", StripControls { gain_db: Some(9.0), tone: true, high_db: 2.0, mid_db: -3.0, low_db: -12.0, pan: 0.0, sends: sends(Some(0.0), Some(-12.0)), sends_off: None }),
                    ("firefox", StripControls { gain_db: Some(0.0), tone: true, pan: -0.35, sends: sends(None, None), ..Default::default() }),
                    ("spotify", StripControls { gain_db: Some(-3.0), tone: true, low_db: 3.0, pan: 0.35, sends: sends(Some(-6.0), None), ..Default::default() }),
                    ("game", StripControls { gain_db: Some(0.0), tone: true, sends: sends(None, None), ..Default::default() }),
                    ("system", StripControls { gain_db: Some(-6.0), tone: true, high_db: -6.0, sends: sends(None, None), ..Default::default() }),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect()
            },
            active_preset: Some("studio"),
            xruns: 0,
        };
        if remote {
            let none = Some(std::array::from_fn(|i| Send { db: None, pre: i < 2 }));
            provider.controls.insert("engine.mic-6".into(), StripControls {
                gain_db: Some(0.0), tone: true, sends: Some([Send { db: Some(-57.0), pre: true }, Send { db: None, pre: true }, Send { db: None, pre: false }, Send { db: None, pre: false }]),
                sends_off: Some(patch_model::audio::SendsOff::Remote), ..Default::default()
            });
            provider.controls.insert("engine.mic-5".into(), StripControls {
                gain_db: Some(0.0),
                tone: true,
                sends: Some(std::array::from_fn(|i| Send { db: (i == 0).then_some(0.0), pre: i < 2 })),
                ..Default::default()
            });
            provider.controls.insert("space".into(), StripControls {
                gain_db: Some(0.0), tone: true, sends: none, sends_off: Some(patch_model::audio::SendsOff::OnLane { lane: 5, far_end: false }), ..Default::default()
            });
        }
        provider
    }
}

fn ports(prefix: &str, n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("{prefix} {i}")).collect()
}

fn group(name: &'static str, direction: Direction, ports: Vec<String>) -> PortGroup {
    PortGroup {
        name,
        direction,
        ports,
    }
}

impl AudioProvider for MockAudioProvider {
    fn provenance(&self) -> Provenance {
        Provenance::Simulated
    }

    fn engine(&self) -> EngineStatus {
        EngineStatus {
            backend: "MOCK ENGINE".into(),
            sample_rate: 96_000,
            buffer: 128,
            xruns: Some(self.xruns),
            dsp_load: 0.14,
        }
    }

    fn devices(&self) -> Vec<Device> {
        use Direction::*;
        vec![
            Device {
                id: "scarlett",
                name: "Scarlett 18i20",
                bus: "USB 2.0 · Focusrite",
                sample_rate: 96_000,
                clock: "Internal · master",
                groups: vec![
                    group("Analog", In, ports("Analog In", 8)),
                    group(
                        "S/PDIF",
                        In,
                        vec!["S/PDIF In L".into(), "S/PDIF In R".into()],
                    ),
                    group("ADAT", In, ports("ADAT In", 8)),
                    group("Analog", Out, ports("Line Out", 10)),
                    group(
                        "S/PDIF",
                        Out,
                        vec!["S/PDIF Out L".into(), "S/PDIF Out R".into()],
                    ),
                    group("ADAT", Out, ports("ADAT Out", 8)),
                ],
                mock: true,
            },
            Device {
                id: "fireface",
                name: "Fireface UCX II",
                bus: "USB 2.0 · RME",
                sample_rate: 96_000,
                clock: "ADAT In · locked",
                groups: vec![
                    group("Mic/Inst", In, ports("Mic In", 2)),
                    group(
                        "Analog",
                        In,
                        (3..=8).map(|i| format!("Line In {i}")).collect(),
                    ),
                    group(
                        "S/PDIF",
                        In,
                        vec!["S/PDIF In L".into(), "S/PDIF In R".into()],
                    ),
                    group("ADAT", In, ports("ADAT In", 8)),
                    group("Analog", Out, ports("Line Out", 6)),
                    group("Phones", Out, vec!["Phones L".into(), "Phones R".into()]),
                    group(
                        "S/PDIF",
                        Out,
                        vec!["S/PDIF Out L".into(), "S/PDIF Out R".into()],
                    ),
                    group("ADAT", Out, ports("ADAT Out", 8)),
                ],
                mock: true,
            },
            Device {
                id: "ntusb",
                name: "Rode NT-USB",
                bus: "USB 2.0 · class compliant",
                sample_rate: 48_000,
                clock: "Internal",
                groups: vec![
                    group("Capsule", In, vec!["Mic".into()]),
                    group("Phones", Out, vec!["Phones L".into(), "Phones R".into()]),
                ],
                mock: true,
            },
            Device {
                id: "builtin",
                name: "Built-in Audio",
                bus: "PCI · HDA",
                sample_rate: 48_000,
                clock: "Internal",
                groups: vec![
                    group("Analog", In, vec!["Line In L".into(), "Line In R".into()]),
                    group("Analog", Out, vec!["Speaker L".into(), "Speaker R".into()]),
                    group("HDMI", Out, ports("HDMI", 8)),
                ],
                mock: true,
            },
        ]
    }

    fn nodes(&self) -> Vec<Node> {
        use NodeKind::*;
        let n = |id, name, kind, role, channels| Node {
            id,
            name,
            kind,
            role,
            channels,
        };
        let processor = |params: &[(&'static str, &str)]| NodeRole::Processor {
            params: params.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        };
        let app = |process, rules: &[&str]| NodeRole::AppStream {
            process,
            rules: rules.iter().map(|r| r.to_string()).collect(),
        };
        vec![
            n(
                "mic",
                "Microphone",
                Source,
                NodeRole::HardwareIn { device: "ntusb" },
                1,
            ),
            n(
                "firefox",
                "Firefox",
                Source,
                app("firefox", &["media.* → Headphones", "duck −12 dB on Mic"]),
                2,
            ),
            n(
                "spotify",
                "Spotify",
                Source,
                app("spotify", &["always → Headphones", "also → Stream Mix"]),
                2,
            ),
            n(
                "game",
                "Game",
                Source,
                app("gamescope", &["exclusive → Headphones"]),
                6,
            ),
            n("system", "System", Source, NodeRole::System, 2),
            n(
                "scarlett-in",
                "Scarlett In 1–8",
                Source,
                NodeRole::HardwareIn { device: "scarlett" },
                8,
            ),
            n(
                "gate",
                "Noise Gate",
                Processor,
                processor(&[
                    ("Threshold", "−48 dB"),
                    ("Attack", "2 ms"),
                    ("Release", "120 ms"),
                ]),
                1,
            ),
            n(
                "eq",
                "EQ",
                Processor,
                processor(&[
                    ("Low cut", "80 Hz"),
                    ("Presence", "+2.5 dB @ 4 kHz"),
                    ("Bands", "6"),
                ]),
                1,
            ),
            n(
                "comp",
                "Compressor",
                Processor,
                processor(&[
                    ("Ratio", "3:1"),
                    ("Threshold", "−18 dB"),
                    ("Makeup", "+4 dB"),
                ]),
                1,
            ),
            n(
                "limiter",
                "Limiter",
                Processor,
                processor(&[("Ceiling", "−1.0 dBTP"), ("Lookahead", "1.5 ms")]),
                2,
            ),
            n(
                "headphones",
                "Headphones",
                Destination,
                NodeRole::HardwareOut { device: "scarlett" },
                2,
            ),
            n(
                "speakers",
                "Speakers",
                Destination,
                NodeRole::HardwareOut { device: "scarlett" },
                2,
            ),
            n(
                "recorder",
                "Recorder",
                Destination,
                NodeRole::Virtual {
                    purpose: "multitrack capture",
                },
                10,
            ),
            n(
                "stream",
                "Stream Mix",
                Destination,
                NodeRole::Virtual {
                    purpose: "virtual output",
                },
                2,
            ),
            n(
                "rme-out",
                "Fireface Out 1–8",
                Destination,
                NodeRole::HardwareOut { device: "fireface" },
                8,
            ),
        ]
    }

    fn routes(&self) -> Vec<Route> {
        let r = |from, to, channels| Route { from, to, channels };
        let mut routes = vec![
            r("mic", "gate", 1),
            r("gate", "eq", 1),
            r("eq", "comp", 1),
            r("comp", "stream", 1),
            r("comp", "recorder", 1),
            r("comp", "headphones", 1),
            r("firefox", "headphones", 2),
            r("spotify", "limiter", 2),
            r("spotify", "stream", 2),
            r("game", "headphones", 6),
            r("system", "limiter", 2),
            r("limiter", "speakers", 2),
            r("scarlett-in", "recorder", 8),
            r("scarlett-in", "rme-out", 8),
        ];
        for (from, to) in &self.patched {
            let ch = routes
                .iter()
                .find(|r| r.from == *from)
                .map(|r| r.channels)
                .unwrap_or(2);
            routes.retain(|r| r.from != *from);
            routes.push(Route {
                from,
                to,
                channels: ch,
            });
        }
        routes
    }

    fn strips(&self) -> Vec<Strip> {
        self.strips.clone()
    }

    fn targets(&self, node: &str) -> Vec<(&'static str, &'static str, bool)> {
        let nodes = self.nodes();
        let Some(n) = nodes.iter().find(|n| n.id == node) else {
            return vec![];
        };
        if n.kind == NodeKind::Destination {
            return vec![];
        }
        let current: Vec<NodeId> = self
            .routes()
            .iter()
            .filter(|r| r.from == node)
            .map(|r| r.to)
            .collect();
        nodes
            .iter()
            .filter(|d| {
                d.id != node
                    && (d.kind == NodeKind::Destination
                        || (n.kind == NodeKind::Source && d.kind == NodeKind::Processor))
            })
            .map(|d| (d.id, d.name, current.contains(&d.id)))
            .collect()
    }

    fn strip_controls(&self, node: &str) -> Option<StripControls> {
        self.controls.get(node).copied()
    }

    /// Renders of the console (Patch selected in the bar): set
    /// `PATCH_MOCK_CONSOLE=1`.
    fn professional_routing(&self) -> bool {
        std::env::var_os("PATCH_MOCK_CONSOLE").is_some()
    }

    fn buses(&self) -> Vec<BusStrip> {
        if !self.professional_routing() {
            return vec![];
        }
        let b = |id, name, gain_db, output: &str, param| BusStrip { id, name, gain_db, mute: false, output: output.into(), param };
        vec![
            b("aux1", "AUX 1", 0.0, "Fireface · AUX 3 + 4", None),
            b("aux2", "AUX 2", -6.0, "Patch AUX 2 source", None),
            b("fx1", "FX 1 · REVERB", -8.0, "into Main", Some(("SIZE", 0.6))),
            b("fx2", "FX 2 · DELAY", -14.0, "into Main", Some(("TIME", 0.35))),
        ]
    }

    fn strip_channel(&self, node: &str) -> Option<String> {
        match node {
            "mic" => Some("mic-1".into()),
            "firefox" => Some("app-1".into()),
            _ => None,
        }
    }

    fn assign_channel(&mut self, node: &str) -> Result<String, String> {
        match node {
            "spotify" => Ok("app-2".into()),
            "game" => Ok("app-3".into()),
            _ => self.strip_channel(node).ok_or_else(|| "this strip has no engine channel".into()),
        }
    }

    fn set_strip_control(&mut self, node: &str, control: &str, value: f32) -> Result<(), String> {
        let c = self.controls.get_mut(node).ok_or("this strip has no knobs")?;
        if let Some(i) = control.strip_prefix("send").and_then(|n| n.parse::<usize>().ok()) {
            if let Some(s) = c.sends.as_mut().and_then(|s| s.get_mut(i.wrapping_sub(1))) {
                s.db = (value > -60.0).then_some(value);
            }
            return Ok(());
        }
        if let Some(i) = control.strip_prefix("pre").and_then(|n| n.parse::<usize>().ok()) {
            if let Some(s) = c.sends.as_mut().and_then(|s| s.get_mut(i.wrapping_sub(1))) {
                s.pre = value > 0.5;
            }
            return Ok(());
        }
        match control {
            "gain" => c.gain_db = Some(value),
            "high" => c.high_db = value,
            "mid" => c.mid_db = value,
            "low" => c.low_db = value,
            "pan" => c.pan = value,
            _ => return Err(format!("no control '{control}'")),
        }
        Ok(())
    }

    fn main_outputs(&self) -> Vec<OutputChoice> {
        self.nodes()
            .into_iter()
            .filter(|n| matches!(n.role, NodeRole::HardwareOut { .. }))
            .map(|n| OutputChoice {
                id: n.id,
                name: n.name.to_string(),
                kind: "USB",
                detail: format!("48 kHz · {} ch", n.channels),
                current: n.id == self.main_out,
            })
            .collect()
    }

    fn connect(&mut self, from: &str, to: &str) -> Result<(), String> {
        let nodes = self.nodes();
        if matches!(from, "master" | "output") {
            let t = nodes
                .iter()
                .find(|n| n.id == to && matches!(n.role, NodeRole::HardwareOut { .. }))
                .ok_or("not an output")?;
            self.main_out = t.id;
            return Ok(());
        }
        let f = nodes
            .iter()
            .find(|n| n.id == from)
            .ok_or("unknown source")?;
        let t = nodes
            .iter()
            .find(|n| n.id == to)
            .ok_or("unknown destination")?;
        self.patched.retain(|(a, _)| *a != f.id);
        self.patched.push((f.id, t.id));
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        let p = |id, name, summary, changes: &[&'static str]| Preset {
            id,
            name,
            summary,
            changes: changes.to_vec(),
            plugin_mock: None,
        };
        vec![
            p(
                "work",
                "Work",
                "Headset, calls ducked over media",
                &[
                    "Output → Built-in Audio",
                    "Firefox ducks −12 dB under Mic",
                    "48 kHz · 256 samples",
                    "Layout → Compact",
                ],
            ),
            p(
                "gaming",
                "Gaming",
                "Game exclusive, voice chat on Mic",
                &[
                    "Game → Headphones, exclusive",
                    "Mic chain: Gate → Comp",
                    "48 kHz · 128 samples",
                ],
            ),
            p(
                "recording",
                "Recording",
                "Scarlett 1–8 → Recorder, 96 kHz",
                &[
                    "Scarlett In 1–8 → Recorder (10 ch)",
                    "Monitoring via Headphones only",
                    "96 kHz · 64 samples",
                    "Layout → Recording",
                ],
            ),
            p(
                "studio",
                "Studio",
                "Monitors + Headphones, full chain",
                &[
                    "Speakers + Headphones",
                    "Mic chain: Gate → EQ → Comp",
                    "96 kHz · 128 samples",
                    "Layout → Studio",
                ],
            ),
            p(
                "streaming",
                "Streaming",
                "Stream Mix with Mic chain + Spotify",
                &[
                    "Mic chain + Spotify → Stream Mix",
                    "Limiter on Stream Mix at −1 dBTP",
                    "48 kHz · 256 samples",
                ],
            ),
            Preset {
                id: "dj",
                name: "DJ",
                summary: "Decks A/B, cue on Headphones",
                changes: vec![
                    "Deck A / Deck B surfaces",
                    "Cue bus → Headphones",
                    "Master → Speakers",
                    "Layout → DJ",
                ],
                plugin_mock: Some("DJ"),
            },
        ]
    }

    fn active_preset(&self) -> Option<&'static str> {
        self.active_preset
    }

    fn set_gain(&mut self, node: &str, db: f32) {
        if let Some(s) = self.strips.iter_mut().find(|s| s.node == node) {
            s.gain_db = db.clamp(-60.0, 6.0);
        }
    }

    fn set_mute(&mut self, node: &str, mute: bool) {
        if let Some(s) = self.strips.iter_mut().find(|s| s.node == node) {
            s.mute = mute;
        }
    }

    fn set_solo(&mut self, node: &str, solo: bool) -> Result<(), String> {
        // Program mix and Main are not soloable: the key does nothing.
        if node == "master" || node == "output" {
            return Ok(());
        }
        if let Some(s) = self.strips.iter_mut().find(|s| s.node == node) {
            s.solo = solo;
        }
        Ok(())
    }

    fn apply_preset(&mut self, id: &str) {
        if let Some(p) = self
            .presets()
            .into_iter()
            .find(|p| p.id == id && p.plugin_mock.is_none())
        {
            self.active_preset = Some(p.id);
        }
    }

    fn meters(&mut self, t: f64) -> MeterFrame {
        let any_solo = self.strips.iter().any(|s| s.solo && !s.master);
        let sources: Vec<f32> = self
            .strips
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.master)
            .map(|(i, s)| {
                let audible = !s.mute && (!any_solo || s.solo);
                if audible {
                    program_db(s.node, i as u32, t) + s.gain_db
                } else {
                    SILENCE_DB
                }
            })
            .collect();
        let master_gain = self
            .strips
            .iter()
            .find(|s| s.master)
            .map(|s| (s.gain_db, s.mute));
        let sum = sources.iter().map(|db| db_to_power(*db)).sum::<f32>();
        let master = match master_gain {
            Some((_, true)) => SILENCE_DB,
            Some((g, false)) => power_to_db(sum) + g,
            None => power_to_db(sum),
        };
        let mut strips_db = sources.clone();
        strips_db.push(master);
        let mic = sources.first().copied().unwrap_or(SILENCE_DB);
        // Rough pink-ish programme spectrum riding on the master level.
        let spectrum_db = (0..32)
            .map(|b| {
                let x = b as f32 / 31.0;
                // Low-mid body, rolling off towards the top octave.
                let shape = -((x - 0.28) * 3.2).powi(2) * 6.0 - x * 12.0;
                let motion = (noise::wave(b * 13 + 5, t * 1.4) - 0.5) * 9.0;
                if master <= SILENCE_DB {
                    SILENCE_DB
                } else {
                    master - 12.0 + shape + motion
                }
            })
            .collect();
        // One simulated xrun, once, to show how the status reads.
        if t > 42.0 && self.xruns == 0 {
            self.xruns = 1;
        }
        MeterFrame {
            buses_db: if self.professional_routing() { vec![[-14.0, -15.0], [-60.0; 2], [-22.0, -21.0], [-30.0, -31.0]] } else { vec![] },
            strips_lr: None,
            strips_post_fader: false,
            deck_input_db: None,
            input: Levels::stereo([mic, mic - 0.4], None),
            output: mock_output(master, t),
            true_peak_db: master + 0.4,
            spectrum_db,
            strips_db,
            loudness: None,
            strips_vu: None,
            sync_ms: None,
            timing: None,
            input_label: "Mic".into(),
            input_sources: vec![],
            output_label: "Master".into(),
        }
    }
}
