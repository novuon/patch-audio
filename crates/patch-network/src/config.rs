//! What the user set up: the sends, the streams Patch listens to, VBAN and
//! the receive latency. Saved in `~/.config/omarchy-patch/network.json`;
//! whether a link runs is its own process (see `link`), not this file.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
use std::path::PathBuf;

/// How the audio travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    /// RTP, 24-bit PCM at 48 kHz (AES67's format), announced with SAP so
    /// other Patches, PipeWire machines and AES67 receivers find it.
    #[default]
    Rtp,
    /// VBAN (VB-Audio's protocol: Voicemeeter, VBAN apps on phones), sent
    /// to one address.
    Vban,
}

impl Proto {
    pub fn label(self) -> &'static str {
        match self {
            Proto::Rtp => "RTP",
            Proto::Vban => "VBAN",
        }
    }
    pub fn parse(s: &str) -> Option<Proto> {
        match s.to_ascii_lowercase().as_str() {
            "rtp" | "aes67" => Some(Proto::Rtp),
            "vban" => Some(Proto::Vban),
            _ => None,
        }
    }
}

/// What a send carries: one of Patch's own taps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Feed {
    /// The program: everything Patch plays, as the Recorder takes it.
    #[default]
    Program,
    Aux1,
    Aux2,
}

impl Feed {
    pub const ALL: [Feed; 3] = [Feed::Program, Feed::Aux1, Feed::Aux2];

    /// The PipeWire source the send captures.
    pub fn node(self) -> &'static str {
        match self {
            Feed::Program => "patch.program",
            Feed::Aux1 => "patch.aux1",
            Feed::Aux2 => "patch.aux2",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Feed::Program => "Program",
            Feed::Aux1 => "AUX 1",
            Feed::Aux2 => "AUX 2",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Feed::Program => "program",
            Feed::Aux1 => "aux1",
            Feed::Aux2 => "aux2",
        }
    }
    pub fn parse(s: &str) -> Option<Feed> {
        match s.to_ascii_lowercase().replace([' ', '-', '_'], "").as_str() {
            "program" | "main" => Some(Feed::Program),
            "aux1" => Some(Feed::Aux1),
            "aux2" => Some(Feed::Aux2),
            _ => None,
        }
    }
}

/// The most sends at once (each has its own group and port).
pub const MAX_SENDS: u8 = 16;
/// RTP's registered port, where AES67 receivers look first.
pub const RTP_PORT: u16 = 5004;
pub const VBAN_PORT: u16 = 6980;

/// One send: a feed to the network.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Send {
    pub id: String,
    /// The stream's name as others see it.
    pub name: String,
    #[serde(default)]
    pub feed: Feed,
    #[serde(default)]
    pub proto: Proto,
    /// One machine's address, or empty: every machine on this network
    /// (RTP multicast). VBAN always needs an address.
    #[serde(default)]
    pub to: String,
    /// Fixes the send's multicast group and port, so receivers find it
    /// again after a restart.
    pub slot: u8,
    /// Runs when Patch's audio service starts (and now).
    #[serde(default)]
    pub on: bool,
}

impl Send {
    /// Where the packets go.
    pub fn destination(&self) -> (Ipv4Addr, u16) {
        match (self.proto, self.to.trim().parse::<Ipv4Addr>()) {
            (Proto::Vban, Ok(ip)) => (ip, VBAN_PORT),
            (Proto::Vban, Err(_)) => (Ipv4Addr::LOCALHOST, VBAN_PORT),
            (Proto::Rtp, Ok(ip)) => (ip, RTP_PORT + 2 * self.slot as u16),
            // An administratively scoped group (RFC 2365) in the range AES67
            // devices use, one per send.
            (Proto::Rtp, Err(_)) => (Ipv4Addr::new(239, 69, 83, 80 + self.slot), RTP_PORT),
        }
    }

    pub fn everyone(&self) -> bool {
        self.proto == Proto::Rtp && self.to.trim().is_empty()
    }

    /// "Everyone on this network" or "192.168.1.20".
    pub fn to_label(&self) -> String {
        if self.everyone() { "everyone on this network".into() } else { self.to.trim().to_string() }
    }
}

/// One stream Patch listens to, as it was announced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receive {
    pub id: String,
    pub name: String,
    /// The machine that sends it.
    pub origin: String,
    /// The multicast group, or the sender for a unicast stream.
    pub address: String,
    pub port: u16,
    pub encoding: String,
    pub rate: u32,
    pub channels: u32,
    #[serde(default)]
    pub on: bool,
}

impl Receive {
    pub fn from_announced(a: &crate::sdp::Announced) -> Receive {
        Receive {
            id: format!("rx-{}", a.id),
            name: a.name.clone(),
            origin: a.origin.to_string(),
            address: a.address.to_string(),
            port: a.port,
            encoding: a.encoding.clone(),
            rate: a.rate,
            channels: a.channels,
            on: true,
        }
    }

    pub fn multicast(&self) -> bool {
        self.address.parse::<std::net::IpAddr>().is_ok_and(|a| a.is_multicast())
    }
}

