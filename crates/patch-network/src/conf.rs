//! The PipeWire configuration each link runs (`pipewire -c <file>` as a
//! client of the user's PipeWire): PipeWire's own RTP, SAP and VBAN modules
//! do the network work, with their clock recovery and jitter buffers;
//! Patch decides what goes where.
//!
//! Every stream is a plain PipeWire stream on Patch's nodes:
//! * a send captures `patch.program` / `patch.aux1` / `patch.aux2`;
//! * a receive plays into `patch` like an application, so it gets its own
//!   channel in the Mixer, with fader, EQ and sends.
//!
//! A send has `node.dont-fallback`: while Patch's node is missing (an engine
//! restart) it is linked to nothing, never to the default source, which
//! would be a microphone. A receive has not: WirePlumber refuses a stream
//! that names `patch` with `dont-fallback` ("defined target not found",
//! 2026-11-22), and without it Patch's routing puts the stream in a stream
//! channel of its own, as it does for any application.

use crate::config::{Proto, Receive, Send, vban_name};
use crate::net::Host;

/// The format a Patch RTP send uses: 24-bit big-endian PCM, 48 kHz, stereo
/// (AES67's L24).
pub const SEND_RATE: u32 = 48_000;

/// A string in SPA JSON (a JSON string is one).
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The node a link names itself by. A send is one of Patch's own
/// (`patch.net.send-0`); a receive must not look like one: the engine gives
/// nodes named `patch.*` no stream lane, and a receive is an application
/// playing into Patch (`network.rx-…`, shown as "Patch Network · <name>").
pub fn node_name(id: &str) -> String {
    if id.starts_with("send-") { format!("patch.net.{id}") } else { format!("network.{id}") }
}

fn base(body: &str) -> String {
    format!(
        "# Written by Patch (Network). Changes are overwritten.\n\
context.properties = {{ log.level = 2 }}\n\
context.spa-libs = {{ support.* = support/libspa-support }}\n\
context.modules = [\n\
  {{ name = libpipewire-module-rt args = {{ nice.level = -11 }} flags = [ ifexists nofail ] }}\n\
  {{ name = libpipewire-module-protocol-native }}\n\
  {{ name = libpipewire-module-client-node }}\n\
  {{ name = libpipewire-module-adapter }}\n\
{body}]\n"
    )
}

fn positions(channels: u32) -> String {
    match channels {
        1 => "[ MONO ]".into(),
        2 => "[ FL FR ]".into(),
        n => format!(
            "[ {} ]",
            (0..n).map(|i| format!("AUX{i}")).collect::<Vec<_>>().join(" ")
        ),
    }
}

/// PipeWire's name for an RTP PCM encoding (all big-endian on the wire).
pub fn pcm_format(encoding: &str) -> Option<&'static str> {
    match encoding {
        "L16" => Some("S16BE"),
        "L24" => Some("S24BE"),
        "L32" => Some("S32BE"),
        _ => None,
    }
}

/// A send: Patch's feed out to the network.
pub fn send(s: &Send, host: Option<&Host>) -> String {
    let node = node_name(&s.id);
    let (ip, port) = s.destination();
    let multicast = ip.is_multicast();
    let iface = host
        .filter(|_| multicast)
        .map(|h| format!("local.ifname = {} ", q(&h.iface)))
        .unwrap_or_default();
    let common = format!(
        "node.name = {node} node.description = {desc} media.name = {name} \
application.name = \"Patch Network\" media.class = \"Stream/Input/Audio\" \
target.object = {feed} node.dont-fallback = true node.network = true",
        node = q(&node),
        desc = q(&format!("Network · {}", s.name)),
        name = q(&s.name),
        feed = q(s.feed.node()),
    );
    let body = match s.proto {
        Proto::Rtp => {
            // SAP leaves by the interface the network is on: PipeWire takes
            // the address it announces from it. A unicast send says whom it
            // is for, so other machines do not offer it.
            let sap_iface = host
                .map(|h| format!("local.ifname = {} ", q(&h.iface)))
                .unwrap_or_default();
            let to = if multicast {
                String::new()
            } else {
                format!("sap.end-extra = [ {} ] ", q(&format!("a=x-patch-to:{ip}")))
            };
            format!(
                "  {{ name = libpipewire-module-rtp-sap\n    args = {{ {sap_iface}sap.ip = \"224.0.0.56\" net.ttl = 1 net.loop = false {to}\n      stream.rules = [ {{ matches = [ {{ node.name = {node} sess.sap.announce = true }} ] actions = {{ announce-stream = {{ }} }} }} ] }} }}\n\
  {{ name = libpipewire-module-rtp-sink\n    args = {{ {iface}destination.ip = {ip} destination.port = {port} net.ttl = 1 net.loop = false\n      sess.name = {name} sess.media = \"audio\"\n      audio.format = \"S24BE\" audio.rate = {SEND_RATE} audio.channels = 2 audio.position = [ FL FR ]\n      stream.props = {{ {common} sess.sap.announce = true }} }} }}\n",
                node = q(&node),
                ip = q(&ip.to_string()),
                name = q(&s.name),
            )
        }
        Proto::Vban => format!(
            "  {{ name = libpipewire-module-vban-send\n    args = {{ destination.ip = {ip} destination.port = {port} net.ttl = 1\n      sess.name = {name} sess.media = \"audio\"\n      audio.format = \"S24LE\" audio.rate = {SEND_RATE} audio.channels = 2 audio.position = [ FL FR ]\n      stream.props = {{ {common} }} }} }}\n",
            ip = q(&ip.to_string()),
            name = q(&vban_name(&s.name)),
        ),
    };
    base(&body)
}

