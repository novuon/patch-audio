//! Program analysis, off the real-time thread: VU (IEC 60268-17), EBU R128
//! loudness with true peak (BS.1770-4), and the spectrum, all with the
//! assembly kernels of `patch-meter`, fed from the RT thread's program ring.

use crate::Params;
use patch_meter::spectrum::{Analyzer, FFT_SIZE};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Snapshot {
    /// VU per side (0 VU = reference level).
    pub vu: [f32; 2],
    pub momentary: f32,
    pub short_term: f32,
    pub integrated: f32,
    pub range: f32,
    /// Maximum true peak since reset, dBTP.
    pub true_peak_max: f32,
    /// Band levels, dBFS, low to high.
    pub spectrum: Vec<f32>,
    /// Program frames analysed so far.
    pub frames: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        let silence = patch_model::audio::SILENCE_DB;
        Self {
            vu: [silence; 2],
            momentary: silence,
            short_term: silence,
            integrated: silence,
            range: 0.0,
            true_peak_max: silence,
            spectrum: Vec::new(),
            frames: 0,
        }
    }
}

/// The Analyzer Unit's view: computed only while someone asks for it.
#[derive(Clone, Debug, Default)]
pub struct AnalyzerSnapshot {
    pub stereo: patch_meter::analyzer::StereoReading,
    pub rta: patch_meter::analyzer::RtaReading,
    /// Vectorscope points (side, mid), the last ~40 ms.
    pub scope: Vec<[f32; 2]>,
    pub rate: u32,
}

pub struct Analysis {
    pub snapshot: Mutex<Snapshot>,
    reset: std::sync::atomic::AtomicBool,
    pub analyzer: Mutex<AnalyzerSnapshot>,
    /// Milliseconds (since `epoch`) until which the Analyzer is wanted.
    analyzer_until: std::sync::atomic::AtomicU64,
    analyzer_reset: std::sync::atomic::AtomicBool,
    epoch: std::time::Instant,
}

/// The Analyzer keeps running this long after the last request.
const ANALYZER_LINGER_MS: u64 = 2000;

pub const BANDS: usize = 32;

