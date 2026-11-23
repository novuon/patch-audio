use anyhow::{Context, Result, bail};
use patch_plugin::{Mirror, Processor, RackConfig, View, scan};
use serde_json::json;
use std::path::{Path, PathBuf};

const USAGE: &str = "plugins [status] | installed | install <file.zip|file.tar.gz|bundle.vst3> | uninstall <name|path> | add <name|path> | remove <slot> | on <slot> | off <slot>
        | move <slot> up|down | place <slot> master|mic-N|app-N|deck-a|aux-1|aux-2|fx-1|fx-2
        | view <slot> patch|original | parameters <slot>
        | set <slot> <parameter-id> <0..1> | test <slot>";

/// Tell a running engine to run the saved rack (nothing when it is off).
fn sync_engine() -> bool {
    patch_engine::client::call(&json!({"op": "plugins", "action": "sync"})).is_ok()
}

fn save(rack: &RackConfig) -> Result<()> {
    patch_plugin::save_config(rack)?;
    sync_engine();
    Ok(())
}

fn slot_id(rack: &RackConfig, word: &str) -> Result<u32> {
    let id: u32 = word.parse().context("a slot is its number (see `plugins status`)")?;
    anyhow::ensure!(rack.slot(id).is_some(), "the rack has no slot {id}");
    Ok(id)
}

