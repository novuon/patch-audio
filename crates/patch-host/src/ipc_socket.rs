//! Bounded single-line session transport, independent of the event loop.
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub(crate) const REQUEST_LIMIT: usize = 64 * 1024;
pub(crate) const REPLY_LIMIT: usize = 1024 * 1024;
pub(crate) const IO_BUDGET: Duration = Duration::from_millis(800);

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "session I/O deadline"))
}

/// One frame per connection. The budget covers the entire read, not each
/// individual chunk: a client sending occasional bytes cannot hold a slot.
pub(crate) fn read_line(
    stream: &mut UnixStream,
    limit: usize,
    budget: Duration,
) -> io::Result<String> {
    let deadline = Instant::now() + budget;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if bytes.len() == limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "session frame too large",
            ));
        }
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let available = chunk.len().min(limit - bytes.len());
        let n = match stream.read(&mut chunk[..available]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete session frame",
            ));
        }
        if let Some(end) = chunk[..n].iter().position(|b| *b == b'\n') {
            bytes.extend_from_slice(&chunk[..=end]);
            return String::from_utf8(bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
}

pub(crate) fn write_line(
    stream: &mut UnixStream,
    text: &str,
    limit: usize,
    budget: Duration,
) -> io::Result<()> {
    if text.len() >= limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session frame too large",
        ));
    }
    let deadline = Instant::now() + budget;
    for mut bytes in [text.as_bytes(), b"\n".as_slice()] {
        while !bytes.is_empty() {
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            let n = match stream.write(bytes) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => other?,
            };
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n..];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_roundtrip_and_exact_limit() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_line(&mut a, "ping", 5, IO_BUDGET).unwrap();
        assert_eq!(read_line(&mut b, 5, IO_BUDGET).unwrap(), "ping\n");
        assert!(write_line(&mut a, "ping", 4, IO_BUDGET).is_err());
        write_line(&mut b, "pong", REPLY_LIMIT, IO_BUDGET).unwrap();
        assert_eq!(read_line(&mut a, REPLY_LIMIT, IO_BUDGET).unwrap(), "pong\n");
    }
    #[test]
    fn rejects_oversized_partial_and_invalid_utf8_frames() {
        for bytes in [&b"12345"[..], &b"ping"[..], &b"\xff\n"[..]] {
            let (mut a, mut b) = UnixStream::pair().unwrap();
            a.write_all(bytes).unwrap();
            drop(a);
            assert!(read_line(&mut b, 5, IO_BUDGET).is_err());
        }
    }
    #[test]
    fn occasional_bytes_do_not_extend_the_read_budget() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let sender = std::thread::spawn(move || {
            for _ in 0..50 {
                if a.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let result = read_line(&mut b, 1024, Duration::from_millis(30));
        assert!(result.is_err());
        drop(b);
        sender.join().unwrap();
    }
}
