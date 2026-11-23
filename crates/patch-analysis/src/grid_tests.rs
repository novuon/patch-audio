
use super::*;

/// Sections changing every 16 beats from the third beat of the counted
/// bar: the vote says bar 1 is beat 2 (0-based); without sections, no
/// vote.
#[test]
fn sections_vote_for_the_bar_they_start_on() {
    let rate = 2000u32;
    let beat_s = 0.5;
    let beats = 256usize;
    let mono: Vec<f32> = (0..(beats as f64 * beat_s * f64::from(rate)) as usize)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let beat = (t / beat_s) as usize;
            let section = (beat + 16 - 2) / 16;
            let level = [0.05, 0.4, 0.15, 0.8][section % 4];
            (level * (t * 440.0 * std::f64::consts::TAU).sin()) as f32
        })
        .collect();
    assert_eq!(section_phase(&mono, rate, beat_s, 0.0), Some(2));
    let flat: Vec<f32> = (0..mono.len()).map(|i| (0.3 * (i as f64 * 0.7).sin()) as f32).collect();
    assert_eq!(section_phase(&flat, rate, beat_s, 0.0), None);
}

/// Kicks at `times` (seconds): a 60 Hz thump with a click.
fn kicks(times: &[f64], secs: f64, rate: u32) -> Vec<f32> {
    let n = (secs * rate as f64) as usize;
    let mut x = vec![0.0f32; n];
    for &t in times {
        let start = (t * rate as f64) as usize;
        for i in 0..(rate as usize / 8) {
            if start + i >= n {
                break;
            }
            let tt = i as f64 / rate as f64;
            let thump = (std::f64::consts::TAU * 60.0 * tt).sin() * (-tt * 25.0).exp();
            let click = (std::f64::consts::TAU * 1500.0 * tt).sin() * (-tt * 300.0).exp();
            x[start + i] += (0.7 * thump + 0.2 * click) as f32;
        }
    }
    x
}

fn straight_times(bpm: f64, secs: f64, offset: f64) -> Vec<f64> {
    (0..)
        .map(|k| offset + k as f64 * 60.0 / bpm)
        .take_while(|t| *t < secs - 0.2)
        .collect()
}

/// Deterministic noise in −1…1.
fn noise(i: usize) -> f64 {
    let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    s ^= s >> 29;
    s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    s ^= s >> 32;
    (s % 20_001) as f64 / 10_000.0 - 1.0
}

#[test]
fn a_machine_beat_is_straight_and_mixable() {
    let rate = 22_050;
    let x = kicks(&straight_times(126.0, 120.0, 0.25), 120.0, rate);
    let q = measure(&x, rate, 126.03, 0.25).unwrap();
    assert!((q.bpm - 126.0).abs() < 0.01, "{}", q.bpm);
    let per = 60.0 / 126.0;
    let err = (q.offset - 0.25 + per / 2.0).rem_euclid(per) - per / 2.0;
    assert!(err.abs() < 0.004, "grid on the kicks: {:.1} ms", err * 1e3);
    assert!(q.straight(), "{}", q.describe());
    assert!(q.mixability > 0.85, "{}", q.describe());
    assert!(q.coverage > 0.95);
}

#[test]
fn a_human_drummer_is_loose() {
    let rate = 22_050;
    // ±15 ms scatter on every hit.
    let t: Vec<f64> = straight_times(120.0, 120.0, 0.3)
        .iter()
        .enumerate()
        .map(|(i, t)| t + noise(i) * 0.015)
        .collect();
    let q = measure(&kicks(&t, 120.0, rate), rate, 120.0, 0.3).unwrap();
    assert!(!q.straight(), "{}", q.describe());
    // (The robust scatter only counts hits within 8 ms: ±15 ms reads ~4.)
    assert!(q.jitter_ms > 3.0, "{}", q.describe());
    assert!(q.mixability < 0.6, "{}", q.describe());
}

#[test]
fn a_tempo_that_wanders_drifts() {
    let rate = 22_050;
    // 124 BPM that sways ±30 ms over the track (a slow sine).
    let t: Vec<f64> = straight_times(124.0, 150.0, 0.2)
        .iter()
        .map(|t| t + 0.030 * (t / 150.0 * std::f64::consts::TAU).sin())
        .collect();
    let q = measure(&kicks(&t, 150.0, rate), rate, 124.0, 0.2).unwrap();
    assert!(q.drift_ms > 15.0, "{}", q.describe());
    assert!(
        q.jitter_ms < 4.0,
        "the beat itself is clean: {}",
        q.describe()
    );
    assert!(q.mixability < 0.7, "{}", q.describe());
}

