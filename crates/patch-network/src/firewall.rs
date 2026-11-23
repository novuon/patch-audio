//! The firewall. Omarchy turns ufw on with incoming traffic denied, which
//! also drops the announcements (SAP) and the audio (RTP, VBAN) other
//! machines send here: sending works, receiving shows nothing. Patch
//! reads ufw's rules (the file is readable without root) and offers the
//! one command that opens Patch's ports to the local subnet only.

use std::time::{Duration, Instant};

/// UDP ports Patch receives on: AES67 / Patch streams (5004–5099), VBAN
/// (6980), SAP announcements (9875) and the range a stock PipeWire
/// rtp-sink picks its port from (46000–47024).
pub const PORTS: &str = "5004:5099,6980,9875,46000:47024";
pub const COMMENT: &str = "Patch Network";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Firewall {
    /// No firewall Patch knows of is running.
    Off,
    /// ufw runs and lets Patch's ports in.
    Open,
    /// ufw runs and drops incoming network audio; `command` opens it.
    Blocked { command: String },
}

impl Firewall {
    pub fn state(&self) -> &'static str {
        match self {
            Firewall::Off => "off",
            Firewall::Open => "open",
            Firewall::Blocked { .. } => "blocked",
        }
    }
}

/// The command that lets network audio in from `subnet` (and only there).
pub fn allow_command(subnet: &str) -> String {
    format!(
        "sudo ufw allow proto udp from {subnet} to any port {PORTS} comment '{COMMENT}'"
    )
}

/// Do ufw's rules let SAP announcements in? (Patch's own rule, or any rule
/// a user made for port 9875.)
pub fn rules_allow_sap(rules: &str) -> bool {
    rules.lines().any(|l| {
        let l = l.trim();
        l.starts_with("-A ufw-user-input")
            && l.contains("-j ACCEPT")
            && (l.contains("--dport 9875")
                || l.split_whitespace()
                    .skip_while(|w| *w != "--dports")
                    .nth(1)
                    .is_some_and(|list| list.split(',').any(|p| covers(p, 9875))))
    })
}

fn covers(spec: &str, port: u16) -> bool {
    match spec.split_once(':') {
        Some((a, b)) => match (a.parse::<u16>(), b.parse::<u16>()) {
            (Ok(a), Ok(b)) => (a..=b).contains(&port),
            _ => false,
        },
        None => spec.parse::<u16>() == Ok(port),
    }
}

fn ufw_active() -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "ufw"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The firewall as it stands (read at most every 4 s: the UI asks every
/// second, and a rule the user just added shows soon after).
pub fn check(subnet: &str) -> Firewall {
    static CACHE: std::sync::Mutex<Option<(Instant, String, Firewall)>> =
        std::sync::Mutex::new(None);
    if let Ok(cache) = CACHE.lock()
        && let Some((at, s, fw)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(4)
        && s == subnet
    {
        return fw.clone();
    }
    let fw = if !ufw_active() {
        Firewall::Off
    } else {
        let rules = std::fs::read_to_string("/etc/ufw/user.rules").unwrap_or_default();
        if rules_allow_sap(&rules) {
            Firewall::Open
        } else {
            Firewall::Blocked { command: allow_command(subnet) }
        }
    };
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((Instant::now(), subnet.to_string(), fw.clone()));
    }
    fw
}

#[cfg(test)]
mod tests {
    use super::*;

    // /etc/ufw/user.rules on the user's machine, 2026-11-22 (LocalSend,
    // Docker DNS, Sunshine, SSH): nothing for network audio.
    const OMARCHY: &str = "\
-A ufw-user-input -p udp --dport 53317 -j ACCEPT
-A ufw-user-input -p tcp --dport 53317 -j ACCEPT
-A ufw-user-input -p udp -d 172.17.0.1 --dport 53 -s 172.16.0.0/12 -j ACCEPT
-A ufw-user-input -i tailscale0 -p tcp --dport 47990 -j ACCEPT
-A ufw-user-input -p tcp --dport 22 -s 192.168.68.58 -j ACCEPT
";

    #[test]
    fn omarchys_default_rules_drop_network_audio() {
        assert!(!rules_allow_sap(OMARCHY));
    }

    #[test]
    fn patchs_rule_opens_it() {
        // What `ufw allow proto udp from … port 5004:5099,6980,9875,…` writes.
        let rules = format!(
            "{OMARCHY}### tuple ### allow udp {PORTS} 0.0.0.0/0 any 192.168.68.0/22 in comment=5061746368204e6574776f726b\n\
-A ufw-user-input -p udp -m multiport --dports {PORTS} -s 192.168.68.0/22 -j ACCEPT\n"
        );
        assert!(rules_allow_sap(&rules));
        assert!(rules_allow_sap("-A ufw-user-input -p udp --dport 9875 -j ACCEPT"));
        assert!(!rules_allow_sap("-A ufw-user-input -p udp --dport 9875 -j DROP"));
        assert!(rules_allow_sap("-A ufw-user-input -p udp -m multiport --dports 9000:9999 -j ACCEPT"));
    }

    #[test]
    fn the_command_opens_the_subnet_only() {
        let c = allow_command("192.168.68.0/22");
        assert!(c.starts_with("sudo ufw allow proto udp from 192.168.68.0/22 "));
        assert!(c.contains("9875") && c.contains("6980") && c.contains("5004:5099"));
    }
}
