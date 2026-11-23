//! Soft takeover: a physical fader or knob only starts driving Patch once it
//! has caught up with what Patch is already doing.
//!
//! Without it, the first touch of a fader parked at zero after a track change
//! slams the channel to silence — the classic controller complaint. Mixxx
//! calls this soft takeover and offers it per control (135 bindings in its
//! shipped mappings use it), so it is a real hardware need, not a nicety.
//!
//! # Semantics
//!
//! * A control is **armed** with the value Patch is currently using, read
//!   from the same authoritative state the feedback layer uses rather than
//!   from a second copy.
//! * While **waiting**, the hardware's value is compared with that target.
//!   Movement away from it dispatches nothing.
//! * Once the hardware reaches the target the control is **in sync** and
//!   behaves normally, tracking its own value.
//! * If something *else* moves the value while the control is in sync — the
//!   AI DJ, the UI, a remote page — the control is re-armed against the new
//!   value, so it has to catch up again rather than jump.
//! * Relative controls are never taken over: a delta has no position to
//!   mismatch.
//!
//! A control with no known target is live on first movement, so a
//! soft-takeover binding is never dead.

/// Per-control takeover state, keyed by the control's identity within a
/// device. One entry per bound control that asks for takeover.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Entry {
    /// Waiting to catch up with the value Patch is using.
    Waiting { target: f64 },
    /// Live. `target` is the value the hardware last reported, so an external
    /// change is detectable by comparing it with the state Patch reports.
    InSync { target: f64 },
}

/// The soft-takeover table for one device.
#[derive(Default, Debug)]
pub struct SoftTakeover {
    entries: Vec<(u32, Entry)>,
}

/// The tolerance within which a hardware value counts as having reached the
/// target. A 7-bit fader's step is 1/127 ≈ 0.0079, so a little under one step
/// is the strictest value that always resolves while still refusing a jump of
/// a whole step.
pub const PICKUP_EPSILON: f64 = 1.0 / 127.0 / 2.0;

/// The most controls one device may have under takeover.
pub const MAX_CONTROLS: usize = 1024;

impl SoftTakeover {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms `control` to catch up with `target` (Patch's current value, in the
    /// binding's own 0…1 space). Called when a profile is loaded, a device
    /// reconnects, or Patch's value changes from somewhere other than this
    /// control.
    pub fn arm(&mut self, control: u32, target: f64) {
        let target = target.clamp(0.0, 1.0);
        if let Some((_, entry)) = self.entries.iter_mut().find(|(key, _)| *key == control) {
            *entry = Entry::Waiting { target };
            return;
        }
        if self.entries.len() < MAX_CONTROLS {
            self.entries.push((control, Entry::Waiting { target }));
        }
    }

    /// The value the control is armed against, in either state: what an
    /// external change is compared with.
    pub fn armed_target(&self, control: u32) -> Option<f64> {
        self.entries
            .iter()
            .find(|(key, _)| *key == control)
            .map(|(_, entry)| match entry {
                Entry::Waiting { target } | Entry::InSync { target } => *target,
            })
    }

    /// Whether `control` is waiting to catch up.
    pub fn is_waiting(&self, control: u32) -> bool {
        matches!(
            self.entries.iter().find(|(key, _)| *key == control),
            Some((_, Entry::Waiting { .. }))
        )
    }

    /// The hardware's value for `control`, if it may be dispatched.
    ///
    /// `hardware` is the absolute position the device reports. A control that
    /// was never armed is live: takeover is only meaningful against a known
    /// target.
    pub fn accept(&mut self, control: u32, hardware: f64) -> Option<f64> {
        let hardware = hardware.clamp(0.0, 1.0);
        let Some((_, entry)) = self.entries.iter_mut().find(|(key, _)| *key == control) else {
            return Some(hardware);
        };
        match entry {
            Entry::InSync { target } => {
                // Live: follow the hardware's own value, so the next feedback
                // pass sees no change and leaves the control alone.
                *target = hardware;
                Some(hardware)
            }
            Entry::Waiting { target } => {
                // The control is live only once the hardware has reached the
                // value Patch is already using. Anything else is discarded:
                // that is the whole point of soft takeover.
                if (hardware - *target).abs() <= PICKUP_EPSILON {
                    *entry = Entry::InSync { target: hardware };
                    Some(hardware)
                } else {
                    None
                }
            }
        }
    }

    /// Forgets a control (a profile reload, or a device that went away).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fader_away_from_the_target_is_ignored_until_it_catches_up() {
        let mut takeover = SoftTakeover::new();
        // Patch's channel fader is at 0.8. The hardware fader is parked at 0.
        takeover.arm(0, 0.8);
        assert!(takeover.is_waiting(0));
        // Sweeping up from 0 dispatches nothing until 0.8.
        for step in 1..=60 {
            assert_eq!(
                takeover.accept(0, 0.01 * step as f64),
                None,
                "at {}",
                0.01 * step as f64
            );
        }
        // Reaching the target picks the control up, and from then on it is
        // live in both directions.
        assert_eq!(takeover.accept(0, 0.8), Some(0.8));
        assert!(!takeover.is_waiting(0));
        assert_eq!(takeover.accept(0, 0.4), Some(0.4));
        assert_eq!(takeover.accept(0, 0.9), Some(0.9));
    }

