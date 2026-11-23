#[test]
fn solo_in_place_silences_the_others_and_mute_wins() {
    let p = Params::new(&["mic-1", "pattern", "deck-a"]);
    p.set_solo(1, true);
    assert_eq!(p.side_gains(0), [0.0, 0.0]);
    assert!(p.side_gains(1)[0] > 0.0);
    // Pattern is the only solo, so the unsoloed Booth is out.
    assert_eq!(p.booth_gain(), 0.0);
    assert_eq!(p.side_gains(2), [0.0, 0.0]);
    p.set_dj_solo(true);
    assert!(p.side_gains(2)[0] > 0.0);
    p.set_mute(1, true);
    assert_eq!(p.side_gains(1), [0.0, 0.0]);
    // The bus fader accepts +6 dB (about 2×, under the linear cap of 4).
    let lin = db_to_linear(6.0);
    assert!(lin > 1.5 && lin < 4.0, "{lin}");
    p.set_bus(0, Some(lin), None);
    assert!((p.bus_of(0).0 - lin).abs() < 1e-4);
}

#[test]
fn mic_solo_keeps_the_dj_booth_audible() {
    let p = Params::new(&["mic-1", "mic-2", "pattern", "deck-a", "app-1"]);
    p.set_solo(0, true);
    assert!(p.booth_gain() > 0.0);
    assert!(p.heard(2), "other engine programme sources remain in the mix");
    assert!(p.side_gains(3)[0] > 0.0, "the DJ deck remains in the mix");
    assert!(p.heard(4), "application audio remains in the mix");
    assert!(!p.heard(1), "unsoloed microphones stay out");
}

#[test]
fn callback_cost_percentiles_come_from_the_lifetime_histogram() {
    let p = Params::new(&["bus"]);
    // 900 cycles under 2^14 ns, 90 under 2^17, 9 under 2^20, 1 under 2^24.
    for (bucket, n) in [(14, 900), (17, 90), (20, 9), (24, 1)] {
        p.cost_hist[bucket].store(n, Ordering::Relaxed);
    }
    let (cycles, [p50, p95, p99, p999]) = p.cost_percentiles();
    assert_eq!(cycles, 1000);
    assert_eq!([p50, p95, p99, p999], [1 << 14, 1 << 17, 1 << 17, 1 << 20]);
}

use super::*;

fn sig(n: usize, seed: f32) -> Vec<f32> {
    (0..n)
        .map(|i| ((i as f32) * 0.01 + seed).sin() * 0.8)
        .collect()
}

/// The output delay follows the driver's mean fill, not the quantum
/// sawtooth it swings through, and a driver that reports none gives none.
#[test]
fn output_delay_is_the_drivers_mean_fill() {
    let mut d = 0.0;
    for cycle in 0..4000 {
        // 280…536 frames: an ALSA buffer refilled by 256 each cycle.
        d = smooth_delay(d, -(280 + (cycle % 2) * 256));
    }
    assert!((d - 408.0).abs() < 4.0, "{d}");
    assert_eq!(smooth_delay(d, 0), 0.0);
    assert_eq!(smooth_delay(0.0, 400), 400.0, "the first report counts at once");
}

fn clock(frames: u32) -> Clock {
    Clock {
        position: 4800,
        nsec: 1_000,
        rate: 48_000,
        frames,
        delay: 0,
        ..Default::default()
    }
}

#[test]
fn engine_exposes_four_deck_workers_and_extra_performance_lanes_start_silent() {
    let params = Params::new(&crate::daemon::CHANNELS);
    assert_eq!(params.decks.len(), 4);
    for (index, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
        let channel = params.channel(&format!("deck-{id}")).unwrap();
        assert_eq!(params.deck_of[channel], Some(index));
        let gain = load(&params.gain[channel]);
        if index >= 2 {
            assert_eq!(gain, 0.0, "extra source lane {id} must be silent initially");
        } else {
            assert_eq!(
                gain, 1.0,
                "standard DJ deck {id} keeps its existing startup gain"
            );
        }
    }
}

#[test]
fn stem_bridge_is_clean_by_default_and_fx_is_opt_in_for_each_send() {
    let p = Params::new(&["bus", "deck-a", "deck-b"]);
    assert!(!p.stem_bridge_fx.load(Ordering::Acquire));
    p.set_stem_bridge(0, 2, true);
    assert!(p.stem_bridge_on.load(Ordering::Acquire));
    assert!(!p.stem_bridge_fx.load(Ordering::Acquire));
    p.set_stem_bridge_fx(true);
    assert!(p.stem_bridge_fx.load(Ordering::Acquire));
    // A new clean send never inherits an old optional FX choice.
    p.set_stem_bridge(1, 2, true);
    assert!(!p.stem_bridge_fx.load(Ordering::Acquire));
    assert!(p.set_stem_bridge_depth(0.72).is_ok());
    assert!(p.set_stem_bridge_depth(1.01).is_err());
    assert!(p.set_stem_bridge_depth(f32::NAN).is_err());
    assert!((f32::from_bits(p.stem_bridge_depth.load(Ordering::Acquire)) - 0.72).abs() < 1e-6);
}

#[test]
fn bridge_fx_depth_is_independent_of_source_deck_settings() {
    let source = fx::Settings {
        depth: 0.18,
        beats: 2.0,
        ..Default::default()
    };
    let bridge = stem_bridge_fx_settings(source, true, 0.73);
    assert!(bridge.on);
    assert!((bridge.depth - 0.73).abs() < 1e-6);
    assert_eq!(bridge.beats, 2.0);
    assert_eq!(source.depth, 0.18);
    assert!(!stem_bridge_fx_settings(source, false, 0.73).on);
}

#[test]
fn sequencer_enters_the_program_and_respects_system_mute() {
    let p = Arc::new(Params::new(&["bus", "sequencer"]));
    p.sequencer
        .request(&serde_json::json!({"playing":true}))
        .unwrap();
    let mut rt = Rt::new(p.clone());
    let mut l = vec![0.0; 4096];
    let mut r = l.clone();
    rt.process(&clock(4096), &[], [&mut l, &mut r], None);
    assert!(l.iter().any(|x| x.abs() > 0.01));
    p.set_system(1.0, 1.0, true);
    for _ in 0..4 {
        rt.process(&clock(4096), &[], [&mut l, &mut r], None);
    }
    assert!(l.iter().all(|x| x.abs() < 0.00001));
    assert!(r.iter().all(|x| x.abs() < 0.00001));
}

