//! Keylock and key shift. Planar in and out. Used only by the deck workers
//! (never on the RT thread). Rubber Band's R3 ("finer") engine in real-time
//! mode, through its C API; tester kits build Signalsmith Stretch instead
//! (`build.rs`: Rubber Band is GPL, the kits are distributed as binaries).
//! Both sit behind the same calls, in `engine`.

use engine::State;

#[cfg(not(patch_signalsmith))]
mod engine {
    use std::ffi::c_void;

    pub type State = *mut c_void;

    // Options from rubberband-c.h (Rubber Band 4).
    pub const REALTIME: i32 = 0x0000_0001;
    pub const PITCH_HIGH_CONSISTENCY: i32 = 0x0400_0000;
    pub const CHANNELS_TOGETHER: i32 = 0x1000_0000;
    pub const ENGINE_FINER: i32 = 0x2000_0000;

    unsafe extern "C" {
        fn rubberband_new(
            rate: u32,
            channels: u32,
            options: i32,
            time_ratio: f64,
            pitch: f64,
        ) -> State;
        fn rubberband_delete(s: State);
        fn rubberband_reset(s: State);
        fn rubberband_set_time_ratio(s: State, r: f64);
        fn rubberband_set_pitch_scale(s: State, p: f64);
        fn rubberband_get_preferred_start_pad(s: State) -> u32;
        fn rubberband_get_start_delay(s: State) -> u32;
        fn rubberband_get_samples_required(s: State) -> u32;
        fn rubberband_set_max_process_size(s: State, n: u32);
        fn rubberband_process(s: State, input: *const *const f32, n: u32, last: i32);
        fn rubberband_available(s: State) -> i32;
        fn rubberband_retrieve(s: State, out: *const *mut f32, n: u32) -> u32;
    }

    pub const NAME: &str = "Rubber Band R3";

    pub fn new(rate: u32, channels: usize) -> State {
        with_options(rate, channels, options())
    }

    pub fn with_options(rate: u32, channels: usize, opts: i32) -> State {
        // SAFETY: valid arguments; freed by `delete`.
        let s = unsafe { rubberband_new(rate, channels as u32, opts, 1.0, 1.0) };
        assert!(!s.is_null());
        unsafe { rubberband_set_max_process_size(s, 4096) };
        s
    }

    /// Stereo image kept together (DJ material), pitch changes smooth while
    /// they move (tempo fader, key shift). PATCH_STRETCH (evaluation): `r2`
    /// (crisp transients) or `r3short` (R3, short window); default R3.
    fn options() -> i32 {
        const WINDOW_SHORT: i32 = 0x0010_0000;
        let base = REALTIME | CHANNELS_TOGETHER | PITCH_HIGH_CONSISTENCY;
        match std::env::var("PATCH_STRETCH").as_deref() {
            Ok("r2") => base,
            Ok("r3short") => base | ENGINE_FINER | WINDOW_SHORT,
            _ => base | ENGINE_FINER,
        }
    }

    // SAFETY (all below): `s` is a live state from `new`, used by one thread.
    pub fn delete(s: State) {
        unsafe { rubberband_delete(s) }
    }
    pub fn reset(s: State) {
        unsafe { rubberband_reset(s) }
    }
    pub fn set(s: State, time_ratio: f64, pitch: f64) {
        unsafe {
            rubberband_set_time_ratio(s, time_ratio);
            rubberband_set_pitch_scale(s, pitch);
        }
    }
    pub fn start_pad(s: State) -> usize {
        unsafe { rubberband_get_preferred_start_pad(s) as usize }
    }
    pub fn start_delay(s: State) -> usize {
        unsafe { rubberband_get_start_delay(s) as usize }
    }
    pub fn required(s: State) -> usize {
        unsafe { rubberband_get_samples_required(s) as usize }
    }
    /// SAFETY: `input` holds one pointer per channel, each to `n` samples.
    pub unsafe fn process(s: State, input: *const *const f32, n: usize) {
        unsafe { rubberband_process(s, input, n as u32, 0) }
    }
    pub fn available(s: State) -> usize {
        unsafe { rubberband_available(s).max(0) as usize }
    }
    /// SAFETY: `out` holds one pointer per channel, each with room for `n`.
    pub unsafe fn retrieve(s: State, out: *const *mut f32, n: usize) -> usize {
        unsafe { rubberband_retrieve(s, out, n as u32) as usize }
    }
}

