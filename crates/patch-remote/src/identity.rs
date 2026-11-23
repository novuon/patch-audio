//! Resolve claimed Tailscale addresses through the local whois API.
//!
//! TCP loopback does not expose useful SO_PEERCRED credentials. Headers
//! therefore never authenticate a caller: Access requires pairing even when
//! whois confirms the configured account for the claimed address.

use crate::access::{self, Peer};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Live {
    socket: PathBuf,
}

impl Live {
    pub fn resolve(flag: Option<PathBuf>) -> Self {
        if let Some(path) = flag {
            return Self { socket: path };
        }
        if let Some(path) = std::env::var_os("PATCH_REMOTE_TAILSCALE_SOCKET")
            && !path.is_empty()
        {
            return Self {
                socket: PathBuf::from(path),
            };
        }
        for candidate in [
            "/var/run/tailscale/tailscaled.sock",
            "/run/tailscale/tailscaled.sock",
        ] {
            if Path::new(candidate).exists() {
                return Self {
                    socket: PathBuf::from(candidate),
                };
            }
        }
        Self {
            socket: PathBuf::from("/var/run/tailscale/tailscaled.sock"),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn probe(&self) -> Result<(), String> {
        UnixStream::connect(&self.socket)
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", self.socket.display()))
    }
}

pub trait Identity: Send + Sync {
    fn peer(&self, stream: &std::net::TcpStream) -> Peer;
    fn whois_login(&self, ip: &str) -> Result<String, String>;
}

impl Identity for Live {
    fn peer(&self, stream: &std::net::TcpStream) -> Peer {
        // The pid can be recycled between this lookup and `read_link`. A
        // recycled pid would have to be `tailscaled` running as root or as
        // this user, which is the same peer we already trust.
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).unwrap_or(0);
        if len == 0 {
            return Peer::Other;
        }
        // SAFETY: `cred` is a live `ucred` and `len` is its size. `fd` is the
        // accepted TCP socket. `SO_PEERCRED` writes the peer's pid/uid/gid.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Peer::Other;
        }
        // `SO_PEERCRED` is not populated for AF_INET loopback sockets on this
        // kernel: a local client and Tailscale Serve's forward both arrive
        // with pid 0 and uid/gid −1. There is therefore no peer process to
        // identify for *any* loopback connection, and the old "must be
        // tailscaled" gate locked out every phone while proving nothing about
        // local ones either.
        //
        // Forwarded addresses are caller supplied. Whois only constrains
        // their account; Access also requires the pairing secret, so copying
        // a known phone address cannot authenticate a local client.
        if cred.pid <= 0 {
            return Peer::Local;
        }
        // A real peer process: it must be tailscaled, as before.
        // SAFETY: `geteuid` takes no pointers and does not change process state.
        let our_uid = unsafe { libc::geteuid() };
        match peer_name(cred.pid, cred.uid, our_uid) {
            Some(name) => classify_peer(cred.uid, name.as_deref(), our_uid),
            None => Peer::Other,
        }
    }

    fn whois_login(&self, ip: &str) -> Result<String, String> {
        match query_whois(&self.socket, ip) {
            Ok(login) => Ok(login),
            Err(err) => {
                if err != "not a Tailscale address" {
                    warn_whois(&err);
                }
                Err(err)
            }
        }
    }
}

fn warn_whois(err: &str) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "patch-remote: Tailscale whois failed ({err}). Phone access is denied until the local API answers."
        );
    });
}

fn classify_peer(peer_uid: u32, exe_name: Option<&std::ffi::OsStr>, our_uid: u32) -> Peer {
    if exe_name != Some(std::ffi::OsStr::new("tailscaled")) {
        return Peer::Other;
    }
    if peer_uid == 0 || peer_uid == our_uid {
        Peer::Tailscaled
    } else {
        Peer::Other
    }
}

/// The peer process's executable name, or `None` when it cannot be read.
///
/// `/proc/<pid>/exe` needs ptrace-class permission on a process of another
/// uid, so a root `tailscaled` is unreadable to an unprivileged Patch. Only a
/// peer whose uid is root or our own can be named at all; a stranger's
/// process is refused without a name, and never reaches whois.
fn peer_name(pid: i32, peer_uid: u32, our_uid: u32) -> Option<Option<std::ffi::OsString>> {
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return Some(exe.file_name().map(std::ffi::OsStr::to_os_string));
    }
    if peer_uid != 0 && peer_uid != our_uid {
        return None;
    }
    // The exe link is denied (a root peer we may still trust by uid). The
    // name cannot be recovered from /proc, so treat it as unnamed: only a
    // uid we vouch for gets this far, and `classify_peer` refuses an unnamed
    // peer. Identity for a served request comes from whois instead.
    Some(None)
}

/// `GET /localapi/v0/whois` on `tailscaled`'s Unix socket.
pub(crate) fn query_whois(socket: &Path, ip: &str) -> Result<String, String> {
    if !access::is_tailscale_ip(ip) {
        return Err("not a Tailscale address".into());
    }
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let request = format!(
        "GET /localapi/v0/whois?proto=tcp&addr={ip} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    loop {
        if buf.len() >= 1_048_576 {
            return Err("whois reply too large".into());
        }
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("whois read: {e}")),
        }
    }
    parse_whois(&buf)
}

