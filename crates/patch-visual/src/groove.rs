//! The shadow DJ's body: how it moves to the music, eased every frame so
//! nothing jumps. The engine's beat arrives in steps (polled, and from the
//! louder deck, which changes in a blend); here it drives a steady clock,
//! and every motion and gesture is a critically damped spring toward its
//! target, so styles crossfade instead of switching.
use crate::performance::{Scene, Source};
use patch_core::visual_intent::Section;

/// What the shader draws from, in metres and radians of the figure.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    /// The steady beat clock (beats; arbitrary origin, bar-locked phase).
    pub beat: f32,
    /// Head down on the beat, head tilted side to side, the whole body
    /// swaying sideways, the shoulders bouncing.
    pub nod: f32,
    pub tilt: f32,
    pub sway: f32,
    pub bounce: f32,
    /// −1 looking at deck A's side … 1 at B's; leaning into the booth 0 … 1.
    pub look: f32,
    pub lean: f32,
    /// Each shoulder raised, A side and B side.
    pub shrug: [f32; 2],
    /// [hand on the headphone A, B · arm raised A, B]
    pub gesture: [f32; 4],
    /// [both hands up overhead · pointing out A, B · nudging the jog]
    pub gesture2: [f32; 4],
    /// The backlight on the outline, 0 … 1: low in a breakdown, up with
    /// the energy and on a drop, breathing with the groove.
    pub rim: f32,
}

/// A critically damped spring: starts and stops without a jolt.
#[derive(Clone, Copy, Debug, Default)]
struct Spring {
    x: f32,
    v: f32,
}
impl Spring {
    fn to(&mut self, target: f32, omega: f32, dt: f32) -> f32 {
        // Sub-steps keep it stable on a slow frame.
        let n = (dt / 0.02).ceil().max(1.0);
        let h = dt / n;
        for _ in 0..n as usize {
            let a = omega * omega * (target - self.x) - 2.0 * omega * self.v;
            self.v += a * h;
            self.x += self.v * h;
        }
        self.x
    }
}

/// A smooth pulse peaking on each whole beat (0 … 1), no corners anywhere.
fn accent(beat: f64) -> f32 {
    let c = 0.5 + 0.5 * (beat * std::f64::consts::TAU).cos();
    (c * c) as f32
}

fn hash(x: f64) -> f32 {
    ((x * 91.345 + 17.17).sin() * 43_758.545).rem_euclid(1.0) as f32
}

