//! Bounded, coalescing background reads. User commands retain their own
//! connection and ordering; slow telemetry never queues in front of them.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

pub struct Reply {
    pub at: Instant,
    pub requested: Instant,
    pub result: Result<Value, String>,
}

#[derive(Default)]
struct State {
    pending: VecDeque<(&'static str, Value, Instant)>,
    replies: HashMap<&'static str, Reply>,
}

struct Worker {
    wake: mpsc::SyncSender<()>,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
pub struct Poller {
    worker: RefCell<Option<Worker>>,
}

impl Poller {
    /// False means the request was not accepted; callers keep edge-triggered
    /// flags until accepted. Only queued polls for the same key are replaced.
    pub fn request(&self, key: &'static str, request: Value) -> bool {
        // Tests never reach the user's live engine: a test window's click
        // on Hush once silenced the Tidal that was playing.
        #[cfg(test)]
        return self.request_with(key, request, |_| Err("no engine in tests".into()));
        #[cfg(not(test))]
        self.request_with(key, request, patch_engine::client::call)
    }

    pub(super) fn request_with(
        &self,
        key: &'static str,
        request: Value,
        call: impl Fn(&Value) -> Result<Value, String> + Send + 'static,
    ) -> bool {
        let mut worker = self.worker.borrow_mut();
        if worker.is_none() {
            let state = Arc::new(Mutex::new(State::default()));
            let shared = state.clone();
            let (wake, rx) = mpsc::sync_channel(1);
            let started = std::thread::Builder::new()
                .name("patch-ui-poll".into())
                .spawn(move || {
                    while rx.recv().is_ok() {
                        loop {
                            let next = shared.lock().unwrap().pending.pop_front();
                            let Some((key, request, requested)) = next else {
                                break;
                            };
                            // No mailbox lock during socket I/O.
                            let result = call(&request);
                            shared.lock().unwrap().replies.insert(
                                key,
                                Reply {
                                    at: Instant::now(),
                                    requested,
                                    result,
                                },
                            );
                        }
                    }
                });
            if started.is_err() {
                return false;
            }
            *worker = Some(Worker { wake, state });
        }
        let w = worker.as_ref().unwrap();
        let Ok(mut state) = w.state.try_lock() else {
            return false;
        };
        if let Some((_, queued, at)) = state.pending.iter_mut().find(|(k, _, _)| *k == key) {
            let reset = queued["reset"].as_bool() == Some(true);
            *queued = request;
            *at = Instant::now();
            if reset {
                queued["reset"] = Value::Bool(true);
            }
        } else {
            if state.pending.len() >= 96 {
                return false;
            }
            state.pending.push_back((key, request, Instant::now()));
        }
        drop(state);
        !matches!(
            w.wake.try_send(()),
            Err(mpsc::TrySendError::Disconnected(_))
        )
    }

    /// Never wait on the worker, even while it publishes a reply.
    pub fn take(&self, key: &'static str) -> Option<Reply> {
        let worker = self.worker.borrow();
        worker.as_ref()?.state.try_lock().ok()?.replies.remove(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_reads_do_not_block_and_queued_reset_survives_coalescing() {
        let p = Poller::default();
        let (started, start_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (seen, seen_rx) = mpsc::channel();
        assert!(
            p.request_with("analyzer", serde_json::json!({"reset": false}), move |v| {
                seen.send(v.clone()).unwrap();
                if v["reset"] == false {
                    started.send(()).unwrap();
                    release_rx.recv().unwrap();
                }
                Ok(v.clone())
            })
        );
        start_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(p.take("analyzer").is_none());
        assert!(p.request("analyzer", serde_json::json!({"reset": true})));
        assert!(p.request("analyzer", serde_json::json!({"reset": false})));
        release.send(()).unwrap();
        assert_eq!(
            seen_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()["reset"],
            false
        );
        assert_eq!(
            seen_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()["reset"],
            true
        );
    }
}
