//! Fixed embedded routes and bounded HTTP workers. No filesystem URL mapping.
use crate::access::{Access, Decision, LOCAL_HOST as HOST, LOCAL_ORIGIN as ORIGIN, RequestMeta};
use crate::engine::{self, Bridge, Job};
use crate::identity::Identity;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub fn serve(bridge: Bridge, access: Access, identity: Arc<dyn Identity>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(HOST)?;
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(32);
    let rx = Arc::new(Mutex::new(rx));
    for i in 0..4 {
        let rx = rx.clone();
        let bridge = bridge.clone();
        let access = access.clone();
        let identity = identity.clone();
        std::thread::Builder::new()
            .name(format!("remote-http-{i}"))
            .spawn(move || {
                loop {
                    let stream = rx.lock().unwrap().recv();
                    let Ok(stream) = stream else { break };
                    let _ = handle(stream, &bridge, &access, identity.as_ref());
                }
            })?;
    }
    println!(
        "Patch Remote: {ORIGIN}/remote/dj ({})",
        if bridge.control {
            "local controls enabled"
        } else {
            "read only"
        }
    );
    println!(
        "Localhost API calls need the pairing secret. The control token is not issued anonymously."
    );
    if let Some(origin) = access.phone_origin() {
        println!("iPhone Auto Mix: {origin}/remote/dj (Tailscale whois)");
    }
    for stream in listener.incoming() {
        let _ = tx.try_send(stream?);
    }
    Ok(())
}
fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(name, value)| {
            (name == key)
                .then(|| {
                    let bytes = value.as_bytes();
                    let mut decoded = Vec::with_capacity(bytes.len());
                    let mut i = 0;
                    while i < bytes.len() {
                        match bytes[i] {
                            b'+' => {
                                decoded.push(b' ');
                                i += 1;
                            }
                            b'%' if i + 2 < bytes.len() => {
                                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                                i += 3;
                            }
                            b'%' => return None,
                            byte => {
                                decoded.push(byte);
                                i += 1;
                            }
                        }
                    }
                    (decoded.len() <= 512)
                        .then(|| String::from_utf8(decoded).ok())
                        .flatten()
                })
                .flatten()
        })
}
fn library_search(query: &str, collection: Option<i64>) -> Result<Value, String> {
    use patch_library::{Filter, Index, index::parse_query};
    let db = patch_library::open_quick()?;
    let collections: Vec<Value> = db
        .collections()
        .into_iter()
        .map(|(id, name, count)| json!({"id":id,"name":name,"count":count}))
        .collect();
    let tracks = db.tracks()?;
    let index = Index::new(tracks);
    let allowed = collection.map(|id| {
        db.playlist(id, &index)
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    });
    let mut filter: Filter = parse_query(query);
    filter.limit = None;
    let matches = index
        .search(&filter)
        .into_iter()
        .filter(|track| allowed.as_ref().is_none_or(|ids| ids.contains(&track.id)))
        .collect::<Vec<_>>();
    let total = matches.len();
    let rows: Vec<Value> = matches
        .into_iter()
        .take(80)
        .map(|t| {
            json!({
                "id":t.id,"title":t.title,"artist":t.artist,"album":t.album,
                "duration":t.duration,"bpm":t.bpm,"key":t.key,"plays":t.plays,
                "rating":t.rating,"missing":t.missing
            })
        })
        .collect();
    Ok(json!({"total":total,"tracks":rows,"collections":collections}))
}
fn respond(stream: &mut TcpStream, code: u16, kind: &str, data: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {code} Response\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",
        data.len()
    )?;
    stream.write_all(data)
}
fn json_response(stream: &mut TcpStream, code: u16, value: Value) -> std::io::Result<()> {
    respond(
        stream,
        code,
        "application/json",
        value.to_string().as_bytes(),
    )
}
fn handle(
    mut stream: TcpStream,
    bridge: &Bridge,
    access: &Access,
    identity: &dyn Identity,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.by_ref().take(2048).read_line(&mut line)?;
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return respond(&mut stream, 400, "text/plain", b"Invalid request");
    }
    let method = parts[0].to_owned();
    let (path, query) = parts[1].split_once('?').unwrap_or((parts[1], ""));
    let path = path.to_owned();
    let query = query.to_owned();
    let mut host = String::new();
    let mut origin = String::new();
    let mut token = String::new();
    let mut pairing = String::new();
    let mut forwarded_for = String::new();
    let mut proxied = false;
    let mut funnel = false;
    let mut length = None;
    let mut size = line.len();
    loop {
        line.clear();
        reader.by_ref().take(8193).read_line(&mut line)?;
        size += line.len();
        if size > 8192 || line.is_empty() {
            return respond(&mut stream, 431, "text/plain", b"Headers too large");
        }
        if line == "\r\n" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            return respond(&mut stream, 400, "text/plain", b"Invalid header");
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "host" => host = value.into(),
            "origin" => origin = value.into(),
            "x-patch-token" => token = value.into(),
            "x-patch-pairing" => pairing = value.into(),
            "x-forwarded-for" => {
                forwarded_for = value.into();
                proxied = true;
            }
            // Identity headers are not read. Their presence only means the
            // request is not a clean localhost browser.
            "x-forwarded-host"
            | "x-forwarded-proto"
            | "forwarded"
            | "tailscale-user-login"
            | "tailscale-user-name"
            | "tailscale-user-profile-pic"
            | "tailscale-headers-info"
            | "tailscale-app-capabilities" => proxied = true,
            "tailscale-funnel-request" => {
                funnel = true;
                proxied = true;
            }
            "content-length" => {
                if length.is_some() {
                    return respond(&mut stream, 400, "text/plain", b"Duplicate length");
                }
                length = value.parse::<usize>().ok();
                if length.is_none() {
                    return respond(&mut stream, 400, "text/plain", b"Invalid length");
                }
            }
            "transfer-encoding" => {
                return respond(&mut stream, 400, "text/plain", b"Unsupported encoding");
            }
            _ => {}
        }
    }
    let Some(page) = access.page_origin(&host) else {
        return respond(
            &mut stream,
            403,
            "text/plain",
            b"This device or account is not allowed",
        );
    };
    if !origin.is_empty() && origin != page {
        return respond(&mut stream, 403, "text/plain", b"Foreign origin denied");
    }
    let peer = identity.peer(&stream);
    let meta = RequestMeta {
        host: &host,
        forwarded_for: &forwarded_for,
        pairing: &pairing,
        proxied,
        funnel,
        peer,
    };
    let decision = access.authorize(&meta, |ip| identity.whois_login(ip));
    let granted = matches!(decision, Decision::Allow { .. });
    if (path.starts_with("/api/") || method != "GET") && !granted {
        let (code, body) = match decision {
            Decision::PairingRequired => (401, &b"Pairing required"[..]),
            Decision::Allow { .. } | Decision::Deny => {
                (403, &b"This device or account is not allowed"[..])
            }
        };
        return respond(&mut stream, code, "text/plain", body);
    }
    if method == "GET"
        && !granted
        && !matches!(decision, Decision::PairingRequired)
        && !access.local_shell(&meta)
    {
        return respond(
            &mut stream,
            403,
            "text/plain",
            b"This device or account is not allowed",
        );
    }
    let expected_origin = match decision {
        Decision::Allow { origin } => origin,
        Decision::PairingRequired | Decision::Deny => page,
    };
    if method == "GET" {
        match path.as_str() {
            "/api/state" => {
                bridge.demand.read();
                return json_response(&mut stream, 200, bridge.snapshot.read().unwrap().clone());
            }
            "/api/session" => {
                return json_response(
                    &mut stream,
                    200,
                    json!({"token":*bridge.token,"control":bridge.control}),
                );
            }
            "/api/library" => {
                let search = query_param(&query, "q").unwrap_or_default();
                let collection =
                    query_param(&query, "collection").and_then(|v| v.parse::<i64>().ok());
                return match library_search(&search, collection) {
                    Ok(result) => json_response(&mut stream, 200, result),
                    Err(error) => json_response(&mut stream, 503, json!({"error":error})),
                };
            }
            "/remote/booth" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/booth.html"),
                );
            }
            "/remote/spin" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/spin.html"),
                );
            }
            "/remote/mixer" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/unit.html"),
                );
            }
            "/remote/unit" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/unit.html"),
                );
            }
            "/remote/library" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/library.html"),
                );
            }
            "/mixer.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/mixer.js"),
                );
            }
            "/mixer.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/mixer.css"),
                );
            }
            "/unit.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/unit.js"),
                );
            }
            "/unit.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/unit.css"),
                );
            }
            "/library.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/library.js"),
                );
            }
            "/library.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/library.css"),
                );
            }
            "/visual.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/visual.js"),
                );
            }
            "/visual-preview.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/visual-preview.js"),
                );
            }
            "/visual.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/visual.css"),
                );
            }
            "/navigation.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/navigation.js"),
                );
            }
            "/navigation.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/navigation.css"),
                );
            }
            "/dj.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/dj.css"),
                );
            }
            "/palette.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/palette.css"),
                );
            }
            "/booth.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/booth.js"),
                );
            }
            "/spin.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/spin.css"),
                );
            }
            "/spin.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/spin.js"),
                );
            }
            "/booth.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/booth.css"),
                );
            }
            "/" | "/remote/dj" | "/dj.html" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/dj.html"),
                );
            }
            "/remote/visualiser" | "/visualiser.html" => {
                return respond(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    include_bytes!("../../../remote/visualiser.html"),
                );
            }
            "/styles.css" => {
                return respond(
                    &mut stream,
                    200,
                    "text/css",
                    include_bytes!("../../../remote/styles.css"),
                );
            }
            "/client.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/client.js"),
                );
            }
            "/pairing.js" => {
                return respond(
                    &mut stream,
                    200,
                    "text/javascript",
                    include_bytes!("../../../remote/pairing.js"),
                );
            }
            _ => return respond(&mut stream, 404, "text/plain", b"Not found"),
        }
    }
    if method != "POST" || path != "/api/command" {
        return respond(&mut stream, 405, "text/plain", b"Method unavailable");
    }
    if !bridge.control
        || origin != expected_origin
        || !crate::access::secrets_eq(&token, &bridge.token)
    {
        return json_response(
            &mut stream,
            403,
            json!({"error":"Local control is disabled or session is invalid"}),
        );
    }
    let Some(length) = length.filter(|n| *n <= 4096) else {
        return respond(
            &mut stream,
            413,
            "text/plain",
            b"Body too large or missing length",
        );
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let body = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => return json_response(&mut stream, 400, json!({"error":e.to_string()})),
    };
    let Some(id) = body["id"].as_str().filter(|id| {
        (16..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) else {
        return json_response(
            &mut stream,
            400,
            json!({"error":"A unique command id is required"}),
        );
    };
    let id = id.to_owned();
    let request = match engine::command(&body) {
        Ok(v) => v,
        Err(e) => return json_response(&mut stream, 400, json!({"error":e})),
    };
    let (reply, rx) = mpsc::sync_channel(1);
    if bridge
        .commands
        .try_send(Job {
            request,
            id: id.clone(),
            reply,
            deadline: Instant::now() + Duration::from_secs(1),
        })
        .is_err()
    {
        return json_response(&mut stream, 503, json!({"error":"Control queue is full"}));
    }
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(value)) => json_response(
            &mut stream,
            200,
            json!({"accepted":true,"id":id,"result":value}),
        ),
        Ok(Err(error)) => json_response(&mut stream, 502, json!({"error":error})),
        Err(_) => json_response(
            &mut stream,
            504,
            json!({"error":"Result unknown; refresh state before trying again"}),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Peer;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PAIR: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    struct Script {
        peer: Peer,
        login: String,
        calls: AtomicUsize,
        seen: Mutex<String>,
    }
    impl Identity for Script {
        fn peer(&self, _: &TcpStream) -> Peer {
            self.peer
        }
        fn whois_login(&self, ip: &str) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.seen.lock().unwrap() = ip.to_owned();
            if self.login.is_empty() {
                Err("whois unavailable".into())
            } else {
                Ok(self.login.clone())
            }
        }
    }
    struct LocalPeer;
    impl Identity for LocalPeer {
        fn peer(&self, _: &TcpStream) -> Peer {
            // A loopback client has no peer process on this kernel.
            Peer::Local
        }
        fn whois_login(&self, ip: &str) -> Result<String, String> {
            panic!("whois must not run for a local peer ({ip})")
        }
    }
    fn local_access() -> Access {
        Access::local(PAIR.into()).unwrap()
    }
    fn phone_access() -> Access {
        Access::tailscale(
            "https://studio.tail123.ts.net:8124".into(),
            "dj@example.com".into(),
            PAIR.into(),
        )
        .unwrap()
    }
    fn request_with(
        bridge: Bridge,
        access: Access,
        identity: Arc<dyn Identity>,
        raw: &str,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            handle(
                listener.accept().unwrap().0,
                &bridge,
                &access,
                identity.as_ref(),
            )
        });
        client.write_all(raw.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap().unwrap();
        response
    }
    fn request(bridge: Bridge, raw: &str) -> String {
        request_with(bridge, local_access(), Arc::new(LocalPeer), raw)
    }
    fn bridge(control: bool) -> (Bridge, mpsc::Receiver<Job>) {
        let (commands, rx) = mpsc::sync_channel(1);
        (
            Bridge {
                snapshot: Arc::new(std::sync::RwLock::new(json!({"sequence":7}))),
                demand: Default::default(),
                commands,
                token: Arc::new("test-token".into()),
                control,
            },
            rx,
        )
    }
    #[test]
    fn rejects_dns_rebinding_cross_origin_and_read_only_commands() {
        let (b, _) = bridge(false);
        for req in [
            "GET /api/session HTTP/1.1\r\nHost: evil.example\r\n\r\n",
            "GET /api/session HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nOrigin: https://evil.example\r\n\r\n",
            &format!(
                "POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nOrigin: http://127.0.0.1:8124\r\nX-Patch-Token: test-token\r\nX-Patch-Pairing: {PAIR}\r\nContent-Length: 0\r\n\r\n"
            ),
        ] {
            let response = request(b.clone(), req);
            assert!(response.starts_with("HTTP/1.1 403"), "{response}");
            assert!(!response.contains("test-token"));
        }
        let denied = request(
            b.clone(),
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:8124\r\n\r\n",
        );
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        assert!(!denied.contains("sequence"));
        assert!(
            request(
                b,
                &format!("GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nX-Patch-Pairing: {PAIR}\r\n\r\n")
            )
            .contains("\"sequence\":7")
        );
    }
    #[test]
    fn anonymous_localhost_gets_the_page_but_not_the_control_token() {
        let (b, _) = bridge(true);
        let page = request(
            b.clone(),
            "GET /remote/dj HTTP/1.1\r\nHost: 127.0.0.1:8124\r\n\r\n",
        );
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("/pairing.js"));
        assert!(!page.contains("test-token"));
        for path in ["/api/session", "/api/state", "/api/library"] {
            let response = request(
                b.clone(),
                &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8124\r\n\r\n"),
            );
            assert!(response.starts_with("HTTP/1.1 401"), "{path}: {response}");
            assert!(!response.contains("test-token"), "{path}");
        }
        let session = request(
            b.clone(),
            &format!(
                "GET /api/session HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nX-Patch-Pairing: {PAIR}\r\n\r\n"
            ),
        );
        assert!(session.starts_with("HTTP/1.1 200"), "{session}");
        assert!(session.contains("\"token\":\"test-token\""));
        let stolen = request(
            b,
            "POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nOrigin: http://127.0.0.1:8124\r\nX-Patch-Token: test-token\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(stolen.starts_with("HTTP/1.1 401"), "{stolen}");
        assert!(!stolen.contains("accepted"));
    }
    #[test]
    fn authenticated_command_is_translated_and_engine_result_is_acknowledged() {
        let (b, rx) = bridge(true);
        let worker = std::thread::spawn(move || {
            let job = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(job.request, json!({"op":"ai_mode","mode":"prepare"}));
            job.reply.send(Ok(json!({"mode":"prepare"}))).unwrap();
        });
        let body = r#"{"action":"mode","value":"prepare","id":"test-command-0001"}"#;
        let response = request(
            b,
            &format!(
                "POST /api/command HTTP/1.1\r\nHost: 127.0.0.1:8124\r\nOrigin: http://127.0.0.1:8124\r\nX-Patch-Token: test-token\r\nX-Patch-Pairing: {PAIR}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"accepted\":true"));
        worker.join().unwrap();
    }
    #[test]
    fn tailscale_allowlist_follows_whois_not_the_login_header() {
        let (b, rx) = bridge(true);
        // A local process copies Tailscale's identity headers and claims a
        // tailnet address. The headers are never read: the forwarded address
        // is put to whois, which is answered by tailscaled and not by the
        // request. Here whois is down, so the request is refused.
        let spoof = Arc::new(Script {
            peer: Peer::Local,
            // An empty login makes whois fail, which is the realistic case
            // for a local process: it cannot make tailscaled answer at all.
            login: String::new(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(String::new()),
        });
        let spoofed = request_with(
            b.clone(),
            phone_access(),
            spoof.clone(),
            "GET /api/session HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\nTailscale-User-Login: dj@example.com\r\nTailscale-User-Name: DJ\r\nTailscale-Headers-Info: https://tailscale.com/s/serve-headers\r\n\r\n",
        );
        assert!(spoofed.starts_with("HTTP/1.1 403"), "{spoofed}");
        assert!(!spoofed.contains("test-token"));
        assert_eq!(
            spoof.seen.lock().unwrap().as_str(),
            "100.64.0.2",
            "the forwarded address is what gets looked up, never a header"
        );

        // A header claiming an account whois does not confirm is refused too:
        // whois answers for a different login than the one configured.
        let mismatch = Arc::new(Script {
            peer: Peer::Local,
            login: "other@example.com".into(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(String::new()),
        });
        let rejected = request_with(
            b.clone(),
            phone_access(),
            mismatch.clone(),
            "GET /api/session HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\nTailscale-User-Login: dj@example.com\r\n\r\n",
        );
        assert!(rejected.starts_with("HTTP/1.1 403"), "{rejected}");
        assert!(!rejected.contains("test-token"));
        assert_eq!(mismatch.calls.load(Ordering::SeqCst), 1);
        assert_eq!(mismatch.seen.lock().unwrap().as_str(), "100.64.0.2");

        // A forwarded address that is not a Tailscale address at all never
        // reaches whois.
        let foreign = Arc::new(Script {
            peer: Peer::Local,
            login: "dj@example.com".into(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(String::new()),
        });
        let denied = request_with(
            b.clone(),
            phone_access(),
            foreign.clone(),
            "GET /api/session HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 8.8.8.8\r\n\r\n",
        );
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
        assert_eq!(
            foreign.calls.load(Ordering::SeqCst),
            0,
            "no whois for a foreign address"
        );

        let phone = Arc::new(Script {
            peer: Peer::Local,
            login: "dj@example.com".into(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(String::new()),
        });
        let unpaired = request_with(
            b.clone(),
            phone_access(),
            phone.clone(),
            "GET /api/session HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\n\r\n",
        );
        assert!(unpaired.starts_with("HTTP/1.1 401"));
        assert!(!unpaired.contains("test-token"));
        let wrong = request_with(
            b.clone(),
            phone_access(),
            phone.clone(),
            "GET /api/state HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\nX-Patch-Pairing: wrong\r\n\r\n",
        );
        assert!(wrong.starts_with("HTTP/1.1 401"));
        let shell = request_with(
            b.clone(),
            phone_access(),
            phone.clone(),
            "GET / HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\n\r\n",
        );
        assert!(shell.starts_with("HTTP/1.1 200"));
        assert!(!shell.contains("test-token"));
        let session = request_with(
            b.clone(),
            phone_access(),
            phone.clone(),
            &format!(
                "GET /api/session HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\nTailscale-User-Login: attacker@example.com\r\nX-Patch-Pairing: {PAIR}\r\n\r\n"
            ),
        );
        assert!(session.starts_with("HTTP/1.1 200"), "{session}");
        assert!(session.contains("\"token\":\"test-token\""));
        assert_eq!(phone.calls.load(Ordering::SeqCst), 4);

        let worker = std::thread::spawn(move || {
            let job = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(job.request["op"], "ai_mode");
            job.reply.send(Ok(json!({"mode":"observe"}))).unwrap();
        });
        let body = r#"{"action":"mode","value":"observe","id":"phone-command-0001"}"#;
        let command = request_with(
            b,
            phone_access(),
            phone,
            &format!(
                "POST /api/command HTTP/1.1\r\nHost: studio.tail123.ts.net:8124\r\nOrigin: https://studio.tail123.ts.net:8124\r\nX-Forwarded-For: 100.64.0.2\r\nTailscale-User-Login: attacker@example.com\r\nX-Patch-Token: test-token\r\nX-Patch-Pairing: {PAIR}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        assert!(command.starts_with("HTTP/1.1 200"), "{command}");
        assert!(command.contains("\"accepted\":true"));
        worker.join().unwrap();
    }
}
