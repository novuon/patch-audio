//! `cargo run --release -p patch-meter --bin patch-meter-bench`

use patch_meter::{Vu, reference, stats};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let frames = 1 << 20; // ~21.8 s of 48 kHz stereo
    let samples: Vec<f32> = (0..frames * 2)
        .map(|i| ((i as f32) * 0.000_731).sin() * 0.8)
        .collect();
    let bytes = (samples.len() * 4) as f64;
    let rounds = 50;

    let time = |label: &str, f: &mut dyn FnMut()| {
        f();
        let t = Instant::now();
        for _ in 0..rounds {
            f();
        }
        let secs = t.elapsed().as_secs_f64() / rounds as f64;
        let realtime = (frames as f64 / 48_000.0) / secs;
        println!(
            "{label:<34} {:>7.2} GB/s  {:>9.0}× realtime (48 kHz stereo)",
            bytes / secs / 1e9,
            realtime
        );
    };

    println!("patch-meter · stats backend: {}", patch_meter::backend());
    time("stats · assembly", &mut || {
        black_box(stats(black_box(&samples)));
    });
    time("stats · Rust reference", &mut || {
        black_box(reference::stats(black_box(&samples)));
    });
    let mut vu = Vu::new(48_000);
    time("VU ballistics · assembly", &mut || {
        black_box(vu.process(black_box(&samples)));
    });
    let mut state = [0.0; 4];
    let c = Vu::new(48_000).coeff();
    time("VU ballistics · Rust reference", &mut || {
        black_box(reference::ballistics(black_box(&samples), &mut state, c));
    });
}