fn playback_props(node: &str, name: &str) -> String {
    format!(
        "node.name = {node} node.description = {desc} media.name = {name} \
application.name = \"Patch Network\" media.class = \"Stream/Output/Audio\" \
target.object = \"patch\" node.network = true",
        node = q(node),
        desc = q(&format!("Network · {name}")),
        name = q(name),
    )
}

/// A receive: an announced stream into Patch.
pub fn receive(r: &Receive, latency_ms: u32, host: Option<&Host>) -> Result<String, String> {
    let node = node_name(&r.id);
    let multicast = r.multicast();
    // A unicast stream arrives at this machine on its port from the
    // sender; PipeWire then takes any source address.
    let source = if multicast { r.address.clone() } else { "0.0.0.0".into() };
    let iface = host
        .filter(|_| multicast)
        .map(|h| format!("local.ifname = {} ", q(&h.iface)))
        .unwrap_or_default();
    let format = if r.encoding == "opus" {
        "sess.media = \"opus\"".to_string()
    } else {
        let f = pcm_format(&r.encoding)
            .ok_or_else(|| format!("Patch can't play {} streams.", r.encoding))?;
        format!("sess.media = \"audio\" audio.format = \"{f}\"")
    };
    let body = format!(
        "  {{ name = libpipewire-module-rtp-source\n    args = {{ {iface}source.ip = {src} source.port = {port}\n      sess.latency.msec = {latency_ms} sess.ignore-ssrc = true {format}\n      audio.rate = {rate} audio.channels = {ch} audio.position = {pos}\n      stream.props = {{ {props} }} }} }}\n",
        src = q(&source),
        port = r.port,
        rate = r.rate,
        ch = r.channels,
        pos = positions(r.channels),
        props = playback_props(&node, &r.name),
    );
    Ok(base(&body))
}

