//! The graph, strips, devices, presets and selection of the mixer surfaces.

use super::*;

impl AudioApp {
    pub(super) fn provider_nodes(&self) -> Vec<Node> {
        self.provider.borrow().nodes()
    }

    pub(super) fn dj_windows(&self) -> Vec<PatchWindow> {
        self.windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| {
                matches!(
                    w.get_surface().as_str(),
                    "deck-a" | "deck-b" | "deck-c" | "deck-d" | "dj-mixer" | "dj-club" | "dj-mini" | "library" | "dj" | "performer" | "visualiser"
                )
            })
            .collect()
    }

    /// `broadcast` for what changes every frame: only windows on screen.
    /// A hidden window that is written to still draws (Hyprland's
    /// render_unfocused gives it frames), and these values reach it again
    /// on its first frame in sight.
    pub(super) fn broadcast_in_sight(&self, f: impl Fn(&AudioState)) {
        self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            if self.in_sight(&w) {
                f(&w.global::<AudioState>());
            }
        }
    }

    /// The DJ windows on screen, for per-frame updates.
    pub(super) fn dj_windows_in_sight(&self) -> Vec<PatchWindow> {
        self.dj_windows().into_iter().filter(|w| self.in_sight(w)).collect()
    }

    /// Whether a window of one of `surfaces` is on screen.
    pub(super) fn surface_in_sight(&self, surfaces: &[&str]) -> bool {
        self.windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .any(|w| surfaces.contains(&w.get_surface().as_str()) && self.in_sight(&w))
    }

    pub(super) fn broadcast(&self, f: impl Fn(&AudioState)) {
        self.windows.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) {
            f(&w.global::<AudioState>());
        }
    }

    pub(super) fn device_name(&self, id: &str) -> &'static str {
        self.provider
            .borrow()
            .devices()
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| d.name)
            .unwrap_or("—")
    }

    pub(super) fn node_detail(&self, n: &Node) -> String {
        match &n.role {
            NodeRole::HardwareIn { device } | NodeRole::HardwareOut { device } => {
                self.device_name(device).to_string()
            }
            NodeRole::AppStream { process, .. } => format!("app · {process}"),
            NodeRole::System => "desktop sounds".into(),
            NodeRole::Processor { params } => {
                params.first().map(|(_, v)| v.clone()).unwrap_or_default()
            }
            NodeRole::Virtual { purpose } => purpose.to_string(),
        }
    }

    /// Nodes and routes on the signal path through the selection.
    pub(super) fn path(&self, routes: &[Route], sel: &str) -> HashSet<String> {
        let mut seen: HashSet<String> = HashSet::from([sel.to_string()]);
        for downstream in [true, false] {
            let mut frontier = vec![sel.to_string()];
            while let Some(n) = frontier.pop() {
                for r in routes {
                    let (from, to) = if downstream {
                        (r.from, r.to)
                    } else {
                        (r.to, r.from)
                    };
                    if from == n && seen.insert(to.to_string()) {
                        frontier.push(to.to_string());
                    }
                }
            }
        }
        seen
    }

    pub(super) fn downstream(&self, routes: &[Route], from: &str) -> HashSet<String> {
        let mut seen = HashSet::new();
        let mut frontier = vec![from.to_string()];
        while let Some(n) = frontier.pop() {
            for r in routes.iter().filter(|r| r.from == n) {
                if seen.insert(r.to.to_string()) {
                    frontier.push(r.to.to_string());
                }
            }
        }
        seen
    }

    pub(super) fn rebuild_graph(&self) {
        let nodes = self.provider_nodes();
        let routes = self.provider.borrow().routes();
        let strips = self.provider.borrow().strips();
        let routeable: HashSet<String> = nodes
            .iter()
            .filter(|n| !self.provider.borrow().targets(n.id).is_empty())
            .map(|n| n.id.to_string())
            .collect();
        let sel = self.selected.borrow().clone();
        let path = self.path(&routes, &sel);
        // Share a row across the three columns when a cable joins them, so the
        // line runs across instead of arcing from a centered block to a centered block.
        // Device blocks stay together: a routed port is not lifted out of its device.
        let kinds: Vec<NodeKind> = nodes.iter().map(|n| n.kind).collect();
        let presented = crate::graph_layout::device_rows(
            &nodes
                .iter()
                .map(|n| (n.kind, n.name))
                .collect::<Vec<_>>(),
        );
        let groups: Vec<u32> = presented.iter().map(|row| row.group).collect();
        let index = |id: &str| nodes.iter().position(|n| n.id == id);
        let route_ix: Vec<(usize, usize)> = routes
            .iter()
            .filter_map(|r| Some((index(r.from)?, index(r.to)?)))
            .collect();
        let positions = crate::graph_layout::place(&kinds, &groups, &route_ix);
        let plugin_node = self.plugin_node();
        let plugin_visible = plugin_node.is_some();
        let mut rows = [0i32; 3];
        let placed: Vec<_> = nodes
            .into_iter()
            .zip(positions)
            .zip(presented)
            .map(|((n, (c, r)), row)| {
                rows[c as usize] = rows[c as usize].max(r + 1);
                (n, c, r, row)
            })
            .collect();
        if plugin_visible {
            rows[1] += 1;
        }
        let pos = |id: &str| {
            placed
                .iter()
                .find(|(n, ..)| n.id == id)
                .map(|(_, c, r, _)| (*c, *r))
        };
        let muted = |id: &str| strips.iter().any(|s| s.node == id && s.mute);
        let name_of = |id: &'static str| -> String {
            placed
                .iter()
                .find(|(n, ..)| n.id == id)
                .map(|(_, _, _, row)| {
                    if row.nest && !row.device.is_empty() {
                        format!("{} · {}", row.device, row.name)
                    } else {
                        row.name.clone()
                    }
                })
                .unwrap_or_else(|| id.to_string())
        };

        let mut g: Vec<GNode> = placed
            .iter()
            .map(|(n, c, r, row)| {
                let detail = self.node_detail(n);
                let detail = if row.nest || detail.is_empty() || detail == "—" || detail == row.name
                {
                    String::new()
                } else {
                    detail
                };
                GNode {
                    id: n.id.into(),
                    name: row.name.as_str().into(),
                    col: *c,
                    row: *r,
                    channels: n.channels as i32,
                    detail: detail.into(),
                    targets: {
                        let t: Vec<String> = routes
                            .iter()
                            .filter(|r| r.from == n.id)
                            .map(|r| name_of(r.to))
                            .collect();
                        if t.is_empty() {
                            SharedString::from("")
                        } else {
                            format!("→ {}", t.join(", ")).into()
                        }
                    },
                    selected: n.id == sel,
                    related: n.id != sel && path.contains(n.id),
                    muted: muted(n.id),
                    hardware: matches!(
                        n.role,
                        NodeRole::HardwareIn { .. } | NodeRole::HardwareOut { .. }
                    ),
                    routeable: routeable.contains(n.id),
                    nest: row.nest,
                    head: row.head,
                }
            })
            .collect();
        if let Some((on, name, detail)) = plugin_node {
            g.push(GNode {
                id: "patch.plugins".into(),
                name: name.into(),
                col: 1,
                row: rows[1] - 1,
                channels: 2,
                detail: detail.into(),
                targets: "→ MAIN output".into(),
                selected: false,
                related: false,
                muted: !on,
                hardware: false,
                routeable: false,
                nest: false,
                head: true,
            });
        }
        let e: Vec<GEdge> = routes
            .iter()
            .filter_map(|r| {
                let (a_col, a_row) = pos(r.from)?;
                let (b_col, b_row) = pos(r.to)?;
                Some(GEdge {
                    a_col,
                    a_row,
                    b_col,
                    b_row,
                    channels: r.channels as i32,
                    selected: path.contains(r.from) && path.contains(r.to),
                    muted: muted(r.from),
                })
            })
            .collect();
        self.nodes.set_vec(g);
        self.edges.set_vec(e);
        let counts = ModelRc::new(VecModel::from(rows.to_vec()));
        self.broadcast(|s| s.set_col_rows(counts.clone()));
    }

    /// Faders moved elsewhere (the DJ Booth's own volume, an AI fade, the
    /// CLI, MIDI): the Mixer's strips follow, row by row, without a rebuild.
    pub(super) fn sync_strips(&self) {
        self.rebuild_buses();
        let all = self.provider.borrow().strips();
        let shown: Vec<_> = all.iter().filter(|s| !s.node.starts_with("engine.deck-")).collect();
        if shown.len() != self.strips.row_count()
            || shown.iter().enumerate().any(|(i,s)| self.strips.row_data(i).is_none_or(|r| r.id != s.node)) {
            self.rebuild_strips();
            return;
        }
        let live = self.is_live();
        for (i, s) in shown.iter().enumerate() {
            let Some(mut row) = self.strips.row_data(i) else { continue };
            let headroom = s.headroom || strip_has_headroom(s.node, live);
            let gain = if s.node == "output" || s.node == "system-sounds" {
                output_pos(s.gain_db)
            } else {
                fader_pos_with_headroom(s.node, live, headroom, s.gain_db)
            };
            let db = strip_gain_text(s.node, live, headroom, s.gain_db);
            let knobs = knob_fields(&mut row, self.provider.borrow().strip_controls(s.node));
            if row.id == s.node
                && ((row.gain - gain).abs() > 1e-4
                    || row.name != s.name
                    || row.db != db
                    || row.headroom != headroom
                    || row.mute != s.mute
                    || knobs)
            {
                row.name = s.name.into();
                row.gain = gain;
                row.db = db.into();
                row.headroom = headroom;
                row.mute = s.mute;
                self.strips.set_row_data(i, row);
            }
        }
    }

    /// A knob turned (−1 … 1): into dB (pan as is) for the strip's engine
    /// channel, and the strip shows where it landed.
    pub(super) fn set_knob(&self, id: &str, control: &str, pos: f32) {
        let value = if control.starts_with("send") {
            send_db(pos)
        } else if control.starts_with("pre") {
            pos
        } else {
            knob_value(control, pos)
        };
        let result = self.provider.borrow_mut().set_strip_control(id, control, value);
        if let Err(e) = result {
            *self.toast.borrow_mut() = format!("Can't turn that: {e}");
            self.push_toast();
        }
        let controls = self.provider.borrow().strip_controls(id);
        if let Some(i) = self.strips.iter().position(|r| r.id == id)
            && let Some(mut row) = self.strips.row_data(i)
            && knob_fields(&mut row, controls)
        {
            self.strips.set_row_data(i, row);
        }
    }

    /// The console's buses from the provider (none unless Patch is the
    /// output in the bar); levels stay as the meters left them.
    pub(super) fn rebuild_buses(&self) {
        let old: Vec<BusItem> = self.buses.iter().collect();
        let buses: Vec<BusItem> = self
            .provider
            .borrow()
            .buses()
            .into_iter()
            .map(|b| {
                let was = old.iter().find(|o| o.id == b.id);
                BusItem {
                    id: b.id.into(),
                    name: b.name.into(),
                    output: b.output.into(),
                    routable: b.id.starts_with("aux"),
                    gain: gain_to_pos(b.gain_db),
                    db: db_text(b.gain_db).into(),
                    mute: b.mute,
                    level: was.map_or(0.0, |o| o.level),
                    peak: was.map_or(0.0, |o| o.peak),
                    param_label: b.param.map(|(l, _)| l).unwrap_or("").into(),
                    param: b.param.map_or(0.0, |(_, v)| v),
                }
            })
            .collect();
        let changed = old != buses;
        if changed {
            if old.len() == buses.len() {
                for (i, b) in buses.into_iter().enumerate() {
                    if self.buses.row_data(i).as_ref() != Some(&b) {
                        self.buses.set_row_data(i, b);
                    }
                }
            } else {
                self.buses.set_vec(buses);
            }
        }
        if changed {
            self.push_fit();
        }
    }

    pub(super) fn set_bus_gain(&self, id: &str, pos: f32) {
        let r = self.provider.borrow_mut().set_bus(id, Some(pos_to_gain(pos)), None, None);
        self.bus_result(r);
    }

    pub(super) fn toggle_bus_mute(&self, id: &str) {
        let mute = self.buses.iter().find(|b| b.id == id).is_some_and(|b| !b.mute);
        let r = self.provider.borrow_mut().set_bus(id, None, Some(mute), None);
        self.bus_result(r);
    }

    pub(super) fn set_bus_param(&self, id: &str, v: f32) {
        let r = self.provider.borrow_mut().set_bus(id, None, None, Some(v.clamp(0.0, 1.0)));
        self.bus_result(r);
    }

    /// Where an AUX can go (its output picker).
    pub(super) fn open_bus_targets(&self, id: &str) {
        let targets: Vec<TargetItem> = self
            .provider
            .borrow()
            .targets(id)
            .into_iter()
            .map(|(id, name, current)| TargetItem { id: id.into(), name: name.into(), current })
            .collect();
        self.bus_targets.set_vec(targets);
    }

    fn bus_result(&self, r: Result<(), String>) {
        if let Err(e) = r {
            *self.toast.borrow_mut() = format!("Can't change that bus: {e}");
            self.push_toast();
        }
        self.rebuild_buses();
    }

    pub(super) fn rebuild_strips(&self) {
        self.rebuild_buses();
        let sel = self.selected.borrow().clone();
        let outputs: Vec<OutputItem> = self
            .provider
            .borrow()
            .main_outputs()
            .into_iter()
            .map(|o| OutputItem {
                id: o.id.into(),
                name: o.name.into(),
                kind: o.kind.into(),
                detail: o.detail.into(),
                current: o.current,
            })
            .collect();
        if self.outputs.iter().ne(outputs.iter().cloned()) {
            let now: SharedString = outputs.iter().find(|o| o.current).map(|o| o.name.clone()).unwrap_or_default();
            self.broadcast(|s| s.set_main_output(now.clone()));
            self.outputs.set_vec(outputs);
        }
        let nodes = self.provider_nodes();
        let old: Vec<StripItem> = self.strips.iter().collect();
        let all = self.provider.borrow().strips();
        {
            // The EQ tabs name the same channels, including user-renamed
            // callers, rather than exposing runtime stream identifiers.
            let mut labels = self.master_eq.mixer_labels.borrow_mut();
            labels.clear();
            let provider = self.provider.borrow();
            for strip in &all {
                if let Some(channel) = provider.strip_channel(strip.node)
                    && patch_core::stream::CHANNELS.contains(&channel.as_str())
                { labels.insert(channel, strip.name.to_string()); }
            }
        }
        let professional = self.provider.borrow().professional_routing();
        self.broadcast(|s| s.set_professional_routing(professional));
        self.deck_strips.set(
            ["engine.deck-a", "engine.deck-b", "engine.deck-c", "engine.deck-d"].map(|id| all.iter().position(|s| s.node == id)),
        );
        let live = self.is_live();
        // The Mixer shows the DJ Booth as one strip; the decks (last in the
        // provider's order, so meter indices hold) stay in the DJ Booth.
        let strips: Vec<StripItem> = all
            .iter()
            .filter(|s| !s.node.starts_with("engine.deck-"))
            .map(|s| StripItem {
                id: s.node.into(),
                name: s.name.into(),
                application: self.provider.borrow().is_application_strip(s.node),
                available: self.provider.borrow().strip_available(s.node),
                persistent: self.provider.borrow().application_strip_kept(s.node),
                channels: nodes
                    .iter()
                    .find(|n| n.id == s.node)
                    .map(|n| n.channels as i32)
                    .unwrap_or(2),
                gain: fader_pos_with_headroom(s.node, live, s.headroom, s.gain_db),
                db: strip_gain_text(s.node, live, s.headroom, s.gain_db).into(),
                mute: s.mute,
                solo: s.solo,
                master: s.master,
                headroom: s.headroom || strip_has_headroom(s.node, live),
                level: old.iter().find(|o| o.id == s.node).map(|o| o.level).unwrap_or(0.0),
                peak: old.iter().find(|o| o.id == s.node).map(|o| o.peak).unwrap_or(0.0),
                level_l: old.iter().find(|o| o.id == s.node).map(|o| o.level_l).unwrap_or(0.0),
                level_r: old.iter().find(|o| o.id == s.node).map(|o| o.level_r).unwrap_or(0.0),
                peak_l: old.iter().find(|o| o.id == s.node).map(|o| o.peak_l).unwrap_or(0.0),
                peak_r: old.iter().find(|o| o.id == s.node).map(|o| o.peak_r).unwrap_or(0.0),
                vu: old.iter().find(|o| o.id == s.node).map(|o| o.vu).unwrap_or(0.0),
                clip: old.iter().find(|o| o.id == s.node).map(|o| o.clip).unwrap_or(false),
                selected: s.node == sel,
                ..Default::default()
            })
            .map(|mut row| {
                let controls = self.provider.borrow().strip_controls(row.id.as_str());
                knob_fields(&mut row, controls);
                row
            })
            .collect();
        if old.iter().map(|s| s.id.as_str()).ne(strips.iter().map(|s| s.id.as_str())) {
            self.clip_until.borrow_mut().clear();
            let mut meters = self.metering.borrow_mut();
            meters.strips.clear();
            meters.strips_lr.clear();
            meters.strips_vu.clear();
        }
        // Where the Patch Mix ends up: its output (the system volume and
        // the device come after it). The console's MAIN column reads the
        // same walk as its PLAYS TO rows — and PLAYS TO lists where the mix
        // ends up, never the destinations it does not play to.
        let routes = self.provider.borrow().routes();
        let out = self.downstream(&routes, if live { "engine.program" } else { "master" });
        let reached: Vec<&Node> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Destination && out.contains(n.id))
            .collect();
        self.main_dests.set_vec(vec![true; reached.len()]);
        let dest_labels: Vec<SharedString> = reached
            .iter()
            .map(|n| SharedString::from(super::main_destination_label(n.name)))
            .collect();
        let mix_out = reached
            .iter()
            .map(|n| n.name.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mix_out: slint::SharedString = if mix_out.is_empty() {
            "".into()
        } else {
            format!("→ {mix_out}").into()
        };
        self.broadcast(|s| {
            s.set_mix_out(mix_out.clone());
            s.set_dests(ModelRc::new(VecModel::from(dest_labels.clone())));
        });
        let sel_strip = strips
            .iter()
            .position(|s| s.id == sel.as_str())
            .map(|i| i as i32)
            .unwrap_or(-1);
        // + Mic while a mic channel is free (two).
        let mics = strips
            .iter()
            .filter(|s| s.id.starts_with("engine.mic-"))
            .count();
        let can = self.is_live() && mics < 16 && professional;
        self.broadcast(|s| s.set_can_add_mic(can));
        if self.strips.row_count() == strips.len() {
            for (i, s) in strips.into_iter().enumerate() {
                self.strips.set_row_data(i, s);
            }
        } else {
            self.strips.set_vec(strips);
        }
        self.broadcast(|s| s.set_sel_strip(sel_strip));
        self.push_fit();
    }

    pub(super) fn rebuild_devices(&self) {
        let nodes = self.provider_nodes();
        let expanded = self.expanded.borrow();
        let cursor = self.device_cursor.get();
        let devices: Vec<DeviceItem> = self
            .provider
            .borrow()
            .devices()
            .into_iter()
            .enumerate()
            .map(|(i, d)| {
                let used = |dir: Direction| -> usize {
                    nodes
                        .iter()
                        .filter(|n| match (&n.role, dir) {
                            (NodeRole::HardwareIn { device }, Direction::In) => *device == d.id,
                            (NodeRole::HardwareOut { device }, Direction::Out) => *device == d.id,
                            _ => false,
                        })
                        .map(|n| n.channels as usize)
                        .sum()
                };
                let groups = |dir: Direction| {
                    let mut budget = used(dir);
                    let g: Vec<PortGroupItem> = d
                        .groups
                        .iter()
                        .filter(|g| g.direction == dir)
                        .map(|g| PortGroupItem {
                            name: g.name.into(),
                            count: g.ports.len() as i32,
                            ports: ModelRc::new(VecModel::from(
                                g.ports
                                    .iter()
                                    .map(|p| {
                                        let used = budget > 0;
                                        budget = budget.saturating_sub(1);
                                        PortItem {
                                            short: short_port(p).into(),
                                            name: p.as_str().into(),
                                            used,
                                        }
                                    })
                                    .collect::<Vec<_>>(),
                            )),
                        })
                        .collect();
                    ModelRc::new(VecModel::from(g))
                };
                DeviceItem {
                    id: d.id.into(),
                    name: d.name.into(),
                    bus: d.bus.into(),
                    rate: format!("{} kHz", d.sample_rate / 1000).into(),
                    clock: d.clock.into(),
                    ins: d.channels(Direction::In) as i32,
                    outs: d.channels(Direction::Out) as i32,
                    inputs: groups(Direction::In),
                    outputs: groups(Direction::Out),
                    expanded: expanded.contains(d.id),
                    selected: i == cursor,
                    clock_master: d.clock.contains("master"),
                    mock: d.mock,
                }
            })
            .collect();
        self.devices.set_vec(devices);
        let c = cursor as i32;
        self.broadcast(|s| s.set_device_cursor(c));
    }

    pub(super) fn rebuild_presets(&self) {
        let p = self.provider.borrow();
        let active = p.active_preset();
        let items: Vec<PresetItem> = p
            .presets()
            .into_iter()
            .map(|x| PresetItem {
                id: x.id.into(),
                name: x.name.into(),
                summary: x.summary.into(),
                active: Some(x.id) == active,
                plugin: x.plugin_mock.unwrap_or("").into(),
                changes: ModelRc::new(VecModel::from(
                    x.changes
                        .iter()
                        .map(|c| SharedString::from(*c))
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        drop(p);
        self.presets.set_vec(items);
        let c = self.preset_cursor.get() as i32;
        let toast: SharedString = self.toast.borrow().as_str().into();
        self.broadcast(|s| {
            s.set_preset_cursor(c);
            s.set_toast(toast.clone());
        });
    }

    pub(super) fn rebuild_selection(&self) {
        let sel = self.selected.borrow().clone();
        let p = self.provider.borrow();
        let view = if sel.starts_with("engine.mic-") {
            inspect::mic(&**p, &sel, &self.mic_info.borrow())
        } else {
            inspect::build(&**p, &sel)
        };
        drop(p);
        let mic_info = self.mic_info.borrow();
        let loop_buses = mic_info["mics"].as_array().into_iter().flatten()
            .find(|mic| mic["channel"] == sel.trim_start_matches("engine."))
            .filter(|mic| mic["input_send"].as_bool() == Some(true))
            .and_then(|mic| mic["loops"].as_array())
            .into_iter().flatten().filter_map(|bus| bus.as_str()).collect::<Vec<_>>();
        let loop_warning = if loop_buses.is_empty() {
            String::new()
        } else {
            let buses = loop_buses.join(" and ");
            format!("Audio loop: this browser also records {buses}. Use a different browser to record, or turn down the send.")
        };
        sync_sections(&self.sections, view.sections);
        let targets: Vec<TargetItem> = self
            .provider
            .borrow()
            .targets(&sel)
            .into_iter()
            .map(|(id, name, current)| TargetItem {
                id: id.into(),
                name: name.into(),
                current,
            })
            .collect();
        if !self.targets.iter().eq(targets.iter().cloned()) {
            self.targets.set_vec(targets);
        }
        let sel_s: SharedString = sel.as_str().into();
        self.broadcast(|s| {
            s.set_selected(sel_s.clone());
            s.set_sel_name(view.name.as_str().into());
            s.set_sel_kind(view.kind.as_str().into());
            s.set_mic_loop_warning(loop_warning.as_str().into());
            s.set_sel_detail(view.detail.as_str().into());
            s.set_sel_hardware(view.hardware);
        });
    }

    pub(super) fn push_engine(&self, s: &AudioState) {
        s.set_live(self.is_live());
        let e = self.provider.borrow().engine();
        s.set_backend(e.backend.as_str().into());
        s.set_rate(format!("{} kHz", e.sample_rate / 1000).into());
        s.set_buffer(format!("{} samples", e.buffer).into());
        s.set_latency(format!("{:.2} ms", e.latency_ms()).into());
        s.set_xruns_known(e.xruns.is_some());
        s.set_xruns(e.xruns.unwrap_or(0).min(i32::MAX as u32) as i32);
        s.set_dsp(e.dsp_load);
    }

    pub fn select(&self, id: &str) {
        if self.selected.borrow().as_str() != id {
            for w in self.windows.borrow().iter().filter_map(|w| w.upgrade()) { w.global::<AudioState>().set_mic_history(ModelRc::default()); }
        }
        *self.selected.borrow_mut() = id.to_string();
        self.rebuild_graph();
        self.rebuild_strips();
        self.rebuild_selection();
    }

    pub(super) fn move_selection(&self, dx: i32, dy: i32) {
        let nodes: Vec<GNode> = self.nodes.iter().collect();
        let cur = nodes
            .iter()
            .find(|n| n.selected)
            .cloned()
            .unwrap_or_else(|| nodes[0].clone());
        let target_col = (cur.col + dx).clamp(0, 2);
        let in_col: Vec<&GNode> = nodes.iter().filter(|n| n.col == target_col).collect();
        let row = if dx != 0 {
            // Keep roughly the same vertical position when changing column.
            let rows_cur = nodes.iter().filter(|n| n.col == cur.col).count().max(1) as f32;
            ((cur.row as f32 + 0.5) / rows_cur * in_col.len() as f32) as i32
        } else {
            cur.row + dy
        };
        let row = row.clamp(0, in_col.len() as i32 - 1);
        if let Some(n) = in_col.iter().find(|n| n.row == row) {
            let id = n.id.to_string();
            self.select(&id);
        }
    }

    /// A deck strip in the Mixer is the DJ Booth's channel fader: one value,
    /// two views. `db` on the Mixer's law maps onto the DJ fader law.
    pub(super) fn set_deck_strip_gain(&self, id: &str, db: f32) -> bool {
        let Some(letter) = id.strip_prefix("engine.deck-") else {
            return false;
        };
        let i = Self::deck_index(letter);
        let mut g = self.deck_gain.get();
        g[i] = ((db.max(-60.0) + 60.0) / 66.0).clamp(0.0, 1.0);
        self.deck_gain.set(g);
        self.apply_dj_gains();
        self.rebuild_strips();
        self.poll_dj();
        true
    }

    pub(super) fn set_gain_pos(&self, id: &str, pos: f32) {
        if self.set_deck_strip_gain(id, pos_to_gain(pos)) {
            return;
        }
        let headroom = self
            .provider
            .borrow()
            .strips()
            .iter()
            .find(|s| s.node == id)
            .is_some_and(|s| s.headroom);
        let db = fader_db_with_headroom(id, self.is_live(), headroom, pos);
        self.provider.borrow_mut().set_gain(id, db);
        self.rebuild_strips();
        self.rebuild_selection();
    }

    pub(super) fn configure_application_strip(&self, id: &str, name: &str, keep: bool) {
        let result = self
            .provider
            .borrow_mut()
            .configure_application_strip(id, name, keep);
        if let Err(error) = result {
            *self.toast.borrow_mut() = format!("Can't update channel: {error}");
            self.push_toast();
        }
        self.rebuild_strips();
    }

    /// A mixer strip by node id, strip name or application name, with its gain.
    pub(super) fn resolve_strip(&self, name: &str) -> Option<(&'static str, f32)> {
        let p = self.provider.borrow();
        let strips = p.strips();
        let want = name.to_lowercase();
        strips
            .iter()
            .find(|s| s.node == name)
            .or_else(|| strips.iter().find(|s| s.name.to_lowercase() == want))
            .or_else(|| {
                p.find_stream(name)
                    .and_then(|id| strips.iter().find(|s| s.node == id))
            })
            .or_else(|| {
                strips
                    .iter()
                    .find(|s| s.name.to_lowercase().contains(&want))
            })
            .map(|s| (s.node, s.gain_db))
    }

    /// Meters show what you hear (OUTPUT) or what the engine processes now.
    pub(super) fn toggle_reference(&self) {
        let next = match self.reference.get() {
            MeterReference::Output => MeterReference::Engine,
            MeterReference::Engine => MeterReference::Output,
        };
        self.reference.set(next);
        self.provider.borrow_mut().set_meter_reference(next);
    }

    pub(super) fn nudge_gain(&self, id: &str, db: f32) {
        if let Some(l) = id.strip_prefix("engine.deck-") {
            let now =
                patch_engine::session::fader_db(self.deck_gain.get()[if l == "b" { 1 } else { 0 }]);
            self.set_deck_strip_gain(id, now + db);
            return;
        }
        let cur = self
            .provider
            .borrow()
            .strips()
            .iter()
            .find(|s| s.node == id)
            .map(|s| s.gain_db);
        if let Some(cur) = cur {
            self.provider.borrow_mut().set_gain(id, cur + db);
            self.rebuild_strips();
            self.rebuild_selection();
        }
    }

    pub(super) fn toggle_mute(&self, id: &str) {
        let cur = self
            .provider
            .borrow()
            .strips()
            .iter()
            .find(|s| s.node == id)
            .map(|s| s.mute);
        if let Some(m) = cur {
            self.provider.borrow_mut().set_mute(id, !m);
            self.rebuild_graph();
            self.rebuild_strips();
            self.rebuild_selection();
        }
    }

    pub(super) fn toggle_solo(&self, id: &str) {
        let cur = self
            .provider
            .borrow()
            .strips()
            .iter()
            .find(|s| s.node == id)
            .map(|s| s.solo);
        if let Some(v) = cur {
            if let Err(e) = self.provider.borrow_mut().set_solo(id, !v) {
                *self.toast.borrow_mut() = e;
                self.push_toast();
            }
            self.rebuild_strips();
        }
    }

    /// Place the console's bands for the surface's own size, and say what
    /// the width can pin. Called by the surface whenever it is resized.
    pub(super) fn fit_console(&self, inner: f32, width: f32, patch_style: bool) {
        self.console_patch_style.set(patch_style);
        self.console_inner.set(inner);
        self.console_w.set(width);
        self.push_fit();
    }

    /// The fit and the desk's placement, from the last size and the models:
    /// the scrolling desk holds every strip the console shows except what the
    /// pinned group carries. Runs after the strips or the returns change too,
    /// and broadcasts only when something moved.
    pub(super) fn push_fit(&self) {
        let geometry = if self.console_patch_style.get() {
            crate::console_fit::PATCH_STYLE
        } else {
            crate::console_fit::CLASSIC
        };
        let fit = crate::console_fit::console_fit_with(
            &geometry,
            self.console_inner.get(),
            self.console_w.get(),
            self.buses.row_count(),
            self.main_dests.row_count(),
        );
        // Program mix (a live engine's internal mix gain) is not a channel on
        // this desk: the console shows the Main output instead.
        let hidden = |s: &StripItem| s.id == "master" && !s.master;
        let non_main = self
            .strips
            .iter()
            .filter(|s| !s.master && !hidden(s))
            .count();
        let mains = self.strips.iter().filter(|s| s.master && !hidden(s)).count();
        let buses = self.buses.row_count();
        let cols = non_main
            + if fit.pins.pin_main { 0 } else { mains }
            + if fit.pins.pin_buses { 0 } else { buses };
        let scroll_w = cols as f32 * crate::console_fit::COL + 80.0;
        if self.fit_cache.get() == Some((fit, scroll_w)) {
            return;
        }
        self.fit_cache.set(Some((fit, scroll_w)));
        let pinned = if fit.pins.pin_main {
            crate::console_fit::pinned_w(buses, fit.pins.pin_buses)
        } else {
            0.0
        };
        self.broadcast(|s| {
            s.set_console_header(fit.header);
            s.set_console_knob_scale(fit.knob_scale);
            s.set_console_knob_view(fit.knob_viewport);
            s.set_console_knob_body(fit.knob_content);
            s.set_console_fader(fit.fader);
            s.set_console_foot(fit.foot);
            s.set_console_scroll_w(scroll_w);
            s.set_console_pinned(pinned);
            s.set_console_pin_buses(fit.pins.pin_buses);
            s.set_console_pin_main(fit.pins.pin_main);
        });
    }

    pub(super) fn toggle_device(&self, id: &str) {
        let ids: Vec<&'static str> = self
            .provider
            .borrow()
            .devices()
            .iter()
            .map(|d| d.id)
            .collect();
        if let Some(i) = ids.iter().position(|d| *d == id) {
            self.device_cursor.set(i);
            let mut ex = self.expanded.borrow_mut();
            if !ex.remove(ids[i]) {
                ex.insert(ids[i]);
            }
        }
        self.rebuild_devices();
    }

    pub(super) fn move_device(&self, d: i32) {
        let n = self.devices.row_count() as i32;
        self.device_cursor
            .set((self.device_cursor.get() as i32 + d).clamp(0, n - 1) as usize);
        self.rebuild_devices();
    }

    pub(super) fn move_preset(&self, d: i32) {
        let n = self.presets.row_count() as i32;
        self.preset_cursor
            .set((self.preset_cursor.get() as i32 + d).clamp(0, n - 1) as usize);
        self.rebuild_presets();
    }

    pub(super) fn apply_preset(&self, id: &str) {
        let preset = self
            .provider
            .borrow()
            .presets()
            .into_iter()
            .find(|p| p.id == id);
        let Some(p) = preset else { return };
        *self.toast.borrow_mut() = match p.plugin_mock {
            Some(plugin) => format!("{} needs the {plugin} plugin · not implemented", p.name),
            None => {
                self.provider.borrow_mut().apply_preset(id);
                format!(
                    "Applied {} · simulated, nothing changed on this system",
                    p.name
                )
            }
        };
        self.rebuild_presets();
    }
}

/// Knob ranges: GAIN ±24 dB; EQ −24 dB (left) … +6 dB (right), the
/// engine's band range; PAN as is.
fn knob_value(control: &str, pos: f32) -> f32 {
    let pos = pos.clamp(-1.0, 1.0);
    match control {
        "gain" => pos * 24.0,
        "high" | "mid" | "low" => {
            if pos < 0.0 {
                pos * 24.0
            } else {
                pos * 6.0
            }
        }
        _ => pos,
    }
}

fn knob_pos(control: &str, value: f32) -> f32 {
    match control {
        "gain" => value / 24.0,
        "high" | "mid" | "low" => {
            if value < 0.0 {
                value / 24.0
            } else {
                value / 6.0
            }
        }
        _ => value,
    }
    .clamp(-1.0, 1.0)
}

/// A strip's knob fields from its controls; true when any changed.
fn knob_fields(row: &mut StripItem, c: Option<patch_model::audio::StripControls>) -> bool {
    let sends_before = (row.sends, row.tone, [row.s1, row.s2, row.s3, row.s4], [row.p1, row.p2, row.p3, row.p4], row.send_note.clone());
    let sends = c.and_then(|c| c.sends);
    row.tone = c.is_some_and(|c| c.tone);
    // Replace unavailable sends with the reason.
    let off = c.and_then(|c| c.sends_off);
    row.sends = sends.is_some() && off.is_none();
    row.send_note = match off {
        Some(patch_model::audio::SendsOff::CaptureOff) => "Mic sends are off".into(),
        Some(patch_model::audio::SendsOff::Remote) => "Sends off to prevent echo in the call".into(),
        Some(patch_model::audio::SendsOff::OnLane { lane, far_end }) => {
            format!("Sound now on {} {lane}", if far_end { "Remote" } else { "Voice" }).into()
        }
        None => "".into(),
    };
    row.send_hint = match off {
        Some(patch_model::audio::SendsOff::CaptureOff) => "Sends off".into(),
        Some(patch_model::audio::SendsOff::Remote) => "Avoids call echo".into(),
        Some(patch_model::audio::SendsOff::OnLane { lane, far_end }) => {
            format!("On {} {lane}", if far_end { "Remote" } else { "Voice" }).into()
        }
        None => "".into(),
    };
    let s = sends.unwrap_or_default();
    [row.s1, row.s2, row.s3, row.s4] = std::array::from_fn(|i| send_pos(s[i].db));
    [row.p1, row.p2, row.p3, row.p4] = std::array::from_fn(|i| s[i].pre);
    let sends_changed = sends_before != (row.sends, row.tone, [row.s1, row.s2, row.s3, row.s4], [row.p1, row.p2, row.p3, row.p4], row.send_note.clone());
    let before = (row.knobs, row.gain_live, row.k_gain, row.k_high, row.k_mid, row.k_low, row.k_pan);
    let c = c.map(|c| {
        (
            c.gain_db.is_some(),
            knob_pos("gain", c.gain_db.unwrap_or(0.0)),
            knob_pos("high", c.high_db),
            knob_pos("mid", c.mid_db),
            knob_pos("low", c.low_db),
            knob_pos("pan", c.pan),
        )
    });
    row.knobs = c.is_some();
    let (live, g, h, m, l, p) = c.unwrap_or((false, 0.0, 0.0, 0.0, 0.0, 0.0));
    row.gain_live = live;
    (row.k_gain, row.k_high, row.k_mid, row.k_low, row.k_pan) = (g, h, m, l, p);
    let after = (row.knobs, row.gain_live, row.k_gain, row.k_high, row.k_mid, row.k_low, row.k_pan);
    before != after || sends_changed
}

/// A send knob (−1 off … 1) on the faders' law: −60 … +6 dB, 0 dB at
/// 0.818 (the fader's 0.909 of its travel).
fn send_db(pos: f32) -> f32 {
    let level = ((pos + 1.0) / 2.0).clamp(0.0, 1.0);
    if level <= 0.0 { -120.0 } else { pos_to_gain(level) }
}

fn send_pos(db: Option<f32>) -> f32 {
    match db {
        Some(db) if db > -60.0 => gain_to_pos(db) * 2.0 - 1.0,
        _ => -1.0,
    }
}

/// The Inspector's sections, rows changed in place. A new model (or a new
/// rows model in a section) makes Slint rebuild every row's controls: a
/// slider under the pointer would be destroyed mid-drag by each poll and by
/// its own `changed`, which made the Mic Unit's controls near-unresponsive.
fn sync_sections(model: &VecModel<InspSection>, sections: Vec<InspSection>) {
    let same_shape = model.row_count() == sections.len()
        && model.iter().zip(&sections).all(|(old, new)| {
            old.title == new.title
                && old.rows.row_count() == new.rows.row_count()
                && old.rows.as_any().is::<VecModel<InspRow>>()
        });
    if !same_shape {
        model.set_vec(sections);
        return;
    }
    for (old, new) in model.iter().zip(sections) {
        let Some(rows) = old.rows.as_any().downcast_ref::<VecModel<InspRow>>() else {
            continue;
        };
        for (i, row) in new.rows.iter().enumerate() {
            if rows.row_data(i).as_ref() != Some(&row) {
                rows.set_row_data(i, row);
            }
        }
    }
}

#[cfg(test)]
mod knob_tests {
    use super::*;
    #[test]
    fn knob_positions_and_db_round_trip() {
        for (control, db) in [("gain", -24.0), ("gain", 6.0), ("high", -24.0), ("mid", -6.0), ("low", 6.0), ("low", 3.0), ("pan", -0.5)] {
            let back = knob_value(control, knob_pos(control, db));
            assert!((back - db).abs() < 1e-4, "{control} {db} → {back}");
        }
        assert_eq!(knob_value("high", -1.0), -24.0);
        assert_eq!(knob_value("high", 1.0), 6.0);
        assert_eq!(knob_pos("gain", 0.0), 0.0);
        // Sends: off at the left, unity where the fader has it.
        assert_eq!(send_pos(None), -1.0);
        assert!(send_db(-1.0) <= -60.0, "off");
        assert!((send_db(send_pos(Some(0.0)))).abs() < 1e-4);
        assert!((send_db(send_pos(Some(-12.0))) + 12.0).abs() < 1e-3);
    }
}
