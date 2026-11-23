//! Mixer presentation follows the explicit bar selection, never the internal
//! default sink. All meter vectors use the same ID projection as controls.
use crate::{intern, node_id_for_stream, pactl::StreamInfo, stream_name};
#[cfg(test)]
use crate::pactl::SinkInfo;
use patch_model::audio::{MeterFrame, SILENCE_DB, Strip};
use std::collections::HashMap;

pub fn professional_selected() -> bool {
    std::fs::read_to_string(patch_core::ipc::runtime_dir().join("output-selection"))
        .is_ok_and(|s| s.trim() == crate::BUS)
}

/// The engine channel an application's audio runs through: Patch being its
/// output, an app with a channel of its own plays into `patch.app-N`. That
/// channel is the app's strip on the desk — its fader, its mute and its
/// meters; an app playing straight to a device has none (`None`).
///
/// Production strips do not see `patch.app-N` (engine plumbing is filtered
/// out of the sink list), so the live desk uses the assignment map. These
/// helpers stay for the sink-rule tests.
#[cfg(test)]
pub fn app_channel(sinks: &[SinkInfo], s: &StreamInfo) -> Option<&'static str> {
    let name = sinks.iter().find(|k| k.index == s.sink)?.name.as_str();
    app_channel_of_sink(name)
}

/// [`app_channel`]'s rule: `patch.app-1` is application channel 1.
#[cfg(test)]
fn app_channel_of_sink(sink_name: &str) -> Option<&'static str> {
    let ch = sink_name.strip_prefix("patch.")?;
    patch_core::eq::APP_CHANNELS.iter().find(|c| **c == ch).copied()
}

pub fn application_base(s: &StreamInfo) -> &'static str {
    // Names distinguish applications sharing a generic executable (Electron,
    // Java, browsers). The explicit application ID wins when supplied.
    let identity = if !s.app_id.is_empty() {
        format!("id:{}", s.app_id)
    } else if !s.app_name.is_empty() {
        format!("name:{}:{}", s.binary, s.app_name)
    } else {
        format!("node:{}:{}", s.binary, s.node_name)
    };
    intern(&format!("application:{identity}"))
}

/// A playback stream owns one strip, even when its client opens several streams.
pub fn application_node(s: &StreamInfo) -> &'static str {
    let base = application_base(s);
    match s.client_id {
        Some(client) => intern(&format!("{base}|client:{client}|pid:{}|stream:{}", s.process_id, s.index)),
        // Missing client metadata must not merge unrelated playback streams.
        None => intern(&format!("{base}|stream:{}", s.index)),
    }
}

pub fn strip_name(s: &StreamInfo, streams: &[StreamInfo]) -> String {
    let base = application_base(s);
    let mut clients = Vec::new();
    for stream in streams
        .iter()
        .filter(|other| application_base(other) == base)
    {
        let node = application_node(stream);
        if !clients.contains(&node) {
            clients.push(node);
        }
    }
    clients.sort_unstable();
    let name = stream_name(s);
    if clients.len() > 1 {
        let number = clients.iter().position(|node| *node == application_node(s)).unwrap_or(0) + 1;
        format!("{name} {number}")
    } else {
        name
    }
}

#[cfg(test)]
pub fn exclusive_app_channel(sinks: &[SinkInfo], streams: &[StreamInfo], s: &StreamInfo) -> Option<&'static str> {
    let node = application_node(s);
    let channel = streams
        .iter()
        .filter(|other| application_node(other) == node)
        .find_map(|other| app_channel(sinks, other))?;
    if streams
        .iter()
        .filter(|other| application_node(other) == node)
        .any(|other| app_channel(sinks, other) != Some(channel))
    {
        return None;
    }
    (!streams.iter().any(|other| {
        app_channel(sinks, other) == Some(channel)
            && application_node(other) != node
    }))
    .then_some(channel)
}

pub fn is_application_node(node: &str) -> bool {
    node.starts_with("application:")
}

