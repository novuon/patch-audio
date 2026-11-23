//! `omarchy-patch perf [fair] [seconds] [baseline]`: the Performance Unit's
//! figures as text. `fair` compares like with like: the Patch service
//! against the native audio layer, both doing only the system's audio,
//! with no Patch window open (they are closed for the measurement and
//! opened again after), no deck playing and the AI DJ not acting.

use anyhow::bail;
use patch_core::ipc::Request;
use patch_surfaces::perf;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn engine(req: Value) -> Option<Value> {
    patch_engine::client::call(&req).ok()
}

/// Why this moment is not a fair comparison, if it isn't.
fn busy() -> Vec<String> {
    let mut why = vec![];
    for d in ["a", "b", "c", "d"] {
        if engine(json!({"op": "deck_status", "deck": d})).is_some_and(|v| v["playing"] == true) {
            why.push(format!("Deck {} is playing", d.to_uppercase()));
        }
    }
    if let Some(ai) = engine(json!({"op": "ai_state"}))
        && ai["mode"].as_str() == Some("act")
    {
        why.push("Auto Mix is mixing (ACT)".into());
    }
    why
}

/// New PulseAudio clients per second while it runs (every `pactl` run is
/// one): how much polling hits the audio server.
fn count_clients() -> (Arc<AtomicU64>, Option<std::process::Child>) {
    let n = Arc::new(AtomicU64::new(0));
    let child = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    let mut child = child;
    if let Some(out) = child.as_mut().and_then(|c| c.stdout.take()) {
        let n = n.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if l.starts_with("Event 'new' on client") {
                    n.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
    (n, child)
}

fn engine_counters() -> (u64, u64) {
    let r = engine(json!({"op": "requests"})).unwrap_or(Value::Null);
    let requests = r["ops"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| o["count"].as_u64().unwrap_or(0))
        .sum();
    let spawned = r["spawned"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, v)| v.as_u64().unwrap_or(0))
        .sum();
    (requests, spawned)
}

pub fn run(rest: &[&str], send: impl Fn(Request) -> Option<String>) -> anyhow::Result<()> {
    let fair = rest.contains(&"fair");
    let secs: u32 = rest
        .iter()
        .find_map(|a| a.parse().ok())
        .unwrap_or(if fair { 20 } else { 5 })
        .clamp(1, 600);
    // What to open again afterwards.
    let mut reopen: Option<Vec<String>> = None;
    if fair {
        let why = busy();
        if !why.is_empty() {
            bail!(
                "not a fair moment: {}. Pause it (Patch won't stop your music for a test), or run `omarchy-patch perf` for an unfair look.",
                why.join(", ")
            );
        }
        if let Some(status) = send(Request::Status).and_then(|s| patch_core::ipc::Status::parse(&s))
            && (!status.open.is_empty() || status.layout.is_some())
        {
            reopen = Some(match status.layout {
                Some(layout) => vec!["layout".into(), layout],
                None => std::iter::once("open".to_string())
                    .chain(status.open)
                    .collect(),
            });
            println!("closing Patch's windows for the test (the audio keeps playing)…");
            send(Request::Quit);
            // Do not label the run fair until the UI host has actually exited;
            // a fixed delay can leave its RSS in the service-only sample.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while send(Request::Status).is_some() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if send(Request::Status).is_some() {
                bail!("Patch's windows did not close; no fair measurement was taken");
            }
        }
        // Let the service settle without its windows.
        std::thread::sleep(Duration::from_secs(3));
    }
    let (clients, mut sub) = count_clients();
    let (req0, sp0) = engine_counters();
    let mut sampler = perf::Sampler::new(None);
    let mut snaps = Vec::new();
    sampler.sample();
    let c0 = clients.load(Ordering::Relaxed);
    for _ in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
        snaps.push(sampler.sample());
    }
    let per_s = |a: u64, b: u64| (b.saturating_sub(a)) as f64 / secs as f64;
    let (req1, sp1) = engine_counters();
    let pulse = per_s(c0, clients.load(Ordering::Relaxed));
    if let Some(c) = sub.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    if let Some(args) = &reopen
        && args.len() > 1
    {
        let exe = std::env::current_exe()?;
        let _ = Command::new("setsid")
            .arg("-f")
            .arg(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        println!(
            "Patch's windows are opening again ({})",
            args[1..].join(" ")
        );
    }
    let sum = perf::summarize(&snaps);
    let base = perf::load_baseline();
    if fair {
        println!("\nFAIR TEST: the same job for both, the system's audio to the output.");
        println!(
            "Patch: the service only (no windows, no deck playing, Auto Mix not acting). Native: pipewire, pipewire-pulse, wireplumber.\n"
        );
    } else {
        let why = busy();
        println!(
            "NOT a fair comparison: Patch's windows count on Patch's side{}. Use `omarchy-patch perf fair` to compare.\n",
            if why.is_empty() {
                String::new()
            } else {
                format!(", and {}", why.join(", "))
            }
        );
    }
    print!(
        "{}",
        perf::report(snaps.last().unwrap(), &sum, base.as_ref())
    );
    println!("\npolling");
    println!(
        "  audio-server clients   {pulse:.1} new per second, whole system (each pactl run is one; this test adds none)"
    );
    // The sampler asks the engine 4 things a second (status, both decks,
    // the AI's state): not counted.
    println!(
        "  engine requests        {:.1} per second (besides this test's own)",
        (per_s(req0, req1) - 4.0).max(0.0)
    );
    println!(
        "  engine processes       {:.2} spawned per second",
        per_s(sp0, sp1)
    );
    if rest.contains(&"baseline") {
        perf::save_baseline(&sum)?;
        println!("\nsaved as the baseline: later reports compare against it");
    }
    Ok(())
}
