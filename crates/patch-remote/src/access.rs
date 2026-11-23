//! Who may call the loopback remote.
//!
//! Localhost is not a trust domain. A browser on this computer must present
//! the owner-only pairing secret. Phones need it too, plus a Tailscale whois
//! account match. Forwarded addresses and identity headers are not proof of
//! the connection's identity.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const LOCAL_HOST: &str = "127.0.0.1:8124";
pub const LOCAL_ORIGIN: &str = "http://127.0.0.1:8124";

/// TCP peer as classified by `SO_PEERCRED`, not by anything the client wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    /// The accepted connection is the `tailscaled` process (root or this user).
    Tailscaled,
    /// No peer process could be identified: `SO_PEERCRED` is not populated
    /// for AF_INET loopback sockets, so a local client and Tailscale Serve's
    /// forward are indistinguishable at the socket level. Both paths need
    /// pairing; the tailnet path additionally constrains the account via whois.
    Local,
    /// A peer process that is not `tailscaled`, including one that copied
    /// Tailscale's headers onto a real local connection.
    Other,
}

#[derive(Clone, Debug)]
pub struct Access {
    pairing: String,
    tailscale: Option<(String, String)>,
}

/// What the HTTP layer knows about one request. Headers are client-supplied.
pub struct RequestMeta<'a> {
    pub host: &'a str,
    pub forwarded_for: &'a str,
    pub pairing: &'a str,
    pub proxied: bool,
    pub funnel: bool,
    pub peer: Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision<'a> {
    Allow {
        origin: &'a str,
    },
    /// Eligible request with a missing or wrong pairing secret.
    PairingRequired,
    Deny,
}

impl Access {
    pub fn local(pairing: String) -> Result<Self, String> {
        check_pairing(&pairing)?;
        Ok(Self {
            pairing,
            tailscale: None,
        })
    }

    pub fn tailscale(origin: String, login: String, pairing: String) -> Result<Self, String> {
        check_pairing(&pairing)?;
        let authority = origin
            .strip_prefix("https://")
            .ok_or("Tailscale origin must use HTTPS")?;
        let name = authority.split(':').next().unwrap_or("");
        if !name.ends_with(".ts.net")
            || name
                .bytes()
                .any(|c| !c.is_ascii_alphanumeric() && c != b'.' && c != b'-')
            || authority.contains('/')
            || authority.contains('@')
            || authority.contains('?')
            || authority.contains('#')
            || login.is_empty()
            || login.contains(['\r', '\n'])
        {
            return Err("Use a Tailscale HTTPS origin and an exact account login".into());
        }
        if let Some((_, port)) = authority.split_once(':')
            && port.parse::<u16>().ok().is_none_or(|p| p == 0)
        {
            return Err("Invalid HTTPS port".into());
        }
        Ok(Self {
            pairing,
            tailscale: Some((origin, login)),
        })
    }

    pub fn phone_origin(&self) -> Option<&str> {
        self.tailscale.as_ref().map(|(origin, _)| origin.as_str())
    }

    /// Origin implied by `Host`, before any proof of who is calling.
    pub fn page_origin(&self, host: &str) -> Option<&str> {
        if host == LOCAL_HOST {
            return Some(LOCAL_ORIGIN);
        }
        let (origin, _) = self.tailscale.as_ref()?;
        (host == origin.trim_start_matches("https://")).then_some(origin.as_str())
    }

    /// The static shell may load on a clean localhost connection so the
    /// browser can ask for the pairing code. That does not authorize APIs.
    pub fn local_shell(&self, req: &RequestMeta<'_>) -> bool {
        req.peer != Peer::Other && req.host == LOCAL_HOST && !req.proxied && !req.funnel
    }

    /// Whois constrains the claimed address to the configured account; the
    /// pairing secret authenticates the caller because TCP headers can be forged.
    pub fn authorize<'a>(
        &'a self,
        req: &RequestMeta<'_>,
        mut whois: impl FnMut(&str) -> Result<String, String>,
    ) -> Decision<'a> {
        if req.funnel {
            return Decision::Deny;
        }
        if let Some((origin, allowed)) = &self.tailscale {
            let authority = origin.trim_start_matches("https://");
            // TCP cannot prove the forwarding process. Host and forwarded IP
            // are spoofable, even when whois confirms the claimed address.
            // Require the owner-only pairing secret as well as the account.
            if req.peer != Peer::Other && req.host == authority {
                let ip = req.forwarded_for.trim();
                if !is_tailscale_ip(ip) {
                    return Decision::Deny;
                }
                return match whois(ip) {
                    Ok(login) if login == *allowed => {
                        if secrets_eq(req.pairing, &self.pairing) {
                            Decision::Allow { origin }
                        } else {
                            Decision::PairingRequired
                        }
                    }
                    _ => Decision::Deny,
                };
            }
        }
        if req.peer != Peer::Other && req.host == LOCAL_HOST && !req.proxied {
            if secrets_eq(req.pairing, &self.pairing) {
                return Decision::Allow {
                    origin: LOCAL_ORIGIN,
                };
            }
            return Decision::PairingRequired;
        }
        Decision::Deny
    }
}

