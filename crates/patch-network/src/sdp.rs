//! SAP packets (RFC 2974) and the SDP they carry (RFC 4566): how a machine
//! on the network says "I am sending this stream, here is where and how".
//! PipeWire, other Patches, AES67 devices and Dante in AES67 mode announce
//! this way; only what a receiver needs is read.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// One stream a machine announces.
#[derive(Debug, Clone, PartialEq)]
pub struct Announced {
    /// Stable for the stream: its origin and session id (not the version,
    /// which changes when the sender edits the stream).
    pub id: String,
    /// The session name (`s=`).
    pub name: String,
    /// Who announces it (the SAP originating source).
    pub origin: IpAddr,
    /// Where the packets go: a multicast group, or the sender itself for a
    /// unicast stream (`c=`).
    pub address: IpAddr,
    pub port: u16,
    pub payload: u8,
    /// `L16`, `L24`, `L32` (big-endian PCM), `opus`, …
    pub encoding: String,
    pub rate: u32,
    pub channels: u32,
    /// Milliseconds of audio per packet, when announced.
    pub ptime: Option<f32>,
    /// The sender keeps a PTP clock (AES67 proper): its packets are timed
    /// against a clock this machine does not follow.
    pub ptp: bool,
    /// What made it (`a=tool:`), e.g. "PipeWire 1.6.11".
    pub tool: String,
    /// A unicast Patch send names the one machine it is for
    /// (`a=x-patch-to:`); other machines do not offer it.
    pub to: Option<IpAddr>,
}

impl Announced {
    /// Can Patch receive this stream? PCM that PipeWire's RTP source
    /// decodes, in a rate and channel count it accepts.
    pub fn supported(&self) -> bool {
        matches!(self.encoding.as_str(), "L16" | "L24" | "L32" | "opus")
            && (8_000..=384_000).contains(&self.rate)
            && (1..=64).contains(&self.channels)
            && self.port > 0
    }

    pub fn multicast(&self) -> bool {
        self.address.is_multicast()
    }

    /// "L24 · 48 kHz · 2 ch".
    pub fn format_label(&self) -> String {
        format_label(&self.encoding, self.rate, self.channels)
    }
}

pub fn format_label(encoding: &str, rate: u32, channels: u32) -> String {
    let khz = if rate.is_multiple_of(1000) {
        format!("{}", rate / 1000)
    } else {
        format!("{:.1}", rate as f64 / 1000.0)
    };
    format!("{encoding} · {khz} kHz · {channels} ch")
}

/// A SAP packet: `bye` is a deletion (the sender stopped the stream).
#[derive(Debug, Clone, PartialEq)]
pub struct Sap {
    pub bye: bool,
    pub origin: IpAddr,
    pub sdp: String,
}

/// Read a SAP packet. Encrypted and compressed packets are skipped, as are
/// payloads other than SDP.
pub fn parse_sap(packet: &[u8]) -> Option<Sap> {
    let flags = *packet.first()?;
    let version = flags >> 5;
    let ipv6 = flags & 0x10 != 0;
    let bye = flags & 0x04 != 0;
    let encrypted = flags & 0x02 != 0;
    let compressed = flags & 0x01 != 0;
    if version != 1 || encrypted || compressed {
        return None;
    }
    let auth_words = *packet.get(1)? as usize;
    let (origin, mut at) = if ipv6 {
        let b: [u8; 16] = packet.get(4..20)?.try_into().ok()?;
        (IpAddr::V6(Ipv6Addr::from(b)), 20)
    } else {
        let b: [u8; 4] = packet.get(4..8)?.try_into().ok()?;
        (IpAddr::V4(Ipv4Addr::from(b)), 8)
    };
    at += auth_words * 4;
    let rest = packet.get(at..)?;
    // An optional payload type ("application/sdp\0") before the SDP; an
    // SDP always starts with "v=".
    let body = if rest.starts_with(b"v=") {
        rest
    } else {
        let nul = rest.iter().position(|&b| b == 0)?;
        if &rest[..nul] != b"application/sdp" {
            return None;
        }
        &rest[nul + 1..]
    };
    let sdp = String::from_utf8_lossy(body).trim_end_matches('\0').to_string();
    Some(Sap { bye, origin, sdp })
}

