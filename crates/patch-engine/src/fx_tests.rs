use super::*;

const RATE: u32 = 48_000;

#[test]
fn pool_prepares_and_reclaims_off_thread() {
    let pool = Pool::new();
    pool.request(0, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let (slot, fx) = loop {
        if let Some(prepared) = pool.take_ready() {
            break prepared;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "FX preparation timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(slot, 0);
    pool.request(0, false);
    assert!(pool.retire(slot, fx).is_ok(), "return queue has capacity");
    while pool.state[slot].load(Ordering::Acquire) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "FX reclamation timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn set(kind: Kind, on: bool, beats: f32, depth: f32) -> Settings {
    Settings {
        kind,
        on,
        beats,
        depth,
        ..Settings::default()
    }
}

fn run_blocks(
    fx: &mut Fx,
    s: Settings,
    beat: Beat,
    input: &[f32],
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    let (mut ol, mut or) = (vec![], vec![]);
    for chunk in input.chunks(block) {
        let (mut l, mut r) = (chunk.to_vec(), chunk.to_vec());
        fx.process(&mut l, &mut r, s, beat, RATE);
        ol.extend_from_slice(&l);
        or.extend_from_slice(&r);
    }
    (ol, or)
}

fn run(fx: &mut Fx, s: Settings, beat: Beat, input: &[f32]) -> Vec<f32> {
    run_blocks(fx, s, beat, input, 256).0
}

fn beat_120() -> Beat {
    Beat::free(RATE) // 0.5 s per beat
}

fn sine(hz: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * amp)
        .collect()
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

#[test]
fn echo_repeats_on_the_beat_and_spills_over() {
    let mut fx = Fx::new();
    let on = set(Kind::Echo, true, 0.5, 0.5);
    // Engage, then an impulse once the send is fully open.
    let mut x = vec![0.0f32; RATE as usize];
    x[1000] = 1.0;
    let y = run(&mut fx, on, beat_120(), &x);
    let around = |y: &[f32], at: usize| {
        y[at - 40..at + 40]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    // Half a beat at 120 BPM = 12 000 frames later, a repeat.
    assert!(
        around(&y, 13_000) > 0.1,
        "first repeat: {}",
        around(&y, 13_000)
    );
    assert!(
        around(&y, 25_000) > 0.02 && around(&y, 25_000) < around(&y, 13_000),
        "decaying"
    );
    assert!(around(&y, 7_000) < 1e-4, "nothing between repeats");
    // Switched off: the line keeps ringing (spill-over), no new input.
    let off = Settings { on: false, ..on };
    let mut x2 = vec![0.0f32; RATE as usize];
    x2[0] = 1.0; // ignored while off (after the 6 ms fade)
    let y2 = run(&mut fx, off, beat_120(), &x2);
    assert!(
        y2[13_000 - 200..13_000 + 200]
            .iter()
            .any(|v| v.abs() > 1e-3),
        "tail rings on"
    );
    assert!(fx.active(&off));
}

#[test]
fn reverb_rings_decays_and_stays_stable() {
    let mut fx = Fx::new();
    let on = set(Kind::Reverb, true, 1.0, 1.0);
    let mut x = vec![0.0f32; RATE as usize];
    for v in x.iter_mut().take(4800) {
        *v = 0.5;
    }
    let y = run(&mut fx, on, beat_120(), &x);
    assert!(
        y[RATE as usize / 2..].iter().any(|v| v.abs() > 1e-3),
        "a tail"
    );
    let off = Settings { on: false, ..on };
    let silent = vec![0.0f32; RATE as usize * 12];
    let t = run(&mut fx, off, beat_120(), &silent);
    assert!(t.iter().all(|v| v.is_finite() && v.abs() < 2.0), "stable");
    assert!(
        t[t.len() - 4800..].iter().all(|v| v.abs() < 1e-3),
        "and it dies away"
    );
    assert!(!fx.active(&off), "then costs nothing");
}

#[test]
fn trans_chops_on_the_grid() {
    let mut fx = Fx::new();
    let s = set(Kind::Trans, true, 0.25, 1.0);
    // Phase 0 at frame 0: open for 1/8 beat (3 000 frames), shut for 1/8.
    let y = run(&mut fx, s, beat_120(), &vec![1.0f32; 24_000]);
    assert!(y[1_500] > 0.95, "open: {}", y[1_500]);
    assert!(y[4_500] < 0.05, "shut: {}", y[4_500]);
    assert!(y[7_500] > 0.95, "open again: {}", y[7_500]);
    // Edges are smoothed: no jump bigger than a 2 ms slew allows.
    assert!(y.windows(2).all(|w| (w[1] - w[0]).abs() < 0.02));
}

#[test]
fn cycles_longer_than_a_beat_lock_to_the_grid() {
    // A 2-beat gate: open for the whole first beat, shut for the second.
    // (Wrapping the grid position per beat kept it open for good.)
    let mut fx = Fx::new();
    let s = set(Kind::Trans, true, 2.0, 1.0);
    let y = run(&mut fx, s, beat_120(), &vec![1.0f32; 48_000]);
    assert!(y[12_000] > 0.95, "beat 1 open: {}", y[12_000]);
    assert!(y[36_000] < 0.05, "beat 2 shut: {}", y[36_000]);
    // And on a real grid: two blocks, the second one beat later.
    let mut fx = Fx::new();
    let grid = |phase| Beat {
        phase,
        locked: true,
        ..beat_120()
    };
    let (mut l, mut r) = (vec![1.0f32; 512], vec![1.0f32; 512]);
    for _ in 0..40 {
        fx.process(&mut l, &mut r, s, grid(1.2), RATE);
        l.fill(1.0);
        r.fill(1.0);
    }
    fx.process(&mut l, &mut r, s, grid(1.2), RATE);
    assert!(
        l[256] < 0.05,
        "beat 1.2 of a 2-beat cut is in the shut half: {}",
        l[256]
    );
}

#[test]
fn the_quantum_does_not_change_the_sound() {
    let x = sine(220.0, 0.6, 48_000);
    for kind in Kind::ALL {
        let s = set(kind, true, 0.5, 0.8);
        // 123.4 BPM: no gate edge falls exactly on a frame (where
        // rounding could move it by one).
        let beat = Beat {
            frames: 48_000.0 * 60.0 / 123.4,
            per_frame: 123.4 / 60.0 / 48_000.0,
            ..beat_120()
        };
        let (a, _) = run_blocks(&mut Fx::new(), s, beat, &x, 64);
        let (b, _) = run_blocks(&mut Fx::new(), s, beat, &x, 1024);
        let worst = a
            .iter()
            .zip(&b)
            .fold(0.0f32, |m, (p, q)| m.max((p - q).abs()));
        assert!(
            worst < 1e-6,
            "{kind:?}: 64 vs 1024 frames differ by {worst}"
        );
    }
}

#[test]
fn on_off_is_click_free_at_the_smallest_quantum() {
    // A 32-frame quantum: the old per-block ramp jumped 1/32 per frame.
    // (Crush is left out: quantising is a step by design.)
    for kind in [
        Kind::Trans,
        Kind::Tremolo,
        Kind::Drive,
        Kind::Sweep,
        Kind::Pan,
    ] {
        let mut fx = Fx::new();
        let x = vec![0.5f32; 4_800];
        let (on, off) = (set(kind, true, 1.0, 1.0), set(kind, false, 1.0, 1.0));
        let mut y = run_blocks(
            &mut fx,
            on,
            Beat {
                phase: 0.75,
                ..beat_120()
            },
            &x,
            32,
        )
        .0;
        y.extend(
            run_blocks(
                &mut fx,
                off,
                Beat {
                    phase: 0.75,
                    ..beat_120()
                },
                &x,
                32,
            )
            .0,
        );
        let jump = y.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(jump < 0.006, "{kind:?}: {jump}");
    }
}

#[test]
fn changing_the_effect_while_it_plays_crossfades() {
    let mut fx = Fx::new();
    let x = vec![0.5f32; 9_600];
    let mut y = run_blocks(
        &mut fx,
        set(Kind::Trans, true, 1.0, 1.0),
        Beat {
            phase: 0.75,
            ..beat_120()
        },
        &x,
        64,
    )
    .0;
    y.extend(
        run_blocks(
            &mut fx,
            set(Kind::Crush, true, 1.0, 1.0),
            beat_120(),
            &x,
            64,
        )
        .0,
    );
    let jump = y.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(jump < 0.006, "switch jumped {jump}");
}

#[test]
fn depth_moves_glide() {
    let mut fx = Fx::new();
    let x = vec![0.5f32; 64];
    let mut y = vec![];
    for (i, _) in (0..100).enumerate() {
        // Hard depth jumps every block: the level glides instead.
        let d = if i % 20 < 10 { 0.0 } else { 1.0 };
        y.extend(run_blocks(&mut fx, set(Kind::Trans, true, 64.0, d), beat_120(), &x, 64).0);
    }
    let jump = y.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
    assert!(jump < 0.02, "depth zipper: {jump}");
}

#[test]
fn off_is_bit_exact_dry() {
    let x = sine(330.0, 0.8, 8_000);
    for kind in Kind::ALL {
        let mut fx = Fx::new();
        let z = run(&mut fx, set(kind, false, 4.0, 1.0), beat_120(), &x);
        assert_eq!(z, x, "{kind:?} off");
    }
}

#[test]
fn every_effect_is_bounded_and_audible() {
    let x = sine(180.0, 0.8, RATE as usize);
    for kind in Kind::ALL {
        let mut fx = Fx::new();
        let (l, r) = run_blocks(&mut fx, set(kind, true, 1.0, 1.0), beat_120(), &x, 256);
        assert!(
            l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 2.5),
            "{kind:?} bounded"
        );
        assert!(
            l.iter()
                .zip(&r)
                .zip(&x)
                .any(|((a, b), c)| (a - c).abs() > 0.05 || (b - c).abs() > 0.05),
            "{kind:?} audible"
        );
    }
}

#[test]
fn roll_repeats_the_slice_it_caught() {
    let mut fx = Fx::new();
    // ¼ beat at 120 BPM = 6 000 frames; a ramp as input.
    let s = fx_on(Kind::Roll, 0.25);
    let x: Vec<f32> = (0..24_000)
        .map(|i| (i % 6_000) as f32 / 6_000.0 * 0.5 + (i / 6_000) as f32 * 0.1)
        .collect();
    let y = run(&mut fx, s, beat_120(), &x);
    // After the first slice, frames repeat every 6 000 (away from edges).
    for k in [7_000usize, 9_500, 13_000, 20_000] {
        assert!(
            (y[k] - x[k % 6_000]).abs() < 1e-4,
            "{k}: repeats the caught slice: {} vs {}",
            y[k],
            x[k % 6_000]
        );
    }
}

fn fx_on(kind: Kind, beats: f32) -> Settings {
    set(kind, true, beats, 1.0)
}

#[test]
fn brake_slows_to_silence() {
    let mut fx = Fx::new();
    let x = sine(380.0, 0.5, 48_000);
    let y = run(&mut fx, fx_on(Kind::Brake, 1.0), beat_120(), &x);
    assert!(y[..2_000].iter().any(|v| v.abs() > 0.2), "starts at speed");
    assert!(
        y[30_000..].iter().all(|v| v.abs() < 1e-6),
        "stopped after the beat"
    );
    assert!(y.iter().all(|v| v.is_finite()));
}

#[test]
fn pingpong_bounces_left_then_right() {
    let mut fx = Fx::new();
    let s = set(Kind::PingPong, true, 0.25, 0.5);
    let mut l = vec![0.0f32; 20_000];
    let mut r = vec![0.0f32; 20_000];
    // Warm-up so the send is fully open, then an impulse.
    fx.process(&mut [0.0; 512], &mut [0.0; 512], s, beat_120(), RATE);
    l[0] = 1.0;
    r[0] = 1.0;
    for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        fx.process(cl, cr, s, beat_120(), RATE);
    }
    let peak =
        |v: &[f32], at: usize| v[at - 5..at + 5].iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(
        peak(&l, 6_000) > 0.3 && peak(&r, 6_000) < 1e-3,
        "first bounce left"
    );
    assert!(
        peak(&r, 12_000) > 0.3 && peak(&l, 12_000) < 1e-3,
        "then right"
    );
}

#[test]
fn sweep_is_unity_when_open_and_closes_on_the_off_beat() {
    // A 1-beat sweep: open at the beat, closed half a beat later.
    let mut fx = Fx::new();
    let s = set(Kind::Sweep, true, 1.0, 1.0);
    let x = sine(3_000.0, 0.5, 48_000);
    let y = run(&mut fx, s, beat_120(), &x);
    let on_beat = rms(&y[23_500..24_500]); // beat 2
    let off_beat = rms(&y[11_500..12_500]); // halfway
    assert!(
        (on_beat / rms(&x[23_500..24_500]) - 1.0).abs() < 0.15,
        "open ≈ unity: {on_beat}"
    );
    assert!(off_beat < on_beat * 0.05, "closed: {off_beat} vs {on_beat}");
    // Bass stays at unity through the sweep (a DJ filter, not a cut).
    let mut fx = Fx::new();
    let bass = vec![0.5f32; 48_000];
    let y = run(&mut fx, s, beat_120(), &bass);
    assert!(
        y[2_000..].iter().all(|v| (v - 0.5).abs() < 0.02),
        "DC at unity"
    );
}

#[test]
fn tremolo_is_loud_on_the_beat() {
    let mut fx = Fx::new();
    let y = run(
        &mut fx,
        set(Kind::Tremolo, true, 1.0, 1.0),
        beat_120(),
        &vec![0.5f32; 48_000],
    );
    assert!(y[24_000] > 0.49, "on the beat: {}", y[24_000]);
    assert!(y[36_000] < 0.01, "between: {}", y[36_000]);
}

#[test]
fn pan_swings_with_constant_power() {
    let mut fx = Fx::new();
    let x = vec![0.5f32; 48_000];
    let (l, r) = run_blocks(&mut fx, set(Kind::Pan, true, 1.0, 1.0), beat_120(), &x, 256);
    // A quarter beat in: fully one way; three quarters: the other.
    assert!(l[6_000] > r[6_000] * 4.0 || r[6_000] > l[6_000] * 4.0);
    assert!(
        (l[6_000] > r[6_000]) != (l[18_000] > r[18_000]),
        "swings back"
    );
    for i in (1_000..48_000).step_by(997) {
        let p = (l[i] * l[i] + r[i] * r[i]) / (2.0 * 0.25);
        assert!((p - 1.0).abs() < 0.03, "power at {i}: {p}");
    }
}

#[test]
fn drive_keeps_its_level_and_aliases_less() {
    let x = sine(1_000.0, 0.25, 48_000); // −12 dBFS
    for d in [0.0, 0.5, 1.0] {
        let mut fx = Fx::new();
        let y = run(&mut fx, set(Kind::Drive, true, 1.0, d), beat_120(), &x);
        let ratio = rms(&y[4_800..]) / rms(&x[4_800..]);
        assert!((0.7..1.42).contains(&ratio), "depth {d}: level ×{ratio}");
    }
    // A high tone, driven hard: ADAA versus clipping each sample.
    let hi = sine(7_919.0, 0.9, 48_000);
    let mut fx = Fx::new();
    let y = run(&mut fx, set(Kind::Drive, true, 1.0, 1.0), beat_120(), &hi);
    let naive: Vec<f32> = hi.iter().map(|v| clip(v * 10f32.powf(1.5))).collect();
    // Energy that isn't the tone or its (in-band) 3rd harmonic's image:
    // compare the spectra's floor between 1 and 6 kHz, where only aliases land.
    let floor = |v: &[f32]| {
        let mut e = 0.0f64;
        for hz in (1_100..6_000).step_by(100) {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (i, s) in v[8_000..40_000].iter().enumerate() {
                let a = std::f64::consts::TAU * hz as f64 * i as f64 / RATE as f64;
                re += *s as f64 * a.cos();
                im += *s as f64 * a.sin();
            }
            e += re * re + im * im;
        }
        e
    };
    let norm = |v: &[f32]| {
        v[8_000..40_000]
            .iter()
            .map(|x| (*x as f64).powi(2))
            .sum::<f64>()
    };
    let ours = floor(&y) / norm(&y);
    let theirs = floor(&naive) / norm(&naive);
    assert!(ours < theirs * 0.5, "aliases: {ours:e} vs naive {theirs:e}");
}

#[test]
fn chain_runs_in_series_on_one_knob() {
    // Echo then Trans: the echoes are chopped by the gate too.
    let mut s = set(Kind::Echo, true, 0.5, 1.0);
    s.set_kinds(&[Kind::Echo, Kind::Trans]);
    assert_eq!(s.kinds().collect::<Vec<_>>(), [Kind::Echo, Kind::Trans]);
    let mut chain = Fx::new();
    let x = sine(440.0, 0.5, 48_000);
    let y = run(&mut chain, s, beat_120(), &x);
    let mut echo = Fx::new();
    let e = run(&mut echo, set(Kind::Echo, true, 0.5, 1.0), beat_120(), &x);
    // In the gate's shut half (¾ beat in, past its 2 ms edge) the chain
    // is silent; in the open half it is the echo.
    assert!(rms(&y[18_500..23_500]) < 0.01 * rms(&e[18_500..23_500]));
    assert!((rms(&y[13_000..17_500]) / rms(&e[13_000..17_500]) - 1.0).abs() < 0.02);
    // Removing the second effect leaves plain echo (after the fade).
    let (a, b) = (
        run(&mut chain, set(Kind::Echo, true, 0.5, 1.0), beat_120(), &x),
        e,
    );
    assert!(rms(&a[40_000..]) > 0.5 * rms(&b[40_000..]));
}

#[test]
fn chains_pack_into_one_word() {
    let mut s = Settings::default();
    s.set_kinds(&[Kind::Sweep, Kind::Echo, Kind::Drive]);
    let mut t = Settings::default();
    t.unpack_kinds(s.pack_kinds());
    assert_eq!(
        t.kinds().collect::<Vec<_>>(),
        [Kind::Sweep, Kind::Echo, Kind::Drive]
    );
    // A lone kind is its own value (what older engines stored).
    t.unpack_kinds(Kind::Reverb as u32);
    assert_eq!(t.kinds().collect::<Vec<_>>(), [Kind::Reverb]);
}

#[test]
fn tails_never_go_denormal() {
    // Ring each recursive effect, then feed digital silence with it on.
    for kind in [
        Kind::Reverb,
        Kind::Phaser,
        Kind::Sweep,
        Kind::Echo,
        Kind::Flanger,
        Kind::Drive,
    ] {
        let mut fx = Fx::new();
        let s = set(kind, true, 0.25, 0.9);
        run(&mut fx, s, beat_120(), &sine(200.0, 0.8, 4_800));
        run(&mut fx, s, beat_120(), &vec![0.0; RATE as usize * 30]);
        let sl = &fx.slots[0];
        let states = sl
            .ap
            .iter()
            .flatten()
            .chain(&sl.ph_fb)
            .chain(sl.ladder.iter().flatten())
            .chain(&sl.echo_lp)
            .chain(&sl.echo_hp)
            .chain(&sl.drive_lp)
            .chain(sl.combs.iter().flatten().map(|c| &c.store))
            .chain(
                sl.combs
                    .iter()
                    .flatten()
                    .flat_map(|c| c.buf[..c.len].iter()),
            )
            .chain(
                sl.allpasses
                    .iter()
                    .flatten()
                    .flat_map(|a| a.buf[..a.len].iter()),
            );
        for v in states {
            assert!(
                *v == 0.0 || v.is_normal(),
                "{kind:?}: subnormal state {v:e}"
            );
        }
    }
}

/// Cost per effect, and the worst block while switching effects
/// (release build):
/// `cargo test --release -p patch-engine fx_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn fx_cost() {
    let cycle_ns = 512.0 / 48_000.0 * 1e9;
    let mut all = Settings::default();
    all.set_kinds(&[Kind::Echo, Kind::Sweep, Kind::Reverb]);
    let cases = Kind::ALL
        .into_iter()
        .map(|k| (k.name().to_string(), set(k, true, 1.0, 0.7)))
        .chain([(
            "echo+sweep+reverb".to_string(),
            Settings {
                on: true,
                depth: 0.7,
                ..all
            },
        )]);
    for (name, s) in cases {
        let mut fx = Fx::new();
        let (mut l, mut r) = (vec![0.3f32; 512], vec![-0.2f32; 512]);
        for _ in 0..200 {
            fx.process(&mut l, &mut r, s, beat_120(), RATE);
        }
        let t0 = std::time::Instant::now();
        let n = 4000;
        for _ in 0..n {
            fx.process(&mut l, &mut r, s, beat_120(), RATE);
        }
        let ns = t0.elapsed().as_nanos() as f64 / (n * 512) as f64;
        println!(
            "{name:18} {ns:6.2} ns/frame · {:5.2} % of a 512-frame cycle at 48 kHz",
            ns * 512.0 / cycle_ns * 100.0
        );
    }
    // Switching: the worst 64-frame block over 40 changes of effect.
    let mut fx = Fx::new();
    let (mut l, mut r) = (vec![0.3f32; 64], vec![-0.2f32; 64]);
    let mut worst = 0u128;
    for kind in Kind::ALL.iter().cycle().take(40) {
        for _ in 0..8 {
            let t0 = std::time::Instant::now();
            fx.process(&mut l, &mut r, fx_on(*kind, 1.0), beat_120(), RATE);
            worst = worst.max(t0.elapsed().as_nanos());
        }
    }
    println!(
        "worst block while switching: {:.1} µs (a 64-frame cycle is 1333 µs)",
        worst as f64 / 1e3
    );
}
