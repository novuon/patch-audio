//! Isolated two-deck RT audit; no PipeWire, device output or original-file writes.
use patch_engine::{
    Clock, Params, Rt,
    deck::{pcm::Pcm, transport::Cmd},
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
thread_local! {static CHECK:Cell<bool>=const {Cell::new(false)};}
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static TRACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
struct Audit;
// SAFETY: delegates allocation/deallocation unchanged to System; only counts this thread.
unsafe impl GlobalAlloc for Audit {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if CHECK.try_with(Cell::get).unwrap_or(false) {
            let n = ALLOCS.fetch_add(1, Ordering::Relaxed);
            if n < 3 && TRACE.load(Ordering::Relaxed) {
                CHECK.set(false);
                eprintln!(
                    "allocation {n}: {}",
                    std::backtrace::Backtrace::force_capture()
                );
                CHECK.set(true);
            }
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if CHECK.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Audit = Audit;
fn measure(source: &str, stems: bool, prepare: bool) -> Result<serde_json::Value, String> {
    let params = Arc::new(Params::new(&["deck-a", "deck-b"]));
    for (d, sh) in params.decks.iter().enumerate() {
        sh.spawn(if d == 0 {
            "stem-audit-a"
        } else {
            "stem-audit-b"
        });
        sh.send(Cmd::Load(Pcm::load_full(
            std::path::Path::new(source),
            48000,
            1.0,
        )?));
        if stems {
            let key = patch_engine::stem_service::context(source)
                .set_id
                .ok_or("Prepare source first")?;
            sh.send(Cmd::AttachStems(
                patch_engine::stem_service::decode(source, &key, 48000)?.into(),
            ));
        }
        sh.send(Cmd::Keylock(true));
        sh.send(Cmd::Tempo(0.06));
        sh.send(Cmd::Play);
    }
    std::thread::sleep(Duration::from_millis(300));
    if stems {
        for sh in &params.decks {
            sh.stems
                .set(0, patch_core::stems::StemValue::Gain(1.0), false)?;
        }
    }
    let job = if prepare {
        let (mut sep, backend) = patch_stems::separator::configured()?;
        sep.0
            .profile
            .push_str(&format!("-audit-{}", patch_engine::now_ns()));
        Some(patch_stems::jobs::request(
            patch_stems::cache::Cache::default_cache()?,
            source.into(),
            source.into(),
            Box::new(sep),
            backend,
            false,
        )?)
    } else {
        None
    };
    let mut rt = Rt::new(params.clone());
    let mut l = [0.0; 256];
    let mut r = [0.0; 256];
    let inputs = [None; 4];
    let mut costs = Vec::with_capacity(1000);
    let start = Instant::now();
    let initial = ALLOCS.load(Ordering::Relaxed);
    let mut finite = true;
    for i in 0..1000 {
        if stems && i == 50 {
            for sh in &params.decks {
                sh.stems.ramp(2, 12800, 192000, 0.0)?;
            }
        }
        if stems && i == 200 {
            params.decks[0]
                .stems
                .set(2, patch_core::stems::StemValue::Gain(0.6), true)?;
        }
        let clock = Clock {
            position: i * 256,
            nsec: 0,
            rate: 48000,
            frames: 256,
            delay: 0,
            ..Default::default()
        };
        let at = Instant::now();
        CHECK.set(true);
        rt.run(&clock, &inputs, [&mut l, &mut r], None);
        CHECK.set(false);
        costs.push(at.elapsed().as_nanos() as u64);
        finite &= l.iter().chain(r.iter()).all(|s| s.is_finite());
        let until = start + Duration::from_secs_f64((i + 1) as f64 * 256.0 / 48000.0);
        if let Some(wait) = until.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    costs.sort_unstable();
    let allocations = ALLOCS.load(Ordering::Relaxed) - initial;
    let underruns: Vec<_> = params
        .decks
        .iter()
        .map(|d| d.underruns.load(Ordering::Relaxed))
        .collect();
    if let Some(job) = job {
        let cache = patch_stems::cache::Cache::default_cache()?;
        let _ = patch_stems::jobs::cancel(&cache, job["id"].as_str().unwrap());
    }
    if allocations != 0 || !finite {
        return Err(format!(
            "RT audit failed: {allocations} allocations, finite={finite}"
        ));
    }
    Ok(
        serde_json::json!({"stems":stems,"preparation":prepare,"frames":256,"rate":48000,"cycles":1000,"rt_allocations":allocations,"finite":finite,"median_us":costs[500] as f64/1000.0,"p99_us":costs[990] as f64/1000.0,"worst_us":costs[999] as f64/1000.0,"deadline_us":256.0/48000.0*1e6,"underruns":underruns}),
    )
}
fn main() -> Result<(), String> {
    TRACE.store(
        std::env::var_os("PATCH_AUDIT_TRACE").is_some(),
        Ordering::Relaxed,
    );
    let source = std::env::args()
        .nth(1)
        .ok_or("Pass a prepared audio source")?;
    for (stems, prep) in [(false, false), (true, false), (true, true)] {
        println!("{}", measure(&source, stems, prep)?);
    }
    Ok(())
}
