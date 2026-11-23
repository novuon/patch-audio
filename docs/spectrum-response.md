# Responsive display spectrum

The Meter spectrum and choreography spectrum shapes share the program display
spectrum. Their beat pulse is separate and follows the stamped deck playhead.
A 60 fps capture with the exact 128 BPM click reference found spectrum maxima
66 ms after the crossfader, while peak bars and tested beat brightness aligned.

The display now uses `Analyzer::reactive()`: a rising half Hann taper with full
weight at the newest sample. `Analyzer::default()` keeps the symmetric Hann
window for reference spectrum measurements. Both retain 4096 bins and normalize
power by their actual window energy. The causal taper has wider spectral leakage;
the studio RTA and its reference measurements are unchanged. Short startup
histories are right-aligned so the newest sample retains its intended weight.

Program analysis wakes every 5 ms while audio flows (previously 20 ms), and the
collector publishes its snapshot on each existing 5 ms tick (previously every
fourth tick). The graph callback, signal routing, latency calibration, beat grid,
VU time constants and peak-meter attack are unchanged. Idle analysis retains
its one-second threshold and 100 ms sleep. Spectrum FFTs still stop when the
history is unchanged. No protocol or track-analysis cache version changes.

## Measurements

For the generated reference's exponentially decaying 1100 Hz click, scanning
transient age in 64-frame steps at 48 kHz:

| Spectrum | Age at maximum energy | Time for 64 FFTs |
|---|---:|---:|
| Symmetric reference | 44.000 ms | 6.723 ms |
| Reactive display | 6.667 ms | 6.232 ms |

These kernel timings are one run, not a CPU benchmark guarantee. Faster worker
cadence performs more FFTs per second; computation stays off the audio callback.
An isolated impulse peaks at age zero with the reactive taper, versus half a
window with the reference taper. Regression checks cover this distinction,
short histories, silence and the existing calibrated reference tones.

The installed release must be remeasured against the same click track. Kernel
response does not by itself measure screen presentation or acoustic latency.
