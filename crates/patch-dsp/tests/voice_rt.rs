//! Allocation regression and coherent publication tests for the actual
//! processor called by Patch's audio callback.
use patch_dsp::voice::{Published, Settings, Voice};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! { static ALLOCS: Cell<Option<usize>> = const { Cell::new(None) }; }
struct CountingAllocator;
// SAFETY: allocation and deallocation are delegated unchanged to System.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|n| {
            if let Some(v) = n.get() {
                n.set(Some(v + 1));
            }
        });
        // SAFETY: the caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the pointer/layout pair came from the same System allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn callback_does_not_allocate_including_rate_changes() {
    let mut processor = Voice::default();
    let mut l = [0.1; 1024];
    let mut r = l;
    let settings = Settings {
        noise_db: Some(-55.0),
        expander_db: Some(-50.0),
        input_gain_db: 3.0,
        output_gain_db: -3.0,
        ..Settings::default()
    };
    ALLOCS.with(|n| n.set(Some(0)));
    for rate in [48000, 44100, 8000, 96000, 192000] {
        for _ in 0..8 {
            processor.process(&mut l, &mut r, &settings, rate);
        }
    }
    let allocations = ALLOCS.with(|n| n.replace(None).unwrap());
    assert_eq!(allocations, 0);
    assert!(l.iter().chain(&r).all(|s| s.is_finite()));
}

#[test]
fn publication_never_exposes_a_partial_profile() {
    let published = std::sync::Arc::new(Published::default());
    let first = Settings {
        hpf_hz: 60.0,
        input_gain_db: 6.0,
        ..Settings::default()
    };
    let second = Settings {
        hpf_hz: 120.0,
        input_gain_db: -6.0,
        noise_db: Some(-50.0),
        ..Settings::default()
    };
    published.set(first);
    let writer = published.clone();
    let thread = std::thread::spawn(move || {
        for i in 0..10000 {
            writer.set(if i % 2 == 0 { first } else { second });
        }
    });
    let mut processor = Voice::default();
    // Prime the cached settings before the writer's bounded reads can fail.
    while published.read().is_none() {
        std::thread::yield_now();
    }
    for _ in 0..10000 {
        if let Some(snapshot) = published.read() {
            assert!(snapshot == first || snapshot == second);
        }
        let _ = processor.read_settings(&published);
    }
    thread.join().unwrap();
    assert!(published.read().is_some());
}

#[test]
fn live_bypass_fades_then_returns_exact_dry_samples() {
    let mut processor = Voice::default();
    let settings = Settings {
        on: true,
        output_gain_db: 6.0,
        ..Settings::OFF
    };
    let raw = [0.1; 4800];
    let mut l = raw;
    let mut r = raw;
    processor.process_crossfaded(&mut l, &mut r, &raw, &raw, &settings, 48000);
    let processed = l[4799];
    assert!(processed > 0.19);
    l = raw;
    r = raw;
    processor.process_crossfaded(&mut l, &mut r, &raw, &raw, &Settings::OFF, 48000);
    assert!((l[0] - processed).abs() < 0.002);
    assert_eq!(l[4799], raw[4799]);
    assert_eq!(l, r);
}