#[cfg(patch_signalsmith)]
mod engine {
    use std::ffi::c_void;

    pub type State = *mut c_void;

    // csrc/stretch.cpp
    unsafe extern "C" {
        fn patch_stretch_new(rate: i32, channels: i32) -> State;
        fn patch_stretch_delete(s: State);
        fn patch_stretch_reset(s: State);
        fn patch_stretch_set(s: State, time_ratio: f64, pitch: f64);
        fn patch_stretch_start_pad(s: State) -> i32;
        fn patch_stretch_start_delay(s: State) -> i32;
        fn patch_stretch_required(s: State) -> i32;
        fn patch_stretch_process(s: State, input: *const *const f32, n: i32);
        fn patch_stretch_available(s: State) -> i32;
        fn patch_stretch_retrieve(s: State, out: *const *mut f32, n: i32) -> i32;
    }

    pub const NAME: &str = "Signalsmith Stretch";

    pub fn new(rate: u32, channels: usize) -> State {
        // SAFETY: valid arguments; freed by `delete`.
        let s = unsafe { patch_stretch_new(rate as i32, channels as i32) };
        assert!(!s.is_null());
        s
    }

    // SAFETY (all below): `s` is a live state from `new`, used by one thread.
    pub fn delete(s: State) {
        unsafe { patch_stretch_delete(s) }
    }
    pub fn reset(s: State) {
        unsafe { patch_stretch_reset(s) }
    }
    pub fn set(s: State, time_ratio: f64, pitch: f64) {
        unsafe { patch_stretch_set(s, time_ratio, pitch) }
    }
    pub fn start_pad(s: State) -> usize {
        unsafe { patch_stretch_start_pad(s).max(0) as usize }
    }
    pub fn start_delay(s: State) -> usize {
        unsafe { patch_stretch_start_delay(s).max(0) as usize }
    }
    pub fn required(s: State) -> usize {
        unsafe { patch_stretch_required(s).max(0) as usize }
    }
    /// SAFETY: `input` holds one pointer per channel, each to `n` samples.
    pub unsafe fn process(s: State, input: *const *const f32, n: usize) {
        unsafe { patch_stretch_process(s, input, n as i32) }
    }
    pub fn available(s: State) -> usize {
        unsafe { patch_stretch_available(s).max(0) as usize }
    }
    /// SAFETY: `out` holds one pointer per channel, each with room for `n`.
    pub unsafe fn retrieve(s: State, out: *const *mut f32, n: usize) -> usize {
        unsafe { patch_stretch_retrieve(s, out, n as i32).max(0) as usize }
    }
}

/// The time-stretcher this build uses, for logs and status.
pub const ENGINE: &str = engine::NAME;

pub struct Stretcher {
    s: State,
    rate: u32,
    ratio: Option<f64>,
    pitch: Option<f64>,
    peak: PeakLimit,
}

/// A phase vocoder moves the phases a mastering limiter lined up: a loud
/// master's peaks come out 3 to 5 dB higher (measured: 1.12 raw, 1.66
/// keylocked, 1.92 two semitones up), its loudness the same. Into the
/// master limiter that was several dB clamped on every kick: crackle in
/// every keylocked transition. So the stretcher's output is limited here,
/// with a look-ahead the deck already compensates (it's part of
/// `start_delay`) and a smooth release: the peaks only, never the level.
const CEILING: f32 = 0.891; // −1 dBFS, the master limiter's ceiling
const LOOKAHEAD_S: f64 = 0.003;
const RELEASE_S: f64 = 0.080;

struct PeakLimit {
    len: usize,
    /// Delay line (interleaved l, r): the output is `len` frames late.
    delay: Vec<[f32; 2]>,
    /// Gain each of the last `len + 1` input frames needs.
    need: Vec<f32>,
    /// The last `len` smoothed window minima, and their running sum.
    held: Vec<f32>,
    sum: f64,
    pos: usize,
    gain: f32,
    release: f32,
}

