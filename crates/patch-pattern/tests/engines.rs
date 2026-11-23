//! The engine contract: every engine behind `Backend` must give the same
//! events for the same Patch definition (where its capabilities say it
//! can). Strudel runs only where its adapter is installed
//! (`cd adapters/strudel && npm install`); otherwise those checks are skipped.

use patch_pattern::backend::{Backend, Definition, Reference};
use patch_pattern::event::Target;
use patch_pattern::scheduler::{Quantize, Scheduler};
use patch_pattern::strudel::{self, Strudel};
use patch_pattern::{Program, Span, Time, dsl};

fn strudel() -> Option<Strudel> {
    let Some(dir) = strudel::locate() else {
        eprintln!("Strudel adapter not installed: skipped");
        return None;
    };
    match Strudel::start(&dir) {
        // These check what Strudel plays, not how fast: a loaded machine
        // can stall Node past the live 50 ms reply limit.
        Ok(mut s) => {
            s.query_timeout = std::time::Duration::from_secs(5);
            Some(s)
        }
        Err(e) => {
            eprintln!("Strudel unavailable ({e}): skipped");
            None
        }
    }
}

/// Deterministic programs (no chance): both engines must agree exactly.
const SAME: &[&str] = &[
    r#"s("bd*4 [hh hh] sd")"#,
    r#"s("bd [sd cp]").every(3, fast(2))"#,
    r#"s("<bd sd> hh*2").rev()"#,
    r#"s("bd(3,8,2) hh(5,8)")"#,
    r#"s("{bd sd cp}%4")"#,
    r#"s("bd@3 sd _ hh!2")"#,
    r#"s("bd sd hh cp").iter(4)"#,
    r#"s("bd sd").ply(3).off(1/8, fast(2))"#,
    r#"s("bd sd hh cp").chunk(4, fast(2))"#,
    r#"deck(a).filter("<200 800 1600 400>")"#,
    r#"visual.intensity("<0.2 0.4 0.8 1>")
visual.symmetry("<2 4 8 16>").slow(2)"#,
    r#"deck(b).stem("vocals", "mute ~ unmute ~")"#,
    r#"visual.hue(saw.segment(4).range(0.2, 0.8))"#,
];

fn program(src: &str) -> Program {
    dsl::parse(src).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

fn events(b: &mut dyn Backend, p: &Program, cycles: i64) -> Vec<(Time, Target, String)> {
    b.load(1, &Definition::Program { program: p.clone() })
        .unwrap_or_else(|e| panic!("{e:?}"));
    let mut out = vec![];
    for c in 0..cycles {
        for e in b.query(1, Span::cycles(c, c + 1)).unwrap() {
            let v = match e.value.num() {
                Some(n) => format!("{n:.6}"),
                None => e.value.to_string(),
            };
            out.push((e.onset(), e.target, v));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
    out
}

#[test]
fn strudel_plays_patch_definitions_like_the_reference() {
    let Some(mut s) = strudel() else { return };
    let caps = s.capabilities();
    assert!(
        caps.program && caps.sandboxed && caps.out_of_process,
        "{caps:?}"
    );
    for src in SAME {
        let p = program(src);
        let want = events(&mut Reference::default(), &p, 8);
        let got = events(&mut s, &p, 8);
        assert!(!want.is_empty(), "{src}");
        assert_eq!(got, want, "{src}");
    }
}

#[test]
fn strudel_source_live_coding() {
    let Some(mut s) = strudel() else { return };
    let code = "drums: s(\"bd*2 [hh sd]\")\nfx: deck(\"a\").filter(\"<200 800>\")\nst: stem(\"vocals\").sometimes(mute)";
    let def = Definition::Source {
        language: "strudel".into(),
        code: code.into(),
        cycle_beats: Time::whole(4),
    };
    let l = s.load(7, &def).unwrap();
    assert_eq!(
        l.layers.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["drums", "fx", "st"]
    );
    let ev = s.query(7, Span::cycles(0, 1)).unwrap();
    assert!(
        ev.iter()
            .any(|e| e.target == Target::Sound && e.loc.is_some())
    );
    // Errors are located; the slot keeps what it had.
    let bad = Definition::Source {
        language: "strudel".into(),
        code: "x: s(\"bd\")\ny: s(\"bd [ sd\")".into(),
        cycle_beats: Time::whole(4),
    };
    let e = s.load(7, &bad).unwrap_err();
    // Strudel points at the character it failed on (the closing quote).
    assert_eq!((e[0].line, e[0].col), (2, 14), "{e:?}");
    assert_eq!(s.query(7, Span::cycles(0, 1)).unwrap().len(), ev.len());
    // The sandbox: no processes, no files.
    for evil in [
        "process.binding('spawn_sync')",
        "await import('node:child_process').then(c => c.execSync('id'))",
        "await import('node:fs').then(f => f.writeFileSync('/tmp/patch-strudel-escape', 'x'))",
    ] {
        let d = Definition::Source {
            language: "strudel".into(),
            code: format!("{evil}\ns(\"bd\")"),
            cycle_beats: Time::whole(4),
        };
        let _ = s.load(8, &d);
    }
    assert!(!std::path::Path::new("/tmp/patch-strudel-escape").exists());
}

#[test]
fn the_scheduler_runs_strudel_on_patch_time() {
    let Some(s) = strudel() else { return };
    let mut sch = Scheduler::new(Box::new(s), 48_000, 120.0);
    let def = Definition::Source {
        language: "strudel".into(),
        code: "s(\"bd*4 [hh hh] sd\")".into(),
        cycle_beats: Time::whole(4),
    };
    sch.load(&def, Quantize::Now, 0).unwrap();
    sch.play(0, Time::ZERO, 120.0);
    let mut all = sch.prepare(0);
    let mut now = 0;
    // Ten seconds of 10 ms ticks.
    for _ in 0..1000 {
        now += 480;
        all.extend(sch.prepare(now));
    }
    let first: Vec<i64> = all.iter().take(7).map(|e| e.sample).collect();
    assert_eq!(first, [0, 8_000, 16_000, 24_000, 32_000, 48_000, 64_000]);
    let m = &sch.metrics;
    assert_eq!(m.late_events, 0);
    assert_eq!(m.query_errors, 0);
}