/// Tailscale's CGNAT range and its ULA prefix (`fd7a:115c:a1e0::/48`).
/// Anything else is not a Serve source address we will look up.
pub(crate) fn is_tailscale_ip(value: &str) -> bool {
    if value.is_empty() || value.bytes().any(|b| b.is_ascii_whitespace() || b == b',') {
        return false;
    }
    match value.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            let [a, b, _, _] = ip.octets();
            a == 100 && (64..128).contains(&b)
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            let o = ip.octets();
            o[0] == 0xfd
                && o[1] == 0x7a
                && o[2] == 0x11
                && o[3] == 0x5c
                && o[4] == 0xa1
                && o[5] == 0xe0
        }
        Err(_) => false,
    }
}

/// Compare two secrets without returning on the first differing byte.
pub fn secrets_eq(presented: &str, expected: &str) -> bool {
    let a = presented.as_bytes();
    let b = expected.as_bytes();
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0 && !expected.is_empty()
}

fn check_pairing(pairing: &str) -> Result<(), String> {
    if pairing.len() == 64 && pairing.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Pairing secret must be 64 hex characters".into())
    }
}

pub fn pairing_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("pairing"))
}

pub fn load_or_create() -> Result<String, String> {
    load_or_create_at(&config_dir()?)
}

fn config_dir() -> Result<PathBuf, String> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("patch-remote"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config").join("patch-remote"))
}

pub(crate) fn load_or_create_at(dir: &Path) -> Result<String, String> {
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|e| e.to_string())?;
    }
    let dir_meta = std::fs::metadata(dir).map_err(|e| e.to_string())?;
    if !dir_meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    owner_writable_only(&dir_meta, dir)?;
    let path = dir.join("pairing");
    if !path.exists() {
        let mut random = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random))
            .map_err(|e| e.to_string())?;
        let secret = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        write_secret(&path, &secret)?;
        return Ok(secret);
    }
    read_secret(&path)
}

fn owner_writable_only(meta: &std::fs::Metadata, path: &Path) -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid {
        return Err(format!("{} is not owned by this user", path.display()));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(format!(
            "{} is group or world writable (mode {mode:o}); refusing to keep a pairing secret there",
            path.display()
        ));
    }
    Ok(())
}

fn write_secret(path: &Path, secret: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut file, secret.as_bytes()).map_err(|e| e.to_string())?;
    let mut perms = file.metadata().map_err(|e| e.to_string())?.permissions();
    perms.set_mode(0o600);
    file.set_permissions(perms).map_err(|e| e.to_string())?;
    let mode = std::fs::metadata(path).map_err(|e| e.to_string())?.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "could not keep {} owner-only (mode {mode:o})",
            path.display()
        ));
    }
    Ok(())
}

