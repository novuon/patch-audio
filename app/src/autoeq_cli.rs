//! `omarchy-patch eq autoeq`: a headphone correction from AutoEq, refitted
//! onto the EQ's eight bands and saved as a preset.
//!
//! The network is touched only here, when asked: the index is fetched on
//! the first search and kept for a week, one profile per import. Both go
//! through curl with a size cap and a timeout.

use anyhow::{Context, bail};
use patch_dsp::autoeq::{self, Entry};
use patch_dsp::filter::FilterType;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const INDEX_CAP: u64 = 8 << 20;
const PROFILE_CAP: u64 = 64 << 10;
const INDEX_FRESH: Duration = Duration::from_secs(7 * 24 * 3600);

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback)
    })
}

/// HTTPS only, capped, timed out; the body or curl's own error.
fn fetch(url: &str, cap: u64) -> anyhow::Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--max-time",
            "20",
            "--max-filesize",
        ])
        .arg(cap.to_string())
        .arg(url)
        .output()
        .context("curl is needed to reach AutoEq")?;
    if !out.status.success() {
        bail!(
            "couldn't reach AutoEq: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The index from the cache while it is a week old or newer; else fetched
/// (an old copy still serves when the network doesn't).
fn index() -> anyhow::Result<Vec<Entry>> {
    let path = xdg("XDG_CACHE_HOME", ".cache").join("omarchy-patch/autoeq-index.md");
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    let cached = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|t| autoeq::parse_index(&t))
    };
    if age.is_some_and(|a| a < INDEX_FRESH)
        && let Some(e) = cached().filter(|e| !e.is_empty())
    {
        return Ok(e);
    }
    match fetch(&format!("{}INDEX.md", autoeq::RESULTS_URL), INDEX_CAP) {
        Ok(text) => {
            let entries = autoeq::parse_index(&text);
            if entries.is_empty() {
                bail!("the AutoEq index changed format");
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
                let tmp = path.with_extension("md.tmp");
                if std::fs::write(&tmp, &text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            Ok(entries)
        }
        Err(e) => cached().filter(|e| !e.is_empty()).ok_or(e),
    }
}

/// `eq autoeq search <headphone>` · `eq autoeq <headphone> [--target T] [--no-load]`
pub fn run(rest: &[&str]) -> anyhow::Result<()> {
    let (search, rest) = match rest {
        ["search", r @ ..] => (true, r),
        r => (false, r),
    };
    let (mut target, mut load, mut words) = (None, true, Vec::new());
    let mut it = rest.iter();
    while let Some(w) = it.next() {
        match *w {
            "--target" => target = it.next().map(|t| t.to_string()),
            "--no-load" => load = false,
            w => words.push(w),
        }
    }
    let query = words.join(" ");
    if query.trim().is_empty() {
        bail!(
            "usage: omarchy-patch eq autoeq [search] <headphone> [--target master|dj|…] [--no-load]"
        );
    }
    let entries = index()?;
    let hits = autoeq::search(&entries, &query, if search { 10 } else { 1 });
    let Some(pick) = hits.first() else {
        bail!("no AutoEq profile matches '{query}'");
    };
    if search {
        for e in &hits {
            println!("{:<44} {}", e.name, e.source);
        }
        return Ok(());
    }

    let profile =
        autoeq::parse_profile(&fetch(&pick.url(), PROFILE_CAP)?).map_err(anyhow::Error::msg)?;
    let fit = autoeq::fit(&profile, patch_core::eq::BANDS);
    let preset = preset(pick, &profile, &fit);
    preset.validate().map_err(anyhow::Error::msg)?;

    let dir = xdg("XDG_CONFIG_HOME", ".config").join("omarchy-patch/eq-presets");
    std::fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
    let path = dir.join(format!("{}.json", preset.id.replace('.', "-")));
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&preset)?)?;
    std::fs::rename(&tmp, &path)?;

    println!("{} — measured by {}", pick.name, pick.source);
    println!(
        "{} filters on {} bands: {:.2} dB RMS from AutoEq's curve (at most {:.1} dB, 20 Hz – 10 kHz)",
        profile.filters.len(),
        fit.bands.len(),
        fit.rms_db,
        fit.max_db
    );
    for b in &fit.bands {
        println!(
            "  {:<10} {:>6.0} Hz {:+5.1} dB Q{:.2}",
            kind(b.kind),
            b.freq,
            b.gain_db,
            b.q
        );
    }
    println!(
        "input {:+.1} dB: the highest boost stays at 0 dB (AutoEq's preamp {:+.1} dB)",
        preset.gain_db, profile.preamp_db
    );
    println!("saved as preset '{}' ({})", preset.name, path.display());
    if load {
        let cmd = patch_core::command::Command::EqPreset {
            target: target.clone(),
            name: preset.name.clone(),
        };
        let json: serde_json::Value = serde_json::from_str(&cmd.to_json())?;
        patch_engine::client::call(
            &serde_json::json!({"op": "command", "command": json, "origin": "cli"}),
        )
        .map_err(anyhow::Error::msg)?;
        println!("loaded on the {} EQ", target.as_deref().unwrap_or("master"));
    }
    Ok(())
}

fn kind(k: FilterType) -> &'static str {
    match k {
        FilterType::LowShelf => "low_shelf",
        FilterType::HighShelf => "high_shelf",
        _ => "bell",
    }
}

/// The fit as a `patch-eq` preset; where it came from in its provenance.
fn preset(e: &Entry, profile: &autoeq::Profile, fit: &autoeq::Fit) -> patch_core::eq::Preset {
    let slug: String = e
        .name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    // f32 as the EQ stores it, rounded to what anyone would type.
    let r = |v: f64, step: f64| ((v / step).round() * step) as f32;
    patch_core::eq::Preset {
        format: "patch-eq".into(),
        version: 1,
        id: format!("autoeq.{slug}"),
        name: format!("{} · AutoEq", e.name),
        description: format!("Headphone correction measured by {}", e.source),
        mode: "parametric".into(),
        channel_mode: "linked".into(),
        // Rounded down: the curve's peak never goes over 0 dB.
        gain_db: ((fit.preamp_db * 10.0).floor() / 10.0) as f32,
        bands: fit
            .bands
            .iter()
            .map(|b| patch_core::eq::PresetBand {
                id: String::new(),
                enabled: true,
                kind: kind(b.kind).into(),
                frequency_hz: r(b.freq, 1.0),
                gain_db: r(b.gain_db, 0.1),
                q: r(b.q, 0.01),
                channel: "all".into(),
                extra: Default::default(),
            })
            .collect(),
        extra: [(
            "provenance".to_string(),
            serde_json::json!({
                "kind": "autoeq",
                "tool": "Patch",
                "source": e.source,
                "path": e.path,
                "filters": profile.filters.len(),
                "autoeq_preamp_db": profile.preamp_db,
                "fit_rms_db": (fit.rms_db * 100.0).round() / 100.0,
            }),
        )]
        .into_iter()
        .collect(),
    }
}