impl PeakLimit {
    fn new(rate: u32) -> PeakLimit {
        let len = ((rate as f64 * LOOKAHEAD_S).round() as usize).max(1);
        PeakLimit {
            len,
            delay: vec![[0.0; 2]; len],
            need: vec![1.0; len + 1],
            held: vec![1.0; len],
            sum: len as f64,
            pos: 0,
            gain: 1.0,
            release: (1.0 - (-1.0 / (rate as f64 * RELEASE_S)).exp()) as f32,
        }
    }

    fn reset(&mut self) {
        self.delay.fill([0.0; 2]);
        self.need.fill(1.0);
        self.held.fill(1.0);
        self.sum = self.len as f64;
        self.pos = 0;
        self.gain = 1.0;
    }

    /// In place, `len` frames late. Each input frame's window minimum (over
    /// the `len + 1` frames ending at it) is released smoothly, and the
    /// output gain is the average of the last `len` of those. Every one of
    /// them covers the frame now leaving the delay line, so the average
    /// never lets it exceed the ceiling, and the gain glides into each peak
    /// over the whole look-ahead instead of stepping at the peak (a gain
    /// step is audible distortion on every kick of a hot track).
    fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.len;
        for i in 0..l.len().min(r.len()) {
            let x = [l[i], r[i]];
            let pk = x[0].abs().max(x[1].abs());
            self.need[self.pos % (n + 1)] = if pk > CEILING { CEILING / pk } else { 1.0 };
            let window = self.need.iter().copied().fold(1.0f32, f32::min);
            self.gain = (self.gain + (1.0 - self.gain) * self.release).min(window);
            let slot = self.pos % n;
            self.sum += f64::from(self.gain - self.held[slot]);
            self.held[slot] = self.gain;
            let g = ((self.sum / n as f64) as f32).min(1.0);
            let out = std::mem::replace(&mut self.delay[slot], x);
            l[i] = out[0] * g;
            r[i] = out[1] * g;
            self.pos += 1;
            if self.pos.is_multiple_of(n * (n + 1) * 64) {
                // Keep the running sum exact over hours of play.
                self.sum = self.held.iter().map(|g| f64::from(*g)).sum();
            }
        }
    }
}

// SAFETY: one Stretcher is used by one worker thread at a time.
unsafe impl Send for Stretcher {}

impl Stretcher {
    pub fn new(rate: u32) -> Stretcher {
        Self::from_state(rate, engine::new(rate, 2))
    }

    #[cfg(all(test, not(patch_signalsmith)))]
    fn with_options(rate: u32, opts: i32) -> Stretcher {
        Self::from_state(rate, engine::with_options(rate, 2, opts))
    }

    fn from_state(rate: u32, s: State) -> Stretcher {
        Stretcher {
            s,
            rate,
            ratio: None,
            pitch: None,
            peak: PeakLimit::new(rate),
        }
    }

    /// Output speed relative to the source (time ratio = 1 / speed) and
    /// pitch scale.
    pub fn set(&mut self, speed: f64, pitch: f64) {
        let target_ratio = 1.0 / speed.clamp(0.05, 4.0);
        let target_pitch = pitch.clamp(0.25, 4.0);
        // Smooth live tempo/key changes over roughly 40 ms. Rubber Band's
        // ratio setters take effect on subsequent processing; stepping them
        // at every worker chunk can create audible roughness during fader
        // moves. First configuration after reset is immediate so a seek or
        // path change starts with the requested sound.
        let alpha = 1.0 - (-(256.0 / (self.rate.max(1) as f64 * 0.04))).exp();
        let ratio = self
            .ratio
            .map_or(target_ratio, |v| v + (target_ratio - v) * alpha);
        let pitch = self
            .pitch
            .map_or(target_pitch, |v| v + (target_pitch - v) * alpha);
        self.ratio = Some(ratio);
        self.pitch = Some(pitch);
        engine::set(self.s, ratio, pitch);
    }

    pub fn reset(&mut self) {
        engine::reset(self.s);
        self.peak.reset();
        self.ratio = None;
        self.pitch = None;
    }

    /// Frames to feed before the first wanted one, and output frames to
    /// drop, so output starts exactly at the wanted position.
    pub fn start_pad(&self) -> usize {
        engine::start_pad(self.s)
    }

    pub fn start_delay(&self) -> usize {
        engine::start_delay(self.s) + self.peak.len
    }

