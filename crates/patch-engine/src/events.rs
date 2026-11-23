//! Audio graph events: wake when PipeWire's devices, streams or links
//! change, instead of polling. Polling with `pactl`/`pw-link` twice a
//! second cost ~20 processes and ~90 PulseAudio client events a second,
//! much of it counted against pipewire-pulse.
//!
//! Two long-lived listeners feed one channel: `pactl subscribe` (devices,
//! streams, modules, the default output) and `pw-link -m -l` (links).
//! They die with the thread that started them (PR_SET_PDEATHSIG).

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

/// Which changes wake the watcher.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Interest {
    /// Devices and streams coming and going, modules, the default output,
    /// links: what the engine's link keeper maintains.
    Structure,
    /// Also volume and mute changes (what the Mixer shows).
    Everything,
}

/// Does a `pactl subscribe` line matter? Clients (every `pactl` run is one)
/// never do.
pub fn relevant(line: &str, interest: Interest) -> bool {
    if line.contains(" on client ") || !line.starts_with("Event ") {
        return false;
    }
    if interest == Interest::Everything {
        return true;
    }
    // Recording streams (source-output) come and go with other programs'
    // microphones; nothing the engine keeps depends on them.
    let structural = (line.starts_with("Event 'new'") || line.starts_with("Event 'remove'"))
        && !line.contains(" on source-output ");
    structural || line.contains(" on server")
}

/// Wake the engine's watcher now (a request changed what it maintains).
pub fn kick() {
    if let Some(tx) = KICK.get() {
        let _ = tx.lock().unwrap().send(());
    }
}

static KICK: std::sync::OnceLock<std::sync::Mutex<Sender<()>>> = std::sync::OnceLock::new();

/// Does a `pw-link -m -l` line matter? A change (+/-), and for the engine
/// only one that involves Patch: its own nodes, or the bus's monitor (a
/// recorder taking desktop audio, moved onto the program at once).
pub fn link_relevant(line: &str, interest: Interest) -> bool {
    (line.starts_with('+') || line.starts_with('-'))
        && (interest == Interest::Everything
            || line.contains("patch.")
            || line.contains("patch:monitor_"))
}

pub struct Watch {
    rx: Receiver<()>,
    tx: Sender<()>,
    /// Both listeners run (else the caller polls as before).
    pub live: bool,
}

fn listen(args: &[&str], tx: Sender<()>, keep: impl Fn(&str) -> bool + Send + 'static) -> bool {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: prctl is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let Some(out) = child.stdout.take() else {
        return false;
    };
    std::thread::Builder::new()
        .name("patch-events".into())
        .spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if keep(&line) && tx.send(()).is_err() {
                    break;
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        })
        .is_ok()
}

impl Watch {
    pub fn new(interest: Interest) -> Watch {
        let (tx, rx) = channel();
        let a = listen(&["pactl", "subscribe"], tx.clone(), move |l| {
            relevant(l, interest)
        });
        // Links: the engine keeps only its own (other programs' link churn
        // woke it every few seconds); the UI's Router shows them all.
        let b = listen(&["pw-link", "-m", "-l"], tx.clone(), move |l| {
            link_relevant(l, interest)
        });
        Watch {
            rx,
            live: a && b,
            tx,
        }
    }

    /// This watcher is the one [`kick`] wakes.
    pub fn kickable(self) -> Watch {
        let _ = KICK.set(std::sync::Mutex::new(self.tx.clone()));
        self
    }

    /// Wait until something changed or `timeout` passed; true on a change.
    /// A burst of events (a device appearing brings several) counts once.
    pub fn wait(&self, timeout: Duration) -> bool {
        match self.rx.recv_timeout(timeout) {
            Ok(()) => {
                std::thread::sleep(Duration::from_millis(80));
                while self.rx.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            // The listeners are gone: behave like a poll.
            Err(RecvTimeoutError::Disconnected) => {
                std::thread::sleep(timeout.min(Duration::from_secs(1)));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_real_changes_wake() {
        use Interest::*;
        assert!(!relevant("Event 'new' on client #12", Everything));
        assert!(!relevant("Event 'change' on client #12", Structure));
        assert!(relevant("Event 'new' on sink #70", Structure));
        assert!(relevant("Event 'remove' on sink-input #9", Structure));
        assert!(relevant("Event 'change' on server #-1", Structure));
        assert!(!relevant("Event 'new' on source-output #77", Structure));
        assert!(relevant("Event 'new' on source-output #77", Everything));
        assert!(!relevant("Event 'change' on sink #70", Structure));
        assert!(relevant("Event 'change' on sink #70", Everything));
        assert!(!relevant("Got SIGINT, exiting.", Everything));
        assert!(link_relevant("+patch.engine:out_FL", Structure));
        assert!(link_relevant("-  |-> patch.deck-a:playback_FL", Structure));
        assert!(!link_relevant(
            "+alsa_capture.python3.12:input_MONO",
            Structure
        ));
        assert!(link_relevant(
            "+alsa_capture.python3.12:input_MONO",
            Everything
        ));
        assert!(!link_relevant("=patch.engine:out_FL", Structure));
        // A screen recorder taking desktop audio (the bus's monitor).
        assert!(link_relevant("+patch:monitor_FL", Structure));
        assert!(!link_relevant("+patch:playback_FL", Structure));
    }
}