/// Receive latencies offered, with when each fits.
pub const LATENCIES: [(u32, &str); 4] = [
    (20, "wired"),
    (50, "wired, busy"),
    (100, "Wi-Fi"),
    (250, "busy Wi-Fi"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub sends: Vec<Send>,
    #[serde(default)]
    pub receives: Vec<Receive>,
    /// Take in VBAN streams sent to this machine (port 6980).
    #[serde(default)]
    pub vban: bool,
    /// How far behind a receive plays, to ride out the network's jitter.
    #[serde(default = "default_latency")]
    pub latency_ms: u32,
}

fn default_latency() -> u32 {
    100
}

impl Default for Config {
    fn default() -> Self {
        Config { sends: Vec::new(), receives: Vec::new(), vban: false, latency_ms: default_latency() }
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("omarchy-patch").join("network.json")
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Written whole to a temporary file and renamed, so a crash never
    /// leaves half a file.
    pub fn save(&self) -> Result<(), String> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = p.with_extension("json.new");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text + "\n").map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// The lowest slot no send holds.
    pub fn free_slot(&self) -> Option<u8> {
        (0..MAX_SENDS).find(|s| self.sends.iter().all(|x| x.slot != *s))
    }

    /// A new send (not started). Checks the destination first.
    pub fn add_send(&mut self, feed: Feed, proto: Proto, to: &str, name: &str) -> Result<Send, String> {
        let to = to.trim();
        if !to.is_empty() {
            crate::net::parse_destination(to)?;
        } else if proto == Proto::Vban {
            return Err("VBAN goes to one machine: type its address.".into());
        }
        let slot = self.free_slot().ok_or("Sixteen sends is the most; remove one first.")?;
        let name = name.trim();
        let name = if name.is_empty() {
            format!("Patch {} · {}", feed.label(), crate::net::hostname())
        } else {
            name.to_string()
        };
        if self.sends.iter().any(|s| s.feed == feed && s.proto == proto && s.to.trim() == to) {
            return Err(format!("{} already goes there.", feed.label()));
        }
        let send = Send { id: format!("send-{slot}"), name, feed, proto, to: to.into(), slot, on: true };
        self.sends.push(send.clone());
        Ok(send)
    }

    pub fn latency(&self) -> u32 {
        if LATENCIES.iter().any(|(ms, _)| *ms == self.latency_ms) { self.latency_ms } else { default_latency() }
    }
}

/// VBAN stream names are at most 16 bytes of ASCII.
pub fn vban_name(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '-' })
        .collect();
    let mut out = ascii.trim().to_string();
    out.truncate(16);
    let out = out.trim_end().to_string();
    if out.is_empty() { "Patch".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_send_keeps_its_own_group_and_port() {
        let mut c = Config::default();
        let a = c.add_send(Feed::Program, Proto::Rtp, "", "").unwrap();
        let b = c.add_send(Feed::Aux1, Proto::Rtp, "", "").unwrap();
        let u = c.add_send(Feed::Aux2, Proto::Rtp, "192.168.1.20", "Kitchen").unwrap();
        assert_eq!(a.destination(), ("239.69.83.80".parse().unwrap(), 5004));
        assert_eq!(b.destination(), ("239.69.83.81".parse().unwrap(), 5004));
        assert_eq!(u.destination(), ("192.168.1.20".parse().unwrap(), 5008));
        assert!(a.everyone() && !u.everyone());
        assert_eq!(u.name, "Kitchen");
        assert!(a.name.starts_with("Patch Program · "));
        // A removed send frees its slot for the next.
        c.sends.retain(|s| s.id != b.id);
        assert_eq!(c.add_send(Feed::Aux1, Proto::Rtp, "", "").unwrap().slot, 1);
    }

    #[test]
    fn vban_needs_an_address_and_the_same_send_is_refused() {
        let mut c = Config::default();
        assert!(c.add_send(Feed::Program, Proto::Vban, "", "").is_err());
        let v = c.add_send(Feed::Program, Proto::Vban, "10.0.0.5", "").unwrap();
        assert_eq!(v.destination(), ("10.0.0.5".parse().unwrap(), 6980));
        assert!(c.add_send(Feed::Program, Proto::Vban, "10.0.0.5", "").is_err());
        assert!(c.add_send(Feed::Program, Proto::Rtp, "not an ip", "").is_err());
    }

    #[test]
    fn sixteen_sends_at_most() {
        let mut c = Config::default();
        for i in 0..MAX_SENDS {
            c.add_send(Feed::Program, Proto::Rtp, &format!("10.0.0.{}", i + 1), "").unwrap();
        }
        assert!(c.add_send(Feed::Program, Proto::Rtp, "", "").is_err());
    }

    #[test]
    fn old_and_partial_files_load() {
        let c: Config = serde_json::from_str(r#"{"sends":[{"id":"send-0","name":"x","slot":0}]}"#).unwrap();
        assert_eq!(c.sends[0].feed, Feed::Program);
        assert_eq!(c.sends[0].proto, Proto::Rtp);
        assert!(!c.sends[0].on);
        assert_eq!(c.latency(), 100);
        let odd = Config { latency_ms: 7, ..Config::default() };
        assert_eq!(odd.latency(), 100);
    }

    #[test]
    fn vban_names_fit_the_header() {
        assert_eq!(vban_name("Patch Program · omarchy-main"), "Patch Program -");
        assert_eq!(vban_name(""), "Patch");
        assert!(vban_name("ééééééééééééééééééé").len() <= 16);
    }
}