#[test]
fn the_pattern_and_the_step_sequencer_play_on_their_own_channels() {
    let p = Arc::new(Params::new(&["bus", "sequencer", "pattern"]));
    let (seq, pat) = (
        p.channel("sequencer").unwrap(),
        p.channel("pattern").unwrap(),
    );
    assert_eq!(p.internal_channel(seq), Some(Internal::Sequencer));
    assert_eq!(p.internal_channel(pat), Some(Internal::Pattern));
    let mut rt = Rt::new(p.clone());
    let kick = |p: &Params, sample: i64| {
        unsafe { p.sequencer.pattern.push(patch_pattern::rt::RtEvent {
            sample,
            kind: patch_pattern::rt::RtKind::Voice,
            index: 0,
            value: 1.0,
            epoch: 0,
            ..Default::default()
        }) };
    };
    // A kick from the Pattern Unit (it pushes into the sequencer's
    // queue), the step sequencer stopped.
    kick(&p, 4_800);
    let mut l = vec![0.0; 8192];
    let mut r = l.clone();
    rt.process(&clock(8192), &[], [&mut l, &mut r], None);
    let mut blocks = vec![];
    unsafe { p.meters.drain(|b| blocks.push(b)) };
    let b = blocks.last().unwrap();
    assert!(
        b.ch_peak[pat][0].max(b.ch_peak[pat][1]) > 0.01,
        "the pattern channel carries the voice"
    );
    assert!(
        b.ch_peak[seq][0].max(b.ch_peak[seq][1]) < 1e-6,
        "the step sequencer's channel stays silent: {:?}",
        b.ch_peak[seq]
    );
    assert!(l.iter().any(|x| x.abs() > 0.01), "and the program hears it");
    // Muting the pattern's strip silences it, and only it. Its post-fader
    // gain goes to 0 (the step sequencer's stays at unity); the program
    // goes quiet once the master limiter's look-ahead has drained.
    p.set_mute(pat, true);
    kick(&p, 4_800);
    for _ in 0..4 {
        rt.process(&clock(8192), &[], [&mut l, &mut r], None);
    }
    l.fill(0.0);
    r.fill(0.0);
    rt.process(&clock(8192), &[], [&mut l, &mut r], None);
    let mut blocks = vec![];
    unsafe { p.meters.drain(|b| blocks.push(b)) };
    // A one-shot kick fed into a muted strip still meters pre-fader while it
    // sounds (0.34 → 0.15 → 0.05 → 0.017 → 0.006 across five blocks: the
    // later values are the decay, not the routing).
    let peak = blocks
        .iter()
        .map(|b| b.ch_peak[pat][0].max(b.ch_peak[pat][1]))
        .fold(0.0f32, f32::max);
    assert!(
        peak > 0.01,
        "the strip still meters what it is fed (pre-fader)"
    );
    // The fader has finished its ramp by the last of those blocks.
    let b = blocks.last().unwrap();
    assert_eq!(
        b.ch_gain[pat],
        [0.0, 0.0],
        "muted: the pattern's fader is at 0"
    );
    assert_eq!(
        b.ch_gain[seq],
        [1.0, 1.0],
        "the step sequencer's fader is untouched"
    );
    assert!(
        l.iter().all(|x| x.abs() < 1e-6),
        "nothing of the pattern reaches the program (max {})",
        l.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    );
    assert!(!p.is_muted(seq), "only the pattern's channel was muted");
}

#[test]
fn the_master_eq_shapes_the_program() {
    // A −12 dB high shelf at 2 kHz: a 5 kHz tone comes out ~12 dB down,
    // a 100 Hz tone untouched; flat again, it passes as it was.
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut st = patch_core::eq::EqState::default();
    st.bands[6] = patch_core::eq::EqBand {
        on: true,
        kind: patch_core::eq::EqKind::HighShelf,
        freq: 2000.0,
        gain_db: -12.0,
        q: 0.71,
    };
    p.master_eq.set(&st);
    let tone = |f: f64| -> f64 {
        let mut rt = Rt::new(p.clone());
        let x: Vec<f32> = (0..1024)
            .map(|i| ((std::f64::consts::TAU * f * i as f64 / 48_000.0).sin() * 0.5) as f32)
            .collect();
        let mut last = 0.0;
        for _ in 0..12 {
            let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
            rt.process(&clock(1024), &[Some(&x), Some(&x)], [&mut l, &mut r], None);
            last = (l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 1024.0).sqrt();
        }
        20.0 * (last / (0.5 / 2f64.sqrt())).log10()
    };
    assert!(
        (tone(5000.0) + 12.0).abs() < 0.8,
        "5 kHz: {:.2} dB",
        tone(5000.0)
    );
    assert!(tone(100.0).abs() < 0.3, "100 Hz: {:.2} dB", tone(100.0));
    p.master_eq.set(&patch_core::eq::EqState::default());
    assert!(tone(5000.0).abs() < 0.01, "flat again");
}

#[test]
fn a_channel_eq_shapes_only_its_channel() {
    // A −12 dB high shelf on the bus (apps): its 5 kHz comes out 12 dB
    // down; the same tone on the deck channel is untouched.
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut st = patch_core::eq::EqState::default();
    st.bands[6] = patch_core::eq::EqBand {
        on: true,
        kind: patch_core::eq::EqKind::HighShelf,
        freq: 2000.0,
        gain_db: -12.0,
        q: 0.71,
    };
    p.channel_eq[0].set(&st);
    let x: Vec<f32> = (0..1024)
        .map(|i| ((std::f64::consts::TAU * 5000.0 * i as f64 / 48_000.0).sin() * 0.25) as f32)
        .collect();
    let silent = vec![0.0f32; 1024];
    let level = |bus: bool| -> f64 {
        let mut rt = Rt::new(p.clone());
        let mut last = 0.0;
        for _ in 0..12 {
            let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
            let ins = if bus {
                [
                    Some(&x[..]),
                    Some(&x[..]),
                    Some(&silent[..]),
                    Some(&silent[..]),
                ]
            } else {
                [
                    Some(&silent[..]),
                    Some(&silent[..]),
                    Some(&x[..]),
                    Some(&x[..]),
                ]
            };
            rt.process(&clock(1024), &ins, [&mut l, &mut r], None);
            last = (l.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 1024.0).sqrt();
        }
        20.0 * (last / (0.25 / 2f64.sqrt())).log10()
    };
    assert!(
        (level(true) + 12.0).abs() < 0.8,
        "bus: {:.2} dB",
        level(true)
    );
    assert!(level(false).abs() < 0.05, "deck: {:.2} dB", level(false));
}

#[test]
fn voice_processing_works_on_its_channel_only() {
    // A quiet room tone (−60 dBFS) on a mic channel with its gate on:
    // turned down ~30 dB. The same tone on another channel: untouched.
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    p.set_voice(
        1,
        voice::Settings {
            on: true,
            hpf_hz: 0.0,
            gate_db: Some(-50.0),
            deess: 0.0,
            comp: 0.0,
            expander_db: None,
            noise_db: None,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
        },
    );
    assert_eq!(p.voice_of(1).gate_db, Some(-50.0));
    assert!(!p.voice_of(0).on);
    let x: Vec<f32> = (0..1024)
        .map(|i| ((std::f64::consts::TAU * 200.0 * i as f64 / 48_000.0).sin() * 0.001) as f32)
        .collect();
    let silent = vec![0.0f32; 1024];
    let level = |mic: bool| -> f64 {
        let mut rt = Rt::new(p.clone());
        let mut last = 0.0;
        for _ in 0..60 {
            let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
            // A mono mic: only its left port linked.
            let ins = if mic {
                [Some(&silent[..]), Some(&silent[..]), Some(&x[..]), None]
            } else {
                [Some(&x[..]), Some(&x[..]), Some(&silent[..]), None]
            };
            rt.process(&clock(1024), &ins, [&mut l, &mut r], None);
            last = (r.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / 1024.0).sqrt();
        }
        20.0 * (last.max(1e-12) / (0.001 / 2f64.sqrt())).log10()
    };
    assert!(
        level(true) < -25.0,
        "mic: {:.1} dB (and on both sides)",
        level(true)
    );
    assert!(level(false).abs() < 0.05, "bus: {:.2} dB", level(false));
}

