# EQ real-time contract

The audio callback may only consume prepared coefficients and preallocated
filter state. It must not parse presets, allocate, lock, access files or the
network, call the UI, log synchronously, or wait. Validation and coefficient
design belong on the control thread. A prepared state is swapped at a graph
boundary or through a bounded handoff.

The shared cascade performs no allocation or locks while processing. Its
current update operation may resize vectors and is therefore a control-thread
operation only. A future engine insert must keep a stable processor topology
in the callback and ramp bypass/gain changes. For coefficient movement,
prepare a stable transition strategy (interpolation or parallel crossfade)
and test it against fast automation before enabling it in the live path.

The current cascade reports zero algorithmic latency. Any future FIR path
must expose block/algorithmic latency to the authoritative Patch timing
model. Avoid ordinary-EQ oversampling. Add platform kernels only after
profiling realistic live block sizes and differential testing against the
portable reference.