#[test]
fn a_breakdown_is_found_and_beats_still_fit() {
    let rate = 22_050;
    let all = straight_times(128.0, 120.0, 0.1);
    // No kick from 40 s to 55 s.
    let t: Vec<f64> = all
        .iter()
        .copied()
        .filter(|t| !(40.0..55.0).contains(t))
        .collect();
    let q = measure(&kicks(&t, 120.0, rate), rate, 128.0, 0.1).unwrap();
    assert!(q.straight(), "{}", q.describe());
    assert_eq!(q.beatless.len(), 1, "{:?}", q.beatless);
    assert!(
        (q.beatless[0][0] - 40.0).abs() < 1.0 && (q.beatless[0][1] - 55.0).abs() < 1.0,
        "{:?}",
        q.beatless
    );
}

#[test]
fn a_breakdown_ends_on_the_first_returning_kick() {
    // The drop after a breakdown is placed on its end: a late end put the
    // drop a bar after the groove was back (blind test 2026-10-19).
    let rate = 22_050;
    let all = straight_times(128.0, 120.0, 0.1);
    let back = *all.iter().find(|t| **t >= 55.0).unwrap();
    // The first bar back sits 12 ms late (a filter opening on the kick):
    // still the kick, not yet the steady one.
    let t: Vec<f64> = all
        .iter()
        .copied()
        .filter(|t| !(40.0..55.0).contains(t))
        .map(|t| if (55.0..back + 1.8).contains(&t) { t + 0.012 } else { t })
        .collect();
    let q = measure(&kicks(&t, 120.0, rate), rate, 128.0, 0.1).unwrap();
    assert_eq!(q.beatless.len(), 1, "{:?}", q.beatless);
    assert!(
        (f64::from(q.beatless[0][1]) - back).abs() < 0.05,
        "ends at {} s, the kick is back at {back:.3} s",
        q.beatless[0][1]
    );
}

#[test]
fn the_precise_pass_finds_the_real_tempo_and_keeps_the_phase() {
    let rate = 44_100;
    let bpm = 125.0;
    let x = kicks(&straight_times(bpm, 150.0, 0.4), 150.0, rate);
    let half: Vec<f32> = x.chunks_exact(2).map(|p| (p[0] + p[1]) * 0.5).collect();
    // A coarse grid 0.03 BPM off: 13 ms apart after five minutes.
    let mut coarse = measure(&half, rate / 2, bpm, 0.4).unwrap();
    coarse.bpm += 0.03;
    let p = precise(&x, rate, &coarse).unwrap();
    assert!(p.precise);
    assert!((p.bpm - bpm).abs() < 0.002, "{}", p.bpm);
    assert!(p.jitter_ms < 0.5 && p.drift_ms < 1.0, "{}", p.describe());
    let per = 60.0 / bpm;
    let err = (p.offset - 0.4 + per / 2.0).rem_euclid(per) - per / 2.0;
    assert!(err.abs() < 0.003, "on the attack: {:.1} ms", err * 1e3);
}

#[test]
fn a_drop_votes_for_the_downbeat() {
    // Bass returns on beats 64, 128, 192 (all ≡ 0 mod 4 from beat 0).
    let mut e = vec![1.0; 256];
    for (k, v) in e.iter_mut().enumerate() {
        if (48..64).contains(&k) || (112..128).contains(&k) || (176..192).contains(&k) {
            *v = 0.1;
        }
    }
    let shifted: Vec<f64> = e[3..].to_vec(); // grid starts on beat 3: drops at ≡ 1
    assert_eq!(downbeat_from_rises(&e).unwrap().0, 0);
    assert_eq!(downbeat_from_rises(&shifted).unwrap().0, 1);
}

#[test]
fn mixability_needs_every_property() {
    assert!(mixability(1.0, 1.0, 1.0) > 0.95);
    assert!(mixability(1.0, 40.0, 1.0) < 0.5, "drift alone ruins it");
    assert!(mixability(15.0, 1.0, 1.0) < 0.5, "jitter alone ruins it");
    assert!(mixability(1.0, 1.0, 0.3) < 0.8);
}

#[test]
fn arrangement_changes_find_the_downbeat() {
    let rate = 22_050;
    let bpm = 125.0;
    let beat = 60.0 / bpm;
    let times = straight_times(bpm, 150.0, 0.2);
    // The kick comes in on the first bar, two beats after the grid starts.
    let mut x = kicks(&times[2..], 150.0, rate);
    // Bars start on grid beat 2. Hats come and go every 8 bars, a bass
    // note every beat in the other 8: all on the first beat of a bar.
    for (k, t) in times.iter().enumerate() {
        if k < 2 {
            continue;
        }
        let bar = (k - 2) / 4;
        let start = (t * rate as f64) as usize;
        let len = (beat * rate as f64) as usize;
        for i in 0..len.min(x.len().saturating_sub(start)) {
            let tt = i as f64 / rate as f64;
            if (bar / 8) % 2 == 1 {
                x[start + i] += (noise(start + i) * 0.25 * (-tt * 60.0).exp()) as f32;
            } else {
                x[start + i] += (0.2 * (std::f64::consts::TAU * 98.0 * tt).sin()) as f32;
            }
        }
    }
    let q = measure(&x, rate, bpm, 0.2).unwrap();
    let first = ((0.2 - q.offset) / beat).round() as i64; // grid index of the track's first beat
    let (d, c) = (q.downbeat.unwrap() as i64, q.downbeat_confidence);
    assert_eq!((d - (first + 2)).rem_euclid(4), 0, "downbeat {d} ({c})");
    // Without a drop the bass-return cue stays silent: one cue, less sure.
    assert!(c > 0.4, "confidence {c}");
}