    #[test]
    fn a_fader_above_the_target_catches_up_coming_back_down() {
        let mut takeover = SoftTakeover::new();
        takeover.arm(0, 0.5);
        // The fader is above the target and moves further up: ignored.
        assert_eq!(takeover.accept(0, 0.9), None);
        assert_eq!(takeover.accept(0, 1.0), None);
        // Coming down through the target picks it up.
        assert_eq!(takeover.accept(0, 0.7), None);
        assert_eq!(takeover.accept(0, 0.5), Some(0.5));
        assert_eq!(takeover.accept(0, 0.1), Some(0.1));
    }

    #[test]
    fn an_in_sync_control_follows_its_own_value_rather_than_re_arming() {
        let mut takeover = SoftTakeover::new();
        takeover.arm(0, 0.5);
        assert_eq!(takeover.accept(0, 0.5), Some(0.5));
        // Moving the fader moves the armed target with it, so a feedback pass
        // does not mistake the control's own movement for an external change
        // and force it to catch up again.
        assert_eq!(takeover.accept(0, 0.8), Some(0.8));
        assert_eq!(takeover.armed_target(0), Some(0.8));
    }

    #[test]
    fn an_unarmed_control_is_never_dead() {
        let mut takeover = SoftTakeover::new();
        // No target is known: the first value is accepted, so a takeover
        // binding on a control Patch has no state for still works.
        assert_eq!(takeover.accept(7, 0.42), Some(0.42));
        assert_eq!(takeover.accept(7, 0.9), Some(0.9));
        assert_eq!(takeover.armed_target(7), None);
    }

    #[test]
    fn arming_again_re_waites_and_clear_forgets() {
        let mut takeover = SoftTakeover::new();
        takeover.arm(1, 0.2);
        assert_eq!(takeover.accept(1, 0.2), Some(0.2));
        // Something else moved the value (the AI DJ, the UI): the control
        // must catch up again rather than jump.
        takeover.arm(1, 0.9);
        assert!(takeover.is_waiting(1));
        assert_eq!(takeover.accept(1, 0.2), None);
        assert_eq!(takeover.accept(1, 0.9), Some(0.9));
        assert_eq!(takeover.len(), 1);
        takeover.clear();
        assert!(takeover.is_empty());
        assert_eq!(takeover.accept(1, 0.0), Some(0.0));
    }

    #[test]
    fn the_table_is_bounded() {
        let mut takeover = SoftTakeover::new();
        for control in 0..5000 {
            takeover.arm(control, 0.5);
        }
        assert_eq!(takeover.len(), MAX_CONTROLS);
    }
}
