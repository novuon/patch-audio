//! Pulse controls run off the UI thread. A slow server retains only the latest
//! value per control; gain and mute, and different streams, never overwrite
//! each other. Routing operations deliberately stay outside this queue.
use crate::pactl::{Pactl, SinkInfo, StreamInfo};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

type Key = (u8, String);

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Write {
    StreamGain(u32, f32),
    StreamMute(u32, bool),
    SinkGain(String, f32),
    SinkMute(String, bool),
    SourceMute(String, bool),
    SystemSounds(f32, bool),
}

impl Write {
    fn key(&self) -> Key {
        match self {
            Self::StreamGain(id, _) => (0, id.to_string()),
            Self::StreamMute(id, _) => (1, id.to_string()),
            Self::SinkGain(id, _) => (2, id.clone()),
            Self::SinkMute(id, _) => (3, id.clone()),
            Self::SourceMute(id, _) => (4, id.clone()),
            Self::SystemSounds(..) => (5, String::new()),
        }
    }

    fn execute(&self, pw: &Pactl) {
        match self {
            Self::StreamGain(id, db) => pw.set_stream_volume(*id, *db),
            Self::StreamMute(id, mute) => pw.set_stream_mute(*id, *mute),
            Self::SinkGain(id, db) => pw.set_sink_volume(id, *db),
            Self::SinkMute(id, mute) => pw.set_sink_mute(id, *mute),
            Self::SourceMute(id, mute) => pw.set_source_mute(id, *mute),
            Self::SystemSounds(db, mute) => crate::system_sounds::save(*db, *mute),
        }
    }
}

#[derive(Default)]
struct State {
    pending: BTreeMap<Key, Write>,
    // Preserve the local control against snapshots begun before its write
    // completed. A later snapshot is authoritative, including external edits.
    latest: BTreeMap<Key, (Write, Option<Instant>)>,
    closed: bool,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

pub(crate) struct PulseWrites {
    shared: Shared,
}

impl Default for PulseWrites {
    fn default() -> Self {
        Self::spawn(|write| write.execute(&Pactl::new()))
    }
}

impl PulseWrites {
    fn spawn(mut execute: impl FnMut(&Write) + Send + 'static) -> Self {
        let shared: Shared = Arc::default();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("patch-pulse-controls".into())
            .spawn(move || {
                let (lock, wake) = &*worker;
                loop {
                    let batch = {
                        let mut state = lock.lock().unwrap();
                        while state.pending.is_empty() && !state.closed {
                            state = wake.wait(state).unwrap();
                        }
                        if state.pending.is_empty() && state.closed {
                            return;
                        }
                        std::mem::take(&mut state.pending)
                    };
                    for (key, write) in batch {
                        execute(&write);
                        let mut state = lock.lock().unwrap();
                        if let Some((latest, done)) = state.latest.get_mut(&key)
                            && *latest == write
                        {
                            *done = Some(Instant::now());
                        }
                    }
                }
            })
            .expect("start Pulse control worker");
        Self { shared }
    }

    pub fn post(&self, write: Write) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        let key = write.key();
        state.pending.insert(key.clone(), write.clone());
        state.latest.insert(key, (write, None));
        wake.notify_one();
    }

    pub fn reconcile(&self, read_at: Instant, sinks: &mut [SinkInfo], streams: &mut [StreamInfo]) {
        let mut state = self.shared.0.lock().unwrap();
        state.latest.retain(|_, (write, done)| {
            if done.is_some_and(|at| read_at >= at) {
                return false;
            }
            match write {
                Write::StreamGain(id, db) => {
                    if let Some(s) = streams.iter_mut().find(|s| s.index == *id) {
                        s.volume_db = *db;
                    }
                }
                Write::StreamMute(id, mute) => {
                    if let Some(s) = streams.iter_mut().find(|s| s.index == *id) {
                        s.mute = *mute;
                    }
                }
                Write::SinkGain(id, db) => {
                    if let Some(s) = sinks.iter_mut().find(|s| s.name == *id) {
                        s.volume_db = *db;
                    }
                }
                Write::SinkMute(id, mute) => {
                    if let Some(s) = sinks.iter_mut().find(|s| s.name == *id) {
                        s.mute = *mute;
                    }
                }
                _ => {}
            }
            true
        });
    }
}

impl Drop for PulseWrites {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        lock.lock().unwrap().closed = true;
        wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_stalled_server_never_blocks_a_drag_or_queues_its_history() {
        let (sent, got) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let queue = PulseWrites::spawn(move |write| {
            sent.send(write.clone()).unwrap();
            if *write == Write::StreamGain(1, 0.0) {
                wait.recv().unwrap();
            }
        });
        queue.post(Write::StreamGain(1, 0.0));
        assert_eq!(
            got.recv_timeout(Duration::from_secs(2)).unwrap(),
            Write::StreamGain(1, 0.0)
        );
        for i in 1..=1000 {
            queue.post(Write::StreamGain(1, -(i as f32)));
        }
        queue.post(Write::StreamMute(1, true));
        queue.post(Write::StreamGain(2, -6.0));
        assert_eq!(queue.shared.0.lock().unwrap().pending.len(), 3);
        release.send(()).unwrap();
        let writes: Vec<_> = (0..3)
            .map(|_| got.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        assert!(writes.contains(&Write::StreamGain(1, -1000.0)));
        assert!(writes.contains(&Write::StreamMute(1, true)));
        assert!(writes.contains(&Write::StreamGain(2, -6.0)));
    }

    #[test]
    #[ignore = "manual control-dispatch timing; no live audio writes"]
    fn dispatch_benchmark() {
        let (sent, got) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let queue = PulseWrites::spawn(move |_| {
            sent.send(()).unwrap();
            wait.recv().unwrap();
        });
        queue.post(Write::StreamGain(1, 0.0));
        got.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut times = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            let at = Instant::now();
            queue.post(Write::StreamGain(1, -(i as f32 % 60.0)));
            times.push(at.elapsed().as_nanos());
        }
        times.sort_unstable();
        println!(
            "Pulse UI dispatch: median {} ns, p99 {} ns, max {} ns; 10,000 moves, {} pending write",
            times[5000],
            times[9899],
            times[9999],
            queue.shared.0.lock().unwrap().pending.len()
        );
        // Let both the in-progress and latest coalesced write finish.
        release.send(()).unwrap();
        got.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
    }

    #[test]
    fn stale_graph_reads_cannot_pull_a_fader_back_but_external_edits_can() {
        let queue = PulseWrites {
            shared: Arc::default(),
        };
        let before = Instant::now();
        queue.post(Write::SinkGain("output".into(), -12.0));
        queue.post(Write::SinkMute("output".into(), true));
        let mut sinks = [SinkInfo {
            name: "output".into(),
            ..Default::default()
        }];
        queue.reconcile(before, &mut sinks, &mut []);
        assert_eq!(sinks[0].volume_db, -12.0);
        assert!(sinks[0].mute);
        let completed = Instant::now();
        for (_, done) in queue.shared.0.lock().unwrap().latest.values_mut() {
            *done = Some(completed);
        }
        sinks[0].volume_db = -3.0;
        queue.reconcile(before, &mut sinks, &mut []);
        assert_eq!(sinks[0].volume_db, -12.0);
        sinks[0].volume_db = -3.0;
        queue.reconcile(Instant::now(), &mut sinks, &mut []);
        assert_eq!(sinks[0].volume_db, -3.0);
        assert!(queue.shared.0.lock().unwrap().latest.is_empty());
    }
}