#[test]
fn the_system_volume_comes_after_the_recording() {
    // With the system volume after the tap: the program (recordings,
    // streams) keeps its level, what leaves for the device follows the
    // volume and the mute. Without: both follow it (the old way).
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let x: Vec<f32> = (0..1024)
        .map(|i| ((std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin() * 0.25) as f32)
        .collect();
    let silent = vec![0.0f32; 1024];
    // `patch.program` (desktop-audio recorders) takes the same tap.
    let tap = std::cell::Cell::new(0.0f32);
    let run = || -> (f32, f32) {
        let mut rt = Rt::new(p.clone());
        let (mut out, mut rec) = (0.0, 0.0);
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
            rt.process(
                &clock(1024),
                &[Some(&silent), Some(&silent), Some(&x), Some(&x)],
                [&mut l, &mut r],
                None,
            );
            out = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            rec = 0.0;
            unsafe { p.program.drain(|v| rec = f32::max(rec, v.abs())) };
            let [tl, tr] = rt.program_audio(1024);
            tap.set(tl.iter().chain(tr).fold(0.0f32, |m, v| m.max(v.abs())));
        }
        (out, rec)
    };
    p.set_system(0.5, 0.5, false);
    p.system_post.store(true, Ordering::Relaxed);
    let (out, rec) = run();
    assert!((rec - 0.25).abs() < 1e-3, "recorded at full level: {rec}");
    assert!((out - 0.125).abs() < 1e-3, "heard at the volume: {out}");
    p.set_system(0.5, 0.5, true);
    let (out, rec) = run();
    assert!(
        out < 1e-6 && (rec - 0.25).abs() < 1e-3,
        "muted: silent out ({out}), recorded ({rec})"
    );
    assert!((tap.get() - 0.25).abs() < 1e-3, "program source while muted: {}", tap.get());
    p.system_post.store(false, Ordering::Relaxed);
    p.set_system(0.5, 0.5, false);
    let (out, rec) = run();
    assert!(
        (out - 0.125).abs() < 1e-3 && (rec - 0.125).abs() < 1e-3,
        "the old way: both at the volume ({out}, {rec})"
    );
}

#[test]
fn unity_centre_crossfader_preserves_a_single_deck_in_the_audio_mix() {
    let p = Arc::new(Params::new(&["deck-a", "deck-b"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let signal = vec![0.1f32; 256];
    let silent = vec![0.0f32; 256];
    let render = |position: f32| {
        let mixer = crate::session::Mixer {
            xfader: position, auto_gain: false, ..Default::default()
        };
        for (i, gain) in mixer.gains_db().into_iter().enumerate() {
            p.set_gain(i, db_to_linear(gain));
        }
        let mut rt = Rt::new(p.clone());
        let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
        for _ in 0..4 {
            rt.process(&clock(256),
                &[Some(&signal), Some(&signal), Some(&silent), Some(&silent)],
                [&mut left, &mut right], None);
        }
        left.iter().copied().fold(0.0f32, f32::max)
    };
    let side = render(0.0);
    let centre = render(0.5);
    assert!((side - 0.1).abs() < 1e-5);
    assert!((centre / side - 1.0).abs() < 1e-5,
        "one deck must not dip at the centre: side={side}, centre={centre}");
    assert!(render(1.0) < 1e-6, "the opposite end fully closes deck A");
}

#[test]
fn the_dj_booth_is_one_channel_on_the_mix() {
    // The same tone on the bus and on deck A: the DJ Booth's fader (-6 dB)
    // and mute act on the deck only; its meter measures the deck's sum.
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let x: Vec<f32> = (0..1024)
        .map(|i| ((std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin() * 0.25) as f32)
        .collect();
    let silent = vec![0.0f32; 1024];
    let run = |bus: bool, deck: bool| -> (f32, [f32; 2], [f32; 2]) {
        let mut rt = Rt::new(p.clone());
        let (mut last, mut dj) = (0.0, [0.0; 2]);
        let mut deck_gain = [0.0; 2];
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
            let (b, d) = (
                if bus { &x } else { &silent },
                if deck { &x } else { &silent },
            );
            rt.process(
                &clock(1024),
                &[Some(b), Some(b), Some(d), Some(d)],
                [&mut l, &mut r],
                None,
            );
            last = l.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            unsafe { p.meters.drain(|b| {
                dj = b.dj_peak;
                deck_gain = b.ch_gain[1];
            }) };
        }
        (last, dj, deck_gain)
    };
    p.set_dj_group(0.5, false);
    let (deck, dj, deck_gain) = run(false, true);
    assert_eq!(
        deck_gain, [1.0; 2],
        "booth volume must not change the deck meter gain"
    );
    assert!((deck - 0.125).abs() < 1e-3, "deck at -6 dB: {deck}");
    assert!((dj[0] - 0.125).abs() < 1e-3, "the group meter: {dj:?}");
    let (bus, dj, _) = run(true, false);
    assert!(
        (bus - 0.25).abs() < 1e-3 && dj[0] < 1e-6,
        "the bus untouched, nothing in the group"
    );
    p.set_dj_group(1.0, true);
    let (deck, _, deck_gain) = run(false, true);
    assert_eq!(
        deck_gain, [1.0; 2],
        "booth mute must not mute the upstream deck meter"
    );
    assert!(deck < 1e-4, "muted: {deck}");
    p.set_dj_group(1.0, false);
}

#[test]
fn the_master_never_goes_over() {
    // Two loud channels summed (+5 dB over full scale at the peaks):
    // the default engine keeps the program under the limiter ceiling.
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    let mut rt = Rt::new(p.clone());
    let ceiling = 10f32.powf(MASTER_CEILING_DBFS / 20.0);
    let x = sig(1024, 0.9);
    let mut worst = 0.0f32;
    for _ in 0..20 {
        let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
        rt.process(
            &clock(1024),
            &[Some(&x), Some(&x), Some(&x), Some(&x)],
            [&mut l, &mut r],
            None,
        );
        worst = l.iter().chain(&r).map(|v| v.abs()).fold(worst, f32::max);
    }
    assert!(
        worst <= ceiling + 1e-6,
        "program peak {worst} over {ceiling}"
    );
    assert!(worst > ceiling * 0.9, "still loud ({worst})");
}

/// Not a test: this binary is the plugin host for the test below.
#[test]
fn plugin_host_entry_for_engine_tests() {
    if std::env::var_os(patch_plugin::chain::HOST_AUDIO).is_some() {
        patch_plugin::chain::serve().unwrap();
        std::process::exit(0);
    }
}

#[test]
fn installed_box_of_bass_runs_in_the_patch_master_insert() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bundle = patch_plugin::bundle_path(&home);
    if patch_plugin::scan::binary(&bundle).is_none() {
        eprintln!("Box Of Bass is not installed; skipping native VST3 engine test");
        return;
    }
    let params = Arc::new(Params::new(&["bus"]));
    params.master_limiter.store(false, Ordering::Relaxed);
    let info = patch_plugin::scan::bundle_info(&bundle);
    let mut rack = patch_plugin::RackConfig::default();
    let id = rack.add(&bundle, &info).unwrap();
    rack.slot_mut(id).unwrap().active = true;
    patch_plugin::chain::set_host_command(vec![
        std::env::current_exe().unwrap().into_os_string(),
        "plugin_host_entry_for_engine_tests".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ]);
    let mut rt = Rt::new_with_plugin_config(params, Some(rack));
    let input = (0..256)
        .map(|i| (std::f32::consts::TAU * 55.0 * i as f32 / 48_000.0).sin() * 0.02)
        .collect::<Vec<_>>();
    // The chain loads off the audio thread, then joins: play quanta until
    // the plugin is heard.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut changed = false;
    while !changed && std::time::Instant::now() < deadline {
        let mut left = input.clone();
        let mut right = input.clone();
        rt.process(&clock(256), &[Some(&input), Some(&input)], [&mut left, &mut right], None);
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        changed = left.iter().zip(&input).any(|(sample, dry)| (*sample - dry).abs() > 1e-4);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(changed, "Box Of Bass never reached the master");
}

/// A plugin placed on one channel processes that channel and nothing
/// else: the other channel's sound reaches the master untouched.
#[test]
fn installed_box_of_bass_runs_on_one_channel_only() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bundle = patch_plugin::bundle_path(&home);
    if patch_plugin::scan::binary(&bundle).is_none() {
        eprintln!("Box Of Bass is not installed; skipping native VST3 engine test");
        return;
    }
    let params = Arc::new(Params::new(&["bus", "app-1"]));
    params.master_limiter.store(false, Ordering::Relaxed);
    let info = patch_plugin::scan::bundle_info(&bundle);
    let mut rack = patch_plugin::RackConfig::default();
    let id = rack.add(&bundle, &info).unwrap();
    let slot = rack.slot_mut(id).unwrap();
    slot.active = true;
    slot.place = patch_plugin::Place::parse("app-1");
    patch_plugin::chain::set_host_command(vec![
        std::env::current_exe().unwrap().into_os_string(),
        "plugin_host_entry_for_engine_tests".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ]);
    let mut rt = Rt::new_with_plugin_config(params, Some(rack));
    let tone = |hz: f32| {
        (0..256)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.02)
            .collect::<Vec<_>>()
    };
    let (bus, app) = (tone(375.0), tone(55.0));
    let silent = vec![0.0f32; 256];
    let dry: Vec<f32> = bus.iter().zip(&app).map(|(a, b)| a + b).collect();
    let play = |rt: &mut Rt, app: &[f32]| {
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        rt.process(&clock(256), &[Some(&bus), Some(&bus), Some(app), Some(app)], [&mut left, &mut right], None);
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        left
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut changed = false;
    while !changed && std::time::Instant::now() < deadline {
        let left = play(&mut rt, &app);
        changed = left.iter().zip(&dry).any(|(sample, dry)| (*sample - dry).abs() > 1e-4);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(changed, "Box Of Bass never reached app-1");
    // app-1 silent: what is left is the bus, as it was sent.
    let mut worst = f32::MAX;
    for _ in 0..200 {
        let left = play(&mut rt, &silent);
        worst = left.iter().zip(&bus).map(|(s, d)| (s - d).abs()).fold(0.0, f32::max);
        if worst < 1e-5 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(worst < 1e-5, "the bus was changed too ({worst})");
}

/// A plugin on AUX 1 processes what is sent there; the master is left as
/// it was.
#[test]
fn installed_box_of_bass_runs_on_aux_1_only() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let bundle = patch_plugin::bundle_path(&home);
    if patch_plugin::scan::binary(&bundle).is_none() {
        eprintln!("Box Of Bass is not installed; skipping native VST3 engine test");
        return;
    }
    let params = Arc::new(Params::new(&["bus"]));
    params.master_limiter.store(false, Ordering::Relaxed);
    params.set_send(0, 0, 1.0);
    let info = patch_plugin::scan::bundle_info(&bundle);
    let mut rack = patch_plugin::RackConfig::default();
    let id = rack.add(&bundle, &info).unwrap();
    let slot = rack.slot_mut(id).unwrap();
    slot.active = true;
    slot.place = patch_plugin::Place::Aux(1);
    patch_plugin::chain::set_host_command(vec![
        std::env::current_exe().unwrap().into_os_string(),
        "plugin_host_entry_for_engine_tests".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ]);
    let mut rt = Rt::new_with_plugin_config(params, Some(rack));
    let input = (0..256)
        .map(|i| (std::f32::consts::TAU * 55.0 * i as f32 / 48_000.0).sin() * 0.02)
        .collect::<Vec<_>>();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut changed = false;
    while !changed && std::time::Instant::now() < deadline {
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        rt.process(&clock(256), &[Some(&input), Some(&input)], [&mut left, &mut right], None);
        assert_eq!(left, input, "the master passes untouched");
        let aux = rt.aux_audio(0, 256).expect("AUX 1 is fed")[0].to_vec();
        changed = aux.iter().zip(&input).any(|(sample, dry)| (*sample - dry).abs() > 1e-4);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(changed, "Box Of Bass never reached AUX 1");
}

#[test]
fn unity_passes_bit_exact() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    let (l_in, r_in) = (sig(1024, 0.1), sig(1024, 0.9));
    let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
    rt.process(
        &clock(1024),
        &[Some(&l_in), Some(&r_in)],
        [&mut l, &mut r],
        None,
    );
    assert_eq!(l, l_in);
    assert_eq!(r, r_in);
}

#[test]
fn all_sixteen_inputs_mix_and_mute_independently() {
    let p = Arc::new(Params::new(&crate::daemon::CHANNELS));
    p.master_limiter.store(false, Ordering::Relaxed);
    p.system_post.store(true, Ordering::Relaxed);
    let signals: Vec<Vec<f32>> = (1..=16).map(|i| vec![i as f32 * 0.001; 256]).collect();
    let mut ins = vec![None; p.channels() * 2];
    for i in 1..=16 {
        let c = p.channel(&format!("mic-{i}")).unwrap();
        ins[c * 2] = Some(signals[i - 1].as_slice());
        ins[c * 2 + 1] = Some(signals[i - 1].as_slice());
    }
    let mut rt = Rt::new(p.clone());
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(l.iter().chain(&r).all(|x| (*x - 0.136).abs() < 1e-6));
    p.set_mute(p.channel("mic-16").unwrap(), true);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(l.iter().chain(&r).all(|x| (*x - 0.120).abs() < 1e-6));
}

#[test]
fn input_capture_has_its_own_mix_without_speaker_feedback_or_system_attenuation() {
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.system_post.store(true, Ordering::Relaxed);
    p.input_monitor[1].store(false, Ordering::Relaxed);
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut rt = Rt::new(p.clone());
    let bus = vec![0.2; 256];
    let mic = vec![0.1; 256];
    let ins = [Some(bus.as_slice()), Some(bus.as_slice()), Some(mic.as_slice()), Some(mic.as_slice())];
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(l.iter().all(|x| (*x - 0.2).abs() < 1e-6), "mic must not reach speakers");
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.1).abs() < 1e-6));
    p.set_system(0.01, 0.01, true);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(l.iter().all(|x| x.abs() < 1e-6));
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.1).abs() < 1e-6));
    p.input_main.store(true, Ordering::Relaxed);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.2).abs() < 1e-6));
}

