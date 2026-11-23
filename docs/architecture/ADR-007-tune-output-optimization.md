# ADR-007: Tune owns output optimization

Status: accepted

## Decision

Name the new Patch Unit **Tune**. Existing Units use short functional names (Mixer, Router, Meter, Recorder); Tune is compact and describes the workflow without claiming automatic knowledge or turning Patch itself into a unit. The user interface presents Patch as the application and Tune as one of its units.

Keep hardware identity and physical routing in the PipeWire/Router layers. Tune consumes selected-output identity and inserts processing into the engine's program path. Meter observes the processed program and retains ownership of measurements and visual meters. Mixer remains responsible for channel mix and creative channel processing.

The control plane parses profiles, resolves identity, validates data, compiles rate-specific filters, and publishes fixed-size DSP state. The RT plane performs bounded IIR filtering, smooth gain, protection, and output. It performs no file/network access, AI inference, dynamic allocation, logging, UI work, or blocking operations. Failures preserve normal flat playback.

The profile format is open TOML v1, offline, data-only, and keeps device, room, and user preference layers separate. The first implementation intentionally does not claim room calibration, microphone capture, excursion/thermal modeling, psychoacoustic bass, FIR phase correction, or AI processing.
