//! `patch-bench [out.md]`: how fast Patch's real-time work is, per tier.
//! `patch-bench --analysis-only`: measure the Level 1 analysis pipeline.
//!
//! * Engine cycle: the complete RT callback (mix with gain ramps, per-channel
//!   and program peaks, stamped meter block, program ring) at 48/96/192 kHz,
//!   32–1024 frames, 3–16 engine channels.
//! * Channel scaling: mix + peak per channel, 2–128 channels.
//! * Analysis kernels in times real time: VU ballistics, K-weighting + true
//!   peak (EBU R128), 24-bit TPDF conversion, spectrum.
//!
//! Every figure is reported as average, 99th percentile and worst case: a
//! real-time engine is only as good as its worst cycle. Run it on an idle
//! machine; it is not a real-time thread, so the worst case includes
//! scheduler noise and is an upper bound.

use patch_dsp::Tier;
use patch_engine::{Clock, Params, Rt};
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

fn sig(n: usize, seed: f32) -> Vec<f32> {
    (0..n)
        .map(|i| ((i as f32) * 0.013 + seed).sin() * 0.7 + ((i as f32) * 0.31 + seed).sin() * 0.1)
        .collect()
}

struct Stats {
    avg: f64,
    p99: f64,
    max: f64,
}

fn stats(mut ns: Vec<u64>) -> Stats {
    ns.sort_unstable();
    let avg = ns.iter().sum::<u64>() as f64 / ns.len() as f64;
    let p99 = ns[(ns.len() as f64 * 0.99) as usize - 1] as f64;
    Stats {
        avg,
        p99,
        max: *ns.last().unwrap() as f64,
    }
}

fn time(iters: usize, mut f: impl FnMut()) -> Stats {
    for _ in 0..iters / 10 + 1 {
        f(); // warm caches and branch predictors
    }
    let mut ns = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f();
        ns.push(t.elapsed().as_nanos() as u64);
    }
    stats(ns)
}

fn us(x: f64) -> String {
    format!("{:.2}", x / 1000.0)
}

fn engine_cycles(out: &mut String, tiers: &[Tier]) {
    writeln!(out, "## Engine cycle (callback and harness)\n").unwrap();
    writeln!(out, "Budget = frames / rate. Harness includes gain updates and ring draining; callback columns isolate Rt::process. Harness worst load is an upper bound.\n").unwrap();
    writeln!(out, "| tier | rate | frames | channels | budget µs | avg µs | p99 µs | worst µs | worst load | callback avg µs | callback p99 µs |").unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for &tier in tiers {
        patch_dsp::set(tier);
        for rate in [48_000u32, 96_000, 192_000] {
            for frames in [32usize, 64, 128, 256, 512, 1024] {
                for chans in [3usize, 8, 16] {
                    let names: Vec<String> = (0..chans).map(|c| format!("ch{c}")).collect();
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    let p = Arc::new(Params::new(&refs));
                    let mut rt = Rt::new(p.clone());
                    let inputs: Vec<Vec<f32>> =
                        (0..chans * 2).map(|i| sig(frames, i as f32)).collect();
                    let ins: Vec<Option<&[f32]>> =
                        inputs.iter().map(|v| Some(v.as_slice())).collect();
                    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
                    let mut clock = Clock {
                        position: 0,
                        nsec: 0,
                        rate,
                        frames: frames as u32,
                        delay: 0,
                        ..Default::default()
                    };
                    patch_dsp::registry::init();
                    let mut k = 0u32;
                    let mut callback_ns = Vec::with_capacity(4401);
                    let s = time(4000, || {
                        // Moving faders: every cycle ramps, as when mixing live.
                        k = k.wrapping_add(1);
                        p.set_gain(0, 0.5 + (k % 7) as f32 * 0.05);
                        clock.position += frames as u64;
                        let at = Instant::now();
                        rt.process(&clock, &ins, [&mut l, &mut r], None);
                        callback_ns.push(at.elapsed().as_nanos() as u64);
                        // SAFETY: one thread runs the callback and drains, in turn.
                        unsafe { p.meters.drain(|b| {
                            black_box(b);
                        }) };
                        unsafe { p.program.drain(|x| {
                            black_box(x);
                        }) };
                    });
                    let callback = stats(callback_ns.into_iter().skip(401).collect());
                    let budget = frames as f64 / rate as f64 * 1e9;
                    writeln!(
                        out,
                        "| {} | {} kHz | {frames} | {chans} | {} | {} | {} | {} | {:.3} % | {} | {} |",
                        tier_short(tier),
                        rate / 1000,
                        us(budget),
                        us(s.avg),
                        us(s.p99),
                        us(s.max),
                        s.max / budget * 100.0,
                        us(callback.avg),
                        us(callback.p99)
                    )
                    .unwrap();
                }
            }
        }
    }
}

fn channel_scaling(out: &mut String, tiers: &[Tier]) {
    writeln!(
        out,
        "\n## Channel scaling (mix with gain ramp + sample peak, per cycle of 256 frames)\n"
    )
    .unwrap();
    writeln!(
        out,
        "| tier | channels | avg µs | p99 µs | worst µs | ns per channel |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|").unwrap();
    let frames = 256;
    for &tier in tiers {
        patch_dsp::set(tier);
        for chans in [2usize, 8, 32, 64, 128] {
            let inputs: Vec<Vec<f32>> = (0..chans).map(|i| sig(frames, i as f32)).collect();
            let mut outb = vec![0.0f32; frames];
            let s = time(4000, || {
                outb.fill(0.0);
                let mut pk = 0.0f32;
                for x in &inputs {
                    let st = patch_meter::stats(x);
                    pk = pk.max(st.peak[0]).max(st.peak[1]);
                    patch_engine::mix(&mut outb, x, 0.8, -0.0001);
                }
                black_box(pk);
            });
            writeln!(
                out,
                "| {} | {chans} | {} | {} | {} | {:.0} |",
                tier_short(tier),
                us(s.avg),
                us(s.p99),
                us(s.max),
                s.avg / chans as f64
            )
            .unwrap();
        }
    }
}

fn kernels(out: &mut String, tiers: &[Tier]) {
    writeln!(
        out,
        "\n## Analysis kernels (one second of 48 kHz stereo, in 1024-frame blocks)\n"
    )
    .unwrap();
    writeln!(
        out,
        "× real time = how many seconds of audio one second of CPU processes.\n"
    )
    .unwrap();
    writeln!(
        out,
        "| tier | kernel | avg ms per s of audio | worst ms | × real time |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|").unwrap();
    let audio = sig(48_000 * 2, 0.5);
    for &tier in tiers {
        patch_dsp::set(tier);
        let mut rows: Vec<(&str, Stats)> = vec![];
        let mut vu = patch_meter::Vu::new(48_000);
        rows.push((
            "VU ballistics (IEC 60268-17)",
            time(40, || {
                for b in audio.chunks(2048) {
                    black_box(vu.process(b));
                }
            }),
        ));
        let mut loud = patch_meter::Loudness::new(48_000);
        rows.push((
            "K-weighting + 4× true peak (BS.1770-4)",
            time(40, || {
                for b in audio.chunks(2048) {
                    loud.push(b);
                }
                black_box(loud.momentary());
            }),
        ));
        let mut dither = patch_record::Dither::tpdf(0x5eed);
        let mut conv = patch_record::ConvStats::default();
        let mut bytes: Vec<u8> = Vec::with_capacity(2048 * 3);
        rows.push((
            "24-bit TPDF conversion",
            time(40, || {
                for b in audio.chunks(2048) {
                    bytes.clear();
                    patch_record::convert_s24(b, &mut dither, &mut conv, &mut bytes);
                    black_box(&bytes);
                }
            }),
        ));
        rows.push((
            "peak + energy (stats)",
            time(40, || {
                for b in audio.chunks(2048) {
                    black_box(patch_meter::stats(b));
                }
            }),
        ));
        for (name, s) in rows {
            writeln!(
                out,
                "| {} | {name} | {:.3} | {:.3} | {:.0}× |",
                tier_short(tier),
                s.avg / 1e6,
                s.max / 1e6,
                1e9 / s.avg
            )
            .unwrap();
        }
    }
}