#[test]
fn a_remote_voice_stays_out_of_the_input_mix_but_plays_in_the_program() {
    // mic-1 your microphone, mic-2 the call's far end (mix-minus).
    let p = Arc::new(Params::new(&["bus", "mic-1", "mic-2"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    p.input_monitor[1].store(false, Ordering::Relaxed);
    p.input_send[2].store(false, Ordering::Relaxed);
    let mut rt = Rt::new(p.clone());
    let silent = vec![0.0; 256];
    let mine = vec![0.1; 256];
    let far = vec![0.3; 256];
    let ins = [Some(silent.as_slice()), Some(silent.as_slice()), Some(mine.as_slice()), Some(mine.as_slice()),
        Some(far.as_slice()), Some(far.as_slice())];
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.1).abs() < 1e-6), "only your voice goes out");
    assert!(l.iter().all(|x| (*x - 0.3).abs() < 1e-6), "the far end plays in the program");
    // Switched back in: ramped over one block, then whole.
    p.input_send[2].store(true, Ordering::Relaxed);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    let ramp = rt.input_audio(256)[0].to_vec();
    assert!(ramp.windows(2).all(|w| w[1] >= w[0] - 1e-6) && ramp[0] < 0.11 && ramp[255] > 0.39);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.4).abs() < 1e-6));
}

#[test]
fn a_remote_voice_never_feeds_a_send_whatever_its_knob_says() {
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    p.input_send[1].store(false, Ordering::Relaxed); // mic-1: a remote voice
    p.set_send(1, 0, 1.0); // AUX 1 turned up by hand
    let silent = vec![0.0f32; 256];
    let far = vec![0.3f32; 256];
    let mut rt = Rt::new(p.clone());
    let ins = [Some(&silent[..]), Some(&silent[..]), Some(&far[..]), Some(&far[..])];
    for _ in 0..3 {
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    }
    assert!(
        rt.aux_audio(0, 256).is_none_or(|a| a.iter().flat_map(|s| s.iter()).all(|x| x.abs() < 1e-6)),
        "AUX 1 leaves for the browser: the far end must not be on it"
    );
    // Back into the mix: the send works again.
    p.input_send[1].store(true, Ordering::Relaxed);
    for _ in 0..3 {
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    }
    assert!(rt.aux_audio(0, 256).is_some_and(|a| a[0].iter().all(|x| (*x - 0.3).abs() < 1e-5)));
}