pub fn matches_node(s: &StreamInfo, node: &str) -> bool {
    node_id_for_stream(s) == node || application_node(s) == node
}

pub fn project_strips(raw: Vec<Strip>, streams: &[StreamInfo], professional: bool) -> Vec<Strip> {
    let mut output: Vec<Strip> = Vec::new();
    for mut strip in raw {
        if !professional && strip.node == "master" {
            continue;
        }
        if let Some(stream) = streams.iter().find(|s| node_id_for_stream(s) == strip.node) {
            strip.node = application_node(stream);
            strip.name = intern(&strip_name(stream, streams));
            if let Some(existing) = output.iter_mut().find(|s| s.node == strip.node) {
                // A grouped control displays the loudest member's setting.
                // Moving it explicitly sets all members; external member
                // changes are reflected without modifying its neighbours.
                existing.gain_db = existing.gain_db.max(strip.gain_db);
                existing.headroom |= strip.headroom;
                existing.mute &= strip.mute;
                existing.solo |= strip.solo;
                continue;
            }
        }
        output.push(strip);
    }
    output
}

pub fn project_meters(
    mut frame: MeterFrame,
    raw: &[Strip],
    shown: &[Strip],
    streams: &[StreamInfo],
) -> MeterFrame {
    let mut members: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, strip) in raw.iter().enumerate() {
        let id = streams
            .iter()
            .find(|s| node_id_for_stream(s) == strip.node)
            .map(application_node)
            .unwrap_or(strip.node);
        members.entry(id).or_default().push(i);
    }
    // An application group reports its loudest member, not an invented
    // coherent sum of unrelated stream peaks. Main output meters the mix.
    let project = |values: &[f32]| {
        shown
            .iter()
            .map(|strip| {
                members
                    .get(strip.node)
                    .into_iter()
                    .flatten()
                    .filter_map(|i| values.get(*i))
                    .copied()
                    .fold(SILENCE_DB, f32::max)
            })
            .collect()
    };
    frame.strips_db = project(&frame.strips_db);
    frame.strips_vu = frame.strips_vu.as_ref().map(|v| project(v));
    frame.strips_lr = frame.strips_lr.as_ref().map(|values| {
        shown
            .iter()
            .map(|strip| {
                members
                    .get(strip.node)
                    .into_iter()
                    .flatten()
                    .filter_map(|i| values.get(*i))
                    .fold([SILENCE_DB; 2], |m, v| [m[0].max(v[0]), m[1].max(v[1])])
            })
            .collect()
    });
    frame
}