/// A dance track with a known beat: `kick` shape at every beat, a
/// sidechained bassline (louder in the low band than the kick) coming
/// in 25 ms after each kick, and off-beat hats. Returns (audio, first beat).
fn produced(kick: &str, bpm: f64, secs: f64, rate: u32) -> (Vec<f32>, f64) {
    let n = (secs * rate as f64) as usize;
    let mut x = vec![0.0f32; n];
    let beat = 60.0 / bpm;
    let first = 0.37;
    let mut k = 0;
    loop {
        let t0 = first + k as f64 * beat;
        if t0 > secs - 1.0 {
            break;
        }
        let at = (t0 * rate as f64) as usize;
        for i in 0..(beat * rate as f64) as usize {
            if at + i >= n {
                break;
            }
            let t = i as f64 / rate as f64;
            let kick_v = match kick {
                // A clicky kick: 2 ms click, fast 55 Hz body.
                "clicky" => {
                    (std::f64::consts::TAU * 3000.0 * t).sin() * (-t * 900.0).exp() * 0.5
                        + (std::f64::consts::TAU * 55.0 * t).sin() * (-t * 20.0).exp()
                }
                // A soft kick: no click, the body swells over 8 ms.
                "soft" => {
                    (std::f64::consts::TAU * 48.0 * t).sin()
                        * (t / 0.008).min(1.0)
                        * (-t * 14.0).exp()
                }
                // An 808: 1 ms attack, pitch falling 160 → 50 Hz.
                _ => {
                    let f = 50.0 + 110.0 * (-t * 30.0).exp();
                    let ph = std::f64::consts::TAU
                        * (50.0 * t + 110.0 / 30.0 * (1.0 - (-t * 30.0).exp()));
                    let _ = f;
                    ph.sin() * (t / 0.001).min(1.0) * (-t * 6.0).exp()
                }
            };
            // The bassline, ducked under the kick (6 dB below its peak,
            // as mixed): back from 25 ms.
            let duck = ((t - 0.025) / 0.02).clamp(0.0, 1.0);
            let bass = (std::f64::consts::TAU * 73.4 * t).sin() * 0.5 * duck;
            // Off-beat hat.
            let ht = t - beat / 2.0;
            // Hats have little low energy: differenced noise (a high-pass).
            let hat = if ht >= 0.0 {
                (noise(at + i) - noise(at + i + 1)) * 0.15 * (-ht * 150.0).exp()
            } else {
                0.0
            };
            x[at + i] += (kick_v + bass + hat) as f32;
        }
        k += 1;
    }
    (x, first)
}

/// A track whose low band carries several elements taking turns around
/// the beat: a click on every beat, and a bass note landing on a
/// rotating six-note figure around it (never on it). "15. Cloonee,
/// Prospa — Free Your Mind" measures the same shape — its clicks hold
/// the beat to −0.5 ms in every one of eleven sections while its low
/// onsets fall over 0, ±12, −25, +21 and +33 ms — and there the low band
/// is what gets layered: a build that timed each beat by its attack
/// instead measured drift 0.98 ms, and the AI then took a 10.9 s layer
/// over the track whose kicks landed **12 ms apart** (rehearsed pair,
/// score 36, "not acceptable"), where the unsure grid had kept it to a
/// clean cut at 82. So this is a lock, not a bug: the low band's wander
/// is real and it is what the AI must not layer over.
fn turning_bass_track(bpm: f64, secs: f64, rate: u32) -> Vec<f32> {
    let n = (secs * rate as f64) as usize;
    let mut x = vec![0.0f32; n];
    let beat = 60.0 / bpm;
    let first = 0.25;
    let mut place = |t: f64, hz: f64, amp: f64, decay: f64| {
        let start = (t * rate as f64) as usize;
        if start >= n {
            return;
        }
        let len = ((decay * 6.0 * f64::from(rate)) as usize).min(n - start);
        for i in 0..len {
            let tt = i as f64 / rate as f64;
            x[start + i] +=
                (amp * (-tt / decay).exp() * (std::f64::consts::TAU * hz * tt).sin()) as f32;
        }
    };
    let mut k = 0;
    loop {
        let t0 = first + k as f64 * beat;
        if t0 > secs - 1.0 {
            break;
        }
        // The attack on every beat, high enough to stay out of the low
        // band (the low band carries the bass alone here).
        place(t0, 4000.0, 0.3, 0.003);
        let offset = [-0.018, -0.012, -0.007, 0.007, 0.012, 0.018][k % 6];
        place(t0 + offset, 73.4, 0.8, 0.09);
        k += 1;
    }
    x
}