/// Read the first audio stream an SDP describes.
pub fn parse_sdp(sdp: &str, origin: IpAddr) -> Option<Announced> {
    let mut name = String::new();
    let mut session = String::new();
    let mut session_addr: Option<IpAddr> = None;
    let mut media_addr: Option<IpAddr> = None;
    let mut port = 0u16;
    let mut payload: Option<u8> = None;
    let mut rtpmap: Option<(String, u32, u32)> = None;
    let mut ptime = None;
    let mut ptp = false;
    let mut tool = String::new();
    let mut to = None;
    let mut in_audio = false;
    let mut seen_audio = false;
    for line in sdp.lines() {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "o" => {
                // o=<user> <session id> <version> IN IP4 <address>
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() >= 6 {
                    session = format!("{}-{}", parts[1], parts[5]);
                }
            }
            "s" if !seen_audio => name = value.trim().to_string(),
            "c" => {
                // c=IN IP4 239.69.83.80/32
                let addr = value
                    .split_whitespace()
                    .nth(2)
                    .and_then(|a| a.split('/').next())
                    .and_then(|a| a.parse().ok());
                if in_audio {
                    media_addr = addr;
                } else if !seen_audio {
                    session_addr = addr;
                }
            }
            "m" => {
                if seen_audio {
                    // Only the first audio stream.
                    in_audio = false;
                    continue;
                }
                let parts: Vec<&str> = value.split_whitespace().collect();
                in_audio = parts.first() == Some(&"audio")
                    && parts.get(2).is_some_and(|p| p.starts_with("RTP/AVP"));
                if in_audio {
                    seen_audio = true;
                    port = parts.get(1).and_then(|p| p.split('/').next()?.parse().ok())?;
                    payload = parts.get(3).and_then(|p| p.parse().ok());
                }
            }
            "a" if in_audio || !seen_audio => {
                let (attr, val) = value.split_once(':').unwrap_or((value, ""));
                match attr {
                    "rtpmap" if in_audio => {
                        // a=rtpmap:<pt> <encoding>/<rate>[/<channels>]
                        let mut it = val.split_whitespace();
                        let pt: Option<u8> = it.next().and_then(|p| p.parse().ok());
                        if pt.is_some() && pt == payload
                            && let Some(spec) = it.next()
                        {
                            let mut f = spec.split('/');
                            let enc = f.next().unwrap_or_default().to_string();
                            let rate = f.next().and_then(|r| r.parse().ok()).unwrap_or(0);
                            let ch = f.next().and_then(|c| c.parse().ok()).unwrap_or(1);
                            rtpmap = Some((enc, rate, ch));
                        }
                    }
                    "ptime" => ptime = val.trim().parse().ok(),
                    "ts-refclk" => ptp |= val.trim().starts_with("ptp"),
                    "tool" => tool = val.trim().to_string(),
                    "x-patch-to" => to = val.trim().parse().ok(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let (encoding, rate, channels) = match (rtpmap, payload) {
        (Some(m), _) => m,
        // The static payload types (RFC 3551).
        (None, Some(10)) => ("L16".into(), 44_100, 2),
        (None, Some(11)) => ("L16".into(), 44_100, 1),
        _ => return None,
    };
    let address = media_addr.or(session_addr).unwrap_or(origin);
    // An unspecified address means "the sender itself".
    let address = if address.is_unspecified() { origin } else { address };
    if session.is_empty() {
        session = format!("{origin}-{address}-{port}");
    }
    if name.is_empty() || name == "-" {
        name = format!("Stream from {origin}");
    }
    Some(Announced {
        id: stream_id(&session),
        name,
        origin,
        address,
        port,
        payload: payload.unwrap_or(127),
        encoding,
        rate,
        channels,
        ptime,
        ptp,
        tool,
        to,
    })
}

/// A short, file-name-safe id from any key (FNV-1a, 32 bits in hex).
pub fn stream_id(key: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // What PipeWire 1.6.11's rtp-sap sent for a Patch send (captured
    // 2026-11-22 from `PIPEWIRE_DEBUG=mod.rtp-sap:5`).
    const PIPEWIRE: &str = "v=0\n\
o=patch 4004355967 4004355967 IN IP4 192.168.68.63\n\
s=Patch test send\n\
c=IN IP4 239.69.83.80/1\n\
t=4004355967 0\n\
m=audio 5004 RTP/AVP 127\n\
i=2 channels: AUX1, AUX2\n\
a=recvonly\n\
a=rtpmap:127 L24/48000/2\n\
a=source-filter: incl IN IP4 239.69.83.80 192.168.68.63\n\
a=ssrc:3867642254\n\
a=ptime:4.291667\n\
a=framecount:206\n\
a=mediaclk:sender\n\
a=tool:PipeWire 1.6.11\n\
a=type:broadcast\n";

    // A Dante device in AES67 mode (the shape Audinate's SDP takes).
    const DANTE: &str = "v=0\r\n\
o=- 1311738121 1311738121 IN IP4 192.168.1.20\r\n\
s=AVIOAI2-1 : 2\r\n\
c=IN IP4 239.69.161.58/32\r\n\
t=0 0\r\n\
a=keywds:Dante\r\n\
m=audio 5004 RTP/AVP 97\r\n\
i=2 channels: Left, Right\r\n\
a=recvonly\r\n\
a=rtpmap:97 L24/48000/2\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0\r\n\
a=mediaclk:direct=0\r\n";

    fn origin() -> IpAddr {
        "192.168.68.63".parse().unwrap()
    }

    #[test]
    fn reads_what_pipewire_announces() {
        let a = parse_sdp(PIPEWIRE, origin()).unwrap();
        assert_eq!(a.name, "Patch test send");
        assert_eq!(a.address, "239.69.83.80".parse::<IpAddr>().unwrap());
        assert_eq!(a.port, 5004);
        assert_eq!((a.encoding.as_str(), a.rate, a.channels), ("L24", 48_000, 2));
        assert_eq!(a.payload, 127);
        assert!(a.multicast() && a.supported() && !a.ptp);
        assert_eq!(a.tool, "PipeWire 1.6.11");
        assert_eq!(a.format_label(), "L24 · 48 kHz · 2 ch");
        assert!((a.ptime.unwrap() - 4.291667).abs() < 1e-4);
    }

    #[test]
    fn reads_an_aes67_device_and_sees_its_ptp_clock() {
        let a = parse_sdp(DANTE, "192.168.1.20".parse().unwrap()).unwrap();
        assert_eq!(a.name, "AVIOAI2-1 : 2");
        assert_eq!(a.payload, 97);
        assert!(a.ptp && a.supported());
        assert_eq!(a.ptime, Some(1.0));
    }

    #[test]
    fn the_id_survives_a_new_version_but_not_a_new_session() {
        let a = parse_sdp(PIPEWIRE, origin()).unwrap();
        let edited = PIPEWIRE.replace("4004355967 4004355967", "4004355967 4004355999");
        assert_eq!(parse_sdp(&edited, origin()).unwrap().id, a.id);
        let other = PIPEWIRE.replace("4004355967 4004355967", "4004355968 4004355968");
        assert_ne!(parse_sdp(&other, origin()).unwrap().id, a.id);
    }

    #[test]
    fn a_unicast_stream_comes_from_its_sender() {
        let sdp = PIPEWIRE.replace("c=IN IP4 239.69.83.80/1", "c=IN IP4 0.0.0.0");
        let a = parse_sdp(&sdp, origin()).unwrap();
        assert_eq!(a.address, origin());
        assert!(!a.multicast());
        assert_eq!(a.to, None);
        // What a unicast Patch send adds at the end (sap.end-extra).
        let named = format!("{sdp}a=x-patch-to:192.168.68.20\n");
        assert_eq!(parse_sdp(&named, origin()).unwrap().to, Some("192.168.68.20".parse().unwrap()));
    }

    #[test]
    fn video_and_unknown_payloads_are_not_audio() {
        let video = "v=0\no=- 1 1 IN IP4 10.0.0.2\ns=cam\nc=IN IP4 239.1.1.1\nm=video 5000 RTP/AVP 96\na=rtpmap:96 H264/90000\n";
        assert!(parse_sdp(video, origin()).is_none());
        let midi = PIPEWIRE.replace("L24/48000/2", "rtp-midi/48000");
        assert!(!parse_sdp(&midi, origin()).unwrap().supported());
    }

    fn packet(bye: bool, typed: bool, sdp: &str) -> Vec<u8> {
        let mut p = vec![0x20 | if bye { 0x04 } else { 0 }, 0, 0x12, 0x34, 192, 168, 68, 63];
        if typed {
            p.extend_from_slice(b"application/sdp\0");
        }
        p.extend_from_slice(sdp.as_bytes());
        p
    }

    #[test]
    fn sap_packets_with_and_without_a_payload_type() {
        for typed in [true, false] {
            let s = parse_sap(&packet(false, typed, PIPEWIRE)).unwrap();
            assert!(!s.bye);
            assert_eq!(s.origin, origin());
            assert!(s.sdp.starts_with("v=0"));
        }
        assert!(parse_sap(&packet(true, true, PIPEWIRE)).unwrap().bye);
    }

    #[test]
    fn encrypted_compressed_and_short_packets_are_skipped() {
        let mut p = packet(false, true, PIPEWIRE);
        p[0] |= 0x02;
        assert!(parse_sap(&p).is_none());
        p[0] = 0x21;
        assert!(parse_sap(&p).is_none());
        assert!(parse_sap(&[0x20, 0, 0]).is_none());
        let mut other = packet(false, false, "");
        other.extend_from_slice(b"text/plain\0hello");
        assert!(parse_sap(&other).is_none());
    }
}