pub fn port_node(device: &str, ports: &[String]) -> &'static str {
    intern(&format!(
        "physical-ports:{}",
        serde_json::to_string(&(device, ports)).unwrap()
    ))
}
pub fn parse_port_node(id: &str) -> Option<(String, Vec<String>)> {
    serde_json::from_str(id.strip_prefix("physical-ports:")?).ok()
}
pub fn port_order(port: &str) -> (u32, &str) {
    let position = port.rsplit('_').next().unwrap_or(port);
    let order = position
        .strip_prefix("AUX")
        .and_then(|n| n.parse().ok())
        .or_else(|| position.parse().ok())
        .unwrap_or(match position {
            "FL" => 0,
            "FR" => 1,
            "FC" => 2,
            "LFE" => 3,
            "SL" => 4,
            "SR" => 5,
            "RL" => 6,
            "RR" => 7,
            _ => 100,
        });
    (order, port)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stream(index: u32, name: &str, binary: &str) -> StreamInfo {
        StreamInfo {
            index,
            client_id: Some(index),
            process_id: "42".into(),
            sink: 1,
            app_name: name.into(),
            app_id: String::new(),
            node_name: format!("node-{index}"),
            binary: binary.into(),
            channels: 2,
            volume_db: 0.0,
            mute: false,
            system_sound: false,
        }
    }
    fn strip(id: &'static str) -> Strip {
        Strip {
            node: id,
            name: id,
            gain_db: 0.0,
            headroom: false,
            mute: false,
            solo: false,
            master: id == "output",
        }
    }
    #[test]
    fn all_streams_take_a_name_number_even_when_the_client_and_metadata_match() {
        let first = stream(1, "Chromium", "chromium");
        let mut second = stream(2, "Chromium", "chromium");
        second.client_id = first.client_id;
        let streams = [first.clone(), second.clone()];
        assert_eq!(strip_name(&first, &streams), "Chromium 1");
        assert_eq!(strip_name(&second, &streams), "Chromium 2");
    }

    #[test]
    fn an_app_plays_through_the_channel_of_its_sink() {
        let sink = |index: u32, name: &str| SinkInfo {
            index,
            name: name.into(),
            ..Default::default()
        };
        let sinks = vec![sink(7, "patch"), sink(9, "patch.app-1"), sink(11, "patch.app-5"), sink(13, "bluez_output.X")];
        // An app with its own channel: its strip is that channel.
        let mut s = stream(3, "Chromium", "chromium");
        s.sink = 9;
        assert_eq!(app_channel(&sinks, &s), Some("app-1"));
        // On the bus (no channel of its own) or on a device: none.
        s.sink = 7;
        assert_eq!(app_channel(&sinks, &s), None);
        s.sink = 13;
        assert_eq!(app_channel(&sinks, &s), None);
        // A sink that is not an engine channel (past the four).
        s.sink = 11;
        assert_eq!(app_channel(&sinks, &s), None);
        // An unknown sink index: none, never a panic.
        s.sink = 99;
        assert_eq!(app_channel(&sinks, &s), None);
    }

    #[test]
    fn desktop_has_only_sources_and_main_with_meter_identity_preserved() {
        let streams = vec![
            stream(1, "Browser", "browser"),
            stream(2, "X", "x"),
            stream(3, "Spotify", "spotify"),
        ];
        let raw = vec![
            strip("stream-1"),
            strip("stream-2"),
            strip("stream-3"),
            strip(crate::SYSTEM_SOUNDS),
            strip("master"),
            strip("output"),
            strip("engine.deck-a"),
        ];
        let shown = project_strips(raw.clone(), &streams, false);
        assert_eq!(shown.len(), 6); // Three applications, System sounds, Main and private DJ meter.
        assert_eq!(shown[3].node, crate::SYSTEM_SOUNDS);
        assert_eq!(shown[4].node, "output");
        let frame = project_meters(
            MeterFrame {
                strips_db: vec![-10., -20., -30., -40., -50., -60., -70.],
                strips_vu: Some(vec![-11., -21., -31., -41., -51., -61., -71.]),
                strips_lr: Some(vec![
                    [-10., -12.],
                    [-20., -22.],
                    [-30., -32.],
                    [-40., -42.],
                    [-50., -52.],
                    [-60., -62.],
                    [-70., -72.],
                ]),
                ..Default::default()
            },
            &raw,
            &shown,
            &streams,
        );
        assert_eq!(frame.strips_db, vec![-10., -20., -30., -40., -60., -70.]);
        assert_eq!(frame.strips_vu.unwrap()[3], -41.);
        assert_eq!(frame.strips_lr.unwrap()[3], [-40., -42.]);
    }
    #[test]
    fn streams_of_one_client_and_generic_executables_stay_independent() {
        let a = stream(1, "Spotify", "electron");
        let mut b = stream(42, "Spotify", "electron");
        b.client_id = a.client_id;
        let c = stream(2, "Chat", "electron");
        assert_ne!(application_node(&a), application_node(&b));
        assert_ne!(application_node(&a), application_node(&c));
        let streams = vec![a, b, c];
        let raw = vec![
            strip("stream-1"),
            strip("stream-42"),
            strip("stream-2"),
            strip("output"),
        ];
        let shown = project_strips(raw.clone(), &streams, false);
        assert_eq!(shown.len(), 4);
        let frame = project_meters(
            MeterFrame {
                strips_db: vec![-10., -3., -30., -2.],
                ..Default::default()
            },
            &raw,
            &shown,
            &streams,
        );
        assert_eq!(frame.strips_db, vec![-10., -3., -30., -2.]);
        assert!(!matches_node(&streams[1], shown[0].node));
        assert!(matches_node(&streams[1], shown[1].node));
    }
    #[test]
    fn solo_is_independent_for_streams_of_one_client() {
        let first = stream(1, "Spotify", "spotify");
        let mut second = stream(2, "Spotify", "spotify");
        second.client_id = first.client_id;
        let streams = vec![first, second];
        let mut loud = strip("stream-1");
        loud.solo = true;
        let shown = project_strips(vec![loud, strip("stream-2"), strip("output")], &streams, true);
        let app = shown.iter().find(|s| s.node.starts_with("application:")).unwrap();
        assert!(app.solo);
        assert!(!shown[1].solo);
    }
    #[test]
    fn professional_keeps_configured_program_controls() {
        let raw = vec![
            strip(crate::SYSTEM_SOUNDS),
            strip("master"),
            strip("output"),
        ];
        assert_eq!(project_strips(raw.clone(), &[], true).len(), 3);
        assert_eq!(project_strips(raw, &[], false).len(), 2);
    }

    #[test]
    fn browser_streams_sharing_a_client_have_independent_controls_and_meters() {
        let a = stream(1, "Chromium", "chromium");
        let mut b = stream(2, "Chromium", "chromium");
        b.node_name = a.node_name.clone();
        b.client_id = a.client_id;
        let streams = vec![a, b];
        let mut first = strip("stream-1");
        first.gain_db = -6.0;
        first.mute = true;
        let raw = vec![first, strip("stream-2"), strip("output")];
        let shown = project_strips(raw.clone(), &streams, true);
        assert_eq!(shown.len(), 3);
        assert_ne!(shown[0].node, shown[1].node);
        assert_ne!(shown[0].name, shown[1].name);
        assert_eq!(shown[0].gain_db, -6.0);
        assert_eq!(shown[1].gain_db, 0.0);
        assert!(shown[0].mute);
        assert!(!shown[1].mute);
        assert_eq!(strip_name(&streams[0], &streams), strip_name(&streams[0], &[streams[1].clone(), streams[0].clone()]));
        assert!(matches_node(&streams[0], shown[0].node));
        assert!(!matches_node(&streams[1], shown[0].node));
        let meters = project_meters(MeterFrame {
            strips_db: vec![-4.0, -24.0, -3.0], ..Default::default()
        }, &raw, &shown, &streams);
        assert_eq!(meters.strips_db, [-4.0, -24.0, -3.0]);
    }

    #[test]
    fn a_recreated_stream_gets_its_own_identity() {
        let a = stream(1, "Chromium", "chromium");
        let mut recreated = a.clone();
        recreated.index = 900;
        assert_ne!(application_node(&a), application_node(&recreated));
        recreated.process_id = "99".into();
        assert_ne!(application_node(&a), application_node(&recreated));
        recreated.client_id = None;
        let mut another = recreated.clone();
        another.index = 901;
        assert_ne!(application_node(&recreated), application_node(&another));
    }

    #[test]
    fn a_shared_engine_channel_is_not_an_independent_browser_fader() {
        let sinks = [SinkInfo { index: 1, name: "patch.app-1".into(), ..Default::default() }];
        let a = stream(1, "Chromium", "chromium");
        let mut b = stream(2, "Chromium", "chromium");
        assert_eq!(exclusive_app_channel(&sinks, &[a.clone(), b.clone()], &a), None);
        b.client_id = a.client_id;
        assert_eq!(exclusive_app_channel(&sinks, &[a.clone(), b.clone()], &a), None);
        b.sink = 99;
        assert_eq!(exclusive_app_channel(&sinks, &[a.clone(), b.clone()], &a), Some("app-1"));
        b.client_id = Some(2);
        b.sink = 99;
        assert_eq!(exclusive_app_channel(&sinks, &[a.clone(), b], &a), Some("app-1"));
    }

}
