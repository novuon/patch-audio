//! Control socket: one line in, one line out.

use patch_core::AppId;
use patch_core::ipc::{Request, socket_path};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "ipc_socket.rs"]
mod socket;

/// Send a request to a running session. `None` if no session is listening.
pub fn send(unit: AppId, request: &Request) -> Option<String> {
    let mut stream = UnixStream::connect(socket_path(unit)).ok()?;
    socket::write_line(
        &mut stream,
        &request.encode(),
        socket::REQUEST_LIMIT,
        socket::IO_BUDGET,
    )
    .ok()?;
    let line = socket::read_line(&mut stream, socket::REPLY_LIMIT, socket::IO_BUDGET).ok()?;
    Some(line.trim().to_string())
}

pub fn is_running(unit: AppId) -> bool {
    send(unit, &Request::Ping).is_some_and(|r| r == "pong")
}

/// Listen for requests. `handle` runs on the Slint event loop thread.
pub fn serve(unit: AppId, handle: fn(Request) -> String) -> std::io::Result<()> {
    let path = socket_path(unit);
    std::fs::create_dir_all(path.parent().unwrap())?;
    if path.exists() {
        if is_running(unit) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "session already running",
            ));
        }
        std::fs::remove_file(&path)?; // stale socket from a crashed session
    }
    let listener = UnixListener::bind(&path)?;
    patch_core::ipc::private_socket(&path);
    // One request at a time, in the order they connect (two quick
    // hotkeys stay in order); every read and write has its own deadline, so
    // a client that stalls holds the next one back by at most that.
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let Ok(line) =
                socket::read_line(&mut stream, socket::REQUEST_LIMIT, socket::IO_BUDGET)
            else {
                continue;
            };
            let reply = match Request::parse(&line) {
                Some(Request::Ping) => "pong".to_string(),
                Some(req) => {
                    let (tx, rx) = mpsc::channel();
                    // A request whose client has given up is not run late.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let _ = slint::invoke_from_event_loop(move || {
                        if Instant::now() < deadline {
                            let _ = tx.send(handle(req));
                        }
                    });
                    rx.recv_timeout(Duration::from_secs(2))
                        .unwrap_or_else(|_| "err timeout".into())
                }
                None => "err unknown request".to_string(),
            };
            let _ = socket::write_line(&mut stream, &reply, socket::REPLY_LIMIT, socket::IO_BUDGET);
        }
    });
    Ok(())
}

pub fn cleanup(unit: AppId) {
    let _ = std::fs::remove_file(socket_path(unit));
}
