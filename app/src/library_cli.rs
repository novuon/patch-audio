//! `omarchy-patch library …`: the sources in the library and where their
//! tracks stand (what the librarian knows), in plain words.

use anyhow::{Context, bail};
use patch_engine::librarian::{self, SourceStatus, Status};
use std::path::{Path, PathBuf};

fn home_short(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

pub fn eta(n: usize, per_track_s: f64) -> String {
    patch_surfaces::library_eta(n, per_track_s)
}

/// The status: the librarian's, or (not running) worked out here.
fn status() -> (Status, bool) {
    if let Some(s) = librarian::read_status() {
        return (s, true);
    }
    let Ok(db) = patch_library::open_default() else {
        return (Status::default(), false);
    };
    let sources = db
        .sources()
        .into_iter()
        .map(|s| SourceStatus {
            connected: Path::new(&s.path).is_dir(),
            removable: s.path.starts_with("/run/media/") || s.path.starts_with("/media/"),
            counts: db.counts_under(&s.path, patch_analysis::GENERATION),
            name: s.name,
            path: s.path,
        })
        .collect();
    (
        Status {
            sources,
            ..Default::default()
        },
        false,
    )
}

fn overview() {
    let (st, live) = status();
    let total: usize = st.sources.iter().map(|s| s.counts.tracks).sum();
    println!(
        "Library · {} source{} · {total} tracks",
        st.sources.len(),
        if st.sources.len() == 1 { "" } else { "s" }
    );
    match (&st.analysing, &st.scanning, live) {
        (_, Some(s), true) => println!("Looking for new music in {s}…"),
        (Some(t), _, true) => println!(
            "Analysing \"{t}\"{} · {} waiting{}",
            st.analysing_source
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default(),
            st.queue,
            match eta(st.queue, st.per_track_s) {
                e if e.is_empty() => String::new(),
                e => format!(" · {e} left"),
            }
        ),
        (None, None, true) if st.vocals_left > 0 => println!(
            "Analysed. Finding where tracks sing{}: {} left{}",
            st.voicing
                .as_deref()
                .map(|t| format!(" (\"{t}\")"))
                .unwrap_or_default(),
            st.vocals_left,
            match eta(st.vocals_left, 7.0) {
                e if e.is_empty() => String::new(),
                e => format!(" · {e}"),
            }
        ),
        (None, None, true) => println!("Everything that can be analysed is analysed."),
        (_, _, false) => println!(
            "The librarian isn't running (it starts with the Patch service: omarchy-patch engine start)."
        ),
    }
    println!();
    let w = st
        .sources
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    for s in &st.sources {
        let c = s.counts;
        let place = home_short(&s.path);
        if !s.connected {
            println!(
                "  {:<w$}  {}  not connected · {} tracks offline",
                s.name, place, c.tracks
            );
            continue;
        }
        let pct = (c.ready * 100).checked_div(c.tracks).unwrap_or(100);
        println!("  {:<w$}  {place}", s.name);
        println!(
            "  {:<w$}  {} tracks · {} ready ({pct} %) · {} check · {} waiting · {} failed",
            "", c.tracks, c.ready, c.check, c.waiting, c.failed
        );
    }
    for o in &st.offers {
        println!(
            "\n  New drive: {} ({}) · add it: omarchy-patch library add {}",
            o.label, o.path, o.path
        );
    }
    println!(
        "\nready: tempo with a steady beat grid, downbeat, key and loudness: the decks and Auto Mix have all they need"
    );
    println!(
        "check: analysed, but the beat is unsteady or no key was found · waiting: not analysed yet"
    );
    if st.sources.iter().any(|s| s.counts.failed > 0) {
        println!("failed: the file couldn't be read · why: omarchy-patch library failed");
    }
}

pub fn run(rest: &[&str]) -> anyhow::Result<()> {
    match rest {
        [] | ["status"] => overview(),
        ["add", folder, name @ ..] => {
            let path = PathBuf::from(folder)
                .canonicalize()
                .with_context(|| format!("no folder {folder}"))?;
            if !path.is_dir() {
                bail!("{} is not a folder", path.display());
            }
            let name = if name.is_empty() {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Music".into())
            } else {
                name.join(" ")
            };
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            db.add_source(&path, &name).map_err(anyhow::Error::msg)?;
            librarian::request_rescan();
            librarian::ensure(&std::env::current_exe()?);
            println!(
                "added '{name}' ({}): the librarian looks for its music now and analyses it in the background",
                path.display()
            );
            println!("see how far it is: omarchy-patch library");
        }
        ["remove", name @ ..] if !name.is_empty() => {
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            let s = db
                .remove_source(&name.join(" "))
                .map_err(anyhow::Error::msg)?;
            println!(
                "'{}' is out of the library; its files are untouched, and its analyses are kept should you add it again",
                s.name
            );
        }
        ["rescan"] => {
            librarian::request_rescan();
            println!("the librarian looks for new music in every source now");
        }
        ["failed"] => {
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            let mut n = 0;
            for (path, (state, why)) in db.readiness(patch_analysis::GENERATION) {
                if state == patch_library::Readiness::Failed {
                    println!(
                        "{}\n    {}",
                        home_short(&path),
                        patch_library::plain_error(&why.unwrap_or_default())
                    );
                    n += 1;
                }
            }
            if n == 0 {
                println!("no track failed");
            }
        }
        ["grid", target, rest @ ..] => {
            // How straight each beat runs (coarse, or precise at the full rate).
            let precise = rest.contains(&"precise");
            let root = PathBuf::from(target);
            let files: Vec<PathBuf> = if root.is_dir() {
                let mut v: Vec<PathBuf> = std::fs::read_dir(&root)?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && patch_analysis::cached(p).is_some())
                    .collect();
                v.sort();
                v
            } else {
                vec![root]
            };
            for f in files {
                let t0 = std::time::Instant::now();
                let name = f
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match patch_analysis::measure_grid(&f, precise) {
                    Some(q) => println!(
                        "{:5.1} %  {:7.3} BPM  first beat {:.3} s  phase {:3.0} %  ±{:4.1} ms  drift {:4.1} ms  kick {:3.0} %  {}{}  {:.1} s  {name}",
                        q.mixability * 100.0,
                        q.bpm,
                        q.offset,
                        q.phase_confidence * 100.0,
                        q.jitter_ms,
                        q.drift_ms,
                        q.coverage * 100.0,
                        if q.straight() { "straight" } else { "loose   " },
                        q.downbeat
                            .map(|d| format!(
                                "  downbeat {d} ({:.0} %)",
                                q.downbeat_confidence * 100.0
                            ))
                            .unwrap_or_else(|| "  no downbeat".into()),
                        t0.elapsed().as_secs_f64()
                    ),
                    None => println!("    –    no steady beat (or not analysed)  {name}"),
                }
            }
        }
        ["downbeat", file, rest @ ..] => {
            // Beat 1 by hand: shift it by whole beats (+1: one beat later),
            // or go back to the analysis' own. Decks, sync and the AI DJ use
            // it from their next read; the analysis cache is not touched.
            let path = std::fs::canonicalize(file).with_context(|| file.to_string())?;
            let p = path.to_string_lossy().into_owned();
            let db = patch_library::open_default().map_err(anyhow::Error::msg)?;
            let effective = || -> anyhow::Result<patch_analysis::Analysis> {
                let mut a = patch_analysis::cached(&path).context("not analysed yet")?;
                patch_engine::track_analysis::apply_saved_manual_grid(&p, &mut a);
                a.use_grid();
                Ok(a)
            };
            let show = |a: &patch_analysis::Analysis, what: &str| {
                println!(
                    "{what}: {:.3} BPM, first beat {:.3} s, beat 1 on grid beat {}",
                    a.bpm.unwrap_or(0.0),
                    a.beat_offset,
                    a.downbeat.map_or("?".into(), |d| d.to_string())
                );
            };
            match rest {
                [] | ["show"] => show(&effective()?, "now"),
                ["clear"] => {
                    db.clear_human_grid(&p).map_err(anyhow::Error::msg)?;
                    let mut a = patch_analysis::cached(&path).context("not analysed yet")?;
                    a.use_grid();
                    show(&a, "back to the analysis");
                }
                [shift] => {
                    let n: i32 = shift.trim_start_matches('+').parse().context("a shift in beats: +1, -1, +2")?;
                    let a = effective()?;
                    let (Some(bpm), Some(d)) = (a.bpm, a.downbeat) else {
                        bail!("this track has no beat grid to shift");
                    };
                    let to = (d as i32 + n).rem_euclid(4) as u8;
                    db.set_human_downbeat(&p, bpm, a.beat_offset, to).map_err(anyhow::Error::msg)?;
                    show(&a, "was");
                    println!("now: beat 1 on grid beat {to} (set by hand; `library downbeat <file> clear` undoes it)");
                }
                _ => bail!("library downbeat <file> [show | +N | -N | clear]"),
            }
        }
        _ => bail!(
            "library [status] | add <folder> [name] | remove <name> | rescan | failed | grid <file|folder> [precise] | downbeat <file> [+N|-N|clear] | scan [dir] | collection …"
        ),
    }
    Ok(())
}