fn read_secret(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid {
        return Err(format!("{} is not owned by this user", path.display()));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} must be mode 0600 (mode {mode:o}); other users must not read the pairing secret",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let secret = text.trim();
    check_pairing(secret)?;
    Ok(secret.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const PAIR: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const ORIGIN: &str = "https://studio.tail123.ts.net:8124";

    fn phone() -> Access {
        Access::tailscale(ORIGIN.into(), "dj@example.com".into(), PAIR.into()).unwrap()
    }

    fn req<'a>(
        host: &'a str,
        forwarded: &'a str,
        pairing: &'a str,
        proxied: bool,
        funnel: bool,
        peer: Peer,
    ) -> RequestMeta<'a> {
        RequestMeta {
            host,
            forwarded_for: forwarded,
            pairing,
            proxied,
            funnel,
            peer,
        }
    }

    fn decide<'a>(
        access: &'a Access,
        meta: RequestMeta<'_>,
        whois_login: Result<&str, ()>,
    ) -> (Decision<'a>, usize) {
        let calls = Cell::new(0);
        let decision = access.authorize(&meta, |ip| {
            calls.set(calls.get() + 1);
            assert!(is_tailscale_ip(ip), "{ip} was looked up");
            whois_login
                .map(str::to_owned)
                .map_err(|()| "whois unavailable".into())
        });
        (decision, calls.get())
    }

    #[test]
    fn remote_requires_exact_https_host_and_account() {
        let access = phone();
        assert_eq!(
            access.page_origin("studio.tail123.ts.net:8124"),
            Some(ORIGIN)
        );
        assert_eq!(access.page_origin(LOCAL_HOST), Some(LOCAL_ORIGIN));
        assert!(access.page_origin("evil.example").is_none());
        for origin in [
            "http://studio.tail123.ts.net",
            "https://evil.example",
            "https://studio.tail123.ts.net/path",
            "https://studio.tail123.ts.net:abc",
        ] {
            assert!(
                Access::tailscale(origin.into(), "dj@example.com".into(), PAIR.into()).is_err()
            );
        }
        assert!(Access::local("short".into()).is_err());
        assert!(!secrets_eq("", ""));
        assert!(!secrets_eq("", PAIR));
        assert!(secrets_eq(PAIR, PAIR));
        assert!(!secrets_eq(&PAIR[..63], PAIR));
    }

    #[test]
    fn pairing_authorizes_only_a_clean_localhost_request() {
        let access = Access::local(PAIR.into()).unwrap();
        let (ok, calls) = decide(
            &access,
            req(LOCAL_HOST, "", PAIR, false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(
            ok,
            Decision::Allow {
                origin: LOCAL_ORIGIN
            }
        );
        assert_eq!(calls, 0);
        let (missing, _) = decide(
            &access,
            req(LOCAL_HOST, "", "", false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(missing, Decision::PairingRequired);
        let (wrong, _) = decide(
            &access,
            req(LOCAL_HOST, "", &"ff".repeat(32), false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(wrong, Decision::PairingRequired);
        let (proxied, calls) = decide(
            &access,
            req(LOCAL_HOST, "100.64.0.2", PAIR, true, false, Peer::Local),
            Ok("dj@example.com"),
        );
        assert_eq!(proxied, Decision::Deny);
        assert_eq!(calls, 0, "a local process must not trigger whois");
    }

    #[test]
    fn allowlist_is_whois_of_a_tailscaled_peer() {
        let access = phone();
        let (ok, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                "",
                true,
                false,
                Peer::Tailscaled,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(ok, Decision::PairingRequired);
        assert_eq!(calls, 1);

        // Presenting the pairing secret does not bypass whois: a tailnet Host
        // is decided by whois alone, and a secret proves nothing there.
        let (spoofed, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                PAIR,
                true,
                false,
                Peer::Local,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(spoofed, Decision::Allow { origin: ORIGIN });
        assert_eq!(calls, 1);

        // With whois unavailable the secret does not rescue it.
        let (down, _) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                PAIR,
                true,
                false,
                Peer::Local,
            ),
            Err(()),
        );
        assert_eq!(down, Decision::Deny, "pairing does not bypass whois");

        let (other, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                "",
                true,
                false,
                Peer::Tailscaled,
            ),
            Ok("other@example.com"),
        );
        assert_eq!(other, Decision::Deny);
        assert_eq!(calls, 1);

        let (down, _) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                PAIR,
                true,
                false,
                Peer::Tailscaled,
            ),
            Err(()),
        );
        assert_eq!(down, Decision::Deny, "pairing does not bypass whois");

        for forwarded in [
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "100.64.0.2, 10.0.0.1",
            "",
        ] {
            let (denied, calls) = decide(
                &access,
                req(
                    "studio.tail123.ts.net:8124",
                    forwarded,
                    "",
                    true,
                    false,
                    Peer::Tailscaled,
                ),
                Ok("dj@example.com"),
            );
            assert_eq!(denied, Decision::Deny, "{forwarded}");
            assert_eq!(calls, 0, "{forwarded}");
        }

        let (v6, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "fd7a:115c:a1e0::12",
                "",
                true,
                false,
                Peer::Tailscaled,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(v6, Decision::PairingRequired);
        assert_eq!(calls, 1);

        let (funnel, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                "",
                true,
                true,
                Peer::Tailscaled,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(funnel, Decision::Deny);
        assert_eq!(calls, 0);
        assert!(!access.local_shell(&req(LOCAL_HOST, "", PAIR, false, true, Peer::Local)));
        let (local, calls) = decide(
            &access,
            req(LOCAL_HOST, "", PAIR, false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(
            local,
            Decision::Allow {
                origin: LOCAL_ORIGIN
            }
        );
        assert_eq!(calls, 0, "localhost pairing does not ask whois");
    }

    /// On loopback there is no peer process, so a served request and a local
    /// client are indistinguishable at the socket level. Both are `Local`,
    /// and both paths require pairing. A tailnet Host additionally requires
    /// whois. Copying a known phone address must not issue a control token.
    #[test]
    fn forged_phone_headers_still_require_pairing() {
        let access = phone();
        let (ok, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                "",
                true,
                false,
                Peer::Local,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(ok, Decision::PairingRequired);
        assert_eq!(calls, 1);

        // The wrong account is still refused.
        let (other, _) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "100.64.0.2",
                "",
                true,
                false,
                Peer::Local,
            ),
            Ok("other@example.com"),
        );
        assert_eq!(other, Decision::Deny);

        // A non-Tailscale forwarded address never reaches whois.
        let (foreign, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "8.8.8.8",
                "",
                true,
                false,
                Peer::Local,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(foreign, Decision::Deny);
        assert_eq!(calls, 0);

        // Forged headers alone prove nothing: with no tailnet address there
        // is nothing for whois to confirm.
        let (spoofed, calls) = decide(
            &access,
            req(
                "studio.tail123.ts.net:8124",
                "",
                "",
                true,
                false,
                Peer::Local,
            ),
            Ok("dj@example.com"),
        );
        assert_eq!(spoofed, Decision::Deny);
        assert_eq!(calls, 0);
    }

    /// A localhost request needs the pairing secret; a tailnet Host with the
    /// secret still has to satisfy whois.
    #[test]
    fn localhost_still_needs_the_pairing_secret() {
        let access = phone();
        let (ok, calls) = decide(
            &access,
            req(LOCAL_HOST, "", PAIR, false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(
            ok,
            Decision::Allow {
                origin: LOCAL_ORIGIN
            }
        );
        assert_eq!(calls, 0, "localhost pairing does not ask whois");

        let (missing, _) = decide(
            &access,
            req(LOCAL_HOST, "", "", false, false, Peer::Local),
            Err(()),
        );
        assert_eq!(missing, Decision::PairingRequired);

        // The shell loads so the browser can ask for the code.
        assert!(access.local_shell(&req(LOCAL_HOST, "", "", false, false, Peer::Local)));
        // But a proxied localhost request is not a clean local shell.
        assert!(!access.local_shell(&req(
            LOCAL_HOST,
            "100.64.0.2",
            PAIR,
            true,
            false,
            Peer::Local
        )));
    }

    /// A peer we positively identified as a non-tailscaled process is refused
    /// on both paths, even with the pairing secret and a tailnet Host.
    #[test]
    fn a_identified_non_tailscaled_peer_is_refused() {
        let access = phone();
        for host in [LOCAL_HOST, "studio.tail123.ts.net:8124"] {
            let (denied, _) = decide(
                &access,
                req(host, "100.64.0.2", PAIR, true, false, Peer::Other),
                Ok("dj@example.com"),
            );
            assert_eq!(denied, Decision::Deny, "{host}");
        }
        assert!(!access.local_shell(&req(LOCAL_HOST, "", "", false, false, Peer::Other)));
    }

    #[test]
    fn pairing_file_is_owner_only_and_reused() {
        let dir = std::env::temp_dir().join(format!(
            "patch-remote-pairing-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let first = load_or_create_at(&dir).unwrap();
        let second = load_or_create_at(&dir).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        let path = dir.join("pairing");
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(&dir).unwrap().mode() & 0o777;
        assert_eq!(dir_mode & 0o022, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_readable_secret_and_writable_directory_are_refused() {
        let dir = std::env::temp_dir().join(format!(
            "patch-remote-pairing-open-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let mut dir_perms = std::fs::metadata(&dir).unwrap().permissions();
        dir_perms.set_mode(0o700);
        std::fs::set_permissions(&dir, dir_perms).unwrap();
        let secret = "ab".repeat(32);
        let path = dir.join("pairing");
        std::fs::write(&path, &secret).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        let err = load_or_create_at(&dir).unwrap_err();
        assert!(!err.contains(&secret));
        assert!(err.contains("0600"), "{err}");

        let open = std::env::temp_dir().join(format!(
            "patch-remote-pairing-dir-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&open);
        std::fs::create_dir(&open).unwrap();
        let mut perms = std::fs::metadata(&open).unwrap().permissions();
        perms.set_mode(0o777);
        std::fs::set_permissions(&open, perms).unwrap();
        let err = load_or_create_at(&open).unwrap_err();
        assert!(err.contains("writable"), "{err}");
        assert!(!open.join("pairing").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&open);
    }
}