/// Full Level 1 analysis on deterministic synthetic DJ material. This isolates
/// analyzer CPU and result footprint from file decoding and filesystem cache.
/// Tempo detection against tagged BPMs (weak labels: tags can be wrong or
/// use a half-time convention), over a list of "path<TAB>bpm" lines,
/// analysed in parallel. Reports exact, octave, 3:2 and other readings.
/// Key detection variants against the library's tagged keys: chroma kind ×
/// key profile, exact and DJ-compatible (same Camelot number, or ±1 with the
/// same letter). Learned profiles are two-fold cross-validated.
fn key_eval() {
    use patch_analysis::key::{self, Key};
    let db = patch_library::open_default().expect("library");
    let tagged: Vec<(String, Key)> = db
        .tracks()
        .expect("tracks")
        .into_iter()
        .filter_map(|t| Some((t.path.clone(), patch_analysis::read_tags(std::path::Path::new(&t.path)).1?)))
        .collect();
    const KINDS: [&str; 4] = ["energy 130–4186 Hz", "energy 196–4186 Hz", "energy 261–5274 Hz", "energy 130–8372 Hz"];
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::<(Key, [[f64; 12]; 4])>::new());
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((path, tag)) = tagged.get(i) else { break };
                let Ok(pcm) = patch_analysis::decode(std::path::Path::new(path)) else {
                    continue;
                };
                let rate = patch_analysis::RATE / 2;
                let mono: Vec<f32> = pcm.chunks_exact(4).map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25).collect();
                let c = [
                    key::chroma_energy(&mono, rate, 130.8, 4186.0, 0.0, false),
                    key::chroma_energy(&mono, rate, 196.0, 4186.0, 0.0, false),
                    key::chroma_energy(&mono, rate, 261.6, 5274.0, 0.0, false),
                    key::chroma_energy(&mono, rate, 130.8, 8372.0, 0.0, false),
                ];
                found.lock().unwrap().push((*tag, c));
            });
        }
    });
    let mut rows = found.into_inner().unwrap();
    rows.sort_by_key(|r| (r.0.tonic, r.0.minor));
    let norm = |c: &[f64; 12]| {
        let s: f64 = c.iter().sum();
        let mut o = *c;
        if s > 0.0 {
            o.iter_mut().for_each(|x| *x /= s);
        }
        o
    };
    let near = |a: Key, b: Key| {
        let (x, y) = (a.camelot_number() as i32, b.camelot_number() as i32);
        x == y || (a.minor == b.minor && ((x - y).rem_euclid(12) == 1 || (y - x).rem_euclid(12) == 1))
    };
    // A profile learned from rows (chroma rotated to the tagged tonic).
    let learn = |rows: &[&(Key, [[f64; 12]; 4])], k: usize| -> ([f64; 12], [f64; 12]) {
        let (mut maj, mut min, mut nm, mut nn) = ([0.0; 12], [0.0; 12], 0.0, 0.0);
        for (tag, c) in rows {
            let c = norm(&c[k]);
            let p = if tag.minor { &mut min } else { &mut maj };
            for i in 0..12 {
                p[i] += c[(i + tag.tonic as usize) % 12];
            }
            if tag.minor { nn += 1.0 } else { nm += 1.0 }
        }
        maj.iter_mut().for_each(|x| *x /= f64::max(nm, 1.0));
        min.iter_mut().for_each(|x| *x /= f64::max(nn, 1.0));
        (maj, min)
    };
    println!("{} tracks with a key tag (major {}, minor {})", rows.len(), rows.iter().filter(|r| !r.0.minor).count(), rows.iter().filter(|r| r.0.minor).count());
    let fixed = [
        ("Krumhansl", key::profiles::KK),
        ("Temperley", key::profiles::TEMPERLEY),
        ("Sha'ath", key::profiles::SHAATH),
    ];
    for (k, kind) in KINDS.iter().enumerate() {
        println!("\n{kind}");
        for cosine in [false, true] {
            let mut line = format!("  {:7}", if cosine { "cosine" } else { "pearson" });
            for (name, prof) in fixed {
                let (mut e, mut c) = (0, 0);
                for (tag, ch) in &rows {
                    let best = key::best_key(&norm(&ch[k]), &prof, cosine)[0].1;
                    e += usize::from(best == *tag);
                    c += usize::from(near(best, *tag));
                }
                line += &format!("  {name} {:4.1}/{:4.1}", e as f64 / rows.len() as f64 * 100.0, c as f64 / rows.len() as f64 * 100.0);
            }
            // Learned, two-fold.
            let (mut e, mut c) = (0, 0);
            for fold in 0..2 {
                let train: Vec<_> = rows.iter().enumerate().filter(|(i, _)| i % 2 != fold).map(|(_, r)| r).collect();
                let prof = learn(&train, k);
                for (i, (tag, ch)) in rows.iter().enumerate().filter(|(i, _)| i % 2 == fold) {
                    let _ = i;
                    let best = key::best_key(&norm(&ch[k]), &prof, cosine)[0].1;
                    e += usize::from(best == *tag);
                    c += usize::from(near(best, *tag));
                }
            }
            line += &format!("  learned {:4.1}/{:4.1}", e as f64 / rows.len() as f64 * 100.0, c as f64 / rows.len() as f64 * 100.0);
            println!("{line}");
        }
    }
    // Error kinds for energy chroma × Sha'ath (Pearson), kind 1 → see KINDS.
    let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
    for (tag, ch) in &rows {
        let best = key::best_key(&norm(&ch[3]), &key::profiles::SHAATH, false)[0].1;
        let (x, y) = (tag.camelot_number() as i32, best.camelot_number() as i32);
        let kind = if best == *tag {
            "exact"
        } else if x == y {
            "relative (same number)"
        } else if tag.minor == best.minor && ((x - y).rem_euclid(12) == 1 || (y - x).rem_euclid(12) == 1) {
            "fifth (±1)"
        } else if best.tonic == tag.tonic {
            "parallel (same tonic)"
        } else if tag.minor == best.minor && ((x - y).rem_euclid(12) == 2 || (y - x).rem_euclid(12) == 2) {
            "two fifths (±2)"
        } else if (best.tonic as i32 - tag.tonic as i32).rem_euclid(12) == 1 || (tag.tonic as i32 - best.tonic as i32).rem_euclid(12) == 1 {
            "semitone"
        } else {
            "other"
        };
        *kinds.entry(kind).or_default() += 1;
    }
    println!("error kinds (65–1047 Hz energy, Sha'ath): {kinds:?}");
    // Mode from the scale degrees that tell major from minor, relative to
    // the profile's tonic: logistic on (m3 − M3, m6 − M6, m7 − M7),
    // two-fold cross-validated.
    for k in [1usize, 3] {
        let feats = |c: &[f64; 12], t: u8| -> [f64; 4] {
            let c = norm(c);
            let at = |i: usize| c[(i + t as usize) % 12] * 12.0;
            [1.0, at(3) - at(4), at(8) - at(9), at(10) - at(11)]
        };
        let tonic = |c: &[f64; 12]| key::best_key(&norm(c), &key::profiles::SHAATH, false)[0].1.tonic;
        let (mut exact, mut tonic_ok) = (0, 0);
        for fold in 0..2 {
            let train: Vec<_> = rows.iter().enumerate().filter(|(i, _)| i % 2 != fold).map(|(_, r)| r).collect();
            let mut w = [0.0f64; 4];
            for _ in 0..3000 {
                let mut g = [0.0f64; 4];
                for (tag, ch) in &train {
                    let x = feats(&ch[k], tag.tonic);
                    let z: f64 = (0..4).map(|j| w[j] * x[j]).sum();
                    let e = 1.0 / (1.0 + (-z).exp()) - f64::from(u8::from(tag.minor));
                    for j in 0..4 {
                        g[j] += e * x[j];
                    }
                }
                for j in 0..4 {
                    w[j] -= 0.5 * g[j] / train.len() as f64;
                }
            }
            for (_, (tag, ch)) in rows.iter().enumerate().filter(|(i, _)| i % 2 == fold) {
                let t = tonic(&ch[k]);
                let x = feats(&ch[k], t);
                let minor = (0..4).map(|j| w[j] * x[j]).sum::<f64>() > 0.0;
                tonic_ok += usize::from(t == tag.tonic || (t + if tag.minor { 3 } else { 9 }) % 12 == tag.tonic);
                exact += usize::from(t == tag.tonic && minor == tag.minor);
            }
        }
        // Fitted on everything: the weights to ship.
        let all: Vec<_> = rows.iter().collect();
        let mut w = [0.0f64; 4];
        for _ in 0..3000 {
            let mut g = [0.0f64; 4];
            for (tag, ch) in &all {
                let x = feats(&ch[k], tag.tonic);
                let z: f64 = (0..4).map(|j| w[j] * x[j]).sum();
                let e = 1.0 / (1.0 + (-z).exp()) - f64::from(u8::from(tag.minor));
                for j in 0..4 {
                    g[j] += e * x[j];
                }
            }
            for j in 0..4 {
                w[j] -= 0.5 * g[j] / all.len() as f64;
            }
        }
        println!(
            "{}: Sha'ath tonic, learned mode → exact {:.1} %, tonic {:.1} % · weights (all) {:?}",
            KINDS[k],
            exact as f64 / rows.len() as f64 * 100.0,
            tonic_ok as f64 / rows.len() as f64 * 100.0,
            w.map(|x| (x * 1e4).round() / 1e4)
        );
    }
    // Pairs: is "do these two sound right together" better answered by the
    // detected keys or by the tracks' pitch-class profiles directly? Truth:
    // the tagged keys are compatible.
    let compatible = |a: Key, b: Key| {
        let (x, y) = (a.camelot_number() as i32, b.camelot_number() as i32);
        x == y || (a.minor == b.minor && ((x - y).rem_euclid(12) <= 1 || (y - x).rem_euclid(12) <= 1))
    };
    let k = 3; // 130–8372 Hz energy chroma
    // The shipped detector's decision on that chroma (tonic, then mode).
    let detected: Vec<Key> = rows
        .iter()
        .map(|(_, c)| {
            let c = norm(&c[k]);
            let tonic = key::best_key(&c, &key::profiles::SHAATH, false)[0].1.tonic;
            let at = |i: usize| c[(i + tonic as usize) % 12] * 12.0;
            let z = -0.6392 + 11.6574 * (at(3) - at(4)) + 3.1251 * (at(8) - at(9)) + 4.3208 * (at(10) - at(11));
            Key { tonic, minor: z > 0.0 }
        })
        .collect();
    let cos = |a: &[f64; 12], b: &[f64; 12]| {
        let (a, b) = (norm(a), norm(b));
        let (ma, mb) = (a.iter().sum::<f64>() / 12.0, b.iter().sum::<f64>() / 12.0);
        let (mut n, mut da, mut db) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            n += (a[i] - ma) * (b[i] - mb);
            da += (a[i] - ma).powi(2);
            db += (b[i] - mb).powi(2);
        }
        n / (da * db).sqrt().max(1e-12)
    };
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut pairs = vec![];
    for _ in 0..20000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let (i, j) = ((seed % rows.len() as u64) as usize, ((seed >> 32) % rows.len() as u64) as usize);
        if i != j {
            pairs.push((i, j));
        }
    }
    let truth: Vec<bool> = pairs.iter().map(|&(i, j)| compatible(rows[i].0, rows[j].0)).collect();
    let share = truth.iter().filter(|t| **t).count() as f64 / pairs.len() as f64;
    let report = |name: &str, pred: &[bool]| {
        let n = pred.len() as f64;
        let acc = pred.iter().zip(&truth).filter(|(p, t)| p == t).count() as f64 / n;
        let allow_bad = pred.iter().zip(&truth).filter(|(p, t)| **p && !**t).count() as f64
            / truth.iter().filter(|t| !**t).count() as f64;
        let block_good = pred.iter().zip(&truth).filter(|(p, t)| !**p && **t).count() as f64
            / truth.iter().filter(|t| **t).count() as f64;
        println!(
            "  {name:34} agree {:5.1} %  clashes let through {:5.1} %  good pairs blocked {:5.1} %",
            acc * 100.0,
            allow_bad * 100.0,
            block_good * 100.0
        );
    };
    println!("\n{} pairs, {:.1} % compatible by their tags", pairs.len(), share * 100.0);
    let by_keys: Vec<bool> = pairs.iter().map(|&(i, j)| compatible(detected[i], detected[j])).collect();
    report("detected keys", &by_keys);
    let sims: Vec<f64> = pairs.iter().map(|&(i, j)| cos(&rows[i].1[k], &rows[j].1[k])).collect();
    for tau in [0.5, 0.6, 0.7, 0.75, 0.8, 0.85] {
        let pred: Vec<bool> = sims.iter().map(|s| *s >= tau).collect();
        report(&format!("pitch-class profiles ≥ {tau}"), &pred);
    }
    println!("\n(exact % / compatible %)");
}