/// The low band of several elements taking turns is refused, and the
/// wander it reports stays past the planner's gate (`loose_ok`: both
/// tracks `drift ≤ 8 && jitter ≤ 5`), so the AI keeps to a cut or a
/// short layer. Measured through the analysis path on the BP Top 100
/// (October) folder: 11 of 100 tracks sit here, and the review that
/// timed the beats by their attack instead lost the protection — see
/// `turning_bass_track`.
#[test]
fn a_low_band_that_takes_turns_is_not_layered_over() {
    let rate = 22_050u32;
    let bpm = 128.0;
    let x = turning_bass_track(bpm, 140.0, rate);
    let q = measure(&x, rate, bpm, 0.25).unwrap();
    assert!(
        !q.straight() && q.drift_ms > 8.0,
        "the low band wanders, whatever the click does: {}",
        q.describe()
    );
    assert!(q.mixability < 0.35, "{}", q.describe());
}

/// A window is read part by part: a stretch that holds its grid and then
/// slips reads as the slip, where one figure for the whole record cannot
/// say anything about the 8-32 bars a mix actually lays over.
#[test]
fn a_window_is_read_part_by_part() {
    let rate = 22_050;
    let bpm = 126.0;
    let beat = 60.0 / bpm;
    let part = 16.0 * beat; // the 4-bar parts the planner measures
    // One signal: 16 s on the grid, then the same beat 20 ms late. The
    // late stretch is built on the *global* grid (16.44 s is the grid
    // beat there) so the join does not move the timebase — appending a
    // separately generated signal would.
    let mut ts = straight_times(bpm, 16.0, 0.25);
    ts.extend(straight_times(bpm, 28.4, 16.44 + 0.020));
    let mono = kicks(&ts, 28.0, rate);
    let w = window_grid(&mono, rate, bpm, 0.25, part, 0.0).expect("parts to measure");
    let step = (part * f64::from(rate)) as usize;
    let phases: Vec<Option<f64>> = mono
        .chunks(step)
        .enumerate()
        .map(|(j, c)| part_phase(c, rate, bpm, 0.25, j as f64 * part))
        .collect();
    assert!(
        w.parts >= 3 && w.coverage >= 0.9,
        "every part has a clear attack: {} parts, {:.2} covered — {phases:?}",
        w.parts,
        w.coverage
    );
    assert!(
        (w.drift_ms - 10.0).abs() < 4.0,
        "the window's parts wander 20 ms: {:.1} — {phases:?}",
        w.drift_ms
    );
    assert!(
        w.mixability < 0.85,
        "a window that slips is not straight: {:.2}",
        w.mixability
    );
    // A window holding one line reads like the line.
    let steady = kicks(&straight_times(bpm, 28.0, 0.25), 28.0, rate);
    let s = window_grid(&steady, rate, bpm, 0.25, part, 0.0).expect("parts to measure");
    assert!(s.drift_ms < 1.0, "one line: {:.1} ms", s.drift_ms);
    assert!(
        s.mixability > 0.9,
        "and it is straight: {:.2}",
        s.mixability
    );
    // A window with no attack at all has nothing to say.
    let quiet = vec![0.0f32; rate as usize * 20];
    assert!(window_grid(&quiet, rate, bpm, 0.25, part, 0.0).is_none());
}

#[test]
fn every_kind_of_kick_lands_on_the_same_beat() {
    let rate = 22_050;
    let bpm = 126.0;
    let beat = 60.0 / bpm;
    let mut errs = vec![];
    // The beat is where each kick starts: its attack, what a listener
    // times it by (a soft kick's swell, an 808's 1 ms attack alike).
    for (kind, energy_at) in [("clicky", 0.0), ("soft", 0.0), ("808", 0.0)] {
        let (x, first) = produced(kind, bpm, 90.0, rate);
        let q = measure(&x, rate, bpm, 0.0).unwrap_or_else(|| panic!("{kind}: no grid"));
        let err = ((q.offset - first) / beat + 0.5).rem_euclid(1.0) - 0.5;
        let ms = err * beat * 1e3;
        errs.push(ms - energy_at);
    }
    // The old detector was 40-200 ms off on these.
    assert!(
        errs.iter().all(|e| e.abs() < 1.5),
        "each within 1.5 ms of its start: {errs:?}"
    );
}
