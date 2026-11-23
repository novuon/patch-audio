//! The example patterns in `examples/patterns` parse, and every layer of
//! each plays something within 32 cycles (a line that never sounds is a
//! mistake in an example). `derived:` addresses resolve to a stand-in id,
//! as the service would.

use patch_core::material::AssetId;
use patch_pattern::dsl::{Resolver, parse_with};
use patch_pattern::backend::{Backend, Definition};
use patch_pattern::time::Span;

struct Stand;
impl Resolver for Stand {
    fn resolve(&self, address: &str) -> Result<AssetId, String> {
        assert!(address.starts_with("derived:"), "{address}");
        Ok(AssetId("dm-0123456789abcdef".into()))
    }
}

#[test]
fn every_example_parses_and_every_layer_plays() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/patterns");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "patch"))
        .collect();
    files.sort();
    let mut strudel = patch_pattern::strudel::locate()
        .and_then(|d| patch_pattern::strudel::Strudel::start(&d).ok());
    if strudel.is_none() {
        eprintln!("Strudel not installed: the reference engine only");
    }
    assert!(files.len() >= 6, "{files:?}");
    for f in files {
        let code = std::fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let program = parse_with(&code, &Stand).unwrap_or_else(|e| {
            panic!("{name}: {}", e.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))
        });
        program.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        let events = program.compile().events(Span::cycles(0, 32));
        // On Strudel too, when installed: it loads, and plays as much.
        if let Some(st) = strudel.as_mut() {
            let def = Definition::Program { program: program.clone() };
            st.load(1, &def).unwrap_or_else(|e| panic!("{name} on Strudel: {e:?}"));
            let theirs = st.query(1, Span::cycles(0, 32)).unwrap_or_else(|e| panic!("{name} on Strudel: {e}"));
            assert!(!theirs.is_empty(), "{name}: silent on Strudel");
            let (a, b) = (events.len() as f64, theirs.len() as f64);
            assert!((a - b).abs() <= 0.25 * a.max(b), "{name}: {a} events here, {b} on Strudel");
        }
        for (i, l) in program.layers.iter().enumerate() {
            assert!(
                events.iter().any(|e| e.layer as usize == i),
                "{name}: layer {} ({}) never plays",
                l.name,
                l.target.label()
            );
        }
    }
}

/// Every Tidal example in `examples/tidal` runs as written (as one script,
/// from the top), every channel plays within 8 cycles, and Patch has a sound
/// for everything it names.
#[test]
fn every_tidal_example_runs_and_every_channel_plays() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/tidal");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "tidal"))
        .collect();
    files.sort();
    assert!(files.len() >= 6, "{files:?}");
    for f in files {
        let code = std::fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let mut s = patch_pattern::tidal::Session::default();
        s.apply(&code).unwrap_or_else(|e| {
            panic!("{name}: {}", e.iter().map(|e| e.located(&code).to_string()).collect::<Vec<_>>().join("; "))
        });
        assert!(s.cps.is_some(), "{name}: sets its tempo");
        let c = s.compile().unwrap();
        let (_, warnings) = c.survey(8);
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
        let ev = c.events(Span::cycles(0, 8));
        for (i, ch) in s.channels.iter().enumerate() {
            assert!(ev.iter().any(|e| e.layer as usize == i), "{name}: {} never plays", ch.0);
        }
    }
}
