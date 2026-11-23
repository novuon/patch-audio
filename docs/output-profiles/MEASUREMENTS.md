# Measurements and room calibration

Measurement records distinguish `measured`, `modeled`, `known`, `inferred`, and `user` values. Record microphone model/serial, calibration curve and source, orientation, position, sample rate, test signal, ambient noise, date, uncertainty, and confidence. An uncalibrated laptop microphone supports rough comparisons only; it is not a reference microphone.

Room measurements are attached to a room/listening setup, never the universal device profile. Planned calibration uses a level checked logarithmic sweep, capture, impulse/frequency analysis, unreliable-band detection, headroom-cost calculation, safe constrained filter fitting, and loudness matched A/B. Warn and request explicit start before test audio; cap its level. Do not use AI to guess response from audio.

Multi-position measurement and distortion characterization are future extensions. Profile data must not imply precise excursion or thermal measurements without an appropriate method and provenance.
