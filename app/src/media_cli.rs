//! Recording, broadcasting, shots and the pattern, material and visual-intent commands.

use super::*;

pub fn library_collection_add(name: &&str, files: &[&str]) -> anyhow::Result<()> {
    let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
    let id = match db.collection_by_name(name) {
        Some(id) => id,
        None => db.create_collection(name).map_err(anyhow::Error::msg)?,
    };
    let mut ids = Vec::new();
    for f in files.iter() {
        let path = std::fs::canonicalize(f).map_or_else(|_| f.to_string(), |p| p.to_string_lossy().into_owned());
        match db.track_by_path(&path) {
            Some(t) => ids.push(t),
            None => println!("not in the library: {f}"),
        }
    }
    let added = db.add_many_to_collection(id, &ids).map_err(anyhow::Error::msg)?;
    println!("collection '{name}': {added} tracks added");
    Ok(())
}

pub fn library_scan(dir: &[&str]) -> anyhow::Result<()> {
    let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
    let root = match dir.first() {
        Some(d) => std::path::PathBuf::from(d),
        None => patch_surfaces::music_dir(),
    };
    db.scan(&root, patch_library::probe_audio_facts)
        .map_err(anyhow::Error::msg)?;
    let n = db.tracks().map(|t| t.len()).unwrap_or(0);
    println!("scanned {} · {n} tracks in the library", root.display());
    Ok(())
}

pub fn shots(rest: &[&str]) -> anyhow::Result<()> {
    let out = rest
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("screenshots"));
    patch_host::shot::install()?;
    let files = patch_host::shot::unit_shots::<AudioApp>(&out, 1.5, rest.get(1).copied())?;
    println!(
        "patch  {} images → {}",
        files.len(),
        out.join(APP.slug()).display()
    );
    Ok(())
}

pub fn record_daemon(source: &&str, fmt: &&str, rate: &&str, path: &&str, label: &&str) -> anyhow::Result<()> {
    let take = patch_record::daemon::Take {
        source: patch_record::daemon::Source::parse(source)
            .ok_or_else(|| anyhow::anyhow!("bad source"))?,
        label: label.to_string(),
        format: if *fmt == "f32" {
            patch_record::wav::SampleFormat::F32
        } else {
            patch_record::wav::SampleFormat::S24
        },
        rate: rate.parse()?,
        path: PathBuf::from(path),
    };
    patch_record::daemon::run(take)?;
    Ok(())
}

pub fn record(verb: &&str) -> anyhow::Result<()> {
    if !patch_record::daemon::pause(*verb == "pause") {
        anyhow::bail!("not recording");
    }
    println!(
        "{}",
        if *verb == "pause" {
            "paused"
        } else {
            "recording"
        }
    );
    Ok(())
}

pub fn material(rest: &[&str]) -> anyhow::Result<()> {
    let action = rest.first().copied().unwrap_or("stats");
    let mut req = serde_json::json!({"op": "material", "action": action});
    let arg = |i: usize, usage: &str| -> anyhow::Result<String> {
        rest.get(i)
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("usage: {usage}"))
    };
    match action {
        "state" | "release" => {
            req["id"] = serde_json::Value::String(arg(1, &format!("material {action} <id>"))?);
            if action == "release"
                && let Some(c) = rest.get(2)
            {
                req["consumer"] = serde_json::Value::String(c.to_string());
            }
        }
        "request" | "prepare" => {
            let path = arg(1, &format!("material {action} <recipe.json>"))?;
            let text = std::fs::read_to_string(&path)?;
            req["recipe"] = serde_json::from_str(&text)?;
        }
        "stats" | "cleanup" | "recipes" => {}
        other => anyhow::bail!("material has no action '{other}'"),
    }
    match patch_engine::client::call(&req) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(e) => println!("{e}"),
    }
    Ok(())
}