/// A bundle by path, or the installed plugin whose name matches.
fn resolve(word: &str) -> Result<PathBuf> {
    let path = Path::new(word);
    if path.extension().is_some_and(|e| e == "vst3") && path.exists() {
        return Ok(path.to_path_buf());
    }
    let want = word.to_lowercase();
    let installed = scan::installed();
    let matches: Vec<_> = installed
        .iter()
        .filter(|b| b.name.to_lowercase().contains(&want))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.path.clone()),
        [] => bail!("no installed VST3 plugin matches \"{word}\" (see `plugins installed`)"),
        many => bail!(
            "\"{word}\" matches {}: be more specific",
            many.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn status() -> Result<()> {
    let rack = patch_plugin::load_or_default()?;
    let live = patch_engine::client::call(&json!({"op": "plugins", "action": "status"})).ok();
    if rack.slots.is_empty() {
        println!("The rack is empty. Add a plugin: omarchy-patch plugins add <name>");
    }
    for slot in &rack.slots {
        let running = live.as_ref().and_then(|l| {
            l.as_array()?
                .iter()
                .find(|r| r["id"].as_u64() == Some(u64::from(slot.id)))
                .cloned()
        });
        let state = match (&running, slot.active) {
            (Some(r), true) => match &r["state"] {
                serde_json::Value::String(s) => s.clone(),
                other => format!("failed: {}", other["failed"].as_str().unwrap_or("?")),
            },
            (None, true) if live.is_some() => "not loaded".into(),
            (None, true) => "on (engine not running)".into(),
            _ => "off".into(),
        };
        println!(
            "{:>2}  {:<24} {:<18} {:<8} {}{}",
            slot.id,
            slot.name,
            slot.vendor,
            slot.place.label(),
            state,
            if slot.view == View::Original { " · opens as the original" } else { "" }
        );
    }
    Ok(())
}

pub fn run(args: &[&str]) -> Result<()> {
    match args {
        ["status"] | [] => status(),
        ["installed"] => {
            for b in scan::installed() {
                println!(
                    "{:<24} {:<18} {:<8} {}{}",
                    b.name,
                    b.vendor,
                    b.version,
                    b.path.display(),
                    if b.native { "" } else { "  (no build for this machine)" }
                );
            }
            Ok(())
        }
        ["install", file] => {
            let into = patch_plugin::install::plugins_dir().context("no home folder")?;
            let done = patch_plugin::install::install(Path::new(file), &into)?;
            println!("{}", patch_plugin::install::summary(&done));
            Ok(())
        }
        ["uninstall", words @ ..] if !words.is_empty() => {
            let path = resolve(&words.join(" "))?;
            let name = patch_plugin::install::uninstall_unused(&path, &patch_plugin::load_or_default()?)?;
            println!("{name} is in the Trash (restore it from there)");
            Ok(())
        }
        // Internal: the engine's plugin host (plugins run here, not there).
        ["host"] => patch_plugin::leave(patch_plugin::chain::serve()),
        // Internal: one plugin's own editor, in a process of its own.
        ["editor"] => patch_plugin::leave(patch_plugin::editor::serve()),
        // Internal: the child process `add` loads a plugin in first.
        ["probe", path] => patch_plugin::leave(
            patch_plugin::probe_here(Path::new(path))
                .and_then(|info| {
                    println!("{}", serde_json::to_string(&info)?);
                    Ok(())
                }),
        ),
        ["add", words @ ..] if !words.is_empty() => {
            let path = resolve(&words.join(" "))?;
            let info = patch_plugin::probe_isolated(&path)?;
            let mut rack = patch_plugin::load_or_default()?;
            let mut bundle = scan::bundle_info(&path);
            bundle.name = info.name.clone();
            if !info.vendor.is_empty() {
                bundle.vendor = info.vendor.clone();
            }
            let id = rack.add(&path, &bundle)?;
            save(&rack)?;
            println!(
                "{} is slot {id}, off. Switch it on: omarchy-patch plugins on {id}",
                info.name
            );
            Ok(())
        }
        ["place", slot, place] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            let place = patch_plugin::Place::parse(place);
            if let patch_plugin::Place::Channel(channel) = &place {
                anyhow::ensure!(
                    patch_engine::daemon::CHANNELS.contains(&channel.as_str()),
                    "no channel {channel}: master, aux-1, aux-2, fx-1, fx-2 or one of {}",
                    patch_engine::daemon::CHANNELS.join(", ")
                );
            }
            let name = place.label();
            rack.slot_mut(id).expect("checked").place = place;
            save(&rack)?;
            println!("{} runs on {name}", rack.slot(id).expect("checked").name);
            Ok(())
        }
        ["remove", slot] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            rack.slots.retain(|s| s.id != id);
            save(&rack)
        }
        [on @ ("on" | "off"), slot] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            rack.slot_mut(id).expect("checked").active = *on == "on";
            patch_plugin::save_config(&rack)?;
            println!(
                "{} {}{}",
                rack.slot(id).expect("checked").name,
                if *on == "on" { "on" } else { "off" },
                if sync_engine() { "" } else { " (applies when the engine starts)" }
            );
            Ok(())
        }
        ["move", slot, way @ ("up" | "down")] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            rack.shift(id, if *way == "up" { -1 } else { 1 });
            save(&rack)
        }
        ["view", slot, view @ ("patch" | "original")] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            rack.slot_mut(id).expect("checked").view =
                if *view == "patch" { View::Patch } else { View::Original };
            patch_plugin::save_config(&rack)
        }
        ["parameters", slot] => {
            let rack = patch_plugin::load_or_default()?;
            let slot = rack.slot(slot_id(&rack, slot)?).expect("checked");
            let mirror = Mirror::load(&slot.plugin, &slot.parameters)?;
            println!("{} controls of {}:", mirror.info().parameters.len(), slot.name);
            for p in mirror.parameters() {
                println!(
                    "{}\t{}\t{:.4}\t{}{}",
                    p.id,
                    p.name,
                    p.value,
                    mirror.text(p.id, p.value),
                    if p.read_only { "\tread-only" } else { "" }
                );
            }
            Ok(())
        }
        ["set", slot, parameter, value] => {
            let mut rack = patch_plugin::load_or_default()?;
            let id = slot_id(&rack, slot)?;
            let parameter: u32 = parameter.parse().context("parameter id must be an integer")?;
            let value: f64 = value.parse().context("parameter value must be a number")?;
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                bail!("normalized parameter value must be between 0 and 1");
            }
            rack.set_parameter(id, parameter, value);
            patch_plugin::save_config(&rack)?;
            let live = patch_engine::client::call(&json!({
                "op": "plugins", "action": "set", "slot": id, "id": parameter, "value": value
            }))
            .is_ok();
            println!(
                "Parameter {parameter} at {value:.4}{}",
                if live { "" } else { " (applies when the engine starts)" }
            );
            Ok(())
        }
        ["test", slot] => {
            let rack = patch_plugin::load_or_default()?;
            let slot = rack.slot(slot_id(&rack, slot)?).expect("checked");
            let mut plugin =
                Processor::load_with_parameters(&slot.plugin, 48_000, 8192, &slot.parameters)
                    .with_context(|| format!("load {}", slot.name))?;
            let mut left = vec![0.0; 1024];
            let mut right = vec![0.0; 1024];
            for (i, (l, r)) in left.iter_mut().zip(&mut right).enumerate() {
                let sample = (std::f32::consts::TAU * 55.0 * i as f32 / 48_000.0).sin() * 0.02;
                *l = sample;
                *r = sample;
            }
            plugin.process(&mut left, &mut right)?;
            let peak = left
                .iter()
                .chain(&right)
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            anyhow::ensure!(peak.is_finite(), "plugin returned invalid samples");
            println!(
                "{} processed 1024 stereo frames of a 55 Hz tone at 48 kHz; peak {peak:.6}",
                plugin.info().name
            );
            Ok(())
        }
        _ => bail!("omarchy-patch {USAGE}"),
    }
}