/// How often the analysed tempo is right, per bin of each raw confidence
/// score, against the library's own BPM tags: the data a calibrated
/// (operational) tempo probability is fitted on.
fn calibrate() {
    let db = patch_library::open_default().expect("library");
    let mut rows: Vec<(bool, [f64; 3])> = vec![];
    for t in db.tracks().expect("tracks") {
        let Some(mut a) = patch_analysis::cached(std::path::Path::new(&t.path)) else {
            continue;
        };
        a.use_grid();
        let (Some(bpm), Some(tag)) = (a.bpm, a.tag_bpm) else {
            continue;
        };
        // A tag in the other octave is a tagging convention, not ground truth.
        if !(60.0..=200.0).contains(&tag) {
            continue;
        }
        let right = (bpm - tag).abs() <= (tag * 0.005).max(0.6);
        let g = a.grid.as_ref();
        rows.push((
            right,
            [
                a.bpm_confidence as f64,
                g.map_or(0.0, |g| g.mixability as f64),
                g.map_or(0.0, |g| g.phase_confidence as f64),
            ],
        ));
    }
    if std::env::var_os("PATCH_CALIBRATE_DUMP").is_some() {
        for (ok, f) in &rows {
            println!("{}\t{:.4}\t{:.4}\t{:.4}", u8::from(*ok), f[0], f[1], f[2]);
        }
        return;
    }
    // Key: the detector, run fresh, against each tagged key (the cache
    // keeps a tag in place of the detector's answer on older records).
    let tagged: Vec<(String, patch_analysis::key::Key)> = db
        .tracks()
        .expect("tracks")
        .into_iter()
        .filter_map(|t| {
            let k = patch_analysis::read_tags(std::path::Path::new(&t.path)).1?;
            Some((t.path, k))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::<(bool, bool, f64)>::new());
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((path, tag)) = tagged.get(i) else { break };
                    let Ok(pcm) = patch_analysis::decode(std::path::Path::new(path)) else {
                        continue;
                    };
                    let mono: Vec<f32> = pcm
                        .chunks_exact(4)
                        .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
                        .collect();
                    let Some((det, conf)) =
                        patch_analysis::key::detect(&mono, patch_analysis::RATE / 2)
                    else {
                        continue;
                    };
                    // Exact, or DJ-compatible: same Camelot number (relative)
                    // or ±1 with the same letter (a fifth).
                    let (x, y) = (tag.camelot_number() as i32, det.camelot_number() as i32);
                    let near = x == y
                        || (tag.minor == det.minor && ((x - y).rem_euclid(12) == 1 || (y - x).rem_euclid(12) == 1));
                    found.lock().unwrap().push((*tag == det, near, conf as f64));
                }
            });
        }
    });
    let keys = found.into_inner().unwrap();
    if std::env::var_os("PATCH_CALIBRATE_DUMP_KEYS").is_some() {
        for (exact, near, conf) in &keys {
            println!("{}\t{}\t{conf:.4}", u8::from(*exact), u8::from(*near));
        }
        return;
    }
    let n = rows.len();
    let right = rows.iter().filter(|r| r.0).count();
    if !keys.is_empty() {
        let k = keys.len() as f64;
        println!(
            "{} tracks with a key tag · key exact {:.1} %, compatible {:.1} %",
            keys.len(),
            keys.iter().filter(|r| r.0).count() as f64 / k * 100.0,
            keys.iter().filter(|r| r.1).count() as f64 / k * 100.0
        );
        println!("key confidence: bin   tracks  exact  compatible");
        for b in 0..10 {
            let (lo, hi) = (b as f64 / 10.0, (b + 1) as f64 / 10.0);
            let bin: Vec<&(bool, bool, f64)> = keys
                .iter()
                .filter(|r| r.2 >= lo && (r.2 < hi || (b == 9 && r.2 <= 1.0)))
                .collect();
            if !bin.is_empty() {
                let m = bin.len() as f64;
                println!(
                    "  {lo:.1}–{hi:.1}  {:5}  {:5.1} %  {:5.1} %",
                    bin.len(),
                    bin.iter().filter(|r| r.0).count() as f64 / m * 100.0,
                    bin.iter().filter(|r| r.1).count() as f64 / m * 100.0
                );
            }
        }
    }
    println!("{n} tracks with a tag · tempo right {:.1} %", right as f64 / n.max(1) as f64 * 100.0);
    for (k, name) in ["bpm_confidence", "grid mixability", "phase confidence"].iter().enumerate() {
        println!("\n{name}: bin   tracks  right");
        for b in 0..10 {
            let (lo, hi) = (b as f64 / 10.0, (b + 1) as f64 / 10.0);
            let bin: Vec<bool> = rows
                .iter()
                .filter(|r| r.1[k] >= lo && (r.1[k] < hi || (b == 9 && r.1[k] <= 1.0)))
                .map(|r| r.0)
                .collect();
            if !bin.is_empty() {
                println!(
                    "  {lo:.1}–{hi:.1}  {:5}  {:5.1} %",
                    bin.len(),
                    bin.iter().filter(|x| **x).count() as f64 / bin.len() as f64 * 100.0
                );
            }
        }
    }
}