pub fn visual_intent(rest: &[&str]) -> anyhow::Result<()> {
    let action = rest.first().copied().unwrap_or("get");
    let mut req = serde_json::json!({"op": "visual_intent", "action": action, "origin": "cli"});
    match action {
        "get" | "clear" => {}
        "set" => {
            let mut intent = serde_json::Map::new();
            for kv in &rest[1..] {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("usage: visual-intent set field=value …"))?;
                // Numbers are numbers; words (section, colour, focus) strings.
                let v = v
                    .parse::<f64>()
                    .map(|n| serde_json::json!(n))
                    .unwrap_or_else(|_| serde_json::Value::String(v.into()));
                if k == "hold_s" {
                    req["hold_s"] = v;
                } else {
                    intent.insert(k.into(), v);
                }
            }
            req["intent"] = serde_json::Value::Object(intent);
        }
        other => anyhow::bail!("visual-intent has no action '{other}'"),
    }
    match patch_engine::client::call(&req) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(e) => println!("{e}"),
    }
    Ok(())
}

/// The Tidal Unit: `tidal eval <code>` (or `-`: the code on stdin, so any
/// editor can send a block, as Tidal's editor plugins do), `tidal run
/// <file>`, hush, play, stop, status, tempo, follow, events; `tidal
/// samples` lists the sample folders, `tidal sample add <name> <file>
/// [from] [to]` cuts a new sample from any audio file.
pub fn tidal(rest: &[&str]) -> anyhow::Result<()> {
    let action = rest.first().copied().unwrap_or("status");
    let mut req = serde_json::json!({"op": "tidal", "action": action});
    match action {
        "eval" => {
            let code = match rest.get(1..).filter(|r| !r.is_empty() && r != &["-"]) {
                Some(words) => words.join(" "),
                None => std::io::read_to_string(std::io::stdin())?,
            };
            req["code"] = serde_json::Value::String(code);
        }
        "run" => {
            let path = rest
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: tidal run <file.tidal>"))?;
            req["action"] = serde_json::Value::String("eval".into());
            req["code"] = serde_json::Value::String(std::fs::read_to_string(path)?);
        }
        "hush" => {
            req["action"] = serde_json::Value::String("eval".into());
            req["code"] = serde_json::Value::String("hush".into());
        }
        "events" => {
            if let Some(from) = rest.get(1).and_then(|s| s.parse::<f64>().ok()) {
                req["from"] = serde_json::json!(from);
            }
            if let Some(to) = rest.get(2).and_then(|s| s.parse::<f64>().ok()) {
                req["to"] = serde_json::json!(to);
            }
        }
        "tempo" => {
            let bpm: f64 = rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("usage: tidal tempo <bpm>"))?;
            req["bpm"] = serde_json::json!(bpm);
        }
        "follow" => {
            let deck = rest.get(1).copied().unwrap_or("auto");
            if !matches!(deck, "auto" | "a" | "b" | "c" | "d") {
                anyhow::bail!("usage: tidal follow <auto|a|b|c|d>");
            }
            req["action"] = serde_json::Value::String("tempo".into());
            if deck != "auto" {
                req["follow"] = serde_json::Value::String(deck.into());
            }
        }
        "samples" => {
            let v = patch_engine::client::call(&req).map_err(|e| anyhow::anyhow!(e))?;
            let folders = v["folders"].as_array().cloned().unwrap_or_default();
            for r in v["roots"].as_array().into_iter().flatten() {
                println!("reads {}", r.as_str().unwrap_or(""));
            }
            if folders.is_empty() {
                println!("no sample folders yet: put audio files in {}/<name>/", v["own"].as_str().unwrap_or(""));
            }
            for f in &folders {
                println!("{:<16} {}", f[0].as_str().unwrap_or(""), f[1]);
            }
            let b = &v["bank"];
            println!("loaded {} ({} MB), loading {}, failed {}", b["loaded"], b["bytes"].as_u64().unwrap_or(0) >> 20, b["loading"], b["failed"]);
            if let Some(e) = b["last_error"].as_str() {
                println!("last error: {e}");
            }
            return Ok(());
        }
        "sample" => {
            let usage = "usage: tidal sample add <name> <audio file> [from seconds] [to seconds]";
            let (Some(&"add"), Some(name), Some(file)) = (rest.get(1), rest.get(2), rest.get(3)) else {
                anyhow::bail!(usage);
            };
            let secs = |i: usize| -> anyhow::Result<Option<f64>> {
                rest.get(i).map(|s| s.parse::<f64>().map_err(|_| anyhow::anyhow!(usage))).transpose()
            };
            let out = patch_engine::sample_bank::save(name, std::path::Path::new(file), secs(4)?, secs(5)?)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{}", out.display());
            return Ok(());
        }
        "status" | "play" | "stop" => {}
        other => anyhow::bail!("tidal has no action '{other}' (eval, run, hush, play, stop, status, tempo, follow, events, samples, sample add)"),
    }
    let v = patch_engine::client::call(&req).map_err(|e| anyhow::anyhow!(e))?;
    if req["action"] == "eval" {
        // As a REPL: what changed, or where the errors are.
        if v["ok"] == false {
            for e in v["errors"].as_array().into_iter().flatten() {
                eprintln!("{}:{}: {}", e["line"], e["col"], e["message"].as_str().unwrap_or(""));
            }
            std::process::exit(1);
        }
        let touched: Vec<&str> = v["touched"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
        println!("{}", if touched.is_empty() { "ok".to_string() } else { touched.join(" ") });
        for w in v["warnings"].as_array().into_iter().flatten() {
            eprintln!("warning: {}", w.as_str().unwrap_or(""));
        }
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

pub fn pattern(rest: &[&str]) -> anyhow::Result<()> {
    let action = rest.first().copied().unwrap_or("status");
    let mut req = serde_json::json!({"op": "pattern", "action": action});
    match action {
        "load" => {
            let path = rest
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: pattern load <file> [patch|strudel]"))?;
            let code = std::fs::read_to_string(path)?;
            let language = rest.get(2).copied().unwrap_or("patch");
            if !matches!(language, "patch" | "strudel") {
                anyhow::bail!("language is patch or strudel, not '{language}'");
            }
            req["language"] = serde_json::Value::String(language.into());
            req["code"] = serde_json::Value::String(code);
        }
        "events" => {
            if let Some(from) = rest.get(1).and_then(|s| s.parse::<f64>().ok()) {
                req["from"] = serde_json::json!(from);
            }
            if let Some(to) = rest.get(2).and_then(|s| s.parse::<f64>().ok()) {
                req["to"] = serde_json::json!(to);
            }
        }
        "timeline" => {
            // What the Visualiser polls; `visual` keeps only the
            // events a visual layer reads.
            if rest.get(1).is_some_and(|a| *a == "visual") {
                req["visual"] = serde_json::json!(true);
            }
        }
        "tempo" => {
            let bpm: f64 = rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("usage: pattern tempo <bpm>"))?;
            req["bpm"] = serde_json::json!(bpm);
        }
        "follow" => {
            // Follow one deck's grid, or `auto`: the lead deck's
            // (whichever the crossfader favours among those playing).
            let deck = rest.get(1).copied().unwrap_or("auto");
            if !matches!(deck, "auto" | "a" | "b" | "c" | "d") {
                anyhow::bail!("usage: pattern follow <auto|a|b|c|d>");
            }
            req["action"] = serde_json::Value::String("tempo".into());
            if deck != "auto" {
                req["follow"] = serde_json::Value::String(deck.into());
            }
        }
        "engine" => {
            let name = rest
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: pattern engine <auto|strudel|reference>"))?;
            req["name"] = serde_json::Value::String(name.to_string());
        }
        "status" | "play" | "stop" | "describe" => {}
        other => anyhow::bail!("pattern has no action '{other}'"),
    }
    match patch_engine::client::call(&req) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v)?),
        Err(e) => println!("{e}"),
    }
    Ok(())
}