/// Smooth value noise, 0 … 1: random values at whole numbers, eased
/// between them. Driven by the beat clock it varies each move's size and
/// timing without a jump, so no two bars move quite alike.
fn wander(x: f64, channel: f64) -> f32 {
    let i = x.floor();
    let f = (x - i) as f32;
    let f = f * f * (3.0 - 2.0 * f);
    let a = hash(i * 13.1 + channel * 71.7);
    let b = hash((i + 1.0) * 13.1 + channel * 71.7);
    a + (b - a) * f
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Groove {
    beat: f64,
    rate: f64,
    locked: bool,
    amount: Spring,
    /// How much of each way of moving: nod, side bob, shoulder roll,
    /// bounce, half-time.
    style: [Spring; 5],
    look: Spring,
    lean: Spring,
    gesture: [Spring; 8],
    melody: Spring,
    bass: Spring,
    soft: [Spring; 4],
    rim: Spring,
}

impl Groove {
    pub fn advance(&mut self, scene: &Scene, sources: &[Source], dt: f32) -> Body {
        let dt = dt.clamp(0.0, 0.1);
        let h = f64::from(dt);
        // The clock: runs at its own rate, pulled toward the engine's beat
        // (phase within the bar), its rate trimmed by the error.
        if let Some(m) = scene.beat.map(f64::from) {
            if !self.locked {
                self.beat = m;
                self.rate = self.rate.clamp(1.5, 3.0);
                self.locked = true;
            } else {
                self.beat += self.rate * h;
                let err = (m - self.beat + 2.0).rem_euclid(4.0) - 2.0;
                self.beat += err * (1.0 - (-h * 3.0).exp());
                self.rate = (self.rate + err * h * 1.5).clamp(1.0, 4.0);
            }
        } else {
            self.locked = false;
            // Wind down to a stop rather than freeze mid-move.
            self.rate *= (-h * 2.0).exp();
            self.beat += self.rate * h;
        }
        let b = self.beat;
        let playing = scene.beat.is_some();
        let energy = scene.intent.intensity.clamp(0.0, 1.0);
        let section = scene.intent.section;
        let feel = match section {
            Section::Drop => 1.25,
            Section::Build => 0.9,
            Section::Breakdown => 0.4,
            Section::Intro | Section::Outro => 0.6,
            _ => 0.75,
        };
        let amount = self.amount.to(if playing { (0.35 + energy * 0.9) * feel } else { 0.0 }, 2.5, dt);

        // Each 16-beat phrase picks its mix of moves; the springs blend
        // them over a couple of beats.
        let phrase = (b / 16.0).floor() + f64::from(scene.seed) * 97.0;
        let pick = |k: f64| hash(phrase * 7.0 + k);
        let mut want = [0.25 + pick(1.0), pick(2.0) * 0.9, pick(3.0) * 0.8, 0.3 + pick(4.0) * 0.7, 0.0];
        let total: f32 = want[..4].iter().sum();
        for w in &mut want[..4] {
            *w /= total;
        }
        want[4] = match section {
            Section::Breakdown | Section::Intro => 1.0,
            Section::Drop => 0.0,
            _ => f32::from(pick(5.0) > 0.7),
        };
        let mut style = [0.0; 5];
        for (i, s) in self.style.iter_mut().enumerate() {
            style[i] = s.to(want[i], 2.0, dt);
        }
        let half = style[4];
        // Human, not a metronome: the moves are rounded waves, not hits;
        // how tightly they follow the beat wanders, and the rest of the
        // motion flows slowly with the melody (mids and highs), the bass
        // driving the bounce. Each move's depth and timing wander too, so
        // the same moves never repeat exactly.
        let seed = f64::from(scene.seed) * 31.0;
        let vary = |rate: f64, k: f64| wander(b / rate + seed, k);
        let melody = self.melody.to((scene.bands[1] * 0.6 + scene.bands[2] * 0.4).clamp(0.0, 1.0), 1.5, dt);
        let bass = self.bass.to(scene.bands[0].clamp(0.0, 1.0), 2.5, dt);
        let lock = 0.35 + 0.5 * vary(11.0, 12.0); // how much on the beat
        let late = f64::from(vary(5.0, 1.0)) * 0.15; // a touch behind it
        let beat_wave = accent(b - late) * (1.0 - half) + accent((b - late) / 2.0) * half;
        // Beat one of the bar a little heavier, by a varying amount.
        let bar = 1.0 + (0.5 + 0.5 * (b * std::f64::consts::FRAC_PI_2).cos() as f32) * 0.35 * vary(7.0, 2.0);
        let pi = std::f64::consts::PI;
        let side = |phase: f64| (phase * pi).sin() as f32;
        let flow = |rate: f64, k: f64| side(b / rate + f64::from(vary(13.0, k)) * 1.5);
        let nod_beat = beat_wave * bar * lock;
        let nod_flow = (0.5 + 0.5 * flow(2.0, 14.0)) * (1.0 - lock) * (0.5 + melody);
        let nod = amount * (style[0] * 1.6 + 0.5) * (nod_beat + nod_flow) * 0.011 * (0.6 + 0.7 * vary(3.0, 3.0));
        // The head's side tilt: a slow drift with the melody over two to
        // four beats, plus the groove's own bob.
        let shift = f64::from(vary(9.0, 4.0)) * 0.6;
        let bob = amount * style[1] * 1.6 * (side(b / 2.0 + shift) * (1.0 - half) + side(b / 4.0 + shift) * half)
            * 0.06 * (0.4 + 1.2 * vary(4.0, 5.0))
            + amount * (flow(4.0, 15.0) * (0.3 + melody) * 0.05 + (vary(6.0, 6.0) - 0.5) * 0.05);
        let roll = amount * style[2] * 2.0 * (0.5 + vary(5.0, 7.0));
        let sway = amount * (0.010 * (0.4 + 1.2 * vary(8.0, 8.0)) * (0.6 + melody) * flow(4.0, 16.0) + roll * 0.006 * side(b / 2.0));
        let bounce = amount * (style[3] * 1.6 + 0.3) * (beat_wave * lock + 0.3) * 0.006 * (0.5 + bass) * (0.6 + 0.8 * vary(4.0, 9.0));
        // The shoulders do not take turns. One side rising and then the
        // other read as a yawn, on the silhouette and on the dots alike.
        let shrug = [0.0, 0.0];
        // A last soft spring on all of it: nothing lands hard.
        let soft = 9.0;
        let nod = self.soft[0].to(nod, soft, dt);
        let bob = self.soft[1].to(bob, soft, dt);
        let sway = self.soft[2].to(sway, soft, dt);
        let bounce = self.soft[3].to(bounce, soft, dt);

        // Who is where in the mix.
        let weight = |i: usize| sources.get(i).map_or(0.0, |s| if s.active { s.weight } else { 0.0 });
        let outgoing = usize::from(weight(1) > weight(0));
        let incoming = 1 - outgoing;
        let blending = scene.handover.is_some_and(|x| x > 0.0 && x < 1.0);
        let cueing = scene.transition_preparing || scene.handover.is_some_and(|x| x < 0.3);
        let p = scene.progress;

        // Gesture targets: one thing at a time, by priority.
        let mut g = [0.0f32; 8];
        let free = if cueing { incoming } else { 1 };
        if cueing {
            g[outgoing] = 1.0;
        }
        match section {
            // A build's last stretch: both hands up overhead.
            Section::Build if p > 0.8 && !cueing && energy > 0.5 => g[4] = 1.0,
            Section::Build if p > 0.6 => g[2 + free] = 0.55,
            // The drop: an arm up, pumping with the beat.
            Section::Drop if p < 0.3 => g[2 + free] = 1.0,
            // A hand to the headphone as a breakdown opens, then out to the
            // crowd.
            Section::Breakdown if p < 0.2 && !cueing => g[free] = 1.0,
            Section::Breakdown if (0.25..0.65).contains(&p) && !cueing => g[5 + free] = 1.0,
            // In a groove most phrases have a gesture, a different one each
            // time: out to the crowd, an arm up, a hand to the headphone.
            Section::Verse | Section::Unknown if !cueing && !blending && b.rem_euclid(16.0) < 6.0 => {
                let r = pick(6.0);
                if r > 0.7 {
                    g[5 + free] = 1.0;
                } else if r > 0.45 {
                    g[2 + free] = 0.75;
                } else if r > 0.2 {
                    g[free] = 1.0;
                }
            }
            _ => {}
        }
        // Between gestures he stays on the mixer. Some phrases the free
        // hand visits the deck's pads. It does not sit on the spinning jog,
        // and it does not lift one hand and then the other.
        let busy = g.iter().take(7).any(|&x| x > 0.0);
        g[7] = if playing && !busy && !blending && pick(12.0) > 0.72 {
            0.55
        } else {
            0.0
        };
        // How high an arm goes and how far it points differ each time.
        let reach = 0.8 + 0.2 * pick(10.0);
        for x in &mut g[2..7] {
            *x *= reach;
        }
        let mut gesture = [0.0; 8];
        for (i, s) in self.gesture.iter_mut().enumerate() {
            gesture[i] = s.to(g[i], 5.0 + pick(11.0 + i as f64) * 2.0, dt).clamp(0.0, 1.2);
        }
        // The raised arm pumps: down a little on each beat.
        let pump = (accent(b) * 0.22) * f32::from(u8::from(section == Section::Drop));
        for a in &mut gesture[2..4] {
            *a = (*a * (1.0 - pump)).max(0.0);
        }

        // Where the head looks: the incoming deck in a blend, the ear's
        // side while cueing, the crowd in a drop, otherwise a slow glance
        // per phrase.
        let toward = |i: usize| if i == 0 { -1.0 } else { 1.0 };
        let look_want = if blending {
            toward(incoming) * 0.8
        } else if cueing {
            toward(outgoing) * 0.6
        } else if section == Section::Drop {
            0.0
        } else {
            (pick(8.0) - 0.5) * 1.2 * f32::from(u8::from(pick(9.0) > 0.4))
        };
        // Small glances around that wherever it looks.
        let look = self.look.to(look_want + (vary(3.0, 10.0) - 0.5) * 0.35, 2.0, dt);
        let lean = self.lean.to(if blending { 1.0 } else { 0.0 }, 1.8, dt);
        let rim_want = match section {
            Section::Drop if p < 0.4 => 0.85,
            Section::Drop => 0.6,
            Section::Breakdown => 0.18,
            Section::Build => 0.3 + 0.4 * p,
            _ => 0.25 + 0.3 * energy,
        };
        let rim = self.rim.to(if playing { rim_want } else { 0.15 }, 1.2, dt) * (0.9 + 0.1 * beat_wave);
        Body {
            beat: b as f32,
            nod,
            tilt: bob,
            sway,
            bounce,
            look,
            lean,
            shrug,
            gesture: [gesture[0], gesture[1], gesture[2], gesture[3]],
            gesture2: [gesture[4], gesture[5], gesture[6], gesture[7]],
            rim,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(beat: f32) -> Scene {
        Scene { beat: Some(beat), ..Default::default() }
    }

    #[test]
    fn the_clock_runs_smooth_through_a_stepped_beat() {
        let mut g = Groove::default();
        let src = [Source { active: true, weight: 1.0, ..Default::default() }; 2];
        // 128 BPM, 60 fps, the engine's beat updated only every 6th frame.
        let mut last: Option<f32> = None;
        let mut biggest = 0.0f32;
        for f in 0..1200 {
            let polled = (f / 6 * 6) as f32 / 60.0 * 128.0 / 60.0;
            let body = g.advance(&scene(polled), &src, 1.0 / 60.0);
            if let Some(prev) = last
                && f > 240
            {
                biggest = biggest.max((body.beat - prev - 128.0 / 3600.0).abs());
            }
            last = Some(body.beat);
        }
        // Each frame's step stays within 25 % of a steady beat's.
        assert!(biggest < 0.25 * 128.0 / 3600.0, "step error {biggest}");
    }

    #[test]
    fn no_motion_jumps_between_frames() {
        let mut g = Groove::default();
        let src = [Source { active: true, weight: 1.0, ..Default::default() }; 2];
        let mut s = scene(0.0);
        s.intent.intensity = 0.9;
        let mut prev = g.advance(&s, &src, 1.0 / 60.0);
        let mut last = (0.0, 0.0);
        for f in 1..6000 {
            s.beat = Some(f as f32 / 60.0 * 2.1);
            // Sections and cues change abruptly, as the Director's do.
            s.intent.section = [Section::Verse, Section::Build, Section::Drop, Section::Breakdown][(f / 700) % 4];
            s.progress = (f % 700) as f32 / 700.0;
            s.transition_preparing = (f / 900) % 2 == 1;
            let b = g.advance(&s, &src, 1.0 / 60.0);
            // A jump is a sudden change of speed: compare each frame's
            // step with the one before.
            let (dn, dt) = (b.nod - prev.nod, b.tilt - prev.tilt);
            assert!((dn - last.0).abs() < 0.0025, "nod jumped at {f}: {dn} after {}", last.0);
            assert!((dt - last.1).abs() < 0.006, "tilt jumped at {f}: {dt} after {}", last.1);
            last = (dn, dt);
            for i in 0..4 {
                assert!((b.gesture[i] - prev.gesture[i]).abs() < 0.12, "gesture {i} jumped at {f}");
                assert!((b.gesture2[i] - prev.gesture2[i]).abs() < 0.12, "gesture2 {i} jumped at {f}");
            }
            prev = b;
        }
    }

    #[test]
    fn the_shoulders_do_not_take_turns() {
        let mut g = Groove::default();
        let src = [Source { active: true, weight: 1.0, ..Default::default() }; 2];
        let mut s = scene(0.0);
        s.intent.intensity = 0.9;
        s.intent.section = Section::Verse;
        for f in 0..800 {
            s.beat = Some(f as f32 / 60.0 * 2.1);
            let b = g.advance(&s, &src, 1.0 / 60.0);
            assert!(
                (b.shrug[0] - b.shrug[1]).abs() < 0.001,
                "shoulders alternated at {f}: {:?}",
                b.shrug
            );
        }
    }

    #[test]
    fn gestures_cue_on_the_headphone_and_rise_on_the_drop() {
        let mut g = Groove::default();
        let src = [Source { active: true, weight: 1.0, ..Default::default() }, Source::default()];
        let mut s = scene(0.0);
        s.transition_preparing = true;
        let mut b = Body::default();
        for f in 0..120 {
            s.beat = Some(f as f32 / 30.0);
            b = g.advance(&s, &src, 1.0 / 60.0);
        }
        assert!(b.gesture[0] > 0.9 && b.gesture[3] < 0.1);
        s.transition_preparing = false;
        s.intent.section = Section::Drop;
        s.progress = 0.05;
        for f in 120..240 {
            s.beat = Some(f as f32 / 30.0);
            b = g.advance(&s, &src, 1.0 / 60.0);
        }
        assert!(b.gesture[0] < 0.1 && b.gesture[3] > 0.7);
    }
}