fn tempo_tags(list: &std::path::Path) {
    let rows: Vec<(String, f64)> = std::fs::read_to_string(list)
        .expect("tempo tag list")
        .lines()
        .filter_map(|l| {
            let (p, t) = l.split_once('\t')?;
            Some((p.to_string(), t.trim().parse().ok()?))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let t0 = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((path, tag)) = rows.get(i) else { break };
                    let Ok(pcm) = patch_analysis::decode(std::path::Path::new(path)) else {
                        continue;
                    };
                    let mono: Vec<f32> = pcm
                        .chunks_exact(4)
                        .map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25)
                        .collect();
                    let bpm = patch_analysis::tempo::detect(&mono, patch_analysis::RATE / 2)
                        .map(|t| t.bpm);
                    results.lock().unwrap().push((path.clone(), *tag, bpm));
                }
            });
        }
    });
    let results = results.into_inner().unwrap();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for (path, tag, bpm) in &results {
        let kind = match bpm {
            None => "none",
            Some(b) if (b - tag).abs() <= (tag * 0.005).max(0.6) => "exact",
            Some(b) => {
                let r = b / tag;
                if (r - 2.0).abs() < 0.02 || (r - 0.5).abs() < 0.02 {
                    "octave"
                } else if (r - 1.5).abs() < 0.02 || (r - 2.0 / 3.0).abs() < 0.02 {
                    "3:2"
                } else {
                    "other"
                }
            }
        };
        *counts.entry(kind).or_default() += 1;
        if kind != "exact" {
            println!("{kind}\ttag {tag}\tread {}\t{path}", bpm.map_or("none".into(), |b| format!("{b:.2}")));
        }
    }
    let n = results.len().max(1);
    println!(
        "\n{} tracks in {:.0} s · {:?} · exact {:.1} %",
        results.len(),
        t0.elapsed().as_secs_f64(),
        counts,
        100.0 * *counts.get("exact").unwrap_or(&0) as f64 / n as f64
    );
}

fn library_analysis(out: &mut String) {
    const RATE: usize = patch_analysis::RATE as usize;
    const SECONDS: usize = 30;
    let mut audio = Vec::with_capacity(RATE * SECONDS * 2);
    let beat_frames = RATE / 2; // 120 BPM
    for frame in 0..RATE * SECONDS {
        let phase = frame % beat_frames;
        let beat = if phase < RATE / 20 {
            let t = phase as f64 / RATE as f64;
            (std::f64::consts::TAU * (52.0 - 420.0 * t.min(0.08)) * t).sin()
                * (-45.0 * t).exp()
                * 0.8
        } else {
            0.0
        };
        let hat = if frame % (RATE / 4) < 8 { 0.22 } else { 0.0 };
        let sample = (beat + hat) as f32;
        audio.extend_from_slice(&[sample, sample * 0.97]);
    }
    let stats = time(5, || {
        black_box(patch_analysis::analyse(black_box(&audio)));
    });
    let analysis = patch_analysis::analyse(&audio);
    let encoded_bytes = serde_json::to_vec(&analysis).unwrap().len();
    let audio_seconds = audio.len() as f64 / 2.0 / RATE as f64;
    writeln!(
        out,
        "\n## Library understanding (Level 1, synthetic 120 BPM DJ track)\n"
    )
    .unwrap();
    writeln!(out, "Measures analysis CPU on a fixed {SECONDS} s stereo fixture; ffmpeg decoding and disk I/O are excluded. Cache size is the analyzer result's compact heap estimate and JSON encoding.\n").unwrap();
    writeln!(out, "| audio seconds | avg analysis ms | p99 ms | worst ms | tracks per second | × real time | result heap KiB | cached JSON KiB |").unwrap();
    writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|").unwrap();
    writeln!(
        out,
        "| {audio_seconds:.0} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2}× | {:.1} | {:.1} |",
        stats.avg / 1e6,
        stats.p99 / 1e6,
        stats.max / 1e6,
        1e9 / stats.avg,
        audio_seconds * 1e9 / stats.avg,
        analysis.heap_bytes() as f64 / 1024.0,
        encoded_bytes as f64 / 1024.0,
    )
    .unwrap();
}

