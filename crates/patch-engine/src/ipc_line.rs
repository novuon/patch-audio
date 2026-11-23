//! Bounded JSON lines on the engine socket.
//!
//! The socket is mode 0600. A peer that can connect is the same user, and
//! that remains the trust boundary: credentials are recorded, not used as
//! a second check. What a same-uid peer must not be able to do is hold a
//! connection thread forever. Once a line has started, it has to finish
//! within a fixed budget, and it cannot grow past the cap. An idle kept
//! connection (a window polls on one socket) is not on that clock.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Longest request the engine will read. A client that never ends its line
/// cannot make the engine hold more than this.
pub(crate) const REQUEST_LIMIT: usize = 16 * 1024 * 1024;
/// Longest reply a client will accept. The remote uses the same cap.
pub(crate) const REPLY_LIMIT: usize = 1024 * 1024;
/// After the first byte of a request, the rest of the line must arrive
/// within this. Idle time before that byte does not count.
pub(crate) const REQUEST_BUDGET: Duration = Duration::from_millis(800);

#[derive(Debug)]
pub(crate) enum LineError {
    /// The peer closed before any byte of this line.
    Closed,
    /// The peer closed after a partial line.
    Incomplete,
    /// The line reached `limit` with no newline.
    TooLarge,
    /// The budget elapsed before the line finished.
    TimedOut,
    /// The line was not UTF-8.
    BadUtf8,
    Io(io::Error),
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineError::Closed => write!(f, "the engine closed the connection"),
            LineError::Incomplete => write!(f, "incomplete engine line"),
            LineError::TooLarge => write!(f, "engine line too large"),
            LineError::TimedOut => write!(f, "engine I/O deadline"),
            LineError::BadUtf8 => write!(f, "malformed engine line"),
            LineError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl From<io::Error> for LineError {
    fn from(error: io::Error) -> Self {
        LineError::Io(error)
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

/// Credentials of the process on the other end of a Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PeerCred {
    pub pid: libc::pid_t,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

/// `SO_PEERCRED` for an accepted engine connection. Failure leaves the
/// connection up: same-uid trust is the socket mode, not this lookup.
pub(crate) fn peer_cred(stream: &UnixStream) -> Option<PeerCred> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` is a live `ucred` and `len` is its size. getsockopt
    // writes at most `len` bytes into that buffer.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 || (len as usize) < std::mem::size_of::<libc::ucred>() {
        return None;
    }
    Some(PeerCred {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// One socket, plus any bytes already taken that belong to a later line.
pub(crate) struct LineReader {
    stream: UnixStream,
    pending: Vec<u8>,
}

impl LineReader {
    pub(crate) fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            pending: Vec::new(),
        }
    }

    /// Read one newline-terminated line.
    ///
    /// When `wait_for_first_byte` is set, the call blocks with no deadline
    /// until the peer sends something or hangs up. The budget starts then.
    /// Otherwise the budget covers the wait for the first byte as well
    /// (a client waiting for a reply).
    pub(crate) fn read_line(
        &mut self,
        limit: usize,
        budget: Duration,
        wait_for_first_byte: bool,
    ) -> Result<String, LineError> {
        let mut deadline = (!wait_for_first_byte).then(|| Instant::now() + budget);
        // Bytes already searched for the newline: each read scans only what
        // it added (a 70 KB reply in 1 KB reads was searched 70 times over).
        let mut scanned = 0;
        let mut timeout_set: Option<Option<Duration>> = None;
        loop {
            if let Some(end) = self.pending[scanned..].iter().position(|b| *b == b'\n') {
                let mut line = std::mem::take(&mut self.pending);
                self.pending = line.split_off(scanned + end + 1);
                return String::from_utf8(line).map_err(|_| LineError::BadUtf8);
            }
            scanned = self.pending.len();
            if self.pending.len() >= limit {
                return Err(LineError::TooLarge);
            }
            if deadline.is_none() && !self.pending.is_empty() {
                deadline = Some(Instant::now() + budget);
            }
            let timeout = match deadline {
                None => None,
                Some(at) => Some(
                    at.checked_duration_since(Instant::now())
                        .filter(|left| !left.is_zero())
                        .ok_or(LineError::TimedOut)?,
                ),
            };
            // One syscall when the deadline changes, not one per read
            // (a deadline's remaining time always changes; no deadline never).
            if timeout.is_some() || timeout_set != Some(None) {
                self.stream.set_read_timeout(timeout)?;
                timeout_set = Some(timeout);
            }
            let mut chunk = [0u8; 16 * 1024];
            let max = chunk.len().min(limit - self.pending.len());
            let n = match self.stream.read(&mut chunk[..max]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if is_timeout(&error) => {
                    if deadline.is_some() {
                        return Err(LineError::TimedOut);
                    }
                    continue;
                }
                Err(error) => return Err(LineError::Io(error)),
                Ok(n) => n,
            };
            if n == 0 {
                return Err(if self.pending.is_empty() {
                    LineError::Closed
                } else {
                    LineError::Incomplete
                });
            }
            if deadline.is_none() {
                deadline = Some(Instant::now() + budget);
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pair() -> (UnixStream, LineReader) {
        let (client, server) = UnixStream::pair().unwrap();
        (client, LineReader::new(server))
    }

    #[test]
    fn budgets_match_the_ipc_policy() {
        assert_eq!(REQUEST_LIMIT, 16 * 1024 * 1024);
        assert_eq!(REPLY_LIMIT, 1024 * 1024);
        assert_eq!(REQUEST_BUDGET, Duration::from_millis(800));
    }

    #[test]
    fn a_line_and_the_exact_cap_roundtrip() {
        let (mut client, mut reader) = pair();
        client.write_all(b"abcd\n").unwrap();
        assert_eq!(
            reader.read_line(5, Duration::from_secs(1), false).unwrap(),
            "abcd\n"
        );
        client.write_all(b"one\ntwo\n").unwrap();
        assert_eq!(
            reader.read_line(16, Duration::from_secs(1), false).unwrap(),
            "one\n"
        );
        assert_eq!(
            reader.read_line(16, Duration::from_secs(1), false).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn oversized_partial_closed_and_bad_utf8_are_rejected() {
        let (mut client, mut reader) = pair();
        client.write_all(b"abcdef").unwrap();
        drop(client);
        assert!(matches!(
            reader.read_line(5, Duration::from_secs(1), false),
            Err(LineError::TooLarge)
        ));

        let (client, mut reader) = pair();
        drop(client);
        assert!(matches!(
            reader.read_line(16, Duration::from_secs(1), false),
            Err(LineError::Closed)
        ));

        let (mut client, mut reader) = pair();
        client.write_all(b"hi").unwrap();
        drop(client);
        assert!(matches!(
            reader.read_line(16, Duration::from_secs(1), false),
            Err(LineError::Incomplete)
        ));

        let (mut client, mut reader) = pair();
        client.write_all(b"\xff\n").unwrap();
        assert!(matches!(
            reader.read_line(16, Duration::from_secs(1), false),
            Err(LineError::BadUtf8)
        ));
    }

    #[test]
    fn idle_wait_is_not_cut_by_the_request_budget() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = LineReader::new(server);
            reader.read_line(64, Duration::from_millis(40), true)
        });
        std::thread::sleep(Duration::from_millis(80));
        client.write_all(b"{\"op\":\"ping\"}\n").unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), "{\"op\":\"ping\"}\n");
    }

    #[test]
    fn a_trickle_cannot_stretch_the_read_budget() {
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = LineReader::new(server);
            reader.read_line(1024, Duration::from_millis(40), true)
        });
        let _ = client.write_all(b"x");
        for _ in 0..30 {
            if client.write_all(b"y").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(worker.join().unwrap(), Err(LineError::TimedOut)));
    }

    #[test]
    fn a_split_line_inside_the_budget_is_kept() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = LineReader::new(server);
            reader.read_line(16, Duration::from_millis(200), false)
        });
        client.write_all(b"ab").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        client.write_all(b"c\n").unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), "abc\n");
    }

    #[test]
    fn socketpair_peer_cred_is_this_process() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let cred = peer_cred(&stream).expect("SO_PEERCRED");
        // SAFETY: getuid has no preconditions.
        assert_eq!(cred.uid, unsafe { libc::getuid() });
        assert_eq!(cred.pid, std::process::id() as libc::pid_t);
    }
}