#[test]
fn the_whole_program_into_patch_input_is_n_minus_one_with_a_remote_voice() {
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    p.input_main.store(true, Ordering::Relaxed);
    p.input_send[1].store(false, Ordering::Relaxed);
    let music = vec![0.2f32; 256];
    let far = vec![0.3f32; 256];
    let mut rt = Rt::new(p.clone());
    let ins = [Some(&music[..]), Some(&music[..]), Some(&far[..]), Some(&far[..])];
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(l.iter().all(|x| (*x - 0.5).abs() < 1e-5), "the program has both");
    assert!(
        rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.2).abs() < 1e-5),
        "Patch Input: the program without the far end"
    );
    // No remote voice: the whole program, as before.
    p.input_send[1].store(true, Ordering::Relaxed);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    assert!(rt.input_audio(256).into_iter().flatten().all(|x| (*x - 0.5).abs() < 1e-5));
}

#[test]
fn ramp_reaches_target_without_steps() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    p.set_mute(0, true); // fade to silence over one block
    let ones = vec![1.0f32; 256];
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    rt.process(
        &clock(256),
        &[Some(&ones), Some(&ones)],
        [&mut l, &mut r],
        None,
    );
    assert_eq!(l[0], 1.0, "starts at the old gain");
    assert!(l.windows(2).all(|w| w[1] <= w[0] + 1e-7), "monotonic fade");
    assert!(l[255] < 0.01, "ends near the new gain: {}", l[255]);
}

