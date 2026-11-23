# ADR-007: EQ and Tune are distinct Patch responsibilities

**Status:** Accepted

EQ owns intentional tonal shaping. Tune owns physical output correction,
calibration, optimization, and protection. Both reuse Patch DSP filter
infrastructure but retain separate configuration, presets, and user-facing
meaning. Tune device calibration must not be merged into an EQ preset.

The graph owns processor placement. EQ can appear more than once at a source,
deck, bus, microphone, or master position. Tune may follow master processing
where hardware correction is appropriate. They are not one global EQ stage.

The Patch DSP layer owns portable filter mathematics and processing kernels;
units own their semantic models. DJ isolator/kill behavior remains a distinct
DJ EQ model while reusing shared low-level kernels where practical.

