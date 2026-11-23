# EQ filter design

`patch-dsp::filter` uses the normalized Audio EQ Cookbook biquad equations by
Robert Bristow-Johnson. Bell, low-pass, high-pass, band-pass, and notch use
the cookbook Q parameterization. Shelves use the cookbook slope-one form.
Coefficient calculation uses f64 and transposed direct form II state.

Input validation restricts frequency to 1 Hz through 0.49 × sample rate, Q
to 0.1–18, and gain to ±36 dB. Sample rates below 8 kHz are rejected. A
coefficient set is rejected if it is non-finite or fails the stability guard.
The processor caps one cascade at 64 active bands.

The existing DJ isolator has separate crossover semantics (Linkwitz–Riley
splitting and kill behavior). It is not equivalent to a row of ordinary
parametric filters and remains independently configured.

