//! The `dj` developer and package commands: stems, levels, tempo, style, DJ packages.

pub fn prepare_stems(folder: &&str, rest: &[&str]) -> anyhow::Result<()> {
    let n: usize = rest
        .first()
        .and_then(|x| x.parse().ok())
        .unwrap_or(usize::MAX);
    let cache = patch_stems::cache::Cache::default_cache().map_err(anyhow::Error::msg)?;
    let mut files: Vec<_> = std::fs::read_dir(folder)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| patch_analysis::cached(p).is_some())
        .collect();
    files.sort();
    for path in files.into_iter().take(n) {
        let src = path.to_string_lossy().into_owned();
        if patch_engine::stem_service::context(&src).availability
            == patch_core::stems::Availability::Ready
        {
            continue;
        }
        let (sep, backend) =
            patch_stems::separator::configured().map_err(anyhow::Error::msg)?;
        let t = std::time::Instant::now();
        let job = patch_stems::jobs::request(
            cache.clone(),
            path.clone(),
            src.clone(),
            Box::new(sep),
            backend,
            false,
        )
        .map_err(anyhow::Error::msg)?;
        let id = job["id"].as_str().unwrap_or_default().to_string();
        let state = loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let v: serde_json::Value =
                std::fs::read(cache.metadata.join(format!("job-{id}.json")))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default();
            if let Some(s @ ("ready" | "failed" | "cancelled" | "interrupted")) =
                v["state"].as_str()
            {
                break s.to_string();
            }
        };
        println!(
            "{state:>6} {:5.1} s  {}",
            t.elapsed().as_secs_f64(),
            path.display()
        );
    }
    Ok(())
}

/// How loud the decks play: the modes, the one in use, or a new one.
pub fn levels(mode: Option<&str>) -> anyhow::Result<()> {
    let mut req = serde_json::json!({"op": "levels"});
    if let Some(m) = mode {
        req["mode"] = m.into();
    }
    let v = patch_engine::client::call(&req).map_err(anyhow::Error::msg)?;
    for m in v["modes"].as_array().into_iter().flatten() {
        let id = m["id"].as_str().unwrap_or("");
        let mark = if v["mode"] == id { "●" } else { " " };
        println!("{mark} {id:<10} {}", m["about"].as_str().unwrap_or(""));
    }
    Ok(())
}

/// Where the AI DJ takes the set's tempo: what is set, or a new mode
/// (`hold 126`, `faster`, `slower`, `original`, `follow`).
pub fn tempo(rest: &[&str]) -> anyhow::Result<()> {
    let mut req = serde_json::json!({"op": "tempo_plan"});
    match rest {
        [] => {}
        ["hold", bpm] => {
            req["mode"] = "hold".into();
            req["bpm"] = bpm.parse::<f64>()?.into();
        }
        [mode] if patch_core::tempo_plan::TempoMode::parse(mode).is_some() => {
            req["mode"] = (*mode).into()
        }
        _ => anyhow::bail!("usage: dj tempo [follow | hold <bpm> | faster | slower | original]"),
    }
    let v = patch_engine::client::call(&req).map_err(anyhow::Error::msg)?;
    println!("tempo: {}", v["summary"].as_str().unwrap_or(""));
    if let Some(now) = v["playing_bpm"].as_f64() {
        match v["target_bpm"].as_f64() {
            Some(to) => println!("playing at {now:.1} BPM, gliding to {to:.1}"),
            None => println!("playing at {now:.1} BPM"),
        }
    }
    Ok(())
}

/// The AI DJ's transition style: what it uses, or a new style or switch.
pub fn style(rest: &[&str]) -> anyhow::Result<()> {
    let mut req = serde_json::json!({"op": "mix_style"});
    match rest {
        [] => {}
        [id @ ("plain" | "balanced" | "adventurous")] => req["style"] = (*id).into(),
        [switch, value @ ("on" | "off")] => {
            let key = switch.replace('-', "_");
            let known = patch_core::mix_style::CONTROLS.iter().any(|(id, ..)| *id == key);
            anyhow::ensure!(
                known,
                "unknown switch {switch:?}: effect-exits, accents, loops, stem-led, bass-replace or prefer-stems"
            );
            req[key] = (*value == "on").into();
        }
        _ => anyhow::bail!(
            "usage: dj style [plain|balanced|adventurous] | dj style <switch> on|off"
        ),
    }
    let v = patch_engine::client::call(&req).map_err(anyhow::Error::msg)?;
    print_style(&v);
    // How many of the set's tracks can lead with their stems.
    if let Ok(c) = patch_engine::client::call(&serde_json::json!({"op": "stem_coverage"})) {
        println!(
            "stems: {} of {} tracks in the set are prepared",
            c["ready"], c["tracks"]
        );
    }
    Ok(())
}

fn print_style(v: &serde_json::Value) {
    println!("{}", v["summary"].as_str().unwrap_or(""));
    for s in v["styles"].as_array().into_iter().flatten() {
        let mark = if v["style"] == s["id"] { "●" } else { " " };
        println!("{mark} {:<12} {}", s["id"].as_str().unwrap_or(""), s["about"].as_str().unwrap_or(""));
    }
    for c in v["controls"].as_array().into_iter().flatten() {
        let id = c["id"].as_str().unwrap_or("");
        let on = v[id].as_bool().unwrap_or(false);
        println!(
            "  {} {:<16} {}",
            if on { "on " } else { "off" },
            c["label"].as_str().unwrap_or(""),
            c["about"].as_str().unwrap_or("")
        );
    }
}

