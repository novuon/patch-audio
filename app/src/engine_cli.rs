//! Engine and audio commands: status, EQ, latency, optimisation, upgrades, stopping, analysis, devices.

pub fn eq() -> anyhow::Result<()> {
    let call =
        |v: serde_json::Value| patch_engine::client::call(&v).map_err(anyhow::Error::msg);
    let v = call(serde_json::json!({"op": "master_eq", "all": true}))?;
    let mut rows: Vec<(String, String)> = patch_core::eq::TARGETS
        .iter()
        .map(|(t, l)| (t.to_string(), l.to_string()))
        .collect();
    rows.extend(v.as_object().into_iter().flatten().filter_map(|(k, _)| {
        k.strip_prefix("app:")
            .map(|a| (k.clone(), format!("App {a}")))
    }));
    for (t, label) in &rows {
        let e = &v[t.as_str()];
        let bands: Vec<String> = e["bands"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, b)| {
                b["on"].as_bool() == Some(true)
                    && (b["gain_db"].as_f64().unwrap_or(0.0).abs() >= 0.01
                        || !matches!(
                            b["kind"].as_str(),
                            Some("bell" | "lowshelf" | "highshelf")
                        ))
            })
            .map(|(i, b)| {
                format!(
                    "{} {} {:.0} Hz {:+.1} dB Q{:.2}",
                    i + 1,
                    b["kind"].as_str().unwrap_or(""),
                    b["freq"].as_f64().unwrap_or(0.0),
                    b["gain_db"].as_f64().unwrap_or(0.0),
                    b["q"].as_f64().unwrap_or(0.0)
                )
            })
            .collect();
        let preset = e["preset"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(|p| {
                format!(
                    " · {p}{}",
                    if e["edited"].as_bool() == Some(true) {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .unwrap_or_default();
        let gain = e["gain_db"].as_f64().unwrap_or(0.0);
        let state = if e["bypass"].as_bool() == Some(true) {
            "bypassed".to_string()
        } else if bands.is_empty() && gain.abs() < 0.01 {
            "flat".to_string()
        } else if gain.abs() >= 0.01 {
            format!("input {gain:+.1} dB · {}", bands.join(" · "))
        } else {
            bands.join(" · ")
        };
        println!("{label:<12} {state}{preset}");
    }
    Ok(())
}

pub fn eq_presets() -> anyhow::Result<()> {
    let call =
        |v: serde_json::Value| patch_engine::client::call(&v).map_err(anyhow::Error::msg);
    let v = call(serde_json::json!({"op": "eq_presets"}))?;
    for p in v.as_array().into_iter().flatten() {
        println!(
            "{:<18} {:<8} {}",
            p["name"].as_str().unwrap_or(""),
            if p["builtin"].as_bool() == Some(true) {
                "built-in"
            } else {
                "saved"
            },
            p["description"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

pub fn latency(rest: &[&str]) -> anyhow::Result<()> {
    let call =
        |v: serde_json::Value| patch_engine::client::call(&v).map_err(anyhow::Error::msg);
    match rest {
        [] => println!(
            "{}",
            serde_json::to_string_pretty(&call(serde_json::json!({"op": "latency"}))?)?
        ),
        ["forget"] => {
            call(serde_json::json!({"op": "latency", "do": "forget"}))?;
            println!("forgotten: meters use the reported latency again");
        }
        ["measure", mic @ ..] => {
            let mut req = serde_json::json!({"op": "latency", "do": "measure"});
            if let Some(m) = mic.first() {
                req["mic"] = serde_json::json!(m);
            }
            let started = call(req)?;
            println!(
                "measuring with {} (≈ {:.0} s, program muted)…",
                started["mic"].as_str().unwrap_or("?"),
                started["seconds"].as_f64().unwrap_or(5.0)
            );
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let v = call(serde_json::json!({"op": "latency"}))?;
                let m = &v["measure"];
                match m["phase"].as_str() {
                    Some("done") => {
                        let r = &m["result"];
                        println!(
                            "output latency {:.1} ms (round trip {:.1} − mic {:.1}, sweeps agree within {:.2} ms, clarity {:.0})",
                            r["output_ms"].as_f64().unwrap_or(0.0),
                            r["round_trip_ms"].as_f64().unwrap_or(0.0),
                            r["input_ms"].as_f64().unwrap_or(0.0),
                            r["spread_ms"].as_f64().unwrap_or(0.0),
                            r["clarity"].as_f64().unwrap_or(0.0),
                        );
                        break;
                    }
                    Some("failed") => {
                        anyhow::bail!("{}", m["error"].as_str().unwrap_or("failed"))
                    }
                    _ => {}
                }
            }
        }
        _ => anyhow::bail!("latency [measure [mic]|forget]"),
    }
    Ok(())
}

pub fn engine_quit(rest: &[&str]) -> anyhow::Result<()> {
    let (chans, program) = patch_engine::client::live().map_err(anyhow::Error::msg)?;
    if (program || !chans.is_empty()) && !rest.contains(&"--force") {
        anyhow::bail!(
            "audio is flowing through the engine ({}): use `engine upgrade`, or --force",
            chans.join(", ")
        );
    }
    patch_engine::client::call(&serde_json::json!({"op": "quit"}))
        .map_err(anyhow::Error::msg)?;
    println!("stopped");
    Ok(())
}

pub fn optimize(rest: &[&str]) -> anyhow::Result<()> {
    let rounds = if rest.contains(&"--quick") { 5 } else { 21 };
    let cpu = patch_dsp::cpu::this();
    println!("CPU:          {} ({})", cpu.model, cpu.arch);
    println!(
        "capabilities: {}",
        cpu.features
            .iter()
            .map(|f| f.name())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let p = patch_dsp::profile::optimize(rounds, &mut |m| println!("  {m}"));
    println!(
        "benchmark:    {} build · {} · governor {}",
        p.env.build,
        if p.env.pinned {
            "pinned to one CPU"
        } else {
            "not pinned (OS refused)"
        },
        p.env.governor
    );
    println!();
    for c in &p.choices {
        let reference = c
            .measured
            .iter()
            .find(|m| m.name == "reference")
            .map(|m| m.score());
        println!("{}:", c.op.name());
        for m in &c.measured {
            let at = |f: usize| {
                m.points
                    .iter()
                    .find(|p| p.frames == f)
                    .map(|p| p.median_ns)
                    .unwrap_or(f64::NAN)
            };
            println!(
                "  {:<10} {:>6.2}x   ns/frame  64: {:>6.3}  256: {:>6.3}  1024: {:>6.3}{}",
                m.name,
                reference.map(|r| r / m.score()).unwrap_or(1.0),
                at(64),
                at(256),
                at(1024),
                if m.name == c.selected {
                    "   ← selected"
                } else {
                    ""
                }
            );
        }
        for (name, why) in &c.rejected {
            println!("  {name:<10} rejected: {why}");
        }
    }
    // Drawing: the tier this machine starts its windows at.
    let (ms, tier) = patch_surfaces::budget::benchmark();
    println!(
        "\ndrawing:      {ms:.1} ms for a deck waveform strip and a Visualiser frame (one thread) → {} tier",
        tier.name()
    );
    if !rest.contains(&"--dry-run") {
        patch_surfaces::budget::save(tier).map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "              saved: {} (windows start at that step; the drawing budget adapts from there)",
            patch_surfaces::budget::path().display()
        );
    }
    if rest.contains(&"--dry-run") {
        println!("\n(dry run: profile not saved)");
    } else {
        patch_dsp::profile::save(&p).map_err(anyhow::Error::msg)?;
        println!(
            "\nprofile saved: {} (an optimised kernel needs ≥ {:.2}x over the reference; the engine loads it at start)",
            patch_dsp::profile::path().display(),
            patch_dsp::profile::MIN_GAIN
        );
    }
    Ok(())
}

pub fn engine_stop(rest: &[&str]) -> anyhow::Result<()> {
    // The loopback that takes over carries the bus, not the decks:
    // never cut a playing deck by accident.
    let live = patch_engine::client::live_decks();
    if !live.is_empty() && rest.is_empty() {
        let names: Vec<String> = live
            .iter()
            .map(|d| format!("Deck {}", d.trim_start_matches("deck-").to_uppercase()))
            .collect();
        println!(
            "{} still playing through the engine; stopping it would cut it. Use `engine stop --force` to stop anyway.",
            names.join(" and ")
        );
        std::process::exit(1);
    }
    // The engine hands the bus back to a plain loopback first.
    match patch_engine::client::call(&serde_json::json!({"op": "quit"})) {
        Ok(_) => println!(
            "stopped (audio continues on the bus loopback; a running Patch restarts the engine)"
        ),
        Err(e) => println!("{e}"),
    }
    Ok(())
}

pub fn analyze(files: &[&str]) -> anyhow::Result<()> {
    // Where it sings, as "1:02–1:40, 3:10–3:55" (spans of 4 s or more).
    fn sung_spans(v: &patch_analysis::vocals::VocalEvidence) -> String {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for t in (0..v.level.len()).filter(|&t| v.sung_at(t as f64)) {
            match spans.last_mut() {
                Some(s) if t <= s.1 + 3 => s.1 = t,
                _ => spans.push((t, t)),
            }
        }
        let spans: Vec<String> = spans
            .into_iter()
            .filter(|s| s.1 - s.0 >= 4)
            .map(|(a, b)| {
                format!(
                    "{}–{}",
                    patch_surfaces::clock_text(a as f64),
                    patch_surfaces::clock_text(b as f64 + 1.0)
                )
            })
            .collect();
        if spans.is_empty() {
            String::new()
        } else {
            format!(" · {}", spans.join(", "))
        }
    }
    for f in files {
        match patch_analysis::analyse_file(std::path::Path::new(f)) {
            Ok(a) => {
                println!(
                    "{f}\n  {}  ·  {}  ·  {:.1} LUFS  LRA {:.1} LU  TP {:.1} dBTP  ·  {}",
                    a.bpm
                        .map(|b| format!(
                            "{b:.2} BPM (grid {:.3} s, {:.0} % likely right)",
                            a.beat_offset,
                            a.tempo_probability() * 100.0
                        ))
                        .unwrap_or_else(|| "no steady beat".into()),
                    a.key
                        .map(|k| format!(
                            "{} ({}, confidence {:.2})",
                            k.name(),
                            k.camelot(),
                            a.key_confidence
                        ))
                        .unwrap_or_else(|| "no key".into()),
                    a.lufs,
                    a.lra,
                    a.true_peak,
                    patch_surfaces::clock_text(a.duration),
                );
                println!(
                    "  {}",
                    a.vocals.as_ref().map_or_else(
                        || "vocals: not measured yet (the librarian measures them with the stem separator)".into(),
                        |v| format!(
                            "{}: a voice in {:.0} % of the track{}",
                            v.describe(),
                            v.presence * 100.0,
                            sung_spans(v)
                        )
                    )
                );
                // The track map: its parts, and the best to mix in (¹²³).
                let parts = patch_analysis::sections::map(&a);
                if !parts.is_empty() {
                    let line: Vec<String> = parts
                        .iter()
                        .map(|p| {
                            format!(
                                "{} {}{} {}",
                                patch_surfaces::clock_text(p.start_s),
                                p.kind.word(),
                                ["", "¹", "²", "³"][p.rank as usize],
                                p.bars
                            )
                        })
                        .collect();
                    println!("  parts: {}", line.join(" · "));
                    if std::env::var_os("PATCH_PARTS_DETAIL").is_some() {
                        for p in &parts {
                            println!(
                                "    {} {:<5} {:>3} bars  kick {:.2}  energy {:.2}  vocals {:.2}  mix {:.2}",
                                patch_surfaces::clock_text(p.start_s),
                                p.kind.word(),
                                p.bars,
                                p.kick,
                                p.energy,
                                p.vocals,
                                p.mix
                            );
                        }
                    }
                }
            }
            Err(e) => println!("{f}\n  {e}"),
        }
    }
    Ok(())
}
