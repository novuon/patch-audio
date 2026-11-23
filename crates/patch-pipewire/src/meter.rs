//! Meter taps: a `parec` capture per metered point, decoded on a thread.
//! Only monitors of playback (sink monitors, per-stream monitors) are tapped.

use patch_model::audio::SILENCE_DB;
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const WINDOW: usize = patch_meter::spectrum::FFT_SIZE;

pub struct Tap {
    pub args: Vec<String>,
    child: Child,
    /// Max |sample| per channel since the last read, as f32 bits.
    peak: Arc<[AtomicU32; 2]>,
    /// Current VU reading per channel (VU, 0 = −18 dBFS sine), as f32 bits.
    vu: Arc<[AtomicU32; 2]>,
    /// Recent mono samples, kept only for the spectrum tap.
    history: Option<Arc<Mutex<VecDeque<f32>>>>,
    loudness: Option<Arc<Mutex<patch_meter::Loudness>>>,
    analyzer: Mutex<patch_meter::spectrum::Analyzer>,
    tp_max: std::cell::Cell<[f32; 2]>,
    rate: u32,
}

impl Tap {
    /// Taps run at the graph rate: a lower rate would under-read sample peaks.
    /// `full` adds the spectrum history and the EBU R128 loudness meter.
    pub fn start(key: &str, args: Vec<String>, rate: u32, full: bool) -> Option<Tap> {
        let spectrum = full;
        let stream_name = format!("Patch meter {key}");
        let mut child = Command::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--channels=2",
                &format!("--rate={rate}"),
                "--latency-msec=20",
                "--client-name=Patch meter",
                &format!("--stream-name={stream_name}"),
                // A meter reads what plays; it never keeps the graph and the
                // sound card running in silence (it did while a window was open).
                "--property=node.passive=true",
            ])
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let peak: Arc<[AtomicU32; 2]> = Arc::new([AtomicU32::new(0), AtomicU32::new(0)]);
        let vu_now: Arc<[AtomicU32; 2]> = Arc::new([
            AtomicU32::new((-120f32).to_bits()),
            AtomicU32::new((-120f32).to_bits()),
        ]);
        let (p, v) = (peak.clone(), vu_now.clone());
        let history = spectrum.then(|| Arc::new(Mutex::new(VecDeque::with_capacity(WINDOW))));
        let h = history.clone();
        let loudness = full.then(|| Arc::new(Mutex::new(patch_meter::Loudness::new(rate))));
        let lm = loudness.clone();
        std::thread::Builder::new()
            .name("patch-meter".into())
            .spawn(move || {
                let mut vu = patch_meter::Vu::new(rate);
                let mut buf = vec![0u8; 8192];
                let mut carry = 0usize; // bytes of an incomplete frame kept from the last read
                let mut samples: Vec<f32> = Vec::with_capacity(buf.len() / 4);
                loop {
                    let n = match out.read(&mut buf[carry..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n + carry,
                    };
                    let whole = n - n % 8;
                    samples.clear();
                    samples.extend(
                        buf[..whole]
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                    // Keep a split frame for the next read so channels stay aligned.
                    buf.copy_within(whole..n, 0);
                    carry = n - whole;

                    // Peak and energy: AVX2 assembly kernel.
                    let st = patch_meter::stats(&samples);
                    for ch in 0..2 {
                        let cur = f32::from_bits(p[ch].load(Ordering::Relaxed));
                        if st.peak[ch] > cur {
                            p[ch].store(st.peak[ch].to_bits(), Ordering::Relaxed);
                        }
                    }
                    // VU ballistics on every sample: SSE2 assembly kernel.
                    vu.process(&samples);
                    for (ch, r) in vu.reading().iter().enumerate() {
                        v[ch].store((*r as f32).to_bits(), Ordering::Relaxed);
                    }
                    // EBU R128: K-weighting and true peak in assembly.
                    if let Some(lm) = &lm {
                        lm.lock().unwrap().push(&samples);
                    }
                    if let Some(h) = &h {
                        let mut h = h.lock().unwrap();
                        h.extend(
                            samples
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|f| (f[0] + f[1]) * 0.5),
                        );
                        while h.len() > WINDOW {
                            h.pop_front();
                        }
                    }
                }
            })
            .ok()?;
        Some(Tap {
            args,
            child,
            peak,
            vu: vu_now,
            history,
            loudness,
            analyzer: Mutex::default(),
            tp_max: [f32::NEG_INFINITY; 2].into(),
            rate,
        })
    }

    /// Band levels (dBFS) of the latest 4096 samples: FFT band analyser,
    /// full-scale sine in a band = 0 dBFS.
    pub fn spectrum_db(&self, bands: usize) -> Vec<f32> {
        let Some(h) = &self.history else {
            return vec![];
        };
        let samples: Vec<f32> = h.lock().unwrap().iter().copied().collect();
        self.analyzer
            .lock()
            .unwrap()
            .bands(&samples, self.rate, bands)
    }

    /// EBU R128 reading, for taps started with `full`.
    pub fn loudness(&self) -> Option<patch_model::audio::LoudnessReading> {
        let l = self.loudness.as_ref()?;
        let mut m = l.lock().unwrap();
        let tp = m.true_peak.take();
        let mut max = self.tp_max.get();
        for ch in 0..2 {
            if tp[ch] > 0.0 {
                max[ch] = max[ch].max(20.0 * tp[ch].log10());
            }
        }
        self.tp_max.set(max);
        Some(patch_model::audio::LoudnessReading {
            momentary: m.momentary() as f32,
            short_term: m.short_term() as f32,
            integrated: m.integrated() as f32,
            range: m.loudness_range() as f32,
            true_peak_max: max[0].max(max[1]),
        })
    }

    pub fn reset_loudness(&self) {
        if let Some(l) = &self.loudness {
            l.lock().unwrap().reset();
        }
        self.tp_max.set([f32::NEG_INFINITY; 2]);
    }

    /// Current VU reading per channel.
    pub fn vu(&self) -> [f32; 2] {
        [
            f32::from_bits(self.vu[0].load(Ordering::Relaxed)),
            f32::from_bits(self.vu[1].load(Ordering::Relaxed)),
        ]
    }

    /// Peak dBFS per channel since the previous call.
    pub fn take_peak_db(&self) -> [f32; 2] {
        let mut out = [SILENCE_DB; 2];
        for (ch, o) in out.iter_mut().enumerate() {
            let v = f32::from_bits(self.peak[ch].swap(0, Ordering::Relaxed));
            if v > 1e-6 {
                *o = 20.0 * v.log10();
            }
        }
        out
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
