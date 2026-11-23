//! One bounded snapshot per lane. RT writes; control-plane readers never wait.
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub struct Published {
    sequence: AtomicU64,
    position: AtomicU64,
    nsec: AtomicU64,
    last_signal_nsec: AtomicU64,
    rate: AtomicU32,
    values: [AtomicU32; 5],
    history_count: AtomicU64,
    history_peaks: [AtomicU32; 128],
    history_nsec: [AtomicU64; 128],
}
impl Default for Published {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            position: AtomicU64::new(0),
            nsec: AtomicU64::new(0),
            last_signal_nsec: AtomicU64::new(0),
            rate: AtomicU32::new(0),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
            history_count: AtomicU64::new(0),
            history_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            history_nsec: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}
impl Published {
    pub fn publish(&self, clock: &crate::Clock, voice: &crate::voice::Voice) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.position.store(clock.position, Ordering::Relaxed);
        self.nsec.store(clock.nsec, Ordering::Relaxed);
        if self.rate.load(Ordering::Relaxed) != clock.rate {
            self.history_count.store(0, Ordering::Relaxed);
        }
        self.rate.store(clock.rate, Ordering::Relaxed);
        let reading = voice.reading();
        if reading[0] > 0.00001 {
            self.last_signal_nsec.store(clock.nsec, Ordering::Relaxed);
        }
        for (slot, value) in self.values.iter().zip(reading) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        let count = self.history_count.load(Ordering::Relaxed);
        let index = count as usize % 128;
        self.history_peaks[index].store(reading[0].to_bits(), Ordering::Relaxed);
        self.history_nsec[index].store(clock.nsec, Ordering::Relaxed);
        self.history_count
            .store(count.wrapping_add(1), Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }
    pub fn heard_within(&self, seconds: f32) -> bool {
        let last = self.last_signal_nsec.load(Ordering::Relaxed);
        last > 0 && crate::now_ns().saturating_sub(last) as f64 <= f64::from(seconds) * 1e9
    }

    pub fn read(&self) -> Option<serde_json::Value> {
        let sequence = self.sequence.load(Ordering::Acquire);
        if sequence == 0 || sequence & 1 != 0 {
            return None;
        }
        let values = self
            .values
            .each_ref()
            .map(|v| f32::from_bits(v.load(Ordering::Relaxed)));
        let position = self.position.load(Ordering::Relaxed);
        let nsec = self.nsec.load(Ordering::Relaxed);
        let rate = self.rate.load(Ordering::Relaxed);
        if crate::now_ns().saturating_sub(nsec) > 500_000_000 {
            return None;
        }
        if sequence != self.sequence.load(Ordering::Acquire) {
            return None;
        }
        Some(
            serde_json::json!({"sample_position":position, "nsec":nsec, "sample_rate":rate,
            "input_peak":values[0], "input_rms":values[1], "gate_gain":values[2],
            "compressor_gain":values[3], "deesser_gain":values[4], "algorithmic_latency_samples":0}),
        )
    }
    /// Control-plane only. Each bar is a complete engine-cycle peak, in
    /// chronological order; the axis is cycles, not a fabricated GUI clock.
    pub fn history(&self) -> Option<serde_json::Value> {
        let sequence = self.sequence.load(Ordering::Acquire);
        if sequence == 0 || sequence & 1 != 0 {
            return None;
        }
        let end = self.nsec.load(Ordering::Relaxed);
        if crate::now_ns().saturating_sub(end) > 500_000_000 {
            return None;
        }
        let count = self.history_count.load(Ordering::Relaxed);
        let start = count.saturating_sub(128);
        let peaks: Vec<f32> = (start..count)
            .map(|i| f32::from_bits(self.history_peaks[i as usize % 128].load(Ordering::Relaxed)))
            .collect();
        let first = self.history_nsec[start as usize % 128].load(Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Acquire);
        if self.sequence.load(Ordering::Relaxed) != sequence {
            return None;
        }
        Some(
            serde_json::json!({"peaks":peaks, "start_nsec":first, "end_nsec":end, "span_ms":end.saturating_sub(first) as f64 / 1e6}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_wraps_in_order_and_resets_on_rate_change() {
        let p = Published::default();
        let mut voice = crate::voice::Voice::default();
        let mut clock = crate::Clock {
            position: 0,
            nsec: crate::now_ns(),
            rate: 48000,
            frames: 1,
            delay: 0,
            ..Default::default()
        };
        for i in 0..256 {
            let mut l = [i as f32 / 256.0];
            let mut r = l;
            voice.process(&mut l, &mut r, &crate::voice::Settings::OFF, 48000);
            clock.position = i;
            clock.nsec = crate::now_ns();
            p.publish(&clock, &voice);
        }
        let history = p.history().unwrap();
        let peaks = history["peaks"].as_array().unwrap();
        assert_eq!(peaks.len(), 128);
        assert_eq!(peaks[0].as_f64(), Some(0.5));
        assert_eq!(peaks[127].as_f64(), Some(255.0 / 256.0));
        clock.rate = 96000;
        p.publish(&clock, &voice);
        assert_eq!(p.history().unwrap()["peaks"].as_array().unwrap().len(), 1);
    }
}
