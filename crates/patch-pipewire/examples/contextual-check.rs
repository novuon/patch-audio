//! Invoked only by the private-server routing integration test.
use patch_model::audio::AudioProvider;
use patch_pipewire::PipeWireAudioProvider;
use std::{
    process::Command,
    time::{Duration, Instant},
};
fn pactl(args: &[&str]) -> serde_json::Value {
    let result = Command::new("pactl").args(args).output().unwrap();
    assert!(result.status.success());
    serde_json::from_slice(&result.stdout).unwrap_or(serde_json::Value::Null)
}
fn settle(p: &mut PipeWireAudioProvider, predicate: impl Fn(&PipeWireAudioProvider) -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(12) {
        p.refresh();
        p.meters(0.0);
        if predicate(p) {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("provider did not settle: {:?}", p.strips());
}
fn main() {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
    assert!(runtime.contains("patch-core-routing-") || runtime.contains("patch-browser-streams-"));
    assert!(std::env::var("PULSE_SERVER").unwrap().contains(&runtime));
    let mut p = PipeWireAudioProvider::connect_existing().unwrap();
    if runtime.contains("patch-browser-streams-") {
        if let Ok(key) = std::env::var("PATCH_TEST_HIDDEN_STREAM") {
            let status = patch_engine::client::call(&serde_json::json!({"op":"eq_apps"})).unwrap();
            let lane = status["assigned"][&key].as_str().unwrap().to_string();
            settle(&mut p, |p| p.nodes().iter().filter(|n| n.name.starts_with("Chromium") || n.name == "Podcast caller").count() == 13
                && p.strips().iter().filter(|s| s.name.starts_with("Chromium") || s.name == "Podcast caller").count() == 12
                && !p.strips().iter().any(|s| p.strip_channel(s.node).as_deref() == Some(lane.as_str())));
            let meters = p.meters(0.0);
            assert_eq!(meters.strips_db.len(), p.strips().len());
            println!("Browser helper: retained in Router, absent from Mixer, aligned meters");
            return;
        }
        settle(&mut p, |p| {
            let strips = p.strips();
            let apps: Vec<_> = strips.iter().filter(|s| s.name.starts_with("Chromium ")).collect();
            apps.len() == 12 && apps.iter().all(|s| s.headroom && p.strip_channel(s.node).is_some())
        });
        let apps: Vec<_> = p.strips().into_iter().filter(|s| s.name.starts_with("Chromium ")).collect();
        let channels: std::collections::HashSet<_> = apps.iter().map(|s| p.strip_channel(s.node).unwrap()).collect();
        assert_eq!(channels.len(), 12);
        let node = apps[0].node;
        let channel = p.strip_channel(node).unwrap();
        p.set_gain(node, -6.0);
        p.set_mute(node, true);
        settle(&mut p, |_| {
            let status = patch_engine::client::call(&serde_json::json!({"op": "status"})).unwrap();
            status["channels"].as_array().unwrap().iter().any(|c| c["name"] == channel && c["mute"] == true && (c["gain_db"].as_f64().unwrap() + 6.0).abs() < 0.01)
        });
        let frame = p.meters(0.0);
        assert_eq!(frame.strips_db.len(), p.strips().len());
        assert_eq!(frame.strips_lr.as_ref().unwrap().len(), p.strips().len());
        let until = Instant::now() + Duration::from_secs(4);
        loop {
            let frame = p.meters(0.0);
            let strips = p.strips();
            let muted = strips.iter().position(|s| s.node == node).unwrap();
            let other_live = strips.iter().enumerate().any(|(i, s)| s.node != node && s.name.starts_with("Chromium ") && frame.strips_db[i] > -80.0);
            if frame.strips_db[muted] < -100.0 && other_live { break; }
            assert!(Instant::now() < until, "native UI meters did not follow the selected caller's mute");
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(p.strip_channel(node).unwrap(), channel);
        p.configure_application_strip(node, "Podcast caller", false).unwrap();
        assert_eq!(p.strips().iter().find(|s| s.node == node).unwrap().name, "Podcast caller");
        println!("Browser provider: twelve native controls, distinct lane mappings, aligned stereo meters and caller renaming passed");
        return;
    }

    // Two streams of one app are two strips, numbered (42fb1aa, 8897e5f).
    settle(&mut p, |p| {
        let strips = p.strips();
        let browsers: Vec<_> = strips.iter().filter(|s| s.name.starts_with("Browser ")).collect();
        browsers.len() == 2 && browsers.iter().all(|s| s.headroom && p.strip_channel(s.node).is_some())
    });
    assert!(!p.professional_routing());
    let strips = p.strips();
    assert!(!strips.iter().any(|s| s.node == "master"));
    assert_eq!(strips.iter().filter(|s| s.node == "system-sounds").count(), 1);
    assert_eq!(strips.iter().filter(|s| s.master).count(), 1);
    // A source's media title stays in its name; only identical names are numbered.
    let (browser_1, browser_2) = ("Browser · tone.wav - mpv 1", "Browser · tone.wav - mpv 2");
    for name in [browser_1, browser_2, "X · tone.wav - mpv", "Spotify · tone.wav - mpv"] {
        assert_eq!(strips.iter().filter(|s| s.name == name).count(), 1, "{name}: {:?}", strips.iter().map(|s| s.name).collect::<Vec<_>>());
    }
    let browser = strips.iter().find(|s| s.name == browser_1).unwrap().node;
    p.set_gain(browser, -12.0);
    p.set_mute(browser, true);
    let channel = p.strip_channel(browser).expect("browser owns a native lane");
    settle(&mut p, |_| {
        let status = patch_engine::client::call(&serde_json::json!({"op": "status"})).unwrap();
        status["channels"].as_array().unwrap().iter().any(|c| c["name"] == channel && c["mute"] == true && (c["gain_db"].as_f64().unwrap() + 12.0).abs() < 0.01)
    });
    let streams = pactl(&["-f", "json", "list", "sink-inputs"]);
    let members: Vec<_> = streams.as_array().unwrap().iter().filter(|s| s["properties"]["application.name"] == "Browser").collect();
    assert_eq!(members.len(), 2);
    // Pulse remains at unity and unmuted; only this native lane changes.
    assert!(members.iter().all(|s| s["mute"] == false && s["volume"].as_object().unwrap().values().all(|v| v["value"] == 65536)));
    let other = strips.iter().find(|s| s.name == browser_2).unwrap().node;
    assert_ne!(p.strip_channel(other).unwrap(), channel);
    assert!(!p.strips().iter().find(|s| s.node == other).unwrap().mute);
    p.set_mute(browser, false);
    p.set_gain("output", 60.0 * 0.37f32.log10());
    settle(&mut p, |p| {
        p.strips()
            .iter()
            .any(|s| s.node == "output" && (10f32.powf(s.gain_db / 60.0) - 0.37).abs() < 0.002)
    });
    let frame = p.meters(0.0);
    assert_eq!(frame.strips_db.len(), p.strips().len());
    assert_eq!(frame.strips_lr.as_ref().unwrap().len(), p.strips().len());
    assert!(
        p.devices()
            .iter()
            .any(|d| d.channels(patch_model::audio::Direction::In) == 16)
    );
    let selection = std::path::Path::new(&runtime).join("omarchy-patch/output-selection");
    std::fs::write(selection, "patch").unwrap();
    settle(&mut p, |p| p.professional_routing());
    let targets = p.targets("output");
    let interface = targets
        .iter()
        .find(|(_, name, _)| name.contains("AUX6 + AUX7"))
        .map(|target| target.0)
        .unwrap_or_else(|| panic!("no AUX6 + AUX7 route among {targets:?}"));
    p.connect("output", interface).unwrap();
    settle(&mut p, |p| {
        p.targets("output")
            .iter()
            .any(|(id, _, current)| *id == interface && *current)
    });
    println!(
        "Contextual mixer: per-stream controls, stable meters, bar volume, 16-input inventory and arbitrary output-pair assignment passed"
    );
}