/// Which tracks get stems prepared by themselves: what is set (and what
/// Auto means here), or a new level.
pub fn stem_prep(rest: &[&str]) -> anyhow::Result<()> {
    let mut req = serde_json::json!({"op": "mix_style"});
    match rest {
        [] => {}
        [level] => {
            let p = patch_core::mix_style::StemPrep::parse(level).ok_or_else(|| {
                anyhow::anyhow!("dj stems is auto, off, when-needed, ahead or everything")
            })?;
            req["stem_prep"] = p.id().into();
        }
        _ => anyhow::bail!("usage: dj stems [auto|off|when-needed|ahead|everything]"),
    }
    let v = patch_engine::client::call(&req).map_err(anyhow::Error::msg)?;
    for p in v["stem_preps"].as_array().into_iter().flatten() {
        let mark = if v["stem_prep"] == p["id"] { "●" } else { " " };
        println!("{mark} {:<12} {}", p["id"].as_str().unwrap_or(""), p["about"].as_str().unwrap_or(""));
    }
    if v["stem_prep"] == "auto" {
        let speed = v["stem_seconds"].as_f64().map(|s| format!(", about {s:.0} s a track")).unwrap_or_default();
        println!("auto here: {}{speed}", v["stem_prep_now_label"].as_str().unwrap_or("?"));
    }
    Ok(())
}

/// How much of each track plays, in percent: what is set, or a new value.
pub fn play_length(rest: &[&str]) -> anyhow::Result<()> {
    let mut req = serde_json::json!({"op": "mix_style"});
    if let [p] = rest {
        req["play_percent"] = p
            .trim_end_matches('%')
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("play-length is a percentage, 20 to 100"))?
            .into();
        req["source"] = "omarchy-patch dj play-length".into();
    } else if !rest.is_empty() {
        anyhow::bail!("usage: dj play-length [20..100]");
    }
    let v = patch_engine::client::call(&req).map_err(anyhow::Error::msg)?;
    let c = patch_engine::client::call(&serde_json::json!({"op": "stem_coverage"}))
        .map_err(anyhow::Error::msg)?;
    println!(
        "each track plays about {}% of its length, transitions included: {:.1} of {:.1} minutes on average",
        v["play_percent"],
        c["minutes_per_track"].as_f64().unwrap_or(0.0),
        c["avg_track_minutes"].as_f64().unwrap_or(0.0),
    );
    Ok(())
}

/// The offline commands (rehearsals, training) plan by the process default,
/// Plain. `--style <id>` and `--play <percent>` anywhere in their arguments
/// choose another, for an A/B render; they are removed from the arguments.
pub fn offline_style(args: Vec<String>) -> Vec<String> {
    let offline = matches!(
        args.as_slice(),
        [dj, sub, ..] if dj == "dj"
            && (sub.starts_with("rehearse") || matches!(sub.as_str(), "train" | "stem-need"))
    );
    if !offline {
        return args;
    }
    let mut style = patch_core::mix_style::MixStyle::default();
    let mut rest = vec![];
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--style" => {
                if let Some(s) = it.next().and_then(|s| patch_core::mix_style::Style::parse(&s)) {
                    style.choose(s);
                }
            }
            "--play" => {
                if let Some(p) = it.next().and_then(|p| p.parse::<u8>().ok()) {
                    style.play_percent = p.clamp(patch_core::mix_style::MIN_PLAY_PERCENT, 100);
                }
            }
            _ => rest.push(a),
        }
    }
    patch_ai::mix::set_mix_style(style);
    rest
}

pub fn package_create(a: &[&str], path: &&str, name: &&str) -> anyhow::Result<()> {
    let mode = match a[5] {
        "portable" => patch_library::PackageMode::Portable,
        "set" => patch_library::PackageMode::Set,
        _ => patch_library::PackageMode::Archive,
    };
    let p = patch_library::Package::create(std::path::Path::new(path), name, mode)
        .map_err(anyhow::Error::msg)?;
    println!(
        "created {} ({})",
        p.manifest().name,
        p.manifest().package_id
    );
    Ok(())
}

pub fn package_validate(path: &&str) -> anyhow::Result<()> {
    let p = patch_library::Package::open(std::path::Path::new(path))
        .map_err(anyhow::Error::msg)?;
    let r = p.validate();
    println!(
        "{}: {} tracks, {} missing audio reference(s)",
        if r.is_valid() { "valid" } else { "invalid" },
        r.tracks,
        r.missing_audio
    );
    for e in &r.errors {
        eprintln!("error: {e}");
    }
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
    if !r.is_valid() {
        std::process::exit(1);
    }
    Ok(())
}

pub fn package_inspect(path: &&str) -> anyhow::Result<()> {
    let p = patch_library::Package::open(std::path::Path::new(path))
        .map_err(anyhow::Error::msg)?;
    let tracks = p.library.tracks().map_err(anyhow::Error::msg)?;
    println!(
        "{} · {} · format {} · schema {} · {} tracks",
        p.manifest().name,
        format!("{:?}", p.manifest().mode).to_lowercase(),
        p.manifest().format_version,
        p.manifest().schema_version,
        tracks.len()
    );
    Ok(())
}

pub fn package_add(a: &[&str], package: &&str, audio: &&str) -> anyhow::Result<()> {
    let embed = a.get(5) == Some(&"--embed");
    let p = patch_library::Package::open(std::path::Path::new(package))
        .map_err(anyhow::Error::msg)?;
    let (id, change) = p
        .add_audio(
            std::path::Path::new(audio),
            patch_library::probe_audio_facts,
            embed,
        )
        .map_err(anyhow::Error::msg)?;
    println!("{change:?}: track {id}");
    Ok(())
}
