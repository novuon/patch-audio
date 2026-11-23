//! Mic control and offline calibration. File decoding never runs in the engine.
use serde_json::{Value, json};
use std::path::Path;

fn call(request: Value) -> anyhow::Result<Value> {
    patch_engine::client::call(&request).map_err(anyhow::Error::msg)
}
fn profile_file(path: &str) -> anyhow::Result<patch_core::mic::Profile> {
    anyhow::ensure!(
        std::fs::metadata(path)?.len() <= 64 * 1024,
        "profile exceeds 64 KiB"
    );
    let p: patch_core::mic::Profile = serde_json::from_slice(&std::fs::read(path)?)?;
    p.validate().map_err(anyhow::Error::msg)?;
    Ok(p)
}
pub fn run(words: &[&str]) -> anyhow::Result<()> {
    let result = match words {
        [] | ["status"] => call(json!({"op":"mics"}))?,
        ["profiles"] => call(json!({"op":"mic_profiles"}))?,
        ["select", ch, source] => {
            call(json!({"op":"mic", "channel":ch, "source":source, "monitor":false}))?
        }
        ["remove", ch] => call(json!({"op":"mic", "channel":ch, "source":null}))?,
        ["monitor", ch, state @ ("on" | "off")] => {
            call(json!({"op":"mic_monitor", "channel":ch, "on":*state == "on"}))?
        }
        ["set", ch, key, value] => {
            anyhow::ensure!(
                [
                    "hpf",
                    "gate",
                    "deess",
                    "comp",
                    "expander",
                    "noise",
                    "input_gain_db",
                    "output_gain_db"
                ]
                .contains(key),
                "unknown voice parameter"
            );
            let value = if *value == "off" {
                if ["gate", "expander", "noise"].contains(key) {
                    Value::Null
                } else {
                    json!(0.0)
                }
            } else {
                let n: f64 = value.parse()?;
                anyhow::ensure!(n.is_finite(), "finite value required");
                json!(n)
            };
            let mut req = json!({"op":"voice", "channel":ch});
            req[*key] = value;
            call(req)?
        }
        ["bypass", ch, state @ ("on" | "off")] => {
            call(json!({"op":"voice", "channel":ch, "on":*state == "off"}))?
        }
        ["profile", ch, name] => call(json!({"op":"mic_profile", "channel":ch, "name":name}))?,
        ["restore", ch] => call(json!({"op":"mic_profile", "channel":ch, "action":"restore"}))?,
        ["save", ch, name] => {
            call(json!({"op":"mic_profile", "channel":ch, "name":name, "action":"save"}))?
        }
        ["save", ch, name, "--replace"] => call(
            json!({"op":"mic_profile", "channel":ch, "name":name, "action":"save", "replace":true}),
        )?,
        ["import", path] => {
            call(json!({"op":"mic_profile", "action":"import", "profile":profile_file(path)?}))?
        }
        ["import", path, "--replace"] => call(
            json!({"op":"mic_profile", "action":"import", "profile":profile_file(path)?, "replace":true}),
        )?,
        ["export", name, path] => {
            let p = call(json!({"op":"mic_profile", "action":"export", "name":name}))?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)?;
            file.write_all(&serde_json::to_vec_pretty(&p)?)?;
            file.sync_all()?;
            p
        }
        ["duplicate", original, name] => {
            let mut p = call(json!({"op":"mic_profile", "action":"export", "name":original}))?;
            p["name"] = json!(name);
            call(json!({"op":"mic_profile", "action":"import", "profile":p}))?
        }
        ["calibrate", path] => {
            let samples =
                patch_analysis::mic::decode_short(Path::new(path)).map_err(anyhow::Error::msg)?;
            json!(patch_analysis::mic::analyze(&samples, 48000).map_err(anyhow::Error::msg)?)
        }
        ["compare", original, profile, output] => {
            let p = profile_file(profile)?;
            let samples = patch_analysis::mic::decode_short(Path::new(original))
                .map_err(anyhow::Error::msg)?;
            patch_record::mic::render(&samples, &p, Path::new(output))
                .map_err(anyhow::Error::msg)?;
            json!({"original":original, "processed":output, "latency_samples":0, "note":"Play both recordings through Patch Player; original remains unchanged. This render excludes the device channel fader."})
        }
        _ => anyhow::bail!(
            "mic status | profiles | select <mic-N> <source> | remove <mic-N> | monitor <mic-N> on|off | profile <mic-N> <name> | restore <mic-N> | save <mic-N> <name> [--replace] | duplicate <name> <new-name> | import <file> [--replace] | export <name> <file> | calibrate <take> | compare <take> <profile.json> <output.wav>"
        ),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