/// The VBAN listener: every VBAN stream sent to this machine.
pub fn vban_listen(latency_ms: u32) -> String {
    let body = format!(
        "  {{ name = libpipewire-module-vban-recv\n    args = {{ source.ip = \"0.0.0.0\" source.port = {port} sess.latency.msec = {latency_ms}\n      stream.props = {{ {props} }} }} }}\n",
        port = crate::config::VBAN_PORT,
        props = playback_props(&node_name("vban"), "VBAN"),
    );
    base(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Feed};

    fn host() -> Host {
        Host { iface: "wlp130s0f0".into(), ip: "192.168.68.63".parse().unwrap(), prefix: 22 }
    }

    fn balanced(conf: &str) -> bool {
        let mut depth = 0i32;
        let mut quoted = false;
        let mut escaped = false;
        for c in conf.chars() {
            if quoted {
                match (escaped, c) {
                    (true, _) => escaped = false,
                    (false, '\\') => escaped = true,
                    (false, '"') => quoted = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => quoted = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return false;
            }
        }
        depth == 0 && !quoted
    }

    #[test]
    fn a_send_to_everyone_announces_itself_and_never_falls_back() {
        let mut c = Config::default();
        let s = c.add_send(Feed::Program, Proto::Rtp, "", "Living \"room\"").unwrap();
        let conf = send(&s, Some(&host()));
        assert!(balanced(&conf), "{conf}");
        assert!(conf.contains("libpipewire-module-rtp-sap"));
        assert!(conf.contains("destination.ip = \"239.69.83.80\" destination.port = 5004"));
        assert!(conf.contains("target.object = \"patch.program\""));
        assert!(conf.contains("node.dont-fallback = true"));
        assert!(conf.contains("audio.format = \"S24BE\""));
        assert!(conf.contains("local.ifname = \"wlp130s0f0\""));
        // Only this link's stream is announced (several sends run at once).
        assert!(conf.contains("matches = [ { node.name = \"patch.net.send-0\" sess.sap.announce = true } ]"));
        assert!(!conf.contains("create-stream"));
        assert!(conf.contains("\"Living \\\"room\\\"\""));
        assert!(!conf.contains("x-patch-to"));
    }

    #[test]
    fn a_send_to_one_machine_says_whom_it_is_for() {
        let mut c = Config::default();
        let s = c.add_send(Feed::Aux1, Proto::Rtp, "192.168.68.20", "").unwrap();
        let conf = send(&s, Some(&host()));
        assert!(balanced(&conf));
        assert!(conf.contains("a=x-patch-to:192.168.68.20"));
        assert!(conf.contains("target.object = \"patch.aux1\""));
        // Unicast follows the routing table, not one interface.
        let sink = &conf[conf.find("rtp-sink").unwrap()..];
        assert!(!sink.contains("local.ifname"));
    }

    #[test]
    fn vban_sends_24_bit_little_endian_with_a_short_name() {
        let mut c = Config::default();
        let s = c.add_send(Feed::Aux2, Proto::Vban, "10.0.0.9", "Patch AUX 2 for the stage").unwrap();
        let conf = send(&s, None);
        assert!(balanced(&conf));
        assert!(conf.contains("libpipewire-module-vban-send"));
        assert!(conf.contains("destination.port = 6980"));
        assert!(conf.contains("audio.format = \"S24LE\""));
        assert!(conf.contains("sess.name = \"Patch AUX 2 for\""));
    }

    #[test]
    fn a_receive_plays_into_patch_like_an_application() {
        let r = Receive {
            id: "rx-1".into(),
            name: "Studio".into(),
            origin: "192.168.68.20".into(),
            address: "239.69.83.80".into(),
            port: 5004,
            encoding: "L24".into(),
            rate: 48_000,
            channels: 2,
            on: true,
        };
        let conf = receive(&r, 100, Some(&host())).unwrap();
        assert!(balanced(&conf));
        assert!(conf.contains("source.ip = \"239.69.83.80\" source.port = 5004"));
        assert!(conf.contains("media.class = \"Stream/Output/Audio\" target.object = \"patch\""));
        assert!(conf.contains("sess.latency.msec = 100 sess.ignore-ssrc = true"));
        // WirePlumber refuses `patch` as a target with dont-fallback.
        assert!(!conf.contains("dont-fallback"));
        // Unicast: any source address, on any interface.
        let uni = Receive { address: "192.168.68.20".into(), ..r.clone() };
        let conf = receive(&uni, 50, Some(&host())).unwrap();
        assert!(conf.contains("source.ip = \"0.0.0.0\""));
        assert!(!conf.contains("local.ifname"));
        let mono = Receive { channels: 1, encoding: "L16".into(), ..r.clone() };
        let conf = receive(&mono, 20, None).unwrap();
        assert!(conf.contains("audio.position = [ MONO ]") && conf.contains("S16BE"));
        let eight = Receive { channels: 8, ..r.clone() };
        assert!(receive(&eight, 20, None).unwrap().contains("[ AUX0 AUX1 AUX2 AUX3 AUX4 AUX5 AUX6 AUX7 ]"));
        assert!(receive(&Receive { encoding: "G711".into(), ..r }, 20, None).is_err());
    }

    #[test]
    fn the_vban_listener_takes_every_stream_into_patch() {
        let conf = vban_listen(100);
        assert!(balanced(&conf));
        assert!(conf.contains("libpipewire-module-vban-recv"));
        assert!(conf.contains("source.port = 6980"));
        assert!(conf.contains("target.object = \"patch\""));
    }
}
