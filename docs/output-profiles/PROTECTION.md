# Protection and limits

Peak protection in the first engine slice has instantaneous gain reduction with smoothed release and a fixed ceiling. It adds no lookahead latency. It is not a true peak limiter, excursion model, thermal model, or guarantee against physical damage. Frequency-dependent, RMS, excursion, amplifier, and distortion protection require validated profile evidence and future implementation.

Correction boost is bounded by profile validation; required attenuation is explicit. Device safety constraints override tonal enhancement. If profile validation fails, clear its correction and fall back to normal audio. Do not turn an unknown physical limit into a fabricated numeric limit.