#[test]
fn a_change_made_while_a_move_is_due_lands_on_the_moves_sample() {
    // The AI posts a fader cut ahead of its beat; a mixer refresh in
    // between (a trim, a read) must not play it early, only update it.
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut rt = Rt::new(p.clone());
    let ones = vec![1.0f32; 1024];
    let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
    let c = clock(1024);
    rt.process(&c, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let next = c.position + 1024;
    assert!(!p.retarget_timed_gain(0, 0.5), "nothing due: a plain change");
    assert!(p.post_at(next + 100, 0, TIMED_GAIN, 0.4));
    assert!(p.post_at(next + 600, 0, TIMED_GAIN, 0.0));
    assert!(p.retarget_timed_gain(0, 0.25), "the latest due move takes it");
    let c2 = Clock {
        position: next,
        ..c
    };
    rt.process(&c2, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    assert!(l[..100].iter().all(|v| (*v - 1.0).abs() < 1e-6), "not early");
    assert!((l[400] - 0.4).abs() < 1e-6, "the earlier step keeps its value");
    assert!((l[600 + TIMED_RAMP + 1] - 0.25).abs() < 1e-6, "the latest lands updated");
    assert!(!p.retarget_timed_gain(0, 0.5), "applied: nothing due any more");
}

#[test]
fn a_ramp_step_glides_across_blocks_and_lands_on_its_sample() {
    // A quarter-beat step of a fade: from unity at 200 to 0 at 1800,
    // spanning two blocks. It sounds as a straight line, not a jump.
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut rt = Rt::new(p.clone());
    let ones = vec![1.0f32; 1024];
    let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
    let c = clock(1024);
    rt.process(&c, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let next = c.position + 1024;
    assert!(p.post_glide(next + 1800, 1600, 0, TIMED_GAIN, 0.0));
    let at = |n: u64| Clock { position: n, ..c };
    rt.process(&at(next), &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let first = l.clone();
    rt.process(&at(next + 1024), &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let line = |i: usize| 1.0 - (i as f32 - 200.0) / 1600.0;
    assert!(first[..200].iter().all(|v| (*v - 1.0).abs() < 1e-6), "untouched before it starts");
    for i in [400usize, 600, 1000] {
        assert!((first[i] - line(i)).abs() < 0.02, "on the line at {i}: {}", first[i]);
    }
    for i in [1100usize, 1400, 1700] {
        assert!((l[i - 1024] - line(i)).abs() < 0.02, "on the line at {i}: {}", l[i - 1024]);
    }
    assert!(l[1800 - 1024..].iter().all(|v| v.abs() < 1e-6), "landed on its sample");
    // A change by hand during a glide ends it: the hand wins.
    assert!(p.post_glide(next + 5000, 2000, 0, TIMED_GAIN, 1.0));
    rt.process(&at(next + 3072), &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    p.set_gain(0, 0.25);
    rt.process(&at(next + 4096), &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    assert!((l[1023] - 0.25).abs() < 1e-6, "the hand's value, not the glide's");
}

#[test]
fn a_timed_change_lands_on_its_sample() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    let ones = vec![1.0f32; 1024];
    let (mut l, mut r) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);
    // A block to settle at unity; then a cut queued for sample 300 of
    // the next block, and a kill of the lows for sample 700.
    let c = clock(1024);
    rt.process(&c, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let next = c.position + 1024;
    assert!(p.post_at(next + 300, 0, TIMED_GAIN, 0.0));
    let c2 = Clock {
        position: next,
        ..c
    };
    rt.process(&c2, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    assert!(
        l[..300].iter().all(|v| (*v - 1.0).abs() < 1e-6),
        "untouched before its sample"
    );
    assert!(
        l[299] == 1.0 && l[301] < 1.0,
        "the change starts on its sample (a ramp begins at the old gain)"
    );
    assert!(
        l[300 + TIMED_RAMP..].iter().all(|v| v.abs() < 1e-6),
        "done {TIMED_RAMP} samples later"
    );
    assert!(load(&p.gain[0]) == 0.0, "and it stays");
    // Queued changes for later blocks wait; cleared ones never come.
    assert!(p.post_at(next + 5000, 0, TIMED_GAIN, 1.0));
    p.clear_timed();
    let c3 = Clock {
        position: next + 4096,
        ..c
    };
    rt.process(&c3, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    let c4 = Clock {
        position: next + 5120,
        ..c
    };
    rt.process(&c4, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    assert!(
        l.iter().all(|v| v.abs() < 1e-6),
        "a cleared change never lands"
    );
}

#[test]
fn headphone_cue_is_pre_fader() {
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    p.set_gain(1, 0.0); // deck A's fader down: the audience hears nothing
    p.set_pfl(1, true);
    let x = vec![0.25f32; 256];
    let (mut l, mut r, mut cl, mut cr) = (
        vec![0.0f32; 256],
        vec![0.0f32; 256],
        vec![0.0f32; 256],
        vec![0.0f32; 256],
    );
    for _ in 0..3 {
        rt.process(
            &clock(256),
            &[None, None, Some(&x), Some(&x)],
            [&mut l, &mut r],
            Some([&mut cl, &mut cr]),
        );
    }
    assert!(l.iter().all(|v| v.abs() < 1e-6), "program silent");
    assert!(
        cl.iter().chain(&cr).all(|v| (v - 0.25).abs() < 1e-6),
        "headphones hear deck A, both sides"
    );
    p.set_cue_mix(1.0); // master only
    for _ in 0..2 {
        rt.process(
            &clock(256),
            &[None, None, Some(&x), Some(&x)],
            [&mut l, &mut r],
            Some([&mut cl, &mut cr]),
        );
    }
    assert!(
        cl.iter().all(|v| v.abs() < 1e-6),
        "headphones follow the (silent) program"
    );
}

#[test]
fn headphone_cue_is_cleared_every_block_on_both_sides() {
    // Real block sizes: a cued deck for many cycles, then un-cued.
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let mut rt = Rt::new(p.clone());
    p.set_gain(1, 0.0);
    p.set_pfl(1, true);
    let n = 1024;
    let x = vec![0.25f32; n];
    let (mut l, mut r, mut cl, mut cr) = (
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
        vec![0.0f32; n],
    );
    let run = |rt: &mut Rt, l: &mut [f32], r: &mut [f32], cl: &mut [f32], cr: &mut [f32]| {
        rt.process(
            &clock(n as u32),
            &[None, None, Some(&x), Some(&x)],
            [l, r],
            Some([cl, cr]),
        );
    };
    for _ in 0..50 {
        run(&mut rt, &mut l, &mut r, &mut cl, &mut cr);
    }
    for (side, c) in [("left", &cl), ("right", &cr)] {
        assert!(
            c.iter().all(|v| (v - 0.25).abs() < 1e-6),
            "{side}: the deck at unity, not summed up ({})",
            c[0]
        );
    }
    p.set_pfl(1, false);
    for _ in 0..3 {
        run(&mut rt, &mut l, &mut r, &mut cl, &mut cr);
    }
    for (side, c) in [("left", &cl), ("right", &cr)] {
        assert!(
            c.iter().all(|v| v.abs() < 1e-6),
            "{side}: silent once un-cued ({})",
            c[0]
        );
    }
}

#[test]
fn beat_fx_echo_is_post_fader_and_spills_over() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    // No grid on the bus: 120 BPM, so ¼ beat = 6 000 frames at 48 kHz.
    p.set_fx(
        0,
        fx::Settings {
            kind: fx::Kind::Echo,
            on: true,
            beats: 0.25,
            depth: 0.5,
            ..fx::Settings::default()
        },
    );
    let clk = Clock {
        position: 0,
        nsec: 0,
        rate: 48_000,
        frames: 512,
        delay: 0,
        ..Default::default()
    };
    let mut hit = vec![0.0f32; 512];
    hit[0] = 1.0;
    let silence = vec![0.0f32; 512];
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut out = vec![];
    // FX buffers are prepared off the callback. Wait until this chain is
    // ready before sending the hit whose tail the test measures.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while rt.fx[0].is_none() && std::time::Instant::now() < deadline {
        rt.process(
            &clk,
            &[Some(&silence), Some(&silence)],
            [&mut l, &mut r],
            None,
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(rt.fx[0].is_some(), "FX worker prepared the requested chain");
    // One block to switch the effect in (it fades in: no click).
    rt.process(
        &clk,
        &[Some(&silence), Some(&silence)],
        [&mut l, &mut r],
        None,
    );
    for block in 0..30 {
        if block == 1 {
            p.set_mute(0, true); // fader shut right after the hit
        }
        let x = if block == 0 { &hit } else { &silence };
        rt.process(&clk, &[Some(x), Some(x)], [&mut l, &mut r], None);
        out.extend_from_slice(&l);
    }
    assert_eq!(out[0], 1.0, "dry hit");
    let near = |at: usize| {
        out[at - 64..at + 64]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    assert!(
        near(6_000) > 0.1,
        "echo after ¼ beat, fader already shut: {}",
        near(6_000)
    );
    assert!(near(12_000) > 0.02, "and again");
    assert!(near(3_000) < 1e-4, "nothing in between");
}

#[test]
fn balance_law() {
    let p = Params::new(&["x"]);
    assert_eq!(p.side_gains(0), [1.0, 1.0]);
    p.set_pan(0, 1.0);
    let [l, r] = p.side_gains(0);
    assert!(l.abs() < 1e-6 && r == 1.0);
}

#[test]
fn meters_are_stamped_per_block() {
    let p = Arc::new(Params::new(&["bus", "deck-a"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let mut rt = Rt::new(p.clone());
    let half = vec![0.5f32; 512];
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    rt.process(
        &clock(512),
        &[Some(&half), None, None, Some(&half)],
        [&mut l, &mut r],
        None,
    );
    let mut got = vec![];
    unsafe { p.meters.drain(|b| got.push(b)) };
    assert_eq!(got.len(), 1);
    let b = got[0];
    assert_eq!(
        (b.stamp.sample_pos, b.stamp.rate, b.stamp.frames),
        (4800, 48_000, 512)
    );
    assert_eq!(b.ch_peak[0], [0.5, 0.0]);
    assert_eq!(b.ch_peak[1], [0.0, 0.5]);
    assert_eq!(b.out_peak, [0.5, 0.5]);
    let mut n = 0;
    unsafe { p.program.drain(|_| n += 1) };
    assert_eq!(n, 1024, "interleaved program");
}

#[test]
fn tune_compiled_filter_runs_in_program_path() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    let coeff = patch_tune::PeakingFilter {
        frequency_hz: 1000.0,
        q: 1.0,
        gain_db: -6.0,
    }
    .compile(48_000)
    .unwrap();
    p.set_tune_profile(&[coeff], 0.0, -1.0, false).unwrap();
    let mut rt = Rt::new(p);
    let input = vec![0.5; 1024];
    let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
    rt.run(
        &clock(1024),
        &[Some(&input), Some(&input)],
        [&mut l, &mut r],
        None,
    );
    assert!(l.iter().all(|v| v.is_finite()));
    assert!(
        l.iter().any(|v| (v - 0.5).abs() > 1e-3),
        "compiled correction reaches the program output"
    );
}

#[test]
fn tune_headroom_ramps_and_profile_guard_caps_peaks() {
    let p = Arc::new(Params::new(&["bus"]));
    p.master_limiter.store(false, Ordering::Relaxed); // the raw mix
    p.set_tune_profile(&[], 6.0, -6.0, true).unwrap();
    let mut rt = Rt::new(p);
    let input = vec![1.0; 256];
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    rt.run(
        &clock(256),
        &[Some(&input), Some(&input)],
        [&mut l, &mut r],
        None,
    );
    assert!(l[0] > l[255], "headroom changes by a ramp, not a step");
    assert!(
        l.iter().all(|v| v.abs() <= 0.502),
        "protection ceiling is enforced"
    );
}

#[test]
fn input_trim_ramps_then_holds_and_trims_a_mic_before_its_voice() {
    // The Mixer's GAIN knob: +6.02 dB doubles a channel. Unity costs
    // nothing; a change ramps over one block (no click), then holds.
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let x = vec![0.1f32; 256];
    let silent = vec![0.0f32; 256];
    let mut rt = Rt::new(p.clone());
    let run = |rt: &mut Rt, bus: &[f32], mic: &[f32]| {
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let ins = [Some(bus), Some(bus), Some(mic), Some(mic)];
        rt.process(&clock(256), &ins, [&mut l, &mut r], None);
        (l, rt.input_audio(256)[0].to_vec())
    };
    let (unity, _) = run(&mut rt, &x, &silent);
    assert!(unity.iter().all(|v| (v - 0.1).abs() < 1e-6), "unity: {}", unity[255]);
    p.set_trim(0, db_to_linear(6.0206));
    let (ramp, _) = run(&mut rt, &x, &silent);
    assert!(ramp[0] < 0.11 && (ramp[255] - 0.2).abs() < 1e-4, "ramp {} → {}", ramp[0], ramp[255]);
    assert!(ramp.windows(2).all(|w| w[1] >= w[0]), "a straight ramp, no jump");
    let (held, _) = run(&mut rt, &x, &silent);
    assert!(held.iter().all(|v| (v - 0.2).abs() < 1e-4), "held: {}", held[0]);
    // A microphone: trimmed before voice processing, into the capture mix.
    p.set_trim(1, db_to_linear(6.0206));
    run(&mut rt, &silent, &x);
    let (_, mic) = run(&mut rt, &silent, &x);
    assert!(mic.iter().all(|v| (v - 0.2).abs() < 1e-3), "mic: {}", mic[0]);
}

#[test]
fn sends_feed_the_buses_pre_and_post_and_the_returns_play_in_the_program() {
    let p = Arc::new(Params::new(&["bus", "mic-1"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let x = vec![0.1f32; 256];
    let silent = vec![0.0f32; 256];
    let mut rt = Rt::new(p.clone());
    let run = |rt: &mut Rt| {
        let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let ins = [Some(&x[..]), Some(&x[..]), Some(&silent[..]), Some(&silent[..])];
        rt.process(&clock(256), &ins, [&mut l, &mut r], None);
        l
    };
    run(&mut rt);
    assert!(rt.aux_audio(0, 256).is_none(), "nothing sent: no AUX, no pass");
    // AUX 1, pre-fader at −6 dB: the input at half, whatever the fader.
    p.set_gain(0, 0.25);
    p.set_send(0, 0, 0.5);
    run(&mut rt);
    run(&mut rt);
    let aux = rt.aux_audio(0, 256).unwrap()[0].to_vec();
    assert!(aux.iter().all(|v| (v - 0.05).abs() < 1e-5), "pre: {}", aux[0]);
    // Post-fader: the fader (0.25) too.
    p.set_send_pre(0, 0, false);
    run(&mut rt);
    run(&mut rt);
    let aux = rt.aux_audio(0, 256).unwrap()[0].to_vec();
    assert!(aux.iter().all(|v| (v - 0.0125).abs() < 1e-5), "post: {}", aux[0]);
    // AUX 1's master at half.
    p.set_bus(0, Some(0.5), None);
    run(&mut rt);
    run(&mut rt);
    assert!((rt.aux_audio(0, 256).unwrap()[0][100] - 0.00625).abs() < 1e-5);
    // Muted: the channel sends nothing (after its ramp).
    p.set_mute(0, true);
    run(&mut rt);
    run(&mut rt);
    assert!(rt.aux_audio(0, 256).is_none(), "a muted channel sends nothing");
    p.set_mute(0, false);
    p.set_send(0, 0, 0.0);
    // FX 2 (delay, 2 ms) post-fader: the dry channel, then its repeat
    // arrives in the program on top of it.
    p.set_gain(0, 1.0);
    p.delay_ms.store(2.0f32.to_bits(), Ordering::Relaxed);
    p.delay_feedback.store(0.0f32.to_bits(), Ordering::Relaxed);
    p.set_send(0, 3, 1.0);
    run(&mut rt);
    let out = run(&mut rt);
    assert!((out[200] - 0.2).abs() < 1e-3, "dry 0.1 + the repeat 0.1: {}", out[200]);
    // Its return muted: the dry channel alone.
    p.set_bus(3, None, Some(true));
    run(&mut rt);
    let out = run(&mut rt);
    assert!((out[200] - 0.1).abs() < 1e-4, "return muted: {}", out[200]);
    p.set_bus(3, None, Some(false));
    p.set_send(0, 3, 0.0);
    p.set_send(0, 2, 1.0);
    let mut wet_peak = 0.0f32;
    for _ in 0..12 {
        let out = run(&mut rt);
        wet_peak = wet_peak.max(out.iter().map(|x| x.abs()).fold(0.0, f32::max));
    }
    assert!(wet_peak > 0.001, "FX 1 reverb return reaches the programme: {wet_peak}");
}

#[test]
fn a_reused_playback_lane_has_no_previous_controls_or_filter_history() {
    let p = Arc::new(Params::new(&["stream-1", "stream-2"]));
    let mut rt = Rt::new(p.clone());
    let mut eq = patch_core::eq::EqState::default();
    eq.bands[0].gain_db = 12.0;
    p.channel_eq[0].set(&eq);
    p.set_gain(0, 0.2);
    p.set_trim(0, 2.0);
    p.set_pan(0, -0.5);
    p.set_send(0, 0, 0.5);
    let one = vec![0.1; 256];
    let two = vec![0.2; 256];
    let inputs = [Some(one.as_slice()), Some(one.as_slice()), Some(two.as_slice()), Some(two.as_slice())];
    let mut out_l = vec![0.0; 256];
    let mut out_r = vec![0.0; 256];
    let mut clock = Clock { rate: 48000, frames: 256, position: 0, nsec: 0, delay: 0, ..Default::default() };
    for _ in 0..8 {
        rt.process(&clock, &inputs, [&mut out_l, &mut out_r], None);
        clock.position += 256;
    }
    p.set_mute(0, true);
    p.set_solo(0, true);
    p.set_pfl(0, true);
    p.reset_stream_channel(0);
    assert!(!p.is_muted(0) && !p.solo_of(0) && !p.pfl_of(0));
    assert_eq!(p.send_of(0, 0), 0.0);
    assert_eq!(p.trim_of(0), 1.0);
    for _ in 0..8 {
        rt.process(&clock, &inputs, [&mut out_l, &mut out_r], None);
        clock.position += 256;
    }
    assert!((out_l[255] - 0.3).abs() < 1e-4, "{}", out_l[255]);
    assert!((out_r[255] - 0.3).abs() < 1e-4, "{}", out_r[255]);
}

#[test]
#[ignore = "manual native stream callback capacity measurement"]
fn native_stream_callback_cost() {
    for count in [28, 60] {
        let names: Vec<_> = crate::daemon::CHANNELS[..count].to_vec();
        let p = Arc::new(Params::new(&names));
        let mut rt = Rt::new(p.clone());
        let tone = vec![0.001; 256];
        let mut inputs = vec![None; count * 2];
        for c in 28..count { inputs[c*2] = Some(tone.as_slice()); inputs[c*2+1] = Some(tone.as_slice()); }
        let mut l = vec![0.0; 256];
        let mut r = vec![0.0; 256];
        let mut times = Vec::new();
        let mut clock = Clock { rate: 48000, frames: 256, position: 0, nsec: 0, delay: 0, ..Default::default() };
        for i in 0..2200 {
            let at = std::time::Instant::now();
            rt.process(&clock, &inputs, [&mut l, &mut r], None);
            if i >= 200 { times.push(at.elapsed().as_nanos()); }
            clock.position += 256;
            // SAFETY: callback and ring consumers run sequentially here.
            unsafe { p.meters.drain(|_| {}); p.program.drain(|_| {}); }
        }
        times.sort_unstable();
        println!("{count} configured channels / {} active streams: average {} ns, p99 {} ns, max {} ns; 5,333,333 ns budget",
            count.saturating_sub(28), times.iter().sum::<u128>() / times.len() as u128, times[1979], times[1999]);
    }
}

#[test]
fn stream_signal_is_sticky_before_mute_and_fader_but_expires_on_reuse() {
    let p = Arc::new(Params::new(&["stream-1"]));
    let mut rt = Rt::new(p.clone());
    let clock = Clock { rate: 48000, frames: 64, ..Default::default() };
    let silence = [0.0; 64];
    let signal = [0.01; 64];
    let mut l = [0.0; 64];
    let mut r = [0.0; 64];
    rt.process(&clock, &[Some(&silence), Some(&silence)], [&mut l, &mut r], None);
    assert!(!p.stream_has_signal(0));
    p.set_gain(0, 0.0);
    p.set_mute(0, true);
    // A click as a browser's helper opens is not a caller.
    rt.process(&clock, &[Some(&signal), Some(&signal)], [&mut l, &mut r], None);
    assert!(!p.stream_has_signal(0), "one block is not a second of signal");
    for _ in 0..48000 / 64 {
        rt.process(&clock, &[Some(&signal), Some(&signal)], [&mut l, &mut r], None);
    }
    assert!(p.stream_has_signal(0), "muted callers must still be discoverable");
    assert!(p.stream_heard_within(0, 1.0));
    for _ in 0..2 * 48000 / 64 {
        rt.process(&clock, &[Some(&silence), Some(&silence)], [&mut l, &mut r], None);
    }
    assert!(p.stream_has_signal(0), "quiet callers retain their channel");
    assert!(!p.stream_heard_within(0, 1.0), "but they are not heard now");
    assert!(p.stream_heard_within(0, 3.0));
    let retiring_epoch = p.channel_epoch[0].load(Ordering::Acquire);
    p.reset_stream_channel(0);
    p.stream_signal[0].store((retiring_epoch as u64) << 32, Ordering::Relaxed);
    assert!(!p.stream_has_signal(0), "late old observations cannot reveal a replacement");
    rt.process(&clock, &[Some(&silence), Some(&silence)], [&mut l, &mut r], None);
    assert!(!p.stream_has_signal(0));
}

/// The `patch` sink's volume reaches the output at once while Patch is the
/// system's audio (no pactl round trip), and never while it isn't.
#[test]
fn the_sink_volume_applies_at_once_only_as_system_audio() {
    let p = Params::new(&["bus", "deck-a"]);
    p.bus_volume_changed(0.25, 0.5, false);
    assert_eq!(p.system(), ([1.0, 1.0], false), "not the system's audio: unity");
    assert_eq!(p.bus_watch.lock().unwrap().volume, Some(([0.25, 0.5], false)));
    p.bus_watch.lock().unwrap().live = true;
    p.bus_volume_changed(0.064, 0.064, false);
    assert_eq!(p.system(), ([0.064, 0.064], false));
    p.bus_volume_changed(0.064, 0.064, true);
    assert_eq!(p.system_out(), [0.0, 0.0], "mute");
    // System audio off: a late change doesn't take the volume back.
    p.bus_watch.lock().unwrap().live = false;
    p.set_system(1.0, 1.0, false);
    p.bus_volume_changed(0.1, 0.1, false);
    assert_eq!(p.system(), ([1.0, 1.0], false));
}

#[test]
fn a_late_wakeup_counts_only_once_the_graph_has_settled() {
    // The graph pauses as the engine joins it (or an upgrade hands over):
    // not a skip. Later, a wake-up three cycles late is.
    let p = Arc::new(Params::new(&["bus"]));
    let mut rt = Rt::new(p.clone());
    let ones = vec![1.0f32; 256];
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let cycle = 256 * 1_000_000_000 / 48_000;
    let mut run = |nsec: u64, frames: u32| {
        let c = Clock { nsec, ..clock(frames) };
        rt.process(&c, &[Some(&ones), Some(&ones)], [&mut l, &mut r], None);
    };
    run(1_000, 256);
    run(1_000 + 4 * cycle, 256);
    assert_eq!(p.gaps.load(Ordering::Relaxed), 0, "settling");
    let t = 40_000_000_000;
    run(t, 256);
    run(t + cycle, 128);
    run(t + 4 * cycle, 256);
    assert_eq!(p.gaps.load(Ordering::Relaxed), 0, "the buffer size changed");
    run(t + 5 * cycle, 256);
    run(t + 8 * cycle, 256);
    assert_eq!(p.gaps.load(Ordering::Relaxed), 1);
}

#[test]
fn only_a_skip_while_patch_sounds_counts_for_the_bar() {
    // The bar's health dot: a late wake-up in silence can't be heard.
    let p = Arc::new(Params::new(&["bus"]));
    let mut rt = Rt::new(p.clone());
    let (ones, zeros) = (vec![1.0f32; 256], vec![0.0f32; 256]);
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let cycle = 256 * 1_000_000_000 / 48_000;
    let mut run = |nsec: u64, input: &[f32]| {
        let c = Clock { nsec, ..clock(256) };
        rt.process(&c, &[Some(input), Some(input)], [&mut l, &mut r], None);
    };
    run(1_000, &zeros);
    let t = 40_000_000_000;
    run(t, &zeros);
    run(t + 4 * cycle, &zeros);
    assert_eq!(p.gaps.load(Ordering::Relaxed), 1);
    assert_eq!(p.heard_dry.load(Ordering::Relaxed), 0, "silence");
    run(t + 5 * cycle, &ones);
    run(t + 8 * cycle, &ones);
    assert_eq!(p.gaps.load(Ordering::Relaxed), 2);
    assert_eq!(p.heard_dry.load(Ordering::Relaxed), 1, "sounding");
    // Just after the music stops, a skip may still cut its tail.
    run(t + 9 * cycle, &zeros);
    run(t + 12 * cycle, &zeros);
    assert_eq!(p.heard_dry.load(Ordering::Relaxed), 2, "within half a second");
}

#[test]
fn four_decks_sum_independently_through_the_booth_group() {
    let p = Arc::new(Params::new(&["deck-a", "deck-b", "deck-c", "deck-d"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let signals: [Vec<f32>; 4] = std::array::from_fn(|i| vec![(i + 1) as f32 * 0.01; 256]);
    let inputs: Vec<Option<&[f32]>> = signals.iter().flat_map(|v| [Some(v.as_slice()); 2]).collect();
    let render = |mixer: crate::session::Mixer, group: f32, mute: bool| {
        for (i, gain) in mixer.gains4_db().into_iter().enumerate() {
            p.set_gain(i, db_to_linear(gain));
        }
        p.set_dj_group(group, mute);
        let mut rt = Rt::new(p.clone());
        let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
        for _ in 0..8 { rt.process(&clock(256), &inputs, [&mut left, &mut right], None); }
        left[255]
    };
    let mixer = crate::session::Mixer {
        extra_faders: [crate::session::UNITY; 2],
        auto_gain: false,
        ..Default::default()
    };
    // The existing A/B overlap level control stays unchanged. C/D are
    // independent manual lanes, added after it and before the group fader.
    let primary = render(crate::session::Mixer { auto_gain: false, ..Default::default() }, 1.0, false);
    let four = render(mixer.clone(), 1.0, false);
    assert!((four - primary - 0.07).abs() < 1e-5, "A/B={primary}, four decks={four}");
    assert!((render(mixer.clone(), 0.5, false) - four * 0.5).abs() < 1e-5);
    assert!(render(mixer.clone(), 1.0, true).abs() < 1e-5);
    for i in 0..4 {
        let mut only = mixer.clone();
        only.faders = [0.0; 2];
        only.extra_faders = [0.0; 2];
        if i < 2 { only.faders[i] = crate::session::UNITY; }
        else { only.extra_faders[i - 2] = crate::session::UNITY; }
        assert!((render(only, 1.0, false) - (i + 1) as f32 * 0.01).abs() < 1e-5);
    }
}

#[test]
fn four_deck_input_trims_are_independent_pre_fader_and_ramped() {
    let p = Arc::new(Params::new(&["deck-a", "deck-b", "deck-c", "deck-d"]));
    p.master_limiter.store(false, Ordering::Relaxed);
    let input = vec![0.025f32; 256];
    let ins = [Some(input.as_slice()); 8];
    let mut rt = Rt::new(p.clone());
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    rt.process(&clock(256), &ins, [&mut l, &mut r], None);
    for i in 0..4 { p.set_gain(i, 0.0); }
    let db = [-24.0, -6.0206, 6.0206, 24.0];
    for (i, db) in db.iter().enumerate() { p.set_trim(i, db_to_linear(*db)); }
    let mut blocks = Vec::new();
    for _ in 0..3 {
        rt.process(&clock(256), &ins, [&mut l, &mut r], None);
        unsafe { p.meters.drain(|b| blocks.push(b)) };
    }
    assert!(l.iter().all(|v| v.abs() < 1e-6), "closed faders silence the program");
    let last = blocks.last().unwrap();
    for (i, db) in db.iter().enumerate() {
        let expected = 0.025 * db_to_linear(*db);
        assert!((last.ch_peak[i][0] - expected).abs() < 1e-5, "deck {i}: input gain {db} dB");
        assert_eq!(last.ch_gain[i], [0.0; 2], "input peaks are independent of closed faders");
    }
    // Restoring unity never retains a prior gain or a neighboring deck's setting.
    for i in 0..4 { p.set_trim(i, 1.0); }
    for _ in 0..2 { rt.process(&clock(256), &ins, [&mut l, &mut r], None); }
    blocks.clear();
    unsafe { p.meters.drain(|b| blocks.push(b)) };
    assert!(blocks.last().unwrap().ch_peak[..4].iter().flatten().all(|v| (*v - 0.025).abs() < 1e-6));
}