impl Analysis {
    pub fn reset_loudness(&self) {
        self.reset.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Keep the Analyzer running (it stops ~2 s after the last call) and
    /// return its latest view.
    pub fn analyzer(&self, reset_peaks: bool) -> AnalyzerSnapshot {
        self.analyzer_until.store(
            self.now_ms() + ANALYZER_LINGER_MS,
            std::sync::atomic::Ordering::Relaxed,
        );
        if reset_peaks {
            self.analyzer_reset
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.analyzer.lock().unwrap().clone()
    }

    fn analyzer_wanted(&self) -> bool {
        self.now_ms()
            < self
                .analyzer_until
                .load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The Analyzer's working state (rebuilt on rate change or when it wakes).
struct AnalyzerState {
    stereo: patch_meter::analyzer::StereoMeter,
    rta: patch_meter::analyzer::Rta,
    scope: patch_meter::analyzer::Scope,
    rate: u32,
}

impl AnalyzerState {
    fn new(rate: u32) -> AnalyzerState {
        AnalyzerState {
            stereo: patch_meter::analyzer::StereoMeter::new(rate),
            rta: patch_meter::analyzer::Rta::new(rate),
            scope: patch_meter::analyzer::Scope::new(rate as usize / 25),
            rate,
        }
    }
}

/// Start the analysis thread. `rate` is re-read from the engine when the
/// graph rate changes (the meters are rebuilt for it).
pub fn spawn(params: Arc<Params>) -> Arc<Analysis> {
    let a = Arc::new(Analysis {
        snapshot: Mutex::default(),
        reset: false.into(),
        analyzer: Mutex::default(),
        analyzer_until: 0.into(),
        analyzer_reset: false.into(),
        epoch: std::time::Instant::now(),
    });
    let out = a.clone();
    std::thread::Builder::new()
        .name("patch-analysis".into())
        .spawn(move || run(params, out))
        .expect("analysis thread");
    a
}

fn update_spectrum_history(
    history: &mut VecDeque<f32>,
    samples: &[f32],
    resting: bool,
    was_resting: bool,
) -> bool {
    if resting {
        if !was_resting {
            history.clear();
            return true;
        }
        return false;
    }
    if samples.is_empty() {
        return false;
    }
    for frame in samples.chunks_exact(2) {
        if history.len() == FFT_SIZE {
            history.pop_front();
        }
        history.push_back((frame[0] + frame[1]) * 0.5);
    }
    true
}

fn run(params: Arc<Params>, out: Arc<Analysis>) {
    let mut rate = 0u32;
    let mut vu = patch_meter::Vu::new(48_000);
    let mut loud = patch_meter::Loudness::new(48_000);
    let mut analyzer = Analyzer::reactive();
    let mut history: VecDeque<f32> = VecDeque::with_capacity(FFT_SIZE);
    let mut spectrum_dirty = false;
    let mut spectrum_resting = false;
    let mut buf: Vec<f32> = Vec::with_capacity(1 << 17);
    let mut tp_max = [f32::NEG_INFINITY; 2];
    let mut frames = 0u64;
    let mut an: Option<AnalyzerState> = None;
    // Frames of exact digital silence in a row: past 3.5 s the meters sit
    // at their floor (VU fallen, the 3 s short-term window empty), so the
    // loudness and VU filters stop until signal comes back.
    let mut silent = 0u64;
    // No samples for a second (the graph sleeps): 10 times a second (the
    // program ring holds 1.4 s: nothing is lost when sound comes back).
    let mut empty = 0u32;
    loop {
        std::thread::sleep(Duration::from_millis(if empty > 200 { 100 } else { 5 }));
        let r = params.last_rate.load(std::sync::atomic::Ordering::Relaxed);
        if r != 0 && r != rate {
            rate = r;
            vu = patch_meter::Vu::new(rate);
            loud = patch_meter::Loudness::new(rate);
            tp_max = [f32::NEG_INFINITY; 2];
            history.clear();
            silent = 0;
            spectrum_dirty = true;
            spectrum_resting = false;
        }
        if out.reset.swap(false, std::sync::atomic::Ordering::Relaxed) {
            loud.reset();
            tp_max = [f32::NEG_INFINITY; 2];
        }
        buf.clear();
        // SAFETY: this analysis thread is the program ring's only consumer.
        unsafe { params.program.drain(|s| buf.push(s)) };
        empty = if buf.is_empty() {
            empty.saturating_add(1)
        } else {
            0
        };
        let whole = buf.len() / 2 * 2;
        let samples = &buf[..whole];
        if samples.iter().all(|s| *s == 0.0) {
            silent += (whole / 2) as u64;
        } else {
            silent = 0;
        }
        let resting = rate != 0 && silent > rate as u64 * 7 / 2;
        if !samples.is_empty() && !resting {
            vu.process(samples);
            loud.push(samples);
            frames += (whole / 2) as u64;
        }
        spectrum_dirty |= update_spectrum_history(&mut history, samples, resting, spectrum_resting);
        spectrum_resting = resting;
        // The Analyzer: only while wanted (two 16k FFTs every 43 ms are
        // not free), rebuilt fresh when it wakes or the rate changes.
        if rate != 0 && out.analyzer_wanted() {
            let st = match &mut an {
                Some(st) if st.rate == rate => st,
                _ => an.insert(AnalyzerState::new(rate)),
            };
            if out
                .analyzer_reset
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                st.rta.reset_peaks();
            }
            if !samples.is_empty() {
                st.stereo.push(samples);
                st.rta.push(samples);
                st.scope.push(samples);
            }
            let snap = AnalyzerSnapshot {
                stereo: st.stereo.reading(),
                rta: st.rta.reading(),
                scope: st.scope.points(),
                rate,
            };
            *out.analyzer.lock().unwrap() = snap;
        } else if an.is_some() {
            an = None;
            *out.analyzer.lock().unwrap() = AnalyzerSnapshot::default();
        }
        let tp = loud.true_peak.take();
        for ch in 0..2 {
            if tp[ch] > 0.0 {
                tp_max[ch] = tp_max[ch].max(20.0 * tp[ch].log10());
            }
        }
        // New audio is visible on this tick; an unchanged history needs no FFT.
        let spectrum = (rate != 0 && spectrum_dirty).then(|| {
            spectrum_dirty = false;
            analyzer.bands(history.make_contiguous(), rate, BANDS)
        });
        let reading = vu.reading();
        let mut s = out.snapshot.lock().unwrap();
        s.vu = [reading[0] as f32, reading[1] as f32];
        // Silence reads −∞ LUFS; the wire carries finite numbers only.
        let fin = |x: f64| {
            if x.is_finite() {
                (x as f32).max(-120.0)
            } else {
                -120.0
            }
        };
        s.momentary = fin(loud.momentary());
        s.short_term = fin(loud.short_term());
        s.integrated = fin(loud.integrated());
        s.range = fin(loud.loudness_range()).max(0.0);
        s.true_peak_max = tp_max[0].max(tp_max[1]).max(-120.0);
        if let Some(sp) = spectrum {
            s.spectrum = sp;
        }
        s.frames = frames;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual performance measurement"]
    fn idle_spectrum_cost() {
        let mut history = VecDeque::from(vec![0.0; FFT_SIZE]);
        let mut analyzer = Analyzer::reactive();
        for gated in [false, true] {
            let start = std::time::Instant::now();
            for _ in 0..10_000 {
                if !gated || std::hint::black_box(false) {
                    let h: Vec<f32> = history.iter().copied().collect();
                    std::hint::black_box(analyzer.bands(&h, 48_000, BANDS));
                } else {
                    std::hint::black_box(&mut history);
                }
            }
            eprintln!(
                "idle gated={gated}: {:.1} ns/tick",
                start.elapsed().as_nanos() as f64 / 10_000.0
            );
        }
    }

    #[test]
    fn spectrum_history_skips_idle_clears_silence_and_resumes() {
        let mut history = VecDeque::with_capacity(FFT_SIZE);
        assert!(!update_spectrum_history(&mut history, &[], false, false));
        assert!(update_spectrum_history(
            &mut history,
            &[1.0, 0.5],
            false,
            false
        ));
        assert_eq!(history[0], 0.75);
        assert!(!update_spectrum_history(&mut history, &[], false, false));
        assert_eq!(history[0], 0.75);
        assert!(update_spectrum_history(
            &mut history,
            &[0.0; 2],
            true,
            false
        ));
        assert!(history.is_empty());
        assert!(!update_spectrum_history(
            &mut history,
            &[0.0; 2],
            true,
            true
        ));
        assert!(update_spectrum_history(
            &mut history,
            &[0.25; 2],
            false,
            true
        ));
        let tone: Vec<f32> = (0..FFT_SIZE * 2)
            .map(|i| (i as f32 / 2.0 * std::f32::consts::TAU * 1000.0 / 48_000.0).sin())
            .collect();
        update_spectrum_history(&mut history, &tone, false, false);
        let mut analyzer = Analyzer::reactive();
        assert!(
            analyzer
                .bands(history.make_contiguous(), 48_000, BANDS)
                .iter()
                .any(|x| *x > -140.0)
        );
        let input = vec![0.5; FFT_SIZE * 4];
        update_spectrum_history(&mut history, &input, false, false);
        assert_eq!(history.len(), FFT_SIZE);
        assert_eq!(history.capacity(), FFT_SIZE);
        update_spectrum_history(&mut history, &[], true, false);
        assert!(
            analyzer
                .bands(history.make_contiguous(), 48_000, BANDS)
                .iter()
                .all(|x| *x == -140.0)
        );
    }

    #[test]
    fn the_analyzer_runs_only_while_asked_for() {
        let a = Analysis {
            snapshot: Mutex::default(),
            reset: false.into(),
            analyzer: Mutex::default(),
            analyzer_until: 0.into(),
            analyzer_reset: false.into(),
            epoch: std::time::Instant::now(),
        };
        assert!(!a.analyzer_wanted());
        let snap = a.analyzer(true);
        assert_eq!(snap.rate, 0, "nothing measured yet");
        assert!(a.analyzer_wanted());
        assert!(a.analyzer_reset.load(std::sync::atomic::Ordering::Relaxed));
        a.analyzer_until
            .store(a.now_ms(), std::sync::atomic::Ordering::Relaxed);
        assert!(!a.analyzer_wanted(), "stops after the linger time");
    }
}
