//! This machine on the local network: the address other machines reach it
//! at, the interface that carries it, and the subnet around it.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// The interface the default route leaves by, with its IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub iface: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

impl Host {
    /// "192.168.68.0/22".
    pub fn subnet(&self) -> String {
        let mask = prefix_mask(self.prefix);
        let net = Ipv4Addr::from(u32::from(self.ip) & mask);
        format!("{net}/{}", self.prefix)
    }

    /// Is `ip` on this machine's own subnet?
    pub fn on_subnet(&self, ip: Ipv4Addr) -> bool {
        let mask = prefix_mask(self.prefix);
        u32::from(ip) & mask == u32::from(self.ip) & mask
    }
}

fn prefix_mask(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) }
}

/// The address the default route uses. Connecting a UDP socket sends
/// nothing; it only asks the kernel which source it would pick.
pub fn route_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    // TEST-NET-1 (RFC 5737): never routed anywhere, only looked up.
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Every IPv4 address on this machine: (interface, address, prefix).
pub fn addresses() -> Vec<(String, Ipv4Addr, u8)> {
    let mut out = Vec::new();
    // SAFETY: getifaddrs fills a linked list that freeifaddrs releases;
    // every pointer is checked before it is read.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut at = list;
        while !at.is_null() {
            let ifa = &*at;
            at = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let addr = &*(ifa.ifa_addr as *const libc::sockaddr_in);
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            let prefix = if ifa.ifa_netmask.is_null() {
                32
            } else {
                let m = &*(ifa.ifa_netmask as *const libc::sockaddr_in);
                u32::from_be(m.sin_addr.s_addr).count_ones() as u8
            };
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            out.push((name, ip, prefix));
        }
        libc::freeifaddrs(list);
    }
    out
}

/// This machine on the local network, or None when it has no network.
pub fn primary() -> Option<Host> {
    let ip = route_ip()?;
    let (iface, _, prefix) = addresses().into_iter().find(|(_, a, _)| *a == ip)?;
    Some(Host { iface, ip, prefix })
}

/// Is this one of this machine's own addresses?
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || addresses().iter().any(|(_, a, _)| *a == v4),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// The machine's name, for the streams it announces.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// A destination typed by the user: an IPv4 address (a name would need a
/// lookup on every start, and PipeWire's modules take addresses).
pub fn parse_destination(text: &str) -> Result<Ipv4Addr, String> {
    let t = text.trim();
    let ip: Ipv4Addr = t
        .parse()
        .map_err(|_| format!("“{t}” is not an IPv4 address (like 192.168.1.20)."))?;
    if ip.is_unspecified() || ip.is_broadcast() {
        return Err(format!("{ip} is not one machine's address."));
    }
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnet_of_the_host() {
        let h = Host { iface: "wlan0".into(), ip: "192.168.68.63".parse().unwrap(), prefix: 22 };
        assert_eq!(h.subnet(), "192.168.68.0/22");
        assert!(h.on_subnet("192.168.71.200".parse().unwrap()));
        assert!(!h.on_subnet("192.168.72.1".parse().unwrap()));
        let all = Host { prefix: 0, ..h.clone() };
        assert_eq!(all.subnet(), "0.0.0.0/0");
    }

    #[test]
    fn this_machine_has_its_own_addresses() {
        assert!(is_local("127.0.0.1".parse().unwrap()));
        if let Some(h) = primary() {
            assert!(is_local(IpAddr::V4(h.ip)));
            assert!(h.prefix > 0 && h.prefix <= 32);
        }
    }

    #[test]
    fn destinations_are_one_machine() {
        assert!(parse_destination(" 192.168.1.20 ").is_ok());
        assert!(parse_destination("kitchen.local").is_err());
        assert!(parse_destination("0.0.0.0").is_err());
        assert!(parse_destination("255.255.255.255").is_err());
    }
}
