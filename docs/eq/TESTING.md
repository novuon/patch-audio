# EQ verification plan

The shared filter foundation has numerical tests for parameter rejection,
coefficient stability at 44.1, 48, 88.2, 96, and 192 kHz, a measured 1 kHz
bell boost, finite impulse response for a high-Q notch, and zero reported
algorithmic latency.

Before live graph insertion, extend tests with impulse/sweep/noise response
for every type; gain and cut; shelves; channel routing; bypass ramps; parameter
automation; sample-rate changes; randomized differential comparisons against
the reference; silence/denormal behavior; and callback allocation/lock
instrumentation. Analyzer taps must be bounded and run outside the critical
audio path, using Meter analysis where available. Response plots must evaluate
the configured transfer functions; spectrum traces represent signal data and
must be drawn as a separate visual layer.

