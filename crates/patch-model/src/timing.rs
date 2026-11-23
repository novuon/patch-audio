//! The one timing model. Everything the engine measures is stamped with the
//! block it came from, so a surface can show it at the moment it is heard
//! rather than the moment it was computed.

/// Where in the engine's timeline a block of audio (and its meter values) sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockStamp {
    /// Graph clock position of the first frame (samples since the clock started).
    pub sample_pos: u64,
    /// Monotonic time of that frame, ns (CLOCK_MONOTONIC, as PipeWire reports it).
    pub nsec: u64,
    pub rate: u32,
    pub frames: u32,
}

impl BlockStamp {
    pub fn duration_ns(&self) -> u64 {
        if self.rate == 0 {
            0
        } else {
            self.frames as u64 * 1_000_000_000 / self.rate as u64
        }
    }

    /// When this block reaches the listener, given the output path latency.
    pub fn heard_at_ns(&self, latency: &Latency) -> u64 {
        self.nsec + latency.best_ns().unwrap_or(0)
    }
}

/// Which moment meters are drawn for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MeterReference {
    /// What the listener hears now: blocks are shown at `nsec + output latency`.
    #[default]
    Output,
    /// What the engine is processing now (no latency compensation).
    Engine,
}

/// How a latency figure was obtained, from least to most trustworthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LatencySource {
    /// Declared by the driver / PipeWire `Latency` param. Bluetooth devices
    /// often under-report.
    Reported,
    /// Tracked from successive clock stamps (drift and changes over time).
    Tracked,
    /// Measured end to end with a calibration (Analyzer loopback).
    Measured,
}

/// Where a sound is on its way out (ADR-005). Each has its own source and
/// certainty; nothing downstream of what a backend exposes is presented
/// as exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Position {
    /// Frame in the track (deck transport). Exact.
    Source,
    /// Frames decoded so far. Exact.
    Decoder,
    /// Rendered ahead by the deck worker. Exact.
    Render,
    /// The graph cycle the engine processes (`BlockStamp`). Exact.
    Graph,
    /// When the device consumes that cycle: + one quantum. Exact for the graph.
    OutputBuffer,
    /// When the DAC or radio plays it: + device latency (REPORTED by the
    /// driver; Bluetooth often under-reports).
    HardwarePresentation,
    /// When it reaches the ear: MEASURED (acoustic loop, with its spread),
    /// else REPORTED + trim, marked uncertain.
    AcousticPresentation,
}

/// Latency of one path, with every level we know. Nothing is invented: a
/// level we don't have stays `None`, and the UI says which one it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Latency {
    pub reported_ns: Option<u64>,
    pub tracked_ns: Option<u64>,
    pub measured_ns: Option<u64>,
    /// Spread of the measurement (sweeps that agreed), when measured.
    pub measured_spread_ns: Option<u64>,
}

/// When a block will be heard, how that was found and how sure it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub at_ns: u64,
    pub source: LatencySource,
    /// ± this much, when known. None = unknown (e.g. a Bluetooth sink with
    /// only the driver's word): shown, never presented as exact.
    pub uncertainty_ns: Option<u64>,
}

impl Latency {
    /// The most trustworthy figure available, and where it came from.
    pub fn best(&self) -> Option<(u64, LatencySource)> {
        self.measured_ns
            .map(|n| (n, LatencySource::Measured))
            .or(self.tracked_ns.map(|n| (n, LatencySource::Tracked)))
            .or(self.reported_ns.map(|n| (n, LatencySource::Reported)))
    }

    pub fn best_ns(&self) -> Option<u64> {
        self.best().map(|(n, _)| n)
    }

    /// When `block` reaches the listener, with the certainty we have.
    /// None when no latency level is known at all.
    pub fn presentation(&self, block: &BlockStamp) -> Option<Presentation> {
        let (ns, source) = self.best()?;
        let uncertainty_ns = match source {
            LatencySource::Measured => Some(self.measured_spread_ns.unwrap_or(0) / 2),
            // Tracked drift is relative: its absolute error is the reported one's.
            LatencySource::Tracked | LatencySource::Reported => None,
        };
        Some(Presentation {
            at_ns: block.nsec + ns,
            source,
            uncertainty_ns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_and_latency() {
        let b = BlockStamp {
            sample_pos: 48_000,
            nsec: 1_000_000_000,
            rate: 48_000,
            frames: 1024,
        };
        assert_eq!(b.duration_ns(), 21_333_333);
        let mut l = Latency {
            reported_ns: Some(219_700_000),
            ..Default::default()
        };
        assert_eq!(l.best(), Some((219_700_000, LatencySource::Reported)));
        assert_eq!(b.heard_at_ns(&l), 1_219_700_000);
        l.measured_ns = Some(250_000_000);
        assert_eq!(l.best().unwrap().1, LatencySource::Measured);
        assert_eq!(Latency::default().best(), None);
    }

    #[test]
    fn bluetooth_latency_says_how_sure_it_is() {
        let b = BlockStamp {
            sample_pos: 0,
            nsec: 5_000_000_000,
            rate: 48_000,
            frames: 512,
        };
        // AirPods-class sink: only the driver's figure → uncertain.
        let mut bt = Latency {
            reported_ns: Some(180_000_000),
            ..Default::default()
        };
        let p = bt.presentation(&b).unwrap();
        assert_eq!(
            (p.at_ns, p.source, p.uncertainty_ns),
            (5_180_000_000, LatencySource::Reported, None)
        );
        // After an acoustic measurement: the measured figure, ± half its spread.
        bt.measured_ns = Some(212_500_000);
        bt.measured_spread_ns = Some(400_000);
        let p = bt.presentation(&b).unwrap();
        assert_eq!(
            (p.at_ns, p.source, p.uncertainty_ns),
            (5_212_500_000, LatencySource::Measured, Some(200_000))
        );
        // Nothing known: no presentation time at all (never a guess).
        assert!(Latency::default().presentation(&b).is_none());
        assert!(Position::Graph < Position::AcousticPresentation);
    }
}