    pub fn required(&self) -> usize {
        engine::required(self.s)
    }

    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(r.len());
        let ptrs = [l.as_ptr(), r.as_ptr()];
        // SAFETY: two pointers, each to `n` samples.
        unsafe { engine::process(self.s, ptrs.as_ptr(), n) };
    }

    pub fn available(&self) -> usize {
        engine::available(self.s)
    }

    pub fn retrieve(&mut self, l: &mut [f32], r: &mut [f32]) -> usize {
        let n = l.len().min(r.len());
        let ptrs = [l.as_mut_ptr(), r.as_mut_ptr()];
        // SAFETY: two pointers, each with room for `n` samples.
        let got = unsafe { engine::retrieve(self.s, ptrs.as_ptr(), n) };
        self.peak.run(&mut l[..got], &mut r[..got]);
        got
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        engine::delete(self.s);
    }
}

/// One stretcher instance for an aligned bundle, with shared padding/delay.
/// Worker only. Deck protection stays after the stem summer.
pub struct MultiStretcher {
    s: State,
    channels: usize,
    rate: u32,
    ratio: Option<f64>,
    pitch: Option<f64>,
}
// SAFETY: owned by a single deck render worker.
unsafe impl Send for MultiStretcher {}
impl MultiStretcher {
    pub fn new(rate: u32, channels: usize) -> Self {
        assert!((2..=18).contains(&channels));
        let s = engine::new(rate, channels);
        Self {
            s,
            channels,
            rate,
            ratio: None,
            pitch: None,
        }
    }
    pub fn set(&mut self, speed: f64, pitch: f64) {
        let alpha = 1.0 - (-(256.0 / (self.rate.max(1) as f64 * 0.04))).exp();
        let ratio = self
            .ratio
            .map_or(1.0 / speed, |v| v + (1.0 / speed - v) * alpha);
        let pitch = self.pitch.map_or(pitch, |v| v + (pitch - v) * alpha);
        self.ratio = Some(ratio);
        self.pitch = Some(pitch);
        engine::set(self.s, ratio, pitch);
    }
    pub fn reset(&mut self) {
        engine::reset(self.s);
        self.ratio = None;
        self.pitch = None;
    }
    pub fn start_pad(&self) -> usize {
        engine::start_pad(self.s)
    }
    pub fn start_delay(&self) -> usize {
        engine::start_delay(self.s)
    }
    pub fn required(&self) -> usize {
        engine::required(self.s)
    }
    pub fn available(&self) -> usize {
        engine::available(self.s)
    }
    pub fn process(&mut self, channels: &[Vec<f32>]) {
        assert_eq!(channels.len(), self.channels);
        assert!(self.channels <= 18);
        let n = channels.iter().map(Vec::len).min().unwrap_or(0);
        let mut ptrs = [std::ptr::null::<f32>(); 18];
        for (i, channel) in channels.iter().enumerate() {
            ptrs[i] = channel.as_ptr();
        }
        // SAFETY: `ptrs` holds one pointer per channel, each pointing at `n` samples.
        unsafe { engine::process(self.s, ptrs.as_ptr(), n) };
    }
    pub fn retrieve(&mut self, channels: &mut [Vec<f32>]) -> usize {
        assert_eq!(channels.len(), self.channels);
        assert!(self.channels <= 18);
        let n = channels.iter().map(Vec::len).min().unwrap_or(0);
        let mut ptrs = [std::ptr::null_mut::<f32>(); 18];
        for (i, channel) in channels.iter_mut().enumerate() {
            ptrs[i] = channel.as_mut_ptr();
        }
        // SAFETY: `ptrs` holds one mutable pointer per channel, each with room for `n` samples.
        unsafe { engine::retrieve(self.s, ptrs.as_ptr(), n) }
    }
}
impl Drop for MultiStretcher {
    fn drop(&mut self) {
        engine::delete(self.s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_limiter_holds_the_ceiling_for_delayed_transients() {
        let mut limiter = PeakLimit::new(48_000);
        let mut left = vec![0.0; limiter.len * 4];
        let mut right = vec![0.0; limiter.len * 4];
        left[3] = 4.0;
        right[limiter.len + 7] = 2.0;
        left[limiter.len * 2 + 11] = 1.5;
        limiter.run(&mut left, &mut right);
        let peak = left
            .iter()
            .chain(&right)
            .map(|v| v.abs())
            .fold(0.0f32, f32::max);
        assert!(peak <= CEILING + 1e-6, "peak {peak} exceeds {CEILING}");
        assert!((left[limiter.len + 3] - CEILING).abs() < 1e-5);
        assert!(right[limiter.len * 2 + 7].abs() <= CEILING + 1e-6);
    }

    #[test]
    fn peak_limiter_glides_into_peaks_without_gain_steps() {
        let mut limiter = PeakLimit::new(48_000);
        let n = limiter.len;
        // A 1 kHz tone jumping from 0.5 to 2.0: needs 6 dB of limiting at once.
        let tone = |i: usize| (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin();
        let input: Vec<f32> = (0..9_600)
            .map(|i| tone(i) * if i < 4_800 { 0.5 } else { 2.0 })
            .collect();
        let (mut left, mut right) = (input.clone(), input.clone());
        limiter.run(&mut left, &mut right);
        assert!(left.iter().all(|v| v.abs() <= CEILING + 1e-6));
        // Gain from output / delayed input where the input is not near zero.
        let gains: Vec<(usize, f32)> = (n..input.len())
            .filter(|i| input[i - n].abs() > 0.2)
            .map(|i| (i, left[i] / input[i - n]))
            .collect();
        let full_drop = 1.0 - CEILING / 2.0;
        let worst_step = gains
            .windows(2)
            .filter(|w| w[1].0 == w[0].0 + 1)
            .map(|w| (w[1].1 - w[0].1).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst_step <= full_drop / n as f32 * 1.05,
            "gain stepped {worst_step} in one frame (attack spans {n})"
        );
    }

    /// Feed a tone through at `speed` and count output frames per input.
    #[test]
    fn keylock_changes_duration_not_pitch() {
        let rate = 48_000;
        let mut st = Stretcher::new(rate);
        st.set(1.25, 1.0);
        let n = rate as usize * 2;
        let tone: Vec<f32> = (0..n)
            .map(|i| (std::f64::consts::TAU * 440.0 * i as f64 / rate as f64).sin() as f32 * 0.4)
            .collect();
        let (mut out, mut fed) = (vec![], 0);
        let mut l = vec![0.0f32; 4096];
        let mut r = vec![0.0f32; 4096];
        while fed < n {
            let want = st.required().max(256).min(n - fed);
            st.process(&tone[fed..fed + want], &tone[fed..fed + want]);
            fed += want;
            loop {
                let a = st.available().min(4096);
                if a == 0 {
                    break;
                }
                let got = st.retrieve(&mut l[..a], &mut r[..a]);
                out.extend_from_slice(&l[..got]);
            }
        }
        // 2 s at 1.25× ≈ 1.6 s of output (minus the stretcher's delay).
        let secs = out.len() as f64 / rate as f64;
        assert!((1.45..1.62).contains(&secs), "{secs:.3} s");
        // Pitch stays 440 Hz: zero crossings over a steady middle second.
        let mid = &out[out.len() / 4..out.len() / 4 + rate as usize / 2];
        let zc = mid
            .windows(2)
            .filter(|w| (w[0] <= 0.0) != (w[1] <= 0.0))
            .count();
        assert!((zc as i64 - 440).abs() <= 6, "{zc} crossings per 0.5 s");
    }

    /// Primed the way a deck primes it (`start_pad` frames of the source
    /// before the position, `start_delay` output frames dropped), each click
    /// of the source lands at its time / speed: the beat grid stays exact.
    #[test]
    fn primed_output_lines_up_with_the_source() {
        let rate = 48_000u32;
        let period = rate as usize / 4;
        let n = rate as usize * 6;
        let src: Vec<f32> = (0..n)
            .map(|i| match i % period {
                0 => 0.8,
                1 => -0.8,
                _ => 0.0,
            })
            .collect();
        let at = |i: i64| {
            if i < 0 {
                0.0
            } else {
                src.get(i as usize).copied().unwrap_or(0.0)
            }
        };
        for (speed, pitch) in [(1.0, 1.0), (1.04, 1.0), (0.97, 2f64.powf(1.0 / 12.0))] {
            let mut st = Stretcher::new(rate);
            st.reset();
            st.set(speed, pitch);
            let pos = (period / 2) as i64;
            let pad = st.start_pad();
            let lead: Vec<f32> = (0..pad as i64).map(|i| at(pos - pad as i64 + i)).collect();
            st.process(&lead, &lead);
            let mut skip = st.start_delay();
            let (mut fed, mut out) = (pos, vec![]);
            let mut b = vec![0.0f32; 4096];
            let mut b2 = vec![0.0f32; 4096];
            while out.len() < (rate as f64 * 4.0 / speed) as usize {
                st.set(speed, pitch);
                let k = st.required().clamp(64, 2048);
                let chunk: Vec<f32> = (0..k as i64).map(|i| at(fed + i)).collect();
                st.process(&chunk, &chunk);
                fed += k as i64;
                while st.available() > 0 {
                    let a = st.available().min(4096);
                    let got = st.retrieve(&mut b[..a], &mut b2[..a]);
                    for v in &b[..got] {
                        if skip > 0 {
                            skip -= 1;
                        } else {
                            out.push(*v);
                        }
                    }
                }
            }
            // Clicks 2..12 (away from the start-up): the output's peak near
            // each expected time.
            let mut worst = 0.0f64;
            for k in 2..12 {
                let want = ((k * period) as f64 - pos as f64) / speed;
                let w = rate as usize / 50;
                let lo = (want as usize).saturating_sub(w);
                let found = (lo..lo + 2 * w)
                    .max_by(|a, b| out[*a].abs().total_cmp(&out[*b].abs()))
                    .unwrap();
                worst = worst.max((found as f64 - want).abs() / rate as f64 * 1000.0);
            }
            assert!(
                worst < 2.0,
                "{ENGINE} at {speed}× pitch {pitch:.3}: a click is {worst:.2} ms off"
            );
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// Where a stretched track's kicks land, per Rubber Band configuration:
    /// each source kick's ideal output time (source time / speed) against
    /// the nearest kick found in the output. Real tracks, keylock, +3 %.
    /// PATCH_STRETCH_BENCH=<folder> cargo test --release -p patch-engine
    ///   stretch_transients -- --ignored --nocapture
    #[test]
    #[ignore]
    #[cfg(not(patch_signalsmith))]
    fn stretch_transients() {
        const RATE: u32 = 48_000;
        const SPEED: f64 = 1.03;
        let dir = std::env::var("PATCH_STRETCH_BENCH").unwrap();
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        let decode = |p: &std::path::Path| -> Vec<f32> {
            let out = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-ss", "60", "-t", "30", "-i"])
                .arg(p)
                .args(["-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
                .output()
                .unwrap();
            out.stdout
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        };
        let stretch = |opts: i32, x: &[f32]| -> Vec<f32> {
            let mut st = Stretcher::with_options(RATE, opts);
            st.set(SPEED, 1.0);
            let (l, r): (Vec<f32>, Vec<f32>) = x.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
            let pad = st.start_pad();
            let (pl, pr) = ([vec![0.0; pad], l].concat(), [vec![0.0; pad], r].concat());
            let mut skip = st.start_delay();
            let (mut out, mut fed) = (vec![], 0);
            let (mut bl, mut br) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
            while fed < pl.len() {
                let k = st.required().clamp(64, 2048).min(pl.len() - fed);
                st.process(&pl[fed..fed + k], &pr[fed..fed + k]);
                fed += k;
                while st.available() > 0 {
                    let a = st.available().min(4096);
                    let got = st.retrieve(&mut bl[..a], &mut br[..a]);
                    for i in 0..got {
                        if skip > 0 {
                            skip -= 1;
                        } else {
                            out.push((bl[i] + br[i]) * 0.5);
                        }
                    }
                }
            }
            out
        };
        use engine::{CHANNELS_TOGETHER, ENGINE_FINER, PITCH_HIGH_CONSISTENCY, REALTIME};
        // Rubber Band option bits (rubberband-c.h).
        const WINDOW_SHORT: i32 = 0x0010_0000;
        const DETECTOR_PERCUSSIVE: i32 = 0x0000_0400;
        let base = REALTIME | PITCH_HIGH_CONSISTENCY;
        let configs: [(&str, i32); 5] = [
            (
                "R3 together (current)",
                base | ENGINE_FINER | CHANNELS_TOGETHER,
            ),
            (
                "R3 together, short window",
                base | ENGINE_FINER | CHANNELS_TOGETHER | WINDOW_SHORT,
            ),
            ("R3 apart", base | ENGINE_FINER),
            ("R2 crisp, together", base | CHANNELS_TOGETHER),
            (
                "R2 crisp percussive",
                base | CHANNELS_TOGETHER | DETECTOR_PERCUSSIVE,
            ),
        ];
        // The detector's own floor: a plain resample to the same speed (no
        // phase vocoder; pitch moves, timing is exact).
        let resample = |x: &[f32]| -> Vec<f32> {
            let n = x.len() / 2;
            let m = (n as f64 / SPEED) as usize;
            (0..m)
                .map(|i| {
                    let p = i as f64 * SPEED;
                    let (j, f) = (p as usize, (p.fract()) as f32);
                    let at = |k: usize| (x[k.min(n - 1) * 2] + x[k.min(n - 1) * 2 + 1]) * 0.5;
                    at(j) * (1.0 - f) + at(j + 1) * f
                })
                .collect()
        };
        let mut errs: Vec<Vec<f64>> = vec![vec![]; configs.len() + 1];
        let mut lost = vec![0usize; configs.len() + 1];
        let mut cpu = vec![0f64; configs.len() + 1];
        let mut total = 0usize;
        for f in files.iter().take(12) {
            let x = decode(f);
            let mono: Vec<f32> = x.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect();
            // Source kicks, away from the edges.
            let src: Vec<f64> = patch_analysis::grid::kick_times(&mono, RATE)
                .into_iter()
                .filter(|t| *t > 1.0 && *t < 28.0)
                .collect();
            total += src.len();
            for c in 0..=configs.len() {
                let t0 = std::time::Instant::now();
                let y = match configs.get(c) {
                    Some((_, opts)) => stretch(*opts, &x),
                    None => resample(&x),
                };
                cpu[c] += t0.elapsed().as_secs_f64();
                let k = patch_analysis::grid::kick_times(&y, RATE);
                for t in &src {
                    let want = t / SPEED;
                    match k
                        .iter()
                        .map(|u| u - want)
                        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                    {
                        Some(e) if e.abs() < 0.03 => errs[c].push(e * 1e3),
                        _ => lost[c] += 1,
                    }
                }
            }
        }
        println!("{total} source kicks, 12 tracks, speed {SPEED}, keylock");
        let names: Vec<&str> = configs
            .iter()
            .map(|c| c.0)
            .chain(["resample (detector floor)"])
            .collect();
        for (c, name) in names.iter().enumerate() {
            let mut a: Vec<f64> = errs[c].iter().map(|e| e.abs()).collect();
            a.sort_by(f64::total_cmp);
            let mut e = errs[c].clone();
            e.sort_by(f64::total_cmp);
            let q = |v: &[f64], p: f64| {
                v.get(((v.len() as f64 - 1.0) * p) as usize)
                    .copied()
                    .unwrap_or(f64::NAN)
            };
            println!(
                "{name:28} |err| median {:4.1} ms  p90 {:4.1} ms  bias {:+4.1} ms  lost {:4.1} %  cpu {:4.1} % of real time",
                q(&a, 0.5),
                q(&a, 0.9),
                q(&e, 0.5),
                lost[c] as f64 / total as f64 * 100.0,
                cpu[c] / (12.0 * 30.0) * 100.0
            );
        }
    }
    /// cargo test --release -p patch-engine stretch_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stretch_speed() {
        for ratio in [1.005, 1.01] {
            let mut st = Stretcher::new(48_000);
            st.set(ratio, 1.0);
            let n = 48_000 * 20;
            let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.0577).sin() * 0.5).collect();
            let (mut l, mut r) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
            let t = std::time::Instant::now();
            let mut fed = 0;
            let mut got = 0;
            while fed < n {
                let k = st.required().clamp(64, 2048).min(n - fed);
                st.process(&x[fed..fed + k], &x[fed..fed + k]);
                fed += k;
                while st.available() > 0 {
                    let a = st.available().min(4096);
                    got += st.retrieve(&mut l[..a], &mut r[..a]);
                }
            }
            let e = t.elapsed();
            println!(
                "{ENGINE}, 20 s at speed {ratio}: {e:?} · {:.2} % of a core · {got} frames out",
                e.as_secs_f64() / 20.0 * 100.0
            );
        }
    }
}