/// Representative control-plane/deck work is timed separately from the
/// callback: actual disk recording and PipeWire scheduling are not simulated.
fn dj_workloads(out: &mut String, tier: Tier) {
    use patch_engine::{
        deck::{
            pcm::Pcm,
            transport::{Cmd, Transport},
        },
        fx,
    };
    writeln!(out, "\n## DJ workloads (48 kHz)\n").unwrap();
    writeln!(out, "External deck inputs, three FX, eight-band master EQ, cue and limiter; recording conversion is a separate consumer. No disk I/O.\n").unwrap();
    writeln!(
        out,
        "| tier | workload | frames | avg µs | p99 µs | worst µs |\n|---|---|---|---|---|---|"
    )
    .unwrap();
    for frames in [64usize, 512, 1024] {
        let p = Arc::new(Params::new(&["bus", "deck-a", "deck-b"]));
        let mut eq = patch_core::eq::EqState::default();
        for b in &mut eq.bands {
            b.on = true;
            b.gain_db = 3.0;
        }
        p.master_eq.set(&eq);
        p.set_fx(
            1,
            fx::Settings {
                on: true,
                kind: fx::Kind::Echo,
                chain: [Some(fx::Kind::Sweep), Some(fx::Kind::Reverb)],
                depth: 0.5,
                ..Default::default()
            },
        );
        p.set_pfl(1, true);
        let mut rt = Rt::new(p.clone());
        let inputs: Vec<Vec<f32>> = (0..6).map(|i| sig(frames, i as f32)).collect();
        let ins: Vec<_> = inputs.iter().map(|v| Some(v.as_slice())).collect();
        let (mut l, mut r, mut cl, mut cr) = (
            vec![0.0; frames],
            vec![0.0; frames],
            vec![0.0; frames],
            vec![0.0; frames],
        );
        let mut clock = Clock {
            position: 0,
            nsec: 0,
            rate: 48_000,
            frames: frames as u32,
            delay: 0,
            ..Default::default()
        };
        let mut times = Vec::with_capacity(2201);
        // Consumer work is outside the inner callback timer.
        time(2000, || {
            clock.position += frames as u64;
            let at = Instant::now();
            rt.process(&clock, &ins, [&mut l, &mut r], Some([&mut cl, &mut cr]));
            times.push(at.elapsed().as_nanos() as u64);
            // SAFETY: one thread runs the callback and drains, in turn.
            unsafe { p.meters.drain(|b| {
                black_box(b);
            }) };
            unsafe { p.program.drain(|s| {
                black_box(s);
            }) };
        });
        let s = stats(times.into_iter().skip(201).collect());
        writeln!(
            out,
            "| {} | mix + FX + EQ + cue + limiter | {frames} | {} | {} | {} |",
            tier_short(tier),
            us(s.avg),
            us(s.p99),
            us(s.max)
        )
        .unwrap();
        let audio = sig(frames * 2, 0.5);
        let mut bytes = Vec::with_capacity(frames * 6);
        let mut dither = patch_record::Dither::tpdf(0x5eed);
        let mut converted = patch_record::ConvStats::default();
        let s = time(2000, || {
            bytes.clear();
            patch_record::convert_s24(&audio, &mut dither, &mut converted, &mut bytes);
            black_box(&bytes);
        });
        writeln!(
            out,
            "| {} | recording: 24-bit conversion | {frames} | {} | {} | {} |",
            tier_short(tier),
            us(s.avg),
            us(s.p99),
            us(s.max)
        )
        .unwrap();
    }
    let pcm = Pcm::from_samples(48_000, sig(48_000 * 2 * 4, 0.2));
    for (name, keylock, tempo) in [
        ("deck direct", false, 0.0),
        ("deck resampling +4%", false, 0.04),
        ("deck keylock +4%", true, 0.04),
    ] {
        let mut deck = Transport::new(48_000);
        for cmd in [
            Cmd::Load(pcm.clone()),
            Cmd::Keylock(keylock),
            Cmd::Tempo(tempo),
            Cmd::Play,
            Cmd::AutoLoop(4.0),
        ] {
            deck.apply(cmd, None);
        }
        let s = time(2000, || {
            deck.render(&mut |tag, samples| {
                black_box(tag);
                black_box(samples);
            })
        });
        writeln!(
            out,
            "| {} | {name} (worker render) | {} | {} | {} | {} |",
            tier_short(tier),
            patch_engine::deck::transport::CHUNK,
            us(s.avg),
            us(s.p99),
            us(s.max)
        )
        .unwrap();
    }
}

fn tier_short(t: Tier) -> &'static str {
    match t {
        Tier::Reference => "reference",
        Tier::Sse2 => "sse2",
        Tier::Avx2 => "avx2",
    }
}

fn main() {
    let best = patch_dsp::detected();
    // Dispatch is intentionally immutable. Each tier must start a fresh process.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--analysis-eval") {
        let manifest = args
            .get(1)
            .expect("--analysis-eval requires a corpus JSON path");
        rhythm_evaluation(std::path::Path::new(manifest));
        return;
    }
    if args.first().map(String::as_str) == Some("--downbeat-check") {
        downbeat_check(args.get(1).and_then(|n| n.parse().ok()).unwrap_or(120));
        return;
    }
    if args.first().map(String::as_str) == Some("--groove-eval") {
        groove_eval();
        return;
    }
    if args.first().map(String::as_str) == Some("--section-real") {
        section_real(args.get(1).and_then(|n| n.parse().ok()).unwrap_or(20));
        return;
    }
    if args.first().map(String::as_str) == Some("--section-eval") {
        section_eval();
        return;
    }
    if args.first().map(String::as_str) == Some("--chord-eval") {
        chord_eval();
        return;
    }
    if args.first().map(String::as_str) == Some("--rolling") {
        rolling(args.get(1).map(String::as_str), args.get(2).and_then(|n| n.parse().ok()).unwrap_or(20));
        return;
    }
    if args.first().map(String::as_str) == Some("--key-eval") {
        key_eval();
        return;
    }
    if args.first().map(String::as_str) == Some("--calibrate") {
        calibrate();
        return;
    }
    if args.first().map(String::as_str) == Some("--tempo-tags") {
        let list = args
            .get(1)
            .expect("--tempo-tags requires a list of \"path<TAB>tagged BPM\" lines");
        tempo_tags(std::path::Path::new(list));
        return;
    }
    if args.first().map(String::as_str) == Some("--analysis-only") {
        let mut out = String::from("# Patch Level 1 analysis benchmark\n");
        library_analysis(&mut out);
        print!("{out}");
        return;
    }
    if args.first().map(String::as_str) != Some("--tier") {
        let mut out = String::from(
            "# Patch benchmark matrix\n\nEach tier runs in a fresh process; callback timing excludes gain updates and ring draining.\n\n",
        );
        for tier in [Tier::Reference, Tier::Sse2, Tier::Avx2]
            .into_iter()
            .filter(|t| *t <= best)
        {
            let result =
                std::process::Command::new(std::env::current_exe().expect("benchmark executable"))
                    .args(["--tier", tier_short(tier)])
                    .env("PATCH_DSP", tier_short(tier))
                    .output()
                    .expect("start tier benchmark");
            assert!(
                result.status.success(),
                "tier {} failed: {}",
                tier_short(tier),
                String::from_utf8_lossy(&result.stderr)
            );
            out.push_str(&String::from_utf8(result.stdout).expect("benchmark UTF-8"));
            out.push('\n');
        }
        print!("{out}");
        if let Some(path) = args.first() {
            std::fs::write(path, out).expect("write report");
        }
        return;
    }
    let tier = match args.get(1).map(String::as_str) {
        Some("reference") => Tier::Reference,
        Some("sse2") => Tier::Sse2,
        Some("avx2") => Tier::Avx2,
        _ => panic!("--tier requires reference, sse2 or avx2"),
    };
    assert!(tier <= best, "unsupported tier");
    patch_dsp::set(tier);
    let tiers = [tier];
    let table = patch_dsp::registry::init();
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| l.starts_with("model name"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_default();
    let mut out = String::new();
    writeln!(out, "# Patch benchmark\n").unwrap();
    writeln!(
        out,
        "CPU: {cpu}. Best tier: {}. Patch {}.\n",
        best.name(),
        env!("CARGO_PKG_VERSION")
    )
    .unwrap();
    writeln!(
        out,
        "Not a real-time thread: worst cases include scheduler noise (upper bounds).\n"
    )
    .unwrap();
    for op in patch_dsp::abi::Op::ALL {
        writeln!(out, "Selected `{}`: `{}`", op.name(), table.get(op).name).unwrap();
    }
    writeln!(out).unwrap();
    engine_cycles(&mut out, &tiers);
    channel_scaling(&mut out, &tiers);
    kernels(&mut out, &tiers);
    library_analysis(&mut out);
    dj_workloads(&mut out, tier);
    print!("{out}");
}

