//! CPU-only control-path baseline. Does not measure device/audio latency.
use patch_controller::CompiledKeymap;
use patch_core::input::{Binding, InputEvent, Keymap, Trigger};
use std::{hint::black_box, time::Instant};

fn main() {
    let bindings = (0..8)
        .flat_map(|channel| {
            (0..128).map(move |cc| Binding {
                context: "midi".into(),
                on: Trigger::cc(channel, cc),
                action: serde_json::json!({"op":"dj.crossfader","position":"{value}"}),
                range: None,
                soft_takeover: false,
                source: None,
            })
        })
        .collect();
    let legacy = Keymap { bindings };
    let map = CompiledKeymap::new(legacy.clone());
    const EVENTS: usize = 100_000;
    let start = Instant::now();
    for i in 0..EVENTS {
        let event = InputEvent::MidiCc {
            channel: (i % 8) as u8,
            cc: (i % 128) as u8,
            value: (i % 128) as u8,
        };
        assert!(black_box(legacy.resolve("midi", black_box(&event))).is_some());
    }
    println!(
        "1024-binding linear baseline: {:.0} ns/event",
        start.elapsed().as_nanos() as f64 / EVENTS as f64
    );
    let mut samples = Vec::with_capacity(10);
    for _ in 0..10 {
        let start = Instant::now();
        for i in 0..EVENTS {
            let event = InputEvent::MidiCc {
                channel: (i % 8) as u8,
                cc: (i % 128) as u8,
                value: (i % 128) as u8,
            };
            assert!(black_box(map.resolve("midi", black_box(&event))).is_some());
        }
        samples.push(start.elapsed().as_nanos() as f64 / EVENTS as f64);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "1024-binding indexed command resolution: median {:.0} ns/event; worst batch {:.0} ns/event",
        samples[5], samples[9]
    );
    let mut parser = patch_core::midi::Parser::default();
    let start = Instant::now();
    let mut count = 0usize;
    for _ in 0..EVENTS {
        parser.feed(black_box(&[0xb0, 7, 64]), &mut |event| {
            black_box(event);
            count += 1;
        });
    }
    assert_eq!(count, EVENTS);
    println!(
        "MIDI parser: {:.0} ns/event",
        start.elapsed().as_nanos() as f64 / EVENTS as f64
    );
}
