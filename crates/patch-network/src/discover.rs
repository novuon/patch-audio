//! Streams other machines announce. One listener thread per process, started
//! the first time it is asked (the Network window, `network scan`): it
//! joins the SAP groups PipeWire (224.0.0.56) and AES67 / Dante
//! (239.255.255.255) announce on and keeps what it heard. A stream that
//! is not announced again for a minute is gone (PipeWire repeats every
//! 5 s, AES67 devices every 30 s).

use crate::sdp::{self, Announced};
use std::collections::HashMap;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::FromRawFd;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SAP_PORT: u16 = 9875;
pub const GROUPS: [Ipv4Addr; 2] = [Ipv4Addr::new(224, 0, 0, 56), Ipv4Addr::new(239, 255, 255, 255)];
pub const EXPIRE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Table {
    streams: HashMap<String, (Announced, Instant)>,
    /// Why the listener could not start, if it could not.
    error: Option<String>,
    started: Option<Instant>,
}

impl Table {
    fn heard(&mut self, sap: sdp::Sap, now: Instant) {
        let Some(a) = sdp::parse_sdp(&sap.sdp, sap.origin) else {
            return;
        };
        if sap.bye {
            self.streams.remove(&a.id);
        } else {
            self.streams.insert(a.id.clone(), (a, now));
        }
    }

    fn current(&mut self, now: Instant) -> Vec<Announced> {
        self.streams.retain(|_, (_, at)| now.duration_since(*at) < EXPIRE);
        let mut v: Vec<Announced> = self.streams.values().map(|(a, _)| a.clone()).collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        v
    }
}

fn table() -> &'static Arc<Mutex<Table>> {
    static T: OnceLock<Arc<Mutex<Table>>> = OnceLock::new();
    T.get_or_init(|| {
        let t = Arc::new(Mutex::new(Table::default()));
        let shared = t.clone();
        match socket() {
            Ok(sock) => {
                shared.lock().unwrap().started = Some(Instant::now());
                let _ = std::thread::Builder::new()
                    .name("patch-net-sap".into())
                    .spawn(move || listen(sock, shared));
            }
            Err(e) => shared.lock().unwrap().error = Some(e),
        }
        t
    })
}

/// A UDP socket on the SAP port that shares it (PipeWire's own SAP
/// listener binds it too), in both groups.
fn socket() -> Result<UdpSocket, String> {
    // SAFETY: a fresh socket descriptor, configured with plain setsockopt
    // calls on stack integers and handed to UdpSocket, which owns it.
    let sock = unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let one: libc::c_int = 1;
        for opt in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        UdpSocket::from_raw_fd(fd)
    };
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: SAP_PORT.to_be(),
        sin_addr: libc::in_addr { s_addr: 0 },
        sin_zero: [0; 8],
    };
    // SAFETY: binds the socket UdpSocket owns to a valid sockaddr_in.
    let r = unsafe {
        use std::os::fd::AsRawFd;
        libc::bind(
            sock.as_raw_fd(),
            (&addr as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if r != 0 {
        return Err(format!("SAP port {SAP_PORT}: {}", std::io::Error::last_os_error()));
    }
    let iface = crate::net::primary().map(|h| h.ip).unwrap_or(Ipv4Addr::UNSPECIFIED);
    let mut joined = 0;
    for g in GROUPS {
        if sock.join_multicast_v4(&g, &iface).is_ok() {
            joined += 1;
        }
    }
    if joined == 0 {
        return Err("could not join the announcement groups (no network?)".into());
    }
    let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
    Ok(sock)
}

fn listen(sock: UdpSocket, table: Arc<Mutex<Table>>) {
    let mut buf = [0u8; 4096];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(sap) = sdp::parse_sap(&buf[..n])
                    && let Ok(mut t) = table.lock()
                {
                    t.heard(sap, Instant::now());
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// What is announced now. The first call starts listening, so it returns
/// what the network said in the moments since.
pub fn streams() -> Vec<Announced> {
    table().lock().map(|mut t| t.current(Instant::now())).unwrap_or_default()
}

/// Why nothing can be heard, when listening failed.
pub fn error() -> Option<String> {
    table().lock().ok()?.error.clone()
}

/// How long the listener has been listening.
pub fn listening_for() -> Duration {
    table()
        .lock()
        .ok()
        .and_then(|t| t.started)
        .map(|s| s.elapsed())
        .unwrap_or_default()
}

/// Listen for `secs` (for `omarchy-patch network scan`).
pub fn scan(secs: u64) -> Vec<Announced> {
    let _ = streams();
    std::thread::sleep(Duration::from_secs(secs).saturating_sub(listening_for()));
    streams()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn sap(bye: bool, version: &str) -> sdp::Sap {
        sdp::Sap {
            bye,
            origin: IpAddr::V4(Ipv4Addr::new(192, 168, 68, 20)),
            sdp: format!(
                "v=0\no=patch 77 {version} IN IP4 192.168.68.20\ns=Studio\nc=IN IP4 239.69.83.80/1\nm=audio 5004 RTP/AVP 127\na=rtpmap:127 L24/48000/2\n"
            ),
        }
    }

    #[test]
    fn announcements_come_and_go() {
        let mut t = Table::default();
        let now = Instant::now();
        t.heard(sap(false, "1"), now);
        t.heard(sap(false, "2"), now);
        assert_eq!(t.current(now).len(), 1, "a new version is the same stream");
        t.heard(sap(true, "2"), now);
        assert!(t.current(now).is_empty(), "a bye removes it");
        t.heard(sap(false, "3"), now);
        assert!(t.current(now + EXPIRE + Duration::from_secs(1)).is_empty(), "silence expires it");
    }
}