/// Evaluate cached Level 1 rhythm evidence against a versioned, checksummed
/// human-annotation corpus. Audio decoding and analysis remain off the RT path.
fn rhythm_evaluation(manifest_path: &std::path::Path) {
    use std::path::Path;
    let text = std::fs::read_to_string(manifest_path).expect("read rhythm corpus manifest");
    let corpus: patch_analysis::evaluation::RhythmCorpus =
        serde_json::from_str(&text).expect("parse rhythm corpus manifest");
    corpus.validate().expect("validate rhythm corpus manifest");
    let base = manifest_path.parent().unwrap_or(Path::new("."));
    let mut results = Vec::with_capacity(corpus.cases.len());
    for case in &corpus.cases {
        let path = base.join(&case.path);
        let actual_hash = patch_analysis::evaluation::sha256_file(&path)
            .unwrap_or_else(|error| panic!("{}: cannot hash source: {error}", case.id));
        assert_eq!(
            actual_hash.to_ascii_lowercase(),
            case.sha256.to_ascii_lowercase(),
            "{}: audio checksum changed; annotation is stale",
            case.id
        );
        let analysis = patch_analysis::analyse_file(&path)
            .unwrap_or_else(|error| panic!("{}: analysis failed: {error}", case.id));
        results.push(patch_analysis::evaluation::evaluate_rhythm(
            &analysis,
            case,
            patch_analysis::evaluation::DEFAULT_BEAT_TOLERANCE_MS,
        ));
    }
    let summary = patch_analysis::evaluation::summarize_rhythm(&results);
    let report = serde_json::json!({
        "schema_version": 1,
        "corpus": corpus.name,
        "patch_version": env!("CARGO_PKG_VERSION"),
        "case_count": results.len(),
        "beat_tolerance_ms": patch_analysis::evaluation::DEFAULT_BEAT_TOLERANCE_MS,
        "summary": summary,
        "cases": results,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize evaluation report")
    );
}

/// Rolling-planning benchmark: from `sets` start tracks (spread over the
/// library, or the tracks under `folder`), play 15-track sets taking the
/// AI's first suggestion each time, and report the raw fit of each
/// transition (selection score, without the lookahead nudge), weak fits
/// (< 0.5) and dead ends (no suggestion left): patch-bench --rolling
/// [folder] [sets]. The baseline for multi-step set planning (a one-step
/// onward-potential tie-break measured ±0.005 mean fit, no fewer dead
/// ends, and was not kept).
fn rolling(folder: Option<&str>, sets: usize) {
    let library: Vec<patch_ai::TrackInfo> = patch_engine::track_analysis::library()
        .iter()
        .filter(|(p, ..)| folder.is_none_or(|f| p.starts_with(f)))
        .filter_map(|(p, t, a, r)| patch_engine::automix::track_info(p, t, a, *r))
        .filter(|t| t.analysis.bpm.is_some() && t.analysis.duration > 150.0)
        .collect();
    println!("{} tracks", library.len());
    let (mut fits, mut weak, mut dead, mut picks) = (vec![], 0, 0, 0);
    let t0 = std::time::Instant::now();
    for k in 0..sets.min(library.len()) {
        let start = &library[k * library.len() / sets.max(1)];
        let mut history: Vec<String> = vec![];
        let mut deck = 0;
        let at = |t: &patch_ai::TrackInfo| (t.analysis.duration - 100.0).max(30.0);
        let Some(mut now) = patch_engine::automix::playing(deck, &start.path, at(start), 0.0, 0) else {
            continue;
        };
        for _ in 0..15 {
            let ctx = patch_ai::Context {
                playing: now.clone(),
                next_deck: if deck == 0 { patch_core::command::Deck::B } else { patch_core::command::Deck::A },
                direction: patch_core::intent::Direction::Hold,
                history: &history,
                recent: &[],
            };
            let all = patch_ai::suggest(&ctx, &library, 6);
            let Some(pick) = all.first() else {
                dead += 1;
                break;
            };
            let track = library.iter().find(|t| t.id == pick.track_id).unwrap();
            let fit = patch_ai::select::score_factors(&ctx, track).map_or(0.0, |f| f.0);
            fits.push(fit);
            weak += usize::from(fit < 0.5);
            picks += 1;
            history.push(now.track.id.clone());
            deck = 1 - deck;
            match patch_engine::automix::playing(deck, &track.path, at(track), 0.0, 0) {
                Some(p) => now = p,
                None => break,
            }
        }
    }
    fits.sort_by(f32::total_cmp);
    let mean = fits.iter().sum::<f32>() / fits.len().max(1) as f32;
    println!(
        "{picks} picks: mean fit {:.3}, p10 {:.3}, weak (< 0.5) {weak}, dead ends {dead}, {:.0} ms a pick",
        mean,
        fits.get(fits.len() / 10).copied().unwrap_or(0.0),
        t0.elapsed().as_secs_f64() * 1000.0 / picks.max(1) as f64
    );
}

/// Chord estimation against known progressions: synthetic tracks (pad
/// triads, bass on the root, kick on the beat, hats off it) of 8 common
/// progressions in all 12 keys, one chord a bar at 120-128 BPM. Scores the
/// share of bars whose top chord (root and quality) is right, for fixed
/// 4 s frames (as regions measured them) and for bar-aligned frames.
fn chord_eval() {
    const RATE: u32 = 22_050;
    // (degree in semitones from the tonic, minor) per bar.
    let major: [&[(u8, bool)]; 4] = [
        &[(0, false), (7, false), (9, true), (5, false)], // I V vi IV
        &[(9, true), (5, false), (0, false), (7, false)], // vi IV I V
        &[(0, false), (5, false), (7, false), (5, false)], // I IV V IV
        &[(2, true), (7, false), (0, false), (9, true)],  // ii V I vi
    ];
    let minor: [&[(u8, bool)]; 4] = [
        &[(0, true), (8, false), (3, false), (10, false)], // i VI III VII
        &[(0, true), (5, true), (7, true), (0, true)],     // i iv v i
        &[(0, true), (10, false), (8, false), (10, false)], // i VII VI VII
        &[(0, true), (7, true), (5, true), (0, true)],      // i v iv i
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let tau = std::f64::consts::TAU;
    let (mut fixed_ok, mut bar_ok, mut bars_total) = (0usize, 0usize, 0usize);
    let mut fixed_root = 0usize;
    for (pi, prog) in major.iter().chain(minor.iter()).enumerate() {
        for tonic in 0..12u8 {
            let bpm = 120.0 + (rnd() * 8.0).floor();
            let bar_s = 4.0 * 60.0 / bpm;
            let bars = 16;
            let n = (bar_s * bars as f64 * f64::from(RATE)) as usize;
            let mut x = vec![0.0f32; n];
            let chords: Vec<(u8, bool)> = (0..bars)
                .map(|b| {
                    let (d, m) = prog[b % prog.len()];
                    ((tonic + d) % 12, m)
                })
                .collect();
            let phase: Vec<f64> = (0..8).map(|_| rnd() * tau).collect();
            for (b, (root, m)) in chords.iter().enumerate() {
                let notes = [
                    i32::from(*root) + 60,
                    i32::from(*root) + 60 + if *m { 3 } else { 4 },
                    i32::from(*root) + 67,
                ];
                let start = (b as f64 * bar_s * f64::from(RATE)) as usize;
                let end = (((b + 1) as f64) * bar_s * f64::from(RATE)) as usize;
                for i in start..end.min(n) {
                    let t = i as f64 / f64::from(RATE);
                    let mut v = 0.0;
                    for (k, note) in notes.iter().enumerate() {
                        let f = 440.0 * 2f64.powf((f64::from(*note) - 69.0) / 12.0);
                        for h in 1..=5 {
                            v += 0.08 / f64::from(h) * (tau * f * f64::from(h) * t + phase[k]).sin();
                        }
                    }
                    // Bass on the root, two octaves down.
                    let fb = 440.0 * 2f64.powf((f64::from(*root) + 36.0 - 69.0) / 12.0);
                    v += 0.25 * (tau * fb * t).sin() + 0.08 * (tau * 2.0 * fb * t).sin();
                    x[i] += v as f32;
                }
            }
            // Drums: a pitched-down kick on every beat, noise hats between.
            let beat = (60.0 / bpm * f64::from(RATE)) as usize;
            for k in (0..n).step_by(beat) {
                for j in 0..(RATE as usize / 6).min(n - k) {
                    let t = j as f64 / f64::from(RATE);
                    let f = 50.0 + 70.0 * (-t * 30.0).exp();
                    x[k + j] += (0.9 * (-t * 12.0).exp() * (tau * f * t).sin()) as f32;
                }
                let h = k + beat / 2;
                for j in 0..(RATE as usize / 30).min(n.saturating_sub(h)) {
                    x[h + j] += ((rnd() - 0.5) * 0.3 * (-(j as f64) / 300.0).exp()) as f32;
                }
            }
            let truth = |t: f64| chords[((t / bar_s) as usize).min(bars - 1)];
            let top = |a: &[f32]| {
                patch_analysis::region::estimate_chords(a, RATE)
                    .first()
                    .map(|c| (c.root, c.minor))
            };
            // Fixed 4 s frames; each bar judged by the frame holding its middle.
            let frame = 4 * RATE as usize;
            let fixed: Vec<Option<(u8, bool)>> = x.chunks(frame).map(top).collect();
            for (b, want) in chords.iter().enumerate() {
                let mid = ((b as f64 + 0.5) * bar_s * f64::from(RATE)) as usize / frame;
                let got = fixed.get(mid).copied().flatten();
                fixed_ok += usize::from(got == Some(*want));
                fixed_root += usize::from(got.map(|g| g.0) == Some(want.0));
                let (s0, s1) = (
                    (b as f64 * bar_s * f64::from(RATE)) as usize,
                    (((b + 1) as f64) * bar_s * f64::from(RATE)) as usize,
                );
                bar_ok += usize::from(top(&x[s0..s1.min(n)]) == Some(*want));
                bars_total += 1;
                let _ = truth;
            }
            let _ = pi;
        }
    }
    let pct = |k: usize| 100.0 * k as f64 / bars_total as f64;
    println!("{bars_total} bars of 8 progressions in 12 keys");
    println!("fixed 4 s frames: chord {:.1} % (root {:.1} %)", pct(fixed_ok), pct(fixed_root));
    println!("bar-aligned frames: chord {:.1} %", pct(bar_ok));
}

/// Section boundaries against known ones: synthetic club tracks of six
/// 16-bar sections (intro: kick and hats; build: + bass; drop: + pad and
/// lead; breakdown: pad alone; drop; outro: kick and hats) at 120-128 BPM,
/// through the full Level 1 analysis. A true boundary is found when a
/// detected one lies within a bar; detected ones farther than a bar from
/// every true boundary are false.
fn section_eval() {
    let rate = patch_analysis::RATE;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let tau = std::f64::consts::TAU;
    // (kick, hats, bass, pad, lead) per section.
    let parts = [
        (true, true, false, false, false),
        (true, true, true, false, false),
        (true, true, true, true, true),
        (false, false, false, true, false),
        (true, true, true, true, true),
        (true, true, false, false, false),
    ];
    let (mut hits, mut truths, mut false_pos, mut err_sum) = (0usize, 0usize, 0usize, 0.0f64);
    for track in 0..8 {
        let bpm = 120.0 + (rnd() * 8.0).floor();
        let beat_s = 60.0 / bpm;
        let section_s = 16.0 * 4.0 * beat_s;
        let n = (section_s * parts.len() as f64 * f64::from(rate)) as usize;
        let mut x = vec![0.0f32; n];
        let root = 36.0 + (rnd() * 12.0).floor();
        for (si, (kick, hats, bass, pad, lead)) in parts.iter().enumerate() {
            let s0 = (si as f64 * section_s * f64::from(rate)) as usize;
            let s1 = (((si + 1) as f64) * section_s * f64::from(rate)) as usize;
            let beat = (beat_s * f64::from(rate)) as usize;
            for i in s0..s1.min(n) {
                let t = i as f64 / f64::from(rate);
                let in_beat = (i - s0) % beat;
                let tb = in_beat as f64 / f64::from(rate);
                let mut v = 0.0;
                if *kick {
                    let f = 50.0 + 70.0 * (-tb * 30.0).exp();
                    v += 0.8 * (-tb * 10.0).exp() * (tau * f * tb).sin();
                }
                if *hats && in_beat >= beat / 2 && in_beat < beat / 2 + rate as usize / 40 {
                    v += (rnd() - 0.5) * 0.25;
                }
                let f0 = 440.0 * 2f64.powf((root - 69.0) / 12.0);
                if *bass {
                    v += 0.3 * (tau * f0 * t).sin() * if in_beat < beat / 2 { 1.0 } else { 0.6 };
                }
                if *pad {
                    for m in [24.0, 27.0, 31.0] {
                        v += 0.07 * (tau * f0 * 2f64.powf(m / 12.0) * t).sin();
                    }
                }
                if *lead {
                    let step = ((i - s0) / (beat / 2)) % 8;
                    let m = [36.0, 39.0, 43.0, 46.0, 43.0, 39.0, 36.0, 31.0][step];
                    v += 0.12 * (tau * f0 * 2f64.powf(m / 12.0) * t).sin();
                }
                x[i] = v as f32;
            }
        }
        let stereo: Vec<f32> = x.iter().flat_map(|v| [*v, *v]).collect();
        let a = patch_analysis::analyse(&stereo);
        let found: Vec<f64> = a
            .structure
            .as_ref()
            .map(|s| s.boundaries.iter().map(|b| b.time_s).collect())
            .unwrap_or_default();
        let truth: Vec<f64> = (1..parts.len()).map(|k| k as f64 * section_s).collect();
        let bar_s = 4.0 * beat_s;
        for t in &truth {
            truths += 1;
            if let Some(d) = found.iter().map(|f| (f - t).abs()).min_by(f64::total_cmp)
                && d <= bar_s
            {
                hits += 1;
                err_sum += d;
            }
        }
        false_pos += found
            .iter()
            .filter(|f| truth.iter().all(|t| (*f - t).abs() > bar_s))
            .count();
        println!(
            "track {track}: {bpm} BPM, found {:?}",
            found.iter().map(|f| (f / bar_s * 10.0).round() / 10.0).collect::<Vec<_>>()
        );
    }
    println!(
        "{hits} of {truths} boundaries within a bar (mean error {:.2} s), {false_pos} false",
        err_sum / hits.max(1) as f64
    );
}

/// Boundaries per minute on real library tracks, without and with the
/// band-balance evidence (false boundaries show as many more a minute; a
/// club track changes section about every 32 bars, once a minute).
fn section_real(n: usize) {
    let db = patch_library::open_default().expect("library");
    let tracks = db.tracks().expect("tracks");
    let step = (tracks.len() / n.max(1)).max(1);
    let (mut before, mut after, mut minutes) = (0usize, 0usize, 0.0f64);
    for t in tracks.iter().step_by(step).take(n) {
        let path = std::path::Path::new(&t.path);
        let Ok(stereo) = patch_analysis::decode(path) else { continue };
        let a = patch_analysis::analyse(&stereo);
        let beats = a.rhythm.as_ref().map(|r| r.beats.as_slice()).unwrap_or(&[]);
        let bands = patch_analysis::structure::band_energy(&stereo, patch_analysis::RATE);
        let b0 = patch_analysis::structure::analyze(&a.energy, &[], beats, a.duration).boundaries.len();
        let b1 = patch_analysis::structure::analyze(&a.energy, &bands, beats, a.duration).boundaries.len();
        println!("{:>3} → {:>3}  {:.1} min  {}", b0, b1, a.duration / 60.0, t.path.rsplit('/').next().unwrap_or(""));
        before += b0;
        after += b1;
        minutes += a.duration / 60.0;
    }
    println!(
        "boundaries a minute: {:.2} → {:.2}",
        before as f64 / minutes.max(1e-9),
        after as f64 / minutes.max(1e-9)
    );
}

/// Swing reading against known grooves: a kick on every beat and a hat on
/// the off-beat placed at phase `swing` (0.50 straight … 0.67 triplet), with
/// and without quieter straight 16th hats between, at 124 BPM. Prints the
/// measured mean off-beat phase per groove.
fn groove_eval() {
    let rate = 22_050u32;
    let tau = std::f64::consts::TAU;
    let mut worst = 0.0f64;
    for bpm in [100.0, 124.0, 140.0] {
    let beat_s = 60.0 / bpm;
    let beats_n = 128;
    let beats: Vec<patch_analysis::tempo::BeatEvent> = (0..beats_n)
        .map(|i| patch_analysis::tempo::BeatEvent {
            time_s: f64::from(i) * beat_s,
            beat_index: i64::from(i),
            interval_s: beat_s as f32,
            bar_index: Some(i64::from(i / 4)),
            beat_in_bar: Some((i % 4) as u8),
            onset_offset_s: Some(0.0),
            onset_support: 1.0,
        })
        .collect();
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    for sixteenths in [false, true] {
        for swing in [0.50, 0.54, 0.58, 0.62, 0.66] {
            let n = (beat_s * f64::from(beats_n) * f64::from(rate)) as usize;
            let mut x = vec![0.0f32; n];
            let mut hit = |at: f64, gain: f64, kick: bool, x: &mut Vec<f32>| {
                let i0 = (at * f64::from(rate)) as usize;
                for j in 0..(rate as usize / 8).min(n.saturating_sub(i0)) {
                    let t = j as f64 / f64::from(rate);
                    x[i0 + j] += if kick {
                        (gain * (-t * 12.0).exp() * (tau * (50.0 + 70.0 * (-t * 30.0).exp()) * t).sin()) as f32
                    } else {
                        (gain * noise() * (-t * 120.0).exp()) as f32
                    };
                }
            };
            for b in 0..beats_n {
                let t0 = f64::from(b) * beat_s;
                hit(t0, 0.9, true, &mut x);
                hit(t0 + swing * beat_s, 0.5, false, &mut x);
                if sixteenths {
                    hit(t0 + 0.25 * beat_s, 0.2, false, &mut x);
                    hit(t0 + 0.75 * beat_s, 0.2, false, &mut x);
                }
            }
            let g = patch_analysis::region::groove_of(&x, rate, &beats);
            let m = g.mean_phase.map(f64::from);
            if let Some(m) = m {
                worst = worst.max((m - swing).abs());
            }
            println!(
                "{bpm} BPM {} swing {swing:.2}: mean phase {}, histogram {:?}",
                if sixteenths { "with 16ths" } else { "8ths only" },
                m.map_or("–".into(), |m| format!("{m:.3}")),
                g.phase_histogram.map(|v| (v * 100.0).round() / 100.0)
            );
        }
    }
    }
    println!("worst swing error: {worst:.3} of a beat");
}


/// Downbeats against the music's own sections: club tracks change section
/// (drops, breaks, new loops) on bar 1. Over `n` analysed library tracks,
/// the share of each track's 12 strongest beat-to-beat energy changes that
/// land on its grid's bar 1 (a quarter by chance), by the analyzer's own
/// downbeat confidence.
fn downbeat_check(n: usize) {
    let lib = patch_engine::track_analysis::library();
    let step = (lib.len() / n.max(1)).max(1);
    let mut rows: Vec<(f32, f64)> = vec![];
    let mut agree: Vec<(f32, bool, bool)> = vec![];
    for (path, ..) in lib.iter().step_by(step).take(n) {
        let Some(a) = patch_analysis::cached(std::path::Path::new(path)) else { continue };
        // The grid the decks play by (its own offset, downbeat, confidence);
        // FRESH=1 measures it again with the current analyzer.
        let fresh = std::env::var_os("FRESH").and_then(|_| {
            let stereo = patch_analysis::decode(std::path::Path::new(path)).ok()?;
            let mono: Vec<f32> = stereo.chunks_exact(4).map(|q| (q[0] + q[1] + q[2] + q[3]) * 0.25).collect();
            patch_analysis::grid::measure(&mono, patch_analysis::RATE / 2, a.grid.as_ref()?.bpm, a.beat_offset)
        });
        let Some(g) = fresh.as_ref().or(a.grid.as_ref()) else { continue };
        let (bpm, downbeat, offset, confidence) = match g.downbeat {
            Some(d) => (g.bpm, d, g.offset, g.downbeat_confidence),
            None => continue,
        };
        let rate = 4000u32;
        let Ok(out) = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i", path, "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "-"])
            .output()
        else {
            continue;
        };
        let x: Vec<f32> = out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let beat = 60.0 / bpm;
        let beats = ((x.len() as f64 / f64::from(rate) - offset) / beat).floor() as usize;
        if beats < 64 {
            continue;
        }
        let e: Vec<f64> = (0..beats)
            .map(|k| {
                let i0 = ((offset + k as f64 * beat) * f64::from(rate)).max(0.0) as usize;
                let i1 = (((offset + (k + 1) as f64 * beat) * f64::from(rate)) as usize).min(x.len());
                let seg = &x[i0.min(i1)..i1];
                (seg.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / seg.len().max(1) as f64 + 1e-9).ln()
            })
            .collect();
        let mut changes: Vec<(f64, usize)> = (4..beats - 4)
            .map(|k| ((e[k..k + 4].iter().sum::<f64>() - e[k - 4..k].iter().sum::<f64>()).abs() / 4.0, k))
            .collect();
        changes.sort_by(|p, q| q.0.total_cmp(&p.0));
        // One change per neighbourhood (a jump spans a few beats).
        let mut picked: Vec<usize> = vec![];
        for (_, k) in changes {
            if picked.iter().all(|p| p.abs_diff(k) > 4) {
                picked.push(k);
            }
            if picked.len() == 12 {
                break;
            }
        }
        let on_one = picked.iter().filter(|k| (**k + 4 - usize::from(downbeat) % 4) % 4 == 0).count();
        // The phase where most changes land, and how clearly.
        let mut by = [0usize; 4];
        for k in &picked {
            by[*k % 4] += 1;
        }
        let best = (0..4).max_by_key(|p| by[*p]).unwrap();
        let mut sorted = by;
        sorted.sort_unstable();
        let clear = sorted[3] >= sorted[2] + 3;
        agree.push((confidence, best == usize::from(downbeat) % 4, clear));
        rows.push((confidence, on_one as f64 / picked.len().max(1) as f64));
    }
    rows.sort_by(|p, q| p.0.total_cmp(&q.0));
    println!("{} tracks", rows.len());
    for (lo, hi) in [(0.0, 0.5), (0.5, 0.75), (0.75, 0.9), (0.9, 1.01)] {
        let g: Vec<f64> = rows.iter().filter(|r| r.0 >= lo && r.0 < hi).map(|r| r.1).collect();
        if g.is_empty() {
            continue;
        }
        let low = g.iter().filter(|s| **s < 0.34).count();
        println!(
            "downbeat confidence {lo:.2}-{hi:.2}: {} tracks, bar-1 share mean {:.2}, {low} at or below chance",
            g.len(),
            g.iter().sum::<f64>() / g.len() as f64
        );
        let a: Vec<&(f32, bool, bool)> = agree.iter().filter(|r| r.0 >= lo && r.0 < hi).collect();
        let clear: Vec<&&(f32, bool, bool)> = a.iter().filter(|r| r.2).collect();
        println!(
            "    section phase = grid downbeat: {} of {}; where the sections are clear: {} of {}",
            a.iter().filter(|r| r.1).count(),
            a.len(),
            clear.iter().filter(|r| r.1).count(),
            clear.len()
        );
    }
}
