//! The engine's control socket: one thread per connection, JSON lines in
//! and out (requests are answered by `handle` in the daemon), the program
//! capture stream, and per-request statistics ({"op":"requests"}).
//!
//! Same-uid peers are trusted (the socket is mode 0600). Each connection
//! records `SO_PEERCRED`. A request line must finish within
//! `REQUEST_BUDGET` of its first byte and cannot exceed `REQUEST_LIMIT`.

use super::*;

/// What clients ask of the engine: per op, how often and how long it took
/// (the Performance Unit reads it with {"op":"requests"}).
#[derive(Default)]
struct RequestStats {
    connections: u64,
    /// op → (count, wall ns, CPU ns): wall − CPU is time spent waiting.
    ops: std::collections::HashMap<String, (u64, u64, u64)>,
}

fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid timespec out-pointer; the clock id is a constant.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

static REQUESTS: std::sync::LazyLock<Mutex<RequestStats>> =
    std::sync::LazyLock::new(Default::default);

fn requests_json() -> Value {
    let r = REQUESTS.lock().unwrap();
    let mut ops: Vec<(&String, &(u64, u64, u64))> = r.ops.iter().collect();
    ops.sort_by_key(|o| std::cmp::Reverse(o.1.2));
    json!({
        "connections": r.connections,
        "spawned": *crate::links::SPAWNED.lock().unwrap(),
        "ops": ops.iter().map(|(k, (n, ns, cpu))| json!({"op": k, "count": n, "ns": ns, "cpu_ns": cpu})).collect::<Vec<_>>(),
    })
}

/// Connections served at once: each UI window keeps one or two, a CLI call
/// one, the remote one; more is a client leaking them.
const MAX_CONNECTIONS: usize = 64;

pub(super) fn control(listener: UnixListener, s: Arc<Shared>) {
    static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    struct Live;
    impl Drop for Live {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
    for stream in listener.incoming().flatten() {
        if LIVE.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            LIVE.fetch_sub(1, Ordering::AcqRel);
            let mut stream = stream;
            let _ = crate::client::write_line(&mut stream, &json!({ "err": "too many connections" }));
            continue;
        }
        let live = Live;
        REQUESTS.lock().unwrap().connections += 1;
        let s = s.clone();
        // One short-lived thread per connection: a slow client can't stall
        // another (the UI polls meters while a CLI call is in flight).
        // A failed spawn drops the closure, and with it the `Live` guard.
        let _ = std::thread::Builder::new().name("patch-engine-client".into()).spawn(move || {
            let _live = live;
            record_peer(&stream);
            let Ok(read) = stream.try_clone() else { return };
            let mut reader = crate::ipc_line::LineReader::new(read);
            let mut stream = stream;
            // Several requests per connection are fine; the UI keeps one open.
            // Idle until the first byte; the rest of that line has a deadline.
            loop {
                let line = match reader.read_line(
                    crate::ipc_line::REQUEST_LIMIT,
                    crate::ipc_line::REQUEST_BUDGET,
                    true,
                ) {
                    Ok(line) => line,
                    Err(crate::ipc_line::LineError::TooLarge) => {
                        let _ = crate::client::write_line(&mut stream, &json!({ "err": "request too large" }));
                        break;
                    }
                    Err(_) => break,
                };
                let (t0, c0) = (std::time::Instant::now(), thread_cpu_ns());
                let mut op = None;
                let reply = match serde_json::from_str::<Value>(&line) {
                    Ok(req) if req["op"] == "capture_program" => {
                        if s.retired.load(Ordering::Acquire) {
                            return;
                        }
                        wake(&s);
                        let _ = crate::capture::stream(&s.params, stream);
                        return;
                    }
                    Ok(req) if req["op"] == "requests" => json!({ "ok": requests_json() }),
                    // A retired engine serves only its handover: any other
                    // request closes the connection, so the client connects
                    // again, to the successor (a kept connection went on
                    // talking to the old engine).
                    Ok(req)
                        if s.retired.load(Ordering::Acquire)
                            && !matches!(
                                req["op"].as_str(),
                                Some("unretire" | "fade" | "quit" | "retire")
                            ) =>
                    {
                        return;
                    }
                    Ok(req) => {
                        op = Some(req["op"].as_str().unwrap_or("?").to_string());
                        match handle(&s, &req) {
                            Ok(v) => json!({ "ok": v }),
                            Err(e) => json!({ "err": e }),
                        }
                    }
                    Err(e) => json!({ "err": format!("bad request: {e}") }),
                };
                let sent = crate::client::write_line(&mut stream, &reply).is_ok();
                if let Some(op) = op {
                    let (ns, cpu) = (
                        t0.elapsed().as_nanos() as u64,
                        thread_cpu_ns().saturating_sub(c0),
                    );
                    let mut st = REQUESTS.lock().unwrap();
                    let e = st.ops.entry(op).or_default();
                    e.0 += 1;
                    e.1 += ns;
                    e.2 += cpu;
                }
                if !sent {
                    return;
                }
            }
        });
    }
}

/// Log the connecting process. A different uid is still accepted: the
/// socket mode is the trust boundary, and this is the record of who crossed it.
fn record_peer(stream: &UnixStream) {
    match crate::ipc_line::peer_cred(stream) {
        Some(cred) => eprintln!(
            "engine: client connected pid={} uid={} gid={}",
            cred.pid, cred.uid, cred.gid
        ),
        None => eprintln!("engine: client connected, peer credentials unavailable"),
    }
}
