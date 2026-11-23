//! Headless visual fixtures for the Patch style: four decks, attached stems
//! and the AI timeline, with no live audio and no engine. Run through
//! `tools/style-shots.sh [out] [surface-filter]`; sizes from `PATCH_SHOT_SIZES`
//! ("647x712,857x712"), else three aspect ratios.
use patch_host::{HostedApp, SurfaceWindow};
use patch_surfaces::{
    AudioApp,
    ui::{DeckLevels, DjState, StemDeck, StemItem, StemState},
};
use slint::{ComponentHandle, ModelRc, VecModel};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PATCH_STYLE_SHOTS").is_none() {
        return Err("Run tools/style-shots.sh (uses private configuration)".into());
    }
    patch_host::shot::install()?;
    let app = AudioApp::new();
    let out = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/patch-style-fixtures".into()),
    );
    std::fs::create_dir_all(&out)?;
    let shooter = patch_host::shot::Shooter { scale: 1.0 };
    for (name, surface) in [
        ("crossfader4", "dj-mixer"),
        ("crossfader2", "dj-mixer"),
        ("deck", "deck-a"),
        ("deck-mission", "deck-a"),
        ("mixer", "mixer"),
        ("eq", "eq"),
        ("meter", "meters"),
        ("inspector", "inspector"),
        ("library", "library"),
        ("graph", "graph"),
        ("booth", "dj"),
        ("visualiser", "visualiser"),
        ("diagnostics", "diagnostics"),
        ("devices", "devices"),
        ("analyzer", "analyzer"),
    ] {
        if let Some(only) = std::env::args().nth(2)
            && !name.starts_with(only.as_str())
        {
            continue;
        }
        let window = app.create(surface);
        let mut host = patch_host::HostState::build(&*app, &AudioApp::spec(), surface, &[], None);
        // `PATCH_SHOT_BADGE=LIVE`: the header as a live unit shows it.
        if let Ok(badge) = std::env::var("PATCH_SHOT_BADGE") {
            host.badge = badge;
        }
        window.apply_host(&host);
        window.apply_theme(&patch_ui::load_palette());
        // `PATCH_SHOT_STYLE=classic|patch|pro` (default pro).
        window.set_style(
            std::env::var("PATCH_SHOT_STYLE").ok().and_then(|v| patch_ui::style::Style::parse(&v)).unwrap_or(patch_ui::style::Style::Pro),
        );
        let dj = window.global::<DjState>();
        dj.set_mixer_four(name == "crossfader4");
        dj.set_mission_view_a(name == "deck-mission");
        dj.set_deck_c(dj.get_deck_a());
        dj.set_deck_d(dj.get_deck_b());
        dj.set_levels_a(DeckLevels {
            level_l: 0.55,
            level_r: 0.52,
            peak: 0.6,
            vu: 0.45,
        });
        dj.set_levels_b(DeckLevels {
            level_l: 0.78,
            level_r: 0.75,
            peak: 0.85,
            vu: 0.7,
        });
        dj.set_levels_c(dj.get_levels_a());
        dj.set_levels_d(dj.get_levels_b());
        dj.set_program_l(0.78);
        dj.set_program_r(0.75);
        // `PATCH_SHOT_SILENT=1`: nothing playing (every level at zero).
        if std::env::var_os("PATCH_SHOT_SILENT").is_some() {
            let zero = DeckLevels { level_l: 0.0, level_r: 0.0, peak: 0.0, vu: -1.0 };
            dj.set_levels_a(zero.clone());
            dj.set_levels_b(zero.clone());
            dj.set_levels_c(zero.clone());
            dj.set_levels_d(zero);
            dj.set_program_l(0.0);
            dj.set_program_r(0.0);
        }
        let stems = window.global::<StemState>();
        stems.set_a(StemDeck {
            ready: true,
            attached: true,
            enabled: true,
            backend_ready: true,
            status: "ready · attached".into(),
            items: ModelRc::new(VecModel::from(
                ["Vocals", "Drums", "Bass", "Other"]
                    .iter()
                    .enumerate()
                    .map(|(i, name)| StemItem {
                        id: name.to_lowercase().into(),
                        name: (*name).into(),
                        gain: if i == 2 { 0.8 } else { 1.0 },
                        meter: 0.5,
                        muted: i == 3,
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )),
            ..Default::default()
        });
        stems.set_bridge_source("a".into());
        stems.set_bridge_destination("b".into());
        stems.set_bridge_stem("Bass".into());
        let sizes: Vec<(u32, u32)> = std::env::var("PATCH_SHOT_SIZES")
            .ok()
            .map(|v| {
                v.split(',')
                    .filter_map(|p| p.split_once('x'))
                    .filter_map(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                    .collect()
            })
            .unwrap_or_else(|| vec![(720, 720), (720, 960), (540, 960)]);
        for (w, h) in sizes {
            // Let the frame renderer observe the new aspect ratio before capture.
            let _ = shooter.render_component(&window, w, h);
            // Frozen fixtures have no frame timer. A full style cycle invokes
            // the existing renderer and returns to Classic, in private config.
            for _ in 0..3 {
                dj.invoke_scope_style_next();
            }
            shooter
                .render_component(&window, w, h)
                .save(out.join(format!("{name}-{w}x{h}.png")))?;
            // `PATCH_SHOT_TIME=N`: N full repaints (software renderer), the
            // median and worst ms per frame, to compare styles' cost.
            if let Some(n) = std::env::var("PATCH_SHOT_TIME").ok().and_then(|v| v.parse::<usize>().ok()) {
                let mut ms = shooter.time_component(&window, w, h, n);
                ms.sort_by(f64::total_cmp);
                println!(
                    "{name:<13} {w}x{h} median {:.2} ms  worst {:.2} ms",
                    ms[ms.len() / 2],
                    ms[ms.len() - 1]
                );
            }
        }
    }
    Ok(())
}