fn parse_whois(bytes: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "whois reply is not utf-8".to_string())?;
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .ok_or("whois reply incomplete")?;
    let status = head.lines().next().ok_or("whois status missing")?;
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        return Err(format!("whois status {code}"));
    }
    let mut length = None;
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("duplicate whois length".into());
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "bad whois length".to_string())?,
            );
        }
    }
    let length = length.ok_or("whois reply has no length")?;
    let body = rest.as_bytes();
    if body.len() < length {
        return Err("whois body short".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&body[..length]).map_err(|e| e.to_string())?;
    value
        .get("UserProfile")
        .and_then(|profile| profile.get("LoginName"))
        .and_then(|login| login.as_str())
        .filter(|login| !login.is_empty() && !login.contains(['\r', '\n']))
        .map(str::to_owned)
        .ok_or_else(|| "whois reply has no login".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn peer_classification_requires_tailscaled_and_a_trusted_uid() {
        let name = Some(std::ffi::OsStr::new("tailscaled"));
        assert_eq!(classify_peer(0, name, 1000), Peer::Tailscaled);
        assert_eq!(classify_peer(1000, name, 1000), Peer::Tailscaled);
        assert_eq!(classify_peer(1001, name, 1000), Peer::Other);
        assert_eq!(
            classify_peer(0, Some(std::ffi::OsStr::new("nginx")), 1000),
            Peer::Other
        );
        assert_eq!(classify_peer(0, None, 0), Peer::Other);
    }

    /// `tailscaled` runs as root, so `/proc/<pid>/exe` is unreadable to an
    /// unprivileged Patch. The peer is then unnamed — and `classify_peer`
    /// refuses an unnamed peer, which is safe because a served request's
    /// identity comes from whois instead.
    #[test]
    fn a_root_peer_is_unnamed_when_exe_is_unreadable() {
        // Our own process: `exe` is readable, so the strong path is used and
        // the name is this test binary, not `tailscaled`.
        let own = std::process::id() as i32;
        let name = peer_name(own, unsafe { libc::geteuid() }, unsafe { libc::geteuid() })
            .expect("our own exe is readable")
            .expect("our own exe has a name");
        assert_ne!(name, std::ffi::OsString::from("tailscaled"));

        // A pid that does not exist: the exe link is unreadable, and with a
        // vouchable uid the peer is simply unnamed (not "refused").
        assert_eq!(peer_name(i32::MAX, 0, 1000), Some(None));

        // A foreign uid we cannot vouch for is refused outright.
        assert!(
            peer_name(1, 1001, 1000).is_none(),
            "a stranger's uid is refused"
        );
        assert!(
            peer_name(i32::MAX, 1001, 1000).is_none(),
            "a stranger's pid is refused"
        );

        // An unnamed peer is never classified as tailscaled.
        assert_eq!(classify_peer(0, None, 1000), Peer::Other);
    }

    /// The peer classifications that decide who reaches whois.
    #[test]
    fn root_tailscaled_is_the_peer_we_trust() {
        assert_eq!(
            classify_peer(0, Some(std::ffi::OsStr::new("tailscaled")), 1000),
            Peer::Tailscaled
        );
        assert_eq!(
            classify_peer(1000, Some(std::ffi::OsStr::new("tailscaled")), 1000),
            Peer::Tailscaled
        );
        // A root process that is not tailscaled is refused.
        assert_eq!(
            classify_peer(0, Some(std::ffi::OsStr::new("nginx")), 1000),
            Peer::Other
        );
        // A non-root process that renamed itself keeps its uid.
        assert_eq!(
            classify_peer(1001, Some(std::ffi::OsStr::new("tailscaled")), 1000),
            Peer::Other
        );
    }

    /// A loopback connection carries no peer process: `SO_PEERCRED` is not
    /// populated for AF_INET loopback sockets, so both the accepted and the
    /// connecting side report pid 0. This is why the peer check cannot be the
    /// basis of trust on loopback — Tailscale Serve's forward looks exactly
    /// the same — and why whois carries the identity decision.
    #[test]
    fn a_loopback_connection_has_no_peer_process() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        let live = Live {
            socket: PathBuf::from("/no/such/tailscaled.sock"),
        };
        assert_eq!(live.peer(&server), Peer::Local);
        assert_eq!(live.peer(&client), Peer::Local);
    }

    #[test]
    fn whois_reads_the_login_and_rejects_other_answers() {
        assert_eq!(
            query_whois(Path::new("/no/such/tailscaled.sock"), "8.8.8.8").unwrap_err(),
            "not a Tailscale address"
        );
        let dir = std::env::temp_dir().join(format!("patch-remote-whois-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let socket = dir.join("tailscaled.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let body = r#"{"Node":{"Name":"phone.tailnet.ts.net."},"UserProfile":{"LoginName":"dj@example.com","DisplayName":"DJ"}}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = String::new();
            let mut chunk = [0u8; 1024];
            let n = stream.read(&mut chunk).unwrap();
            request.push_str(&String::from_utf8_lossy(&chunk[..n]));
            assert!(request.contains("GET /localapi/v0/whois?proto=tcp&addr=100.64.0.8 "));
            assert!(request.contains("Host: local-tailscaled.sock"));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        let login = query_whois(&socket, "100.64.0.8").unwrap();
        assert_eq!(login, "dj@example.com");
        server.join().unwrap();

        let denied = dir.join("denied.sock");
        let listener = UnixListener::bind(&denied).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut chunk = [0u8; 512];
            let _ = stream.read(&mut chunk);
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nno")
                .unwrap();
        });
        let err = query_whois(&denied, "fd7a:115c:a1e0::9").unwrap_err();
        assert!(err.contains("404"), "{err}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
