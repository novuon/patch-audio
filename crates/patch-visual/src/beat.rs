//! How a scene answers the beat: each scene its own feel (a lift on every
//! kick, a strobe on the one, off-beat pulses, 16th ticks, a shake, a
//! colour flash, a slow breath over the bar, or nothing but the motion).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beat {
    /// Brighter, a jump up and a colour split on every kick.
    Lift,
    /// A hard flash on the bar's one, nothing between.
    Strobe,
    /// Pulses between the beats, like an open hat.
    Offbeat,
    /// Small quick ticks on every 16th.
    Sixteenths,
    /// A sideways jolt on the kick.
    Shake,
    /// The ground lights up in the song's colour on the kick.
    Flash,
    /// A slow swell over each bar.
    Breathe,
    /// No pulse: the motion carries the beat.
    Still,
}

impl Beat {
    pub const ALL: [Beat; 8] = [
        Beat::Lift,
        Beat::Strobe,
        Beat::Offbeat,
        Beat::Sixteenths,
        Beat::Shake,
        Beat::Flash,
        Beat::Breathe,
        Beat::Still,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Beat::Lift => "lift",
            Beat::Strobe => "strobe",
            Beat::Offbeat => "offbeat",
            Beat::Sixteenths => "16ths",
            Beat::Shake => "shake",
            Beat::Flash => "flash",
            Beat::Breathe => "breathe",
            Beat::Still => "still",
        }
    }

    /// Gentle enough for a breakdown or a slow song.
    pub fn calm(self) -> bool {
        matches!(self, Beat::Breathe | Beat::Still | Beat::Offbeat)
    }
}

/// What the picture does at one moment: brightness lift (0…1), a jump up
/// (share of a cell), a colour split (0…1 of its full width), a sideways
/// shake (−1…1 of its full width) and a flash of the ground (0…1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Feel {
    pub pulse: f32,
    pub bump: f32,
    pub split: f32,
    pub shake: f32,
    pub tint: f32,
}

/// The feel of `beat` at `at` beats (the grid's), `energy` 0…1.
pub fn feel(beat: Beat, at: f64, energy: f32) -> Feel {
    let into = at.rem_euclid(1.0) as f32;
    let bar = at.rem_euclid(4.0) as f32;
    let e = 0.5 + 0.5 * energy;
    let kick = (1.0 - into).powi(4) * e;
    match beat {
        Beat::Lift => {
            let one = if bar < 1.0 { 1.0 } else { 0.75 };
            Feel {
                pulse: kick * one,
                bump: kick * one,
                split: kick * one,
                ..Default::default()
            }
        }
        Beat::Strobe => {
            let f = if bar < 1.0 { (1.0 - into).powi(8) } else { 0.0 };
            Feel {
                pulse: f * 1.3,
                split: f * 0.4,
                tint: f * 0.6,
                ..Default::default()
            }
        }
        Beat::Offbeat => {
            let off = (into + 0.5).rem_euclid(1.0).mul_add(-1.0, 1.0).powi(3) * e;
            Feel {
                pulse: off * 0.8,
                bump: -off * 0.5,
                ..Default::default()
            }
        }
        Beat::Sixteenths => {
            let tick = (1.0 - (at * 4.0).rem_euclid(1.0) as f32).powi(6) * e;
            Feel {
                pulse: tick * 0.5 + kick * 0.3,
                split: tick * 0.25,
                ..Default::default()
            }
        }
        Beat::Shake => {
            // Left on one beat, right on the next.
            let side = if (at.floor() as i64) % 2 == 0 {
                1.0
            } else {
                -1.0
            };
            Feel {
                pulse: kick * 0.4,
                shake: kick * side,
                ..Default::default()
            }
        }
        Beat::Flash => Feel {
            pulse: kick * 0.5,
            tint: kick,
            ..Default::default()
        },
        Beat::Breathe => {
            let swell = (0.5 - 0.5 * (bar / 4.0 * std::f32::consts::TAU).cos()) * e;
            Feel {
                pulse: swell * 0.45,
                ..Default::default()
            }
        }
        Beat::Still => Feel::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_feel_is_its_own() {
        let on = |b: Beat, at: f64| feel(b, at, 0.8);
        assert!(on(Beat::Lift, 1.0).pulse > 0.5 && on(Beat::Lift, 1.6).pulse < 0.05);
        assert!(on(Beat::Strobe, 4.0).pulse > 0.9, "on the one");
        assert_eq!(on(Beat::Strobe, 5.0).pulse, 0.0, "not on the two");
        assert!(
            on(Beat::Offbeat, 2.5).pulse > on(Beat::Offbeat, 2.0).pulse,
            "between the beats"
        );
        assert!(on(Beat::Sixteenths, 2.25).pulse > on(Beat::Sixteenths, 2.2).pulse);
        assert!(on(Beat::Shake, 2.0).shake > 0.5 && on(Beat::Shake, 3.0).shake < -0.5);
        assert!(on(Beat::Flash, 1.0).tint > 0.5);
        assert!(
            on(Beat::Breathe, 2.0).pulse > on(Beat::Breathe, 0.1).pulse,
            "swells over the bar"
        );
        assert_eq!(on(Beat::Still, 1.0), Feel::default());
    }
}
